//! Folder compare view.

use crate::config::Config;
use crate::core::diff::{self, DiffOptions};
use crate::core::filter::{FileFilter, FltFilter};
use crate::core::folder::{self, CmpStatus, DirItem, FolderOptions, Progress, Scanner};
use crate::core::folder_merge::{self, Step};
use crate::core::patch::{self, PatchStyle};
use crate::core::report::{self, fmt_size, fmt_time, side_name, status_text, ReportFormat};
use crate::core::text::{self, FileKind, TextDoc};
use crate::i18n::{tr, trf};
use crate::ui::text_view::TextView;
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, Icon, Outcome};
use egui::{vec2, Align, Color32, FontId, Key, Layout, Margin, Modifiers, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub enum FolderAction {
    OpenFiles {
        paths: Vec<PathBuf>,
        missing: Vec<bool>,
        kind: Option<FileKind>,
        /// Relative path of the item, to find the next or previous file from it later.
        rel: String,
    },
    OpenFolders {
        paths: Vec<PathBuf>,
    },
    /// The filter was changed from the folder view; it becomes the default for new compares.
    FilterChanged(String),
    /// The compare method or subfolder option was changed from the folder view.
    OptionsChanged {
        method: folder::CompareMethod,
        recursive: bool,
    },
    /// Turn every "Show ... items" view option back on.
    ShowAll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Folder,
    Result,
    Date(usize),
    Size(usize),
    Ext,
}

#[derive(Clone)]
enum OpKind {
    Copy { from: usize, to: usize },
    Delete { side: Option<usize> },
}

struct PendingOp {
    kind: OpKind,
    targets: Vec<usize>,
}

enum MenuCmd {
    Open(usize, Option<FileKind>),
    Op(OpKind),
    SystemOpen(usize, usize),
    Reveal(usize, usize),
    CopyPath(usize, usize),
    CopyTo(usize),
    SelectPattern,
}

/// How often a running comparison refreshes the list with what it has found so far, at most.
const UPDATE_INTERVAL: Duration = Duration::from_millis(250);

struct Running {
    progress: Arc<Progress>,
    handle: JoinHandle<Vec<DirItem>>,
    started: Instant,
    updated: Instant,
}

pub struct FolderView {
    pub roots: Vec<PathBuf>,
    pub display_roots: Vec<String>,
    _temps: Vec<tempfile::TempDir>,
    pub items: Vec<DirItem>,
    pub filter_spec: String,
    filter: FileFilter,
    fopts: FolderOptions,
    dopts: DiffOptions,
    running: Option<Running>,
    elapsed: Option<Duration>,
    expanded: HashSet<String>,
    selected: BTreeSet<usize>,
    /// Start of a Shift range selection, as an index into `visible`.
    anchor: Option<usize>,
    /// Keyboard focus row, as an index into `visible`.
    cursor: Option<usize>,
    sort: (Col, bool),
    visible: Vec<usize>,
    view_key: Option<(bool, bool, bool, [bool; 3])>,
    /// Time spent on the last list refresh; a running comparison refreshes less often when the
    /// list is expensive to rebuild.
    view_cost: Duration,
    pub status: String,
    confirm: Option<PendingOp>,
    scroll_to: Option<usize>,
    /// Text of the filter field in the filter popup, while it is open.
    filter_edit: Option<String>,
    /// Only items whose name contains this (case-insensitive) are listed, with their folders.
    search: String,
    focus_search: bool,
    /// Quick preview of the item under the cursor (Space).
    peek: bool,
    peek_cache: Option<(usize, Vec<(PeekKind, String)>)>,
    /// Duration of a comparison that just finished, until the app takes it.
    finished: Option<Duration>,
    /// The item shown in the docked file compare, and how far its compare has got.
    dock: Option<(usize, Dock)>,
    /// Text of the "select by name" dialog while it is open.
    select_pattern: Option<String>,
    /// A 3-way auto merge being planned, then waiting for confirmation.
    merge_job: Option<JoinHandle<folder_merge::Plan>>,
    merge_plan: Option<folder_merge::Plan>,
    id: egui::Id,
}

#[derive(Clone, Copy)]
enum PeekKind {
    Plain,
    Meta,
    Hunk,
    Add,
    Del,
}

/// Files larger than this are not loaded for the quick preview.
const PEEK_MAX_BYTES: u64 = 4 << 20;
const PEEK_MAX_LINES: usize = 5000;
/// Files larger than this are not loaded into the docked file compare.
const DOCK_MAX_BYTES: u64 = 8 << 20;
/// The docked file compare loads once the selection has stayed this long on an item, so
/// moving through the list with the arrow keys does not load every file on the way.
const DOCK_DELAY: Duration = Duration::from_millis(150);

enum Dock {
    Waiting(Instant),
    Loading(std::sync::mpsc::Receiver<Option<Vec<TextDoc>>>),
    /// The compare, or `None` when the item is not a text file.
    Ready(Option<Box<TextView>>),
}

/// The sides of `it` as read-only documents for the docked file compare, or `None` when it
/// is not a text file small enough to show.
fn load_dock_docs(it: &DirItem, roots: &[PathBuf]) -> Option<Vec<TextDoc>> {
    let n = roots.len();
    if it.is_dir || it.binary || (0..n).any(|s| it.size[s].unwrap_or(0) > DOCK_MAX_BYTES) {
        return None;
    }
    (0..n)
        .map(|s| {
            let p = it.path(&roots[s]);
            let mut d = if it.exists[s] {
                if text::detect_kind(&p) == FileKind::Binary {
                    return None;
                }
                text::load_doc(&p, None).ok()?
            } else {
                let mut d = TextDoc::empty(None);
                d.description = Some(format!("{} — {}", p.display(), tr("peek.missing")));
                d
            };
            d.read_only = true;
            Some(d)
        })
        .collect()
}

/// Case-insensitive ordering without allocating lowercase copies.
fn cmp_ci(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        let (a, b) = (a.bytes(), b.bytes());
        return a
            .map(|c| c.to_ascii_lowercase())
            .cmp(b.map(|c| c.to_ascii_lowercase()));
    }
    let (a, b) = (a.chars(), b.chars());
    a.flat_map(char::to_lowercase)
        .cmp(b.flat_map(char::to_lowercase))
}

