//! Folder compare view.

use crate::config::Config;
use crate::core::archive;
use crate::core::diff::DiffOptions;
use crate::core::filter::{FileFilter, FltFilter};
use crate::core::folder::{self, CmpStatus, DirItem, FolderOptions, Progress, Scanner};
use crate::core::folder_merge::{self, Step};
use crate::core::folder_sync;
use crate::core::patch::{self, PatchStyle};
use crate::core::report::{self, fmt_size, fmt_time, side_name, status_text, ReportFormat};
use crate::core::rules::{self, RuleDraft};
use crate::core::text::{self, FileKind, TextDoc};
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::sync_dialog::SyncDialog;
use crate::ui::text_view::TextView;
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, Icon, Outcome};
use egui::{vec2, Align, Color32, FontId, Key, Layout, Margin, Modifiers, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

pub enum FolderAction {
    OpenCaptured {
        comparison: super::baseline_panel::CapturedComparison,
    },
    OpenFiles {
        paths: Vec<PathBuf>,
        missing: Vec<bool>,
        /// Sides that must not be edited, such as a Git revision, and what to call them.
        read_only: Vec<bool>,
        descriptions: Vec<Option<String>>,
        kind: Option<FileKind>,
        /// Relative path of the item, to find the next or previous file from it later.
        rel: String,
        /// Compare rules file of the folder compare.
        rules: String,
        snapshot_options: Option<Box<DiffOptions>>,
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
    /// Show (true) or hide the file compare below the folder list.
    FilePane(bool),
    /// Turn every "Show ... items" view option back on.
    ShowAll,
    /// Add a rule, picked in the docked file compare, to the compare rules file.
    AddRule(RuleDraft),
    /// Open the compare rules files in the options.
    EditRules,
    /// Save the HTML report of the folder compare with the differences of every changed file.
    HtmlReport,
    /// Show or hide each kind of item.
    Show {
        identical: bool,
        different: bool,
        unique: [bool; 3],
    },
    /// Show the differences of every changed file in one tab.
    AllDiffs,
    SaveReview(bool),
    /// Save the HTML report of the docked file compare: a file name to suggest, and what makes
    /// the report.
    ExportFileReport {
        name: String,
        job: report::ReportJob,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Folder,
    Result,
    /// The newest modification time of the sides.
    Modified,
    Size(usize),
    Ext,
}

/// Files counted by how they compare: below a folder, or in the whole comparison.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Tally {
    identical: usize,
    different: usize,
    /// Files on one side only, by side.
    only: [usize; 3],
    errors: usize,
    /// Folders left uncompared (not compared recursively): listed, but saying nothing about
    /// whether their files differ, as in the console compare.
    unknown: usize,
}

impl Tally {
    fn add(&mut self, o: &Tally) {
        self.identical += o.identical;
        self.different += o.different;
        for s in 0..3 {
            self.only[s] += o.only[s];
        }
        self.errors += o.errors;
        self.unknown += o.unknown;
    }

    fn all(&self) -> usize {
        self.identical
            + self.different
            + self.errors
            + self.unknown
            + self.only.iter().sum::<usize>()
    }

    /// Each folder's files and, last, every file. Parents come before their children in
    /// `items`, so one pass from the end adds each item into its parent after its own children.
    fn of(items: &[DirItem], n: usize) -> (Vec<Tally>, Tally) {
        let mut tallies = vec![Tally::default(); items.len()];
        let mut total = Tally::default();
        for i in (0..items.len()).rev() {
            let it = &items[i];
            // Counted as listed: files, and folders with nothing listed below them (empty,
            // not scanned, or not compared recursively). See `FolderView::shown`.
            if !it.is_dir || it.children.is_empty() {
                let own = &mut tallies[i];
                match it.status {
                    CmpStatus::Identical => own.identical = 1,
                    CmpStatus::NotCompared if it.is_dir => own.unknown = 1,
                    CmpStatus::Different | CmpStatus::NotCompared => own.different = 1,
                    CmpStatus::Error => own.errors = 1,
                    CmpStatus::Unique => match only_side(it, n) {
                        Some(s) => own.only[s] = 1,
                        None => own.different = 1,
                    },
                    CmpStatus::Pending => {}
                }
                total.add(&tallies[i]);
            }
            if let Some(p) = it.parent.filter(|&p| p < i) {
                let own = tallies[i];
                tallies[p].add(&own);
            }
        }
        (tallies, total)
    }

    /// "3 different · 1 only in Left", or `None` when nothing below differs.
    fn summary(&self, n: usize) -> Option<String> {
        let mut parts = Vec::new();
        if self.different > 0 {
            parts.push(trf(
                "folder.n_different",
                &[("n", self.different.to_string())],
            ));
        }
        for s in 0..n {
            if self.only[s] > 0 {
                parts.push(trf(
                    "folder.n_only",
                    &[
                        ("n", self.only[s].to_string()),
                        ("side", side_name(s, n).to_string()),
                    ],
                ));
            }
        }
        if self.errors > 0 {
            let key = if self.errors == 1 {
                "folder.n_errors_one"
            } else {
                "folder.n_errors"
            };
            parts.push(trf(key, &[("n", self.errors.to_string())]));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// The side an item is on, when it is on one side only. In a 3-way compare an item missing
/// from one side only differs, for the counts and the filters alike.
fn only_side(it: &DirItem, n: usize) -> Option<usize> {
    let mut sides = (0..n).filter(|&s| it.exists[s]);
    match (sides.next(), sides.next()) {
        (Some(s), None) => Some(s),
        _ => None,
    }
}

/// A popup under the button `r` with the id `id`, opened and closed by clicking the button.
fn popup_below<'a>(
    r: &egui::Response,
    id: egui::Id,
    close: egui::PopupCloseBehavior,
) -> egui::Popup<'a> {
    egui::Popup::from_toggle_button_response(r)
        .id(id)
        .close_behavior(close)
}

/// Short status of a file for the list (the full sentence is its tooltip).
fn short_status(it: &DirItem, n: usize) -> String {
    match it.status {
        CmpStatus::Identical => tr("view.identical").into(),
        CmpStatus::Different if n == 2 || it.diff_side.is_none() => tr("view.different").into(),
        CmpStatus::Error => tr("status.error").into(),
        _ => status_text(it, n),
    }
}

/// The newest modification time of the sides an item is on.
fn newest(it: &DirItem) -> Option<SystemTime> {
    it.mtime.iter().flatten().max().copied()
}

/// The side alone in having the newest modification time, when the sides differ by more than
/// `tolerance` seconds (the time tolerance of the compare options).
fn newer_side(it: &DirItem, n: usize, tolerance: u64) -> Option<usize> {
    let times = || (0..n).filter_map(|s| it.mtime[s].map(|t| (s, t)));
    if times().count() < 2 {
        return None;
    }
    let max = times().map(|(_, t)| t).max()?;
    let tolerance = Duration::from_secs(tolerance);
    let mut newest = times()
        .filter(|&(_, t)| max.duration_since(t).unwrap_or_default() <= tolerance)
        .map(|(s, _)| s);
    match (newest.next(), newest.next()) {
        (Some(s), None) => Some(s),
        _ => None,
    }
}

/// The result shown for an item in the list: a folder's summary of what differs below it, or
/// the short status (`tally` is the folder's, see [`Tally::of`]).
fn shown_status(it: &DirItem, n: usize, tally: Option<&Tally>) -> String {
    let summary = (it.is_dir && it.status == CmpStatus::Different)
        .then(|| tally.and_then(|t| t.summary(n)))
        .flatten();
    match summary {
        Some(summary) => summary,
        None if it.is_dir => status_text(it, n),
        None => short_status(it, n),
    }
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
    Bookmark(usize),
    Comment(usize),
    SaveReview,
}

/// How many items a file operation changed, and its errors.
type OpResult = (usize, Vec<String>);

/// How often a running comparison refreshes the list with what it has found so far, at most.
const UPDATE_INTERVAL: Duration = Duration::from_millis(250);

struct Running {
    progress: Arc<Progress>,
    handle: JoinHandle<Vec<DirItem>>,
    started: Instant,
    updated: Instant,
}

/// The panels of the side drawer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FolderPanel {
    Review,
    Verification,
    Baselines,
}

pub struct FolderView {
    /// Whether the side drawer is open, and the panel it shows.
    pub drawer_open: bool,
    pub drawer_tab: FolderPanel,
    merge_verification: Box<super::merge_verification::FolderVerification>,
    pub workspace: crate::core::review_state::Workspace,
    pub baseline_panel: Box<super::baseline_panel::BaselinePanel>,
    review_filter: Option<crate::core::review_state::State>,
    review_stale_only: bool,
    review_fingerprints:
        Option<super::analysis::Task<BTreeMap<String, crate::core::review_state::Prints>>>,
    review_checked: bool,
    review_resume: Option<String>,

    pub roots: Vec<PathBuf>,
    pub notes: BTreeMap<String, String>,
    note_edit: Option<(String, String)>,
    snapshot_options: Option<DiffOptions>,
    pub display_roots: Vec<String>,
    /// Extracted archives, shared with the tabs that show their files.
    temps: Vec<std::sync::Arc<tempfile::TempDir>>,
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
    /// Each item's files by how they compare, and every file's, as of the last rebuild.
    tallies: Vec<Tally>,
    total: Tally,
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
    /// Where the docked compare of the item it is loading opens.
    dock_land: Option<Land>,
    /// Counts changes of the items or of how they are listed.
    list_gen: u64,
    /// The file and difference under the cursor among those next and previous stop at.
    nav_text: Option<String>,
    /// The way the last step went, for skipping a file that cannot be shown.
    dock_forward: bool,
    /// The last step went through open conflicts only.
    dock_conflicts: bool,
    /// The files next and previous step through, in order, while the list and the options
    /// it depends on stay the same.
    stops: std::cell::RefCell<Option<(StopsKey, std::rc::Rc<Stops>)>>,
    /// A message for the status bar, until the app takes it.
    notice: Option<&'static str>,
    /// Text of the "select by name" dialog while it is open.
    select_pattern: Option<String>,
    /// A 3-way auto merge being planned, then waiting for confirmation.
    merge_job: Option<JoinHandle<folder_merge::Plan>>,
    merge_plan: Option<folder_merge::Plan>,
    /// Copying or merging files on another thread: how many items it changed and the errors,
    /// and whether to compare again afterwards.
    file_job: Option<(JoinHandle<OpResult>, bool)>,
    /// Set to stop the auto merge running as `file_job`, which then undoes what it changed.
    merge_stop: Option<Arc<AtomicBool>>,
    /// The synchronization of the two folders being previewed or applied.
    sync: Option<Box<SyncDialog>>,
    /// Sides nothing may be copied to or deleted from, such as an extracted Git revision.
    pub read_only: Vec<bool>,
    /// For a side that is a genuine archive extracted to compare, its archive file; edits
    /// made to it are repacked back into this file on save. `None` for every other side,
    /// such as a plain folder or an archive shown read-only (a review snapshot, a web tree).
    archive_sources: Vec<Option<PathBuf>>,
    /// Whether each archive side in `archive_sources` has been edited since it was last
    /// repacked (or opened, if never).
    archive_dirty: Vec<bool>,
    /// Whether each side is a folder fetched over SFTP or FTP, written back to its server on
    /// save by the app (which holds the connection), and whether it was edited since.
    remote_sides: Vec<bool>,
    remote_dirty: Vec<bool>,
    /// Archive files written by saving the archive sides, outer archives they are nested in
    /// included, not yet taken by [`Self::take_written`].
    written: Vec<PathBuf>,
    /// Compare rules file used (a name or path, "" = none); read again on every refresh.
    pub rules: String,
    /// Whether `rules` named a file that could be read the last time it was applied.
    rules_found: bool,
    /// Names of the rules files, listed while the rules popup is open.
    rule_names: Vec<String>,
    /// A rule picked in the docked file compare, until the app takes it.
    rule_request: Option<RuleDraft>,
    /// The HTML report asked for in the docked file compare, until the app takes it.
    file_report: Option<(String, report::ReportJob)>,
    /// The last comparison was stopped before comparing every file.
    stopped: bool,
    /// The last comparison broke off with an internal error.
    failed: bool,
    /// The last comparison listed every subfolder, also those on one side only.
    whole_tree: bool,
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
    Loading(std::sync::mpsc::Receiver<DockLoad>),
    /// The compare, or `None` when the item is not a text file, with the files' modification
    /// times and when they were last checked, to load it again when a file is saved.
    Ready(Option<Box<TextView>>, Vec<Option<SystemTime>>, Instant),
    /// The files could not be read.
    Failed(String),
}

/// The docked compare of a file, `None` when it is not a text file, or why it failed.
type DockLoad = Result<Option<Box<TextView>>, String>;

/// Where the docked compare of the item it is loading opens: at its first or last difference,
/// or the difference it showed before it was loaded again, then moved on by the next (+1) and
/// previous (-1) steps asked for while it loaded.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Land {
    item: usize,
    at: LandAt,
    steps: isize,
    /// Open conflicts only: a file without one is passed over.
    conflicts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LandAt {
    First,
    Last,
    Nth(usize),
}

impl Land {
    fn new(item: usize, first: bool) -> Self {
        Land {
            item,
            at: if first { LandAt::First } else { LandAt::Last },
            steps: 0,
            conflicts: false,
        }
    }

    fn conflict(item: usize, first: bool) -> Self {
        Land {
            conflicts: true,
            ..Land::new(item, first)
        }
    }
}

/// What the files stepped through depend on besides the list: tree mode and the status chips.
type StopsKey = (u64, bool, bool, bool, [bool; 3]);

/// The order next and previous go through items in, and the files they stop at.
struct Stops {
    /// The files stopped at, in order.
    files: Vec<usize>,
    /// The place of each item in the order, by item; `None` for an item not in it.
    rank: Vec<Option<usize>>,
    /// The item of each path in the order.
    item: HashMap<String, usize>,
}

/// How often the docked file compare checks whether its files changed.
const DOCK_RECHECK: Duration = Duration::from_secs(1);

fn mtimes(it: &DirItem, roots: &[PathBuf]) -> Vec<Option<SystemTime>> {
    roots
        .iter()
        .map(|r| {
            std::fs::metadata(it.path(r))
                .and_then(|m| m.modified())
                .ok()
        })
        .collect()
}

/// Whether the docked file compare may show `it` as text, before reading it.
fn may_dock(it: &DirItem) -> bool {
    !it.is_dir && !it.binary && it.size.iter().all(|s| s.unwrap_or(0) <= DOCK_MAX_BYTES)
}

/// The sides of `it` as read-only documents for the docked file compare, or `None` when it
/// is not a text file small enough to show.
/// The files of `it` for the docked compare: `None` when it is not a text file.
fn load_dock_docs(
    it: &DirItem,
    roots: &[PathBuf],
    opts: &DiffOptions,
) -> Result<Option<Vec<TextDoc>>, String> {
    let n = roots.len();
    if !may_dock(it) {
        return Ok(None);
    }
    let mut docs = Vec::with_capacity(n);
    for (s, root) in roots.iter().enumerate() {
        let p = it.path(root);
        let mut d = if it.exists[s] {
            if crate::core::unpacker::find(opts, &p).is_none()
                && text::detect_kind(&p) == FileKind::Binary
            {
                return Ok(None);
            }
            crate::core::unpacker::load_doc(&p, opts, None)
                .map_err(|e| format!("{}: {e}", p.display()))?
        } else {
            let mut d = TextDoc::empty(None);
            d.description = Some(format!("{} — {}", p.display(), tr("peek.missing")));
            d
        };
        d.read_only = true;
        docs.push(d);
    }
    Ok(Some(docs))
}

/// The read-only compare of `it` for the docked file compare, when it is a text file. It reads
/// and diffs the files, so it runs on another thread.
fn load_dock_view(it: &DirItem, roots: &[PathBuf], cfg: &Config, dopts: &DiffOptions) -> DockLoad {
    let Some(docs) = load_dock_docs(it, roots, dopts)? else {
        return Ok(None);
    };
    let cfg = Config {
        diff: dopts.clone(),
        ..cfg.clone()
    };
    let mut v = TextView::new(docs, &cfg);
    v.keys_enabled = false;
    Ok(Some(Box::new(v)))
}

/// Case-insensitive ordering without allocating lowercase copies.
/// Compares `a` and `b` with runs of digits as numbers: "2 different" before "10 different".
fn cmp_numeric(mut a: &str, mut b: &str) -> std::cmp::Ordering {
    let digits = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    loop {
        let (da, db) = (digits(a), digits(b));
        if da > 0 && db > 0 {
            let (na, nb) = (
                a[..da].trim_start_matches('0'),
                b[..db].trim_start_matches('0'),
            );
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if o.is_ne() {
                return o;
            }
            (a, b) = (&a[da..], &b[db..]);
            continue;
        }
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        if ca != cb {
            return ca.cmp(&cb);
        }
        (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
    }
}

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

/// Starts `cmd` without waiting for it; an error names the file it was started for.
fn launch(mut cmd: std::process::Command, path: &std::path::Path) -> Result<(), String> {
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Opens `path` (a file or folder) with the program the system uses for it.
#[cfg(windows)]
pub(crate) fn system_open(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    // The shell gets the path as a file to open, never inside a command line it parses.
    let file: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success.
    if code as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ))
    }
}

/// Opens `path` (a file or folder) with the program the system uses for it.
#[cfg(not(windows))]
pub(crate) fn system_open(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path);
    launch(cmd, path)
}

