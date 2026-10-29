//! Folder comparison (2-way and 3-way) and folder merge operations.

use super::diff::{DiffOptions, Normalizer};
use super::filter::FileFilter;
use super::{comments, prettify, text};
use crate::i18n::tr;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum CompareMethod {
    FullContents,
    QuickContents,
    BinaryContents,
    Date,
    DateSize,
    Size,
}

impl CompareMethod {
    pub const ALL: [CompareMethod; 6] = [
        CompareMethod::FullContents,
        CompareMethod::QuickContents,
        CompareMethod::BinaryContents,
        CompareMethod::Date,
        CompareMethod::DateSize,
        CompareMethod::Size,
    ];

    pub fn key(self) -> &'static str {
        match self {
            CompareMethod::FullContents => "method.full",
            CompareMethod::QuickContents => "method.quick",
            CompareMethod::BinaryContents => "method.binary",
            CompareMethod::Date => "method.date",
            CompareMethod::DateSize => "method.datesize",
            CompareMethod::Size => "method.size",
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct FolderOptions {
    pub method: CompareMethod,
    pub recursive: bool,
    pub include_unique_subfolders: bool,
    pub skip_hidden: bool,
    /// Tolerance for date comparisons (FAT file systems store 2 second precision).
    pub time_tolerance_secs: u64,
    pub threads: usize,
}

impl Default for FolderOptions {
    fn default() -> Self {
        FolderOptions {
            method: CompareMethod::FullContents,
            recursive: true,
            include_unique_subfolders: true,
            skip_hidden: false,
            time_tolerance_secs: 2,
            threads: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpStatus {
    Identical,
    Different,
    Unique,
    Error,
    /// Folder whose contents were not compared (non-recursive mode), or a file left over when
    /// the comparison was stopped.
    NotCompared,
    /// File on every side whose comparison has not finished yet.
    Pending,
}

#[derive(Clone, Debug)]
pub struct DirItem {
    pub rel_dir: String,
    pub name: String,
    pub is_dir: bool,
    pub exists: [bool; 3],
    pub size: [Option<u64>; 3],
    pub mtime: [Option<SystemTime>; 3],
    pub status: CmpStatus,
    /// Three-way: index of the pane that differs from the other two.
    pub diff_side: Option<usize>,
    pub binary: bool,
    pub error: Option<String>,
    pub depth: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl DirItem {
    pub fn rel_path(&self) -> String {
        if self.rel_dir.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.rel_dir, self.name)
        }
    }

    pub fn ext(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        Path::new(&self.name)
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub fn path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for part in self.rel_path().split('/') {
            p.push(part);
        }
        p
    }
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub abort: AtomicBool,
    pub current: Mutex<String>,
    /// Collects every item and result for `take_updates`, so a view can show them while the
    /// comparison runs.
    pub live: bool,
    updates: Mutex<Updates>,
}

impl Progress {
    pub fn live() -> Progress {
        Progress {
            live: true,
            ..Default::default()
        }
    }

    /// Items found and files compared since the last call.
    pub fn take_updates(&self) -> Updates {
        std::mem::take(&mut *self.updates.lock().unwrap())
    }
}

#[derive(Default)]
pub struct Updates {
    /// New items in index order. Their `children` are empty: each item is a child of its
    /// `parent`, which came earlier.
    pub items: Vec<DirItem>,
    pub results: Vec<Outcome>,
}

impl Updates {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.results.is_empty()
    }
}

/// Result of one file comparison.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub index: usize,
    pub status: CmpStatus,
    /// Three-way: the pane that differs from the other two.
    pub diff_side: Option<usize>,
    pub binary: bool,
    pub error: Option<String>,
}

impl Outcome {
    pub fn apply(self, items: &mut [DirItem]) {
        let it = &mut items[self.index];
        it.status = self.status;
        it.diff_side = self.diff_side;
        it.binary = self.binary;
        it.error = self.error;
    }
}

/// Files waiting for a compare worker, filled while the folders are still being scanned.
#[derive(Default)]
struct Queue {
    state: Mutex<(VecDeque<(usize, DirItem)>, bool)>,
    ready: Condvar,
}

impl Queue {
    fn push(&self, job: (usize, DirItem)) {
        self.state.lock().unwrap().0.push_back(job);
        self.ready.notify_one();
    }

    fn close(&self) {
        self.state.lock().unwrap().1 = true;
        self.ready.notify_all();
    }

    /// Next file, or `None` once the queue is closed and empty.
    fn pop(&self) -> Option<(usize, DirItem)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(job) = state.0.pop_front() {
                return Some(job);
            }
            if state.1 {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }
}

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: Option<SystemTime>,
}

fn is_hidden(e: &std::fs::DirEntry) -> bool {
    if e.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(m) = e.metadata() {
            return m.file_attributes() & 0x2 != 0;
        }
    }
    false
}