/// File extension for sorting; folders have none.
fn sort_ext(it: &DirItem) -> &str {
    if it.is_dir {
        return "";
    }
    std::path::Path::new(&it.name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

fn system_open(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(parent) = path.parent() {
            system_open(parent);
        }
    }
}

impl FolderView {
    pub fn new(
        roots: Vec<PathBuf>,
        display_roots: Vec<String>,
        temps: Vec<tempfile::TempDir>,
        filter_spec: &str,
        named: &[FltFilter],
        cfg: &Config,
    ) -> Self {
        let mut v = FolderView {
            roots,
            display_roots,
            _temps: temps,
            items: Vec::new(),
            filter_spec: filter_spec.to_string(),
            filter: FileFilter::resolve(filter_spec, named),
            fopts: cfg.folder.clone(),
            dopts: cfg.diff.clone(),
            running: None,
            elapsed: None,
            expanded: HashSet::new(),
            selected: BTreeSet::new(),
            anchor: None,
            cursor: None,
            sort: (Col::Name, true),
            visible: Vec::new(),
            view_key: None,
            view_cost: Duration::ZERO,
            status: String::new(),
            confirm: None,
            scroll_to: None,
            filter_edit: None,
            search: String::new(),
            focus_search: false,
            peek: false,
            peek_cache: None,
            finished: None,
            dock: None,
            select_pattern: None,
            merge_job: None,
            merge_plan: None,
            id: egui::Id::new(("folderview", std::time::Instant::now())),
        };
        v.start_compare();
        v
    }

    pub fn set_recursive(&mut self, recursive: bool) {
        self.fopts.recursive = recursive;
    }

    pub fn set_method(&mut self, method: folder::CompareMethod) {
        self.fopts.method = method;
    }

    /// Whether subfolders are compared, and how files are compared.
    pub fn scan_settings(&self) -> (bool, folder::CompareMethod) {
        (self.fopts.recursive, self.fopts.method)
    }

    pub fn n(&self) -> usize {
        self.roots.len()
    }

    pub fn title(&self) -> String {
        self.display_roots
            .iter()
            .map(|r| {
                std::path::Path::new(r)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.clone())
            })
            .collect::<Vec<_>>()
            .join(" ↔ ")
    }

    pub fn start_compare(&mut self) {
        if let Some(r) = &self.running {
            r.progress.abort.store(true, Ordering::Relaxed);
        }
        let progress = Arc::new(Progress::live());
        let (roots, filter, fopts, dopts, pr) = (
            self.roots.clone(),
            self.filter.clone(),
            self.fopts.clone(),
            self.dopts.clone(),
            progress.clone(),
        );
        let handle = std::thread::spawn(move || {
            Scanner {
                roots: &roots,
                filter: &filter,
                fopts: &fopts,
                dopts: &dopts,
                progress: &pr,
            }
            .run()
        });
        self.running = Some(Running {
            progress,
            handle,
            started: Instant::now(),
            updated: Instant::now(),
        });
        self.items.clear();
        self.visible.clear();
        self.dock = None;
        // They refer to items by index.
        self.confirm = None;
        self.merge_job = None;
        self.merge_plan = None;
        self.selected.clear();
        self.anchor = None;
        self.cursor = None;
        self.view_key = None;
        self.status.clear();
    }

    /// Re-runs the comparison with new options (F5 / options changed).
    pub fn refresh(&mut self, cfg: &Config, named: &[FltFilter]) {
        let recursive = self.fopts.recursive;
        let method = self.fopts.method;
        self.fopts = cfg.folder.clone();
        self.fopts.recursive = recursive;
        self.fopts.method = method;
        self.dopts = cfg.diff.clone();
        self.filter = FileFilter::resolve(&self.filter_spec, named);
        self.start_compare();
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// How long the comparison took, once, right after it finished without being stopped.
    pub fn take_finished(&mut self) -> Option<Duration> {
        self.finished.take()
    }

    /// Notices a comparison finishing while the tab is in the background.
    pub fn poll_background(&mut self) {
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.handle.is_finished())
        {
            self.poll();
        }
    }

    pub fn stop(&mut self) {
        if let Some(r) = &self.running {
            r.progress.abort.store(true, Ordering::Relaxed);
        }
    }

    fn poll(&mut self) {
        let Some(r) = &mut self.running else { return };
        if r.handle.is_finished() {
            let r = self.running.take().unwrap();
            let aborted = r.progress.abort.load(Ordering::Relaxed);
            // Same items in the same order as the streamed ones, so the selection stays valid.
            self.items = r.handle.join().unwrap_or_default();
            self.elapsed = Some(r.started.elapsed());
            self.selected.retain(|&i| i < self.items.len());
            self.view_key = None;
            self.peek_cache = None;
            if aborted {
                self.status = tr("msg.aborted").into();
            } else {
                self.finished = self.elapsed;
            }
        } else if r.updated.elapsed() >= UPDATE_INTERVAL.max(self.view_cost * 4) {
            r.updated = Instant::now();
            let updates = r.progress.take_updates();
            if !updates.is_empty() {
                let t = Instant::now();
                self.apply_updates(updates);
                self.view_cost = t.elapsed();
            }
        }
    }

    /// Shows what the running comparison has found so far: new items, and the results of
    /// the files compared since the last update.
    fn apply_updates(&mut self, updates: folder::Updates) {
        for it in updates.items {
            if let Some(p) = it.parent {
                let idx = self.items.len();
                self.items[p].children.push(idx);
            }
            self.items.push(it);
        }
        for o in updates.results {
            o.apply(&mut self.items);
        }
        let panes = self.n();
        folder::aggregate(&mut self.items, panes);
        self.view_key = None;
    }

    // ---------- visibility & sorting ----------

    fn shown(&self, it: &DirItem, cfg: &Config) -> bool {
        let n = self.n();
        if !it.exists[..n].iter().any(|&e| e) {
            return false;
        }
        match it.status {
            CmpStatus::Identical => cfg.show_identical,
            CmpStatus::Different | CmpStatus::Error | CmpStatus::NotCompared => cfg.show_different,
            CmpStatus::Pending => false,
            CmpStatus::Unique => {
                let unique_flag = |s: usize| {
                    if n == 2 && s == 1 {
                        cfg.show_unique[2]
                    } else {
                        cfg.show_unique[s]
                    }
                };
                (0..n).any(|s| it.exists[s] && unique_flag(s))
            }
        }
    }

    fn cmp_items(&self, a: usize, b: usize) -> std::cmp::Ordering {
        let (ia, ib) = (&self.items[a], &self.items[b]);
        let (col, asc) = self.sort;
        let name = || cmp_ci(&ia.name, &ib.name);
        let o = match col {
            Col::Name => ib.is_dir.cmp(&ia.is_dir).then_with(name),
            Col::Folder => cmp_ci(&ia.rel_dir, &ib.rel_dir).then_with(name),
            Col::Result => status_text(ia, self.n())
                .cmp(&status_text(ib, self.n()))
                .then_with(name),
            Col::Date(s) => ia.mtime[s].cmp(&ib.mtime[s]).then_with(name),
            Col::Size(s) => ia.size[s].cmp(&ib.size[s]).then_with(name),
            Col::Ext => cmp_ci(sort_ext(ia), sort_ext(ib)).then_with(name),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    }

    fn visibility(&self, cfg: &Config) -> Vec<bool> {
        let query = self.search.trim().to_lowercase();
        let matches = |it: &DirItem| it.name.to_lowercase().contains(&query);
        let mut vis = vec![false; self.items.len()];
        for i in (0..self.items.len()).rev() {
            let it = &self.items[i];
            vis[i] = if it.is_dir && !it.children.is_empty() {
                it.children.iter().any(|&c| vis[c])
                    || if query.is_empty() {
                        self.shown(it, cfg) && it.status != CmpStatus::Identical
                    } else {
                        self.shown(it, cfg) && matches(it)
                    }
            } else {
                self.shown(it, cfg) && (query.is_empty() || matches(it))
            };
        }
        vis
    }

    fn flat_list(&self, cfg: &Config) -> Vec<usize> {
        let vis = self.visibility(cfg);
        let mut v: Vec<usize> = (0..self.items.len())
            .filter(|&i| vis[i] && (!self.items[i].is_dir || self.items[i].children.is_empty()))
            .collect();
        v.sort_by(|&a, &b| {
            if self.sort.0 == Col::Name {
                cmp_ci(&self.items[a].rel_dir, &self.items[b].rel_dir)
                    .then_with(|| self.cmp_items(a, b))
            } else {
                self.cmp_items(a, b)
            }
        });
        v
    }

    fn rebuild_visible(&mut self, cfg: &Config) {
        self.visible = self.display_order(cfg, false);
    }

    /// Listed items in display order; with `expand_all`, as if every folder were expanded.
    fn display_order(&self, cfg: &Config, expand_all: bool) -> Vec<usize> {
        if !cfg.tree_mode {
            return self.flat_list(cfg);
        }
        let vis = self.visibility(cfg);
        let searching = expand_all || !self.search.trim().is_empty();
        let mut out = Vec::new();
        let mut roots: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.items[i].parent.is_none() && vis[i])
            .collect();
        roots.sort_by(|&a, &b| self.cmp_items(a, b));
        let mut stack: Vec<usize> = roots.into_iter().rev().collect();
        while let Some(i) = stack.pop() {
            out.push(i);
            let it = &self.items[i];
            if it.is_dir && (searching || self.expanded.contains(&it.rel_path())) {
                let mut ch: Vec<usize> = it.children.iter().copied().filter(|&c| vis[c]).collect();
                ch.sort_by(|&a, &b| self.cmp_items(a, b));
                stack.extend(ch.into_iter().rev());
            }
        }
        out
    }

    /// The file with differences listed after (or before) the item at `rel`, in display order.
    pub fn adjacent_different_file(&self, cfg: &Config, rel: &str, forward: bool) -> Option<usize> {
        let order = self.display_order(cfg, true);
        let pos = order
            .iter()
            .position(|&i| self.items[i].rel_path() == rel)?;
        let differs = |i: &&usize| {
            let it = &self.items[**i];
            !it.is_dir && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
        };
        if forward {
            order[pos + 1..].iter().find(differs).copied()
        } else {
            order[..pos].iter().rev().find(differs).copied()
        }
    }

    /// Selects item `i`, expanding its folders and scrolling to it, and returns the action that
    /// opens it.
    pub fn reveal_and_open(&mut self, i: usize, cfg: &Config) -> Option<FolderAction> {
        let mut p = self.items[i].parent;
        while let Some(pi) = p {
            self.expanded.insert(self.items[pi].rel_path());
            p = self.items[pi].parent;
        }
        self.rebuild_visible(cfg);
        if let Some(v) = self.visible.iter().position(|&x| x == i) {
            self.selected = BTreeSet::from([i]);
            self.anchor = Some(v);
            self.cursor = Some(v);
            self.scroll_to = Some(v);
        }
        self.open_action(i, None)
    }

    // ---------- operations ----------

    fn selected_targets(&self) -> Vec<usize> {
        // skip items whose ancestor is also selected
        self.selected
            .iter()
            .copied()
            .filter(|&i| {
                let mut p = self.items[i].parent;
                while let Some(pi) = p {
                    if self.selected.contains(&pi) {
                        return false;
                    }
                    p = self.items[pi].parent;
                }
                true
            })
            .collect()
    }

    pub fn request_op_copy(&mut self, from: usize, to: usize, cfg: &Config) {
        let targets: Vec<usize> = self
            .selected_targets()
            .into_iter()
            .filter(|&i| self.items[i].exists[from])
            .collect();
        if targets.is_empty() {
            return;
        }
        let op = PendingOp {
            kind: OpKind::Copy { from, to },
            targets,
        };
        if cfg.confirm_operations {
            self.confirm = Some(op);
        } else {
            self.execute(op, cfg);
        }
    }

    fn request_op(&mut self, kind: OpKind, cfg: &Config) {
        match kind {
            OpKind::Copy { from, to } => self.request_op_copy(from, to, cfg),
            OpKind::Delete { .. } => {
                let targets = self.selected_targets();
                if !targets.is_empty() {
                    // deletions are always confirmed
                    self.confirm = Some(PendingOp { kind, targets });
                }
            }
        }
    }

    fn execute(&mut self, op: PendingOp, cfg: &Config) {
        self.dock = None;
        self.merge_job = None;
        self.merge_plan = None;
        let mut errors = Vec::new();
        let mut any_dir = false;
        let mut done = 0;
        for &i in &op.targets {
            let it = &self.items[i];
            any_dir |= it.is_dir;
            let r = match op.kind {
                OpKind::Copy { from, to } => {
                    folder::copy_recursive(&it.path(&self.roots[from]), &it.path(&self.roots[to]))
                }
                OpKind::Delete { side } => {
                    let sides: Vec<usize> = match side {
                        Some(s) => vec![s],
                        None => (0..self.n()).filter(|&s| it.exists[s]).collect(),
                    };
                    sides
                        .into_iter()
                        .filter(|&s| it.exists[s])
                        .try_for_each(|s| {
                            folder::delete_path(&it.path(&self.roots[s]), cfg.use_recycle_bin)
                        })
                }
            };
            match r {
                Ok(()) => done += 1,
                Err(e) => errors.push(format!("{}: {e}", it.rel_path())),
            }
        }
        self.status = trf("msg.op_done", &[("n", done.to_string())]);
        if !errors.is_empty() {
            self.status
                .push_str(&format!(" — {}: {}", tr("status.error"), errors.join("; ")));
        }
        if any_dir {
            self.start_compare();
            return;
        }
        for &i in &op.targets {
            folder::refresh_file_item(&mut self.items, i, &self.roots, &self.fopts, &self.dopts);
            // ancestors may now exist on the target side
            let mut p = self.items[i].parent;
            while let Some(pi) = p {
                for s in 0..self.n() {
                    self.items[pi].exists[s] = self.items[pi].path(&self.roots[s]).is_dir();
                }
                p = self.items[pi].parent;
            }
        }
        let n = self.n();
        folder::aggregate(&mut self.items, n);
        self.view_key = None;
    }

    fn open_action(&self, i: usize, kind: Option<FileKind>) -> Option<FolderAction> {
        let it = &self.items[i];
        let n = self.n();
        let paths: Vec<PathBuf> = self.roots.iter().map(|r| it.path(r)).collect();
        if it.is_dir {
            if it.exists[..n].iter().all(|&e| e) {
                return Some(FolderAction::OpenFolders { paths });
            }
            return None;
        }
        let missing: Vec<bool> = (0..n).map(|s| !it.exists[s]).collect();
        Some(FolderAction::OpenFiles {
            paths,
            missing,
            kind,
            rel: it.rel_path(),
        })
    }

    /// Lines shown by the quick preview: a unified diff for text files on both sides,
    /// otherwise what is known about each side.
    fn preview(&self, i: usize) -> Vec<(PeekKind, String)> {
        let it = &self.items[i];
        let n = self.n();
        let mut out = Vec::new();
        let paths: Vec<PathBuf> = self.roots.iter().map(|r| it.path(r)).collect();
        let small = (0..n).all(|s| it.size[s].unwrap_or(0) <= PEEK_MAX_BYTES);
        let present: Vec<usize> = (0..n).filter(|&s| it.exists[s]).collect();
        let push_text = |out: &mut Vec<(PeekKind, String)>, text: &str| {
            let mut lines = text.lines();
            for line in lines.by_ref().take(PEEK_MAX_LINES) {
                let kind = if line.starts_with("+++") || line.starts_with("---") {
                    PeekKind::Meta
                } else if line.starts_with("@@") {
                    PeekKind::Hunk
                } else if line.starts_with('+') {
                    PeekKind::Add
                } else if line.starts_with('-') {
                    PeekKind::Del
                } else {
                    PeekKind::Plain
                };
                out.push((kind, line.to_string()));
            }
            if lines.next().is_some() {
                out.push((PeekKind::Meta, "…".into()));
            }
        };
        if !it.is_dir && !it.binary && small {
            if n == 2 && present.len() == 2 {
                if let (Ok(a), Ok(b)) = (
                    text::load_doc(&paths[0], None),
                    text::load_doc(&paths[1], None),
                ) {
                    let refs: Vec<&[String]> = vec![&a.lines, &b.lines];
                    let d = diff::compute(&refs, &self.dopts);
                    let rel = it.rel_path();
                    let input = patch::PatchInput {
                        name_a: &format!("a/{rel}"),
                        name_b: &format!("b/{rel}"),
                        date_a: "",
                        date_b: "",
                        a: &a.lines,
                        b: &b.lines,
                    };
                    let p = patch::make_patch(&input, &d, PatchStyle::Unified, 3);
                    if p.is_empty() {
                        out.push((PeekKind::Plain, tr("msg.identical").into()));
                    }
                    push_text(&mut out, &p);
                    return out;
                }
            } else if let [s] = present[..] {
                if let Ok(doc) = text::load_doc(&paths[s], None) {
                    out.push((
                        PeekKind::Meta,
                        trf("status.only", &[("side", side_name(s, n).into())]),
                    ));
                    push_text(&mut out, &doc.lines.join("\n"));
                    return out;
                }
            }
        }
        for s in 0..n {
            let what = if it.exists[s] {
                format!("{}  {}", fmt_size(it.size[s]), fmt_time(it.mtime[s]))
            } else {
                tr("peek.missing").to_string()
            };
            out.push((PeekKind::Meta, format!("{}: {what}", side_name(s, n))));
        }
        out.push((PeekKind::Plain, String::new()));
        out.push((PeekKind::Plain, status_text(it, n)));
        let note = if it.is_dir {
            "peek.folder"
        } else if it.binary {
            "peek.binary"
        } else if !small {
            "peek.too_large"
        } else {
            "peek.open_hint"
        };
        out.push((PeekKind::Plain, tr(note).into()));
        out
    }

    fn peek_window(&mut self, ctx: &egui::Context) {
        if !self.peek {
            return;
        }
        let Some(&i) = self
            .cursor
            .or(self.anchor)
            .and_then(|v| self.visible.get(v))
        else {
            self.peek = false;
            return;
        };
        if self.peek_cache.as_ref().map(|(k, _)| *k) != Some(i) {
            self.peek_cache = Some((i, self.preview(i)));
        }
        let Some((_, lines)) = &self.peek_cache else {
            return;
        };
        let mut open = true;
        let screen = ctx.screen_rect();
        egui::Window::new(self.items[i].rel_path())
            .id(self.id.with("peek"))
            .open(&mut open)
            .collapsible(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .default_size([720.0_f32.min(screen.width() - 80.0), 460.0])
            .show(ctx, |ui| {
                let pal = widgets::pal(ui);
                ui.label(
                    egui::RichText::new(tr("peek.hint"))
                        .size(12.0)
                        .color(pal.text_dim),
                );
                ui.separator();
                let font = FontId::monospace(12.5);
                let row_h = ui.fonts(|f| f.row_height(&font));
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, lines.len(), |ui, range| {
                        for (kind, text) in &lines[range] {
                            let color = match kind {
                                PeekKind::Plain => pal.text,
                                PeekKind::Meta => pal.text_dim,
                                PeekKind::Hunk => pal.accent,
                                PeekKind::Add => pal.success,
                                PeekKind::Del => pal.danger,
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(text).font(font.clone()).color(color),
                                )
                                .extend(),
                            );
                        }
                    });
            });
        if !open {
            self.peek = false;
        }
    }

    /// Copies the selected items of side `side` into `dest`, keeping their relative paths.
    pub fn copy_to_folder(&mut self, side: usize, dest: &Path) {
        let mut done = 0;
        let mut errors = Vec::new();
        for i in self.selected_targets() {
            let it = &self.items[i];
            if !it.exists[side] {
                continue;
            }
            let rel = it.rel_path();
            match folder::copy_recursive(&it.path(&self.roots[side]), &dest.join(&rel)) {
                Ok(()) => done += 1,
                Err(e) => errors.push(format!("{rel}: {e}")),
            }
        }
        self.status = trf("msg.op_done", &[("n", done.to_string())]);
        if !errors.is_empty() {
            self.status
                .push_str(&format!(" — {}: {}", tr("status.error"), errors.join("; ")));
        }
    }

    /// Asks where to copy the selected items of side `side`.
    fn pick_copy_destination(&mut self, side: usize) {
        if self.selected.is_empty() {
            return;
        }
        if let Some(dest) = rfd::FileDialog::new()
            .set_title(tr("cmd.copy_to_folder"))
            .pick_folder()
        {
            self.copy_to_folder(side, &dest);
        }
    }

    /// Selects the items whose name matches one of the `;`-separated wildcards, among those
    /// the view options show, opening the folders that hold them.
    pub fn select_matching(&mut self, patterns: &str, cfg: &Config) {
        let res: Vec<regex::Regex> = patterns
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .filter_map(crate::core::filter::wildcard_to_regex)
            .collect();
        let vis = self.visibility(cfg);
        self.selected = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                vis[i] && self.shown(it, cfg) && res.iter().any(|re| re.is_match(&it.name))
            })
            .collect();
        for &i in &self.selected {
            let mut p = self.items[i].parent;
            while let Some(pi) = p {
                self.expanded.insert(self.items[pi].rel_path());
                p = self.items[pi].parent;
            }
        }
        self.rebuild_visible(cfg);
        self.view_key = None;
        let first = self.visible.iter().position(|i| self.selected.contains(i));
        self.anchor = first;
        self.cursor = first;
        self.scroll_to = first;
        self.status = format!("{} {}", self.selected.len(), tr("label.selected"));
    }

    pub fn open_select_pattern(&mut self) {
        self.select_pattern = Some(String::new());
    }

    /// Counts of the compared items: a label and a value per row.
    pub fn statistics(&self) -> Vec<(String, String)> {
        let n = self.n();
        let (same, diff, uniq, err) = folder::status_counts(&self.items);
        let files = self.items.iter().filter(|i| !i.is_dir);
        let mut rows = vec![
            (tr("stats.files").to_string(), files.clone().count()),
            (
                tr("stats.folders").to_string(),
                self.items.iter().filter(|i| i.is_dir).count(),
            ),
            (tr("stats.identical").to_string(), same),
            (tr("stats.different").to_string(), diff),
            (tr("stats.unique").to_string(), uniq),
            (tr("stats.errors").to_string(), err),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect::<Vec<_>>();
        for s in 0..n {
            let only = files
                .clone()
                .filter(|it| (0..n).all(|o| it.exists[o] == (o == s)))
                .count();
            rows.push((
                trf("stats.only_in", &[("side", side_name(s, n).into())]),
                only.to_string(),
            ));
        }
        for s in 0..n {
            let size: u64 = files.clone().filter_map(|it| it.size[s]).sum();
            rows.push((
                trf("stats.total_size", &[("side", side_name(s, n).into())]),
                fmt_size(Some(size)),
            ));
        }
        rows
    }

    /// Plans a 3-way merge into the middle folder, to be confirmed.
    pub fn request_merge(&mut self) {
        if self.n() != 3 || self.running.is_some() {
            return;
        }
        if self.merge_job.is_some() {
            return;
        }
        let (items, roots) = (self.items.clone(), self.roots.clone());
        self.merge_job = Some(std::thread::spawn(move || {
            folder_merge::plan(&items, &roots)
        }));
        self.status = tr("msg.folder_merge_planning").to_string();
    }

    /// Takes the merge plan once it is ready.
    fn poll_merge(&mut self, ctx: &egui::Context) {
        if !self.merge_job.as_ref().is_some_and(|j| j.is_finished()) {
            if self.merge_job.is_some() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        match self.merge_job.take().map(JoinHandle::join) {
            Some(Ok(plan)) if plan.steps.is_empty() => {
                self.status = tr("msg.folder_merge_nothing").to_string();
            }
            Some(Ok(plan)) => {
                self.status.clear();
                self.merge_plan = Some(plan);
            }
            _ => self.status = tr("status.error").to_string(),
        }
    }

    /// Whether one of the view's dialogs is open, so the app leaves the keyboard to it.
    pub fn has_dialog(&self) -> bool {
        self.confirm.is_some() || self.merge_plan.is_some() || self.select_pattern.is_some()
    }

    fn merge_dialog(&mut self, ui: &Ui, cfg: &Config) {
        let Some(plan) = self.merge_plan.take() else {
            return;
        };
        let (mut keep, mut run) = (true, false);
        let c = plan.counts();
        egui::Modal::new(self.id.with("merge")).show(ui.ctx(), |ui| {
            ui.set_min_width(400.0);
            ui.set_max_width(520.0);
            let pal = widgets::pal(ui);
            ui.heading(tr("cmd.folder_merge"));
            ui.label(trf(
                "msg.folder_merge_plan",
                &[
                    ("copies", c.copies.to_string()),
                    ("deletes", c.deletes.to_string()),
                    ("merges", c.merges.to_string()),
                ],
            ));
            if c.conflicts > 0 {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(trf(
                        "msg.folder_merge_conflicts",
                        &[("n", c.conflicts.to_string())],
                    ))
                    .color(pal.warning),
                );
                egui::Frame::new()
                    .fill(pal.surface_alt)
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let conflicts = plan
                            .steps
                            .iter()
                            .filter(|(_, s)| matches!(s, Step::Conflict))
                            .map(|(i, _)| *i);
                        for i in conflicts.clone().take(10) {
                            ui.label(
                                egui::RichText::new(self.items[i].rel_path())
                                    .monospace()
                                    .color(pal.text_dim),
                            );
                        }
                        if conflicts.count() > 10 {
                            ui.label("…");
                        }
                    });
            }
            widgets::dialog_footer(ui, |ui| {
                let ok = ui.add_enabled_ui(plan.changes() > 0, |ui| {
                    widgets::primary_button(ui, tr("btn.ok"))
                });
                if ok.inner.clicked() {
                    run = true;
                    keep = false;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    keep = false;
                }
            });
        });
        if run {
            let (done, errors) =
                folder_merge::apply(plan, &self.items, &self.roots, cfg.use_recycle_bin);
            self.start_compare();
            self.status = trf("msg.op_done", &[("n", done.to_string())]);
            if !errors.is_empty() {
                self.status
                    .push_str(&format!(" — {}: {}", tr("status.error"), errors.join("; ")));
            }
        } else if keep {
            self.merge_plan = Some(plan);
        }
    }

    fn select_pattern_dialog(&mut self, ui: &Ui, cfg: &Config) {
        let Some(mut text) = self.select_pattern.take() else {
            return;
        };
        let (mut keep, mut run) = (true, false);
        egui::Modal::new(self.id.with("select_pattern")).show(ui.ctx(), |ui| {
            ui.set_min_width(380.0);
            ui.heading(tr("cmd.select_pattern"));
            ui.label(tr("hint.select_pattern"));
            let r = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("*.c; *.h")
                    .desired_width(f32::INFINITY),
            );
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                run = true;
            }
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.ok")).clicked() {
                    run = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    keep = false;
                }
            });
        });
        if run {
            self.select_matching(&text, cfg);
        } else if keep {
            self.select_pattern = Some(text);
        }
    }

    fn dock_view_of(&self, docs: Vec<TextDoc>, cfg: &Config) -> Box<TextView> {
        let mut v = TextView::new(docs, cfg);
        v.keys_enabled = false;
        v.set_options(&self.dopts);
        Box::new(v)
    }

    /// A read-only compare of item `i` for the docked file compare, when it is a text file.
    #[cfg(test)]
    fn dock_view(&self, i: usize, cfg: &Config) -> Option<Box<TextView>> {
        load_dock_docs(&self.items[i], &self.roots).map(|d| self.dock_view_of(d, cfg))
    }

    /// Moves the docked file compare along: it waits for the selection to settle, loads the
    /// files in the background, then shows them.
    fn update_dock(&mut self, ctx: &egui::Context, cfg: &Config) {
        let cur = self
            .cursor
            .or(self.anchor)
            .and_then(|v| self.visible.get(v))
            .copied();
        if cur != self.dock.as_ref().map(|d| d.0) {
            self.dock = cur.map(|i| (i, Dock::Waiting(Instant::now())));
        }
        let Some((i, state)) = &self.dock else {
            return;
        };
        let i = *i;
        let next = match state {
            Dock::Waiting(since) if since.elapsed() >= DOCK_DELAY => {
                let (it, roots) = (self.items[i].clone(), self.roots.clone());
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(load_dock_docs(&it, &roots));
                });
                Some(Dock::Loading(rx))
            }
            Dock::Loading(rx) => match rx.try_recv() {
                Ok(docs) => Some(Dock::Ready(docs.map(|d| self.dock_view_of(d, cfg)))),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Dock::Ready(None)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            },
            _ => None,
        };
        if let Some(state) = next {
            self.dock = Some((i, state));
        }
        if !matches!(self.dock, Some((_, Dock::Ready(_)))) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn dock_ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.update_dock(ui.ctx(), cfg);
        let avail = ui.available_height();
        egui::TopBottomPanel::bottom(self.id.with("dock"))
            .resizable(true)
            .default_height(avail * 0.45)
            .height_range(100.0..=(avail - 120.0).max(100.0))
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| match &mut self.dock {
                Some((_, Dock::Ready(Some(v)))) => {
                    v.set_options(&self.dopts);
                    v.ui(ui, cfg, c);
                }
                other => {
                    let key = match other {
                        Some((i, Dock::Ready(None))) if self.items[*i].is_dir => "peek.folder",
                        Some((_, Dock::Ready(None))) => "dock.not_text",
                        Some(_) => "msg.comparing",
                        None => "dock.hint",
                    };
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new(tr(key)).color(widgets::pal(ui).text_dim));
                    });
                }
            });
    }

    pub fn copy_selected(&mut self, to_right: bool, cfg: &Config) {
        let n = self.n();
        let (from, to) = if to_right { (0, 1) } else { (n - 1, n - 2) };
        self.request_op_copy(from, to, cfg);
    }

    /// Patch of all differing/unique text files.
    pub fn make_patch(&self, style: PatchStyle, context: usize) -> String {
        if self.n() != 2 {
            return String::new();
        }
        patch::folder_patch(
            &self.roots,
            &self.display_roots,
            &self.items,
            &self.dopts,
            style,
            context,
        )
    }

    pub fn report(&self, format: ReportFormat, cfg: &Config) -> String {
        let list = self.flat_list(cfg);
        let items: Vec<&DirItem> = list.iter().map(|&i| &self.items[i]).collect();
        let roots: Vec<PathBuf> = self.display_roots.iter().map(PathBuf::from).collect();
        report::generate(&items, &roots, format)
    }

    /// Writes the HTML report of the listed items followed by the differences of every changed
    /// text file; it reads every changed file, so it can run on another thread.
    pub fn report_with_diffs_job(&self, cfg: &Config) -> impl FnOnce() -> String + Send + 'static {
        let items: Vec<DirItem> = self
            .flat_list(cfg)
            .into_iter()
            .map(|i| self.items[i].clone())
            .collect();
        let display: Vec<PathBuf> = self.display_roots.iter().map(PathBuf::from).collect();
        let (roots, dopts) = (self.roots.clone(), self.dopts.clone());
        move || {
            let refs: Vec<&DirItem> = items.iter().collect();
            report::folder_html_with_diffs(&refs, &roots, &display, &dopts)
        }
    }

    pub fn outcome(&self) -> Outcome {
        if self.running.is_some() {
            return Outcome::Busy;
        }
        let (_, diff, uniq, err) = folder::status_counts(&self.items);
        if err > 0 {
            Outcome::Error
        } else if diff + uniq > 0 {
            Outcome::Different
        } else {
            Outcome::Identical
        }
    }

    pub fn status_line(&self) -> String {
        if let Some(r) = &self.running {
            return format!(
                "{} {}/{}",
                tr("msg.comparing"),
                r.progress.done.load(Ordering::Relaxed),
                r.progress.total.load(Ordering::Relaxed)
            );
        }
        let (same, diff, uniq, err) = folder::status_counts(&self.items);
        let mut s = trf(
            "msg.folder_summary",
            &[
                ("same", same.to_string()),
                ("diff", diff.to_string()),
                ("unique", uniq.to_string()),
                ("err", err.to_string()),
            ],
        );
        if let Some(e) = self.elapsed {
            s.push_str(&format!("  |  {:.2}s", e.as_secs_f32()));
        }
        if self.selected.len() > 1 {
            s.push_str(&format!(
                "  |  {} {}",
                self.selected.len(),
                tr("label.selected")
            ));
        }
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    pub fn expand_all(&mut self, expand: bool) {
        if expand {
            self.expanded = self
                .items
                .iter()
                .filter(|i| i.is_dir)
                .map(|i| i.rel_path())
                .collect();
        } else {
            self.expanded.clear();
        }
        self.view_key = None;
    }

    // ---------- UI ----------

    pub fn focus_search(&mut self) {
        self.focus_search = true;
    }

    fn header_bar(
        &mut self,
        ui: &mut Ui,
        cfg: &Config,
        named: &[FltFilter],
    ) -> (bool, Option<FolderAction>) {
        let mut refresh = false;
        let mut action = None;
        let mut new_filter: Option<String> = None;
        let mut new_options: Option<(folder::CompareMethod, bool)> = None;
        let n = self.n();
        let sel = !self.selected.is_empty();
        widgets::toolbar(ui, |ui| {
            let key = widgets::with_key;
            if widgets::ghost_icon_button(ui, Icon::Refresh, tr("cmd.refresh"))
                .on_hover_text("F5")
                .clicked()
            {
                refresh = true;
            }
            if cfg.tree_mode {
                if widgets::icon_button(ui, Icon::ExpandAll, tr("cmd.expand_all")).clicked() {
                    self.expand_all(true);
                }
                if widgets::icon_button(ui, Icon::CollapseAll, tr("cmd.collapse_all")).clicked() {
                    self.expand_all(false);
                }
            }
            widgets::toolbar_sep(ui);
            if n == 2 {
                if widgets::icon_button_enabled(
                    ui,
                    sel,
                    Icon::CopyRight,
                    &key(tr("cmd.copy_right"), "Alt+→"),
                )
                .clicked()
                {
                    self.request_op_copy(0, 1, cfg);
                }
                if widgets::icon_button_enabled(
                    ui,
                    sel,
                    Icon::CopyLeft,
                    &key(tr("cmd.copy_left"), "Alt+←"),
                )
                .clicked()
                {
                    self.request_op_copy(1, 0, cfg);
                }
            } else {
                if widgets::icon_button_enabled(ui, sel, Icon::CopyRight, tr("cmd.copy_from_left"))
                    .clicked()
                {
                    self.request_op_copy(0, 1, cfg);
                }
                if widgets::icon_button_enabled(ui, sel, Icon::CopyLeft, tr("cmd.copy_from_right"))
                    .clicked()
                {
                    self.request_op_copy(2, 1, cfg);
                }
                if widgets::icon_button_enabled(
                    ui,
                    self.running.is_none() && self.merge_job.is_none(),
                    Icon::Bolt,
                    tr("cmd.folder_merge"),
                )
                .clicked()
                {
                    self.request_merge();
                }
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, Icon::Check, tr("cmd.select_pattern")).clicked() {
                self.open_select_pattern();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let close = egui::PopupCloseBehavior::CloseOnClickOutside;

                let search = widgets::search_edit(
                    ui,
                    &mut self.search,
                    &key(tr("folder.search_hint"), "Ctrl+F"),
                    170.0,
                );
                if std::mem::take(&mut self.focus_search) {
                    search.request_focus();
                }
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.search.clear();
                    self.view_key = None;
                }
                if search.changed() {
                    self.view_key = None;
                }

                let method_id = self.id.with("method_popup");
                let r = widgets::chip_button(
                    ui,
                    Icon::Method,
                    tr("label.method"),
                    tr(self.fopts.method.key()),
                )
                .on_hover_text(tr("tip.click_change"));
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(method_id));
                }
                egui::popup::popup_below_widget(ui, method_id, &r, close, |ui| {
                    ui.set_min_width(220.0);
                    for m in folder::CompareMethod::ALL {
                        if ui
                            .selectable_label(self.fopts.method == m, tr(m.key()))
                            .clicked()
                            && self.fopts.method != m
                        {
                            new_options = Some((m, self.fopts.recursive));
                        }
                    }
                    ui.separator();
                    let mut recursive = self.fopts.recursive;
                    if ui.checkbox(&mut recursive, tr("label.recursive")).changed() {
                        new_options = Some((self.fopts.method, recursive));
                    }
                });

                let filter_id = self.id.with("filter_popup");
                let r =
                    widgets::chip_button(ui, Icon::Filter, tr("label.filter"), &self.filter_spec)
                        .on_hover_text(tr("tip.click_change"));
                let opened = r.clicked();
                if opened {
                    self.filter_edit = Some(self.filter_spec.clone());
                    ui.memory_mut(|m| m.toggle_popup(filter_id));
                }
                egui::popup::popup_below_widget(ui, filter_id, &r, close, |ui| {
                    ui.set_min_width(300.0);
                    let edit = self
                        .filter_edit
                        .get_or_insert_with(|| self.filter_spec.clone());
                    let field = ui.add(
                        egui::TextEdit::singleline(edit)
                            .margin(vec2(8.0, 4.0))
                            .desired_width(f32::INFINITY),
                    );
                    if opened {
                        field.request_focus();
                    }
                    if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        new_filter = Some(edit.clone());
                    }
                    ui.separator();
                    let listed = cfg.recent_filters.iter().cloned().chain(
                        named
                            .iter()
                            .map(|f| format!("{}{}", crate::core::filter::FLT_PREFIX, f.name)),
                    );
                    for f in listed {
                        if ui.selectable_label(f == self.filter_spec, &f).clicked() {
                            new_filter = Some(f);
                        }
                    }
                });
            });
        });
        if let Some((method, recursive)) = new_options {
            if method != self.fopts.method {
                ui.memory_mut(|m| m.close_popup());
            }
            self.set_method(method);
            self.set_recursive(recursive);
            self.start_compare();
            action = Some(FolderAction::OptionsChanged { method, recursive });
        }
        if let Some(spec) = new_filter.filter(|s| !s.trim().is_empty()) {
            ui.memory_mut(|m| m.close_popup());
            self.filter_edit = None;
            self.filter_spec = spec.clone();
            refresh = true;
            action = Some(FolderAction::FilterChanged(spec));
        }
        let pal = widgets::pal(ui);
        let paths = egui::Frame::new()
            .fill(pal.surface)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.columns(n, |cols| {
                    for (s, col) in cols.iter_mut().enumerate() {
                        col.horizontal(|ui| {
                            widgets::side_chip(ui, s, n);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.display_roots[s]).size(13.0),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.display_roots[s]);
                        });
                    }
                });
            });
        let rect = paths.response.rect;
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.border),
        );
        if let Some(r) = &self.running {
            let done = r.progress.done.load(Ordering::Relaxed);
            let total = r.progress.total.load(Ordering::Relaxed);
            let mut stop = false;
            egui::Frame::new()
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let frac = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        };
                        let text = if total > 0 {
                            format!("{done} / {total}")
                        } else {
                            format!(
                                "{} {}",
                                tr("msg.scanning"),
                                r.progress
                                    .current
                                    .lock()
                                    .map(|c| c.clone())
                                    .unwrap_or_default()
                            )
                        };
                        ui.label(egui::RichText::new(text).size(12.5).color(pal.text_dim));
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .fill(pal.accent)
                                .desired_height(6.0)
                                .corner_radius(3)
                                .desired_width(ui.available_width() - 90.0)
                                .animate(total == 0),
                        );
                        if widgets::secondary_button(ui, tr("btn.stop")).clicked() {
                            stop = true;
                        }
                    });
                });
            if stop {
                self.stop();
            }
        }
        (refresh, action)
    }

    /// Shown instead of the list when a finished comparison has nothing to list.
    fn empty_state(
        &mut self,
        ui: &mut Ui,
        cfg: &Config,
        named: &[FltFilter],
    ) -> Option<FolderAction> {
        let pal = widgets::pal(ui);
        let hidden = !self.items.is_empty();
        let default_filter = matches!(self.filter_spec.trim(), "" | "*.*" | "*");
        let mut action = None;
        if !self.search.trim().is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.3).max(24.0));
                let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter().rect_filled(rect, 12.0, pal.control);
                crate::ui::icons::paint(
                    ui.painter(),
                    rect.shrink(10.0),
                    Icon::Search,
                    pal.text_dim,
                );
                ui.add_space(10.0);
                let query = self.search.trim().to_string();
                ui.label(
                    egui::RichText::new(trf("folder.no_match", &[("text", query)]))
                        .font(theme::semibold(15.0)),
                );
                ui.add_space(12.0);
                if widgets::secondary_button(ui, tr("btn.clear_search")).clicked() {
                    self.search.clear();
                    self.view_key = None;
                }
            });
            return None;
        }
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.3).max(24.0));
            let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            ui.painter().rect_filled(rect, 12.0, pal.control);
            crate::ui::icons::paint(
                ui.painter(),
                rect.shrink(10.0),
                if hidden { Icon::Filter } else { Icon::Folder },
                pal.text_dim,
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(if hidden {
                    tr("folder.empty_hidden")
                } else {
                    tr("folder.empty_none")
                })
                .font(theme::semibold(15.0)),
            );
            if !hidden && !default_filter {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("{}  {}", tr("label.filter"), self.filter_spec))
                        .color(pal.text_dim),
                );
            }
            ui.add_space(12.0);
            if hidden {
                if widgets::secondary_button(ui, tr("btn.show_all")).clicked() {
                    action = Some(FolderAction::ShowAll);
                }
            } else if !default_filter
                && widgets::secondary_button(ui, tr("filters.clear")).clicked()
            {
                self.filter_spec = "*.*".into();
                self.refresh(cfg, named);
                action = Some(FolderAction::FilterChanged(self.filter_spec.clone()));
            }
        });
        action
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        cfg: &Config,
        c: &Colors,
        named: &[FltFilter],
    ) -> Option<FolderAction> {
        let was_running = self.running.is_some();
        self.poll();
        if was_running {
            // Also repaints once after finishing, as the status bar was drawn before this poll.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let (refresh, header_action) = self.header_bar(ui, cfg, named);
        if refresh {
            self.refresh(cfg, named);
        }
        let key = (
            cfg.tree_mode,
            cfg.show_identical,
            cfg.show_different,
            cfg.show_unique,
        );
        if self.view_key != Some(key) {
            self.view_key = Some(key);
            let t = Instant::now();
            self.rebuild_visible(cfg);
            self.view_cost += t.elapsed();
        }
        if self.running.is_none() && self.visible.is_empty() {
            return header_action.or_else(|| self.empty_state(ui, cfg, named));
        }
        if cfg.folder_file_pane {
            self.dock_ui(ui, cfg, c);
        } else {
            self.dock = None;
        }

        let mut result: Option<FolderAction> = header_action;
        let mut toggle: Option<usize> = None;
        let mut click: Option<(usize, Modifiers)> = None;
        let mut double: Option<usize> = None;
        let mut menu: Option<MenuCmd> = None;
        let mut new_sort: Option<Col> = None;

        // keyboard navigation
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 12.0;
        if !ui.ctx().wants_keyboard_input()
            && !widgets::modal_open(ui.ctx())
            && self.confirm.is_none()
            && self.merge_plan.is_none()
            && self.select_pattern.is_none()
            && !self.visible.is_empty()
        {
            let last = self.visible.len() - 1;
            let cur = self.cursor.or(self.anchor).unwrap_or(0).min(last);
            let page = ((ui.available_height() / row_h) as usize)
                .saturating_sub(2)
                .max(1);
            let mut target = None;
            let mut extend = false;
            let mut select_all = false;
            let mut peek = false;
            ui.input_mut(|i| {
                extend = i.modifiers.shift;
                peek = i.consume_key(Modifiers::NONE, Key::Space);
                if self.peek && i.consume_key(Modifiers::NONE, Key::Escape) {
                    self.peek = false;
                }
                for (key, to) in [
                    (Key::ArrowDown, (cur + 1).min(last)),
                    (Key::ArrowUp, cur.saturating_sub(1)),
                    (Key::PageDown, (cur + page).min(last)),
                    (Key::PageUp, cur.saturating_sub(page)),
                    (Key::Home, 0),
                    (Key::End, last),
                ] {
                    // Matches with or without Shift; Shift extends the selection.
                    if i.consume_key(Modifiers::NONE, key) {
                        target = Some(to);
                    }
                }
                if i.consume_key(Modifiers::COMMAND, Key::A) {
                    select_all = true;
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) && self.anchor.is_some() {
                    double = Some(self.visible[cur]);
                }
                if cfg.tree_mode && i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    let it = &self.items[self.visible[cur]];
                    if it.is_dir && !self.expanded.contains(&it.rel_path()) {
                        toggle = Some(self.visible[cur]);
                    }
                }
                if cfg.tree_mode && i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    let it = &self.items[self.visible[cur]];
                    if it.is_dir && self.expanded.contains(&it.rel_path()) {
                        toggle = Some(self.visible[cur]);
                    }
                }
            });
            if let Some(t) = target {
                match self.anchor.filter(|_| extend) {
                    Some(a) => {
                        let a = a.min(last);
                        let (lo, hi) = if a < t { (a, t) } else { (t, a) };
                        self.selected = self.visible[lo..=hi].iter().copied().collect();
                    }
                    None => {
                        self.selected.clear();
                        self.selected.insert(self.visible[t]);
                        self.anchor = Some(t);
                    }
                }
                self.cursor = Some(t);
                self.scroll_to = Some(t);
            }
            if select_all {
                self.selected = self.visible.iter().copied().collect();
            }
            if peek {
                self.peek = !self.peek;
                if self.peek && self.cursor.or(self.anchor).is_none() {
                    self.selected = BTreeSet::from([self.visible[cur]]);
                    self.anchor = Some(cur);
                    self.cursor = Some(cur);
                }
            }
        }

        let n = self.n();
        let pal = widgets::pal(ui);
        let side_cols: Vec<Color32> = (0..n).map(|s| widgets::side_color(ui, s, n)).collect();
        let status_color = |it: &DirItem| -> Color32 {
            match it.status {
                CmpStatus::Identical => pal.success,
                CmpStatus::Different => pal.warning,
                CmpStatus::Error => pal.danger,
                CmpStatus::NotCompared | CmpStatus::Pending => pal.text_dim,
                CmpStatus::Unique => {
                    let present: Vec<usize> = (0..n).filter(|&s| it.exists[s]).collect();
                    if present.len() == 1 {
                        side_cols[present[0]]
                    } else {
                        pal.warning
                    }
                }
            }
        };
        let list_rect = ui.available_rect_before_wrap();
        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list_rect));
        list_ui.set_clip_rect(list_rect.intersect(ui.clip_rect()));
        let ui = &mut list_ui;
        let mut table = TableBuilder::new(ui)
            .id_salt(self.id)
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(280.0).at_least(80.0).clip(true))
            .column(Column::initial(160.0).at_least(40.0).clip(true))
            .column(Column::initial(190.0).at_least(60.0).clip(true));
        for _ in 0..n {
            table = table.column(Column::initial(140.0).at_least(40.0).clip(true));
        }
        for _ in 0..n {
            table = table.column(Column::initial(90.0).at_least(30.0).clip(true));
        }
        table = table.column(Column::remainder().at_least(40.0));
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, None);
        }
        let mut cols = vec![
            (Col::Name, tr("col.name").to_string()),
            (Col::Folder, tr("col.folder").into()),
            (Col::Result, tr("col.result").into()),
        ];
        for s in 0..n {
            cols.push((
                Col::Date(s),
                format!("{} {}", side_name(s, n), tr("col.date")),
            ));
        }
        for s in 0..n {
            cols.push((
                Col::Size(s),
                format!("{} {}", side_name(s, n), tr("col.size")),
            ));
        }
        cols.push((Col::Ext, tr("col.ext").into()));

        let sort = self.sort;
        let items = &self.items;
        let visible = &self.visible;
        let selected = &self.selected;
        let expanded = &self.expanded;
        let searching = !self.search.trim().is_empty();
        let tree = cfg.tree_mode;
        table
            .header(row_h, |mut header| {
                for (col, label) in &cols {
                    header.col(|ui| {
                        ui.painter().rect_filled(
                            ui.max_rect().expand2(vec2(4.0, 3.0)),
                            0.0,
                            pal.surface_alt,
                        );
                        let r = ui.max_rect().expand2(vec2(4.0, 3.0));
                        ui.painter().hline(
                            r.x_range(),
                            r.bottom() - 0.5,
                            Stroke::new(1.0_f32, pal.border),
                        );
                        let sorted = sort.0 == *col;
                        let arrow = if sorted {
                            if sort.1 {
                                "  ▲"
                            } else {
                                "  ▼"
                            }
                        } else {
                            ""
                        };
                        let color = if sorted { pal.text } else { pal.text_dim };
                        let text = egui::RichText::new(format!("{label}{arrow}"))
                            .font(theme::semibold(12.5))
                            .color(color);
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            new_sort = Some(*col);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, visible.len(), |mut row| {
                    let vi = row.index();
                    let idx = visible[vi];
                    let it = &items[idx];
                    row.set_selected(selected.contains(&idx));
                    let bg = match it.status {
                        CmpStatus::Different => Some(c.folder_diff),
                        CmpStatus::Unique => Some(c.folder_unique),
                        CmpStatus::Error => Some(c.folder_error),
                        _ => None,
                    };
                    let paint_bg = |ui: &mut Ui| {
                        if let Some(bg) = bg {
                            if !selected.contains(&idx) {
                                let r = ui.max_rect().expand2(vec2(4.0, 2.0));
                                ui.painter().rect_filled(r, 0.0, bg);
                            }
                        }
                    };
                    let txt = egui::RichText::new;
                    row.col(|ui| {
                        paint_bg(ui);
                        if tree {
                            ui.add_space(it.depth as f32 * 16.0);
                            if it.is_dir && !it.children.is_empty() {
                                let open = searching || expanded.contains(&it.rel_path());
                                let arrow = egui::RichText::new(if open { "⏷" } else { "⏵" })
                                    .color(pal.text_dim);
                                if ui
                                    .add(egui::Button::new(arrow).frame(false).small())
                                    .clicked()
                                {
                                    toggle = Some(idx);
                                }
                            } else {
                                ui.add_space(16.0);
                            }
                        }
                        let (icon, icon_color) = if it.is_dir {
                            (Icon::Folder, Color32::from_rgb(0xE0, 0xA4, 0x2E))
                        } else {
                            (Icon::File, pal.text_dim)
                        };
                        widgets::inline_icon(ui, icon, icon_color);
                        ui.add(
                            egui::Label::new(txt(it.name.clone()))
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        paint_bg(ui);
                        ui.add(
                            egui::Label::new(txt(it.rel_dir.clone()).color(pal.text_dim))
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        paint_bg(ui);
                        let sc = status_color(it);
                        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, sc);
                        ui.add(
                            egui::Label::new(
                                txt(status_text(it, n)).font(FontId::proportional(13.5)),
                            )
                            .selectable(false)
                            .truncate(),
                        );
                    });
                    for s in 0..n {
                        row.col(|ui| {
                            paint_bg(ui);
                            ui.add(egui::Label::new(txt(fmt_time(it.mtime[s]))).selectable(false));
                        });
                    }
                    for s in 0..n {
                        row.col(|ui| {
                            paint_bg(ui);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(txt(fmt_size(it.size[s]))).selectable(false),
                                );
                            });
                        });
                    }
                    row.col(|ui| {
                        paint_bg(ui);
                        ui.add(egui::Label::new(txt(it.ext())).selectable(false));
                    });
                    let resp = row.response();
                    if resp.double_clicked() {
                        double = Some(idx);
                    } else if resp.clicked() {
                        click = Some((vi, resp.ctx.input(|i| i.modifiers)));
                    }
                    if resp.secondary_clicked() && !selected.contains(&idx) {
                        click = Some((vi, Modifiers::NONE));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(tr("cmd.compare")).clicked() {
                            menu = Some(MenuCmd::Open(idx, None));
                        }
                        if !it.is_dir {
                            ui.menu_button(tr("cmd.compare_as"), |ui| {
                                if ui.button(tr("kind.text")).clicked() {
                                    menu = Some(MenuCmd::Open(idx, Some(FileKind::Text)));
                                }
                                if ui.button(tr("kind.binary")).clicked() {
                                    menu = Some(MenuCmd::Open(idx, Some(FileKind::Binary)));
                                }
                                if ui.button(tr("kind.image")).clicked() {
                                    menu = Some(MenuCmd::Open(idx, Some(FileKind::Image)));
                                }
                            });
                        }
                        ui.separator();
                        for from in 0..n {
                            for to in 0..n {
                                if from != to && (n == 2 || from == 1 || to == 1) {
                                    let label = trf(
                                        "cmd.copy_from_to",
                                        &[
                                            ("from", side_name(from, n).into()),
                                            ("to", side_name(to, n).into()),
                                        ],
                                    );
                                    if ui
                                        .add_enabled(it.exists[from], egui::Button::new(label))
                                        .clicked()
                                    {
                                        menu = Some(MenuCmd::Op(OpKind::Copy { from, to }));
                                    }
                                }
                            }
                        }
                        ui.menu_button(tr("cmd.copy_to_folder"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::CopyTo(s));
                                }
                            }
                        });
                        ui.menu_button(tr("cmd.delete"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::Op(OpKind::Delete { side: Some(s) }));
                                }
                            }
                            if ui.button(tr("side.all")).clicked() {
                                menu = Some(MenuCmd::Op(OpKind::Delete { side: None }));
                            }
                        });
                        ui.separator();
                        ui.menu_button(tr("cmd.open_with_app"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::SystemOpen(idx, s));
                                }
                            }
                        });
                        ui.menu_button(tr("cmd.reveal"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::Reveal(idx, s));
                                }
                            }
                        });
                        ui.menu_button(tr("cmd.copy_path"), |ui| {
                            for s in 0..n {
                                if ui.button(side_name(s, n)).clicked() {
                                    menu = Some(MenuCmd::CopyPath(idx, s));
                                }
                            }
                        });
                        ui.separator();
                        if ui.button(tr("cmd.select_pattern")).clicked() {
                            menu = Some(MenuCmd::SelectPattern);
                        }
                        if menu.is_some() {
                            ui.close_menu();
                        }
                    });
                });
            });

        if let Some(col) = new_sort {
            self.sort = if self.sort.0 == col {
                (col, !self.sort.1)
            } else {
                (col, true)
            };
            self.view_key = None;
        }
        if let Some((vi, mods)) = click {
            let idx = self.visible[vi];
            if mods.command {
                if !self.selected.remove(&idx) {
                    self.selected.insert(idx);
                }
                self.anchor = Some(vi);
            } else if mods.shift && self.anchor.is_some() {
                let a = self.anchor.unwrap().min(self.visible.len() - 1);
                let (lo, hi) = if a < vi { (a, vi) } else { (vi, a) };
                self.selected = self.visible[lo..=hi].iter().copied().collect();
            } else {
                self.selected.clear();
                self.selected.insert(idx);
                self.anchor = Some(vi);
            }
            self.cursor = Some(vi);
        }
        if let Some(i) = toggle {
            let key = self.items[i].rel_path();
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.view_key = None;
        }
        if let Some(i) = double {
            let it = &self.items[i];
            if it.is_dir && cfg.tree_mode && !it.children.is_empty() {
                let key = it.rel_path();
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
                self.view_key = None;
            } else {
                result = self.open_action(i, None);
                self.peek = false;
            }
        }
        self.peek_window(ui.ctx());
        if let Some(m) = menu {
            match m {
                MenuCmd::Open(i, kind) => result = self.open_action(i, kind),
                MenuCmd::Op(kind) => self.request_op(kind, cfg),
                MenuCmd::SystemOpen(i, s) => system_open(&self.items[i].path(&self.roots[s])),
                MenuCmd::Reveal(i, s) => reveal(&self.items[i].path(&self.roots[s])),
                MenuCmd::CopyPath(i, s) => ui
                    .ctx()
                    .copy_text(self.items[i].path(&self.roots[s]).display().to_string()),
                MenuCmd::CopyTo(s) => self.pick_copy_destination(s),
                MenuCmd::SelectPattern => self.open_select_pattern(),
            }
        }

        // confirmation dialog
        if let Some(op) = self.confirm.take() {
            let mut keep = true;
            let mut run = false;
            let msg = match op.kind {
                OpKind::Copy { from, to } => trf(
                    "msg.confirm_copy",
                    &[
                        ("n", op.targets.len().to_string()),
                        ("from", side_name(from, n).into()),
                        ("to", side_name(to, n).into()),
                    ],
                ),
                OpKind::Delete { side } => trf(
                    "msg.confirm_delete",
                    &[
                        ("n", op.targets.len().to_string()),
                        (
                            "side",
                            side.map(|s| side_name(s, n))
                                .unwrap_or(tr("side.all"))
                                .into(),
                        ),
                    ],
                ),
            };
            egui::Modal::new(self.id.with("confirm")).show(ui.ctx(), |ui| {
                ui.set_min_width(400.0);
                ui.set_max_width(520.0);
                let pal = widgets::pal(ui);
                ui.heading(tr("dlg.confirm"));
                ui.label(msg);
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(pal.surface_alt)
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for &i in op.targets.iter().take(10) {
                            ui.label(
                                egui::RichText::new(self.items[i].rel_path())
                                    .monospace()
                                    .color(pal.text_dim),
                            );
                        }
                        if op.targets.len() > 10 {
                            ui.label("…");
                        }
                    });
                widgets::dialog_footer(ui, |ui| {
                    if widgets::primary_button(ui, tr("btn.ok")).clicked() {
                        run = true;
                        keep = false;
                    }
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        keep = false;
                    }
                });
            });
            if run {
                self.execute(op, cfg);
            } else if keep {
                self.confirm = Some(op);
            }
        }
        self.poll_merge(ui.ctx());
        self.merge_dialog(ui, cfg);
        self.select_pattern_dialog(ui, cfg);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_through_files_with_differences_in_display_order() {
        let cfg = Config::default();
        let roots = ["samples/left", "samples/right"]
            .map(PathBuf::from)
            .to_vec();
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        let order = v.display_order(&cfg, true);
        let differs: Vec<String> = order
            .iter()
            .map(|&i| &v.items[i])
            .filter(|it| {
                !it.is_dir && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
            })
            .map(|it| it.rel_path())
            .collect();
        assert!(differs.len() >= 3, "{differs:?}");
        assert!(!differs.iter().any(|r| r == "same.txt"));

        let mut walked = vec![differs[0].clone()];
        while let Some(i) = v.adjacent_different_file(&cfg, walked.last().unwrap(), true) {
            walked.push(v.items[i].rel_path());
        }
        assert_eq!(walked, differs);
        let mut back = vec![differs.last().unwrap().clone()];
        while let Some(i) = v.adjacent_different_file(&cfg, back.last().unwrap(), false) {
            back.push(v.items[i].rel_path());
        }
        back.reverse();
        assert_eq!(back, differs);

        let nested = order
            .iter()
            .copied()
            .find(|&i| v.items[i].parent.is_some() && !v.items[i].is_dir)
            .expect("a file inside a folder");
        let Some(FolderAction::OpenFiles { rel, .. }) = v.reveal_and_open(nested, &cfg) else {
            panic!("expected files to open");
        };
        assert_eq!(rel, v.items[nested].rel_path());
        assert_eq!(v.cursor.map(|c| v.visible[c]), Some(nested));
    }

    #[test]
    fn selects_by_name_docks_text_files_and_copies_to_a_folder() {
        let cfg = Config::default();
        let roots = ["samples/left", "samples/right"]
            .map(PathBuf::from)
            .to_vec();
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        v.rebuild_visible(&cfg);
        v.select_matching("*.CSV; same.*", &cfg);
        let names: Vec<&str> = v
            .selected
            .iter()
            .map(|&i| v.items[i].name.as_str())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"data.csv") && names.contains(&"same.txt"));

        let find = |name: &str| v.items.iter().position(|it| it.name == name).unwrap();
        let docked = v.dock_view(find("data.csv"), &cfg).expect("a text compare");
        assert_eq!(docked.n(), 2);
        assert!(!docked.keys_enabled);
        assert!(v.dock_view(find("blob.bin"), &cfg).is_none());

        let dest = tempfile::tempdir().unwrap();
        v.copy_to_folder(1, dest.path());
        assert!(dest.path().join("data.csv").is_file());
        assert!(dest.path().join("same.txt").is_file());

        let mut tree = cfg.clone();
        tree.tree_mode = true;
        v.expanded.clear();
        v.rebuild_visible(&tree);
        let nested = (0..v.items.len())
            .find(|&i| v.items[i].parent.is_some() && !v.items[i].is_dir)
            .unwrap();
        assert!(!v.visible.contains(&nested));
        let name = v.items[nested].name.clone();
        v.select_matching(&name, &tree);
        assert!(v.selected.contains(&nested));
        assert!(v.visible.contains(&nested));

        v.confirm = Some(PendingOp {
            kind: OpKind::Delete { side: None },
            targets: vec![nested],
        });
        v.merge_plan = Some(folder_merge::Plan::default());
        assert!(v.has_dialog());

        let stats = v.statistics();
        let get = |key: &'static str| stats.iter().find(|(k, _)| k == tr(key)).unwrap().1.clone();
        assert_eq!(get("stats.identical"), "1");

        v.start_compare();
        assert!(!v.has_dialog());
    }
}