fn reveal(path: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer reads `/select,"path"` itself and not as quoting of the whole argument
        // would have it; Windows paths can't contain quotes.
        let mut cmd = std::process::Command::new("explorer");
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        launch(cmd, path)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg("-R").arg(path);
        launch(cmd, path)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        system_open(path.parent().unwrap_or(path))
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
        Self::with_snapshot(roots, display_roots, temps, filter_spec, named, cfg, None)
    }

    fn with_snapshot(
        roots: Vec<PathBuf>,
        display_roots: Vec<String>,
        temps: Vec<tempfile::TempDir>,
        filter_spec: &str,
        named: &[FltFilter],
        cfg: &Config,
        snapshot_options: Option<DiffOptions>,
    ) -> Self {
        // A side extracted from a real archive to compare it can be edited and repacked;
        // any other extracted copy (a review snapshot, a web page tree, ...) is read-only,
        // as changes to it would otherwise be lost. So is an archive that can't be written
        // back, or is nested in an extracted archive that can't (see `archive::editable`).
        let temp_backed: Vec<bool> = roots
            .iter()
            .map(|r| temps.iter().any(|t| r.starts_with(t.path())))
            .collect();
        let archive_sources: Vec<Option<PathBuf>> = temp_backed
            .iter()
            .zip(&display_roots)
            .map(|(&backed, disp)| {
                if !backed {
                    return None;
                }
                let p = PathBuf::from(disp);
                let editable = archive::is_archive(&p) && archive::editable(&p);
                editable.then_some(p)
            })
            .collect();
        let read_only: Vec<bool> = temp_backed
            .iter()
            .zip(&archive_sources)
            .map(|(&backed, src)| backed && src.is_none())
            .collect();
        let archive_dirty = vec![false; archive_sources.len()];
        let mut v = FolderView {
            drawer_open: false,
            drawer_tab: FolderPanel::Review,
            merge_verification: Default::default(),
            workspace: Default::default(),
            baseline_panel: Box::new(super::baseline_panel::BaselinePanel::with_review_links()),
            review_filter: None,
            review_stale_only: false,
            review_fingerprints: None,
            review_checked: false,
            review_resume: None,

            roots,
            notes: BTreeMap::new(),
            note_edit: None,
            snapshot_options,
            display_roots,
            temps: temps.into_iter().map(std::sync::Arc::new).collect(),
            items: Vec::new(),
            filter_spec: filter_spec.to_string(),
            filter: FileFilter::resolve(filter_spec, named),
            fopts: cfg.folder.clone(),
            dopts: cfg.comparison_options(),
            running: None,
            elapsed: None,
            expanded: HashSet::new(),
            selected: BTreeSet::new(),
            anchor: None,
            cursor: None,
            sort: (Col::Name, true),
            visible: Vec::new(),
            tallies: Vec::new(),
            total: Tally::default(),
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
            dock_land: None,
            dock_forward: true,
            dock_conflicts: false,
            list_gen: 0,
            nav_text: None,
            stops: Default::default(),
            notice: None,
            select_pattern: None,
            merge_job: None,
            merge_plan: None,
            file_job: None,
            merge_stop: None,
            sync: None,
            read_only: Vec::new(),
            archive_sources: Vec::new(),
            archive_dirty: Vec::new(),
            remote_sides: Vec::new(),
            remote_dirty: Vec::new(),
            written: Vec::new(),
            rules: cfg.rules.clone(),
            rules_found: false,
            rule_names: Vec::new(),
            rule_request: None,
            file_report: None,
            stopped: false,
            failed: false,
            whole_tree: false,
            id: egui::Id::unique(("folderview", std::time::Instant::now())),
        };
        v.read_only = read_only;
        v.archive_sources = archive_sources;
        v.archive_dirty = archive_dirty;
        v.set_rules(cfg, cfg.rules.clone());
        v.start_compare();
        v
    }

    pub fn from_review(review: crate::core::review::Review, cfg: &Config) -> Self {
        let manifest = review.manifest;
        let mut local = cfg.clone();
        local.diff = manifest.options.clone();
        local.folder = manifest.folder;
        local.unpackers.clear();
        local.rules.clear();
        // Inputs were already filtered at capture time, including named .flt filters.
        let mut view = Self::with_snapshot(
            review.roots,
            manifest.labels,
            vec![review.temp],
            "*",
            &[],
            &local,
            Some(manifest.options),
        );
        view.filter_spec = manifest.filter;
        view.notes = manifest.notes;
        view.review_resume = manifest.workspace.current.clone();
        view.workspace = manifest.workspace;
        view.peek = false;
        view.read_only = vec![true; view.n()];
        // A review is a frozen snapshot: never write edits back into an archive from here,
        // even if its labels happen to match one (`archive_sources` is normally derived
        // from a real extraction's display labels, see `with_snapshot`).
        view.archive_sources = vec![None; view.n()];
        view.archive_dirty = vec![false; view.n()];
        view
    }

    pub fn accept_review_update(
        &mut self,
        path: String,
        mut decision: crate::core::review_state::Decision,
    ) {
        match crate::core::review_state::prints(&path, &self.roots, &self.dopts) {
            Ok(p) => {
                if decision.fingerprint.is_empty() {
                    decision.fingerprint = p.fingerprint;
                    decision.content = p.content;
                } else if decision.fingerprint != p.fingerprint {
                    decision.stale = true;
                } else if decision.content.is_empty() {
                    decision.content = p.content;
                }
            }
            Err(_) => decision.stale = true,
        }
        self.workspace.current = Some(path.clone());
        self.workspace.entries.insert(path, decision);
        self.view_key = None;
        self.list_gen += 1;
    }

    pub fn save_review(&self, path: &Path, selected_only: bool) -> anyhow::Result<()> {
        if self.running.is_some()
            || self.file_job.is_some()
            || self.review_fingerprints.is_some()
            || (!self.review_checked && !self.workspace.entries.is_empty())
            || self.stopped
            || self.failed
        {
            anyhow::bail!(tr("review.finish_first"));
        }
        let selected_paths: Vec<String> = self
            .selected
            .iter()
            .map(|&i| self.items[i].rel_path())
            .collect();
        if selected_only && selected_paths.is_empty() {
            anyhow::bail!(tr("review.select_first"));
        }
        let items: Vec<&DirItem> = self
            .items
            .iter()
            .filter(|item| {
                if !selected_only {
                    return true;
                }
                let path = item.rel_path();
                selected_paths
                    .iter()
                    .any(|p| path == *p || path.starts_with(&format!("{p}/")))
            })
            .collect();
        crate::core::review::save_with_workspace(
            path,
            crate::core::review::Capture {
                roots: &self.roots,
                labels: &self.display_roots,
                items: &items,
                options: &self.dopts,
                folder: &self.fopts,
                filter: &self.filter_spec,
                notes: &self.notes,
            },
            &self.workspace,
        )
    }

    fn note_window(&mut self, ui: &Ui) {
        let Some((path, mut note)) = self.note_edit.take() else {
            return;
        };
        let mut open = !ui.input(|i| i.key_pressed(Key::Escape));
        let mut done = false;
        let mut cancel = false;
        egui::Window::new(tr("dlg.bookmark_comment"))
            .id(self.id.with("folder_note"))
            .collapsible(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(&path);
                ui.add(
                    egui::TextEdit::multiline(&mut note)
                        .desired_rows(4)
                        .desired_width(360.0),
                );
                ui.horizontal(|ui| {
                    done = ui.button(tr("btn.ok")).clicked();
                    cancel = ui.button(tr("btn.cancel")).clicked();
                });
            });
        if done {
            self.notes.insert(path, note);
        } else if open && !cancel {
            self.note_edit = Some((path, note));
        }
    }

    fn report_notes(&self, content: String, format: ReportFormat) -> String {
        let content = crate::core::review::annotate_report(content, format, &self.notes);
        if self.workspace.entries.is_empty() {
            content
        } else {
            crate::core::review_state::annotate_report(
                content,
                format,
                &self.workspace.summary(
                    self.items
                        .iter()
                        .filter(|i| !i.is_dir)
                        .map(|i| i.rel_path()),
                ),
            )
        }
    }

    pub fn set_recursive(&mut self, recursive: bool) {
        self.fopts.recursive = recursive;
    }

    pub fn set_method(&mut self, method: folder::CompareMethod) {
        self.fopts.method = method;
    }

    /// This comparison as a compare job: its folders, filter, method, rules and the comparison
    /// options that are on, so the job compares as this tab does.
    pub fn as_job(&self, cfg: &Config) -> crate::core::job::Job {
        use crate::core::diff::WhitespaceMode;
        use crate::core::folder::CompareMethod;
        let path = |i: usize| self.roots[i].display().to_string();
        let n = self.roots.len();
        let d = cfg.comparison_options();
        let method = match self.fopts.method {
            CompareMethod::FullContents => "full",
            CompareMethod::QuickContents => "quick",
            CompareMethod::BinaryContents => "binary",
            CompareMethod::Date => "date",
            CompareMethod::DateSize => "datesize",
            CompareMethod::Size => "size",
        };
        crate::core::job::Job {
            left: Some(path(0)),
            base: (n == 3).then(|| path(1)),
            right: Some(path(n.saturating_sub(1))),
            recursive: self.fopts.recursive,
            filter: Some(self.filter_spec.trim().to_string()).filter(|f| !f.is_empty()),
            method: Some(method.into()),
            structured: self.fopts.structured,
            // "" compares without rules, as this tab does, rather than with those selected later.
            rules: Some(self.used_rules().to_string()),
            whitespace: match d.whitespace {
                WhitespaceMode::Compare => None,
                WhitespaceMode::IgnoreChange => Some("change".into()),
                WhitespaceMode::IgnoreAll => Some("all".into()),
            },
            ignore_case: d.ignore_case,
            ignore_blank: d.ignore_blank_lines,
            ignore_eol: d.ignore_eol,
            ignore_numbers: d.ignore_numbers,
            ignore_comments: d.ignore_comments,
            prettify: d.prettify,
            numeric_tolerance: d.numeric_tolerance_enabled.then_some(d.numeric_tolerance),
            ..Default::default()
        }
    }

    /// Uses the compare rules file `rules` ("" = none) from the next comparison on.
    pub fn set_rules(&mut self, cfg: &Config, rules: String) {
        if let Some(options) = &self.snapshot_options {
            self.dopts = options.clone();
            return;
        }
        self.rules = rules;
        let set = rules::find(&self.rules);
        self.rules_found = set.is_some();
        self.dopts = rules::options_with(&cfg.comparison_options(), set.as_ref());
    }

    /// The compare rules file as reports name it: none when it could not be read, as the
    /// files were then compared without it.
    fn used_rules(&self) -> &str {
        if self.rules_found {
            &self.rules
        } else {
            ""
        }
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
        self.baseline_panel.invalidate();
        self.merge_verification.clear_validation();
        self.review_checked = false;
        self.review_fingerprints = None;
        if let Some(r) = &self.running {
            r.progress.abort.store(true, Ordering::Relaxed);
        }
        let progress = Arc::new(Progress::live());
        self.whole_tree = self.fopts.recursive && self.fopts.include_unique_subfolders;
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
        self.dock_land = None;
        // They refer to items by index.
        self.confirm = None;
        self.merge_job = None;
        self.merge_plan = None;
        self.selected.clear();
        self.anchor = None;
        self.cursor = None;
        self.view_key = None;
        self.list_gen += 1;
        self.status.clear();
    }

    /// Re-runs the comparison with new options (F5 / options changed).
    pub fn refresh(&mut self, cfg: &Config, named: &[FltFilter]) {
        if self.snapshot_options.is_some() {
            self.start_compare();
            return;
        }
        let recursive = self.fopts.recursive;
        let method = self.fopts.method;
        self.fopts = cfg.folder.clone();
        self.fopts.recursive = recursive;
        self.fopts.method = method;
        self.set_rules(cfg, self.rules.clone());
        self.filter = FileFilter::resolve(&self.filter_spec, named);
        self.start_compare();
    }

    /// Extracted archives the compared folders are in.
    pub fn temps(&self) -> &[std::sync::Arc<tempfile::TempDir>] {
        &self.temps
    }

    /// Whether a side is an archive extracted to compare, rather than a plain folder.
    pub fn is_archive_side(&self, s: usize) -> bool {
        self.archive_sources.get(s).is_some_and(Option::is_some)
    }

    /// The archive file side `s` is repacked into when saved, if it is an archive side.
    pub fn archive_source(&self, s: usize) -> Option<&Path> {
        self.archive_sources.get(s)?.as_deref()
    }

    /// Marks the archive side `s` as having unpacked edits pending, so a save repacks it.
    /// Does nothing if `s` is not an archive side (see [`Self::is_archive_side`]).
    pub fn mark_archive_dirty(&mut self, s: usize) {
        if let Some(dirty) = self.archive_dirty.get_mut(s) {
            *dirty = true;
        }
    }

    /// Marks every archive side as possibly edited, after a job that may have written into
    /// any of them (an automatic merge, a synchronization): saving then repacks those whose
    /// contents really changed.
    fn mark_archives_dirty(&mut self) {
        for s in 0..self.n() {
            self.mark_written(s);
        }
    }

    /// Marks side `s` as written into, for saving to write it back into its archive file or
    /// to its server (see [`Self::mark_archive_dirty`] and [`Self::mark_remote_dirty`]).
    fn mark_written(&mut self, s: usize) {
        if self.is_archive_side(s) {
            self.mark_archive_dirty(s);
        }
        self.mark_remote_dirty(s);
    }

    /// Whether any archive side has edits not yet written back into its archive file.
    pub fn has_archive_changes(&self) -> bool {
        self.archive_dirty.iter().any(|&d| d)
    }

    /// Makes side `s` a folder fetched from a server, for saving to write back to it.
    pub fn set_remote_side(&mut self, s: usize) {
        let n = self.n();
        self.remote_sides.resize(n, false);
        self.remote_dirty.resize(n, false);
        if let Some(r) = self.remote_sides.get_mut(s) {
            *r = true;
        }
    }

    pub fn is_remote_side(&self, s: usize) -> bool {
        self.remote_sides.get(s).copied().unwrap_or(false)
    }

    /// Marks the remote side `s` as possibly edited here, so saving writes it back to its
    /// server. Does nothing if `s` is not a remote side (see [`Self::set_remote_side`]).
    pub fn mark_remote_dirty(&mut self, s: usize) {
        if self.is_remote_side(s) {
            self.remote_dirty[s] = true;
        }
    }

    /// Whether the remote side `s` was marked edited, clearing the mark: the app then tracks
    /// the side until its upload is done.
    pub fn take_remote_dirty(&mut self, s: usize) -> bool {
        self.remote_dirty.get_mut(s).is_some_and(std::mem::take)
    }

    /// Whether any remote side has edits not yet written back to its server.
    pub fn has_remote_changes(&self) -> bool {
        self.remote_dirty.iter().any(|&d| d)
    }

    /// Repacks every archive side with pending edits back into its archive file, and each
    /// archive that one is nested in, returning the archive files that were written (an
    /// archive whose contents came out unchanged is left alone) and the errors of those that
    /// could not be. A side that fails keeps its edits pending and does not stop the others.
    pub fn save_archives(&mut self) -> (Vec<PathBuf>, Vec<String>) {
        let mut written = Vec::new();
        let mut errors = Vec::new();
        for s in 0..self.archive_sources.len() {
            if !self.archive_dirty[s] {
                continue;
            }
            let Some(src) = &self.archive_sources[s] else {
                // Not an archive side (or it stopped being one on a later refresh): there is
                // nothing to repack it into, so the flag was a mistake and is cleared here.
                self.archive_dirty[s] = false;
                continue;
            };
            match archive::repack_all(src, &self.roots[s]) {
                Ok(w) => written.extend(w),
                Err(e) => {
                    errors.push(format!("{e:#}"));
                    continue;
                }
            }
            self.archive_dirty[s] = false;
        }
        self.written.extend(written.iter().cloned());
        (written, errors)
    }

    /// The archive files saving has written since this was last called.
    pub fn take_written(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.written)
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Whether a compare, a merge plan, file operations or the check of review decisions are
    /// still under way.
    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.running.is_some()
            || self.merge_job.is_some()
            || self.file_job.is_some()
            || self.review_fingerprints.is_some()
            || (!self.review_checked && !self.workspace.entries.is_empty())
    }

    /// How long the comparison took, once, right after it finished without being stopped.
    pub fn take_finished(&mut self) -> Option<Duration> {
        self.finished.take()
    }

    pub fn take_notice(&mut self) -> Option<&'static str> {
        self.notice.take()
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
        if let Some(stop) = &self.merge_stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(sync) = &self.sync {
            sync.stop();
        }
    }

    fn poll(&mut self) {
        let Some(r) = &mut self.running else { return };
        if r.handle.is_finished() {
            let r = self.running.take().unwrap();
            let aborted = r.progress.abort.load(Ordering::Relaxed);
            // Same items in the same order as the streamed ones, so the selection stays valid.
            let joined = r.handle.join();
            self.failed = joined.is_err();
            self.items = joined.unwrap_or_default();
            self.stopped = aborted || self.failed;
            self.elapsed = Some(r.started.elapsed());
            self.selected.retain(|&i| i < self.items.len());
            self.view_key = None;
            self.list_gen += 1;
            self.peek_cache = None;
            if self.failed {
                self.status = tr("status.error").into();
            } else if aborted {
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
        self.list_gen += 1;
    }

    // ---------- visibility & sorting ----------

    fn shown(&self, it: &DirItem, cfg: &Config) -> bool {
        let n = self.n();
        if !it.exists[..n].iter().any(|&e| e) {
            return false;
        }
        match it.status {
            // Counted in no chip but All, so no chip hides them either (see `Tally::of`).
            CmpStatus::NotCompared if it.is_dir => true,
            CmpStatus::Identical => cfg.show_identical,
            CmpStatus::Different | CmpStatus::Error | CmpStatus::NotCompared => cfg.show_different,
            CmpStatus::Pending => false,
            CmpStatus::Unique => match only_side(it, n) {
                Some(s) if n == 2 && s == 1 => cfg.show_unique[2],
                Some(s) => cfg.show_unique[s],
                None => cfg.show_different,
            },
        }
    }

    /// Sorts `v` into list order after `first`, working out the result shown for each item once
    /// rather than for every comparison.
    fn sort_items(&self, v: &mut [usize], first: impl Fn(usize, usize) -> std::cmp::Ordering) {
        let n = self.n();
        let shown: BTreeMap<usize, String> = if self.sort.0 == Col::Result {
            v.iter()
                .map(|&i| (i, shown_status(&self.items[i], n, self.tallies.get(i))))
                .collect()
        } else {
            BTreeMap::new()
        };
        v.sort_by(|&a, &b| first(a, b).then_with(|| self.cmp_items(a, b, &shown)));
    }

    /// `shown`: the result shown for each item, when sorting by it.
    fn cmp_items(&self, a: usize, b: usize, shown: &BTreeMap<usize, String>) -> std::cmp::Ordering {
        let (ia, ib) = (&self.items[a], &self.items[b]);
        let (col, asc) = self.sort;
        let name = || cmp_ci(&ia.name, &ib.name);
        let o = match col {
            Col::Name => ib.is_dir.cmp(&ia.is_dir).then_with(name),
            Col::Folder => cmp_ci(&ia.rel_dir, &ib.rel_dir).then_with(name),
            Col::Result => {
                let shown = |i: usize| shown.get(&i).map_or("", String::as_str);
                cmp_numeric(shown(a), shown(b)).then_with(name)
            }
            Col::Modified => newest(ia).cmp(&newest(ib)).then_with(name),
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
        self.sort_items(&mut v, |a, b| {
            if self.sort.0 == Col::Name {
                cmp_ci(&self.items[a].rel_dir, &self.items[b].rel_dir)
            } else {
                std::cmp::Ordering::Equal
            }
        });
        v
    }

    /// Works out the list and its counts again when they may have changed. Then draws once
    /// more, as the chips above the list may have shown the old counts this frame.
    fn update_visible(&mut self, ctx: &egui::Context, cfg: &Config) {
        // The tree has no Folder column to show the sort on.
        if cfg.tree_mode && self.sort.0 == Col::Folder {
            self.sort = (Col::Name, true);
            self.view_key = None;
            self.list_gen += 1;
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
            ctx.request_repaint();
        }
    }

    fn rebuild_visible(&mut self, cfg: &Config) {
        self.list_gen += 1;
        (self.tallies, self.total) = Tally::of(&self.items, self.n());
        let item = |at: Option<usize>| at.and_then(|v| self.visible.get(v).copied());
        let (cursor, anchor) = (item(self.cursor), item(self.anchor));
        self.visible = if self.review_filtering() {
            self.review_list(cfg)
        } else {
            self.display_order(cfg, false)
        };
        // The cursor stays on its item, or where it was when that is no longer listed.
        let visible = &self.visible;
        let place = |at: Option<usize>, item: Option<usize>| {
            item.and_then(|i| visible.iter().position(|&x| x == i))
                .or_else(|| Some(at?.min(visible.len().checked_sub(1)?)))
        };
        self.cursor = place(self.cursor, cursor);
        self.anchor = place(self.anchor, anchor);
    }

    fn review_filtering(&self) -> bool {
        self.review_filter.is_some() || self.review_stale_only
    }

    /// The files the review filter lists, in display order.
    fn review_list(&self, cfg: &Config) -> Vec<usize> {
        self.flat_list(cfg)
            .into_iter()
            .filter(|&i| {
                let decision = self
                    .workspace
                    .entries
                    .get(&self.items[i].rel_path())
                    .cloned()
                    .unwrap_or_default();
                !self.items[i].is_dir
                    && self.review_filter.is_none_or(|s| {
                        if decision.stale {
                            s == crate::core::review_state::State::NeedsAction
                        } else {
                            decision.state == s
                        }
                    })
                    && (!self.review_stale_only || decision.stale)
            })
            .collect()
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
        self.sort_items(&mut roots, |_, _| std::cmp::Ordering::Equal);
        let mut stack: Vec<usize> = roots.into_iter().rev().collect();
        while let Some(i) = stack.pop() {
            out.push(i);
            let it = &self.items[i];
            if it.is_dir && (searching || self.expanded.contains(&it.rel_path())) {
                let mut ch: Vec<usize> = it.children.iter().copied().filter(|&c| vis[c]).collect();
                self.sort_items(&mut ch, |_, _| std::cmp::Ordering::Equal);
                stack.extend(ch.into_iter().rev());
            }
        }
        out
    }

    fn is_different_file(&self, i: usize) -> bool {
        let it = &self.items[i];
        !it.is_dir && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
    }

    /// A text file of a three-way compare that both sides changed, unlike the middle and each
    /// other: the files next and previous conflict look for open conflicts in.
    fn may_conflict(&self, i: usize) -> bool {
        let it = &self.items[i];
        self.n() == 3
            && !it.is_dir
            && !it.binary
            && it.status == CmpStatus::Different
            && it.diff_side.is_none()
            && it.exists.iter().all(|&e| e)
    }

    /// The order next and previous go through items in: as listed with every folder
    /// expanded, or the flat list for the review filter; with the files the review filter
    /// lists, which alone are stopped at.
    fn step_base(&self, cfg: &Config) -> (Vec<usize>, Option<HashSet<usize>>) {
        if self.review_filtering() {
            let listed = self.review_list(cfg).into_iter().collect();
            (self.flat_list(cfg), Some(listed))
        } else {
            (self.display_order(cfg, true), None)
        }
    }

    /// The files next and previous stop at: the changed files listed, in display order.
    fn stops(&self, cfg: &Config) -> std::rc::Rc<Stops> {
        let key = (
            self.list_gen,
            cfg.tree_mode,
            cfg.show_identical,
            cfg.show_different,
            cfg.show_unique,
        );
        if let Some((k, stops)) = &*self.stops.borrow() {
            if *k == key {
                return stops.clone();
            }
        }
        let (order, listed) = self.step_base(cfg);
        let mut rank = vec![None; self.items.len()];
        let mut item = HashMap::with_capacity(order.len());
        for (r, &i) in order.iter().enumerate() {
            rank[i] = Some(r);
            item.insert(self.items[i].rel_path(), i);
        }
        let files = order
            .into_iter()
            .filter(|&i| {
                self.is_different_file(i) && listed.as_ref().is_none_or(|l| l.contains(&i))
            })
            .collect();
        let stops = std::rc::Rc::new(Stops { files, rank, item });
        *self.stops.borrow_mut() = Some((key, stops.clone()));
        stops
    }

    /// The file with differences listed after (or before) the item at `rel`, in display order,
    /// also when the review filter leaves that item out.
    pub fn adjacent_different_file(&self, cfg: &Config, rel: &str, forward: bool) -> Option<usize> {
        self.adjacent_stop(cfg, rel, forward, false)
    }

    /// As [`Self::adjacent_different_file`], among the files that may have open conflicts.
    pub fn adjacent_conflict_file(&self, cfg: &Config, rel: &str, forward: bool) -> Option<usize> {
        self.adjacent_stop(cfg, rel, forward, true)
    }

    fn adjacent_stop(
        &self,
        cfg: &Config,
        rel: &str,
        forward: bool,
        conflicts: bool,
    ) -> Option<usize> {
        let s = self.stops(cfg);
        let pos = s.rank[*s.item.get(rel)?]?;
        let wanted = |i: &&usize| !conflicts || self.may_conflict(**i);
        if forward {
            let after = s.files.partition_point(|&i| s.rank[i] <= Some(pos));
            s.files[after..].iter().find(wanted).copied()
        } else {
            let before = s.files.partition_point(|&i| s.rank[i] < Some(pos));
            s.files[..before].iter().rev().find(wanted).copied()
        }
    }

    /// The first (or last) file with differences, in display order.
    fn edge_different_file(&self, cfg: &Config, first: bool) -> Option<usize> {
        let stops = &self.stops(cfg).files;
        if first {
            stops.first().copied()
        } else {
            stops.last().copied()
        }
    }

    /// "File 8 / 42" for item `item` among the files next and previous stop at, or
    /// "File 8 · Scanning…" while the count may still grow.
    fn stop_label(&self, cfg: &Config, item: Option<usize>) -> String {
        let stops = &self.stops(cfg).files;
        let pos = item.and_then(|item| stops.iter().position(|&i| i == item));
        let n = pos.map_or("–".to_string(), |p| (p + 1).to_string());
        if self.running.is_some() {
            format!("{} · {}", trf("nav.file", &[("n", n)]), tr("nav.scanning"))
        } else {
            trf(
                "nav.file_of",
                &[("n", n), ("total", stops.len().to_string())],
            )
        }
    }

    /// The position of file `rel` among the files next and previous stop at, for the status
    /// bar of a tab opened from this compare.
    pub fn file_label(&self, cfg: &Config, rel: &str) -> String {
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let item = self
            .items
            .iter()
            .position(|it| it.name == name && it.rel_dir == dir);
        self.stop_label(cfg, item)
    }

    /// The position shown in the status bar: the file under the cursor among the files next
    /// and previous stop at, and the difference of its docked compare.
    fn nav_label(&self, cfg: &Config) -> Option<String> {
        let cur = self.cursor_item()?;
        if self.items[cur].is_dir {
            return None;
        }
        let mut label = self.stop_label(cfg, Some(cur));
        if let Some((i, Dock::Ready(Some(v), ..))) = &self.dock {
            if *i == cur {
                let (pos, total) = v.diff_position();
                let n = pos.map_or("–".to_string(), |p| (p + 1).to_string());
                label.push_str(" · ");
                label.push_str(&trf(
                    "nav.diff_of",
                    &[("n", n), ("total", total.to_string())],
                ));
            }
        }
        Some(label)
    }

    /// The item under the cursor.
    fn cursor_item(&self) -> Option<usize> {
        self.cursor
            .or(self.anchor)
            .and_then(|v| self.visible.get(v))
            .copied()
    }

    /// The docked file compare, when it shows the item under the cursor as text.
    fn docked_text(&mut self) -> Option<&mut TextView> {
        let cur = self.cursor_item();
        match &mut self.dock {
            Some((i, Dock::Ready(Some(v), ..))) if Some(*i) == cur => Some(v),
            _ => None,
        }
    }

    /// The item under the cursor, while the docked file compare is still loading it as text.
    fn docking(&self, cfg: &Config) -> Option<usize> {
        let cur = self.cursor_item()?;
        let ready = matches!(self.dock, Some((i, Dock::Ready(..) | Dock::Failed(_))) if i == cur);
        (cfg.folder_file_pane && !ready && may_dock(&self.items[cur])).then_some(cur)
    }

    /// Next or previous difference: in the docked file compare, then on to the adjacent file
    /// with differences, which the docked compare opens at its first (or last) difference.
    pub fn step_diff(&mut self, forward: bool, cfg: &Config) {
        self.dock_forward = forward;
        self.dock_conflicts = false;
        // Moving on before the file shows would skip its differences: the step waits for it.
        if let Some(cur) = self.docking(cfg) {
            let step = if forward { 1 } else { -1 };
            match &mut self.dock_land {
                Some(land) if land.item == cur && !land.conflicts => land.steps += step,
                land => *land = Some(Land::new(cur, forward)),
            }
            return;
        }
        if matches!(&self.dock, Some((i, Dock::Failed(_))) if Some(*i) == self.cursor_item()) {
            self.step_file(forward, cfg);
            return;
        }
        let moved = self.docked_text().is_some_and(|v| {
            if forward {
                v.next_diff(false)
            } else {
                v.prev_diff(false)
            }
        });
        if !moved {
            self.step_file(forward, cfg);
        }
    }

    /// First or last difference of the docked file compare, or else the first or last file
    /// with differences.
    pub fn edge_diff(&mut self, first: bool, cfg: &Config) {
        if let Some(cur) = self.docking(cfg) {
            self.dock_land = Some(Land::new(cur, first));
        } else if let Some(v) = self.docked_text() {
            if first {
                v.first_diff();
            } else {
                v.last_diff();
            }
        } else if let Some(i) = self.edge_different_file(cfg, first) {
            self.step_to(i, first, cfg);
        } else if self.review_filtering() {
            self.notice = Some("msg.no_listed_different_files");
        } else if self.running.is_some() || self.stopped {
            self.notice = Some(self.no_more_files(first, false));
        } else {
            self.notice = Some("msg.no_different_files");
        }
    }

    /// What to say when there is no file to step to: the compare may still list more, or
    /// may have been stopped before it listed them all.
    pub fn no_more_files(&self, forward: bool, conflicts: bool) -> &'static str {
        if self.running.is_some() {
            "msg.no_more_files_yet"
        } else if self.stopped {
            "msg.no_more_files_stopped"
        } else {
            match (forward, conflicts) {
                (true, false) => "msg.no_next_file",
                (false, false) => "msg.no_prev_file",
                (true, true) => "msg.no_next_conflict_file",
                (false, true) => "msg.no_prev_conflict_file",
            }
        }
    }

    /// Next or previous open conflict: in the docked file compare, then on through the files
    /// both sides changed, passing over those without open conflicts once they are loaded.
    pub fn step_conflict(&mut self, forward: bool, cfg: &Config) {
        self.dock_forward = forward;
        self.dock_conflicts = true;
        if let Some(cur) = self.docking(cfg) {
            let step = if forward { 1 } else { -1 };
            match &mut self.dock_land {
                Some(land) if land.item == cur && land.conflicts => land.steps += step,
                land => *land = Some(Land::conflict(cur, forward)),
            }
            return;
        }
        if matches!(&self.dock, Some((i, Dock::Failed(_))) if Some(*i) == self.cursor_item()) {
            self.step_conflict_file(forward, cfg);
            return;
        }
        let moved = self.docked_text().is_some_and(|v| {
            if forward {
                v.next_diff(true)
            } else {
                v.prev_diff(true)
            }
        });
        if !moved {
            self.step_conflict_file(forward, cfg);
        }
    }

    fn step_conflict_file(&mut self, forward: bool, cfg: &Config) {
        self.dock_forward = forward;
        self.dock_conflicts = true;
        let next = match self.cursor_item() {
            Some(i) => self.adjacent_conflict_file(cfg, &self.items[i].rel_path(), forward),
            None => {
                let stops = &self.stops(cfg).files;
                let mut open = stops.iter().copied().filter(|&i| self.may_conflict(i));
                if forward {
                    open.next()
                } else {
                    open.last()
                }
            }
        };
        if let Some(i) = next {
            if self.reveal(i, cfg) && self.docking(cfg) == Some(i) {
                self.dock_land = Some(Land::conflict(i, forward));
            }
        } else {
            self.notice = Some(self.no_more_files(forward, true));
        }
    }

    fn step_file(&mut self, forward: bool, cfg: &Config) {
        self.dock_forward = forward;
        self.dock_conflicts = false;
        let next = match self.cursor_item() {
            Some(i) => self.adjacent_different_file(cfg, &self.items[i].rel_path(), forward),
            None => self.edge_different_file(cfg, forward),
        };
        if let Some(i) = next {
            self.step_to(i, forward, cfg);
        } else {
            self.notice = Some(self.no_more_files(forward, false));
        }
    }

    /// Selects file `i`, which the docked file compare then opens at its first (or last)
    /// difference.
    fn step_to(&mut self, i: usize, first: bool, cfg: &Config) {
        if self.reveal(i, cfg) && self.docking(cfg) == Some(i) {
            self.dock_land = Some(Land::new(i, first));
        }
    }

    /// Selects item `i`, expanding its folders and scrolling to it, and returns the action that
    /// opens it.
    pub fn reveal_and_open(&mut self, i: usize, cfg: &Config) -> Option<FolderAction> {
        self.reveal(i, cfg);
        self.open_action(i, None)
    }

    /// Selects file `rel` without opening it, as a tab of all differences moves to it.
    pub fn select_rel(&mut self, rel: &str, cfg: &Config) {
        if self
            .cursor_item()
            .is_some_and(|i| self.items[i].rel_path() == rel)
        {
            return;
        }
        if let Some(i) = self.items.iter().position(|it| it.rel_path() == rel) {
            self.reveal(i, cfg);
        }
    }

    /// The file under the cursor and the difference its docked compare shows.
    pub fn cursor_position(&self) -> Option<(String, Option<usize>)> {
        let cur = self.cursor_item()?;
        let at = match &self.dock {
            Some((i, Dock::Ready(Some(v), ..))) if *i == cur => v.diff_position().0,
            _ => None,
        };
        Some((self.items[cur].rel_path(), at))
    }

    /// Selects item `i`, expanding its folders and scrolling to it; whether it is listed.
    fn reveal(&mut self, i: usize, cfg: &Config) -> bool {
        let mut p = self.items[i].parent;
        let mut expanded = false;
        while let Some(pi) = p {
            expanded |= self.expanded.insert(self.items[pi].rel_path());
            p = self.items[pi].parent;
        }
        if expanded || !self.visible.contains(&i) {
            // the files stopped at are in order as if every folder were expanded
            let kept = self.stops.take().filter(|(k, _)| k.0 == self.list_gen);
            self.rebuild_visible(cfg);
            if let Some((k, stops)) = kept {
                *self.stops.borrow_mut() = Some(((self.list_gen, k.1, k.2, k.3, k.4), stops));
            }
        }
        let Some(v) = self.visible.iter().position(|&x| x == i) else {
            return false;
        };
        self.selected = BTreeSet::from([i]);
        self.anchor = Some(v);
        self.cursor = Some(v);
        self.scroll_to = Some(v);
        true
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

    /// Whether files can be copied to or deleted from side `s` now.
    fn writable(&self, s: usize) -> bool {
        !self.read_only.get(s).copied().unwrap_or(false) && self.file_job.is_none()
    }

    pub fn request_op_copy(&mut self, from: usize, to: usize, cfg: &Config) {
        if !self.writable(to) {
            return;
        }
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
            OpKind::Delete { side } => {
                if side.map_or(self.file_job.is_some(), |s| !self.writable(s)) {
                    return;
                }
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
        self.dock_land = None;
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
                    let sides = sides.into_iter().filter(|&s| !self.read_only[s]);
                    sides.filter(|&s| it.exists[s]).try_for_each(|s| {
                        folder::delete_path(&it.path(&self.roots[s]), cfg.use_recycle_bin)
                    })
                }
            };
            match r {
                Ok(()) => done += 1,
                Err(e) => errors.push(format!("{}: {e}", it.rel_path())),
            }
        }
        if done > 0 {
            let touched: Vec<usize> = match op.kind {
                OpKind::Copy { to, .. } => vec![to],
                OpKind::Delete { side: Some(s) } => vec![s],
                OpKind::Delete { side: None } => (0..self.n()).collect(),
            };
            for s in touched {
                self.mark_written(s);
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
        self.list_gen += 1;
    }

    fn open_action(&self, i: usize, kind: Option<FileKind>) -> Option<FolderAction> {
        let it = &self.items[i];
        let n = self.n();
        let paths: Vec<PathBuf> = self.roots.iter().map(|r| it.path(r)).collect();
        if it.is_dir {
            if self.snapshot_options.is_some() {
                return None;
            }
            if it.exists[..n].iter().all(|&e| e) {
                return Some(FolderAction::OpenFolders { paths });
            }
            return None;
        }
        let missing: Vec<bool> = (0..n).map(|s| !it.exists[s]).collect();
        let rel = it.rel_path();
        let descriptions = (0..n)
            .map(|s| {
                let root = self.display_roots.get(s).filter(|_| self.read_only[s])?;
                Some(format!("{root}/{rel}"))
            })
            .collect();
        Some(FolderAction::OpenFiles {
            paths,
            missing,
            read_only: self.read_only.clone(),
            descriptions,
            kind,
            rel: it.rel_path(),
            rules: self.rules.clone(),
            snapshot_options: self.snapshot_options.clone().map(Box::new),
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
                    crate::core::unpacker::load_compare_doc(&paths[0], &self.dopts, None),
                    crate::core::unpacker::load_compare_doc(&paths[1], &self.dopts, None),
                ) {
                    let rel = it.rel_path();
                    let input = patch::PatchInput {
                        name_a: &format!("a/{rel}"),
                        name_b: &format!("b/{rel}"),
                        date_a: "",
                        date_b: "",
                        a: &a.lines,
                        b: &b.lines,
                        final_newline_a: a.final_newline,
                        final_newline_b: b.final_newline,
                    };
                    let p = match patch::make_doc_patch(
                        &input,
                        &[&a, &b],
                        &self.dopts,
                        PatchStyle::Unified,
                        3,
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            out.push((PeekKind::Meta, e));
                            return out;
                        }
                    };
                    if p.is_empty() {
                        out.push((PeekKind::Plain, tr("msg.identical").into()));
                    }
                    push_text(&mut out, &p);
                    return out;
                }
            } else if let [s] = present[..] {
                if let Ok(doc) =
                    crate::core::unpacker::load_compare_doc(&paths[s], &self.dopts, None)
                {
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
        let screen = ctx.content_rect();
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
                        .size(theme::size::SMALL)
                        .color(pal.text_dim),
                );
                ui.separator();
                let font = FontId::monospace(theme::size::SMALL);
                let row_h = ui.fonts_mut(|f| f.row_height(&font));
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

    /// Copies the selected items of side `side` into `dest` on another thread, keeping their
    /// relative paths.
    pub fn copy_to_folder(&mut self, side: usize, dest: &Path) {
        if self.file_job.is_some() {
            return;
        }
        let jobs: Vec<(String, PathBuf)> = self
            .selected_targets()
            .into_iter()
            .filter(|&i| self.items[i].exists[side])
            .map(|i| {
                (
                    self.items[i].rel_path(),
                    self.items[i].path(&self.roots[side]),
                )
            })
            .collect();
        let dest = dest.to_path_buf();
        self.status = tr("msg.copying").to_string();
        self.file_job = Some((
            std::thread::spawn(move || {
                let mut done = 0;
                let mut errors = Vec::new();
                for (rel, from) in jobs {
                    match folder::copy_recursive(&from, &dest.join(&rel)) {
                        Ok(()) => done += 1,
                        Err(e) => errors.push(format!("{rel}: {e}")),
                    }
                }
                (done, errors)
            }),
            false,
        ));
    }

    /// Takes the result of a finished copy or merge. Returns whether one is still running.
    fn poll_file_job(&mut self) -> bool {
        if !self.file_job.as_ref().is_some_and(|(j, _)| j.is_finished()) {
            return self.file_job.is_some();
        }
        let Some((job, rescan)) = self.file_job.take() else {
            return false;
        };
        self.merge_stop = None;
        let (done, errors) = job
            .join()
            .unwrap_or_else(|_| (0, vec![tr("status.error").to_string()]));
        if rescan {
            if done > 0 {
                self.mark_archives_dirty();
            }
            self.start_compare();
        }
        self.status = trf("msg.op_done", &[("n", done.to_string())]);
        if !errors.is_empty() {
            self.status
                .push_str(&format!(" — {}: {}", tr("status.error"), errors.join("; ")));
        }
        false
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
        self.list_gen += 1;
        let first = self.visible.iter().position(|i| self.selected.contains(i));
        self.anchor = first;
        self.cursor = first;
        self.scroll_to = first;
        // The status line counts a selection of more than one item by itself.
        self.status = if self.selected.len() > 1 {
            String::new()
        } else {
            format!("{} {}", self.selected.len(), tr("label.selected"))
        };
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
        if self.n() != 3 || self.running.is_some() || !self.writable(1) {
            return;
        }
        if self.merge_job.is_some() {
            return;
        }
        // Folders not listed would be merged as empty ones, and their files left out.
        if self.stopped || !self.whole_tree {
            self.status = tr("msg.folder_merge_incomplete").to_string();
            return;
        }
        let (mut items, roots) = (self.items.clone(), self.roots.clone());
        for item in &mut items {
            if crate::core::unpacker::find(&self.dopts, &item.path(&roots[0])).is_some() {
                item.binary = true;
            }
        }
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
            Some(Ok(plan)) if !plan.stale.is_empty() => {
                self.status = trf(
                    "msg.folder_merge_stale",
                    &[
                        ("n", plan.stale.len().to_string()),
                        ("file", self.items[plan.stale[0]].rel_path()),
                    ],
                );
            }
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

    pub fn handles_escape(&self) -> bool {
        self.has_dialog() || self.peek || self.note_edit.is_some()
    }

    /// Whether one of the view's dialogs is open, so the app leaves the keyboard to it.
    pub fn has_dialog(&self) -> bool {
        self.confirm.is_some()
            || self.merge_plan.is_some()
            || self.select_pattern.is_some()
            || self.sync.is_some()
    }

    /// Whether a synchronization is changing files now, which quitting would cut short.
    pub fn syncing(&self) -> bool {
        self.sync.as_ref().is_some_and(|d| d.busy())
    }

    /// Whether a synchronization is being planned or applied, so its dialog is not settled yet.
    pub fn sync_settling(&self) -> bool {
        self.sync.as_ref().is_some_and(|d| d.planning() || d.busy())
    }

    /// Whether copying or deleting files is still going on.
    pub fn has_file_job(&self) -> bool {
        self.file_job.is_some()
    }

    /// Opens the preview of synchronizing the two folders, which changes nothing until asked.
    pub fn request_sync(&mut self) {
        if self.n() != 2 || self.running.is_some() || self.file_job.is_some() {
            return;
        }
        if self.stopped || !self.whole_tree {
            self.status = tr("folder_sync.err_incomplete").to_string();
            return;
        }
        let scope = folder_sync::Scope {
            complete: true,
            excluded: self.filter.excludes_anything() || self.fopts.skip_hidden,
            filter: self.filter_spec.clone(),
            skip_hidden: self.fopts.skip_hidden,
            copy_links: false,
        };
        let writable = [self.writable(0), self.writable(1)];
        self.sync = Some(Box::new(SyncDialog::new(
            self.items.clone(),
            self.roots.clone(),
            self.display_roots.clone(),
            scope,
            writable,
            self.id,
        )));
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
            let list = |ui: &mut Ui, items: Vec<usize>| {
                egui::Frame::new()
                    .fill(pal.surface_alt)
                    .corner_radius(theme::radius::MD)
                    .inner_margin(Margin::same(theme::space::CONTROL))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for &i in items.iter().take(10) {
                            ui.label(
                                egui::RichText::new(self.items[i].rel_path())
                                    .monospace()
                                    .color(pal.text_dim),
                            );
                        }
                        if items.len() > 10 {
                            ui.label("…");
                        }
                    });
            };
            if c.deletes > 0 {
                ui.add_space(4.0);
                ui.label(tr("msg.folder_merge_deletes"));
                list(
                    ui,
                    plan.items_where(|s| matches!(s, Step::Delete | Step::Prune)),
                );
            }
            if c.conflicts > 0 {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(trf(
                        "msg.folder_merge_conflicts",
                        &[("n", c.conflicts.to_string())],
                    ))
                    .color(pal.warning),
                );
                list(ui, plan.items_where(|s| matches!(s, Step::Conflict)));
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
            let (items, roots) = (self.items.clone(), self.roots.clone());
            let recycle = cfg.use_recycle_bin;
            let backup = crate::core::fsutil::backup_on();
            let stop = Arc::new(AtomicBool::new(false));
            self.merge_stop = Some(stop.clone());
            self.status = tr("msg.folder_merge_applying").to_string();
            self.file_job = Some((
                std::thread::spawn(move || {
                    let stop = || stop.load(Ordering::Relaxed);
                    folder_merge::apply_until(plan, &items, &roots, recycle, backup, &stop)
                }),
                true,
            ));
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

    #[cfg(test)]
    fn dock_view(&self, i: usize, cfg: &Config) -> Option<Box<TextView>> {
        load_dock_view(&self.items[i], &self.roots, cfg, &self.dopts).unwrap()
    }

    /// The item the docked file compare shows once loaded, and its current difference.
    #[cfg(test)]
    pub fn docked(&self) -> Option<(String, Option<usize>)> {
        match &self.dock {
            Some((i, Dock::Ready(Some(v), ..))) => {
                Some((self.items[*i].rel_path(), v.diff_position().0))
            }
            _ => None,
        }
    }

    /// Moves the docked file compare along: it waits for the selection to settle, loads the
    /// files in the background, then shows them.
    fn update_dock(&mut self, ctx: &egui::Context, cfg: &Config) {
        let cur = self.cursor_item();
        if cur != self.dock.as_ref().map(|d| d.0) {
            self.dock = cur.map(|i| (i, Dock::Waiting(Instant::now())));
            self.dock_land = self.dock_land.filter(|l| Some(l.item) == cur);
        }
        if let Some((i, Dock::Ready(view, stamp, checked))) = &mut self.dock {
            if checked.elapsed() >= DOCK_RECHECK {
                *checked = Instant::now();
                if mtimes(&self.items[*i], &self.roots) != *stamp {
                    // Loaded again, it shows the same difference as before.
                    let at = view.as_ref().and_then(|v| v.diff_position().0);
                    if let Some(n) = at {
                        self.dock_land.get_or_insert(Land {
                            item: *i,
                            at: LandAt::Nth(n),
                            steps: 0,
                            conflicts: false,
                        });
                    }
                    let since = Instant::now().checked_sub(DOCK_DELAY);
                    self.dock = Some((*i, Dock::Waiting(since.unwrap_or_else(Instant::now))));
                }
            }
        }
        let Some((i, state)) = &self.dock else {
            return;
        };
        let i = *i;
        let mut step_on = None;
        let next = match state {
            // A file stepped to is wanted at once.
            Dock::Waiting(since)
                if since.elapsed() >= DOCK_DELAY || self.dock_land.is_some_and(|l| l.item == i) =>
            {
                let (it, roots) = (self.items[i].clone(), self.roots.clone());
                let (cfg, dopts) = (cfg.clone(), self.dopts.clone());
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(load_dock_view(&it, &roots, &cfg, &dopts));
                });
                Some(Dock::Loading(rx))
            }
            Dock::Loading(rx) => match rx.try_recv() {
                Ok(Ok(mut view)) => {
                    let land = self.dock_land.take().filter(|l| l.item == i);
                    if let Some(land) = land {
                        let first = !matches!(land.at, LandAt::Last);
                        // passed over, the file leaves every step asked for to the next one
                        match (&mut view, land.at) {
                            (Some(v), LandAt::First | LandAt::Last) if land.conflicts => {
                                if !v.edge_conflict(first) {
                                    step_on = Some((first, true, land.steps));
                                }
                            }
                            (Some(v), LandAt::First) => v.first_diff(),
                            (Some(v), LandAt::Last) => v.last_diff(),
                            (Some(v), LandAt::Nth(n)) => v.go_to_diff(n),
                            // Not shown as text, so it has no conflicts to stop at.
                            (None, _) if land.conflicts => {
                                step_on = Some((first, true, land.steps));
                            }
                            (None, _) => {}
                        }
                        if let (Some(v), None) = (&mut view, step_on) {
                            let (n, sign) = (land.steps.abs(), land.steps.signum());
                            for taken in 0..n {
                                let forward = sign > 0;
                                let moved = if forward {
                                    v.next_diff(land.conflicts)
                                } else {
                                    v.prev_diff(land.conflicts)
                                };
                                if !moved {
                                    // this step lands in the next file; the rest go on from there
                                    let rest = (n - taken - 1) * sign;
                                    step_on = Some((forward, land.conflicts, rest));
                                    break;
                                }
                            }
                        }
                    }
                    Some(Dock::Ready(
                        view,
                        mtimes(&self.items[i], &self.roots),
                        Instant::now(),
                    ))
                }
                Ok(Err(error)) => {
                    self.dock_land = None;
                    Some(Dock::Failed(error))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.dock_land = None;
                    Some(Dock::Failed(tr("dock.load_failed").to_string()))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            },
            _ => None,
        };
        if let Some(state) = next {
            self.dock = Some((i, state));
        }
        // Steps asked for while the file loaded went past its differences or conflicts.
        if let Some((forward, conflicts, rest)) = step_on {
            if conflicts {
                self.step_conflict_file(forward, cfg);
            } else {
                self.step_file(forward, cfg);
            }
            if let Some(land) = self.dock_land.as_mut() {
                land.steps += rest;
            }
        }
        if !matches!(self.dock, Some((_, Dock::Ready(..) | Dock::Failed(_)))) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn dock_ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.update_dock(ui.ctx(), cfg);
        let avail = ui.available_height();
        let rules = self.used_rules().to_string();
        let mut step = None;
        let mut conflict_step = None;
        let mut skip = false;
        let mut retry = false;
        egui::Panel::bottom(self.id.with("dock"))
            .resizable(true)
            .default_size(avail * 0.45)
            .size_range(100.0..=(avail - 120.0).max(100.0))
            .frame(egui::Frame::new())
            .show(ui, |ui| match &mut self.dock {
                Some((_, Dock::Ready(Some(v), ..))) => {
                    v.set_options(&self.dopts);
                    v.ui(ui, cfg, c);
                    step = v.step_file.take();
                    conflict_step = v.step_conflict.take();
                    if let Some(r) = v.rule_request.take() {
                        self.rule_request = Some(r);
                    }
                    if std::mem::take(&mut v.export_request) {
                        let name = report::html_file_name(&v.titles());
                        self.file_report = Some((name, v.html_report_job(&rules)));
                    }
                }
                Some((i, Dock::Failed(error))) => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() / 3.0);
                        ui.label(self.items[*i].rel_path());
                        ui.colored_label(c.ui.warning, error.as_str());
                        ui.horizontal(|ui| {
                            retry = ui.button(tr("btn.retry")).clicked();
                            skip = ui.button(tr("btn.skip")).clicked();
                        });
                    });
                }
                other => {
                    let key = match other {
                        Some((i, Dock::Ready(None, ..))) if self.items[*i].is_dir => "peek.folder",
                        Some((_, Dock::Ready(None, ..))) => "dock.not_text",
                        Some(_) => "msg.comparing",
                        None => "dock.hint",
                    };
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new(tr(key)).color(widgets::pal(ui).text_dim));
                    });
                }
            });
        if retry {
            if let Some((i, _)) = self.dock {
                let since = Instant::now().checked_sub(DOCK_DELAY);
                self.dock = Some((i, Dock::Waiting(since.unwrap_or_else(Instant::now))));
            }
        }
        if skip && self.dock_conflicts {
            self.step_conflict_file(self.dock_forward, cfg);
        } else if skip {
            self.step_file(self.dock_forward, cfg);
        }
        if let Some(forward) = step {
            self.step_file(forward, cfg);
        }
        if let Some(forward) = conflict_step {
            self.step_conflict_file(forward, cfg);
        }
    }

    pub fn copy_selected(&mut self, to_right: bool, cfg: &Config) {
        let n = self.n();
        let (from, to) = if to_right { (0, 1) } else { (n - 1, n - 2) };
        self.request_op_copy(from, to, cfg);
    }

    /// Patch of all differing/unique text files.
    pub fn make_patch(&self, style: PatchStyle, context: usize) -> Result<String, String> {
        if self.n() != 2 {
            return Ok(String::new());
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
        self.report_notes(
            report::generate(&items, &roots, format, self.used_rules()),
            format,
        )
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
        let rules = self.used_rules().to_string();
        let notes = self.notes.clone();
        let review_summary = (!self.workspace.entries.is_empty()).then(|| {
            self.workspace
                .summary(items.iter().filter(|i| !i.is_dir).map(|i| i.rel_path()))
        });
        let temps = self.temps.clone();
        move || {
            let _keep_alive = temps;
            let refs: Vec<&DirItem> = items.iter().collect();
            let content = crate::core::review::annotate_report(
                report::folder_html_with_diffs(&refs, &roots, &display, &dopts, &rules),
                ReportFormat::Html,
                &notes,
            );
            match review_summary {
                Some(summary) => crate::core::review_state::annotate_report(
                    content,
                    ReportFormat::Html,
                    &summary,
                ),
                None => content,
            }
        }
    }

    /// Makes the PDF report of the listed items with the differences of every changed text
    /// file, review notes and the review summary, as the HTML report with differences; it reads
    /// every changed file, so it runs on another thread.
    pub fn pdf_job(
        &self,
        cfg: &Config,
        pdf: crate::core::pdf_report::PdfOptions,
        lang: String,
    ) -> impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static {
        use crate::core::pdf_report::Section;
        let items: Vec<DirItem> = self
            .flat_list(cfg)
            .into_iter()
            .map(|i| self.items[i].clone())
            .collect();
        let display: Vec<PathBuf> = self.display_roots.iter().map(PathBuf::from).collect();
        let (roots, dopts) = (self.roots.clone(), self.dopts.clone());
        let rules = self.used_rules().to_string();
        let mut sections = Vec::new();
        if !self.notes.is_empty() {
            sections.push(Section {
                title: crate::i18n::tr("review.notes").to_string(),
                lines: self
                    .notes
                    .iter()
                    .map(|(path, note)| format!("{path}: {note}"))
                    .collect(),
                mono: false,
            });
        }
        if !self.workspace.entries.is_empty() {
            let summary = self
                .workspace
                .summary(items.iter().filter(|i| !i.is_dir).map(|i| i.rel_path()));
            let json = serde_json::to_string_pretty(&summary).unwrap_or_default();
            sections.push(Section {
                title: tr("workspace.title").into(),
                lines: json.lines().map(str::to_owned).collect(),
                mono: true,
            });
        }
        let temps = self.temps.clone();
        move || {
            let _keep_alive = temps;
            let refs: Vec<&DirItem> = items.iter().collect();
            let fonts = crate::ui::fonts::document_fonts(&lang);
            crate::core::pdf_report::folder_pdf(
                &refs,
                &roots,
                &display,
                Some(&dopts),
                &rules,
                &sections,
                &fonts,
                &pdf,
            )
        }
    }

    /// Makes the HTML slideshow of the listed items: a slide for each difference of every
    /// changed text file. It reads every changed file, so it can run on another thread.
    pub fn slideshow_job(&self, cfg: &Config) -> impl FnOnce() -> String + Send + 'static {
        let items: Vec<DirItem> = self
            .flat_list(cfg)
            .into_iter()
            .map(|i| self.items[i].clone())
            .collect();
        let display: Vec<PathBuf> = self.display_roots.iter().map(PathBuf::from).collect();
        let (roots, dopts) = (self.roots.clone(), self.dopts.clone());
        let rules = self.used_rules().to_string();
        let notes = self.notes.clone();
        let review_summary = (!self.workspace.entries.is_empty()).then(|| {
            self.workspace
                .summary(items.iter().filter(|i| !i.is_dir).map(|i| i.rel_path()))
        });
        let temps = self.temps.clone();
        move || {
            let _keep_alive = temps;
            let refs: Vec<&DirItem> = items.iter().collect();
            let content = crate::core::review::annotate_report(
                report::folder_slideshow(&refs, &roots, &display, &dopts, &rules),
                ReportFormat::Html,
                &notes,
            );
            match review_summary {
                Some(summary) => crate::core::review_state::annotate_report(
                    content,
                    ReportFormat::Html,
                    &summary,
                ),
                None => content,
            }
        }
    }

    /// What the tab of all the differences shows: the changed files listed, in display order.
    pub fn diffs_source(&self, cfg: &Config) -> crate::ui::diffs_view::Source {
        crate::ui::diffs_view::Source {
            roots: self.roots.clone(),
            display_roots: self.display_roots.clone(),
            read_only: self.read_only.clone(),
            dopts: self.dopts.clone(),
            snapshot_options: self.snapshot_options.clone(),
            rules: self.rules.clone(),
            title: self.title(),
            items: self
                .display_order(cfg, true)
                .into_iter()
                .map(|i| self.items[i].clone())
                .collect(),
            stopped: self.stopped,
            temps: self.temps.clone(),
        }
    }

    pub fn outcome(&self) -> Outcome {
        if self.running.is_some() || self.file_job.is_some() {
            return Outcome::Busy;
        }
        let (_, diff, uniq, err) = folder::status_counts(&self.items);
        if err > 0 || self.failed {
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

    /// The status line for the status bar: the chips above the list show the counts.
    pub fn status_detail(&self) -> String {
        if self.running.is_some() {
            return match &self.nav_text {
                Some(nav) => format!("{nav}  |  {}", self.status_line()),
                None => self.status_line(),
            };
        }
        let mut parts: Vec<String> = self.nav_text.iter().cloned().collect();
        if let Some(e) = self.elapsed {
            let secs = format!("{:.2}", e.as_secs_f32());
            parts.push(trf("folder.compared_in", &[("s", secs)]));
        }
        if self.selected.len() > 1 {
            parts.push(format!("{} {}", self.selected.len(), tr("label.selected")));
        }
        if !self.status.is_empty() {
            parts.push(self.status.clone());
        }
        if parts.is_empty() {
            return self.status_line();
        }
        parts.join("  |  ")
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
        self.list_gen += 1;
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
        if self.snapshot_options.is_some() {
            widgets::toolbar(ui, |ui| {
                ui.label(tr("review.snapshot"));
                ui.label(format!("{}: {}", tr("label.filter"), self.filter_spec));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    widgets::drawer_toggle(ui, &mut self.drawer_open);
                });
            });
            ui.horizontal_wrapped(|ui| {
                for label in &self.display_roots {
                    ui.label(label);
                }
            });
            return (false, None);
        }
        let mut refresh = false;
        let mut action = None;
        let mut new_filter: Option<String> = None;
        let mut new_options: Option<(folder::CompareMethod, bool)> = None;
        let mut new_rules: Option<String> = None;
        let n = self.n();
        let sel = !self.selected.is_empty();
        widgets::toolbar(ui, |ui| {
            let key = widgets::with_key;
            // Icons and shorter chips once the full toolbar, as last measured, does not fit:
            // long filter or rule names and longer translations need more room.
            // Measured again whenever the labels change, shorter or longer.
            let start_x = ui.cursor().min.x;
            let need_id = self.id.with("toolbar_width");
            let labels = egui::Id::unique((
                &self.filter_spec,
                &self.rules,
                self.fopts.method.key(),
                crate::i18n::current_language(),
                cfg.tree_mode,
                theme::look().compact,
            ));
            let measured = |ui: &Ui, id: egui::Id| {
                ui.data(|d| d.get_temp::<(egui::Id, f32)>(id))
                    .filter(|&(l, _)| l == labels)
                    .map_or(0.0, |(_, w)| w)
            };
            let need = measured(ui, need_id);
            let compact = ui.available_width() < need.max(1180.0);
            // Where even icons and short chips do not fit, the chips take a row of their own.
            let compact_id = self.id.with("toolbar_compact_width");
            let wrap = compact && ui.available_width() < measured(ui, compact_id);
            let labelled = |ui: &mut Ui, icon: Icon, label: &str| {
                if compact {
                    widgets::icon_button(ui, icon, label)
                } else {
                    widgets::ghost_icon_button(ui, icon, label)
                }
            };
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
            let mut pane = cfg.folder_file_pane;
            let tip = Cmd::ToggleFilePane.tip();
            if widgets::toggle_icon_button(ui, &mut pane, Some(Icon::BottomPane), "", &tip)
                .clicked()
            {
                action = Some(FolderAction::FilePane(pane));
            }
            widgets::toolbar_sep(ui);
            if n == 2 {
                if widgets::icon_button_enabled(
                    ui,
                    sel && self.writable(1),
                    Icon::CopyRight,
                    &Cmd::CopyRight.tip(),
                )
                .clicked()
                {
                    self.request_op_copy(0, 1, cfg);
                }
                if widgets::icon_button_enabled(
                    ui,
                    sel && self.writable(0),
                    Icon::CopyLeft,
                    &Cmd::CopyLeft.tip(),
                )
                .clicked()
                {
                    self.request_op_copy(1, 0, cfg);
                }
                let idle = self.running.is_none() && self.file_job.is_none();
                if ui
                    .add_enabled_ui(idle, |ui| {
                        labelled(ui, Icon::CopyAllRight, tr("cmd.sync_folders"))
                            .on_hover_text(tr("folder_sync.tip"))
                    })
                    .inner
                    .clicked()
                {
                    self.request_sync();
                }
            } else {
                let sel = sel && self.writable(1);
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
                    self.running.is_none() && self.merge_job.is_none() && self.writable(1),
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
            widgets::toolbar_sep(ui);
            let ready = self.running.is_none();
            if ui
                .add_enabled_ui(ready, |ui| {
                    labelled(ui, Icon::Compare, tr("cmd.all_diffs"))
                        .on_hover_text(tr("diffs.tip"))
                        .on_disabled_hover_text(tr("msg.comparing"))
                })
                .inner
                .clicked()
            {
                action = Some(FolderAction::AllDiffs);
            }
            if labelled(ui, Icon::Export, tr("btn.export_html"))
                .on_hover_text(Cmd::HtmlReport.tip())
                .clicked()
            {
                action = Some(FolderAction::HtmlReport);
            }
            let left_end = ui.cursor().min.x + 4.0;
            if wrap {
                ui.end_row();
            }
            let right = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if !wrap {
                    let mut clip = ui.clip_rect();
                    clip.min.x = clip.min.x.max(left_end);
                    ui.set_clip_rect(clip);
                }
                let close = egui::PopupCloseBehavior::CloseOnClickOutside;
                widgets::drawer_toggle(ui, &mut self.drawer_open);

                let search = widgets::search_edit(
                    ui,
                    &mut self.search,
                    &key(tr("folder.search_hint"), Cmd::Find.key()),
                    if compact { 120.0 } else { 170.0 },
                );
                if std::mem::take(&mut self.focus_search) {
                    search.request_focus();
                }
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.search.clear();
                    self.view_key = None;
                    self.list_gen += 1;
                }
                if search.changed() {
                    self.view_key = None;
                    self.list_gen += 1;
                }

                let method_id = self.id.with("method_popup");
                let r = widgets::chip_button(
                    ui,
                    Icon::Method,
                    tr("label.method"),
                    tr(self.fopts.method.key()),
                    !compact,
                )
                .on_hover_text(tr("tip.click_change"));
                popup_below(&r, method_id, close).show(|ui| {
                    widgets::menu_setup(ui);
                    for m in folder::CompareMethod::ALL {
                        let on = self.fopts.method == m;
                        if widgets::menu_check(ui, on, tr(m.key()), "").clicked() && !on {
                            new_options = Some((m, self.fopts.recursive));
                        }
                    }
                    ui.separator();
                    let recursive = self.fopts.recursive;
                    if widgets::menu_check(ui, recursive, tr("label.recursive"), "").clicked() {
                        new_options = Some((self.fopts.method, !recursive));
                    }
                });

                let rules_id = self.id.with("rules_popup");
                let shown = if self.rules.is_empty() {
                    tr("rules.none")
                } else {
                    rules::display_name(&self.rules)
                };
                let r =
                    widgets::chip_button(ui, Icon::Settings, tr("label.rules"), shown, !compact)
                        .on_hover_text(tr("tip.rules"));
                if r.clicked() {
                    self.rule_names = rules::list().into_iter().map(|s| s.name).collect();
                }
                popup_below(&r, rules_id, close).show(|ui| {
                    widgets::menu_setup(ui);
                    if widgets::menu_check(ui, self.rules.is_empty(), tr("rules.none"), "")
                        .clicked()
                    {
                        new_rules = Some(String::new());
                    }
                    let refs: Vec<&str> = self.rule_names.iter().map(String::as_str).collect();
                    let picked = rules::chosen(&self.rules, &refs);
                    for name in &self.rule_names {
                        let on = picked == Some(name.as_str());
                        if widgets::menu_check(ui, on, name, "").clicked() {
                            new_rules = Some(name.clone());
                        }
                    }
                    ui.separator();
                    if ui.button(tr("cmd.edit_rules")).clicked() {
                        ui.close();
                        action = Some(FolderAction::EditRules);
                    }
                });

                let filter_id = self.id.with("filter_popup");
                let r = widgets::chip_button(
                    ui,
                    Icon::Filter,
                    tr("label.filter"),
                    &self.filter_spec,
                    !compact,
                )
                .on_hover_text(tr("tip.click_change"));
                let opened = r.clicked();
                if opened {
                    self.filter_edit = Some(self.filter_spec.clone());
                }
                popup_below(&r, filter_id, close).show(|ui| {
                    ui.set_min_width(widgets::menu_width(ui.ctx()).max(300.0));
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
            if !wrap {
                let width = left_end - start_x + right.response.rect.width() + 16.0;
                let id = if compact { compact_id } else { need_id };
                if (measured(ui, id) - width).abs() > 0.5 {
                    ui.data_mut(|d| d.insert_temp(id, (labels, width)));
                    ui.ctx().request_repaint();
                }
            }
        });
        if let Some((method, recursive)) = new_options {
            if method != self.fopts.method {
                egui::Popup::close_all(ui.ctx());
            }
            self.set_method(method);
            self.set_recursive(recursive);
            self.start_compare();
            action = Some(FolderAction::OptionsChanged { method, recursive });
        }
        if let Some(name) = new_rules {
            egui::Popup::close_all(ui.ctx());
            self.rules = name;
            refresh = true;
        }
        if let Some(spec) = new_filter.filter(|s| !s.trim().is_empty()) {
            egui::Popup::close_all(ui.ctx());
            self.filter_edit = None;
            self.filter_spec = spec.clone();
            refresh = true;
            action = Some(FolderAction::FilterChanged(spec));
        }
        let pal = widgets::pal(ui);
        let paths = egui::Frame::new()
            .fill(pal.surface)
            .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.columns(n, |cols| {
                    for (s, col) in cols.iter_mut().enumerate() {
                        col.horizontal(|ui| {
                            widgets::side_chip(ui, s, n);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.display_roots[s])
                                        .size(theme::size::DENSE),
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
        let chip_action = self.status_chips(ui, cfg);
        action = action.or(chip_action);
        if let Some(r) = &self.running {
            let done = r.progress.done.load(Ordering::Relaxed);
            let total = r.progress.total.load(Ordering::Relaxed);
            let mut stop = false;
            egui::Frame::new()
                .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
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
                        ui.label(
                            egui::RichText::new(text)
                                .size(theme::size::SMALL)
                                .color(pal.text_dim),
                        );
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .fill(pal.accent)
                                .desired_height(6.0)
                                .corner_radius(theme::radius::XS)
                                .desired_width(ui.available_width() - 90.0)
                                .animate(total == 0 && !theme::look().reduce_motion),
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
        if let Some(flag) = &self.merge_stop {
            let stopping = flag.load(Ordering::Relaxed);
            let mut stop = false;
            egui::Frame::new()
                .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(tr("msg.folder_merge_applying"))
                                .size(theme::size::SMALL)
                                .color(pal.text_dim),
                        );
                        ui.add(
                            egui::ProgressBar::new(0.0)
                                .fill(pal.accent)
                                .desired_height(6.0)
                                .corner_radius(theme::radius::XS)
                                .desired_width(ui.available_width() - 90.0)
                                .animate(!theme::look().reduce_motion),
                        );
                        let button = ui.add_enabled_ui(!stopping, |ui| {
                            widgets::secondary_button(ui, tr("btn.stop"))
                        });
                        stop = button.inner.clicked();
                    });
                });
            if stop {
                self.stop();
            }
        }
        (refresh, action)
    }

    /// Chips that show or hide each kind of item, with how many files are of that kind.
    fn status_chips(&self, ui: &mut Ui, cfg: &Config) -> Option<FolderAction> {
        let n = self.n();
        let t = &self.total;
        let unique_slot = |s: usize| if n == 2 && s == 1 { 2 } else { s };
        let mut identical = cfg.show_identical;
        let mut different = cfg.show_different;
        let mut unique = cfg.show_unique;
        let all_on = identical && different && (0..n).all(|s| unique[unique_slot(s)]);
        let mut all = all_on;
        let mut changed = false;
        egui::Frame::new()
            .inner_margin(Margin::symmetric(theme::space::MD, theme::space::XS))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let count = |label: &str, k: usize| format!("{label}  {k}");
                    let files = t.all();
                    let mut chip = |ui: &mut Ui, on: &mut bool, label: String| {
                        let r = widgets::toggle_button(ui, on, &label, &label);
                        changed |= r.clicked();
                        r
                    };
                    chip(ui, &mut all, count(tr("analysis.all"), files));
                    widgets::toolbar_sep(ui);
                    chip(
                        ui,
                        &mut different,
                        count(tr("view.different"), t.different + t.errors),
                    );
                    for s in 0..n {
                        let label = trf("stats.only_in", &[("side", side_name(s, n).into())]);
                        chip(ui, &mut unique[unique_slot(s)], count(&label, t.only[s]));
                    }
                    chip(ui, &mut identical, count(tr("view.identical"), t.identical));
                });
            });
        if !changed {
            return None;
        }
        if all != all_on {
            // "All" turns everything on; clicked while everything is on, it keeps only the
            // differences.
            identical = !all_on;
            different = true;
            unique = [true; 3];
        }
        Some(FolderAction::Show {
            identical,
            different,
            unique,
        })
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
                ui.painter()
                    .rect_filled(rect, theme::radius::XL, pal.control);
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
                        .font(theme::semibold(theme::size::SUBTITLE)),
                );
                ui.add_space(12.0);
                if widgets::secondary_button(ui, tr("btn.clear_search")).clicked() {
                    self.search.clear();
                    self.view_key = None;
                    self.list_gen += 1;
                }
            });
            return None;
        }
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.3).max(24.0));
            let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, theme::radius::XL, pal.control);
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
                .font(theme::semibold(theme::size::SUBTITLE)),
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

    fn review_ui(&mut self, ui: Option<&mut Ui>, cfg: &Config) {
        use crate::core::{
            analysis,
            review_state::{Decision, State},
        };
        if self
            .workspace
            .bind_policy(&self.dopts, &self.fopts, &self.filter_spec)
        {
            self.view_key = None;
            self.list_gen += 1;
        }
        if self.running.is_none() {
            if let Some(path) = self.review_resume.take() {
                if let Some(i) = self.items.iter().position(|item| item.rel_path() == path) {
                    self.reveal(i, cfg);
                }
            }
        }
        if self.running.is_none()
            && !self.review_checked
            && !self.workspace.entries.is_empty()
            && self.review_fingerprints.is_none()
        {
            let paths: Vec<_> = self
                .items
                .iter()
                .filter(|i| !i.is_dir)
                .map(|i| i.rel_path())
                .collect();
            let roots = self.roots.clone();
            let opts = self.dopts.clone();
            self.review_fingerprints =
                Some(super::analysis::Task::start(move |cancel, progress| {
                    let mut fingerprints = BTreeMap::new();
                    for path in paths {
                        analysis::check_cancel(cancel)?;
                        let p = crate::core::review_state::prints(&path, &roots, &opts)
                            .unwrap_or_else(|_| crate::core::review_state::Prints::unreadable());
                        fingerprints.insert(path, p);
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(fingerprints)
                }));
        }
        if let Some(task) = &mut self.review_fingerprints {
            if let Some(result) = task.poll() {
                match result {
                    Ok(f) => self.workspace.reconcile(&f),
                    Err(e) => {
                        for d in self.workspace.entries.values_mut() {
                            d.stale = true;
                        }
                        self.status = e.to_string();
                    }
                };
                self.review_fingerprints = None;
                self.review_checked = true;
                self.view_key = None;
                self.list_gen += 1;
            }
        }
        let Some(ui) = ui else { return };
        {
            if let Some(task) = &self.review_fingerprints {
                task.status(ui);
            }
            let summary = self.workspace.summary(
                self.items
                    .iter()
                    .filter(|i| !i.is_dir)
                    .map(|i| i.rel_path()),
            );
            ui.label(format!(
                "{}: {} · {}: {} · {}: {} · {}: {}",
                tr("workspace.unreviewed"),
                summary.unreviewed,
                tr("workspace.accepted"),
                summary.accepted + summary.resolved,
                tr("workspace.needs_action"),
                summary.needs_action,
                tr("workspace.stale"),
                summary.stale
            ));
            ui.horizontal_wrapped(|ui| {
                let mut changed = ui
                    .selectable_value(&mut self.review_filter, None, tr("analysis.all"))
                    .changed();
                for state in State::ALL {
                    changed |= ui
                        .selectable_value(&mut self.review_filter, Some(state), tr(state.key()))
                        .changed();
                }
                changed |= ui
                    .checkbox(&mut self.review_stale_only, tr("workspace.stale"))
                    .changed();
                if changed {
                    self.view_key = None;
                    self.list_gen += 1;
                }
                if ui.button(tr("workspace.next")).clicked() {
                    let current = self.workspace.current.as_deref();
                    let mut candidates: Vec<_> = self
                        .items
                        .iter()
                        .enumerate()
                        .filter(|(_, i)| !i.is_dir)
                        .filter(|(_, i)| {
                            self.workspace
                                .entries
                                .get(&i.rel_path())
                                .is_none_or(|d| d.state == State::Unreviewed || d.stale)
                        })
                        .map(|(i, item)| (item.rel_path(), i))
                        .collect();
                    candidates.sort();
                    if let Some((path, i)) = candidates
                        .iter()
                        .find(|(p, _)| current.is_none_or(|c| p.as_str() > c))
                        .or(candidates.first())
                        .cloned()
                    {
                        self.review_filter = None;
                        self.review_stale_only = false;
                        self.view_key = None;
                        self.list_gen += 1;
                        self.workspace.current = Some(path);
                        self.reveal(i, cfg);
                    }
                }
                if ui.button(tr("workspace.export")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("review-summary.json")
                        .save_file()
                    {
                        if let Err(e) = analysis::report(&summary, &path, &self.roots) {
                            self.status = e.to_string();
                        }
                    }
                }
                if ui.button(tr("workspace.update_inputs")).clicked() {
                    let mut roots = Vec::new();
                    for _ in 0..self.n() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            roots.push(path);
                        } else {
                            break;
                        }
                    }
                    if roots.len() == self.n() {
                        self.display_roots =
                            roots.iter().map(|p| p.display().to_string()).collect();
                        self.roots = roots;
                        self.read_only = vec![true; self.n()];
                        self.start_compare();
                    }
                }
            });
            let selected = self
                .cursor
                .and_then(|v| self.visible.get(v))
                .copied()
                .or_else(|| self.selected.iter().next().copied());
            if let Some(i) = selected.filter(|&i| !self.items[i].is_dir) {
                let path = self.items[i].rel_path();
                self.workspace.current = Some(path.clone());
                ui.label(&path);
                let mut decision = self
                    .workspace
                    .entries
                    .get(&path)
                    .cloned()
                    .unwrap_or_default();
                let mut changed = false;
                ui.horizontal(|ui| {
                    for state in State::ALL {
                        if ui
                            .selectable_label(decision.state == state, tr(state.key()))
                            .clicked()
                        {
                            match crate::core::review_state::prints(&path, &self.roots, &self.dopts)
                            {
                                Ok(prints) => {
                                    decision.id = analysis::identity(&(&path, &prints.fingerprint));
                                    decision.fingerprint = prints.fingerprint;
                                    decision.content = prints.content;
                                    decision.state = state;
                                    decision.stale = false;
                                    decision.ambiguous_rename = false;
                                    changed = true;
                                }
                                Err(e) => self.status = e.to_string(),
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tr("workspace.reviewer"));
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut decision.reviewer).char_limit(64))
                        .changed();
                });
                changed |= ui
                    .add(
                        egui::TextEdit::multiline(&mut decision.comment)
                            .hint_text(tr("workspace.comment"))
                            .char_limit(4096)
                            .desired_rows(2),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut decision.rationale)
                            .hint_text(tr("workspace.rationale"))
                            .char_limit(4096),
                    )
                    .changed();
                if decision.stale {
                    ui.colored_label(ui.visuals().warn_fg_color, tr("workspace.stale"));
                }
                if let Some(old) = &decision.renamed_from {
                    let key = if decision.ambiguous_rename {
                        "workspace.ambiguous_rename"
                    } else {
                        "workspace.renamed_from"
                    };
                    ui.label(trf(key, &[("path", old.clone())]));
                }
                if changed {
                    self.workspace.entries.insert(path, Decision { ..decision });
                    self.view_key = None;
                    self.list_gen += 1;
                }
            }
        }
    }

    /// The side panels: each keeps its background work going; the one shown in the drawer,
    /// if any, is drawn there.
    fn panels(
        &mut self,
        mut ui: Option<&mut Ui>,
        shown: Option<FolderPanel>,
        cfg: &Config,
    ) -> (
        Option<String>,
        Option<super::baseline_panel::CapturedComparison>,
    ) {
        let mut ui_for = |panel| {
            if shown == Some(panel) {
                ui.take()
            } else {
                None
            }
        };
        self.review_ui(ui_for(FolderPanel::Review), cfg);
        let navigate = self.merge_verification.ui(
            ui_for(FolderPanel::Verification),
            &self.items,
            &self.roots,
            &self.dopts,
            self.running.is_some() || self.file_job.is_some(),
            &self.filter_spec,
        );
        let captured = self.baseline_panel.ui(
            ui_for(FolderPanel::Baselines),
            &self.roots,
            &self.dopts,
            &self.filter_spec,
        );
        (navigate, captured)
    }

    fn panel_tabs(&self) -> Vec<(FolderPanel, &'static str)> {
        let mut tabs = vec![(FolderPanel::Review, tr("workspace.title"))];
        if self.n() == 3 && self.snapshot_options.is_none() {
            tabs.push((FolderPanel::Verification, tr("analysis.merge_verification")));
        }
        if self.snapshot_options.is_none() {
            tabs.push((FolderPanel::Baselines, tr("baseline.title")));
        }
        tabs
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        cfg: &Config,
        c: &Colors,
        named: &[FltFilter],
    ) -> Option<FolderAction> {
        self.note_window(ui);
        let was_running = self.running.is_some();
        self.poll();
        // Before the header, whose chips show the counts worked out with the list.
        self.update_visible(ui.ctx(), cfg);
        self.nav_text = self.nav_label(cfg);
        let (refresh, header_action) = self.header_bar(ui, cfg, named);
        if refresh {
            self.refresh(cfg, named);
        }
        let (mut navigate, captured) = if self.drawer_open {
            let tabs = self.panel_tabs();
            let mut tab = self.drawer_tab;
            let mut out = (None, None);
            widgets::drawer(ui, self.id.with("drawer"), &mut tab, &tabs, |ui, tab| {
                out = self.panels(Some(ui), Some(tab), cfg);
            });
            self.drawer_tab = tab;
            out
        } else {
            self.panels(None, None, cfg)
        };
        if let Some(path) = self
            .baseline_panel
            .review_request
            .take()
            .filter(|path| self.items.iter().any(|item| item.rel_path() == *path))
        {
            self.drawer_tab = FolderPanel::Review;
            self.workspace.current = Some(path.clone());
            navigate = Some(path);
        }
        if let Some(path) = navigate {
            if let Some(i) = self.items.iter().position(|item| item.rel_path() == path) {
                self.reveal(i, cfg);
            }
        }

        let captured_action = captured.map(|comparison| FolderAction::OpenCaptured { comparison });
        if was_running {
            // Also repaints once after finishing, as the status bar was drawn before this poll.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.update_visible(ui.ctx(), cfg);
        if self.running.is_none() && self.visible.is_empty() {
            return captured_action
                .or(header_action)
                .or_else(|| self.empty_state(ui, cfg, named));
        }
        if cfg.folder_file_pane {
            self.dock_ui(ui, cfg, c);
        } else {
            self.dock = None;
            self.dock_land = None;
        }

        let mut result: Option<FolderAction> = captured_action
            .or(header_action)
            .or_else(|| self.rule_request.take().map(FolderAction::AddRule))
            .or_else(|| {
                self.file_report
                    .take()
                    .map(|(name, job)| FolderAction::ExportFileReport { name, job })
            });
        let mut toggle: Option<usize> = None;
        let mut click: Option<(usize, Modifiers)> = None;
        let mut double: Option<usize> = None;
        let mut menu: Option<MenuCmd> = None;
        let mut new_sort: Option<Col> = None;

        // keyboard navigation
        let row_h = ui.text_style_height(&egui::TextStyle::Body)
            + if theme::look().compact { 6.0 } else { 12.0 };
        if !widgets::typing(ui.ctx())
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
        // The tree already shows where each item is.
        let folder_col = !cfg.tree_mode;
        // Starting widths and minimums of the columns before the last, which takes the rest.
        let mut widths: Vec<(f32, f32)> = vec![(280.0, 80.0)];
        if folder_col {
            widths.push((160.0, 40.0));
        }
        widths.extend([(230.0, 60.0), (210.0, 60.0)]);
        widths.extend((0..n).map(|_| (90.0, 30.0)));
        // A list too narrow for them starts them narrower, in steps of an eighth, each step
        // keeping its own column state, so every column stays in view.
        let need = widths.iter().map(|w| w.0).sum::<f32>();
        let fixed = 40.0 + ui.spacing().item_spacing.x * (widths.len() + 2) as f32;
        let eighths = (((list_rect.width() - fixed) / need * 8.0).floor() as u8).clamp(3, 8);
        let scale = eighths as f32 / 8.0;
        let mut table = TableBuilder::new(ui)
            .id_salt(self.id.with((folder_col, eighths)))
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center));
        for (width, least) in widths {
            let width = width * scale;
            table = table.column(Column::initial(width).at_least(least.min(width)).clip(true));
        }
        table = table.column(Column::remainder().at_least(40.0).clip(true));
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, None);
        }
        let mut cols = vec![(Col::Name, tr("col.name").to_string())];
        if folder_col {
            cols.push((Col::Folder, tr("col.folder").into()));
        }
        cols.push((Col::Result, tr("col.result").into()));
        cols.push((Col::Modified, tr("col.modified").into()));
        for s in 0..n {
            cols.push((
                Col::Size(s),
                format!("{} {}", side_name(s, n), tr("col.size")),
            ));
        }
        cols.push((Col::Ext, tr("col.ext").into()));

        let sort = self.sort;
        let tolerance = self.fopts.time_tolerance_secs;
        let tallies = &self.tallies;
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
                        let color = if sorted { pal.text } else { pal.text_dim };
                        let text = egui::RichText::new(label.as_str())
                            .font(theme::semibold(theme::size::SMALL))
                            .color(color);
                        let response = ui.add(egui::Button::new(text).frame(false));
                        if sorted {
                            let key = if sort.1 {
                                "col.sorted_ascending"
                            } else {
                                "col.sorted_descending"
                            };
                            let name = format!("{label}, {}", tr(key));
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::Button, true, &name)
                            });
                        }
                        if response.clicked() {
                            new_sort = Some(*col);
                        }
                        if sorted {
                            let mark = if sort.1 { Icon::Prev } else { Icon::Next };
                            widgets::small_icon(ui, mark, color);
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
                    // Only errors tint the whole row; other differences mark its edge, so a
                    // list where most rows differ stays readable.
                    let bg = (it.status == CmpStatus::Error).then_some(c.folder_error);
                    let paint_bg = |ui: &mut Ui| {
                        if let Some(bg) = bg {
                            if !selected.contains(&idx) {
                                let r = ui.max_rect().expand2(vec2(4.0, 2.0));
                                ui.painter().rect_filled(r, 0.0, bg);
                            }
                        }
                    };
                    let txt = egui::RichText::new;
                    let sc = status_color(it);
                    row.col(|ui| {
                        paint_bg(ui);
                        if matches!(
                            it.status,
                            CmpStatus::Different | CmpStatus::Unique | CmpStatus::Error
                        ) {
                            let r = ui.max_rect().expand2(vec2(0.0, 2.0));
                            let edge = egui::Rect::from_min_size(r.min, vec2(3.0, r.height()));
                            ui.painter().rect_filled(edge, 0.0, sc);
                        }
                        if tree {
                            ui.add_space(it.depth as f32 * 16.0);
                            if it.is_dir && !it.children.is_empty() {
                                let open = searching || expanded.contains(&it.rel_path());
                                let (icon, key) = if open {
                                    (Icon::Next, "folder.collapse")
                                } else {
                                    (Icon::ChevronRight, "folder.expand")
                                };
                                let name = trf(key, &[("name", it.name.clone())]);
                                if widgets::mini_icon_button(ui, icon, &name).clicked() {
                                    toggle = Some(idx);
                                }
                            } else {
                                ui.add_space(16.0);
                            }
                        }
                        let (icon, icon_color) = if it.is_dir {
                            (Icon::Folder, pal.folder)
                        } else {
                            (Icon::File, pal.text_dim)
                        };
                        widgets::inline_icon(ui, icon, icon_color);
                        ui.add(
                            egui::Label::new(txt(if self.notes.contains_key(&it.rel_path()) {
                                format!("* {}", it.name)
                            } else {
                                it.name.clone()
                            }))
                            .selectable(false)
                            .truncate(),
                        );
                    });
                    if folder_col {
                        row.col(|ui| {
                            paint_bg(ui);
                            ui.add(
                                egui::Label::new(txt(it.rel_dir.clone()).color(pal.text_dim))
                                    .selectable(false)
                                    .truncate(),
                            );
                        });
                    }
                    row.col(|ui| {
                        paint_bg(ui);
                        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, sc);
                        let label = shown_status(it, n, tallies.get(idx));
                        let detail = (!it.is_dir && it.binary && it.status != CmpStatus::Unique)
                            .then(|| tr("kind.binary"));
                        let r = ui.add(
                            egui::Label::new(
                                txt(label).font(FontId::proportional(theme::size::CONTROL)),
                            )
                            .selectable(false)
                            .truncate(),
                        );
                        if let Some(d) = detail {
                            ui.label(
                                txt(format!("· {d}"))
                                    .size(theme::size::SMALL)
                                    .color(pal.text_dim),
                            );
                        }
                        r.on_hover_ui(|ui| {
                            ui.label(status_text(it, n));
                        });
                    });
                    row.col(|ui| {
                        paint_bg(ui);
                        let r =
                            ui.add(egui::Label::new(txt(fmt_time(newest(it)))).selectable(false));
                        if let Some(s) = newer_side(it, n, tolerance) {
                            ui.label(
                                txt(trf("folder.newer", &[("side", side_name(s, n).into())]))
                                    .size(theme::size::SMALL)
                                    .color(side_cols[s]),
                            );
                        }
                        if it.mtime[..n].iter().flatten().count() > 1 {
                            r.on_hover_ui(|ui| {
                                for s in (0..n).filter(|&s| it.exists[s]) {
                                    ui.label(format!(
                                        "{}  {}",
                                        side_name(s, n),
                                        fmt_time(it.mtime[s])
                                    ));
                                }
                            });
                        }
                    });
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
                    if let Some(note) = self.notes.get(&it.rel_path()) {
                        resp.clone().on_hover_text(note);
                    }
                    if resp.double_clicked() {
                        double = Some(idx);
                    } else if resp.clicked() {
                        click = Some((vi, resp.ctx.input(|i| i.modifiers)));
                    }
                    if resp.secondary_clicked() && !selected.contains(&idx) {
                        click = Some((vi, Modifiers::NONE));
                    }
                    resp.context_menu(|ui| {
                        widgets::menu_setup(ui);
                        if ui.button(tr("cmd.compare")).clicked() {
                            menu = Some(MenuCmd::Open(idx, None));
                        }
                        if !it.is_dir {
                            widgets::submenu(ui, tr("cmd.compare_as"), |ui| {
                                for (kind, key) in [
                                    (FileKind::Text, "kind.text"),
                                    (FileKind::Binary, "kind.binary"),
                                    (FileKind::Image, "kind.image"),
                                    (FileKind::Web, "kind.web"),
                                ] {
                                    if ui.button(tr(key)).clicked() {
                                        menu = Some(MenuCmd::Open(idx, Some(kind)));
                                    }
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
                                        .add_enabled(
                                            it.exists[from] && self.writable(to),
                                            egui::Button::new(label),
                                        )
                                        .clicked()
                                    {
                                        menu = Some(MenuCmd::Op(OpKind::Copy { from, to }));
                                    }
                                }
                            }
                        }
                        widgets::submenu(ui, tr("cmd.copy_to_folder"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::CopyTo(s));
                                }
                            }
                        });
                        widgets::submenu(ui, tr("cmd.delete"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(
                                        it.exists[s] && self.writable(s),
                                        egui::Button::new(side_name(s, n)),
                                    )
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
                        widgets::submenu(ui, tr("cmd.open_with_app"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::SystemOpen(idx, s));
                                }
                            }
                        });
                        widgets::submenu(ui, tr("cmd.reveal"), |ui| {
                            for s in 0..n {
                                if ui
                                    .add_enabled(it.exists[s], egui::Button::new(side_name(s, n)))
                                    .clicked()
                                {
                                    menu = Some(MenuCmd::Reveal(idx, s));
                                }
                            }
                        });
                        widgets::submenu(ui, tr("cmd.copy_path"), |ui| {
                            for s in 0..n {
                                if ui.button(side_name(s, n)).clicked() {
                                    menu = Some(MenuCmd::CopyPath(idx, s));
                                }
                            }
                        });
                        ui.separator();
                        if ui.button(tr("cmd.toggle_bookmark")).clicked() {
                            menu = Some(MenuCmd::Bookmark(idx));
                        }
                        if ui.button(tr("dlg.bookmark_comment")).clicked() {
                            menu = Some(MenuCmd::Comment(idx));
                        }
                        if ui
                            .add_enabled(
                                self.running.is_none(),
                                egui::Button::new(tr("review.save_selected")),
                            )
                            .clicked()
                        {
                            menu = Some(MenuCmd::SaveReview);
                        }
                        if ui.button(tr("cmd.select_pattern")).clicked() {
                            menu = Some(MenuCmd::SelectPattern);
                        }
                        if menu.is_some() {
                            ui.close();
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
            self.list_gen += 1;
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
            self.list_gen += 1;
        }
        if let Some(i) = double {
            let it = &self.items[i];
            if it.is_dir && cfg.tree_mode && !it.children.is_empty() {
                let key = it.rel_path();
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
                self.view_key = None;
                self.list_gen += 1;
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
                MenuCmd::SystemOpen(i, s) => {
                    if let Err(e) = system_open(&self.items[i].path(&self.roots[s])) {
                        self.status = e;
                    }
                }
                MenuCmd::Reveal(i, s) => {
                    if let Err(e) = reveal(&self.items[i].path(&self.roots[s])) {
                        self.status = e;
                    }
                }
                MenuCmd::CopyPath(i, s) => ui
                    .ctx()
                    .copy_text(self.items[i].path(&self.roots[s]).display().to_string()),
                MenuCmd::CopyTo(s) => self.pick_copy_destination(s),
                MenuCmd::SelectPattern => self.open_select_pattern(),
                MenuCmd::Bookmark(i) => {
                    let path = self.items[i].rel_path();
                    if self.notes.remove(&path).is_none() {
                        self.notes.insert(path, String::new());
                    }
                }
                MenuCmd::Comment(i) => {
                    let path = self.items[i].rel_path();
                    self.note_edit = Some((
                        path.clone(),
                        self.notes.get(&path).cloned().unwrap_or_default(),
                    ));
                }
                MenuCmd::SaveReview => result = Some(FolderAction::SaveReview(true)),
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
                    .corner_radius(theme::radius::MD)
                    .inner_margin(Margin::same(theme::space::CONTROL))
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
        if self.poll_file_job() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.merge_dialog(ui, cfg);
        self.select_pattern_dialog(ui, cfg);
        if let Some(rescan) = self.sync.as_mut().and_then(|d| d.show(ui)) {
            self.sync = None;
            if rescan {
                self.mark_archives_dirty();
                self.start_compare();
            }
        }
        if let Some(i) = self.cursor.and_then(|v| self.visible.get(v)).copied() {
            if !self.items[i].is_dir {
                self.workspace.current = Some(self.items[i].rel_path());
            }
        }
        result
    }
}