fn list_dir(dir: &Path, skip_hidden: bool) -> std::io::Result<Vec<Entry>> {
    let rd = std::fs::read_dir(dir)?;
    let mut v = Vec::new();
    for e in rd {
        let e = e?;
        if skip_hidden && is_hidden(&e) {
            continue;
        }
        let md = e.metadata()?;
        v.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
            mtime: md.modified().ok(),
        });
    }
    Ok(v)
}

/// How the scan matches names across sides: folders apart from files, and ignoring case where
/// the file system usually does.
pub(crate) fn name_key(name: &str, is_dir: bool) -> (bool, String) {
    let n = if cfg!(windows) || cfg!(target_os = "macos") {
        name.to_lowercase()
    } else {
        name.to_string()
    };
    (!is_dir, n)
}

pub struct Scanner<'a> {
    pub roots: &'a [PathBuf],
    pub filter: &'a FileFilter,
    pub fopts: &'a FolderOptions,
    pub dopts: &'a DiffOptions,
    pub progress: &'a Progress,
}

impl<'a> Scanner<'a> {
    /// Scans the folders and compares the files found on every side. Workers compare files
    /// while the scan is still running, so results arrive early on large trees.
    pub fn run(&self) -> Vec<DirItem> {
        let threads = if self.fopts.threads > 0 {
            self.fopts.threads
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(8)
        };
        let queue = Queue::default();
        let results = Mutex::new(Vec::new());
        let norm = Normalizer::new(self.dopts);
        let mut items = Vec::new();
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| self.compare_worker(&queue, &results, &norm));
            }
            self.scan("", None, 0, [true; 3], &mut items, &queue);
            queue.close();
        });
        for o in results.into_inner().unwrap() {
            o.apply(&mut items);
        }
        for it in &mut items {
            if it.status == CmpStatus::Pending {
                it.status = CmpStatus::NotCompared;
            }
        }
        aggregate(&mut items, self.roots.len());
        items
    }

    fn compare_worker(&self, queue: &Queue, results: &Mutex<Vec<Outcome>>, norm: &Normalizer) {
        while let Some((index, it)) = queue.pop() {
            if self.progress.abort.load(Ordering::Relaxed) {
                break;
            }
            let paths: Vec<PathBuf> = self.roots.iter().map(|r| it.path(r)).collect();
            let o = match compare_files_with_cancel(
                &paths,
                &it,
                self.fopts,
                self.dopts,
                norm,
                Some(&self.progress.abort),
            ) {
                Ok((status, diff_side, binary)) => Outcome {
                    index,
                    status,
                    diff_side,
                    binary,
                    error: None,
                },
                Err(e) => Outcome {
                    index,
                    status: CmpStatus::Error,
                    diff_side: None,
                    binary: false,
                    error: Some(format!("{e:#}")),
                },
            };
            if self.progress.live {
                self.progress
                    .updates
                    .lock()
                    .unwrap()
                    .results
                    .push(o.clone());
            }
            results.lock().unwrap().push(o);
            self.progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn scan(
        &self,
        rel: &str,
        parent: Option<usize>,
        depth: usize,
        present: [bool; 3],
        items: &mut Vec<DirItem>,
        queue: &Queue,
    ) {
        if self.progress.abort.load(Ordering::Relaxed) {
            return;
        }
        let n = self.roots.len();
        let mut merged: BTreeMap<(bool, String), DirItem> = BTreeMap::new();
        for (side, root) in self.roots.iter().enumerate() {
            if !present[side] {
                continue;
            }
            let mut dir = root.clone();
            if !rel.is_empty() {
                for p in rel.split('/') {
                    dir.push(p);
                }
            }
            let entries = match list_dir(&dir, self.fopts.skip_hidden) {
                Ok(entries) => entries,
                Err(e) => {
                    // An incomplete listing cannot establish that any child is missing.
                    // Discard this subtree rather than offer destructive merge operations.
                    let error = format!("{}: {e}", dir.display());
                    let index = if let Some(p) = parent {
                        items[p].status = CmpStatus::Error;
                        items[p].error = Some(error.clone());
                        p
                    } else {
                        let index = items.len();
                        let item = DirItem {
                            rel_dir: String::new(),
                            name: ".".into(),
                            is_dir: true,
                            exists: present,
                            size: [None; 3],
                            mtime: [None; 3],
                            status: CmpStatus::Error,
                            diff_side: None,
                            binary: false,
                            error: Some(error.clone()),
                            depth,
                            parent: None,
                            children: Vec::new(),
                        };
                        if self.progress.live {
                            self.progress
                                .updates
                                .lock()
                                .unwrap()
                                .items
                                .push(item.clone());
                        }
                        items.push(item);
                        index
                    };
                    if self.progress.live {
                        self.progress.updates.lock().unwrap().results.push(Outcome {
                            index,
                            status: CmpStatus::Error,
                            diff_side: None,
                            binary: false,
                            error: Some(error),
                        });
                    }
                    return;
                }
            };
            for e in entries {
                let item = merged
                    .entry(name_key(&e.name, e.is_dir))
                    .or_insert_with(|| DirItem {
                        rel_dir: rel.to_string(),
                        name: e.name.clone(),
                        is_dir: e.is_dir,
                        exists: [false; 3],
                        size: [None; 3],
                        mtime: [None; 3],
                        status: CmpStatus::Unique,
                        diff_side: None,
                        binary: false,
                        error: None,
                        depth,
                        parent,
                        children: Vec::new(),
                    });
                item.exists[side] = true;
                if !e.is_dir {
                    item.size[side] = Some(e.size);
                }
                item.mtime[side] = e.mtime;
            }
        }
        *self.progress.current.lock().unwrap() = rel.to_string();
        for (_, item) in merged {
            let rel_path = item.rel_path();
            let keep = if item.is_dir {
                self.filter.include_dir(&rel_path, &item.name)
            } else {
                self.filter.include_file(&rel_path, &item.name)
            };
            if !keep {
                continue;
            }
            let idx = items.len();
            if let Some(p) = parent {
                items[p].children.push(idx);
            }
            let mut item = item;
            let all = item.exists[..n].iter().all(|&e| e);
            if item.is_dir && all {
                // Until scanned, a folder must not look like an empty, identical one.
                item.status = if self.fopts.recursive {
                    CmpStatus::Pending
                } else {
                    CmpStatus::NotCompared
                };
            } else if !item.is_dir && all {
                item.status = CmpStatus::Pending;
                self.progress.total.fetch_add(1, Ordering::Relaxed);
                queue.push((idx, item.clone()));
            }
            if self.progress.live {
                self.progress
                    .updates
                    .lock()
                    .unwrap()
                    .items
                    .push(item.clone());
            }
            let (is_dir, exists) = (item.is_dir, item.exists);
            items.push(item);
            if is_dir && self.fopts.recursive && (all || self.fopts.include_unique_subfolders) {
                self.scan(&rel_path, Some(idx), depth + 1, exists, items, queue);
                if all && !self.progress.abort.load(Ordering::Relaxed) {
                    self.scanned(idx, items);
                }
            }
        }
    }

    /// Marks a folder as fully scanned; `aggregate` derives its status from the children.
    fn scanned(&self, index: usize, items: &mut [DirItem]) {
        if items[index].error.is_some() {
            return;
        }
        items[index].status = CmpStatus::Identical;
        if self.progress.live {
            let o = Outcome {
                index,
                status: CmpStatus::Identical,
                diff_side: None,
                binary: false,
                error: None,
            };
            self.progress.updates.lock().unwrap().results.push(o);
        }
    }
}

/// Recomputes folder statuses from their children (post-order).
pub fn aggregate(items: &mut [DirItem], panes: usize) {
    for i in (0..items.len()).rev() {
        if !items[i].is_dir || items[i].error.is_some() {
            continue;
        }
        let all = items[i].exists[..panes].iter().all(|&e| e);
        if !all {
            items[i].status = CmpStatus::Unique;
            continue;
        }
        if matches!(items[i].status, CmpStatus::NotCompared | CmpStatus::Pending)
            && items[i].children.is_empty()
        {
            continue;
        }
        let (mut pending, mut error, mut skipped) = (false, false, false);
        let mut st = CmpStatus::Identical;
        for &c in &items[i].children {
            match items[c].status {
                CmpStatus::Different | CmpStatus::Unique => {
                    st = CmpStatus::Different;
                    break;
                }
                CmpStatus::Pending => pending = true,
                CmpStatus::Error => error = true,
                CmpStatus::NotCompared => skipped = true,
                CmpStatus::Identical => {}
            }
        }
        if st != CmpStatus::Different {
            st = if pending {
                CmpStatus::Pending
            } else if error {
                CmpStatus::Error
            } else if skipped {
                CmpStatus::NotCompared
            } else {
                CmpStatus::Identical
            };
        }
        items[i].status = st;
    }
}

/// Reads a whole file; `size` from the scan saves asking the file system for it again.
fn read_file(path: &Path, size: Option<u64>) -> Result<Vec<u8>> {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut f = std::fs::File::open(path)?;
        let mut bytes = Vec::with_capacity(size.map_or(0, |s| s as usize + 1));
        f.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    read().with_context(|| format!("cannot read {}", path.display()))
}

/// Byte comparison of two files with the sizes found by the scan.
fn same_bytes(a: &Path, b: &Path, size_a: Option<u64>, size_b: Option<u64>) -> Result<bool> {
    if size_a != size_b {
        return Ok(false);
    }
    let open =
        |p: &Path| std::fs::File::open(p).with_context(|| format!("cannot read {}", p.display()));
    let (mut fa, mut fb) = (open(a)?, open(b)?);
    // One spare byte, so a small file is done after a single read.
    let chunk = size_a.map_or(CHUNK, |s| (s as usize).saturating_add(1).min(CHUNK));
    let (mut ba, mut bb) = (vec![0u8; chunk], vec![0u8; chunk]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na < chunk {
            return Ok(true);
        }
    }
}

/// Binary detection from the start of a file, without reading all of it.
fn head_is_binary(path: &Path) -> Result<bool> {
    let mut f =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut head = vec![0u8; 8192];
    let n = read_full(&mut f, &mut head)?;
    Ok(text::looks_binary(&head[..n]))
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = r.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

fn time_eq(a: Option<SystemTime>, b: Option<SystemTime>, tol: u64) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let d = a
                .duration_since(b)
                .or_else(|_| b.duration_since(a))
                .unwrap_or_default();
            d.as_secs() <= tol
        }
        _ => false,
    }
}

