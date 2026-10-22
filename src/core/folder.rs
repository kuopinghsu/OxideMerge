//! Folder comparison (2-way and 3-way) and folder merge operations.

use super::diff::{DiffOptions, Normalizer};
use super::filter::FileFilter;
use super::text;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
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
    /// Folder whose contents were not compared (non-recursive mode).
    NotCompared,
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

fn list_dir(dir: &Path, skip_hidden: bool) -> Option<Vec<Entry>> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut v = Vec::new();
    for e in rd.flatten() {
        if skip_hidden && is_hidden(&e) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        v.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
            mtime: md.modified().ok(),
        });
    }
    Some(v)
}

fn name_key(name: &str, is_dir: bool) -> (bool, String) {
    let n = if cfg!(windows) || cfg!(target_os = "macos") { name.to_lowercase() } else { name.to_string() };
    (!is_dir, n)
}

/// Item index, status, differing side, binary flag and error message of one file comparison.
type CompareOutcome = (usize, CmpStatus, Option<usize>, bool, Option<String>);

pub struct Scanner<'a> {
    pub roots: &'a [PathBuf],
    pub filter: &'a FileFilter,
    pub fopts: &'a FolderOptions,
    pub dopts: &'a DiffOptions,
    pub progress: &'a Progress,
}

impl<'a> Scanner<'a> {
    pub fn run(&self) -> Vec<DirItem> {
        let mut items = Vec::new();
        self.scan("", None, 0, [true; 3], &mut items);
        if self.progress.abort.load(Ordering::Relaxed) {
            return items;
        }
        let todo: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| !it.is_dir && it.exists[..self.roots.len()].iter().all(|&e| e))
            .map(|(i, _)| i)
            .collect();
        self.progress.total.store(todo.len(), Ordering::Relaxed);
        self.compare_all(&mut items, &todo);
        aggregate(&mut items, self.roots.len());
        items
    }

    fn scan(&self, rel: &str, parent: Option<usize>, depth: usize, present: [bool; 3], items: &mut Vec<DirItem>) {
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
            let Some(entries) = list_dir(&dir, self.fopts.skip_hidden) else { continue };
            for e in entries {
                let item = merged.entry(name_key(&e.name, e.is_dir)).or_insert_with(|| DirItem {
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
            let is_dir = item.is_dir;
            let exists = item.exists;
            items.push(item);
            if is_dir {
                let all = exists[..n].iter().all(|&e| e);
                if !self.fopts.recursive {
                    items[idx].status = if all { CmpStatus::NotCompared } else { CmpStatus::Unique };
                } else if all || self.fopts.include_unique_subfolders {
                    self.scan(&rel_path, Some(idx), depth + 1, exists, items);
                }
            }
        }
    }

    fn compare_all(&self, items: &mut [DirItem], todo: &[usize]) {
        let threads = if self.fopts.threads > 0 {
            self.fopts.threads
        } else {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8)
        };
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<CompareOutcome>> = Mutex::new(Vec::new());
        let items_ref: &[DirItem] = items;
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| loop {
                    if self.progress.abort.load(Ordering::Relaxed) {
                        break;
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= todo.len() {
                        break;
                    }
                    let idx = todo[k];
                    let it = &items_ref[idx];
                    let paths: Vec<PathBuf> = self.roots.iter().map(|r| it.path(r)).collect();
                    let r = compare_files(&paths, it, self.fopts, self.dopts);
                    let entry = match r {
                        Ok((st, side, bin)) => (idx, st, side, bin, None),
                        Err(e) => (idx, CmpStatus::Error, None, false, Some(e.to_string())),
                    };
                    results.lock().unwrap().push(entry);
                    self.progress.done.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        for (idx, st, side, bin, err) in results.into_inner().unwrap() {
            items[idx].status = st;
            items[idx].diff_side = side;
            items[idx].binary = bin;
            items[idx].error = err;
        }
    }
}

/// Recomputes folder statuses from their children (post-order).
pub fn aggregate(items: &mut [DirItem], panes: usize) {
    for i in (0..items.len()).rev() {
        if !items[i].is_dir {
            continue;
        }
        let all = items[i].exists[..panes].iter().all(|&e| e);
        if !all {
            items[i].status = CmpStatus::Unique;
            continue;
        }
        if items[i].status == CmpStatus::NotCompared && items[i].children.is_empty() {
            continue;
        }
        let mut st = CmpStatus::Identical;
        for &c in &items[i].children {
            match items[c].status {
                CmpStatus::Different | CmpStatus::Unique => {
                    st = CmpStatus::Different;
                    break;
                }
                CmpStatus::Error => st = CmpStatus::Error,
                _ => {}
            }
        }
        items[i].status = st;
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))
}

fn same_bytes(a: &Path, b: &Path) -> Result<bool> {
    let ma = std::fs::metadata(a)?;
    let mb = std::fs::metadata(b)?;
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let mut fa = std::io::BufReader::new(std::fs::File::open(a)?);
    let mut fb = std::io::BufReader::new(std::fs::File::open(b)?);
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
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
            let d = a.duration_since(b).or_else(|_| b.duration_since(a)).unwrap_or_default();
            d.as_secs() <= tol
        }
        _ => false,
    }
}