/// Compares `roots`, then steps from the first file with differences to the last as Next
/// Difference does without the docked compare, reading the status bar position at each.
#[cfg(test)]
pub(crate) fn navigation_bench(roots: &[PathBuf]) -> String {
    let cfg = Config::default();
    let display = roots.iter().map(|r| r.display().to_string()).collect();
    let started = Instant::now();
    let mut v = FolderView::new(roots.to_vec(), display, Vec::new(), "", &[], &cfg);
    while v.running.is_some() {
        std::thread::sleep(Duration::from_millis(2));
        v.poll();
    }
    let compared = started.elapsed();
    v.rebuild_visible(&cfg);
    let started = Instant::now();
    v.edge_diff(true, &cfg);
    let first = started.elapsed();
    let mut steps = 1;
    loop {
        std::hint::black_box(v.nav_label(&cfg));
        v.step_diff(true, &cfg);
        if v.take_notice().is_some() {
            break;
        }
        steps += 1;
    }
    let walked = started.elapsed();
    format!(
        "{steps} files in {} ms, {:.2} ms a step, first {} ms; compare {} ms",
        walked.as_millis(),
        walked.as_secs_f64() * 1000.0 / steps as f64,
        first.as_millis(),
        compared.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, is_dir: bool, exists: [bool; 3], status: CmpStatus) -> DirItem {
        DirItem {
            rel_dir: String::new(),
            name: name.into(),
            is_dir,
            exists,
            size: [None; 3],
            mtime: [None; 3],
            status,
            diff_side: None,
            binary: false,
            error: None,
            depth: 0,
            parent: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn counts_follow_what_is_listed() {
        let items = [
            // 3-way: on Left and Right, missing from Middle: differs, as the filters treat it.
            item("two.txt", false, [true, false, true], CmpStatus::Unique),
            item("left.txt", false, [true, false, false], CmpStatus::Unique),
            // Left over after Stop.
            item(
                "later.txt",
                false,
                [true, true, true],
                CmpStatus::NotCompared,
            ),
            // A folder on Left only that was not scanned.
            item("old", true, [true, false, false], CmpStatus::Unique),
            // A folder of a compare that is not recursive: listed, but not different.
            item("sub", true, [true, true, true], CmpStatus::NotCompared),
        ];
        let (_, total) = Tally::of(&items, 3);
        assert_eq!(total.different, 2);
        assert_eq!(total.only, [2, 0, 0]);
        assert_eq!((total.unknown, total.all()), (1, 5));
        assert_eq!(only_side(&items[0], 3), None);
        assert_eq!(only_side(&items[1], 3), Some(0));
    }

    #[test]
    fn results_sort_their_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_numeric("2 different", "10 different"), Less);
        assert_eq!(cmp_numeric("3 different · 1", "3 different · 01"), Equal);
        assert_eq!(cmp_numeric("Different", "Identical"), Less);
        assert_eq!(cmp_numeric("9", "9 · 2"), Less);
        assert_eq!(cmp_numeric("12 個不同", "3 個不同"), Greater);
    }

    #[test]
    fn the_newer_side_allows_for_the_time_tolerance() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut it = item("a.txt", false, [true, true, false], CmpStatus::Different);
        it.mtime = [Some(t), Some(t + Duration::from_secs(3600)), None];
        assert_eq!(newer_side(&it, 2, 2), Some(1));
        assert_eq!(newer_side(&it, 2, 3600), None);
    }

    #[test]
    fn saved_folder_rules_apply_to_initial_scan_refresh_and_reports() {
        use crate::core::review::{self, Capture};
        let source = tempfile::tempdir().unwrap();
        let roots: Vec<PathBuf> = ["left", "right"]
            .iter()
            .map(|p| source.path().join(p))
            .collect();
        for root in &roots {
            std::fs::create_dir(root).unwrap();
        }
        for (name, left, right) in [
            (
                "lines.txt",
                "timestamp=one\nkeep\n",
                "timestamp=two\nkeep\n",
            ),
            ("sub.txt", "value=123\n", "value=456\n"),
            (
                "blocks.txt",
                "begin\nold\nend\nkeep\n",
                "begin\nnew\nend\nkeep\n",
            ),
        ] {
            std::fs::write(roots[0].join(name), left).unwrap();
            std::fs::write(roots[1].join(name), right).unwrap();
        }
        let options = DiffOptions {
            line_filters_enabled: true,
            line_filters: vec!["^timestamp=".into()],
            substitutions_enabled: true,
            substitutions: vec![("value=\\d+".into(), "value=constant".into())],
            block_filters_enabled: true,
            block_filters: vec![("^begin$".into(), "^end$".into())],
            ..Default::default()
        };
        let folder = FolderOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &folder,
            dopts: &options,
            progress: &Progress::default(),
        }
        .run();
        assert!(items.iter().all(|i| i.status == CmpStatus::Identical));
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join("rules.oxmreview");
        review::save(
            &path,
            Capture {
                roots: &roots,
                labels: &["Left".into(), "Right".into()],
                items: &items.iter().collect::<Vec<_>>(),
                options: &options,
                folder: &folder,
                filter: "*.txt",
                notes: &BTreeMap::new(),
            },
        )
        .unwrap();
        drop(source);
        let cfg = Config::default();
        let mut view = FolderView::from_review(review::load(&path).unwrap(), &cfg);
        let wait = |v: &mut FolderView| {
            let start = Instant::now();
            while v.running.is_some() {
                assert!(start.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(5));
                v.poll();
            }
        };
        wait(&mut view);
        assert_eq!(view.dopts, options);
        assert_eq!(view.items.len(), 3);
        assert!(view.items.iter().all(|i| i.status == CmpStatus::Identical));
        assert_eq!(view.diffs_source(&cfg).dopts, options);
        view.refresh(&cfg, &[]);
        wait(&mut view);
        assert_eq!(view.dopts, options);
        assert!(view.items.iter().all(|i| i.status == CmpStatus::Identical));
    }

    #[test]
    fn extracted_sides_are_read_only_in_all_differences() {
        let cfg = Config::default();
        let left = tempfile::tempdir().unwrap();
        let extracted = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), "x\n").unwrap();
        std::fs::write(extracted.path().join("a.txt"), "y\n").unwrap();
        let roots = vec![left.path().to_path_buf(), extracted.path().to_path_buf()];
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let mut v = FolderView::new(roots, display, vec![extracted], "", &[], &cfg);
        let wait = |v: &mut FolderView| {
            while v.running.is_some() {
                std::thread::sleep(Duration::from_millis(10));
                v.poll();
            }
        };
        wait(&mut v);
        assert_eq!(v.read_only, [false, true]);
        let src = v.diffs_source(&cfg);
        assert_eq!(src.read_only, [false, true]);
        assert_eq!(src.items.len(), 1);
        assert!(!src.stopped);
    }

    #[test]
    fn archive_sides_are_writable_and_save_repacks_them() {
        use std::io::Write;
        let cfg = Config::default();
        let left = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), "x\n").unwrap();

        let work = tempfile::tempdir().unwrap();
        let zip_path = work.path().join("right.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zw.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"y\n").unwrap();
        zw.finish().unwrap();

        let extracted = archive::extract_to_temp(&zip_path).unwrap();
        let roots = vec![left.path().to_path_buf(), extracted.path().to_path_buf()];
        let display = vec![
            left.path().display().to_string(),
            zip_path.display().to_string(),
        ];
        let mut v = FolderView::new(roots, display, vec![extracted], "", &[], &cfg);
        let wait = |v: &mut FolderView| {
            while v.running.is_some() {
                std::thread::sleep(Duration::from_millis(10));
                v.poll();
            }
        };
        wait(&mut v);

        // Unlike a non-archive extracted copy, this side is writable, and the real archive
        // file is tracked as its save target.
        assert_eq!(v.read_only, [false, false]);
        assert!(!v.is_archive_side(0));
        assert!(v.is_archive_side(1));
        assert!(!v.has_archive_changes());

        // Edit the extracted copy directly, as saving a file tab opened from it would.
        std::fs::write(v.roots[1].join("a.txt"), "edited\n").unwrap();
        v.mark_archive_dirty(1);
        assert!(v.has_archive_changes());
        assert_eq!(v.save_archives().1, Vec::<String>::new());
        assert!(!v.has_archive_changes());

        let after = archive::extract_to_temp(&zip_path).unwrap();
        assert_eq!(
            std::fs::read_to_string(after.path().join("a.txt")).unwrap(),
            "edited\n"
        );

        // A side fetched from a server is marked edited by what writes into it, and only it.
        v.set_remote_side(0);
        assert!(v.is_remote_side(0) && !v.is_remote_side(1));
        assert!(!v.has_remote_changes());
        v.mark_remote_dirty(1);
        assert!(!v.has_remote_changes());
        v.mark_archives_dirty();
        assert!(v.has_remote_changes());
        assert!(v.take_remote_dirty(0));
        assert!(!v.take_remote_dirty(0));
        assert!(!v.has_remote_changes());
    }

    /// A zip holding one `a.txt`, under `name` in `dir`, inside a zip of that name when
    /// `inside` is given, extracted.
    fn nested_zip(dir: &Path, name: &str, inside: &str) -> (PathBuf, tempfile::TempDir) {
        let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        inner
            .start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut inner, b"old\n").unwrap();
        let outer = dir.join(name);
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&outer).unwrap());
        zw.start_file(inside, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zw, &inner.finish().unwrap().into_inner()).unwrap();
        zw.finish().unwrap();
        let outer_copy = archive::extract_to_temp(&outer).unwrap();
        (outer_copy.path().join(inside), outer_copy)
    }

    /// The archive side of a folder compare of `path` with an empty folder.
    fn archive_side(path: &Path, left: &Path, cfg: &Config) -> FolderView {
        let extracted = archive::extract_to_temp(path).unwrap();
        let roots = vec![left.to_path_buf(), extracted.path().to_path_buf()];
        let display = vec![left.display().to_string(), path.display().to_string()];
        FolderView::new(roots, display, vec![extracted], "", &[], cfg)
    }

    /// A signed package, and an archive inside an extracted signed package, can't be
    /// written back, so their sides open read-only.
    #[test]
    fn archives_that_cannot_be_written_back_open_read_only() {
        let cfg = Config::default();
        let left = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let apk = work.path().join("app.apk");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&apk).unwrap());
        zw.start_file("classes.dex", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.finish().unwrap();
        let (nested, _outer) = nested_zip(work.path(), "outer.apk", "inner.zip");

        for path in [apk, nested] {
            let v = archive_side(&path, left.path(), &cfg);
            assert_eq!(v.read_only, [false, true], "{}", path.display());
            assert!(!v.is_archive_side(1));
        }
    }

    /// A `.tar.xz` side and a side of a zip inside an extracted zip can be edited; saving
    /// the nested one writes the outer zip too.
    #[test]
    fn a_tar_xz_and_a_nested_archive_open_editable() {
        let cfg = Config::default();
        let left = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let txz = work.path().join("a.tar.xz");
        let mut tar = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o644);
        tar.append_data(&mut h, "a.txt", &b"abc"[..]).unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar.into_inner().unwrap().as_slice(), &mut xz).unwrap();
        std::fs::write(&txz, xz).unwrap();
        let v = archive_side(&txz, left.path(), &cfg);
        assert_eq!(v.read_only, [false, false]);
        assert!(v.is_archive_side(1));

        let (nested, _outer) = nested_zip(work.path(), "outer.zip", "inner.zip");
        let mut v = archive_side(&nested, left.path(), &cfg);
        assert_eq!(v.read_only, [false, false]);
        assert!(v.is_archive_side(1));
        std::fs::write(v.roots[1].join("a.txt"), "new\n").unwrap();
        v.mark_archive_dirty(1);
        let outer = work.path().join("outer.zip");
        assert_eq!(v.save_archives(), (vec![nested, outer.clone()], Vec::new()));
        assert_eq!(v.take_written().len(), 2);
        let outer_after = archive::extract_to_temp(&outer).unwrap();
        let inner_after = archive::extract_to_temp(&outer_after.path().join("inner.zip")).unwrap();
        assert_eq!(
            std::fs::read_to_string(inner_after.path().join("a.txt")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn a_comparison_that_breaks_off_shows_an_error() {
        let cfg = Config::default();
        let roots = ["samples/left", "samples/right"]
            .map(PathBuf::from)
            .to_vec();
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        v.running.take().unwrap().handle.join().unwrap();
        v.running = Some(Running {
            progress: Arc::new(Progress::live()),
            handle: std::thread::spawn(|| panic!("comparison broke off")),
            started: Instant::now(),
            updated: Instant::now(),
        });
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        assert_eq!(v.outcome(), Outcome::Error);
        assert!(v.status_line().ends_with(tr("status.error")));
        assert!(v.diffs_source(&cfg).stopped);
    }

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
    fn steps_between_files_as_listed_and_waits_for_the_docked_compare() {
        use crate::core::review_state::{Decision, State};
        let dir = tempfile::tempdir().unwrap();
        for (side, text) in [("left", "x\n"), ("right", "y\n")] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
            for f in ["a.txt", "b.txt", "c.txt"] {
                std::fs::write(dir.path().join(side).join(f), text).unwrap();
            }
        }
        let roots = vec![dir.path().join("left"), dir.path().join("right")];
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let mut cfg = Config::default();
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        v.rebuild_visible(&cfg);
        let at = |v: &FolderView| v.cursor_item().map(|i| v.items[i].rel_path());

        // Without the docked compare, the keys select files.
        v.edge_diff(false, &cfg);
        assert_eq!(at(&v).as_deref(), Some("c.txt"));
        v.edge_diff(true, &cfg);
        assert_eq!(at(&v).as_deref(), Some("a.txt"));
        v.step_diff(true, &cfg);
        assert_eq!(at(&v).as_deref(), Some("b.txt"));
        v.step_diff(false, &cfg);
        v.step_diff(false, &cfg);
        assert_eq!(at(&v).as_deref(), Some("a.txt"));
        assert_eq!(v.take_notice(), Some("msg.no_prev_file"));
        assert!(v.dock_land.is_none());

        // Files the review filter leaves out are passed over.
        for (rel, state) in [
            ("a.txt", State::NeedsAction),
            ("b.txt", State::Accepted),
            ("c.txt", State::NeedsAction),
        ] {
            let decision = Decision {
                state,
                ..Default::default()
            };
            v.workspace.entries.insert(rel.into(), decision);
        }
        v.review_filter = Some(State::NeedsAction);
        v.rebuild_visible(&cfg);
        v.edge_diff(true, &cfg);
        v.step_diff(true, &cfg);
        assert_eq!(at(&v).as_deref(), Some("c.txt"));
        // from a file tab whose file the filter leaves out
        let rel = |i: Option<usize>| i.map(|i| v.items[i].rel_path());
        assert_eq!(
            rel(v.adjacent_different_file(&cfg, "b.txt", true)).as_deref(),
            Some("c.txt")
        );
        assert_eq!(
            rel(v.adjacent_different_file(&cfg, "b.txt", false)).as_deref(),
            Some("a.txt")
        );
        // the last file listed leaves the list: the cursor stays at its end
        v.workspace.entries.get_mut("c.txt").unwrap().state = State::Accepted;
        v.rebuild_visible(&cfg);
        assert_eq!(at(&v).as_deref(), Some("a.txt"));
        v.step_diff(true, &cfg);
        assert_eq!(v.take_notice(), Some("msg.no_next_file"));
        v.workspace.entries.get_mut("c.txt").unwrap().state = State::NeedsAction;
        v.review_filter = Some(State::Resolved);
        v.rebuild_visible(&cfg);
        v.edge_diff(true, &cfg);
        assert_eq!(v.take_notice(), Some("msg.no_listed_different_files"));

        // While the docked compare loads a file, steps wait for it and are all kept, and First
        // or Last is where it opens.
        v.review_filter = None;
        v.rebuild_visible(&cfg);
        v.edge_diff(true, &cfg);
        let a = v.cursor_item().unwrap();
        cfg.folder_file_pane = true;
        v.step_diff(true, &cfg);
        assert_eq!(v.cursor_item(), Some(a));
        assert_eq!(v.dock_land, Some(Land::new(a, true)));
        v.step_diff(true, &cfg);
        v.step_diff(true, &cfg);
        v.step_diff(false, &cfg);
        assert_eq!(
            v.dock_land.map(|l| (l.at, l.steps)),
            Some((LandAt::First, 1))
        );
        v.edge_diff(false, &cfg);
        assert_eq!(v.dock_land, Some(Land::new(a, false)));
        v.edge_diff(true, &cfg);
        assert_eq!(v.dock_land, Some(Land::new(a, true)));
        assert_eq!(v.cursor_item(), Some(a));
    }

    #[test]
    fn steps_asked_for_while_the_docked_compare_loads_are_all_taken() {
        let dir = tempfile::tempdir().unwrap();
        let far = |v: [&str; 3]| v.join(&format!("\n{}", "same\n".repeat(12))) + "\n";
        let files = [
            ("a.txt", far(["1", "2", "3"]), far(["one", "two", "three"])),
            ("b.txt", "p\n".to_string(), "q\n".to_string()),
            ("c.txt", "r\n".to_string(), "s\n".to_string()),
        ];
        for (name, left, right) in &files {
            for (side, text) in [("left", left), ("right", right)] {
                std::fs::create_dir_all(dir.path().join(side)).unwrap();
                std::fs::write(dir.path().join(side).join(name), text).unwrap();
            }
        }
        let roots = vec![dir.path().join("left"), dir.path().join("right")];
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let cfg = Config {
            folder_file_pane: true,
            ..Config::default()
        };
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        v.rebuild_visible(&cfg);
        let ctx = egui::Context::default();
        let settle = |v: &mut FolderView| {
            let start = Instant::now();
            loop {
                v.update_dock(&ctx, &cfg);
                let loading = matches!(v.dock, Some((_, Dock::Waiting(_) | Dock::Loading(_))));
                if !loading && v.dock_land.is_none() {
                    return;
                }
                assert!(start.elapsed() < Duration::from_secs(20), "never docked");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let docked = |v: &FolderView| v.docked().map(|(rel, at)| (rel, at.map(|a| a + 1)));

        v.edge_diff(true, &cfg);
        v.step_diff(true, &cfg);
        v.step_diff(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(3))));
        assert_eq!(
            v.nav_label(&cfg).as_deref(),
            Some("File 1 / 3 · Difference 3 / 3")
        );
        v.step_diff(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("b.txt".into(), Some(1))));
        // Back to the last difference of a.txt, then two steps on while it loads: past its
        // end to b.txt, and past the one difference of b.txt on to c.txt.
        v.step_diff(false, &cfg);
        v.step_diff(true, &cfg);
        v.step_diff(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("c.txt".into(), Some(1))));
        assert_eq!(v.take_notice(), None);
        v.step_diff(false, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("b.txt".into(), Some(1))));

        // A file that cannot be read stops the steps, to retry or skip it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let c = dir.path().join("left/c.txt");
            std::fs::set_permissions(&c, std::fs::Permissions::from_mode(0o000)).unwrap();
            if std::fs::File::open(&c).is_err() {
                v.step_diff(true, &cfg);
                settle(&mut v);
                assert!(matches!(v.dock, Some((_, Dock::Failed(_)))));
                assert_eq!(v.nav_label(&cfg).as_deref(), Some("File 3 / 3"));
                v.step_diff(true, &cfg);
                assert_eq!(v.take_notice(), Some("msg.no_next_file"));
                v.step_diff(false, &cfg);
                settle(&mut v);
                assert_eq!(docked(&v), Some(("b.txt".into(), Some(1))));
            }
            std::fs::set_permissions(&c, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    #[test]
    fn next_conflict_goes_through_open_conflicts_of_the_files_both_sides_changed() {
        let dir = tempfile::tempdir().unwrap();
        let far = |v: [&str; 2]| v.join(&format!("\n{}", "same\n".repeat(12))) + "\n";
        // Left, middle, right. b.txt changed on both sides in different places merges
        // cleanly; c.txt changed on the left only.
        let files = [
            (
                "a.txt",
                [far(["L1", "L2"]), far(["1", "2"]), far(["R1", "R2"])],
            ),
            (
                "b.txt",
                [far(["L1", "2"]), far(["1", "2"]), far(["1", "R2"])],
            ),
            ("c.txt", ["L\n".into(), "m\n".into(), "m\n".into()]),
            ("d.txt", ["L\n".into(), "m\n".into(), "R\n".into()]),
        ];
        let sides = ["left", "middle", "right"];
        for (name, texts) in &files {
            for (side, text) in sides.iter().zip(texts) {
                std::fs::create_dir_all(dir.path().join(side)).unwrap();
                std::fs::write(dir.path().join(side).join(name), text).unwrap();
            }
        }
        let roots: Vec<PathBuf> = sides.iter().map(|s| dir.path().join(s)).collect();
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let cfg = Config {
            folder_file_pane: true,
            ..Config::default()
        };
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        v.rebuild_visible(&cfg);
        let ctx = egui::Context::default();
        let settle = |v: &mut FolderView| {
            let start = Instant::now();
            loop {
                v.update_dock(&ctx, &cfg);
                let loading = matches!(v.dock, Some((_, Dock::Waiting(_) | Dock::Loading(_))));
                if !loading && v.dock_land.is_none() {
                    return;
                }
                assert!(start.elapsed() < Duration::from_secs(20), "never docked");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let docked = |v: &FolderView| v.docked().map(|(rel, at)| (rel, at.map(|a| a + 1)));

        v.step_conflict(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(1))));
        v.step_conflict(true, &cfg);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(2))));
        // b.txt has no open conflict once loaded, so the step goes on to d.txt.
        v.step_conflict(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("d.txt".into(), Some(1))));
        v.step_conflict(true, &cfg);
        assert_eq!(v.take_notice(), Some("msg.no_next_conflict_file"));
        assert_eq!(docked(&v), Some(("d.txt".into(), Some(1))));
        // Back to the last conflict of a.txt, and steps asked for while files load are taken.
        v.step_conflict(false, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(2))));
        v.step_conflict(false, &cfg);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(1))));
        v.step_conflict(false, &cfg);
        assert_eq!(v.take_notice(), Some("msg.no_prev_conflict_file"));
        // Next difference still stops at every changed file, c.txt too.
        v.step_diff(true, &cfg);
        v.step_diff(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v).map(|d| d.0), Some("b.txt".into()));
        // From the last conflict of a.txt, two steps while b.txt loads: b.txt has none, so the
        // first lands in d.txt and the second finds no file after it.
        v.step_conflict(false, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("a.txt".into(), Some(2))));
        v.step_conflict(true, &cfg);
        v.step_conflict(true, &cfg);
        settle(&mut v);
        assert_eq!(docked(&v), Some(("d.txt".into(), Some(1))));
        assert_eq!(v.take_notice(), Some("msg.no_next_conflict_file"));
    }

    #[test]
    fn steps_through_every_kind_of_file_and_files_changed_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let far = |v: [&str; 3]| v.join(&format!("\n{}", "same\n".repeat(12))) + "\n";
        let png = |shade: u8| [b"\x89PNG\r\n\x1a\n".as_slice(), &[0, shade, 0, 0]].concat();
        let files: [(&str, Vec<u8>, Vec<u8>); 7] = [
            ("added.txt", b"a\n".to_vec(), b"a\nb\n".to_vec()),
            ("bin.dat", vec![0, 1, 2], vec![0, 1, 3]),
            ("img.png", png(1), png(2)),
            (
                "multi.txt",
                far(["1", "2", "3"]).into_bytes(),
                far(["one", "two", "three"]).into_bytes(),
            ),
            ("only-left.txt", b"x\n".to_vec(), Vec::new()),
            ("removed.txt", b"a\nb\n".to_vec(), b"a\n".to_vec()),
            ("same.txt", b"s\n".to_vec(), b"s\n".to_vec()),
        ];
        for side in ["left", "right"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        for (name, left, right) in &files {
            std::fs::write(dir.path().join("left").join(name), left).unwrap();
            if *name != "only-left.txt" {
                std::fs::write(dir.path().join("right").join(name), right).unwrap();
            }
        }
        let roots = vec![dir.path().join("left"), dir.path().join("right")];
        let display = roots.iter().map(|r| r.display().to_string()).collect();
        let cfg = Config {
            folder_file_pane: true,
            ..Config::default()
        };
        let mut v = FolderView::new(roots, display, Vec::new(), "", &[], &cfg);
        while v.running.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            v.poll();
        }
        v.rebuild_visible(&cfg);
        let ctx = egui::Context::default();
        let settle = |v: &mut FolderView| {
            let start = Instant::now();
            loop {
                v.update_dock(&ctx, &cfg);
                let loading = matches!(v.dock, Some((_, Dock::Waiting(_) | Dock::Loading(_))));
                if !loading && v.dock_land.is_none() {
                    return;
                }
                assert!(start.elapsed() < Duration::from_secs(20), "never docked");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let at = |v: &FolderView| {
            let rel = v.cursor_item().map(|i| v.items[i].rel_path()).unwrap();
            (rel, v.docked().and_then(|d| d.1).map(|a| a + 1))
        };
        let walk = |v: &mut FolderView| {
            let mut walked = Vec::new();
            let first = v.edge_different_file(&cfg, true).unwrap();
            v.step_to(first, true, &cfg);
            loop {
                settle(v);
                walked.push(at(v));
                v.step_diff(true, &cfg);
                if let Some(notice) = v.take_notice() {
                    assert_eq!(notice, "msg.no_next_file");
                    return walked;
                }
                assert!(walked.len() < 20, "{walked:?}");
            }
        };
        let s = |rel: &str, n: Option<usize>| (rel.to_string(), n);
        assert_eq!(
            walk(&mut v),
            [
                s("added.txt", Some(1)),
                s("bin.dat", None),
                s("img.png", None),
                s("multi.txt", Some(1)),
                s("multi.txt", Some(2)),
                s("multi.txt", Some(3)),
                s("only-left.txt", Some(1)),
                s("removed.txt", Some(1)),
            ]
        );
        // A compare stopped part way says so at the end of what it listed.
        v.stopped = true;
        v.step_diff(true, &cfg);
        assert_eq!(v.take_notice(), Some("msg.no_more_files_stopped"));
        v.stopped = false;

        // Since the compare, multi.txt became the same on both sides and only-left.txt went:
        // the first shows no differences, the second fails to load, and steps go on past both.
        let right_multi = dir.path().join("right/multi.txt");
        std::fs::copy(&right_multi, dir.path().join("left/multi.txt")).unwrap();
        std::fs::remove_file(dir.path().join("left/only-left.txt")).unwrap();
        let walked = walk(&mut v);
        assert_eq!(walked.len(), 6, "{walked:?}");
        assert_eq!(
            walked[3..],
            [
                s("multi.txt", None),
                s("only-left.txt", None),
                s("removed.txt", Some(1)),
            ]
        );
        // A docked file changed while shown is loaded again at the same difference.
        assert_eq!(
            v.nav_label(&cfg).as_deref(),
            Some("File 6 / 6 · Difference 1 / 1")
        );
        let same = "same\n".repeat(12);
        std::fs::write(
            dir.path().join("left/removed.txt"),
            format!("a\nb\n{same}c\n"),
        )
        .unwrap();
        std::fs::write(dir.path().join("right/removed.txt"), format!("a\n{same}")).unwrap();
        std::thread::sleep(DOCK_RECHECK + Duration::from_millis(100));
        settle(&mut v);
        assert_eq!(
            v.nav_label(&cfg).as_deref(),
            Some("File 6 / 6 · Difference 1 / 2")
        );
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
        while v.poll_file_job() {
            std::thread::sleep(Duration::from_millis(10));
        }
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

        v.read_only[0] = true;
        v.selected = BTreeSet::from([nested]);
        v.request_op_copy(1, 0, &cfg);
        v.request_op(OpKind::Delete { side: Some(0) }, &cfg);
        assert!(v.confirm.is_none());
        let Some(FolderAction::OpenFiles {
            read_only,
            descriptions,
            ..
        }) = v.open_action(nested, None)
        else {
            panic!("a file opens");
        };
        assert_eq!(read_only, [true, false]);
        let rel = v.items[nested].rel_path();
        assert!(descriptions[0]
            .as_deref()
            .is_some_and(|d| d.ends_with(&rel)));
        assert!(descriptions[1].is_none());
        v.read_only[0] = false;

        v.confirm = Some(PendingOp {
            kind: OpKind::Delete { side: None },
            targets: vec![nested],
        });
        v.merge_plan = Some(folder_merge::Plan::default());
        assert!(v.has_dialog());

        let stats = v.statistics();
        let get = |key: &'static str| stats.iter().find(|(k, _)| k == tr(key)).unwrap().1.clone();
        // Every identical file of the samples is counted, whatever the samples hold.
        let identical = v
            .items
            .iter()
            .filter(|it| !it.is_dir && it.status == CmpStatus::Identical)
            .count();
        assert!(identical >= 1);
        assert_eq!(get("stats.identical"), identical.to_string());

        v.start_compare();
        assert!(!v.has_dialog());
    }
}