/// Read size for comparing files in chunks.
const CHUNK: usize = 1 << 20;

/// Full-contents compares read files up to this size in one go. Larger files are compared in
/// chunks first, so identical and binary files never have to fit in memory; only large text
/// files whose bytes differ are loaded to apply the ignore options.
const LOAD_LIMIT: u64 = 16 << 20;

struct Loaded {
    bytes: Vec<u8>,
    binary: bool,
}

impl Loaded {
    fn read(path: &Path, size: Option<u64>) -> Result<Loaded> {
        let bytes = read_file(path, size)?;
        let binary = text::looks_binary(&bytes);
        Ok(Loaded { bytes, binary })
    }
}

/// Text aware equality honoring the ignore options. `path` (of either side) tells the
/// language for ignoring comments and the format for reformatting JSON and XML.
fn text_equal(a: &Loaded, b: &Loaded, norm: &Normalizer, dopts: &DiffOptions, path: &Path) -> bool {
    if a.bytes == b.bytes {
        return true;
    }
    if a.binary || b.binary {
        return false;
    }
    let (mut ta, enc_a, bom_a) = text::decode_cow(&a.bytes, None);
    let (mut tb, enc_b, bom_b) = text::decode_cow(&b.bytes, None);
    if !dopts.ignore_codepage && (enc_a != enc_b || bom_a != bom_b) {
        return false;
    }
    let formatted = dopts
        .prettify
        .then(|| prettify::detect(Some(path), &ta))
        .flatten()
        .and_then(|f| Some((prettify::format(&ta, f)?, prettify::format(&tb, f)?)));
    if let Some((fa, fb)) = formatted {
        (ta, tb) = (Cow::Owned(fa), Cow::Owned(fb));
    } else if !dopts.ignore_eol {
        let (ea, eb) = (text::EolCounts::of(&ta), text::EolCounts::of(&tb));
        let eol = |c: &text::EolCounts| c.dominant().unwrap_or_else(text::Eol::platform);
        if eol(&ea) != eol(&eb) || ea.mixed() || eb.mixed() {
            return false;
        }
    }
    if ta.ends_with(['\r', '\n']) != tb.ends_with(['\r', '\n']) {
        return false;
    }
    if norm.needs_diff() {
        let lines = |t: &str| -> Vec<String> { text::lines(t).map(|(l, _)| l.into()).collect() };
        let (la, lb) = (lines(&ta), lines(&tb));
        let opts = DiffOptions {
            detect_moved: false,
            ..dopts.clone()
        };
        return super::diff::compute_with(&[&la, &lb], &opts, comments::for_path(path))
            .is_identical();
    }
    match comments::for_path(path).filter(|_| dopts.ignore_comments) {
        Some(syn) => {
            let code = |t: &str| -> Vec<String> {
                let lines: Vec<&str> = text::lines(t).map(|(l, _)| l).collect();
                comments::strip(&lines, syn)
                    .into_iter()
                    .filter(|s| !s.comment_only && !norm.is_ignorable(&s.code))
                    .map(|s| norm.key(&s.code).into_owned())
                    .collect()
            };
            code(&ta) == code(&tb)
        }
        None => line_keys(&ta, norm).eq(line_keys(&tb, norm)),
    }
}