struct Loaded {
    bytes: Vec<u8>,
    binary: bool,
}

/// Text aware equality honoring the ignore options.
fn text_equal(a: &Loaded, b: &Loaded, norm: &Normalizer, dopts: &DiffOptions) -> bool {
    if a.bytes == b.bytes {
        return true;
    }
    if a.binary || b.binary {
        return false;
    }
    let da = text::doc_from_bytes(None, &a.bytes, None);
    let db = text::doc_from_bytes(None, &b.bytes, None);
    if !dopts.ignore_eol && (da.eol != db.eol || da.mixed_eol || db.mixed_eol) {
        return false;
    }
    if !dopts.ignore_codepage && (da.encoding != db.encoding || da.bom != db.bom) {
        return false;
    }
    let ka = da.lines.iter().filter(|l| !norm.is_ignorable(l)).map(|l| norm.key(l));
    let kb = db.lines.iter().filter(|l| !norm.is_ignorable(l)).map(|l| norm.key(l));
    ka.eq(kb)
}

/// Returns (status, differing side for 3-way, is binary).
pub fn compare_files(
    paths: &[PathBuf],
    item: &DirItem,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
) -> Result<(CmpStatus, Option<usize>, bool)> {
    let n = paths.len();
    let pair_eq: Box<dyn Fn(usize, usize) -> Result<bool>> = match fopts.method {
        CompareMethod::Size => Box::new(|a, b| Ok(item.size[a] == item.size[b])),
        CompareMethod::Date => {
            Box::new(|a, b| Ok(time_eq(item.mtime[a], item.mtime[b], fopts.time_tolerance_secs)))
        }
        CompareMethod::DateSize => Box::new(|a, b| {
            Ok(item.size[a] == item.size[b] && time_eq(item.mtime[a], item.mtime[b], fopts.time_tolerance_secs))
        }),
        CompareMethod::QuickContents | CompareMethod::BinaryContents => {
            Box::new(|a, b| same_bytes(&paths[a], &paths[b]))
        }
        CompareMethod::FullContents => {
            let mut loaded = Vec::new();
            for p in paths {
                let bytes = read_file(p)?;
                let binary = text::looks_binary(&bytes);
                loaded.push(Loaded { bytes, binary });
            }
            let norm = Normalizer::new(dopts);
            let any_bin = loaded.iter().any(|l| l.binary);
            let eqs: Vec<bool> = [(0, 1), (1, 2), (0, 2)]
                .iter()
                .map(|&(a, b)| b < n && text_equal(&loaded[a], &loaded[b], &norm, dopts))
                .collect();
            return Ok(three_way_status(n, eqs[0], eqs[1], eqs[2], any_bin));
        }
    };
    let binary = matches!(fopts.method, CompareMethod::BinaryContents);
    let e01 = pair_eq(0, 1)?;
    if n == 2 {
        return Ok(three_way_status(2, e01, false, false, binary));
    }
    let e12 = pair_eq(1, 2)?;
    let e02 = if e01 && e12 { true } else { pair_eq(0, 2)? };
    Ok(three_way_status(3, e01, e12, e02, binary))
}

fn three_way_status(n: usize, e01: bool, e12: bool, e02: bool, binary: bool) -> (CmpStatus, Option<usize>, bool) {
    if n == 2 {
        let st = if e01 { CmpStatus::Identical } else { CmpStatus::Different };
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
                match compare_files(&paths, &snapshot, fopts, dopts) {
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

pub fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    let md = std::fs::metadata(from).with_context(|| format!("{}", from.display()))?;
    if md.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if to.exists() {
            let mut perm = std::fs::metadata(to)?.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                std::fs::set_permissions(to, perm).ok();
            }
        }
        std::fs::copy(from, to).with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
    }
    Ok(())
}

pub fn delete_path(path: &Path, use_recycle_bin: bool) -> Result<()> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    if use_recycle_bin && trash::delete(path).is_ok() {
        return Ok(());
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
    for it in items.iter().filter(|i| !i.is_dir) {
        match it.status {
            CmpStatus::Identical => same += 1,
            CmpStatus::Different => diff += 1,
            CmpStatus::Unique => uniq += 1,
            CmpStatus::Error => err += 1,
            CmpStatus::NotCompared => {}
        }
    }
    (same, diff, uniq, err)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let items = Scanner { roots: &roots, filter: &filter, fopts: &fopts, dopts: &dopts, progress: &progress }.run();
        let find = |n: &str| items.iter().find(|i| i.name == n).unwrap();
        assert_eq!(find("same.txt").status, CmpStatus::Identical);
        assert_eq!(find("diff.txt").status, CmpStatus::Different);
        assert_eq!(find("sub").status, CmpStatus::Unique);
        assert_eq!(find("only.txt").status, CmpStatus::Unique);
    }
}