fn line_keys<'a>(text: &'a str, norm: &'a Normalizer) -> impl Iterator<Item = Cow<'a, str>> {
    text::lines(text)
        .map(|(l, _)| l)
        .filter(|l| !norm.is_ignorable(l))
        .map(|l| norm.key(l))
}

/// Compares the sides pairwise; left and right are equal when both equal the middle.
fn pairwise(
    n: usize,
    binary: bool,
    mut eq: impl FnMut(usize, usize) -> Result<bool>,
) -> Result<(CmpStatus, Option<usize>, bool)> {
    let e01 = eq(0, 1)?;
    if n == 2 {
        return Ok(three_way_status(2, e01, false, false, binary));
    }
    let e12 = eq(1, 2)?;
    let e02 = if e01 && e12 { true } else { eq(0, 2)? };
    Ok(three_way_status(3, e01, e12, e02, binary))
}

fn full_contents(
    paths: &[PathBuf],
    item: &DirItem,
    norm: &Normalizer,
    dopts: &DiffOptions,
    load_limit: u64,
) -> Result<(CmpStatus, Option<usize>, bool)> {
    let n = paths.len();
    if item.size[..n]
        .iter()
        .all(|s| s.is_some_and(|s| s <= load_limit))
    {
        let loaded: Vec<Loaded> = paths
            .iter()
            .zip(item.size)
            .map(|(p, size)| Loaded::read(p, size))
            .collect::<Result<_>>()?;
        let any_bin = loaded.iter().any(|l| l.binary);
        return pairwise(n, any_bin, |a, b| {
            Ok(text_equal(&loaded[a], &loaded[b], norm, dopts, &paths[a]))
        });
    }
    let binary: Vec<bool> = paths
        .iter()
        .map(|p| head_is_binary(p))
        .collect::<Result<_>>()?;
    let mut loaded: Vec<Option<Loaded>> = paths.iter().map(|_| None).collect();
    pairwise(n, binary.contains(&true), |a, b| {
        if same_bytes(&paths[a], &paths[b], item.size[a], item.size[b])? {
            return Ok(true);
        }
        if binary[a] || binary[b] {
            return Ok(false);
        }
        for s in [a, b] {
            if loaded[s].is_none() {
                loaded[s] = Some(Loaded::read(&paths[s], item.size[s])?);
            }
        }
        let (la, lb) = (loaded[a].as_ref().unwrap(), loaded[b].as_ref().unwrap());
        Ok(text_equal(la, lb, norm, dopts, &paths[a]))
    })
}

/// Returns (status, differing side for 3-way, is binary). `norm` must be built from `dopts`.
pub fn compare_files(
    paths: &[PathBuf],
    item: &DirItem,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    norm: &Normalizer,
) -> Result<(CmpStatus, Option<usize>, bool)> {
    compare_files_with_cancel(paths, item, fopts, dopts, norm, None)
}

fn compare_files_with_cancel(
    paths: &[PathBuf],
    item: &DirItem,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    norm: &Normalizer,
    cancel: Option<&AtomicBool>,
) -> Result<(CmpStatus, Option<usize>, bool)> {
    let n = paths.len();
    if fopts.method == CompareMethod::FullContents
        && paths
            .iter()
            .any(|p| super::unpacker::find(&dopts.unpackers, p).is_some())
    {
        let docs = paths
            .iter()
            .map(|p| super::unpacker::load_compare_doc(p, dopts, cancel))
            .collect::<Result<Vec<_>>>()?;
        return pairwise(n, false, |a, b| {
            let refs = [docs[a].lines.as_slice(), docs[b].lines.as_slice()];
            let mut result = super::diff::compute_with(&refs, dopts, comments::for_path(&paths[a]));
            super::diff::include_doc_formats(&mut result, &[&docs[a], &docs[b]], dopts);
            Ok(result.is_identical())
        });
    }
    let tol = fopts.time_tolerance_secs;
    let (size, mtime) = (&item.size, &item.mtime);
    match fopts.method {
        CompareMethod::Size => pairwise(n, false, |a, b| Ok(size[a] == size[b])),
        CompareMethod::Date => pairwise(n, false, |a, b| Ok(time_eq(mtime[a], mtime[b], tol))),
        CompareMethod::DateSize => pairwise(n, false, |a, b| {
            Ok(size[a] == size[b] && time_eq(mtime[a], mtime[b], tol))
        }),
        CompareMethod::QuickContents | CompareMethod::BinaryContents => {
            let binary = fopts.method == CompareMethod::BinaryContents;
            pairwise(n, binary, |a, b| {
                same_bytes(&paths[a], &paths[b], size[a], size[b])
            })
        }
        CompareMethod::FullContents => full_contents(paths, item, norm, dopts, LOAD_LIMIT),
    }
}

fn three_way_status(
    n: usize,
    e01: bool,
    e12: bool,
    e02: bool,
    binary: bool,
) -> (CmpStatus, Option<usize>, bool) {
    if n == 2 {
        let st = if e01 {
            CmpStatus::Identical
        } else {
            CmpStatus::Different
        };
        return (st, None, binary);
    }
    if e01 && e12 {
        return (CmpStatus::Identical, None, binary);
    }
    let side = if e02 {
        Some(1)
    } else if e01 {
        Some(2)
    } else if e12 {
        Some(0)
    } else {
        None
    };
    (CmpStatus::Different, side, binary)
}

/// Re-reads metadata and recompares a single file item in place.
pub fn refresh_file_item(
    items: &mut [DirItem],
    idx: usize,
    roots: &[PathBuf],
    fopts: &FolderOptions,
    dopts: &DiffOptions,
) {
    let n = roots.len();
    {
        let it = &mut items[idx];
        for (side, root) in roots.iter().enumerate().take(n) {
            let p = it.path(root);
            match std::fs::metadata(&p) {
                Ok(md) => {
                    it.exists[side] = true;
                    it.size[side] = if md.is_dir() { None } else { Some(md.len()) };
                    it.mtime[side] = md.modified().ok();
                }
                Err(_) => {
                    it.exists[side] = false;
                    it.size[side] = None;
                    it.mtime[side] = None;
                }
            }
        }
        if !it.is_dir {
            if it.exists[..n].iter().all(|&e| e) {
                let paths: Vec<PathBuf> = roots.iter().map(|r| it.path(r)).collect();
                let snapshot = it.clone();
                let norm = Normalizer::new(dopts);
                match compare_files(&paths, &snapshot, fopts, dopts, &norm) {
                    Ok((st, side, bin)) => {
                        it.status = st;
                        it.diff_side = side;
                        it.binary = bin;
                        it.error = None;
                    }
                    Err(e) => {
                        it.status = CmpStatus::Error;
                        it.error = Some(e.to_string());
                    }
                }
            } else {
                it.status = CmpStatus::Unique;
                it.diff_side = None;
            }
        }
    }
    aggregate(items, n);
}

/// Copies a file or folder over `to`. Refuses to copy a file onto itself (also through a link),
/// which would empty it, and a folder into itself, which would never end.
pub fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    let src = std::fs::canonicalize(from).with_context(|| format!("{}", from.display()))?;
    let mut dest = to.to_path_buf();
    let mut missing = Vec::new();
    let dest = loop {
        match std::fs::canonicalize(&dest) {
            Ok(d) => break missing.into_iter().rev().fold(d, |d, n| d.join(n)),
            Err(_) => match (dest.parent(), dest.file_name()) {
                (Some(parent), Some(name)) => {
                    missing.push(name.to_owned());
                    dest = parent.to_path_buf();
                }
                _ => break to.to_path_buf(),
            },
        }
    };
    if dest == src
        || (src.is_dir() && dest.starts_with(&src))
        || same_file::is_same_file(from, to).unwrap_or(false)
    {
        bail!(
            "{}: {} -> {}",
            tr("msg.copy_into_itself"),
            from.display(),
            to.display()
        );
    }
    copy_tree(from, to, &mut Vec::new())
}

/// `inside` holds the folders being copied around `from`, so a link back to one of them stops
/// the copy instead of repeating it forever.
fn copy_tree(from: &Path, to: &Path, inside: &mut Vec<PathBuf>) -> Result<()> {
    let md = std::fs::metadata(from).with_context(|| format!("{}", from.display()))?;
    if md.is_dir() {
        let real = std::fs::canonicalize(from)?;
        if inside.contains(&real) {
            bail!("{}: {}", tr("msg.copy_into_itself"), from.display());
        }
        std::fs::create_dir_all(to)?;
        inside.push(real);
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()), inside)?;
        }
        inside.pop();
    } else {
        super::fsutil::copy_atomic(from, to, false)?;
    }
    Ok(())
}

pub fn delete_path(path: &Path, use_recycle_bin: bool) -> Result<()> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    if use_recycle_bin {
        // Never deleted for good instead: whoever chose the trash expects to get it back.
        return trash::delete(path).context("not moved to the trash, so not deleted");
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn status_counts(items: &[DirItem]) -> (usize, usize, usize, usize) {
    let (mut same, mut diff, mut uniq, mut err) = (0, 0, 0, 0);
    for it in items {
        if it.is_dir {
            if it.status == CmpStatus::Error && it.error.is_some() {
                err += 1;
            }
            continue;
        }
        match it.status {
            CmpStatus::Identical => same += 1,
            CmpStatus::Different => diff += 1,
            CmpStatus::Unique => uniq += 1,
            CmpStatus::Error => err += 1,
            CmpStatus::NotCompared | CmpStatus::Pending => {}
        }
    }
    (same, diff, uniq, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_copy_onto_itself_or_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::write(root.join("sub/f.txt"), "keep\n").unwrap();
        assert!(copy_recursive(&root.join("sub/f.txt"), &root.join("sub/./f.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("sub/f.txt")).unwrap(),
            "keep\n"
        );
        assert!(copy_recursive(&root.join("sub"), &root.join("sub/deeper/new/sub")).is_err());
        copy_recursive(&root.join("sub"), &root.join("copy/sub")).unwrap();
        assert!(root.join("copy/sub/f.txt").is_file());
        copy_recursive(&root.join("sub/f.txt"), &root.join("copy/sub/f.txt")).unwrap();

        std::fs::hard_link(root.join("sub/f.txt"), root.join("hard.txt")).unwrap();
        assert!(copy_recursive(&root.join("sub/f.txt"), &root.join("hard.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("hard.txt")).unwrap(),
            "keep\n"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("sub"), root.join("sub/deeper/up")).unwrap();
            assert!(copy_recursive(&root.join("sub"), &root.join("looped")).is_err());
        }
    }

    #[test]
    fn folder_compare() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("same.txt"), "x\n").unwrap();
        std::fs::write(b.path().join("same.txt"), "x\r\n").unwrap();
        std::fs::write(a.path().join("diff.txt"), "1\n").unwrap();
        std::fs::write(b.path().join("diff.txt"), "2\n").unwrap();
        std::fs::create_dir(a.path().join("sub")).unwrap();
        std::fs::write(a.path().join("sub").join("only.txt"), "o").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let progress = Progress::default();
        let filter = FileFilter::default();
        let fopts = FolderOptions::default();
        let dopts = DiffOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &filter,
            fopts: &fopts,
            dopts: &dopts,
            progress: &progress,
        }
        .run();
        let find = |n: &str| items.iter().find(|i| i.name == n).unwrap();
        assert_eq!(find("same.txt").status, CmpStatus::Identical);
        assert_eq!(find("diff.txt").status, CmpStatus::Different);
        assert_eq!(find("sub").status, CmpStatus::Unique);
        assert_eq!(find("only.txt").status, CmpStatus::Unique);
    }

    #[test]
    fn streamed_updates_rebuild_the_result() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for root in [a.path(), b.path()] {
            std::fs::create_dir_all(root.join("deep").join("er")).unwrap();
            std::fs::create_dir(root.join("empty")).unwrap();
            std::fs::create_dir(root.join("same")).unwrap();
            std::fs::write(root.join("same").join("s.txt"), "s").unwrap();
        }
        std::fs::write(a.path().join("deep").join("er").join("d.txt"), "1").unwrap();
        std::fs::write(b.path().join("deep").join("er").join("d.txt"), "2").unwrap();
        std::fs::write(a.path().join("deep").join("u.txt"), "u").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let progress = Progress::live();
        let filter = FileFilter::default();
        let fopts = FolderOptions::default();
        let dopts = DiffOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &filter,
            fopts: &fopts,
            dopts: &dopts,
            progress: &progress,
        }
        .run();

        let updates = progress.take_updates();
        let mut streamed: Vec<DirItem> = Vec::new();
        for it in updates.items {
            if let Some(p) = it.parent {
                let idx = streamed.len();
                streamed[p].children.push(idx);
            }
            streamed.push(it);
        }
        for o in updates.results {
            o.apply(&mut streamed);
        }
        aggregate(&mut streamed, 2);

        let summary = |v: &[DirItem]| -> Vec<(String, CmpStatus, Vec<usize>)> {
            v.iter()
                .map(|i| (i.rel_path(), i.status, i.children.clone()))
                .collect()
        };
        assert_eq!(summary(&streamed), summary(&items));
        let find = |n: &str| items.iter().find(|i| i.rel_path() == n).unwrap();
        assert_eq!(find("deep").status, CmpStatus::Different);
        assert_eq!(find("deep/er").status, CmpStatus::Different);
        assert_eq!(find("empty").status, CmpStatus::Identical);
        assert_eq!(find("same").status, CmpStatus::Identical);
        assert_eq!(progress.total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn large_and_small_files_compare_alike() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let text: String = (0..60_000).map(|i| format!("line {i}\n")).collect();
        let mut binary = vec![0u8; 2 * CHUNK];
        binary[5] = 1;
        let mut binary2 = binary.clone();
        *binary2.last_mut().unwrap() = 9;
        let cases = [
            (
                "same",
                text.as_bytes().to_vec(),
                text.as_bytes().to_vec(),
                true,
            ),
            (
                "eol",
                text.as_bytes().to_vec(),
                text.replace('\n', "\r\n").into_bytes(),
                true,
            ),
            (
                "text",
                text.as_bytes().to_vec(),
                text.replace("line 7\n", "x\n").into_bytes(),
                false,
            ),
            ("bin", binary.clone(), binary.clone(), true),
            ("bin2", binary, binary2, false),
        ];
        let dopts = DiffOptions::default();
        let norm = Normalizer::new(&dopts);
        for (name, a, b, equal) in cases {
            let paths = vec![
                write(&format!("{name}.a"), &a),
                write(&format!("{name}.b"), &b),
            ];
            let mut item = DirItem {
                rel_dir: String::new(),
                name: name.into(),
                is_dir: false,
                exists: [true, true, false],
                size: [Some(a.len() as u64), Some(b.len() as u64), None],
                mtime: [None; 3],
                status: CmpStatus::Pending,
                diff_side: None,
                binary: false,
                error: None,
                depth: 0,
                parent: None,
                children: Vec::new(),
            };
            let expected = if equal {
                CmpStatus::Identical
            } else {
                CmpStatus::Different
            };
            for limit in [0, LOAD_LIMIT] {
                let (status, _, _) = full_contents(&paths, &item, &norm, &dopts, limit).unwrap();
                assert_eq!(status, expected, "{name} with load limit {limit}");
            }
            item.size[1] = item.size[0];
            let quick = same_bytes(&paths[0], &paths[1], item.size[0], item.size[1]).unwrap();
            assert_eq!(quick, a == b, "{name} byte compare");
        }
    }

    #[test]
    fn unscanned_folders_stay_pending() {
        let mut items = vec![DirItem {
            rel_dir: String::new(),
            name: "dir".into(),
            is_dir: true,
            exists: [true, true, false],
            size: [None; 3],
            mtime: [None; 3],
            status: CmpStatus::Pending,
            diff_side: None,
            binary: false,
            error: None,
            depth: 0,
            parent: None,
            children: Vec::new(),
        }];
        aggregate(&mut items, 2);
        assert_eq!(items[0].status, CmpStatus::Pending);
    }
}
