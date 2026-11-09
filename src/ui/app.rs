//! Main application window: menus, tabs, dialogs and command dispatch.

use crate::config::{Config, SessionTab, TextLayout, ThemeMode, Workspace};
use crate::core::archive;
use crate::core::conflict;
use crate::core::diff::{DiffAlgorithm, DiffOptions, InlineMode, WhitespaceMode};
use crate::core::filter::{self, FltFilter, FLT_PREFIX};
use crate::core::folder::CompareMethod;
use crate::core::patch::{self, PatchStyle};
use crate::core::project::{self, Project};
use crate::core::remote;
use crate::core::remote_sync;
use crate::core::report::{self, side_name, ReportFormat};
use crate::core::rules::{self, RuleDraft, RuleKind, RuleSet};
use crate::core::saved::{self, SavedCompare};
use crate::core::text::{self, FileKind, TextDoc};
use crate::core::unpacker::{self, Unpacker};
use crate::core::vcs;
use crate::core::web;
use crate::i18n::{self, tr, trf};
use crate::ui::binary_view::BinaryView;
use crate::ui::commands::{self, Cmd, Scope, BINDINGS};
use crate::ui::diffs_view::DiffsView;
use crate::ui::folder_view::{FolderAction, FolderView};
use crate::ui::image_view::ImageView;
use crate::ui::palette::{Entry as PaletteEntry, Outcome as PaletteOutcome, Palette};
use crate::ui::text_view::TextView;
use crate::ui::web_view::WebView;
use crate::ui::widgets::{self, pal, Icon, Outcome};
use crate::ui::{about, fonts, help, theme};
use egui::{vec2, Align, Align2, CornerRadius, FontId, Key, Layout, Margin, Sense, Stroke};
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub struct OpenRequest {
    pub paths: Vec<PathBuf>,
    pub descriptions: Vec<Option<String>>,
    pub read_only: Vec<bool>,
    pub missing: Vec<bool>,
    pub kind: Option<FileKind>,
    pub recursive: Option<bool>,
    pub filter: Option<String>,
    pub method: Option<CompareMethod>,
    pub output: Option<PathBuf>,
    /// Not added to the recent comparisons (opened from a folder compare, or a restored session).
    pub skip_recent: bool,
    /// Compare the single path with how it was in this Git revision.
    pub git_rev: Option<String>,
    /// Compare rules file to use instead of the one of the options ("" = none).
    pub rules: Option<String>,
    pub snapshot_options: Option<Box<DiffOptions>>,
    /// How a table compare matches records.
    pub table: Option<crate::core::table_match::TableMatch>,
}

pub enum Content {
    Folder(FolderView),
    Text(TextView),
    Binary(BinaryView),
    Image(ImageView),
    Web(WebView),
    Diffs(DiffsView),
}

impl Content {
    /// The view that receives commands: what a web page tab currently shows, otherwise itself.
    fn view(&self) -> &Content {
        match self {
            Content::Web(WebView {
                inner: Some(inner), ..
            }) => inner,
            other => other,
        }
    }

    fn view_mut(&mut self) -> &mut Content {
        match self {
            Content::Web(WebView {
                inner: Some(inner), ..
            }) => inner,
            other => other,
        }
    }

    fn status_line(&self) -> String {
        match self {
            Content::Folder(v) => v.status_line(),
            Content::Text(v) => v.status_line(),
            Content::Binary(v) => v.status_line(),
            Content::Image(v) => v.status_line(),
            Content::Web(v) => v.status_line(v.inner.as_deref().map(Content::status_line)),
            Content::Diffs(v) => v.status_line(),
        }
    }

    /// The status line without what the view's own toolbar already shows.
    fn status_bar_line(&self) -> String {
        match self {
            Content::Folder(v) => v.status_detail(),
            Content::Text(v) => v.status_detail(),
            Content::Web(v) => v.status_line(v.inner.as_deref().map(Content::status_bar_line)),
            other => other.status_line(),
        }
    }

    fn outcome(&self) -> Option<Outcome> {
        match self {
            Content::Folder(v) => Some(v.outcome()),
            Content::Text(v) => Some(v.outcome()),
            Content::Binary(v) => Some(v.outcome()),
            Content::Image(v) => Some(v.outcome()),
            Content::Web(v) => v.outcome(v.inner.as_deref().and_then(Content::outcome)),
            Content::Diffs(v) => Some(v.outcome()),
        }
    }
}

pub struct Tab {
    /// Stays the same while the tab lives, wherever it moves in the tab strip.
    pub id: u64,
    pub content: Content,
    /// Paths used to open this tab (for "compare as" re-opening).
    pub source: Vec<PathBuf>,
    /// User-provided labels, including view types without editable pane headers.
    descriptions: Vec<Option<String>>,
    /// The folder compare this tab was opened from, for moving on to its next or previous file.
    origin: Option<Origin>,
    /// Temporary copies of sides (a Git revision, extracted archives, a file or folder fetched
    /// over SFTP), deleted with the last tab showing them: the folder compare and the files
    /// opened from it.
    temps: Vec<std::sync::Arc<tempfile::TempDir>>,
    /// The connection and remote path of a side staged from [`remote::fetch`], one slot per
    /// `source`, for *Save* to write it back to; `None` for a side opened any other way.
    remote: Vec<Option<RemoteFile>>,
}

/// A tab's side backed by a file or folder fetched over SFTP or FTP: enough to write it back
/// on *Save*, kept only in memory, for as long as its tab is open.
#[derive(Clone)]
struct RemoteFile {
    conn: remote::RemoteConnection,
    /// The file's or folder's path on the server.
    remote_path: String,
    /// For a folder, what it held as fetched or last saved, to write back only what changed
    /// since (see [`remote_sync`]); `None` for a file.
    manifest: Option<remote::Manifest>,
    /// How the file was on the server when fetched or last uploaded, so an upload does not
    /// write over a change someone else made there meanwhile.
    stamp: Option<remote::RemoteStamp>,
    /// A hash of the local file's bytes as last fetched or uploaded, so a save that touches
    /// another side of the same tab does not upload this one again with nothing new to send.
    sent: Option<u64>,
    /// Saved here but not on the server yet (the upload failed, was stopped, or is under way):
    /// the tab counts as modified, so closing it asks first and *Save* tries again.
    pending: bool,
}

/// A hash of the file at `path`'s bytes, to tell whether it changed since it was last sent.
fn file_hash(path: &Path) -> Option<u64> {
    remote::file_hash(path)
}

/// A file or folder side to upload to the server it was fetched from.
struct UploadJob {
    tab: u64,
    side: usize,
    conn: remote::RemoteConnection,
    local: PathBuf,
    remote_path: String,
    expected: Option<remote::RemoteStamp>,
    /// For a file, the hash of what the server had as fetched or last uploaded (see
    /// [`remote::upload`]).
    sent: Option<u64>,
    /// For a folder, what the server's folder held as fetched or last saved.
    manifest: Option<remote::Manifest>,
    hash: u64,
    /// The temporary folder `local` is in, kept until the upload is done even if its tab
    /// closes meanwhile.
    _keep: Vec<std::sync::Arc<tempfile::TempDir>>,
}

/// How an [`UploadJob`] ended.
struct UploadDone {
    tab: u64,
    side: usize,
    hash: u64,
    label: String,
    result: anyhow::Result<Uploaded>,
}

/// What an [`UploadJob`] wrote.
enum Uploaded {
    /// A file, now as this stamp says on the server.
    File(remote::RemoteStamp),
    /// What changed in a folder, as far as it could be written.
    Folder(remote_sync::Saved),
}

/// Uploads under way on another thread, one after the other.
struct UploadRun {
    rx: std::sync::mpsc::Receiver<UploadDone>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// What is being uploaded, as shown.
    names: String,
    /// What to do once every upload succeeded: close the tab, or the program, that asked to
    /// save first.
    then: Option<Prompt>,
    /// Whether more tabs wait to be asked about closing after `then`.
    queued: bool,
    errors: Vec<String>,
}

/// An archive slow to compress to write back with [`archive::repack_all`] on another thread:
/// the folder tab and side it is extracted on (`None` for one from a closed tab), the
/// archive file, and the folder it was extracted to.
struct Repack {
    side: Option<(u64, usize)>,
    archive: PathBuf,
    root: PathBuf,
}

/// How writing back a [`Repack`] went: its side, and the archive files written or why not.
type Repacked = (Option<(u64, usize)>, Result<Vec<PathBuf>, String>);

/// Archives slow to compress being written back on another thread, one after the other,
/// before the sides that saving them touched are uploaded.
struct RepackRun {
    rx: std::sync::mpsc::Receiver<Repacked>,
    /// What is being written, as shown.
    names: String,
    done: Vec<Repacked>,
    /// Asked for while these were under way, started once they are done.
    later: Vec<Repack>,
    /// The tabs to upload once done (see [`OxideMergeApp::queue_uploads`]).
    up: Vec<u64>,
    /// What to do once every archive was written and uploaded (see [`UploadRun::then`]).
    then: Option<Prompt>,
    queued: bool,
    /// The temporary folders the archives were extracted to, kept until they are written
    /// even if their tabs close meanwhile.
    _keep: Vec<std::sync::Arc<tempfile::TempDir>>,
}

/// *Reload from server* under way on another thread: each side fetched again, and how.
struct ReloadRun {
    tab: u64,
    rx: std::sync::mpsc::Receiver<(usize, anyhow::Result<remote::Staged>)>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// What is being fetched, as shown.
    names: String,
    done: Vec<(usize, anyhow::Result<remote::Staged>)>,
    /// The temporary folders the sides are in, kept until the fetch is done even if the tab
    /// closes meanwhile.
    _keep: Vec<std::sync::Arc<tempfile::TempDir>>,
}

/// The sides of `tab` that came from a server and are still read from it (not moved to a local
/// path of their own by *Save As*): side, connection, path on the server, and local copy.
fn reload_sides(tab: &Tab) -> Vec<(usize, remote::RemoteConnection, String, PathBuf)> {
    let paths = tab.paths();
    tab.remote
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let r = r.as_ref()?;
            let local = paths.get(i).filter(|p| tab.source.get(i) == Some(*p))?;
            Some((i, r.conn.clone(), r.remote_path.clone(), local.clone()))
        })
        .collect()
}

/// A fetch that stopped at a server whose key is not trusted yet, waiting for the user to check
/// its fingerprint.
struct HostPrompt {
    host: remote::UnknownHost,
    target: RemoteTarget,
    conn: remote::RemoteConnection,
    remote_path: String,
    dialog: Option<RemoteDialog>,
    /// Stopped listing the server's folders for `dialog` (see [`RemoteBrowse`]), not fetching:
    /// trusting the server lists `remote_path` again instead.
    browse: bool,
}

/// A fetch staged by the remote connection dialog, waiting to be claimed by the tab it ends up
/// opening in (matched on the local path it was staged to, by [`OxideMergeApp::push_tab`]).
struct PendingRemote {
    local: PathBuf,
    temp: std::sync::Arc<tempfile::TempDir>,
    file: RemoteFile,
}

#[derive(Clone)]
struct Origin {
    /// Id of the folder compare tab.
    folder: u64,
    /// Relative path of the item in that folder compare.
    rel: String,
    /// Each side's root as it was in the folder compare when this tab was opened from it,
    /// and the archive file to repack it into, for a side that was an archive side. Used to
    /// save this tab's archive even after the folder tab it came from was closed, when there
    /// is no live `FolderView` left to ask; `archives[s]` is `None` for a plain folder side.
    roots: Vec<PathBuf>,
    archives: Vec<Option<PathBuf>>,
}

impl Tab {
    fn project(&self) -> Option<Project> {
        if self.source.len() < 2 {
            return None;
        }
        let mut proj = Project {
            paths: self
                .source
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            descriptions: self.descriptions.clone(),
            ..Default::default()
        };
        match &self.content {
            Content::Folder(v) => {
                let (recursive, method) = v.scan_settings();
                proj.read_only = v.read_only.clone();
                proj.filter = Some(v.filter_spec.clone());
                proj.recursive = Some(recursive);
                proj.method = Some(method);
                proj.rules = Some(v.rules.clone());
            }
            Content::Text(v) => {
                proj.paths = v
                    .panes
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        p.doc
                            .path
                            .as_ref()
                            .unwrap_or(&self.source[i])
                            .display()
                            .to_string()
                    })
                    .collect();
                proj.descriptions = v.panes.iter().map(|p| p.doc.description.clone()).collect();
                proj.read_only = v.panes.iter().map(|p| p.doc.read_only).collect();
                proj.kind = Some(kind_name(FileKind::Text).into());
                proj.rules = v.rules.clone();
                proj.table = Some(v.table_match.clone()).filter(|m| m.by_key());
            }
            Content::Binary(v) => {
                proj.kind = Some(kind_name(FileKind::Binary).into());
                proj.read_only = v.read_only.clone();
            }
            Content::Image(v) => {
                proj.kind = Some(kind_name(FileKind::Image).into());
                proj.descriptions = v.descriptions.clone();
                proj.read_only = v.read_only.clone();
            }
            Content::Web(v) => {
                proj.kind = Some(kind_name(FileKind::Web).into());
                proj.read_only = vec![true; v.urls.len()];
            }
            Content::Diffs(_) => return None,
        }
        Some(proj)
    }

    /// The temporary copies files opened from this tab need kept: its own and the extracted
    /// archives its folder compare shows.
    fn shown_temps(&self) -> Vec<std::sync::Arc<tempfile::TempDir>> {
        let mut temps = self.temps.clone();
        match self.content.view() {
            Content::Folder(v) => temps.extend(v.temps().iter().cloned()),
            Content::Diffs(v) => temps.extend(v.temps().iter().cloned()),
            _ => {}
        }
        temps
    }

    /// The files the tab compares: where its text files are now (after Save As), otherwise
    /// what it was opened with.
    fn paths(&self) -> Vec<PathBuf> {
        if let Content::Text(v) = self.content.view() {
            if let Some(paths) = v.panes.iter().map(|p| p.doc.path.clone()).collect() {
                return paths;
            }
        }
        self.source.clone()
    }

    fn title(&self) -> String {
        match &self.content {
            Content::Folder(v) => v.title(),
            Content::Text(v) => v.title(),
            Content::Binary(v) => v.title(),
            Content::Image(v) => v.title(),
            Content::Web(v) => v.title(),
            Content::Diffs(v) => v.title(),
        }
    }

    fn icon(&self) -> Icon {
        match &self.content {
            Content::Folder(_) => Icon::Folder,
            Content::Text(_) => Icon::Text,
            Content::Binary(_) => Icon::Binary,
            Content::Image(_) => Icon::Image,
            Content::Web(_) => Icon::Globe,
            Content::Diffs(_) => Icon::Compare,
        }
    }

    fn is_modified(&self) -> bool {
        if self.remote.iter().flatten().any(|f| f.pending) {
            return true;
        }
        match &self.content {
            Content::Folder(v) => v.has_archive_changes() || v.has_remote_changes(),
            Content::Web(_) => false,
            Content::Text(v) => v.is_modified(),
            Content::Binary(v) => v.is_modified(),
            Content::Image(v) => v.is_modified(),
            Content::Diffs(v) => v.is_modified(),
        }
    }

    fn status_line(&self) -> String {
        self.content.status_line()
    }

    /// Saves changes; for a folder compare, repacks every archive side with pending edits
    /// back into its archive file (see [`App::mark_origin_folder_dirty`]), but for those slow
    /// to repack, which [`OxideMergeApp::after_save`] writes on another thread.
    fn save(&mut self) -> anyhow::Result<()> {
        match &mut self.content {
            Content::Folder(v) => match v.save_archives().1 {
                errors if errors.is_empty() => Ok(()),
                errors => anyhow::bail!("{}", errors.join("\n")),
            },
            Content::Web(_) => Ok(()),
            Content::Text(v) => v.save_modified(),
            Content::Binary(v) => v.save(),
            Content::Image(v) => v.save(),
            Content::Diffs(v) => v.save_modified(),
        }
    }
}

fn content_ui(
    content: &mut Content,
    ui: &mut egui::Ui,
    cfg: &Config,
    colors: &theme::Colors,
    named: &[FltFilter],
) -> Option<FolderAction> {
    match content {
        Content::Text(v) => {
            let options = cfg.comparison_options();
            match v.options_for(&options) {
                Some(o) => v.set_options(&o),
                None => v.set_options(&options),
            }
            v.ui(ui, cfg, colors);
            None
        }
        Content::Binary(v) => {
            v.ui(ui, cfg, colors);
            None
        }
        Content::Image(v) => {
            v.ui(ui, cfg, colors);
            None
        }
        Content::Folder(v) => v.ui(ui, cfg, colors, named),
        Content::Diffs(v) => {
            v.ui(ui, cfg, colors);
            v.open_request.take()
        }
        Content::Web(v) => {
            v.header(ui, cfg, named);
            if let Some(f) = &mut v.formatted {
                f.ui(ui, colors, cfg.show_location_pane);
            }
            v.inner
                .as_deref_mut()
                .and_then(|c| content_ui(c, ui, cfg, colors, named))
        }
    }
}

/// What a command palette entry does.
#[derive(Clone)]
enum PaletteAction {
    Cmd(Cmd),
    Compare(Vec<String>),
    Settings(OptTab),
    Theme(ThemeMode),
    Toggle(ConfigFlag),
    Inline,
}

/// Selects one on/off setting.
type ConfigFlag = fn(&mut Config) -> &mut bool;

/// Labels of the file manager integration: add, remove, added, removed.
pub const SHELL_KEYS: [&str; 4] = if cfg!(windows) {
    [
        "menu.shell_register",
        "menu.shell_unregister",
        "msg.shell_registered",
        "msg.shell_unregistered",
    ]
} else if cfg!(target_os = "macos") {
    [
        "menu.finder_register",
        "menu.finder_unregister",
        "msg.finder_registered",
        "msg.finder_unregistered",
    ]
} else {
    [
        "menu.fm_register",
        "menu.fm_unregister",
        "msg.fm_registered",
        "msg.fm_unregistered",
    ]
};

/// Menu labels without the trailing ellipsis or colon.
fn plain(label: &str) -> String {
    label.trim_end_matches(['.', '…', ':', '：']).to_string()
}

struct OpenDialog {
    paths: [String; 3],
    three_way: bool,
    filter: String,
    recursive: bool,
    method: CompareMethod,
    kind: Option<FileKind>,
    read_only: [bool; 3],
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum OptTab {
    General,
    Compare,
    Rules,
    Folder,
    Media,
    Plugins,
}

impl OptTab {
    const ALL: [(OptTab, Icon, &'static str); 6] = [
        (OptTab::General, Icon::Settings, "opt.tab_general"),
        (OptTab::Compare, Icon::Compare, "opt.tab_compare"),
        (OptTab::Rules, Icon::Check, "opt.rules"),
        (OptTab::Folder, Icon::Folder, "opt.tab_folder"),
        (OptTab::Media, Icon::Image, "opt.tab_media"),
        (OptTab::Plugins, Icon::Bolt, "opt.tab_plugins"),
    ];
}

/// Every setting shown in the Options dialog, by the tab that holds it, for search.
const OPT_INDEX: &[(OptTab, &str)] = &[
    (OptTab::General, "menu.language"),
    (OptTab::General, "view.theme"),
    (OptTab::General, "opt.font_size"),
    (OptTab::General, "opt.tab_size"),
    (OptTab::General, "view.text_layout"),
    (OptTab::General, "view.layout_compact"),
    (OptTab::General, "view.layout_aligned"),
    (OptTab::General, "view.line_numbers"),
    (OptTab::General, "view.location_pane"),
    (OptTab::General, "view.whitespace"),
    (OptTab::General, "view.syntax"),
    (OptTab::General, "opt.color_blind"),
    (OptTab::General, "opt.high_contrast"),
    (OptTab::General, "opt.reduce_motion"),
    (OptTab::General, "opt.compact"),
    (OptTab::General, "cmd.collapse_unchanged"),
    (OptTab::General, "cmd.word_wrap"),
    (OptTab::General, "opt.backup_on_save"),
    (OptTab::General, "opt.recycle_bin"),
    (OptTab::General, "opt.confirm_ops"),
    (OptTab::General, "opt.restore_session"),
    (OptTab::Compare, "opt.whitespace"),
    (OptTab::Compare, "opt.ws_compare"),
    (OptTab::Compare, "opt.ws_change"),
    (OptTab::Compare, "opt.ws_all"),
    (OptTab::Compare, "opt.ignore_blank"),
    (OptTab::Compare, "opt.ignore_case"),
    (OptTab::Compare, "opt.ignore_eol"),
    (OptTab::Compare, "opt.ignore_numbers"),
    (OptTab::Compare, "opt.ignore_comments"),
    (OptTab::Compare, "opt.prettify"),
    (OptTab::Compare, "opt.ignore_codepage"),
    (OptTab::Compare, "opt.detect_moved"),
    (OptTab::Compare, "opt.algorithm"),
    (OptTab::Compare, "opt.inline"),
    (OptTab::Compare, "opt.numeric_tolerance"),
    (OptTab::Rules, "opt.rules"),
    (OptTab::Rules, "rules.lines"),
    (OptTab::Rules, "rules.substitutions"),
    (OptTab::Rules, "rules.blocks"),
    (OptTab::Folder, "label.method"),
    (OptTab::Folder, "label.recursive"),
    (OptTab::Folder, "opt.unique_subfolders"),
    (OptTab::Folder, "opt.skip_hidden"),
    (OptTab::Folder, "opt.structured_equal"),
    (OptTab::Folder, "opt.time_tolerance"),
    (OptTab::Folder, "opt.threads"),
    (OptTab::Media, "img.threshold"),
    (OptTab::Media, "img.block_size"),
    (OptTab::Media, "opt.bytes_per_line"),
    (OptTab::Plugins, "opt.extract_documents"),
    (OptTab::Plugins, "opt.unpackers_hint"),
    (OptTab::Plugins, "opt.pattern"),
    (OptTab::Plugins, "opt.command"),
];

/// `a => b` lines, as edited in the options dialog.
fn arrow_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(a, b)| format!("{a} => {b}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads `a => b` lines like [`parse_arrow_pairs`], except that a line left as [`arrow_pairs`]
/// wrote it is the pair it was written from, whose patterns may hold " => " themselves.
fn edited_pairs(text: &str, old: &[(String, String)]) -> Vec<(String, String)> {
    text.lines()
        .flat_map(|l| {
            old.iter()
                .find(|(a, b)| format!("{a} => {b}") == l)
                .cloned()
                .map_or_else(|| parse_arrow_pairs(l), |p| vec![p])
        })
        .collect()
}

/// Lines (1-based) of `a => b` text that are not read as one pair: without an arrow, or with
/// more than one " => ".
fn unclear_arrow_lines(text: &str, old: &[(String, String)], both: bool) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .filter(|(_, l)| !old.iter().any(|(a, b)| format!("{a} => {b}") == *l))
        .filter(|(_, l)| {
            let pair = parse_arrow_pairs(l);
            let empty = pair
                .first()
                .is_none_or(|(a, b)| a.is_empty() || (both && b.is_empty()));
            empty || l.matches(" => ").count() > 1
        })
        .map(|(i, _)| i + 1)
        .collect()
}

/// Warns about the lines [`unclear_arrow_lines`] finds.
fn arrow_warning(ui: &mut egui::Ui, lines: &[usize]) {
    if lines.is_empty() {
        return;
    }
    let lines = lines
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    ui.label(
        egui::RichText::new(trf("rules.unclear_lines", &[("lines", lines)]))
            .small()
            .color(pal(ui).warning),
    );
}

/// Reads `a => b` lines. The spaced arrow is looked for first, so a pattern may hold `=>`; spaces
/// around the patterns are kept, as they are part of them.
fn parse_arrow_pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            l.split_once(" => ")
                .or_else(|| {
                    // "a =>" or "=> b": the space on the arrow's side goes with the arrow.
                    let (a, b) = l.split_once("=>")?;
                    Some((
                        a.strip_suffix(' ').unwrap_or(a),
                        b.strip_prefix(' ').unwrap_or(b),
                    ))
                })
                .map(|(a, b)| (a.to_string(), b.to_string()))
        })
        .filter(|(a, _)| !a.is_empty())
        .collect()
}

/// A warning naming the first of `patterns` that is not a valid regular expression; such
/// patterns are left out of the compare.
fn regex_warning(ui: &mut egui::Ui, patterns: impl IntoIterator<Item = String>) {
    thread_local! {
        static VALID: std::cell::RefCell<std::collections::HashMap<String, bool>> = Default::default();
    }
    let valid = |p: &String| {
        VALID.with_borrow_mut(|known| {
            if known.len() > 512 {
                known.clear();
            }
            *known
                .entry(p.clone())
                .or_insert_with(|| regex::Regex::new(p).is_ok())
        })
    };
    let bad = patterns.into_iter().find(|p| !p.is_empty() && !valid(p));
    if let Some(bad) = bad {
        ui.label(
            egui::RichText::new(trf("opt.invalid_regex", &[("pattern", bad)]))
                .small()
                .color(pal(ui).danger),
        );
    }
}

/// Saves the rules file being edited in the options; false, with the reason shown, when it
/// cannot be.
fn save_rule_edit(d: &mut OptionsDialog) -> bool {
    let Some(e) = &d.rule_edit else {
        return true;
    };
    let dir = e
        .original
        .as_ref()
        .and_then(|o| o.path.parent().map(Path::to_path_buf))
        .or_else(rules::dir);
    let Some(dir) = dir else {
        d.rule_error = Some(tr("msg.no_config").into());
        return false;
    };
    if e.unclear() {
        d.rule_error = Some(tr("rules.fix_unclear").into());
        return false;
    }
    let set = e.to_set(&dir);
    let old = e.original.clone();
    let renamed = old.as_ref().filter(|o| o.path != set.path);
    // A new case of the name is the same file where names ignore case (macOS, Windows), unless
    // the folder lists a file of exactly that name.
    let listed = |p: &Path| {
        let name = p.file_name();
        p.parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .is_some_and(|rd| {
                rd.flatten()
                    .any(|e| Some(e.file_name().as_os_str()) == name)
            })
    };
    let same_name = rules::same_name;
    let recased = renamed.is_some_and(|o| same_name(&o.name, &set.name)) && !listed(&set.path);
    // Names that differ in case only would be two files where names keep their case (Linux),
    // of which the list shows one.
    let taken = d
        .rule_sets
        .iter()
        .any(|s| same_name(&s.name, &set.name) && !old.as_ref().is_some_and(|o| o.path == s.path));
    if (old.is_none() || renamed.is_some()) && (taken || (!recased && set.path.exists())) {
        d.rule_error = Some(trf("rules.exists", &[("name", set.name.clone())]));
        return false;
    }
    let moved = match renamed {
        Some(o) if recased => std::fs::rename(&o.path, &set.path).map(|()| Some(o)),
        _ => Ok(None),
    };
    let saved = moved.and_then(|moved| {
        set.save().inspect_err(|_| {
            // The file goes back to its name, which the editor still knows it by.
            if let Some(o) = moved {
                let _ = std::fs::rename(&set.path, &o.path);
            }
        })
    });
    if let Err(err) = saved {
        d.rule_error = Some(err.to_string());
        return false;
    }
    let mut left = None;
    if let Some(o) = renamed {
        if !recased {
            if let Err(err) = std::fs::remove_file(&o.path) {
                let file = o.path.display().to_string();
                left = Some(format!(
                    "{}: {err}",
                    trf("msg.cannot_delete", &[("file", file)])
                ));
            }
        }
        if rules::picks(&d.cfg.rules, o, &d.rule_sets) {
            d.cfg.rules = set.name.clone();
        }
        d.rules_renamed.push((o.clone(), set.name.clone()));
    }
    d.rule_edit = None;
    d.rule_error = left;
    d.rule_discard = false;
    d.rule_sets = rules::list();
    d.rules_touched = true;
    true
}

/// Whether the rules file being edited may be left: unchanged, or left once already since its
/// changes were shown to be unsaved.
fn may_leave_rule_edit(d: &mut OptionsDialog) -> bool {
    if d.rule_discard || !d.rule_edit.as_ref().is_some_and(RuleEdit::changed) {
        d.rule_discard = false;
        return true;
    }
    d.rule_discard = true;
    d.tab = OptTab::Rules;
    d.rule_error = Some(tr("rules.unsaved_edit").into());
    false
}

/// Imports rules files into the rules folder; what happened, and whether some failed.
fn import_rules(files: &[PathBuf]) -> (String, bool) {
    let Some(dir) = rules::dir() else {
        return (tr("msg.no_config").into(), true);
    };
    copied_rules(
        &rules::import_into(&dir, files, &rules::dirs()),
        "rules.imported",
    )
}

/// What copying rules files did, the copies named with `added_key`; and whether some failed.
fn copied_rules(done: &rules::Imported, added_key: &'static str) -> (String, bool) {
    let mut parts = Vec::new();
    for (key, names) in [
        (added_key, &done.added),
        ("rules.present", &done.present),
        ("rules.not_rules", &done.failed),
        ("rules.not_saved", &done.unsaved),
    ] {
        if !names.is_empty() {
            parts.push(trf(key, &[("names", names.join(", "))]));
        }
    }
    let failed = !done.failed.is_empty() || !done.unsaved.is_empty();
    (parts.join(" · "), failed)
}

/// The compare rules files in the options: which one is used by default, and editing them.
fn rules_options(ui: &mut egui::Ui, d: &mut OptionsDialog) {
    ui.label(egui::RichText::new(tr("opt.rules_hint")).small());
    let mut edit: Option<Option<RuleSet>> = None;
    let mut delete: Option<RuleSet> = None;
    let mut import: Vec<PathBuf> = Vec::new();
    // `Some(None)` exports every rules file.
    let mut export: Option<Option<RuleSet>> = None;
    egui::Grid::new("rules_grid")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            if ui.radio(d.cfg.rules.is_empty(), tr("rules.none")).clicked() {
                d.cfg.rules.clear();
            }
            ui.end_row();
            for set in &d.rule_sets {
                let on = rules::picks(&d.cfg.rules, set, &d.rule_sets);
                if ui
                    .radio(on, &set.name)
                    .on_hover_text(set.path.display().to_string())
                    .clicked()
                {
                    d.cfg.rules = set.name.clone();
                }
                let about = if set.description.is_empty() {
                    trf("rules.count", &[("n", set.count().to_string())])
                } else {
                    set.description.clone()
                };
                ui.label(egui::RichText::new(about).small().color(pal(ui).text_dim));
                let editing = d
                    .rule_edit
                    .as_ref()
                    .and_then(|e| e.original.as_ref())
                    .is_some_and(|o| o.path == set.path);
                if ui
                    .add(egui::Button::new(tr("btn.edit")).selected(editing))
                    .clicked()
                {
                    edit = Some(Some(set.clone()));
                }
                let confirming = d.rule_delete.as_deref() == Some(set.name.as_str());
                let label = tr(if confirming {
                    "rules.confirm_delete"
                } else {
                    "cmd.delete"
                });
                if ui.button(label).clicked() {
                    if confirming {
                        delete = Some(set.clone());
                    } else {
                        d.rule_delete = Some(set.name.clone());
                    }
                }
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        if widgets::ghost_icon_button(ui, Icon::Plus, tr("rules.new")).clicked() {
            edit = Some(None);
        }
        if ui.button(tr("rules.open_folder")).clicked() {
            if let Some(dir) = rules::dir() {
                d.rule_error = open_folder(&dir).err();
            }
        }
        if ui.button(tr("filters.reload")).clicked() {
            d.rule_sets = rules::list();
        }
        ui.separator();
        if ui.button(tr("rules.import")).clicked() {
            let files = rfd::FileDialog::new()
                .set_title(tr("rules.import"))
                .add_filter(tr("rules.file_type"), &[rules::EXT])
                .pick_files();
            if let Some(files) = files {
                import = files;
            }
        }
        ui.add_enabled_ui(!d.rule_sets.is_empty(), |ui| {
            ui.menu_button(tr("rules.export"), |ui| {
                widgets::menu_scroll(ui, |ui| {
                    for set in &d.rule_sets {
                        if ui.button(&set.name).clicked() {
                            export = Some(Some(set.clone()));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(tr("rules.export_all")).clicked() {
                        export = Some(None);
                        ui.close();
                    }
                });
            });
        });
    });
    // Another action disarms a delete waiting to be confirmed, and replaces an earlier note.
    if edit.is_some() || !import.is_empty() || export.is_some() {
        d.rule_delete = None;
        d.rule_note = None;
    }
    if !import.is_empty() {
        let (msg, failed) = import_rules(&import);
        if failed {
            (d.rule_error, d.rule_note) = (Some(msg), None);
        } else {
            (d.rule_error, d.rule_note) = (None, Some(msg));
        }
        d.rule_sets = rules::list();
        d.rules_touched = true;
    }
    match export {
        Some(Some(set)) => {
            let path = rfd::FileDialog::new()
                .set_title(tr("rules.export"))
                .add_filter(tr("rules.file_type"), &[rules::EXT])
                .set_file_name(format!("{}.{}", set.name, rules::EXT))
                .save_file();
            if let Some(path) = path {
                match rules::export_to(&set, &path) {
                    Ok(()) => {
                        d.rule_note =
                            Some(trf("msg.saved", &[("file", path.display().to_string())]));
                        d.rule_error = None;
                    }
                    Err(e) => d.rule_error = Some(e.to_string()),
                }
            }
        }
        Some(None) => {
            let dir = rfd::FileDialog::new()
                .set_title(tr("rules.export_all"))
                .pick_folder();
            if let Some(dir) = dir {
                let done = rules::export_all(&d.rule_sets, &dir);
                let (mut msg, failed) = copied_rules(&done, "rules.exported");
                if done.present.is_empty() && !failed {
                    msg = trf(
                        "rules.exported_all",
                        &[
                            ("n", done.added.len().to_string()),
                            ("folder", dir.display().to_string()),
                        ],
                    );
                }
                if failed {
                    (d.rule_error, d.rule_note) = (Some(msg), None);
                } else {
                    (d.rule_error, d.rule_note) = (None, Some(msg));
                }
            }
        }
        None => {}
    }
    if let Some(set) = delete {
        d.rule_delete = None;
        match std::fs::remove_file(&set.path) {
            Ok(()) => {
                if rules::picks(&d.cfg.rules, &set, &d.rule_sets) {
                    d.cfg.rules.clear();
                }
                let path = d
                    .rule_edit
                    .as_ref()
                    .and_then(|e| e.original.as_ref())
                    .map(|o| &o.path);
                if path == Some(&set.path) {
                    d.rule_edit = None;
                }
                d.rule_sets = rules::list();
                d.rules_touched = true;
                d.rules_renamed.push((set, String::new()));
            }
            Err(e) => d.rule_error = Some(e.to_string()),
        }
    }
    if edit.is_some() && d.rule_edit.as_ref().is_some_and(RuleEdit::changed) {
        d.rule_error = Some(tr("rules.editing").into());
        edit = None;
    }
    let started = edit.is_some();
    if let Some(original) = edit {
        let mut name = tr("rules.new_name").to_string();
        let mut k = 2;
        while d.rule_sets.iter().any(|s| rules::same_name(&s.name, &name)) {
            name = format!("{} {k}", tr("rules.new_name"));
            k += 1;
        }
        d.rule_edit = Some(RuleEdit::new(original, &name));
        d.rule_error = None;
        d.rule_discard = false;
    }
    let mut save = false;
    let mut cancel = false;
    let mut edited = false;
    if let Some(e) = &mut d.rule_edit {
        ui.add_space(6.0);
        let editor = egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::Grid::new("rule_edit_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(tr("col.name"));
                    ui.horizontal(|ui| {
                        edited |= ui
                            .add(egui::TextEdit::singleline(&mut e.name).desired_width(320.0))
                            .changed();
                        let saved = rules::file_name(&e.name);
                        if saved != e.name.trim() {
                            ui.label(
                                egui::RichText::new(trf("rules.saved_as", &[("name", saved)]))
                                    .small()
                                    .color(pal(ui).warning),
                            );
                        }
                    });
                    ui.end_row();
                    ui.label(tr("filters.desc"));
                    edited |= ui
                        .add(
                            egui::TextEdit::singleline(&mut e.description)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                    ui.end_row();
                });
            let list = |ui: &mut egui::Ui,
                        title: &'static str,
                        hint: &'static str,
                        text: &mut String,
                        rows| {
                ui.label(egui::RichText::new(tr(title)).strong());
                ui.label(egui::RichText::new(tr(hint)).small());
                ui.add(
                    egui::TextEdit::multiline(text)
                        .code_editor()
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            };
            edited |= list(ui, "rules.lines", "opt.line_filters_hint", &mut e.lines, 3);
            regex_warning(ui, e.lines.lines().map(String::from));
            edited |= list(
                ui,
                "rules.substitutions",
                "opt.substitutions_hint",
                &mut e.substitutions,
                3,
            );
            let old_subs: Vec<(String, String)> = e
                .original
                .iter()
                .flat_map(|o| &o.substitutions)
                .map(|s| (s.find.clone(), s.replace.clone()))
                .collect();
            regex_warning(
                ui,
                edited_pairs(&e.substitutions, &old_subs)
                    .into_iter()
                    .map(|(a, _)| a),
            );
            arrow_warning(ui, &unclear_arrow_lines(&e.substitutions, &old_subs, false));
            edited |= list(
                ui,
                "rules.blocks",
                "opt.block_filters_hint",
                &mut e.blocks,
                2,
            );
            let old_blocks: Vec<(String, String)> = e
                .original
                .iter()
                .flat_map(|o| &o.ignore_blocks)
                .map(|b| (b.start.clone(), b.end.clone()))
                .collect();
            regex_warning(
                ui,
                edited_pairs(&e.blocks, &old_blocks)
                    .into_iter()
                    .flat_map(|(a, b)| [a, b]),
            );
            arrow_warning(ui, &unclear_arrow_lines(&e.blocks, &old_blocks, true));
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, tr("rules.save")).clicked() {
                    save = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if started {
            editor.response.scroll_to_me(Some(Align::Min));
        }
    }
    if edited {
        // The changes shown to be unsaved are not these anymore.
        d.rule_discard = false;
        d.rule_note = None;
    }
    if let Some(err) = &d.rule_error {
        ui.label(
            egui::RichText::new(err.as_str())
                .small()
                .color(pal(ui).danger),
        );
    }
    if let Some(note) = &d.rule_note {
        ui.label(
            egui::RichText::new(note.as_str())
                .small()
                .color(pal(ui).success),
        );
    }
    if save {
        save_rule_edit(d);
    }
    if cancel && may_leave_rule_edit(d) {
        d.rule_edit = None;
        d.rule_error = None;
    }
}

/// Settings whose label contains `query` (case-insensitive), with the tab that holds them.
fn search_options(query: &str) -> Vec<(OptTab, String)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    OPT_INDEX
        .iter()
        .map(|&(t, key)| (t, tr(key).trim_end_matches([':', '.', '…']).to_string()))
        .filter(|(_, label)| label.to_lowercase().contains(&q))
        .collect()
}

struct OptionsDialog {
    cfg: Config,
    tab: OptTab,
    search: String,
    rule_sets: Vec<RuleSet>,
    rule_edit: Option<RuleEdit>,
    /// Rules file waiting for a second click on Delete.
    rule_delete: Option<String>,
    rule_error: Option<String>,
    /// Whether leaving the rules file being edited was refused once, its changes being unsaved.
    rule_discard: bool,
    /// What the last import or export did.
    rule_note: Option<String>,
    /// Whether rules files were saved or deleted.
    rules_touched: bool,
    /// Rules files renamed, with their new names, or deleted ("").
    rules_renamed: Vec<(RuleSet, String)>,
}

/// A compare rules file being edited in the options, its lists as text.
struct RuleEdit {
    /// The file as it was; none for a new one.
    original: Option<RuleSet>,
    name: String,
    description: String,
    lines: String,
    substitutions: String,
    blocks: String,
    /// The texts as the editor opened with them.
    start: [String; 5],
}

impl RuleEdit {
    fn new(original: Option<RuleSet>, name: &str) -> RuleEdit {
        let set = original.clone().unwrap_or_default();
        let pairs = |v: Vec<(String, String)>| arrow_pairs(&v);
        let mut edit = RuleEdit {
            name: original
                .as_ref()
                .map_or(name.to_string(), |s| s.name.clone()),
            description: set.description.clone(),
            lines: set.ignore_lines.join("\n"),
            substitutions: pairs(
                set.substitutions
                    .iter()
                    .map(|s| (s.find.clone(), s.replace.clone()))
                    .collect(),
            ),
            blocks: pairs(
                set.ignore_blocks
                    .iter()
                    .map(|b| (b.start.clone(), b.end.clone()))
                    .collect(),
            ),
            original,
            start: Default::default(),
        };
        edit.start = edit.texts();
        edit
    }

    fn texts(&self) -> [String; 5] {
        [
            self.name.clone(),
            self.description.clone(),
            self.lines.clone(),
            self.substitutions.clone(),
            self.blocks.clone(),
        ]
    }

    fn changed(&self) -> bool {
        self.texts() != self.start
    }

    /// Whether some `a => b` line is not read as one rule.
    fn unclear(&self) -> bool {
        let old = self.original.clone().unwrap_or_default();
        let subs: Vec<(String, String)> = old
            .substitutions
            .into_iter()
            .map(|s| (s.find, s.replace))
            .collect();
        let blocks: Vec<(String, String)> = old
            .ignore_blocks
            .into_iter()
            .map(|b| (b.start, b.end))
            .collect();
        !unclear_arrow_lines(&self.substitutions, &subs, false).is_empty()
            || !unclear_arrow_lines(&self.blocks, &blocks, true).is_empty()
    }

    /// The edited rules, as a file in `dir` unless it keeps the name it had.
    fn to_set(&self, dir: &Path) -> RuleSet {
        let mut set = RuleSet::new(dir, &self.name);
        // A file whose name is not one made for a new file (say "a:b") keeps it while unchanged.
        if let Some(o) = self
            .original
            .as_ref()
            .filter(|o| o.name == set.name || o.name == self.name.trim())
        {
            set.name = o.name.clone();
            set.path = o.path.clone();
        }
        set.description = self.description.trim().to_string();
        set.ignore_lines = self
            .lines
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect();
        let old = self.original.clone().unwrap_or_default();
        let subs: Vec<(String, String)> = old
            .substitutions
            .into_iter()
            .map(|s| (s.find, s.replace))
            .collect();
        set.substitutions = edited_pairs(&self.substitutions, &subs)
            .into_iter()
            .map(|(find, replace)| rules::Substitution { find, replace })
            .collect();
        let blocks: Vec<(String, String)> = old
            .ignore_blocks
            .into_iter()
            .map(|b| (b.start, b.end))
            .collect();
        set.ignore_blocks = edited_pairs(&self.blocks, &blocks)
            .into_iter()
            .filter(|(_, end)| !end.is_empty())
            .map(|(start, end)| rules::Block { start, end })
            .collect();
        set
    }
}

/// Adding a rule picked in a compare to a compare rules file.
struct RuleDialog {
    /// Tab the rule was picked in.
    tab: u64,
    draft: RuleDraft,
    names: Vec<String>,
    /// Rules file to add to (a name or path); a new one when there is no such file.
    target: String,
}

fn open_folder(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::ui::folder_view::system_open(dir)
}

/// Line or item counts of the active compare.
struct StatsDialog {
    columns: Vec<String>,
    rows: Vec<(String, Vec<String>)>,
}

type CommitsRx = std::sync::mpsc::Receiver<Vec<(String, String)>>;

/// A log of recent revisions being read on another thread. Dropping it (when a newer path's log
/// replaces it, or the dialog closes) stops its `svn` or `hg` process.
struct LogJob {
    path: String,
    rx: CommitsRx,
    started: std::time::Instant,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for LogJob {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// What the revision dialog does about its list on this frame.
#[derive(Debug, PartialEq)]
enum LogStep {
    /// Nothing to start.
    Idle,
    /// Look again after this long: typing has not paused yet, or an earlier path's log is
    /// still let finish.
    WaitFor(std::time::Duration),
    /// Start this path's log, stopping an earlier path's that is still running.
    Start,
}

/// `pending`: the list is not for the typed path and no log for it runs. `typing`: how long
/// the path has stayed the same. `running`: how long an earlier path's log has run, if one does.
fn log_step(
    pending: bool,
    typing: std::time::Duration,
    running: Option<std::time::Duration>,
) -> LogStep {
    if !pending {
        return LogStep::Idle;
    }
    if typing < GIT_LOG_DELAY {
        return LogStep::WaitFor(GIT_LOG_DELAY - typing);
    }
    match running {
        // let it finish for a while rather than piling up `svn log`s on a slow server
        Some(ran) if ran < STALE_LOG => LogStep::WaitFor(STALE_LOG - ran),
        _ => LogStep::Start,
    }
}

struct GitDialog {
    path: String,
    rev: String,
    /// Recent commits of `commits_for`: short hash and subject.
    commits: Vec<(String, String)>,
    commits_for: String,
    /// Recent revisions being read.
    commits_job: Option<LogJob>,
    /// The path as last typed and when, so `git log` waits until typing pauses.
    typed: (String, std::time::Instant),
    /// Example revisions for the system holding the typed path, found on another thread when
    /// the path changes, since that looks through every parent folder (slow on a network share).
    rev_hint: &'static str,
    /// The path `rev_hint` was found for.
    hint_for: String,
    /// The hint being found, and the path it is for: one at a time, once typing pauses.
    hint_job: Option<(String, std::sync::mpsc::Receiver<&'static str>)>,
}

/// Which path field a remote connection dialog started from, and so which one its fetch fills
/// in once it is done.
#[derive(Clone, Copy)]
enum RemoteTarget {
    /// A side of the start page's compare form.
    Start(usize),
    /// A side of the Open dialog.
    Open(usize),
}

/// How the remote connection dialog signs in. FTP has only a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignIn {
    Password,
    /// A private key file and its passphrase.
    Key,
    /// The keys `ssh-agent` holds, and those of `~/.ssh/config` or `~/.ssh/id_*`.
    Agent,
}

/// *File → Compare over SFTP or FTP*: a protocol, a server, a user name, how to sign in, and
/// the file or folder to fetch from it.
struct RemoteDialog {
    target: RemoteTarget,
    protocol: remote::Protocol,
    host: String,
    port: String,
    user: String,
    sign_in: SignIn,
    password: String,
    key_path: String,
    passphrase: String,
    remote_path: String,
    /// The server's folders and files, shown to pick `remote_path` from.
    browse: Option<RemoteBrowse>,
    /// A proxy to connect through, and its password when the URL has a user.
    proxy: String,
    proxy_password: String,
    /// FTP in active mode.
    active: bool,
    /// FTPS: a client certificate, and its key when not in the same file.
    client_cert: String,
    client_key: String,
}

impl RemoteDialog {
    /// The proxy the field names, if any.
    fn proxy(&self) -> anyhow::Result<Option<crate::core::net::Proxy>> {
        if self.proxy.trim().is_empty() {
            return Ok(None);
        }
        let mut proxy = crate::core::net::Proxy::parse(&self.proxy)?;
        if !self.proxy_password.is_empty() {
            proxy.password = Some(self.proxy_password.clone());
        }
        Ok(Some(proxy))
    }

    /// Moves a password typed into the proxy URL to its own field, where it is not shown and is
    /// wiped with the dialog.
    fn hide_proxy_password(&mut self) {
        let Ok(mut proxy) = crate::core::net::Proxy::parse(&self.proxy) else {
            return;
        };
        let Some(mut password) = proxy.password.take() else {
            return;
        };
        if self.proxy_password.is_empty() {
            self.proxy_password = password;
        } else {
            remote::wipe(&mut password);
        }
        let mut typed = std::mem::replace(&mut self.proxy, proxy.url());
        remote::wipe(&mut typed);
    }

    /// The connection the fields describe, once every one it needs is filled in. A user name
    /// is optional over SFTP, where `~/.ssh/config` or the local user's name stands in.
    fn conn(&self) -> Option<remote::RemoteConnection> {
        let port = self.port.trim().parse().ok()?;
        let sign_in = self.sign_in();
        if self.host.trim().is_empty()
            || (self.user.trim().is_empty() && self.protocol.is_ftp())
            || (sign_in == SignIn::Key && self.key_path.trim().is_empty())
        {
            return None;
        }
        let tls = matches!(
            self.protocol,
            remote::Protocol::Ftps | remote::Protocol::FtpsImplicit
        );
        let file =
            |text: &str| (tls && !text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        let options = remote::ConnOptions {
            proxy: self.proxy().ok()?,
            active: self.protocol.is_ftp() && self.active,
            client_cert: file(&self.client_cert),
            client_key: file(&self.client_key).filter(|_| !self.client_cert.trim().is_empty()),
            port_given: false,
        };
        let auth = match sign_in {
            SignIn::Key => remote::Auth::Key {
                path: PathBuf::from(self.key_path.trim()),
                passphrase: self.passphrase.clone(),
            },
            SignIn::Agent => remote::Auth::Agent,
            SignIn::Password => remote::Auth::Password(self.password.clone()),
        };
        Some(remote::RemoteConnection {
            protocol: self.protocol,
            host: self.host.trim().to_string(),
            port,
            user: self.user.trim().to_string(),
            auth,
            options,
        })
    }

    /// How the dialog signs in: as chosen over SFTP, and always with a password over FTP.
    fn sign_in(&self) -> SignIn {
        if self.protocol.is_ftp() {
            SignIn::Password
        } else {
            self.sign_in
        }
    }

    /// Switches to `protocol`, and to its usual port unless another was typed.
    fn set_protocol(&mut self, protocol: remote::Protocol) {
        let port = self.port.trim();
        if port.is_empty() || port == self.protocol.default_port().to_string() {
            self.port = protocol.default_port().to_string();
        }
        self.protocol = protocol;
    }
}

impl Drop for RemoteDialog {
    fn drop(&mut self) {
        remote::wipe(&mut self.password);
        remote::wipe(&mut self.passphrase);
        remote::wipe(&mut self.proxy_password);
        remote::wipe(&mut self.proxy);
    }
}

/// The remote connection dialog's less common settings, folded away until opened: a proxy,
/// FTP's active mode, an FTPS client certificate, and a word on `~/.ssh/config` for SFTP.
fn remote_advanced(ui: &mut egui::Ui, d: &mut RemoteDialog) {
    let set = !d.proxy.trim().is_empty() || d.active || !d.client_cert.trim().is_empty();
    egui::CollapsingHeader::new(tr("label.remote_advanced"))
        .id_salt("remote_advanced")
        .default_open(set)
        .show(ui, |ui| {
            egui::Grid::new("remote_advanced_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(tr("label.remote_proxy"));
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut d.proxy)
                            .hint_text("socks5://host:1080")
                            .desired_width(260.0),
                    );
                    if field.lost_focus() {
                        d.hide_proxy_password();
                    }
                    ui.end_row();
                    if !d.proxy.trim().is_empty() {
                        ui.label(tr("label.remote_proxy_password"));
                        ui.add(
                            egui::TextEdit::singleline(&mut d.proxy_password)
                                .password(true)
                                .desired_width(220.0),
                        );
                        ui.end_row();
                        if let Err(e) = d.proxy() {
                            ui.label("");
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{e:#}"))
                                        .color(ui.visuals().error_fg_color),
                                )
                                .wrap(),
                            );
                            ui.end_row();
                        }
                    }
                    if d.protocol.is_ftp() {
                        ui.label("");
                        ui.checkbox(&mut d.active, tr("label.remote_active"))
                            .on_hover_text(tr("msg.remote_active_hint"));
                        ui.end_row();
                    }
                    if matches!(
                        d.protocol,
                        remote::Protocol::Ftps | remote::Protocol::FtpsImplicit
                    ) {
                        for (label, path) in [
                            ("label.remote_client_cert", &mut d.client_cert),
                            ("label.remote_client_key", &mut d.client_key),
                        ] {
                            ui.label(tr(label));
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(path).desired_width(260.0));
                                if widgets::icon_button(ui, Icon::File, tr("btn.browse_file"))
                                    .clicked()
                                {
                                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                                        *path = p.display().to_string();
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    }
                });
            if !d.protocol.is_ftp() {
                ui.weak(tr("msg.remote_ssh_config_hint"));
            }
        });
}

/// `protocol` as the remote connection dialog lists it.
fn protocol_name(protocol: remote::Protocol) -> &'static str {
    tr(match protocol {
        remote::Protocol::Sftp => "label.protocol_sftp",
        remote::Protocol::Ftps => "label.protocol_ftps",
        remote::Protocol::FtpsImplicit => "label.protocol_ftps_implicit",
        remote::Protocol::Ftp => "label.protocol_ftp",
    })
}

/// Whether `a` and `b` connect to the same server the same way, as the same user, signed in
/// the same way.
fn same_conn(a: &remote::RemoteConnection, b: &remote::RemoteConnection) -> bool {
    a.protocol == b.protocol
        && a.host == b.host
        && a.port == b.port
        && a.user == b.user
        && a.options == b.options
        && match (&a.auth, &b.auth) {
            (remote::Auth::Password(x), remote::Auth::Password(y)) => x == y,
            (remote::Auth::Agent, remote::Auth::Agent) => true,
            (
                remote::Auth::Key {
                    path: p,
                    passphrase: x,
                },
                remote::Auth::Key {
                    path: q,
                    passphrase: y,
                },
            ) => p == q && x == y,
            _ => false,
        }
}

/// The remote connection dialog's list of a folder on the server, to pick the remote path from
/// instead of typing it. Its connection stays open while the list is shown.
struct RemoteBrowse {
    browser: remote::Browser,
    /// What `browser` connects with: changing any of the dialog's fields closes the list.
    conn: remote::RemoteConnection,
    /// The path last asked for.
    asked: String,
    listing: Option<remote::Listing>,
    /// Whether `asked` is being listed.
    loading: bool,
    error: Option<String>,
    /// The path field when `asked` was asked for: its listing fills the field in only if the
    /// user did not change it meanwhile (picking a file, say).
    field: String,
}

impl RemoteBrowse {
    fn start(conn: remote::RemoteConnection, path: &str) -> Self {
        let mut b = Self {
            browser: remote::Browser::start(conn.clone()),
            conn,
            asked: String::new(),
            listing: None,
            loading: false,
            error: None,
            field: String::new(),
        };
        b.list(path, path);
        b
    }

    fn list(&mut self, path: &str, field: &str) {
        self.asked = path.to_string();
        self.field = field.to_string();
        self.loading = true;
        self.browser.list(path);
    }
}

/// Everything but the password or passphrase of the last connection the remote connect
/// dialog succeeded with, to fill in a later one with: fetching a second path from the same
/// server, most often the other side of a compare, should not need host, port, user and key
/// path retyped. The password or a key's passphrase is never kept here, consistent with
/// [`RemoteConnection`] itself never outliving the tab it was used for.
struct LastRemote {
    protocol: remote::Protocol,
    host: String,
    port: String,
    user: String,
    sign_in: SignIn,
    key_path: String,
    /// The proxy, without its password.
    proxy: String,
    active: bool,
    client_cert: String,
    client_key: String,
}

impl LastRemote {
    fn from_conn(conn: &remote::RemoteConnection) -> Self {
        let (sign_in, key_path) = match &conn.auth {
            remote::Auth::Password(_) => (SignIn::Password, String::new()),
            remote::Auth::Key { path, .. } => (SignIn::Key, path.display().to_string()),
            remote::Auth::Agent => (SignIn::Agent, String::new()),
        };
        let shown = |p: &Option<PathBuf>| {
            p.as_ref()
                .map_or_else(String::new, |p| p.display().to_string())
        };
        Self {
            protocol: conn.protocol,
            host: conn.host.clone(),
            port: conn.port.to_string(),
            user: conn.user.clone(),
            sign_in,
            key_path,
            proxy: conn
                .options
                .proxy
                .as_ref()
                .map_or_else(String::new, |p| p.url()),
            active: conn.options.active,
            client_cert: shown(&conn.options.client_cert),
            client_key: shown(&conn.options.client_key),
        }
    }
}

/// An archive being opened that is encrypted: its password is asked for, then what was being
/// opened is opened again.
struct ArchivePrompt {
    req: OpenRequest,
    archive: PathBuf,
    /// Whether the password given before was wrong.
    wrong: bool,
    password: zeroize::Zeroizing<String>,
    /// Whether the field still needs focus (only the first frame it shows).
    focus: bool,
}

/// A server's questions while signing in, put to the user in a window of their own (see
/// [`install_asker`]).
struct AskRequest {
    challenge: remote::Challenge,
    answers: Vec<String>,
    reply: std::sync::mpsc::Sender<Option<Vec<String>>>,
    /// Set once the connection stopped waiting (it was cancelled), closing the window.
    gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Whether the first answer's field still needs focus (only the first frame it shows).
    focus: bool,
}

impl Drop for AskRequest {
    fn drop(&mut self) {
        for a in &mut self.answers {
            remote::wipe(a);
        }
    }
}

/// Puts a server's questions while signing in to the user, in a window, for connections made
/// on other threads (see [`remote::set_asker`]); the window shows once the receiver returned is
/// read.
fn install_asker(ctx: &egui::Context) -> std::sync::mpsc::Receiver<AskRequest> {
    let (ask, rx) = asker(ctx);
    remote::set_asker(Some(ask));
    rx
}

/// What [`install_asker`] installs, and the receiver its questions come through. Asked on the
/// window's own thread, which must not wait for itself, a question is declined.
fn asker(
    ctx: &egui::Context,
) -> (
    std::sync::Arc<remote::Asker>,
    std::sync::mpsc::Receiver<AskRequest>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<AskRequest>();
    let ctx = ctx.clone();
    let ui_thread = std::thread::current().id();
    let tx = std::sync::Mutex::new(tx);
    let ask = std::sync::Arc::new(
        move |challenge: &remote::Challenge, cancel: &std::sync::atomic::AtomicBool| {
            if std::thread::current().id() == ui_thread {
                return None;
            }
            let (reply, answered) = std::sync::mpsc::channel();
            let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            tx.lock()
                .ok()?
                .send(AskRequest {
                    answers: vec![String::new(); challenge.prompts.len()],
                    challenge: challenge.clone(),
                    reply,
                    gone: gone.clone(),
                    focus: true,
                })
                .ok()?;
            ctx.request_repaint();
            let answers = loop {
                match answered.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(answers) => break answers,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        if !cancel.load(std::sync::atomic::Ordering::Relaxed) => {}
                    Err(_) => break None,
                }
            };
            gone.store(true, std::sync::atomic::Ordering::Relaxed);
            ctx.request_repaint();
            answers
        },
    );
    (ask, rx)
}

/// A file or folder being fetched over SFTP, on another thread, for [`RemoteDialog`].
struct RemoteLoad {
    target: RemoteTarget,
    conn: remote::RemoteConnection,
    remote_path: String,
    rx: std::sync::mpsc::Receiver<anyhow::Result<remote::Fetched>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The dialog it was started from, shown again when fetching fails or is stopped.
    dialog: Option<RemoteDialog>,
}

/// A revision being read for *Compare with revision*, on another thread.
struct RevLoad {
    path: PathBuf,
    rev: String,
    rx: std::sync::mpsc::Receiver<anyhow::Result<(tempfile::TempDir, PathBuf)>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The dialog it was started from, shown again when reading fails or is stopped.
    dialog: Option<GitDialog>,
}

/// How long the Git dialog's path must stay unchanged before its commits are read.
const GIT_LOG_DELAY: std::time::Duration = std::time::Duration::from_millis(300);
/// How long a log read for an earlier path may run before a new path's starts anyway, so a
/// Subversion server that does not answer cannot hold up the list for every later path.
const STALE_LOG: std::time::Duration = std::time::Duration::from_secs(2);

struct PatchDialog {
    style: PatchStyle,
    context: usize,
    path: String,
}

#[derive(Clone, Copy)]
/// Questions about unsaved changes; tabs are named by id, as others may close meanwhile.
enum Prompt {
    CloseTab(u64),
    /// Replace the tab with the next (true) or previous (false) file of its folder compare.
    SwitchFile(u64, bool, bool),
    Exit,
}

/// How long an information toast stays up; error toasts stay until they are closed.
const TOAST_SECS: f64 = 4.0;
/// Toasts offering to undo an edit stay longer, to leave time to reach the button.
/// Folder compares that take at least this long announce when they finish unattended.
const LONG_COMPARE: std::time::Duration = std::time::Duration::from_secs(5);
const UNDO_TOAST_SECS: f64 = 8.0;
/// The most workspaces listed in the *Workspaces* menu; *Manage workspaces...* lists them all.
const MAX_WORKSPACE_MENU: usize = 15;
/// The longest name a workspace can be given.
const MAX_WORKSPACE_NAME: usize = 80;

struct Toast {
    text: String,
    error: bool,
    /// When the toast appeared, or when the pointer last left it, in `egui::InputState::time`.
    shown: Option<f64>,
    undo: Option<ToastUndo>,
}

/// What a toast's Undo button puts back.
#[derive(Clone, PartialEq)]
enum ToastUndo {
    /// The last edit of this tab.
    Tab(u64),
    /// These recent comparisons, after they were cleared.
    Recent(Vec<Vec<String>>),
    /// This workspace, deleted from this index of [`Config::workspaces`].
    Workspace(usize, Workspace),
}

pub struct OxideMergeApp {
    baseline_start: Box<super::baseline_panel::BaselinePanel>,
    /// Folder synchronizations that did not finish, found at startup.
    sync_recovery: Option<super::sync_dialog::Recovery>,
    cfg: Config,
    saved_cfg: Config,
    /// The config with the options of the command line, and as loaded without them.
    cli_options: Option<(Config, Config)>,
    tabs: Vec<Tab>,
    active: usize,
    open_dlg: Option<OpenDialog>,
    /// The paths typed on the start page, and whether it asks for a base (3-way).
    start_paths: [String; 3],
    start_three_way: bool,
    options_dlg: Option<OptionsDialog>,
    rule_dlg: Option<RuleDialog>,
    patch_dlg: Option<PatchDialog>,
    stats_dlg: Option<StatsDialog>,
    save_job: Option<crate::ui::job_dialog::SaveJob>,
    run_job: Option<crate::ui::job_dialog::RunJob>,
    git_dlg: Option<GitDialog>,
    rev_load: Option<RevLoad>,
    remote_dlg: Option<RemoteDialog>,
    remote_load: Option<RemoteLoad>,
    host_prompt: Option<HostPrompt>,
    upload: Option<UploadRun>,
    /// Uploads asked for while others were under way, started once they are done.
    upload_queue: Vec<UploadJob>,
    repack: Option<RepackRun>,
    /// The tab whose *Reload from server* waits for the user to agree to lose its changes.
    reload_prompt: Option<u64>,
    reload: Option<ReloadRun>,
    /// Fetches staged by the remote connection dialog, waiting for the tab they end up
    /// opening in to claim them (see [`PendingRemote`]).
    pending_remote: Vec<PendingRemote>,
    /// Host, port, user and key path of the last successful connection, to fill in a new
    /// connect dialog with (see [`LastRemote`]). Kept only for this run; never saved.
    last_remote: Option<LastRemote>,
    /// A server's questions while signing in (see [`remote::set_asker`]), from connections on
    /// other threads, and the one being answered.
    asks: Option<std::sync::mpsc::Receiver<AskRequest>>,
    ask: Option<AskRequest>,
    archive_password: Option<ArchivePrompt>,
    /// The archives given a password while opening, whose passwords are forgotten once it
    /// opened or was given up on (extractions keep their own).
    archive_passwords: Vec<PathBuf>,
    /// A folder report with file differences being written on another thread: its file.
    report_job: Option<(PathBuf, std::sync::mpsc::Receiver<Result<(), String>>)>,
    /// Whether the report being written opens in the system's viewer once done, to be printed.
    report_opens: bool,
    /// The PDF files written to be printed, readable only by the user; deleted when the
    /// program ends.
    prints: Vec<tempfile::TempPath>,
    /// Ctrl+Tab presses taken from the input before egui saw them.
    tab_keys: Vec<Cmd>,
    filters_dlg: bool,
    /// The name being typed into the "Save as workspace..." dialog, while open, and whether
    /// its field still needs focus (only true the first frame it shows).
    workspace_save_dlg: Option<(String, bool)>,
    manage_workspaces: bool,
    /// The clipboard being read for [`Cmd::CompareClipboard`].
    clipboard_read: Option<std::sync::mpsc::Receiver<ClipboardRead>>,
    about: bool,
    help: bool,
    named_filters: Vec<FltFilter>,
    prompt: Option<Prompt>,
    /// Tabs with unsaved changes still to ask about after the one prompted, to close them all.
    close_queue: Vec<u64>,
    toasts: Vec<Toast>,
    font_lang: String,
    applied_lang: String,
    /// The dark mode and look the style was last built for.
    visuals_dark: Option<(bool, theme::Look)>,
    allow_close: bool,
    /// When to try saving the settings again after saving them failed.
    settings_retry: Option<f64>,
    /// Tab last scrolled into view in the tab strip.
    shown_active: Option<usize>,
    /// A tab that opened a file without open conflicts, to move on from to the next (`true`) or
    /// previous file with conflicts on the next frame, one file a frame.
    pass_on: Option<(u64, bool)>,
    /// Whether the window had focus on the previous frame; files are checked for outside changes
    /// when it comes back.
    was_focused: bool,
    next_tab_id: u64,
    /// Whether the open tabs are remembered for the next start; not when started to compare
    /// something given on the command line, so a one-off compare keeps the previous session.
    track_session: bool,
    palette: Option<(Palette, Vec<PaletteEntry<PaletteAction>>)>,
    /// Folding and wrapping last applied to the text views, to tell which side changed.
    text_layout: (bool, bool),
    /// `OXIDEMERGE_SMOKE_REPORT`: where to write what the window shows once its compares are
    /// done, then close; and the frames drawn so far.
    smoke: Option<(PathBuf, u32)>,
    /// `OXIDEMERGE_SCREENSHOT`: where to save a picture of the window once its compares are done,
    /// then close; with the command named by `OXIDEMERGE_SCREENSHOT_COMMAND` (its label key, such
    /// as `cmd.sync_folders`) run first; and the frames drawn so far.
    screenshot: Option<Screenshot>,
}

/// Set when the picture `OXIDEMERGE_SCREENSHOT` asks for was not saved.
static SCREENSHOT_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the picture `OXIDEMERGE_SCREENSHOT` asks for was not saved, so that the program
/// can end with an error rather than as if it had been.
pub fn screenshot_failed() -> bool {
    SCREENSHOT_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A picture of the window asked for by `OXIDEMERGE_SCREENSHOT`.
struct Screenshot {
    path: PathBuf,
    command: Option<Cmd>,
    frames: u32,
    /// Set once the picture is asked for; true when it has been saved.
    taken: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

fn kind_name(k: FileKind) -> &'static str {
    match k {
        FileKind::Text => "text",
        FileKind::Binary => "binary",
        FileKind::Image => "image",
        FileKind::Web => "web",
    }
}

fn kind_from_name(s: &str) -> Option<FileKind> {
    [
        FileKind::Text,
        FileKind::Binary,
        FileKind::Image,
        FileKind::Web,
    ]
    .into_iter()
    .find(|&k| kind_name(k) == s)
}

/// The file after (or before) `rel` in folder compare `f` with differences, or with
/// `conflicts`, that both sides changed.
fn adjacent_file(
    f: &FolderView,
    cfg: &Config,
    rel: &str,
    forward: bool,
    conflicts: bool,
) -> Option<usize> {
    if conflicts {
        f.adjacent_conflict_file(cfg, rel, forward)
    } else {
        f.adjacent_different_file(cfg, rel, forward)
    }
}

/// What opening the project file `path` compares.
fn project_request(path: &Path) -> anyhow::Result<OpenRequest> {
    let p = Project::load(path)?;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let base = abs.parent().unwrap_or(Path::new(""));
    Ok(OpenRequest {
        paths: p.resolved_paths(&abs),
        descriptions: p.descriptions.clone(),
        read_only: p.read_only.clone(),
        kind: p.kind.as_deref().and_then(kind_from_name),
        recursive: p.recursive,
        filter: p.filter.clone(),
        method: p.method,
        rules: p.rules.as_deref().map(|r| rules::absolute_spec(r, base)),
        table: p.table.clone(),
        ..Default::default()
    })
}

/// `now`, with each field still as the command line set it (`cli`) put back as it was loaded.
fn unchanged_as_loaded<T>(now: &T, cli: &T, loaded: &T) -> T
where
    T: Clone + serde::Serialize + serde::de::DeserializeOwned,
{
    let as_map = |v: &T| match serde_json::to_value(v) {
        Ok(serde_json::Value::Object(m)) => Some(m),
        _ => None,
    };
    let (Some(mut out), Some(cli), Some(loaded)) = (as_map(now), as_map(cli), as_map(loaded))
    else {
        return now.clone();
    };
    for (k, v) in out.iter_mut() {
        if cli.get(k) == Some(v) {
            if let Some(l) = loaded.get(k) {
                *v = l.clone();
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(out)).unwrap_or_else(|_| now.clone())
}

/// Save dialog filters of report types: name and extensions.
const HTML: (&str, &[&str]) = ("HTML", &["html", "htm"]);
const XML: (&str, &[&str]) = ("XML", &["xml"]);

/// `path` with ".html" added to its name.
fn with_html_ext(path: PathBuf) -> PathBuf {
    let mut name = path.into_os_string();
    name.push(".html");
    name.into()
}

/// An empty editable document for File → New.
fn blank_doc() -> TextDoc {
    let mut d = TextDoc::empty(None);
    d.lines.push(String::new());
    d.final_newline = false;
    d
}

#[cfg(test)]
thread_local! {
    /// Stands in for the real clipboard in tests, so they don't depend on one being available
    /// (or on what happens to be in it) in a CI sandbox. `None` means "use the real clipboard".
    static TEST_CLIPBOARD: std::cell::RefCell<Option<Result<String, String>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_clipboard(text: Result<&str, &str>) {
    TEST_CLIPBOARD.with(|c| {
        *c.borrow_mut() = Some(text.map(String::from).map_err(String::from));
    });
}

/// The most clipboard text compared, in MiB. The clipboard hands over its text whole, so this
/// bounds what is compared and kept, not what is read from it.
const MAX_CLIPBOARD_MIB: usize = 64;

/// What became of reading the clipboard: its text, or why there is none.
type ClipboardRead = Result<String, String>;

/// The system clipboard's current text. It can take seconds (on X11, waiting for the program
/// holding the clipboard), so it is read on a thread of its own.
fn read_clipboard_text() -> ClipboardRead {
    use arboard::Error;
    let mut cb = arboard::Clipboard::new().map_err(|_| tr("msg.clipboard_unavailable"))?;
    cb.get_text().map_err(|e| match e {
        Error::ContentNotAvailable => tr("msg.clipboard_empty").to_string(),
        Error::ClipboardOccupied => tr("msg.clipboard_busy").to_string(),
        Error::ConversionFailure => tr("msg.clipboard_not_text").to_string(),
        Error::ClipboardNotSupported => tr("msg.clipboard_unavailable").to_string(),
        e => trf("msg.clipboard_failed", &[("err", e.to_string())]),
    })
}

/// A new unsaved document holding the clipboard's text, for [`Cmd::CompareClipboard`].
fn clipboard_doc(read: ClipboardRead) -> anyhow::Result<TextDoc> {
    let text = read.map_err(|e| anyhow::anyhow!(e))?;
    if text.is_empty() {
        anyhow::bail!("{}", tr("msg.clipboard_empty"));
    }
    if text.len() > MAX_CLIPBOARD_MIB << 20 {
        anyhow::bail!(
            "{}",
            trf(
                "msg.clipboard_too_large",
                &[("n", MAX_CLIPBOARD_MIB.to_string())]
            )
        );
    }
    let mut doc = TextDoc::from_text(None, &text);
    doc.description = Some(tr("label.clipboard").to_string());
    Ok(doc)
}

fn kind_label(k: Option<FileKind>) -> &'static str {
    match k {
        None => tr("kind.auto"),
        Some(FileKind::Text) => tr("kind.text"),
        Some(FileKind::Binary) => tr("kind.binary"),
        Some(FileKind::Image) => tr("kind.image"),
        Some(FileKind::Web) => tr("kind.web"),
    }
}

/// The kind of view the active tab shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewKind {
    Empty,
    Text,
    Binary,
    Image,
    Folder,
    Web,
    Diffs,
}

/// The menus of the menu bar, by title.
const MENUS: [&str; 7] = [
    "menu.file",
    "menu.edit",
    "menu.view",
    "menu.compare",
    "menu.merge",
    "menu.tools",
    "menu.help",
];

/// What the menus know about the active tab, and what was picked from them.
struct Menus {
    view: ViewKind,
    /// Sides of a text compare (0 otherwise).
    panes: usize,
    /// Sides of a text or folder compare (0 otherwise).
    sides: usize,
    can_take_both: bool,
    /// The text compare shows the inline view, which edits no text (differences still copy).
    inline_view: bool,
    /// The text compare has synchronization points, or a line picked for one.
    has_sync_points: bool,
    /// The tab compares two or more paths.
    has_source: bool,
    /// The folder compare of the tab is still running.
    folder_busy: bool,
    /// Files of the folder compare are still being copied or deleted.
    folder_job: bool,
    /// The tab compares web pages, whichever way it shows them.
    web: bool,
    /// The web page tab shows the pages formatted, with its own differences to step through.
    formatted: bool,
    tabs: usize,
    picked: Option<Cmd>,
    /// Compare rules file picked for the active tab ("" = none).
    rules: Option<String>,
    recent: Option<Vec<String>>,
    clear_recent: bool,
    /// Whether any workspace is saved, for [`Cmd::ManageWorkspaces`] and [`Cmd::OpenWorkspace`].
    has_workspaces: bool,
    /// Whether the tab has a side fetched from a server, and nothing is being sent to or
    /// fetched from one, for [`Cmd::ReloadRemote`].
    can_reload: bool,
}

impl Menus {
    /// Whether `c` does something for the active tab, matching what [`App::run`] handles.
    fn enabled(&self, c: Cmd) -> bool {
        use Cmd::*;
        use ViewKind::{Binary, Diffs, Folder, Image, Text, Web};
        match (c, self.view) {
            (SaveAs(p), _) => p < self.panes,
            (TakeBoth(_), _) => self.can_take_both,
            (SaveProject, _) => self.has_source,
            (SaveJob, Folder) => !self.folder_busy,
            (RunJob, _) => true,
            (Refresh, _) if self.web => true,
            (NextTab | PrevTab, _) => self.tabs > 1,
            (CloseTab | CloseAllTabs | SaveAll, _) => self.tabs > 0,
            (CompareAs(_), Text | Binary | Image | Web) => self.has_source,
            (
                Save | Undo | NextDiff | PrevDiff | FirstDiff | LastDiff | CopyRight | CopyLeft,
                Text | Binary | Image,
            ) => true,
            (
                Redo | Find | FindNext | FindPrev | GotoLine | CopyAllRight | CopyAllLeft,
                Text | Binary,
            ) => true,
            (NextConflict | PrevConflict | AutoMerge, Text | Folder) => self.sides == 3,
            (Replace | Indent(_) | DuplicateLines | MoveLines(_), Text) => !self.inline_view,
            (
                ToggleBookmark | NextBookmark | PrevBookmark | ClearBookmarks | BookmarkComment
                | SaveComparison | SyncPoint,
                Text,
            ) => true,
            (SaveComparison, Folder) => !self.folder_busy,
            (ClearSyncPoints, Text) => self.has_sync_points,
            (
                Find | CopyRight | CopyLeft | SelectPattern | Statistics | NextDiff | PrevDiff
                | FirstDiff | LastDiff | ToggleFilePane,
                Folder,
            ) => true,
            // A report or patch of a folder compare still running would miss files.
            (HtmlReport | Slideshow | ExportPdf | Print, Folder) => !self.folder_busy,
            (Patch, Folder) => self.sides == 2 && !self.folder_busy,
            (SyncFolders, Folder) => self.sides == 2 && !self.folder_busy && !self.folder_job,
            (Patch, Text) => self.sides == 2,
            (HtmlReport | Slideshow | ExportPdf | Print | Statistics, Text) => true,
            (Refresh, Text | Folder | Web) => true,
            (NextDiff | PrevDiff | FirstDiff | LastDiff, Web) => self.formatted,
            (AllDiffs, Folder) => !self.folder_busy,
            (
                Save | Undo | NextDiff | PrevDiff | FirstDiff | LastDiff | CopyRight | CopyLeft
                | Refresh | Find,
                Diffs,
            ) => true,
            (
                New | CompareClipboard | Open | OpenRemote | OpenConflict | OpenProject
                | GitCompare | ApplyPatch | Options | CompareOptions | Filters | EditRules | Help
                | Palette | Exit | ZoomIn | ZoomOut | ZoomReset | ToggleCollapse | ToggleWrap
                | CloseTabAt(_) | CloseOtherTabs(_) | CloseTabsToRight(_) | SaveWorkspace,
                _,
            ) => true,
            (ManageWorkspaces | OpenWorkspace(_), _) => self.has_workspaces,
            (ReloadRemote, _) => self.can_reload,
            (DeleteWorkspace(_), _) => true,
            _ => false,
        }
    }

    /// A menu item that runs `c` with its key, greyed out when it does nothing for the active tab.
    fn item(&mut self, ui: &mut egui::Ui, c: Cmd) {
        self.item_as(ui, tr(c.label()), c);
    }

    fn item_as(&mut self, ui: &mut egui::Ui, label: &str, c: Cmd) {
        if widgets::menu_item(ui, self.enabled(c), label, &c.shown_key()).clicked() {
            self.picked = Some(c);
            ui.close();
        }
    }
}

/// The layer [`OxideMergeApp::menu_width`] lays the menus out on.
const MENU_MEASURE: &str = "menu_measure";

/// A submenu of a menu bar menu, opening as wide as the menus. Laid out by
/// [`OxideMergeApp::menu_width`], it is a plain button that also takes the room of the arrow it
/// has in a menu, followed by its contents, so the widest submenu sets the width of all menus.
fn submenu<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    submenu_measured(ui, title, true, add)
}

/// [`submenu`] for a list of the user's own entries, such as recent paths or rule names, which
/// are not measured: they may make it wider than the other menus, but never the other menus
/// wider, and the menus need not be measured again when they change.
fn submenu_list<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    submenu_measured(ui, title, false, add)
}

fn submenu_measured<R>(
    ui: &mut egui::Ui,
    title: &str,
    contents: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    if ui.layer_id().id != egui::Id::unique(MENU_MEASURE) {
        return widgets::submenu(ui, title, add);
    }
    let shown = ui.menu_button(title, |_| ());
    let arrow = egui::WidgetText::from("⏵")
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Button,
        )
        .size()
        .x;
    let width = shown.response.rect.width() + ui.spacing().item_spacing.x + arrow;
    ui.allocate_exact_size(vec2(width, 0.0), Sense::hover());
    if contents {
        ui.scope(add);
    }
    egui::InnerResponse::new(None, shown.response)
}

/// A menu item turning `on` on or off; see [`widgets::menu_check`].
fn check(ui: &mut egui::Ui, on: &mut bool, label: &str, keys: &str) {
    if widgets::menu_check(ui, *on, label, keys).clicked() {
        *on = !*on;
    }
}

fn edit_menu(ui: &mut egui::Ui, m: &mut Menus) {
    m.item(ui, Cmd::Undo);
    m.item(ui, Cmd::Redo);
    ui.separator();
    for c in [
        Cmd::Find,
        Cmd::Replace,
        Cmd::FindNext,
        Cmd::FindPrev,
        Cmd::GotoLine,
    ] {
        m.item(ui, c);
    }
    ui.separator();
    ui.add_enabled_ui(m.view == ViewKind::Text, |ui| {
        submenu(ui, tr("menu.bookmarks"), |ui| {
            for c in [
                Cmd::ToggleBookmark,
                Cmd::NextBookmark,
                Cmd::PrevBookmark,
                Cmd::BookmarkComment,
            ] {
                m.item(ui, c);
            }
            ui.separator();
            m.item(ui, Cmd::ClearBookmarks);
        });
    });
    m.item(ui, Cmd::Indent(false));
    m.item(ui, Cmd::Indent(true));
    m.item(ui, Cmd::DuplicateLines);
    m.item(ui, Cmd::MoveLines(true));
    m.item(ui, Cmd::MoveLines(false));
    m.item(ui, Cmd::SelectPattern);
    ui.separator();
    m.item(ui, Cmd::Options);
}

fn merge_menu(ui: &mut egui::Ui, m: &mut Menus) {
    for c in [
        Cmd::CopyRight,
        Cmd::CopyLeft,
        Cmd::CopyAllRight,
        Cmd::CopyAllLeft,
    ] {
        m.item(ui, c);
    }
    ui.separator();
    let merge = if m.view == ViewKind::Folder {
        tr("cmd.folder_merge")
    } else {
        tr("cmd.auto_merge")
    };
    m.item_as(ui, merge, Cmd::AutoMerge);
    if m.view == ViewKind::Folder {
        m.item(ui, Cmd::SyncFolders);
    }
    m.item(ui, Cmd::TakeBoth(true));
    m.item(ui, Cmd::TakeBoth(false));
    ui.separator();
    m.item(ui, Cmd::SyncPoint);
    m.item(ui, Cmd::ClearSyncPoints);
}

/// File name of a compared path, or the whole URL.
fn recent_name(s: &str) -> String {
    if web::is_url(s) {
        return s.to_string();
    }
    Path::new(s)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| s.to_string())
}

/// One recent compare on a line: the names of its sides.
fn recent_label(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| recent_name(p))
        .collect::<Vec<_>>()
        .join(" ↔ ")
}

/// What a tab shows for title `full` of a compare of `source`: a name all sides share once,
/// followed by the folders that tell them apart; otherwise `full` alone.
fn tab_label(full: &str, source: &[PathBuf]) -> (String, Option<String>) {
    let parts: Vec<&str> = full.split(" ↔ ").map(|p| p.trim_end_matches('*')).collect();
    if parts.len() < 2 || parts.iter().any(|p| *p != parts[0]) || source.len() != parts.len() {
        return (full.to_string(), None);
    }
    let dirs: Vec<Vec<std::path::Component>> = source
        .iter()
        .map(|p| {
            p.parent()
                .map(|d| d.components().collect())
                .unwrap_or_default()
        })
        .collect();
    let common = (0..)
        .take_while(|&i| {
            dirs.iter()
                .all(|d| d.get(i).is_some() && d[i] == dirs[0][i])
        })
        .count();
    // Rebuilt as paths, so a drive or root left over keeps its own separator.
    let rest: Vec<String> = dirs
        .iter()
        .map(|d| {
            d[common..]
                .iter()
                .collect::<PathBuf>()
                .display()
                .to_string()
        })
        .collect();
    if rest.iter().all(|r| r.is_empty()) {
        // The same file on every side.
        return (full.to_string(), None);
    }
    let folders = rest
        .iter()
        .map(|r| if r.is_empty() { "." } else { r.as_str() })
        .collect::<Vec<_>>()
        .join(" ↔ ");
    (parts[0].to_string(), Some(folders))
}

/// `s`, cut to `max` characters with an ellipsis.
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 2).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Card of the start page's recent list: the compared names, with where they are below.
fn recent_card(ui: &mut egui::Ui, w: f32, paths: &[String]) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 56.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered();
    ui.painter().rect(
        rect,
        theme::radius::LG,
        if hovered { p.surface_alt } else { p.surface },
        Stroke::new(1.0_f32, if hovered { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    let first = paths.first().map(String::as_str).unwrap_or_default();
    let icon = match path_kind(first) {
        PathKind::Url => Icon::Globe,
        PathKind::Dir => Icon::Folder,
        PathKind::File | PathKind::Missing => Icon::File,
    };
    crate::ui::icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            vec2(16.0, 16.0),
        ),
        icon,
        p.text_dim,
    );
    let dir = |s: &String| -> String {
        if web::is_url(s) {
            return String::new();
        }
        Path::new(s)
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
    };
    let names: Vec<String> = paths.iter().map(|p| recent_name(p)).collect();
    let mut dirs: Vec<String> = paths.iter().map(dir).collect();
    dirs.dedup();
    let clip = ui.painter().with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(rect.left() + 30.0, rect.top()),
        egui::pos2(rect.right() - 10.0, rect.bottom()),
    ));
    let x = rect.left() + 36.0;
    let has_dirs = dirs.iter().any(|d| !d.is_empty());
    clip.text(
        egui::pos2(x, rect.center().y - if has_dirs { 8.0 } else { 0.0 }),
        Align2::LEFT_CENTER,
        names.join("  ↔  "),
        theme::semibold(theme::size::CONTROL),
        p.text,
    );
    clip.text(
        egui::pos2(x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        dirs.join("  ↔  "),
        FontId::proportional(theme::size::SMALL),
        p.text_dim,
    );
    widgets::focus_ring(ui, &resp, rect, theme::radius::LG);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, names.join(" ↔ ")));
    resp.on_hover_text(paths.join("\n"))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Marks a path field with what the typed path points to, and outlines it when nothing is there.
/// Margin of the Open dialog's path fields, with room on the right for `path_status`.
const PATH_FIELD_MARGIN: Margin = Margin {
    left: 8,
    right: 28,
    top: 5,
    bottom: 5,
};

#[derive(Clone, Copy, PartialEq, Debug)]
enum PathKind {
    Url,
    Dir,
    File,
    Missing,
}

/// How long [`path_kind`] trusts what it found.
const PATH_KIND_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// What `raw` points to, looked up at most every [`PATH_KIND_TTL`] per path: the start page
/// and the Open dialog are drawn continuously, and a network path can be slow to answer.
fn path_kind(raw: &str) -> PathKind {
    use std::collections::HashMap;
    use std::time::Instant;
    type Cache = HashMap<String, (PathKind, Instant)>;
    static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);
    if web::is_url(raw) {
        return PathKind::Url;
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(&(kind, at)) = cache.get(raw) {
        if at.elapsed() < PATH_KIND_TTL {
            return kind;
        }
    }
    let path = Path::new(raw);
    let kind = if path.is_dir() {
        PathKind::Dir
    } else if path.exists() {
        PathKind::File
    } else {
        PathKind::Missing
    };
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(raw.to_string(), (kind, Instant::now()));
    kind
}

fn path_status(ui: &egui::Ui, field: &egui::Response, text: &str) {
    let raw = text.trim().trim_matches('"');
    if raw.is_empty() {
        return;
    }
    let p = pal(ui);
    // Looks again once the status may be out of date, even with nothing else happening.
    ui.ctx().request_repaint_after(PATH_KIND_TTL);
    let (icon, color, missing) = match path_kind(raw) {
        PathKind::Url => (Icon::Globe, p.accent, false),
        PathKind::Dir => (Icon::Folder, p.success, false),
        PathKind::File => (Icon::File, p.success, false),
        PathKind::Missing => (Icon::Warning, p.danger, true),
    };
    // The response covers only the text, inside the margin.
    let r = field.rect + PATH_FIELD_MARGIN;
    let icon_rect =
        egui::Rect::from_center_size(egui::pos2(r.right() - 15.0, r.center().y), vec2(14.0, 14.0));
    crate::ui::icons::paint(ui.painter(), icon_rect, icon, color);
    if missing {
        ui.painter().rect_stroke(
            r,
            theme::radius::MD,
            Stroke::new(1.5_f32, p.danger),
            egui::StrokeKind::Inside,
        );
        ui.interact(icon_rect, field.id.with("missing"), Sense::hover())
            .on_hover_text(tr("msg.not_exist"));
    }
}

/// Options search result: the setting, and the tab it lives on.
fn option_result_row(ui: &mut egui::Ui, icon: Icon, label: &str, tab: &str) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{label} — {tab}"))
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, theme::radius::MD, p.control_hover);
    }
    crate::ui::icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            vec2(16.0, 16.0),
        ),
        icon,
        p.text_dim,
    );
    let clip = ui.painter().with_clip_rect(rect.shrink2(vec2(10.0, 0.0)));
    let x = rect.left() + 36.0;
    clip.text(
        egui::pos2(x, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::size::CONTROL),
        p.text,
    );
    clip.text(
        egui::pos2(x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        tab,
        FontId::proportional(theme::size::SMALL),
        p.text_dim,
    );
    widgets::focus_ring(ui, &resp, rect, theme::radius::MD);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Full-window highlight while files are dragged over the window.
fn drop_overlay(ctx: &egui::Context, filling_dialog: bool) {
    let count = ctx.input(|i| i.raw.hovered_files.len());
    if count == 0 {
        return;
    }
    let p = theme::palette(ctx.global_style().visuals.dark_mode);
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("drop_overlay"),
    ));
    painter.rect_filled(screen, 0.0, p.surface.gamma_multiply(0.82));
    let frame = screen.shrink(14.0);
    painter.rect_filled(frame, 16.0, p.accent.gamma_multiply(0.06));
    painter.extend(egui::Shape::dashed_line(
        &[
            frame.left_top(),
            frame.right_top(),
            frame.right_bottom(),
            frame.left_bottom(),
            frame.left_top(),
        ],
        Stroke::new(2.0_f32, p.accent),
        10.0,
        6.0,
    ));
    let c = frame.center();
    let tile = egui::Rect::from_center_size(c - vec2(0.0, 40.0), vec2(64.0, 64.0));
    painter.rect_filled(tile, 18.0, p.accent_soft);
    crate::ui::icons::paint(&painter, tile.shrink(16.0), Icon::Drop, p.accent);
    let title = if filling_dialog {
        tr("app.drop_fill")
    } else {
        tr("app.drop_overlay")
    };
    painter.text(
        c + vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        title,
        theme::semibold(theme::size::HEADING),
        p.text,
    );
    if !filling_dialog {
        painter.text(
            c + vec2(0.0, 42.0),
            Align2::CENTER_CENTER,
            tr("app.drop_overlay_desc"),
            FontId::proportional(theme::size::BODY),
            p.text_dim,
        );
    }
}

impl OxideMergeApp {
    /// `stored` is the config as loaded, before the options of the command line went into `cfg`.
    #[cfg(test)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        stored: Config,
        initial: Option<OpenRequest>,
    ) -> Self {
        Self::with_fetched(cc, cfg, stored, initial, Vec::new())
    }

    /// [`Self::new`], with paths of `initial` fetched from servers on the command line: each
    /// connection, path on the server and fetch, for *Save* to write the side back.
    pub fn with_fetched(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        stored: Config,
        initial: Option<OpenRequest>,
        fetched: Vec<(remote::RemoteConnection, String, remote::Fetched)>,
    ) -> Self {
        let lang = i18n::current_language().to_string();
        fonts::setup(&cc.egui_ctx, &lang);
        #[cfg(target_os = "macos")]
        crate::ui::macos::ask_before_quitting();
        let applied_lang = cfg.language.clone();
        let mut app = OxideMergeApp {
            baseline_start: Default::default(),
            sync_recovery: Some(super::sync_dialog::Recovery::all()).filter(|r| !r.is_empty()),
            saved_cfg: cfg.clone(),
            cli_options: (cfg != stored).then(|| (cfg.clone(), stored)),
            cfg,
            tabs: Vec::new(),
            active: 0,
            open_dlg: None,
            start_paths: Default::default(),
            start_three_way: false,
            options_dlg: None,
            patch_dlg: None,
            stats_dlg: None,
            save_job: None,
            run_job: None,
            git_dlg: None,
            remote_dlg: None,
            remote_load: None,
            host_prompt: None,
            upload: None,
            upload_queue: Vec::new(),
            repack: None,
            reload_prompt: None,
            reload: None,
            pending_remote: Vec::new(),
            last_remote: None,
            asks: Some(install_asker(&cc.egui_ctx)),
            ask: None,
            archive_password: None,
            archive_passwords: Vec::new(),
            rev_load: None,
            report_job: None,
            report_opens: false,
            prints: Vec::new(),
            tab_keys: Vec::new(),
            filters_dlg: false,
            workspace_save_dlg: None,
            manage_workspaces: false,
            clipboard_read: None,
            rule_dlg: None,
            about: false,
            help: false,
            named_filters: filter::load_all_filters(),
            prompt: None,
            close_queue: Vec::new(),
            toasts: Vec::new(),
            font_lang: lang,
            applied_lang,
            visuals_dark: None,
            allow_close: false,
            settings_retry: None,
            shown_active: None,
            pass_on: None,
            was_focused: true,
            next_tab_id: 0,
            track_session: initial.is_none(),
            palette: None,
            text_layout: (false, false),
            smoke: std::env::var_os("OXIDEMERGE_SMOKE_REPORT")
                .filter(|p| !p.is_empty())
                .map(|p| (PathBuf::from(p), 0)),
            screenshot: std::env::var_os("OXIDEMERGE_SCREENSHOT")
                .filter(|p| !p.is_empty())
                .map(|p| Screenshot {
                    path: PathBuf::from(p),
                    command: std::env::var("OXIDEMERGE_SCREENSHOT_COMMAND")
                        .ok()
                        .and_then(|k| {
                            let cmd = commands::PALETTE.iter().find(|c| c.label() == k);
                            if cmd.is_none() {
                                eprintln!("OXIDEMERGE_SCREENSHOT_COMMAND={k}: no such command");
                            }
                            cmd.copied()
                        }),
                    frames: 0,
                    taken: None,
                }),
        };
        app.text_layout = (app.cfg.collapse_unchanged, app.cfg.word_wrap);
        crate::core::fsutil::set_backup(app.cfg.backup_on_save);
        for (conn, remote_path, fetched) in fetched {
            app.stage_fetch(conn, remote_path, fetched);
        }
        if let Some(req) = initial {
            app.open(req);
        } else if app.cfg.restore_session {
            app.restore_session();
        }
        if let (Some(u), Some(p)) = (crate::config::unreadable(), crate::config::config_path()) {
            app.error(crate::config::unreadable_message(
                &p,
                u.as_ref().ok().map(|k| k.as_path()),
            ));
        }
        if !app.cfg.rules.is_empty() && rules::find(&app.cfg.rules).is_none() {
            let name = rules::display_name(&app.cfg.rules).to_string();
            app.error(trf("rules.missing", &[("name", name)]));
        }
        app
    }

    /// Reopens the tabs of the previous session; those that can't be opened are left out.
    fn restore_session(&mut self) {
        let session = self.cfg.session.clone();
        if let (Some(a), _) = self.open_saved_tabs(&session, self.cfg.session_active) {
            self.active = a;
        }
    }

    /// Opens every tab of `tabs` (the previous session, or a named workspace), skipping ones
    /// that can no longer be opened (a file of it is gone, say), alongside whatever is open
    /// already. Returns the app tab index to focus afterwards: the one that came from index
    /// `focus` of `tabs` if it opened, else the last one of `tabs` that did, `None` if none
    /// opened; and how many of `tabs` could not be opened.
    fn open_saved_tabs(&mut self, tabs: &[SessionTab], focus: usize) -> (Option<usize>, usize) {
        let recent_paths = self.cfg.recent_paths.clone();
        let mut active = None;
        let mut failed = 0;
        for (i, s) in tabs.iter().enumerate() {
            if s.paths
                .iter()
                .any(|p| !web::is_url(p) && !Path::new(p).exists())
            {
                failed += 1;
                continue;
            }
            let kind = match s.kind.as_deref() {
                Some("text") => Some(FileKind::Text),
                Some("binary") => Some(FileKind::Binary),
                Some("image") => Some(FileKind::Image),
                Some("web") => Some(FileKind::Web),
                _ => None,
            };
            let before = self.tabs.len();
            let req = OpenRequest {
                paths: s.paths.iter().map(PathBuf::from).collect(),
                kind,
                skip_recent: true,
                rules: s.rules.clone(),
                ..Default::default()
            };
            if self.try_open(req).is_ok() && self.tabs.len() > before {
                if i <= focus {
                    active = Some(self.tabs.len() - 1);
                }
            } else {
                failed += 1;
            }
        }
        // a single file without conflicts opens the Open dialog instead of a tab
        self.open_dlg = None;
        self.cfg.recent_paths = recent_paths;
        (active, failed)
    }

    /// The open tabs right now, as a list to save (the auto-saved session, or a named
    /// workspace): "All differences" and review tabs, and tabs of temporary copies, have
    /// nothing of their own to reopen from and are left out.
    fn session_tabs(&self) -> Vec<SessionTab> {
        let tab = |t: &Tab| -> Option<SessionTab> {
            if t.source.is_empty() {
                return None;
            }
            // A side fetched over SFTP, read from a revision or opened from inside an extracted
            // archive is staged in a temporary folder gone by the next start (and reopening a
            // remote one would need its password or key again in any case).
            if t.remote.iter().any(Option::is_some) || t.source.iter().any(|p| self.is_staged(p)) {
                return None;
            }
            let kind = match &t.content {
                Content::Folder(_) => None,
                Content::Text(_) => Some("text"),
                Content::Binary(_) => Some("binary"),
                Content::Image(_) => Some("image"),
                Content::Web(_) => Some("web"),
                Content::Diffs(_) => return None,
            };
            let rules = match &t.content {
                Content::Text(v) => v.rules.clone(),
                Content::Folder(v) => Some(v.rules.clone()).filter(|r| *r != self.cfg.rules),
                _ => None,
            };
            Some(SessionTab {
                paths: t.source.iter().map(|p| p.display().to_string()).collect(),
                kind: kind.map(str::to_string),
                rules,
            })
        };
        self.tabs.iter().filter_map(tab).collect()
    }

    fn save_session(&mut self) {
        if !self.track_session {
            return;
        }
        let session = if self.cfg.restore_session {
            self.session_tabs()
        } else {
            Vec::new()
        };
        let active = self.tabs[..self.active.min(self.tabs.len())]
            .iter()
            .filter(|t| !t.source.is_empty())
            .count();
        if session != self.cfg.session {
            self.cfg.session = session;
        }
        self.cfg.session_active = active;
    }

    /// Saves the open tabs as a named workspace (see [`Config::workspaces`]): a workspace of
    /// the same name, told apart regardless of case, is replaced (the dialog asked first),
    /// otherwise a new one is added. Nothing is saved when no open tab can be.
    fn save_workspace(&mut self, name: String) {
        let tabs = self.session_tabs();
        if tabs.is_empty() {
            self.error(tr("msg.workspace_nothing"));
            return;
        }
        let left_out = self.tabs.len().saturating_sub(tabs.len());
        let n = tabs.len();
        let mut msg = trf(
            "msg.workspace_saved",
            &[("name", name.clone()), ("n", n.to_string())],
        );
        match self.cfg.workspaces.iter_mut().find(|w| w.is_named(&name)) {
            Some(w) => *w = Workspace { name, tabs },
            None => self.cfg.workspaces.push(Workspace { name, tabs }),
        }
        if left_out > 0 {
            msg.push(' ');
            msg.push_str(&trf(
                "msg.workspace_left_out",
                &[("n", left_out.to_string())],
            ));
        }
        self.push_toast(msg, false, None);
    }

    /// Opens every tab of the workspace at `idx` of [`Config::workspaces`], alongside whatever
    /// is open already, and focuses its last tab. Tabs that could not be reopened are counted
    /// in a message.
    fn open_workspace(&mut self, idx: usize) {
        let Some(tabs) = self.cfg.workspaces.get(idx).map(|w| w.tabs.clone()) else {
            return;
        };
        let (active, failed) = self.open_saved_tabs(&tabs, usize::MAX);
        if let Some(a) = active {
            self.active = a;
        }
        if failed > 0 {
            self.error(trf(
                "msg.workspace_not_reopened",
                &[("n", failed.to_string()), ("total", tabs.len().to_string())],
            ));
        }
    }

    /// Starts reading the clipboard for [`Cmd::CompareClipboard`]; [`Self::poll_clipboard`]
    /// opens the tab once it is read. Tests stand in for the clipboard (see
    /// [`TEST_CLIPBOARD`]), with no thread.
    fn start_clipboard_read(&mut self) {
        if self.clipboard_read.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        #[cfg(test)]
        if let Some(r) = TEST_CLIPBOARD.with(|c| c.borrow().clone()) {
            let _ = tx.send(r);
            self.clipboard_read = Some(rx);
            return;
        }
        std::thread::spawn(move || {
            let _ = tx.send(read_clipboard_text());
        });
        self.clipboard_read = Some(rx);
    }

    /// Opens the clipboard compare once the clipboard is read.
    fn poll_clipboard(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.clipboard_read else {
            return;
        };
        let read = match rx.try_recv() {
            Ok(read) => read,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(tr("msg.clipboard_unavailable").to_string())
            }
        };
        self.clipboard_read = None;
        match clipboard_doc(read) {
            Ok(left) => {
                let mut v = TextView::new(vec![left, blank_doc()], &self.cfg);
                v.status = tr("msg.clipboard_hint").to_string();
                self.push_tab(Content::Text(v), Vec::new());
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Deletes the workspace at `idx` of [`Config::workspaces`], with an Undo in its message.
    fn delete_workspace(&mut self, idx: usize) {
        if idx >= self.cfg.workspaces.len() {
            return;
        }
        let w = self.cfg.workspaces.remove(idx);
        let msg = trf("msg.workspace_deleted", &[("name", w.name.clone())]);
        self.push_toast(msg, false, Some(ToastUndo::Workspace(idx, w)));
    }

    /// Keeps the workspaces another running OxideMerge saved since this one read the settings:
    /// saving the settings would otherwise write this one's older list over them.
    fn merge_saved_workspaces(&mut self) {
        if let Some(theirs) = crate::config::saved_workspaces() {
            self.cfg.workspaces = crate::config::merge_workspaces(
                &self.saved_cfg.workspaces,
                &self.cfg.workspaces,
                &theirs,
            );
            // What the file holds is what the next merge starts from, even if saving fails:
            // a workspace deleted there afterwards then stays deleted.
            self.saved_cfg.workspaces = theirs;
        }
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.push_toast(msg.into(), true, None);
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.push_toast(msg.into(), false, None);
    }

    fn push_toast(&mut self, text: String, error: bool, undo: Option<ToastUndo>) {
        self.toasts
            .retain(|t| t.text != text && (undo.is_none() || t.undo != undo));
        self.toasts.push(Toast {
            text,
            error,
            shown: None,
            undo,
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn new_open_dialog(&self, left: Option<String>) -> OpenDialog {
        OpenDialog {
            paths: [left.unwrap_or_default(), String::new(), String::new()],
            three_way: false,
            filter: self.cfg.filter.clone(),
            recursive: self.cfg.folder.recursive,
            method: self.cfg.folder.method,
            kind: None,
            read_only: [false; 3],
        }
    }

    // ---------- opening ----------

    fn load_text(&self, p: &Path, missing: bool) -> anyhow::Result<TextDoc> {
        if missing {
            let mut d = TextDoc::empty(Some(p.to_path_buf()));
            d.read_only = unpacker::find(&self.cfg.comparison_options(), p).is_some();
            d.description = Some(format!("{} ({})", p.display(), tr("label.missing")));
            return Ok(d);
        }
        unpacker::load_doc(p, &self.cfg.comparison_options(), None)
    }

    /// Whether `path` is in a temporary copy (a fetch over SFTP, a revision, an extracted
    /// archive), gone by the next start and so not worth remembering among recent paths.
    fn is_staged(&self, path: &Path) -> bool {
        self.pending_remote
            .iter()
            .any(|p| path.starts_with(p.temp.path()))
            || self
                .tabs
                .iter()
                .any(|t| t.shown_temps().iter().any(|d| path.starts_with(d.path())))
    }

    pub fn open(&mut self, req: OpenRequest) {
        let staged = req.paths.iter().any(|p| self.is_staged(p));
        let recent: Option<Vec<String>> = (!req.skip_recent && !staged && req.paths.len() >= 2)
            .then(|| req.paths.iter().map(|p| p.display().to_string()).collect());
        let retry = req.clone();
        let result = self.try_open(req);
        if let Some(n) = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<archive::NeedsPassword>())
        {
            self.archive_password = Some(ArchivePrompt {
                archive: n.archive.clone(),
                wrong: n.wrong,
                req: retry,
                password: Default::default(),
                focus: true,
            });
            return;
        }
        self.forget_archive_passwords();
        match result {
            Ok(()) => {
                if let Some(r) = recent {
                    self.cfg.add_recent_compare(&r);
                }
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// The password of an encrypted archive being opened (see [`ArchivePrompt`]).
    fn archive_password_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.archive_password else {
            return;
        };
        let (mut ok, mut cancel) = (false, false);
        // The Enter that opened the archive is not taken as an answer.
        let first = std::mem::take(&mut p.focus);
        let modal = egui::Modal::new(egui::Id::unique("archive_password_dialog")).show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.set_max_width(520.0);
            if widgets::dialog_title(ui, &plain(tr("label.archive_password_title"))) {
                cancel = true;
            }
            let key = if p.wrong {
                "msg.archive_password_wrong"
            } else {
                "msg.archive_password_needed"
            };
            let file = p.archive.display().to_string();
            ui.add(egui::Label::new(trf(key, &[("file", file)])).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(tr("label.archive_password"));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut *p.password)
                        .password(true)
                        .desired_width(240.0),
                );
                if first {
                    field.request_focus();
                }
            });
            let ready = !p.password.is_empty();
            widgets::dialog_footer(ui, |ui| {
                ui.add_enabled_ui(ready, |ui| {
                    if widgets::primary_button(ui, tr("btn.ok")).clicked()
                        || (ready && !first && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        ok = true;
                    }
                });
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if cancel {
            self.archive_password = None;
            self.forget_archive_passwords();
        } else if ok {
            if let Some(mut p) = self.archive_password.take() {
                let password = std::mem::take(&mut p.password);
                archive::give_password(&p.archive, password);
                self.archive_passwords.push(p.archive.clone());
                self.open(p.req);
            }
        }
    }

    fn forget_archive_passwords(&mut self) {
        for p in self.archive_passwords.drain(..) {
            archive::forget_password(&p);
        }
    }

    fn try_open(&mut self, req: OpenRequest) -> anyhow::Result<()> {
        let mut paths = req.paths.clone();
        if paths.is_empty() {
            return Ok(());
        }
        // Every view compares two or three sides (a single path opens a conflict file, a
        // project or the open dialog).
        if paths.len() > 3 {
            anyhow::bail!(
                "{}",
                trf("msg.too_many_paths", &[("n", paths.len().to_string())])
            );
        }
        if let Some(r) = req.rules.as_deref().filter(|r| !r.is_empty()) {
            if rules::find(r).is_none() {
                let name = rules::display_name(r).to_string();
                self.error(trf("rules.missing", &[("name", name)]));
            }
        }
        if paths.len() == 1 && project::is_project(&paths[0]) {
            let mut opened = project_request(&paths[0])?;
            opened.skip_recent = req.skip_recent;
            if !req.skip_recent {
                let r: Vec<String> = opened
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                self.cfg.add_recent_compare(&r);
            }
            return self.try_open(opened);
        }
        if let (Some(rev), [path]) = (&req.git_rev, &paths[..]) {
            self.open_git_revision(path, rev, None);
            return Ok(());
        }
        if paths.len() == 1 && text::extension(&paths[0]) == crate::core::review::EXT {
            let review = crate::core::review::load(&paths[0])?;
            let view = FolderView::from_review(review, &self.cfg);
            self.cfg.add_recent_path(&paths[0].display().to_string());
            self.push_tab(Content::Folder(view), Vec::new());
            return Ok(());
        }
        if paths.len() == 1 && saved::is_saved_compare(&paths[0]) {
            let s = SavedCompare::load(&paths[0])?;
            let mut v = TextView::from_saved(&s, &self.cfg);
            v.status = trf("msg.saved_compare", &[("date", s.saved.clone())]);
            self.cfg.add_recent_path(&paths[0].display().to_string());
            self.push_tab(Content::Text(v), Vec::new());
            return Ok(());
        }
        if paths.len() == 1 {
            let p = &paths[0];
            if p.is_file() && conflict::has_markers(p) {
                if let Ok(doc) = text::load_doc(p, None) {
                    if conflict::parse(&doc.lines).is_some() {
                        return self.open_conflict(p);
                    }
                }
            }
            self.open_dlg = Some(self.new_open_dialog(Some(p.display().to_string())));
            return Ok(());
        }
        for p in &paths {
            if !self.is_staged(p) {
                self.cfg.add_recent_path(&p.display().to_string());
            }
        }
        let html = req.kind.is_none()
            && req.output.is_none()
            && req.snapshot_options.is_none()
            && web::are_html_files(&paths);
        if html
            || req.kind == Some(FileKind::Web)
            || paths.iter().any(|p| web::is_url(&p.to_string_lossy()))
        {
            let urls = paths
                .iter()
                .map(|p| web::location(p))
                .collect::<anyhow::Result<Vec<_>>>()?;
            self.push_tab(Content::Web(WebView::new(urls, &self.cfg)), paths);
            return Ok(());
        }
        // a folder compared with a file: use the same file name inside the folder
        if let Some(file) = paths.iter().find(|p| p.is_file()).cloned() {
            if paths.iter().any(|p| p.is_dir()) {
                let name = file.file_name().unwrap_or_default().to_owned();
                for p in &mut paths {
                    if p.is_dir() {
                        *p = p.join(&name);
                    }
                }
            }
        }
        let mut missing = req.missing.clone();
        missing.resize(paths.len(), false);
        for (i, p) in paths.iter().enumerate() {
            if !p.exists() {
                missing[i] = true;
            }
        }
        if missing.iter().all(|&m| m) {
            anyhow::bail!(
                "{}: {}",
                tr("msg.not_exist"),
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let source = paths.clone();
        let filter_spec = req
            .filter
            .clone()
            .unwrap_or_else(|| self.cfg.filter.clone());
        let make_folder = |app: &mut Self,
                           roots: Vec<PathBuf>,
                           display: Vec<String>,
                           temps: Vec<tempfile::TempDir>| {
            let mut v = FolderView::new(
                roots,
                display,
                temps,
                &filter_spec,
                &app.named_filters,
                &app.cfg,
            );
            for (i, ro) in req.read_only.iter().enumerate().take(v.read_only.len()) {
                v.read_only[i] |= *ro;
            }
            let mut restart = false;
            if let Some(r) = req.recursive {
                v.set_recursive(r);
                restart = true;
            }
            if let Some(m) = req.method {
                v.set_method(m);
                restart = true;
            }
            if let Some(r) = &req.rules {
                v.set_rules(&app.cfg, r.clone());
                restart = true;
            }
            if restart {
                v.start_compare();
            }
            app.cfg.add_recent_filter(&filter_spec);
            app.push_tab(Content::Folder(v), source.clone());
            app.tabs.last_mut().unwrap().descriptions = req.descriptions.clone();
        };
        if req.kind.is_none() && paths.iter().all(|p| p.is_dir()) {
            let display = paths.iter().map(|p| p.display().to_string()).collect();
            make_folder(self, paths, display, Vec::new());
            return Ok(());
        }
        if req.kind.is_none()
            && req.snapshot_options.is_none()
            && paths.iter().all(|p| {
                archive::is_archive(p)
                    && unpacker::find(&self.cfg.comparison_options(), p).is_none()
            })
        {
            let mut temps = Vec::new();
            for p in &paths {
                temps.push(archive::extract_to_temp(p)?);
            }
            let roots = temps.iter().map(|t| t.path().to_path_buf()).collect();
            let display = paths.iter().map(|p| p.display().to_string()).collect();
            make_folder(self, roots, display, temps);
            return Ok(());
        }
        let existing: Vec<&PathBuf> = paths
            .iter()
            .zip(&missing)
            .filter(|(_, m)| !**m)
            .map(|(p, _)| p)
            .collect();
        let kind = req.kind.unwrap_or_else(|| {
            if existing.iter().all(|p| text::is_image_path(p)) {
                FileKind::Image
            } else if existing.iter().any(|p| {
                (req.snapshot_options.is_some()
                    || req.output.is_some()
                    || unpacker::find(&self.cfg.comparison_options(), p).is_none())
                    && text::detect_kind(p) == FileKind::Binary
            }) {
                FileKind::Binary
            } else {
                FileKind::Text
            }
        });
        let content = match kind {
            FileKind::Text => {
                let mut docs = Vec::new();
                // A merge written to an output file (`git mergetool`) works on the files as
                // they are: text read out of a notebook or document could not be saved there.
                let raw = req.snapshot_options.is_some() || req.output.is_some();
                for (i, p) in paths.iter().enumerate() {
                    let mut d = if raw && !missing[i] {
                        text::load_doc(p, None)?
                    } else {
                        self.load_text(p, missing[i])?
                    };
                    if let Some(Some(desc)) = req.descriptions.get(i) {
                        d.description = Some(desc.clone());
                    }
                    if req.read_only.get(i).copied().unwrap_or(false) {
                        d.read_only = true;
                    }
                    docs.push(d);
                }
                if let Some(out) = &req.output {
                    crate::ui::text_view::redirect_output(&mut docs, out);
                }
                let mut v = TextView::new(docs, &self.cfg);
                if let Some(m) = &req.table {
                    v.set_table_match(m.clone());
                }
                if req.rules.is_some() {
                    v.set_rules(req.rules.clone());
                    if let Some(o) = v.options_for(&self.cfg.diff) {
                        v.set_options(&o);
                    }
                }
                if let Some(options) = &req.snapshot_options {
                    v.freeze_options((**options).clone());
                }
                Content::Text(v)
            }
            FileKind::Binary => {
                let mut v = BinaryView::load(paths)?;
                for (i, ro) in req.read_only.iter().enumerate() {
                    if i < v.read_only.len() {
                        v.read_only[i] = *ro;
                    }
                }
                Content::Binary(v)
            }
            FileKind::Image => {
                if missing.iter().any(|&m| m) {
                    anyhow::bail!("{}", tr("msg.image_missing"));
                }
                let mut v = ImageView::load(paths, &self.cfg)?;
                for (i, desc) in req
                    .descriptions
                    .iter()
                    .enumerate()
                    .take(v.descriptions.len())
                {
                    v.descriptions[i] = desc.clone();
                }
                for (i, ro) in req.read_only.iter().enumerate().take(v.read_only.len()) {
                    v.read_only[i] |= *ro;
                }
                Content::Image(v)
            }
            FileKind::Web => unreachable!("web pages are opened above"),
        };
        self.push_tab(content, source);
        self.tabs.last_mut().unwrap().descriptions = req.descriptions.clone();
        Ok(())
    }

    /// Compares `path` with how it was in revision `rev` of its Git repository, Subversion
    /// working copy or Mercurial repository (the left side, read-only). The revision is read on
    /// another thread, as a server may be slow or not answer; `dialog` comes back if it fails.
    fn open_git_revision(&mut self, path: &Path, rev: &str, dialog: Option<GitDialog>) {
        if let Some(old) = self.rev_load.take() {
            old.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let rev = vcs::rev_or_default(path, rev).to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (p, r, stop) = (path.to_path_buf(), rev.clone(), cancel.clone());
        std::thread::spawn(move || {
            let _ = tx.send(vcs::copy_of(&p, &r, &stop));
        });
        self.rev_load = Some(RevLoad {
            path: path.to_path_buf(),
            rev,
            rx,
            cancel,
            dialog,
        });
    }

    /// Opens the connection dialog of *Compare over SFTP* for `target`'s path field.
    fn start_remote_connect(&mut self, target: RemoteTarget) {
        let last = self.last_remote.as_ref();
        self.remote_dlg = Some(RemoteDialog {
            target,
            protocol: last.map_or(remote::Protocol::Sftp, |l| l.protocol),
            host: last.map_or_else(String::new, |l| l.host.clone()),
            port: last.map_or_else(|| "22".to_string(), |l| l.port.clone()),
            user: last.map_or_else(String::new, |l| l.user.clone()),
            sign_in: last.map_or(SignIn::Password, |l| l.sign_in),
            password: String::new(),
            key_path: last.map_or_else(String::new, |l| l.key_path.clone()),
            passphrase: String::new(),
            remote_path: String::new(),
            browse: None,
            proxy: last.map_or_else(String::new, |l| l.proxy.clone()),
            proxy_password: String::new(),
            active: last.is_some_and(|l| l.active),
            client_cert: last.map_or_else(String::new, |l| l.client_cert.clone()),
            client_key: last.map_or_else(String::new, |l| l.client_key.clone()),
        });
    }

    /// *File → Compare over SFTP*: a server, a user name, a password or a private key, and the
    /// file or folder to fetch from it, for one path field.
    fn remote_connect_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.remote_dlg.take() else {
            return;
        };
        let (mut connect, mut cancel) = (false, false);
        let conn = d.conn();
        if let Some(b) = &d.browse {
            if !conn.as_ref().is_some_and(|c| same_conn(c, &b.conn)) {
                d.browse = None;
            }
        }
        if let Some(b) = &mut d.browse {
            if let Some(listed) = b.browser.poll() {
                b.loading = false;
                ctx.request_repaint();
                match listed {
                    Ok(l) => {
                        if d.remote_path == b.field {
                            d.remote_path = match &l.file {
                                Some(file) => remote::join(&l.path, file),
                                None => l.path.clone(),
                            };
                        }
                        b.error = None;
                        b.listing = Some(l);
                    }
                    Err(e) => match e.downcast::<remote::UnknownHost>() {
                        Ok(host) => {
                            let Some(b) = d.browse.take() else {
                                return;
                            };
                            self.host_prompt = Some(HostPrompt {
                                host,
                                target: d.target,
                                conn: b.conn.clone(),
                                remote_path: b.asked.clone(),
                                dialog: Some(d),
                                browse: true,
                            });
                            return;
                        }
                        Err(e) => b.error = Some(format!("{e:#}")),
                    },
                }
            }
        }
        if d.browse.as_ref().is_some_and(|b| b.loading) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let ready = conn.is_some() && !d.remote_path.trim().is_empty();
        let (mut toggle_browse, mut go, mut pick, mut fetch_now) = (false, None, None, None);
        let modal = egui::Modal::new(egui::Id::unique("remote_connect_dialog")).show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.set_max_width(600.0);
            if widgets::dialog_title(ui, &plain(tr("menu.open_remote"))) {
                cancel = true;
            }
            egui::Grid::new("remote_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(tr("label.remote_protocol"));
                    let mut protocol = d.protocol;
                    egui::ComboBox::from_id_salt("remote_protocol")
                        .selected_text(protocol_name(protocol))
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for p in remote::Protocol::ALL {
                                ui.selectable_value(&mut protocol, p, protocol_name(p));
                            }
                        });
                    if protocol != d.protocol {
                        d.set_protocol(protocol);
                    }
                    ui.end_row();
                    ui.label(tr("label.remote_host"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.host)
                                .hint_text("example.com")
                                .desired_width(220.0),
                        );
                        ui.label(tr("label.remote_port"));
                        ui.add(egui::TextEdit::singleline(&mut d.port).desired_width(50.0));
                    });
                    ui.end_row();
                    ui.label(tr("label.remote_user"));
                    ui.add(egui::TextEdit::singleline(&mut d.user).desired_width(220.0));
                    ui.end_row();
                    if !d.protocol.is_ftp() {
                        ui.label(tr("label.remote_auth"));
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut d.sign_in,
                                SignIn::Password,
                                tr("label.remote_password"),
                            );
                            ui.selectable_value(
                                &mut d.sign_in,
                                SignIn::Key,
                                tr("label.remote_key"),
                            );
                            ui.selectable_value(
                                &mut d.sign_in,
                                SignIn::Agent,
                                tr("label.remote_agent"),
                            );
                        });
                        ui.end_row();
                    }
                    if d.sign_in() == SignIn::Agent {
                        ui.label("");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(tr("msg.remote_agent_hint")).weak(),
                            )
                            .wrap(),
                        );
                        ui.end_row();
                    } else if d.sign_in() == SignIn::Key {
                        ui.label(tr("label.remote_key_path"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut d.key_path).desired_width(260.0),
                            );
                            if widgets::icon_button(ui, Icon::File, tr("btn.browse_file")).clicked()
                            {
                                if let Some(p) = rfd::FileDialog::new().pick_file() {
                                    d.key_path = p.display().to_string();
                                }
                            }
                        });
                        ui.end_row();
                        ui.label(tr("label.remote_passphrase"));
                        ui.add(
                            egui::TextEdit::singleline(&mut d.passphrase)
                                .password(true)
                                .desired_width(220.0),
                        );
                        ui.end_row();
                    } else {
                        ui.label(tr("label.remote_password"));
                        ui.add(
                            egui::TextEdit::singleline(&mut d.password)
                                .password(true)
                                .desired_width(220.0),
                        );
                        ui.end_row();
                    }
                    ui.label(tr("label.remote_path"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.remote_path)
                                .hint_text(tr("msg.remote_path_hint"))
                                .desired_width(260.0),
                        );
                        if widgets::icon_button_enabled(
                            ui,
                            conn.is_some(),
                            Icon::Folder,
                            tr("btn.list_remote"),
                        )
                        .clicked()
                        {
                            toggle_browse = true;
                        }
                    });
                    ui.end_row();
                });
            if d.protocol == remote::Protocol::Ftp {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().warn_fg_color, tr("msg.ftp_plain_warning"));
            }
            ui.add_space(4.0);
            remote_advanced(ui, &mut d);
            if let Some(b) = &d.browse {
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let up = b
                            .listing
                            .as_ref()
                            .and_then(|l| remote::parent_path(&l.path));
                        if widgets::icon_button_enabled(
                            ui,
                            up.is_some(),
                            Icon::Prev,
                            tr("btn.remote_up"),
                        )
                        .clicked()
                        {
                            go = up;
                        }
                        if widgets::icon_button(ui, Icon::Refresh, tr("cmd.refresh")).clicked() {
                            go = Some(d.remote_path.clone());
                        }
                        if b.loading {
                            ui.spinner();
                        }
                        if let Some(l) = &b.listing {
                            ui.add(
                                egui::Label::new(egui::RichText::new(&l.path).monospace())
                                    .truncate(),
                            );
                        }
                    });
                    if let Some(e) = &b.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| match &b.listing {
                            None => {
                                if b.loading {
                                    ui.weak(trf(
                                        "msg.remote_listing",
                                        &[("path", b.asked.clone())],
                                    ));
                                }
                            }
                            Some(l) => {
                                if l.entries.is_empty() {
                                    ui.weak(tr("msg.remote_empty"));
                                }
                                for e in &l.entries {
                                    let full = remote::join(&l.path, &e.name);
                                    ui.horizontal(|ui| {
                                        let icon = match e.kind {
                                            remote::EntryKind::Dir => Icon::Folder,
                                            remote::EntryKind::File => Icon::File,
                                            remote::EntryKind::Link => Icon::ChevronRight,
                                        };
                                        widgets::small_icon(ui, icon, ui.visuals().text_color());
                                        let size = (e.kind == remote::EntryKind::File)
                                            .then(|| report::fmt_size(Some(e.size)));
                                        let size_width = size.as_ref().map_or(0.0, |s| {
                                            ui.painter()
                                                .layout_no_wrap(
                                                    s.clone(),
                                                    egui::TextStyle::Body.resolve(ui.style()),
                                                    egui::Color32::PLACEHOLDER,
                                                )
                                                .size()
                                                .x
                                                + ui.spacing().item_spacing.x
                                        });
                                        let row = ui
                                            .scope(|ui| {
                                                ui.set_max_width(
                                                    (ui.available_width() - size_width).max(40.0),
                                                );
                                                ui.add(
                                                    egui::Button::selectable(
                                                        d.remote_path == full,
                                                        &e.name,
                                                    )
                                                    .truncate(),
                                                )
                                            })
                                            .inner;
                                        if let Some(size) = size {
                                            ui.weak(size);
                                        }
                                        if e.kind != remote::EntryKind::File {
                                            if row.clicked() {
                                                go = Some(full);
                                            }
                                        } else if row.double_clicked() {
                                            fetch_now = Some(full);
                                        } else if row.clicked() {
                                            pick = Some(full);
                                        }
                                    });
                                }
                                if l.truncated {
                                    ui.weak(trf(
                                        "msg.remote_truncated",
                                        &[("count", remote::MAX_BROWSE_ENTRIES.to_string())],
                                    ));
                                }
                            }
                        });
                    ui.weak(tr("msg.remote_browse_hint"));
                });
            }
            widgets::dialog_footer(ui, |ui| {
                ui.add_enabled_ui(ready, |ui| {
                    if widgets::primary_button(ui, tr("btn.connect")).clicked()
                        || (ready && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        connect = true;
                    }
                });
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if toggle_browse {
            d.browse = match (d.browse.take(), &conn) {
                (None, Some(c)) => Some(RemoteBrowse::start(c.clone(), &d.remote_path)),
                _ => None,
            };
        }
        if let (Some(path), Some(b)) = (go, &mut d.browse) {
            b.list(&path, &d.remote_path);
        }
        if let Some(path) = pick {
            d.remote_path = path;
        }
        if let Some(path) = fetch_now {
            d.remote_path = path;
            connect = true;
        }
        match conn {
            Some(conn) if connect && !cancel && !d.remote_path.trim().is_empty() => {
                let remote_path = d.remote_path.trim().to_string();
                let target = d.target;
                self.open_remote(target, conn, remote_path, Some(d));
            }
            _ if cancel => {}
            _ => self.remote_dlg = Some(d),
        }
    }

    /// A server's questions while signing in (see [`install_asker`]), one window at a time,
    /// in the order they came.
    fn ask_dialog(&mut self, ctx: &egui::Context) {
        let gone = |a: &AskRequest| a.gone.load(std::sync::atomic::Ordering::Relaxed);
        if self.ask.as_ref().is_some_and(gone) {
            self.ask = None;
        }
        while self.ask.is_none() {
            match self.asks.as_ref().and_then(|rx| rx.try_recv().ok()) {
                Some(a) if !gone(&a) => self.ask = Some(a),
                Some(_) => {}
                None => return,
            }
        }
        let Some(a) = &mut self.ask else {
            return;
        };
        let (mut ok, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("remote_ask_dialog")).show(ctx, |ui| {
            ui.set_min_width(380.0);
            if widgets::dialog_title(ui, &plain(tr("label.remote_ask_title"))) {
                cancel = true;
            }
            ui.label(trf(
                "msg.remote_ask_server",
                &[("server", a.challenge.server.clone())],
            ));
            if !a.challenge.name.trim().is_empty() {
                ui.strong(a.challenge.name.trim());
            }
            if !a.challenge.instructions.trim().is_empty() {
                ui.label(a.challenge.instructions.trim());
            }
            ui.add_space(4.0);
            egui::Grid::new("remote_ask_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    let focus = std::mem::take(&mut a.focus);
                    for (i, (p, answer)) in
                        a.challenge.prompts.iter().zip(&mut a.answers).enumerate()
                    {
                        ui.label(p.text.trim());
                        let field = ui.add(
                            egui::TextEdit::singleline(answer)
                                .password(!p.echo)
                                .desired_width(220.0),
                        );
                        if focus && i == 0 {
                            field.request_focus();
                        }
                        ui.end_row();
                    }
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.ok")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Enter))
                {
                    ok = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if ok || cancel {
            if let Some(mut a) = self.ask.take() {
                let answers = ok.then(|| std::mem::take(&mut a.answers));
                let _ = a.reply.send(answers);
            }
        }
    }

    /// Fetches `remote_path` over SFTP for `target`'s path field, authenticated as `conn`
    /// says. Read on another thread, as a server may be slow or not answer; `dialog` comes
    /// back if it fails or is stopped.
    fn open_remote(
        &mut self,
        target: RemoteTarget,
        conn: remote::RemoteConnection,
        remote_path: String,
        dialog: Option<RemoteDialog>,
    ) {
        if let Some(old) = self.remote_load.take() {
            old.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (c, p, stop) = (conn.clone(), remote_path.clone(), cancel.clone());
        // Tests stand in for the network with a thread-local fake server (see
        // `remote::fake`), so a fetch for one runs on this thread rather than a spawned one,
        // which would not see it.
        #[cfg(test)]
        let on_this_thread = remote::fake::installed();
        #[cfg(not(test))]
        let on_this_thread = false;
        if on_this_thread {
            let _ = tx.send(remote::fetch(&c, &p, &stop));
        } else {
            std::thread::spawn(move || {
                let _ = tx.send(remote::fetch(&c, &p, &stop));
            });
        }
        self.remote_load = Some(RemoteLoad {
            target,
            conn,
            remote_path,
            rx,
            cancel,
            dialog,
        });
    }

    /// Shows that a file or folder is being fetched over SFTP, with a button to stop, and
    /// fills in its path field once it is done.
    fn remote_load_dialog(&mut self, ctx: &egui::Context) {
        let Some(load) = self.remote_load.take() else {
            return;
        };
        let result = match load.rx.try_recv() {
            Ok(r) => Some(r),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("{}", tr("status.error"))))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        };
        let Some(result) = result else {
            let label = remote::label(&load.conn, &load.remote_path);
            let mut stop = false;
            egui::Modal::new(egui::Id::unique("remote_load_dialog")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(trf("msg.fetching_remote", &[("name", label)]));
                });
                widgets::dialog_footer(ui, |ui| {
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        stop = true;
                    }
                });
            });
            if stop {
                load.cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            self.remote_load = Some(load);
            return;
        };
        let stopped = load.cancel.load(std::sync::atomic::Ordering::Relaxed);
        match result {
            Ok(fetched) => {
                self.last_remote = Some(LastRemote::from_conn(&load.conn));
                self.use_remote_fetch(load.target, load.conn, load.remote_path, fetched);
            }
            Err(e) => match e.downcast::<remote::UnknownHost>() {
                Ok(host) if !stopped => {
                    self.host_prompt = Some(HostPrompt {
                        host,
                        target: load.target,
                        conn: load.conn,
                        remote_path: load.remote_path,
                        dialog: load.dialog,
                        browse: false,
                    });
                }
                Ok(_) => self.remote_dlg = load.dialog,
                Err(e) => {
                    if !stopped {
                        self.error(format!("{e:#}"));
                    }
                    self.remote_dlg = load.dialog;
                }
            },
        }
    }

    /// Asks whether to trust a server this program has not connected to before, showing its
    /// key's fingerprint to check against the real one; fetches again if so.
    fn host_prompt_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = self.host_prompt.take() else {
            return;
        };
        let (mut trust, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("host_prompt")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(tr("dlg.unknown_host"));
            ui.label(if p.host.key_type == tr("label.tls_certificate") {
                trf(
                    "msg.ftps_unknown_cert",
                    &[("server", p.host.server.clone())],
                )
            } else {
                trf(
                    "msg.sftp_unknown_host",
                    &[
                        ("server", p.host.server.clone()),
                        ("type", p.host.key_type.clone()),
                    ],
                )
            });
            ui.add(egui::Label::new(
                egui::RichText::new(&p.host.fingerprint).monospace(),
            ));
            if let Some(file) = &p.host.differs_from {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    trf(
                        "msg.sftp_ssh_mismatch",
                        &[("file", file.display().to_string())],
                    ),
                );
            }
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.trust_connect")).clicked() {
                    trust = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if trust {
            match remote::trust_host(&p.host) {
                Ok(()) if p.browse => {
                    self.remote_dlg = p.dialog.map(|mut d| {
                        let mut b = RemoteBrowse::start(p.conn, &p.remote_path);
                        b.field = d.remote_path.clone();
                        d.browse = Some(b);
                        d
                    });
                }
                Ok(()) => self.open_remote(p.target, p.conn, p.remote_path, p.dialog),
                Err(e) => {
                    self.error(format!("{e:#}"));
                    self.remote_dlg = p.dialog;
                }
            }
        } else if cancel {
            self.remote_dlg = p.dialog;
        } else {
            self.host_prompt = Some(p);
        }
    }

    /// Fills in the path field a fetch over SFTP was started for, and remembers the fetch to
    /// claim once a tab opens with that path (see [`PendingRemote`]).
    fn use_remote_fetch(
        &mut self,
        target: RemoteTarget,
        conn: remote::RemoteConnection,
        remote_path: String,
        fetched: remote::Fetched,
    ) {
        let shown = fetched.local.display().to_string();
        match target {
            RemoteTarget::Start(s) => self.start_paths[s] = shown,
            RemoteTarget::Open(s) => {
                let mut d = self
                    .open_dlg
                    .take()
                    .unwrap_or_else(|| self.new_open_dialog(None));
                d.paths[s] = shown;
                self.open_dlg = Some(d);
            }
        }
        self.stage_fetch(conn, remote_path, fetched);
    }

    /// Remembers `fetched`, from `remote_path` over `conn`, to claim once a tab opens with its
    /// local copy (see [`PendingRemote`]).
    fn stage_fetch(
        &mut self,
        conn: remote::RemoteConnection,
        remote_path: String,
        fetched: remote::Fetched,
    ) {
        let remote::Fetched {
            temp,
            local,
            is_dir,
            stamp,
            manifest,
        } = fetched;
        let sent = if is_dir { None } else { file_hash(&local) };
        self.pending_remote.push(PendingRemote {
            local,
            temp: std::sync::Arc::new(temp),
            file: RemoteFile {
                conn,
                remote_path,
                manifest,
                stamp,
                sent,
                pending: false,
            },
        });
    }

    /// Shows that a revision is being read, with a button to stop, and opens its tab once read.
    fn rev_load_dialog(&mut self, ctx: &egui::Context) {
        let Some(load) = self.rev_load.take() else {
            return;
        };
        let result = match load.rx.try_recv() {
            Ok(r) => Some(r),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("{}", tr("status.error"))))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        };
        let Some(result) = result else {
            let label = format!("{} @ {}", load.path.display(), load.rev);
            let mut stop = false;
            egui::Modal::new(egui::Id::unique("rev_load_dialog")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(trf("msg.reading_revision", &[("name", label)]));
                });
                widgets::dialog_footer(ui, |ui| {
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        stop = true;
                    }
                });
            });
            if stop {
                load.cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            self.rev_load = Some(load);
            return;
        };
        let stopped = load.cancel.load(std::sync::atomic::Ordering::Relaxed);
        let shown =
            result.and_then(|(temp, copy)| self.show_revision(&load.path, &load.rev, temp, copy));
        if let Err(e) = shown {
            if !stopped {
                self.error(e.to_string());
            }
            self.git_dlg = load.dialog;
        }
    }

    /// The "Save as workspace..." dialog: a name for the open tabs, saved under
    /// [`Config::workspaces`] on confirm (see [`App::save_workspace`]).
    fn workspace_save_dialog(&mut self, ctx: &egui::Context) {
        if self.workspace_save_dlg.is_none() {
            return;
        }
        let savable = !self.session_tabs().is_empty();
        let Some((name, focus)) = &mut self.workspace_save_dlg else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let empty = name.trim().is_empty() || !savable;
        let existing = self
            .cfg
            .workspaces
            .iter()
            .find(|w| w.is_named(name))
            .map(|w| w.name.clone());
        egui::Modal::new(egui::Id::unique("workspace_save_dialog")).show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.heading(tr("menu.save_workspace"));
            let field = ui.add(
                egui::TextEdit::singleline(name)
                    .char_limit(MAX_WORKSPACE_NAME)
                    .hint_text(tr("msg.workspace_name_hint")),
            );
            if *focus {
                field.request_focus();
                *focus = false;
            }
            if !savable {
                ui.label(tr("msg.workspace_nothing"));
            } else if let Some(existing) = &existing {
                ui.label(trf("msg.workspace_exists", &[("name", existing.clone())]));
            }
            // Replacing a workspace takes a click on Replace, not just Enter.
            if field.lost_focus()
                && ui.input(|i| i.key_pressed(Key::Enter))
                && !empty
                && existing.is_none()
            {
                save = true;
            }
            widgets::dialog_footer(ui, |ui| {
                ui.add_enabled_ui(!empty, |ui| {
                    let label = if existing.is_some() {
                        tr("btn.replace")
                    } else {
                        tr("btn.save")
                    };
                    if widgets::primary_button(ui, label).clicked() {
                        save = true;
                    }
                });
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if save {
            if let Some((name, _)) = self.workspace_save_dlg.take() {
                self.save_workspace(name.trim().to_string());
            }
        } else if cancel {
            self.workspace_save_dlg = None;
        }
    }

    /// The "Manage workspaces..." dialog: every saved workspace, each with a button to open or
    /// delete it.
    fn manage_workspaces_dialog(&mut self, ctx: &egui::Context) {
        if !self.manage_workspaces {
            return;
        }
        let mut open = None;
        let mut delete = None;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("manage_workspaces_dialog")).show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.heading(tr("menu.manage_workspaces"));
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (i, w) in self.cfg.workspaces.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(&w.name);
                            if widgets::secondary_button(ui, tr("job.open_output")).clicked() {
                                open = Some(i);
                            }
                            if widgets::secondary_button(ui, tr(Cmd::DeleteWorkspace(i).label()))
                                .clicked()
                            {
                                delete = Some(i);
                            }
                        });
                    }
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.close")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    close = true;
                }
            });
        });
        if let Some(i) = delete {
            self.delete_workspace(i);
        }
        if let Some(i) = open {
            self.open_workspace(i);
            self.manage_workspaces = false;
        }
        if close {
            self.manage_workspaces = false;
        }
    }

    /// Opens the tab comparing `path` with `copy`, its copy as of revision `rev` in `temp`.
    fn show_revision(
        &mut self,
        path: &Path,
        rev: &str,
        temp: tempfile::TempDir,
        copy: PathBuf,
    ) -> anyhow::Result<()> {
        let label = format!("{} @ {rev}", path.display());
        let content = if path.is_dir() {
            let display = vec![label, path.display().to_string()];
            let mut v = FolderView::new(
                vec![copy, path.to_path_buf()],
                display,
                Vec::new(),
                &self.cfg.filter,
                &self.named_filters,
                &self.cfg,
            );
            v.read_only[0] = true;
            Content::Folder(v)
        } else {
            let paths = vec![copy.clone(), path.to_path_buf()];
            if text::is_image_path(path) {
                let mut v = ImageView::load(paths, &self.cfg)?;
                v.read_only[0] = true;
                Content::Image(v)
            } else if text::detect_kind(&copy) == FileKind::Binary {
                let mut v = BinaryView::load(paths)?;
                v.read_only[0] = true;
                Content::Binary(v)
            } else {
                let mut left = text::load_doc(&copy, None)?;
                left.path = None;
                left.syntax = Some(text::extension(path));
                left.description = Some(label);
                left.read_only = true;
                let right = self.load_text(path, !path.exists())?;
                Content::Text(TextView::new(vec![left, right], &self.cfg))
            }
        };
        self.cfg.add_recent_path(&path.display().to_string());
        self.push_tab(content, Vec::new());
        if let Some(t) = self.tabs.last_mut() {
            t.temps = vec![std::sync::Arc::new(temp)];
        }
        Ok(())
    }

    fn open_conflict(&mut self, path: &Path) -> anyhow::Result<()> {
        let doc = text::load_doc(path, None)?;
        let parts = conflict::parse(&doc.lines)
            .ok_or_else(|| anyhow::anyhow!("{}", tr("msg.no_conflicts")))?;
        let mk = |lines: Vec<String>, desc: String, ro: bool, p: Option<PathBuf>| {
            let mut d = TextDoc::empty(p);
            d.lines = lines;
            d.eol = doc.eol;
            d.encoding = doc.encoding;
            d.bom = doc.bom;
            d.final_newline = doc.final_newline;
            d.description = Some(desc);
            d.read_only = ro;
            d
        };
        let name = path.display().to_string();
        let docs = vec![
            mk(
                parts.mine,
                format!("{} ({name})", tr("conflict.mine")),
                true,
                None,
            ),
            mk(
                parts.base,
                format!("{} → {name}", tr("conflict.merged")),
                false,
                Some(path.to_path_buf()),
            ),
            mk(
                parts.theirs,
                format!("{} ({name})", tr("conflict.theirs")),
                true,
                None,
            ),
        ];
        let mut v = TextView::new(docs, &self.cfg);
        if parts.has_base {
            v.auto_merge();
            v.bulk_edit = None;
        }
        v.status = trf("msg.conflicts_found", &[("n", parts.conflicts.to_string())]);
        self.push_tab(Content::Text(v), vec![path.to_path_buf()]);
        Ok(())
    }

    fn open_captured(&mut self, comparison: super::baseline_panel::CapturedComparison) {
        use super::baseline_panel::CapturedComparison;
        match comparison {
            CapturedComparison::Text(docs) => {
                let view = TextView::new(docs, &self.cfg);
                self.push_tab(Content::Text(view), Vec::new());
            }
            CapturedComparison::Source { docs, line } => {
                let mut view = TextView::new(docs, &self.cfg);
                view.goto_line(line);
                self.push_tab(Content::Text(view), Vec::new());
            }
            CapturedComparison::Bytes(files) => {
                let result = (|| -> anyhow::Result<_> {
                    let temp = tempfile::Builder::new()
                        .prefix("oxidemerge-baseline-view-")
                        .tempdir()?;
                    let mut paths = Vec::new();
                    for (side, (label, bytes)) in files.iter().enumerate() {
                        let dir = temp.path().join(side.to_string());
                        std::fs::create_dir(&dir)?;
                        let name = Path::new(label)
                            .file_name()
                            .unwrap_or_else(|| std::ffi::OsStr::new("captured.bin"));
                        let path = dir.join(name);
                        std::fs::write(&path, bytes)?;
                        paths.push(path);
                    }
                    let content = if paths.iter().all(|p| text::is_image_path(p)) {
                        match ImageView::load(paths.clone(), &self.cfg) {
                            Ok(mut view) => {
                                view.read_only.fill(true);
                                view.descriptions =
                                    files.iter().map(|(label, _)| Some(label.clone())).collect();
                                Content::Image(view)
                            }
                            Err(_) => {
                                let mut view = BinaryView::load(paths.clone())?;
                                view.read_only.fill(true);
                                Content::Binary(view)
                            }
                        }
                    } else {
                        let mut view = BinaryView::load(paths.clone())?;
                        view.read_only.fill(true);
                        Content::Binary(view)
                    };
                    Ok((content, temp))
                })();
                match result {
                    Ok((content, temp)) => {
                        self.push_tab(content, Vec::new());
                        if let Some(tab) = self.tabs.last_mut() {
                            tab.temps.push(std::sync::Arc::new(temp));
                        }
                    }
                    Err(e) => self.error(e.to_string()),
                }
            }
        }
    }

    fn push_tab(&mut self, mut content: Content, source: Vec<PathBuf>) {
        self.next_tab_id += 1;
        let (temps, remote) = self.take_pending_remote(&source);
        if let Content::Folder(v) = &mut content {
            for (i, r) in remote.iter().enumerate() {
                if r.as_ref().is_some_and(|r| r.manifest.is_some()) {
                    v.set_remote_side(i);
                }
            }
        }
        self.tabs.push(Tab {
            id: self.next_tab_id,
            content,
            source,
            descriptions: Vec::new(),
            origin: None,
            temps,
            remote,
        });
        self.active = self.tabs.len() - 1;
    }

    /// Claims the fetches of [`Self::pending_remote`] that `source` was staged from (by the
    /// local path each was staged to): the temporary folders to keep as long as the new tab
    /// does, and for each side the connection *Save* writes it back to.
    fn take_pending_remote(
        &mut self,
        source: &[PathBuf],
    ) -> (
        Vec<std::sync::Arc<tempfile::TempDir>>,
        Vec<Option<RemoteFile>>,
    ) {
        let mut temps = Vec::new();
        let mut remote = vec![None; source.len()];
        self.pending_remote.retain(|p| {
            let Some(i) = source.iter().position(|s| s == &p.local) else {
                return true;
            };
            temps.push(p.temp.clone());
            remote[i] = Some(p.file.clone());
            false
        });
        (temps, remote)
    }

    /// Drops a fetch of [`Self::pending_remote`] that no longer sits in any path field it
    /// could still be claimed from: the Open dialog it staged a path for was cancelled (or
    /// closed without comparing), or that path field was overwritten by a later fetch or a
    /// browsed path before a tab ever opened from it. Without this, its temporary folder (and
    /// the disk space it uses) would sit unclaimed for the rest of the session, since nothing
    /// else ever revisits [`Self::pending_remote`] once [`Self::use_remote_fetch`] adds to it.
    /// Run once a frame, alongside the dialogs that can add to or replace it.
    fn gc_pending_remote(&mut self) {
        if self.pending_remote.is_empty() {
            return;
        }
        let mut live: Vec<&Path> = self.start_paths.iter().map(Path::new).collect();
        if let Some(d) = &self.open_dlg {
            live.extend(d.paths.iter().map(Path::new));
        }
        self.pending_remote
            .retain(|p| live.iter().any(|l| *l == p.local));
    }

    /// Moves on from the active tab to the next or previous file with differences in the folder
    /// compare it was opened from, or with `conflicts`, that both sides changed.
    fn step_file(&mut self, forward: bool, conflicts: bool) {
        let ti = self.active;
        let Some(Origin { folder, rel, .. }) = self.tabs.get(ti).and_then(|t| t.origin.clone())
        else {
            return;
        };
        let Some(Content::Folder(f)) = self
            .tabs
            .iter()
            .find(|t| t.id == folder)
            .map(|t| t.content.view())
        else {
            return;
        };
        if adjacent_file(f, &self.cfg, &rel, forward, conflicts).is_none() {
            let message = f.no_more_files(forward, conflicts);
            self.notify(tr(message));
        } else if self.tabs[ti].is_modified() {
            self.prompt = Some(Prompt::SwitchFile(self.tabs[ti].id, forward, conflicts));
        } else {
            self.switch_file(ti, forward, conflicts);
        }
    }

    fn switch_file(&mut self, ti: usize, forward: bool, conflicts: bool) {
        let Some(Origin { folder, rel, .. }) = self.tabs.get(ti).and_then(|t| t.origin.clone())
        else {
            return;
        };
        let temps = self.tabs[ti].temps.clone();
        let cfg = &self.cfg;
        let Some(Content::Folder(f)) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == folder)
            .map(|t| t.content.view_mut())
        else {
            return;
        };
        let (origin_roots, origin_archives) = (
            f.roots.clone(),
            (0..f.n())
                .map(|s| f.archive_source(s).map(Path::to_path_buf))
                .collect(),
        );
        let Some(FolderAction::OpenFiles {
            paths,
            missing,
            read_only,
            descriptions,
            kind,
            rel,
            rules,
            snapshot_options,
        }) =
            adjacent_file(f, cfg, &rel, forward, conflicts).and_then(|i| f.reveal_and_open(i, cfg))
        else {
            // The folders were compared again while the tab waited for an answer.
            let message = f.no_more_files(forward, conflicts);
            self.notify(tr(message));
            return;
        };
        let count = self.tabs.len();
        self.open(OpenRequest {
            paths,
            missing,
            read_only,
            descriptions,
            kind,
            skip_recent: true,
            rules: Some(rules),
            snapshot_options,
            ..Default::default()
        });
        if self.tabs.len() == count {
            return;
        }
        let Some(mut tab) = self.tabs.pop() else {
            return;
        };
        let mut pass = false;
        match tab.content.view_mut() {
            Content::Text(v) if conflicts => {
                if !v.edge_conflict(forward) {
                    pass = true;
                    if forward {
                        v.first_diff();
                    } else {
                        v.last_diff();
                    }
                }
            }
            Content::Text(v) if forward => v.first_diff(),
            Content::Text(v) => v.last_diff(),
            Content::Binary(v) if forward => v.first_diff(),
            Content::Binary(v) => v.last_diff(),
            Content::Image(v) if forward => v.first_diff(),
            Content::Image(v) => v.last_diff(),
            // Pages are loaded in the background; they open there once loaded.
            Content::Web(v) => v.land = Some(forward),
            _ => {}
        }
        // A file without open conflicts is passed over on the next frame, unless it is the last.
        let next = pass
            .then(|| {
                self.tabs
                    .iter()
                    .find(|t| t.id == folder)
                    .and_then(|t| match t.content.view() {
                        Content::Folder(f) => Some((
                            f.adjacent_conflict_file(&self.cfg, &rel, forward).is_some(),
                            f.no_more_files(forward, true),
                        )),
                        _ => None,
                    })
            })
            .flatten();
        match next {
            Some((true, _)) => self.pass_on = Some((tab.id, forward)),
            Some((false, message)) => {
                self.notify(trf("msg.file_switched", &[("file", rel.clone())]));
                self.notify(tr(message));
            }
            None => self.notify(trf("msg.file_switched", &[("file", rel.clone())])),
        }
        tab.origin = Some(Origin {
            folder,
            rel,
            roots: origin_roots,
            archives: origin_archives,
        });
        tab.temps = temps;
        self.tabs[ti] = tab;
        self.active = ti;
    }

    /// Moves on from a file passed over without open conflicts, as [`Self::pass_on`] asks.
    fn pass_on(&mut self) {
        let Some((id, forward)) = self.pass_on.take() else {
            return;
        };
        if let Some(ti) = self.tabs.iter().position(|t| t.id == id) {
            if !self.tabs[ti].is_modified() {
                self.switch_file(ti, forward, true);
            }
        }
    }

    fn close_tab(&mut self, i: usize, force: bool) {
        if i >= self.tabs.len() {
            return;
        }
        if !force && self.tabs[i].is_modified() {
            self.close_queue.clear();
            self.prompt = Some(Prompt::CloseTab(self.tabs[i].id));
            return;
        }
        self.take_review_updates();
        self.refresh_saved_folders();
        let closed = self.tabs.remove(i);
        self.hand_down_remote(&closed);
        drop(closed);
        archive::forget_removed();
        self.shown_active = None;
        if i < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// After the folder compare `closed` closed: gives each tab still open from it, for its
    /// files on a side fetched from a server, that file's own place on the server, so saving
    /// one still uploads it, as the folder's write-back did while it was open.
    fn hand_down_remote(&mut self, closed: &Tab) {
        if !matches!(closed.content.view(), Content::Folder(_)) {
            return;
        }
        for (s, r) in closed.remote.iter().enumerate() {
            let Some(RemoteFile {
                conn,
                remote_path,
                manifest: Some(manifest),
                ..
            }) = r
            else {
                continue;
            };
            let Some(root) = closed.source.get(s) else {
                continue;
            };
            for t in &mut self.tabs {
                if t.origin.as_ref().map(|o| o.folder) != Some(closed.id) {
                    continue;
                }
                let paths = t.paths();
                t.remote.resize(paths.len(), None);
                for (slot, p) in t.remote.iter_mut().zip(&paths) {
                    let Some(rel) = p.strip_prefix(root).ok().and_then(|r| {
                        r.components()
                            .map(|c| c.as_os_str().to_str())
                            .collect::<Option<Vec<_>>>()
                    }) else {
                        continue;
                    };
                    if slot.is_some() || rel.is_empty() {
                        continue;
                    }
                    let rel = rel.join("/");
                    let fetched = manifest.files.get(&rel);
                    *slot = Some(RemoteFile {
                        conn: conn.clone(),
                        remote_path: remote::join(remote_path, &rel),
                        manifest: None,
                        stamp: fetched.map(|(stamp, _)| *stamp),
                        sent: fetched.map(|(_, hash)| *hash),
                        pending: false,
                    });
                }
            }
        }
    }

    /// Closes the tabs selected by `close`. Tabs with unsaved changes stay open and ask, one after
    /// the other, whether to save.
    fn close_tabs(&mut self, close: impl Fn(usize) -> bool) {
        let mut kept = Vec::new();
        let mut active = 0;
        let mut ask = Vec::new();
        let mut closed = Vec::new();
        for (i, t) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if close(i) {
                if !t.is_modified() {
                    closed.push(t);
                    continue;
                }
                ask.push(t.id);
            }
            if i <= self.active {
                active = kept.len();
            }
            kept.push(t);
        }
        self.tabs = kept;
        for t in &closed {
            self.hand_down_remote(t);
        }
        drop(closed);
        archive::forget_removed();
        self.active = active;
        self.shown_active = None;
        self.close_queue = ask;
        self.ask_next_close();
    }

    /// Asks about the next tab of [`Self::close_queue`] still unsaved.
    fn ask_next_close(&mut self) {
        while !self.close_queue.is_empty() {
            let id = self.close_queue.remove(0);
            if let Some(i) = self.tabs.iter().position(|t| t.id == id && t.is_modified()) {
                self.active = i;
                self.prompt = Some(Prompt::CloseTab(id));
                return;
            }
        }
    }

    /// The config to write: options given on the command line are for this run only, so
    /// those not changed since are written as they were loaded.
    fn stored_cfg(&self) -> Config {
        let mut c = self.cfg.clone();
        if let Some((cli, loaded)) = &self.cli_options {
            if c.language == cli.language {
                c.language = loaded.language.clone();
            }
            c.diff = unchanged_as_loaded(&c.diff, &cli.diff, &loaded.diff);
            if c.folder.method == cli.folder.method {
                c.folder.method = loaded.folder.method;
            }
            if c.folder.recursive == cli.folder.recursive {
                c.folder.recursive = loaded.folder.recursive;
            }
        }
        c
    }

    // ---------- commands ----------

    fn open_options(&mut self, tab: OptTab) {
        self.options_dlg = Some(OptionsDialog {
            cfg: self.cfg.clone(),
            tab,
            search: String::new(),
            rule_sets: rules::list(),
            rule_edit: None,
            rule_discard: false,
            rule_delete: None,
            rule_error: None,
            rule_note: None,
            rules_touched: false,
            rules_renamed: Vec::new(),
        });
    }

    /// Compare rules file of tab `i` (a name or path, "" = none), for the compares that use one.
    fn tab_rules(&self, i: usize) -> Option<String> {
        match self.tabs.get(i).map(|t| t.content.view())? {
            Content::Text(v) => Some(v.rules.clone().unwrap_or_else(|| self.cfg.rules.clone())),
            Content::Folder(v) => Some(v.rules.clone()),
            _ => None,
        }
    }

    /// Makes tab `i` compare with the rules file `name` ("" = none).
    fn use_rules(&mut self, i: usize, name: String) {
        match self.tabs.get_mut(i).map(|t| t.content.view_mut()) {
            Some(Content::Text(v)) => v.set_rules(Some(name)),
            Some(Content::Folder(v)) => {
                v.rules = name;
                v.refresh(&self.cfg, &self.named_filters);
            }
            _ => {}
        }
    }

    /// Points the options and the compares that use the rules file `old` at `new` ("" = none).
    fn rules_renamed(&mut self, old: &RuleSet, new: &str) {
        let all = rules::list();
        let uses = |r: &str| rules::picks(r, old, &all);
        if uses(&self.cfg.rules) {
            self.cfg.rules = new.to_string();
        }
        let cfg = self.cfg.clone();
        for t in &mut self.tabs {
            match t.content.view_mut() {
                Content::Text(v) if v.rules.as_deref().is_some_and(uses) => {
                    v.set_rules(Some(new.to_string()));
                }
                Content::Folder(f) if uses(&f.rules) => f.set_rules(&cfg, new.to_string()),
                Content::Diffs(v) if uses(v.rules()) => v.rules_renamed(new),
                _ => {}
            }
        }
    }

    /// Rules files were changed: the options and the text compares use them at once, folder
    /// compares when refreshed.
    fn rules_changed(&mut self) {
        rules::apply_selected(&mut self.cfg);
        for t in &mut self.tabs {
            if let Content::Text(v) = t.content.view_mut() {
                v.reload_rules();
            }
        }
    }

    fn open_rule_dialog(&mut self, i: usize, draft: RuleDraft) {
        let Some(id) = self.tabs.get(i).map(|t| t.id) else {
            return;
        };
        let target = self
            .tab_rules(i)
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| rules::DEFAULT_NAME.to_string());
        self.rule_dlg = Some(RuleDialog {
            tab: id,
            draft,
            names: rules::list().into_iter().map(|s| s.name).collect(),
            target,
        });
    }

    /// Adds the rule of the dialog to its rules file, which the compare it was picked in then
    /// uses, and so does the folder compare that file compare was opened from when they used the
    /// same rules: its name, and whether that folder compare has to be refreshed.
    fn save_rule(&mut self, d: &RuleDialog) -> std::io::Result<(String, bool)> {
        let target = Path::new(&d.target);
        // Only a whole path names a file; anything else is a name, made fit to be one.
        let is_file = target.is_absolute() && rules::is_rules_file(target);
        let name = if is_file {
            rules::display_name(&d.target).to_string()
        } else {
            rules::file_name(&d.target)
        };
        let found = match rules::find(&d.target) {
            None if !is_file => rules::find(&name),
            found => found,
        };
        let mut set = match found {
            Some(set) => set,
            None => {
                let set = if is_file {
                    RuleSet {
                        name,
                        path: target.to_path_buf(),
                        ..RuleSet::default()
                    }
                } else {
                    let dir =
                        rules::dir().ok_or_else(|| std::io::Error::other(tr("msg.no_config")))?;
                    RuleSet::new(&dir, &name)
                };
                if set.path.exists() {
                    return Err(std::io::Error::other(trf(
                        "rules.unreadable",
                        &[("name", set.name)],
                    )));
                }
                set
            }
        };
        set.add(&d.draft);
        set.save()?;
        // The name, unless it finds other rules (or none): then the file.
        let spec = if rules::find(&set.name).is_some_and(|s| s.path == set.path) {
            set.name.clone()
        } else {
            set.path.display().to_string()
        };
        self.rules_changed();
        let cfg = self.cfg.clone();
        let mut folder_stale = false;
        if let Some(i) = self.tabs.iter().position(|t| t.id == d.tab) {
            let before = self.tab_rules(i);
            let origin = self.tabs[i].origin.as_ref().map(|o| o.folder);
            let folder = self.tabs.iter_mut().find(|t| Some(t.id) == origin);
            if let Some(Content::Folder(f)) = folder.map(|t| t.content.view_mut()) {
                if Some(&f.rules) == before.as_ref() || f.rules == spec {
                    f.set_rules(&cfg, spec.clone());
                    folder_stale = true;
                }
            }
            let follows =
                matches!(self.tabs[i].content.view(), Content::Text(v) if v.rules.is_none());
            if !(follows && self.cfg.rules == spec) {
                self.use_rules(i, spec);
            }
        }
        Ok((set.name, folder_stale))
    }

    fn rule_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.rule_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut save = false;
        let problem = d.draft.problem();
        let modal = egui::Modal::new(egui::Id::unique("rule_dialog")).show(ctx, |ui| {
            ui.set_width(600.0);
            if widgets::dialog_title(ui, tr("cmd.add_rule")) {
                keep = false;
            }
            let kinds: Vec<(RuleKind, &str)> =
                RuleKind::ALL.iter().map(|&k| (k, tr(k.key()))).collect();
            widgets::segmented(ui, &mut d.draft.kind, &kinds);
            ui.add_space(8.0);
            let (a, b) = match d.draft.kind {
                RuleKind::IgnoreLine => ("rules.lines", None),
                RuleKind::Substitute => ("rules.find", Some("rules.replace")),
                RuleKind::IgnoreBlock => ("rules.block_start", Some("rules.block_end")),
            };
            egui::Grid::new("rule_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    let field = |ui: &mut egui::Ui, text: &mut String| {
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .code_editor()
                                .margin(vec2(8.0, 5.0))
                                .desired_width(440.0),
                        );
                    };
                    ui.label(tr(a));
                    field(ui, &mut d.draft.a);
                    ui.end_row();
                    if let Some(b) = b {
                        ui.label(tr(b));
                        field(ui, &mut d.draft.b);
                        ui.end_row();
                    }
                    ui.label(tr("rules.file"));
                    let shown = rules::display_name(&d.target).to_string();
                    egui::ComboBox::from_id_salt("rule_target")
                        .selected_text(&shown)
                        .width(440.0)
                        .show_ui(ui, |ui| {
                            for name in &d.names {
                                ui.selectable_value(&mut d.target, name.clone(), name);
                            }
                        });
                    ui.end_row();
                    ui.label(tr("rules.new_file"));
                    ui.add(
                        egui::TextEdit::singleline(&mut d.target)
                            .margin(vec2(8.0, 5.0))
                            .desired_width(440.0),
                    );
                    ui.end_row();
                });
            let shown_name = rules::display_name(&d.target);
            let new_file = !Path::new(&d.target).is_file()
                && !d.names.iter().any(|n| rules::same_name(n, shown_name));
            let note = match &problem {
                Some(p) if p.trim().is_empty() => Some(tr("rules.need_pattern").to_string()),
                Some(p) => Some(trf("opt.invalid_regex", &[("pattern", p.clone())])),
                None => None,
            };
            if let Some(note) = note {
                ui.label(egui::RichText::new(note).small().color(pal(ui).danger));
            } else if new_file {
                ui.label(
                    egui::RichText::new(trf(
                        "rules.will_create",
                        &[("name", rules::file_name(shown_name))],
                    ))
                    .small()
                    .color(pal(ui).text_dim),
                );
            }
            ui.label(
                egui::RichText::new(tr("rules.add_hint"))
                    .small()
                    .color(pal(ui).text_dim),
            );
            widgets::dialog_footer(ui, |ui| {
                let ok = ui
                    .add_enabled_ui(problem.is_none() && !d.target.trim().is_empty(), |ui| {
                        widgets::primary_button(ui, tr("btn.ok")).clicked()
                    });
                if ok.inner {
                    save = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    keep = false;
                }
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if save {
            match self.save_rule(&d) {
                Ok((name, refresh)) => self.notify(trf(
                    if refresh {
                        "msg.rule_added_refresh"
                    } else {
                        "msg.rule_added"
                    },
                    &[("name", name)],
                )),
                Err(e) => {
                    self.error(e.to_string());
                    self.rule_dlg = Some(d);
                }
            }
        } else if keep {
            self.rule_dlg = Some(d);
        }
    }

    fn palette_entries(&self) -> Vec<PaletteEntry<PaletteAction>> {
        let entry = |icon, label: String, detail: String, action| PaletteEntry {
            icon,
            label,
            detail,
            action,
        };
        // Only what does something in the active tab.
        let menus = self.menus();
        let mut out: Vec<_> = commands::PALETTE
            .iter()
            .filter(|&&c| menus.enabled(c))
            .map(|&c| {
                let label = plain(tr(c.label()));
                entry(c.icon(), label, c.shown_key(), PaletteAction::Cmd(c))
            })
            .collect();
        let compare_as = plain(tr("cmd.compare_as"));
        for k in [
            FileKind::Text,
            FileKind::Binary,
            FileKind::Image,
            FileKind::Web,
        ] {
            let c = Cmd::CompareAs(k);
            if !menus.enabled(c) {
                continue;
            }
            let label = format!("{compare_as}: {}", kind_label(Some(k)));
            out.push(entry(c.icon(), label, String::new(), PaletteAction::Cmd(c)));
        }
        let on = tr("palette.on").to_string();
        if let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) {
            if v.n() == 2 && !v.table_mode {
                let detail = if v.unified { on.clone() } else { String::new() };
                let label = plain(tr("cmd.inline_view"));
                out.push(entry(Icon::Inline, label, detail, PaletteAction::Inline));
            }
        }
        let toggles: [(&str, ConfigFlag); 15] = [
            ("view.line_numbers", |c| &mut c.show_line_numbers),
            ("view.location_pane", |c| &mut c.show_location_pane),
            ("view.line_detail", |c| &mut c.show_line_detail),
            ("view.edit_markers", |c| &mut c.show_edit_markers),
            ("view.folder_file_pane", |c| &mut c.folder_file_pane),
            ("view.whitespace", |c| &mut c.show_whitespace),
            ("view.syntax", |c| &mut c.syntax_highlight),
            ("view.tree_mode", |c| &mut c.tree_mode),
            ("menu.open_reports", |c| &mut c.open_reports),
            ("menu.pdf_letter", |c| &mut c.pdf_letter),
            ("menu.report_hide_paths", |c| &mut c.report_hide_paths),
            ("opt.color_blind", |c| &mut c.color_blind_colors),
            ("opt.high_contrast", |c| &mut c.high_contrast),
            ("opt.reduce_motion", |c| &mut c.reduce_motion),
            ("opt.compact", |c| &mut c.compact_density),
        ];
        let mut cfg = self.cfg.clone();
        for (key, f) in toggles {
            let detail = if *f(&mut cfg) {
                on.clone()
            } else {
                String::new()
            };
            out.push(entry(
                Icon::Check,
                plain(tr(key)),
                detail,
                PaletteAction::Toggle(f),
            ));
        }
        let theme = plain(tr("view.theme"));
        for (m, icon) in [
            (ThemeMode::System, Icon::Settings),
            (ThemeMode::Light, Icon::Sun),
            (ThemeMode::Dark, Icon::Moon),
        ] {
            let detail = if self.cfg.theme == m {
                on.clone()
            } else {
                String::new()
            };
            let label = format!("{theme}: {}", tr(m.key()));
            out.push(entry(icon, label, detail, PaletteAction::Theme(m)));
        }
        let recent = plain(tr("label.recent"));
        for paths in &self.cfg.recent_compares {
            let label = recent_label(paths);
            let icon = if paths.first().is_some_and(|p| web::is_url(p)) {
                Icon::Globe
            } else {
                Icon::Clock
            };
            out.push(entry(
                icon,
                label,
                recent.clone(),
                PaletteAction::Compare(paths.clone()),
            ));
        }
        let options = plain(tr("menu.options"));
        for &(tab, key) in OPT_INDEX {
            let (icon, tab_key) = OptTab::ALL
                .iter()
                .find(|(t, ..)| *t == tab)
                .map(|&(_, i, k)| (i, k))
                .unwrap_or((Icon::Settings, "opt.tab_general"));
            let detail = format!("{options} › {}", tr(tab_key));
            out.push(entry(
                icon,
                plain(tr(key)),
                detail,
                PaletteAction::Settings(tab),
            ));
        }
        out
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some((palette, entries)) = &mut self.palette else {
            return;
        };
        match palette.show(ctx, entries) {
            PaletteOutcome::Keep => {}
            PaletteOutcome::Close => self.palette = None,
            PaletteOutcome::Pick(action) => {
                self.palette = None;
                match action {
                    PaletteAction::Cmd(c) => self.run(c, ctx),
                    PaletteAction::Compare(paths) => self.open(OpenRequest {
                        paths: paths.iter().map(PathBuf::from).collect(),
                        ..Default::default()
                    }),
                    PaletteAction::Settings(tab) => self.open_options(tab),
                    PaletteAction::Theme(m) => self.cfg.theme = m,
                    PaletteAction::Toggle(f) => {
                        let v = f(&mut self.cfg);
                        *v = !*v;
                    }
                    PaletteAction::Inline => {
                        if let Some(Content::Text(v)) =
                            self.tabs.get_mut(self.active).map(|t| t.content.view_mut())
                        {
                            v.unified = !v.unified;
                        }
                    }
                }
            }
        }
    }

    /// Folding and wrapping are shared by every text compare, so a toggle in one view, the
    /// menu, the palette or the options applies to all of them.
    fn sync_text_layout(&mut self) {
        let mut want = (self.cfg.collapse_unchanged, self.cfg.word_wrap);
        if want == self.text_layout {
            if let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) {
                want = (v.collapse, v.wrap);
            }
        }
        (self.cfg.collapse_unchanged, self.cfg.word_wrap) = want;
        self.text_layout = want;
        for t in &mut self.tabs {
            if let Content::Text(v) = t.content.view_mut() {
                if v.collapse != want.0 {
                    v.set_collapse(want.0);
                }
                v.wrap = want.1;
            }
        }
    }

    /// Long folder compares that finish while the window is in the background or their tab
    /// is hidden ask for attention and leave a toast.
    fn announce_finished(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let mut running = false;
        let mut done = Vec::new();
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let Content::Folder(v) = &mut t.content else {
                continue;
            };
            v.poll_background();
            running |= v.is_running();
            let unattended = !focused || i != self.active;
            if v.take_finished().is_some_and(|d| d >= LONG_COMPARE) && unattended {
                done.push(v.title());
            }
        }
        if running {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !done.is_empty() && !focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        for name in done {
            self.push_toast(trf("msg.compare_done", &[("name", name)]), false, None);
        }
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        if cmd == Cmd::Refresh {
            self.rules_changed();
        }
        if matches!(
            cmd,
            Cmd::HtmlReport
                | Cmd::Slideshow
                | Cmd::ExportPdf
                | Cmd::Print
                | Cmd::Patch
                | Cmd::Statistics
                | Cmd::SaveComparison
        ) {
            if let Some(Content::Text(v)) =
                self.tabs.get_mut(self.active).map(|t| t.content.view_mut())
            {
                v.commit_pending_edit();
            }
        }
        let cfg = self.cfg.clone();
        match cmd {
            Cmd::New => {
                let mut v = TextView::new(vec![blank_doc(), blank_doc()], &self.cfg);
                v.status = tr("msg.new_hint").to_string();
                self.push_tab(Content::Text(v), Vec::new());
            }
            Cmd::CompareClipboard => self.start_clipboard_read(),
            Cmd::SaveWorkspace => self.workspace_save_dlg = Some((String::new(), true)),
            Cmd::OpenWorkspace(i) => self.open_workspace(i),
            Cmd::ManageWorkspaces => self.manage_workspaces = true,
            Cmd::DeleteWorkspace(i) => self.delete_workspace(i),
            Cmd::OpenProject => {
                if let Some(p) = rfd::FileDialog::new()
                    .set_title(tr("menu.open_project"))
                    .add_filter(
                        tr("label.project"),
                        &[project::EXT, "WinMerge", project::WINMERGE_EXT],
                    )
                    .add_filter(
                        tr("label.saved_compare"),
                        &[saved::EXT, crate::core::review::EXT],
                    )
                    .pick_file()
                {
                    self.open(OpenRequest {
                        paths: vec![p],
                        ..Default::default()
                    });
                }
            }
            Cmd::SaveProject => self.save_project(),
            Cmd::SaveJob => self.begin_save_job(),
            Cmd::RunJob => {
                if self.run_job.as_ref().is_some_and(|r| r.is_running()) {
                    return;
                }
                if let Some(p) = rfd::FileDialog::new()
                    .set_title(tr("dlg.run_job"))
                    .add_filter(tr("label.job"), &["oxjob", "json"])
                    .pick_file()
                {
                    self.start_run_job(p);
                }
            }
            Cmd::ApplyPatch => self.apply_patch(),
            Cmd::Open => self.open_dlg = Some(self.new_open_dialog(None)),
            Cmd::OpenConflict => {
                if let Some(p) = rfd::FileDialog::new()
                    .set_title(tr("menu.open_conflict"))
                    .pick_file()
                {
                    if let Err(e) = self.open_conflict(&p) {
                        self.error(e.to_string());
                    }
                }
            }
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Help => self.help = true,
            Cmd::Options => self.open_options(OptTab::General),
            Cmd::CompareOptions => self.open_options(OptTab::Compare),
            Cmd::ToggleCollapse => self.cfg.collapse_unchanged ^= true,
            Cmd::ToggleWrap => self.cfg.word_wrap ^= true,
            Cmd::ToggleFilePane => self.cfg.folder_file_pane ^= true,
            Cmd::Palette => {
                let entries = self.palette_entries();
                self.palette = Some((Palette::default(), entries));
            }
            Cmd::Filters => self.filters_dlg = true,
            Cmd::EditRules => self.open_options(OptTab::Rules),
            Cmd::ZoomIn => self.cfg.font_size = (self.cfg.font_size + 1.0).min(40.0),
            Cmd::ZoomOut => self.cfg.font_size = (self.cfg.font_size - 1.0).max(7.0),
            Cmd::ZoomReset => self.cfg.font_size = 14.0,
            Cmd::NextTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            Cmd::PrevTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                }
            }
            Cmd::CloseTab => self.close_tab(self.active, false),
            Cmd::CloseTabAt(i) => self.close_tab(i, false),
            Cmd::CloseOtherTabs(keep) => self.close_tabs(|i| i != keep),
            Cmd::CloseTabsToRight(keep) => self.close_tabs(|i| i > keep),
            Cmd::CloseAllTabs => self.close_tabs(|_| true),
            Cmd::SaveAll => {
                let mut errors = Vec::new();
                let mut saved = Vec::new();
                for i in 0..self.tabs.len() {
                    if !self.tabs[i].is_modified() {
                        continue;
                    }
                    if let Err(e) = self.tabs[i].save() {
                        errors.push(e.to_string());
                    }
                    // What was written is uploaded even when another side failed to save.
                    saved.push(self.tabs[i].id);
                }
                errors.extend(self.after_save(&saved));
                if !errors.is_empty() {
                    self.error(errors.join("\n"));
                }
            }
            Cmd::Patch | Cmd::HtmlReport | Cmd::Slideshow | Cmd::ExportPdf | Cmd::Print
                if matches!(self.tabs.get(self.active).map(|t| t.content.view()),
                    Some(Content::Folder(v)) if v.is_running()) =>
            {
                self.notify(tr("msg.comparing"));
            }
            Cmd::Patch => {
                self.patch_dlg = Some(PatchDialog {
                    style: self.cfg.patch_style,
                    context: self.cfg.patch_context,
                    path: String::new(),
                })
            }
            Cmd::HtmlReport => self.save_html_report(),
            Cmd::AllDiffs => self.show_all_diffs(),
            Cmd::Slideshow => self.save_slideshow(),
            Cmd::ExportPdf => self.save_pdf(false),
            Cmd::Print => self.save_pdf(true),
            Cmd::SaveComparison => self.save_comparison(),
            Cmd::Statistics => self.show_statistics(),
            Cmd::OpenRemote => {
                if self.open_dlg.is_none() {
                    let mut d = self.new_open_dialog(None);
                    d.paths = self.start_paths.clone();
                    d.three_way = self.start_three_way;
                    self.open_dlg = Some(d);
                }
                self.start_remote_connect(RemoteTarget::Open(0));
            }
            Cmd::ReloadRemote => self.reload_remote(false),
            Cmd::GitCompare => {
                let path = self
                    .tabs
                    .get(self.active)
                    .and_then(|t| t.source.last())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.git_dlg = Some(GitDialog {
                    path,
                    rev: String::new(),
                    commits: Vec::new(),
                    commits_for: String::new(),
                    commits_job: None,
                    typed: (String::new(), std::time::Instant::now()),
                    rev_hint: vcs::Vcs::Git.rev_hint(),
                    hint_for: String::new(),
                    hint_job: None,
                });
            }
            Cmd::CompareAs(kind) => {
                if let Some(tab) = self.tabs.get(self.active) {
                    if !matches!(tab.content, Content::Folder(_)) && tab.source.len() >= 2 {
                        let paths = tab.source.clone();
                        let rules = match tab.content.view() {
                            Content::Text(v) => v.rules.clone(),
                            _ => None,
                        };
                        // The new tab shows the same staged copies: it keeps them, and writes a
                        // remote side back to its server, as this one does.
                        let (temps, remote) = (tab.temps.clone(), tab.remote.clone());
                        let before = self.tabs.len();
                        self.open(OpenRequest {
                            paths,
                            kind: Some(kind),
                            rules,
                            ..Default::default()
                        });
                        if self.tabs.len() > before {
                            if let Some(new) = self.tabs.last_mut() {
                                new.temps.extend(temps);
                                for (slot, r) in new.remote.iter_mut().zip(remote) {
                                    if slot.is_none() {
                                        *slot = r;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                let named = self.named_filters.clone();
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                if let Content::Web(w) = &mut tab.content {
                    if cmd == Cmd::Refresh {
                        w.reload(&cfg);
                        return;
                    }
                }
                let mut err: Option<String> = None;
                if let Content::Text(v) = tab.content.view_mut() {
                    // Commands read the comparison, which typing may have put off.
                    v.settle_diff();
                }
                match tab.content.view_mut() {
                    Content::Text(v) => match cmd {
                        Cmd::Save => {
                            if let Err(e) = v.save_modified() {
                                err = Some(e.to_string());
                            }
                        }
                        Cmd::SaveAs(p) if p < v.n() => {
                            if let Err(e) = v.save_pane_as(p) {
                                err = Some(e.to_string());
                            }
                        }
                        Cmd::Undo => v.undo(),
                        Cmd::Redo => v.redo(),
                        Cmd::Find => v.open_find(),
                        Cmd::Replace => v.open_replace(),
                        Cmd::FindNext => v.find_next(true),
                        Cmd::FindPrev => v.find_next(false),
                        Cmd::GotoLine => v.goto = Some(String::new()),
                        Cmd::NextDiff if !v.next_diff(false) => v.step_file = Some(true),
                        Cmd::PrevDiff if !v.prev_diff(false) => v.step_file = Some(false),
                        Cmd::FirstDiff => v.first_diff(),
                        Cmd::LastDiff => v.last_diff(),
                        Cmd::NextConflict if !v.next_diff(true) => v.step_conflict = Some(true),
                        Cmd::PrevConflict if !v.prev_diff(true) => v.step_conflict = Some(false),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        Cmd::CopyAllRight => v.copy_all(0, 1),
                        Cmd::CopyAllLeft => {
                            let n = v.n();
                            v.copy_all(n - 1, n - 2);
                        }
                        Cmd::AutoMerge => v.auto_merge(),
                        Cmd::Refresh => v.refresh(),
                        Cmd::ToggleBookmark => v.toggle_bookmark(),
                        Cmd::NextBookmark => v.goto_bookmark(true),
                        Cmd::PrevBookmark => v.goto_bookmark(false),
                        Cmd::ClearBookmarks => v.clear_bookmarks(),
                        Cmd::SyncPoint => v.set_sync_point(),
                        Cmd::ClearSyncPoints => v.clear_sync_points(),
                        Cmd::TakeBoth(left_first) => v.take_both(left_first),
                        Cmd::Indent(outdent) => v.indent(outdent),
                        Cmd::DuplicateLines => v.duplicate_lines(),
                        Cmd::MoveLines(up) => v.move_lines(up),
                        Cmd::BookmarkComment => v.edit_note(),
                        _ => {}
                    },
                    Content::Binary(v) => match cmd {
                        Cmd::Save => {
                            if let Err(e) = v.save() {
                                err = Some(e.to_string());
                            }
                        }
                        Cmd::Undo => v.undo(),
                        Cmd::Redo => v.redo(),
                        Cmd::Find => v.open_find(),
                        Cmd::FindNext => v.find_next(true),
                        Cmd::FindPrev => v.find_next(false),
                        Cmd::GotoLine => v.goto = Some(String::new()),
                        Cmd::NextDiff if !v.next_diff() => v.step_file = Some(true),
                        Cmd::PrevDiff if !v.prev_diff() => v.step_file = Some(false),
                        Cmd::FirstDiff => v.first_diff(),
                        Cmd::LastDiff => v.last_diff(),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        Cmd::CopyAllRight => v.copy_all(true),
                        Cmd::CopyAllLeft => v.copy_all(false),
                        _ => {}
                    },
                    Content::Image(v) => match cmd {
                        Cmd::Save => {
                            if let Err(e) = v.save() {
                                err = Some(e.to_string());
                            }
                        }
                        Cmd::Undo => v.undo(),
                        Cmd::NextDiff if !v.next_diff() => v.step_file = Some(true),
                        Cmd::PrevDiff if !v.prev_diff() => v.step_file = Some(false),
                        Cmd::FirstDiff => v.first_diff(),
                        Cmd::LastDiff => v.last_diff(),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        _ => {}
                    },
                    Content::Folder(v) => match cmd {
                        Cmd::Save => {
                            let errors = v.save_archives().1;
                            if !errors.is_empty() {
                                err = Some(errors.join("\n"));
                            }
                        }
                        Cmd::Refresh => v.refresh(&cfg, &named),
                        Cmd::Find => v.focus_search(),
                        Cmd::CopyRight => v.copy_selected(true, &cfg),
                        Cmd::CopyLeft => v.copy_selected(false, &cfg),
                        Cmd::AutoMerge => v.request_merge(),
                        Cmd::SyncFolders => v.request_sync(),
                        Cmd::SelectPattern => v.open_select_pattern(),
                        Cmd::NextDiff => v.step_diff(true, &cfg),
                        Cmd::PrevDiff => v.step_diff(false, &cfg),
                        Cmd::NextConflict => v.step_conflict(true, &cfg),
                        Cmd::PrevConflict => v.step_conflict(false, &cfg),
                        Cmd::FirstDiff => v.edge_diff(true, &cfg),
                        Cmd::LastDiff => v.edge_diff(false, &cfg),
                        _ => {}
                    },
                    Content::Diffs(v) => match cmd {
                        Cmd::Save => {
                            if let Err(e) = v.save_modified() {
                                err = Some(e.to_string());
                            }
                        }
                        Cmd::Undo => v.undo(),
                        Cmd::Find => v.focus_search(),
                        Cmd::Refresh => v.refresh(&cfg),
                        Cmd::NextDiff => v.step(true),
                        Cmd::PrevDiff => v.step(false),
                        Cmd::FirstDiff => v.first(),
                        Cmd::LastDiff => v.last(),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        _ => {}
                    },
                    Content::Web(v) => {
                        if let Some(f) = &mut v.formatted {
                            match cmd {
                                Cmd::NextDiff if !f.next_diff() => f.step_file = Some(true),
                                Cmd::PrevDiff if !f.prev_diff() => f.step_file = Some(false),
                                Cmd::FirstDiff => f.first_diff(),
                                Cmd::LastDiff => f.last_diff(),
                                _ => {}
                            }
                        }
                    }
                }
                let mut errors: Vec<String> = err.into_iter().collect();
                if cmd == Cmd::Save {
                    // What was written is uploaded even when another side failed to save.
                    if let Some(id) = self.tabs.get(self.active).map(|t| t.id) {
                        errors.extend(self.after_save(&[id]));
                    }
                }
                if !errors.is_empty() {
                    self.error(errors.join("\n"));
                }
                self.take_step_file();
            }
        }
    }

    /// Acts on a view of the active tab having run past its last or first difference.
    fn take_step_file(&mut self) {
        let mut notice = None;
        let step = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| match t.content.view_mut() {
                Content::Text(v) => match v.step_conflict.take() {
                    Some(forward) => Some((forward, true)),
                    None => v.step_file.take().map(|f| (f, false)),
                },
                Content::Binary(v) => v.step_file.take().map(|f| (f, false)),
                Content::Image(v) => v.step_file.take().map(|f| (f, false)),
                Content::Web(v) => v
                    .formatted
                    .as_mut()
                    .and_then(|f| f.step_file.take())
                    .map(|f| (f, false)),
                Content::Folder(v) => {
                    notice = v.take_notice();
                    None
                }
                _ => None,
            });
        if let Some(msg) = notice {
            self.notify(tr(msg));
        }
        if let Some((forward, conflicts)) = step {
            self.step_file(forward, conflicts);
        }
    }

    /// Saves the report of the active text or folder compare. HTML has every difference, of
    /// every changed text file for a folder; the save dialog also offers XML, and for a folder
    /// CSV and plain text, which list the files.
    fn save_html_report(&mut self) {
        let title = tr("menu.export_html").replace("...", "");
        match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => {
                let name = report::html_file_name(&v.titles());
                let Some(path) = self.report_path(&title, &name, &[HTML, XML]) else {
                    return;
                };
                let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view())
                else {
                    return;
                };
                match ReportFormat::from_path(&path) {
                    ReportFormat::Xml => {
                        let job = v.report_job(ReportFormat::Xml, &self.cfg.rules);
                        self.spawn_report(path, job);
                    }
                    _ => self.export_text_html_to(path),
                }
            }
            Some(Content::Folder(v)) => {
                let name = report::html_file_name(&v.display_roots);
                let filters = [HTML, ("CSV", &["csv"][..]), ("Text", &["txt"][..]), XML];
                let Some(path) = self.report_path(&title, &name, &filters) else {
                    return;
                };
                let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view())
                else {
                    return;
                };
                match ReportFormat::from_path(&path) {
                    ReportFormat::Html | ReportFormat::Slideshow => {
                        let job = v.report_with_diffs_job(&self.cfg);
                        self.spawn_report(path, job);
                    }
                    format => {
                        let text = v.report(format, &self.cfg);
                        self.spawn_report(path, move || text);
                    }
                }
            }
            _ => self.error(tr("msg.report_folder_only")),
        }
    }

    /// Saves the PDF report of the active text or folder compare, with every difference; to
    /// `print`, writes it to a temporary file and opens it in the system's viewer instead.
    fn save_pdf(&mut self, print: bool) {
        use crate::core::pdf_report::{Layout, PageSize, PdfOptions};
        if self.report_job.is_some() {
            self.notify(tr("msg.report_writing"));
            return;
        }
        let mut pdf = PdfOptions {
            page: if self.cfg.pdf_letter {
                PageSize::Letter
            } else {
                PageSize::A4
            },
            color_blind: self.cfg.color_blind_colors,
            ..Default::default()
        };
        let lang = crate::i18n::current_language().to_string();
        let (name, job): (String, crate::core::pdf_report::PdfJob) =
            match self.tabs.get(self.active).map(|t| t.content.view()) {
                Some(Content::Text(v)) => {
                    // The inline view prints as one column, on portrait pages.
                    if v.unified && v.titles().len() == 2 {
                        pdf.layout = Layout::Unified;
                        pdf.portrait = true;
                    }
                    (
                        report::html_file_name(&v.titles()),
                        v.pdf_job(&self.cfg.rules, pdf, lang),
                    )
                }
                Some(Content::Folder(v)) => (
                    report::html_file_name(&v.display_roots),
                    Box::new(v.pdf_job(&self.cfg, pdf, lang)),
                ),
                _ => {
                    self.error(tr("msg.report_folder_only"));
                    return;
                }
            };
        let stem = name.strip_suffix(".html").unwrap_or(&name).to_string();
        let mut print_file = None;
        let path = if print {
            let made = tempfile::Builder::new()
                .prefix("oxidemerge-print-")
                .suffix(&format!("-{stem}.pdf"))
                .tempfile();
            let (file, temp) = match made {
                Ok(f) => f.into_parts(),
                Err(e) => {
                    self.error(format!("{}: {e}", std::env::temp_dir().display()));
                    return;
                }
            };
            let path = temp.to_path_buf();
            self.prints.push(temp);
            print_file = Some(file);
            path
        } else {
            let title = tr("menu.export_pdf");
            let Some(path) = rfd::FileDialog::new()
                .set_title(title)
                .add_filter("PDF", &["pdf"])
                .set_file_name(&stem)
                .save_file()
            else {
                return;
            };
            if crate::core::text::extension(&path) == "pdf" {
                path
            } else {
                let mut p = path.into_os_string();
                p.push(".pdf");
                PathBuf::from(p)
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let file = path.clone();
        std::thread::spawn(move || {
            let written = job().and_then(|bytes| match print_file {
                Some(mut f) => std::io::Write::write_all(&mut f, &bytes)
                    .map_err(|e| format!("{}: {e}", file.display())),
                None => crate::core::fsutil::write_atomic_with(&file, &bytes, false)
                    .map_err(|e| format!("{e:#}")),
            });
            let _ = tx.send(written);
        });
        self.report_job = Some((path, rx));
        self.report_opens = print;
        self.notify(tr("msg.report_writing"));
    }

    /// Saves an HTML slideshow of the active text or folder compare: one difference per slide.
    fn save_slideshow(&mut self) {
        let (name, job): (String, Box<dyn FnOnce() -> String + Send>) =
            match self.tabs.get(self.active).map(|t| t.content.view()) {
                Some(Content::Text(v)) => (
                    report::html_file_name(&v.titles()),
                    v.report_job(ReportFormat::Slideshow, &self.cfg.rules),
                ),
                Some(Content::Folder(v)) => (
                    report::html_file_name(&v.display_roots),
                    Box::new(v.slideshow_job(&self.cfg)),
                ),
                _ => {
                    self.error(tr("msg.report_folder_only"));
                    return;
                }
            };
        // "a vs b.html" becomes "a vs b_slides.html".
        let name = match name.strip_suffix(".html") {
            Some(stem) => format!("{stem}_slides.html"),
            None => name,
        };
        self.write_html_report(tr("menu.slideshow"), &name, job);
    }

    /// Selects in the folder compare the file its tab of all differences moved to, so both
    /// show the same file.
    fn sync_diffs_selection(&mut self) {
        let Some(Content::Diffs(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.content) else {
            return;
        };
        let (Some(rel), folder) = (v.take_moved_to(), v.folder) else {
            return;
        };
        let cfg = &self.cfg;
        if let Some(Content::Folder(f)) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == folder)
            .map(|t| t.content.view_mut())
        {
            f.select_rel(&rel, cfg);
        }
    }

    /// Opens the differences of every changed file of the active folder compare in one tab.
    fn show_all_diffs(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Content::Folder(f) = tab.content.view() else {
            return;
        };
        if f.is_running() {
            self.notify(tr("msg.comparing"));
            return;
        }
        let folder = tab.id;
        let src = f.diffs_source(&self.cfg);
        let at = f.cursor_position();
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.content, Content::Diffs(v) if v.folder == folder))
        {
            self.active = i;
            let Content::Diffs(v) = &mut self.tabs[i].content else {
                return;
            };
            let mut outdated = false;
            if v.lists_all(&src) {
                v.set_stopped(src.stopped);
            } else if v.is_modified() || v.can_undo() {
                outdated = true;
            } else {
                v.update_from(src);
            }
            if let Some((rel, at)) = at {
                v.show_file(&rel, at);
            }
            if outdated {
                self.notify(tr("diffs.outdated"));
            }
            return;
        }
        let mut view = DiffsView::new(src, folder);
        if view.is_empty() {
            self.notify(tr("diffs.empty"));
            return;
        }
        if let Some((rel, at)) = at {
            view.show_file(&rel, at);
        }
        let temps = tab.temps.clone();
        self.push_tab(Content::Diffs(view), Vec::new());
        if let Some(t) = self.tabs.last_mut() {
            t.temps = temps;
        }
    }

    /// Shows the errors of tabs of all differences, and after they saved files, compares their
    /// folder compares again and has text compares notice the files that changed.
    fn take_review_updates(&mut self) {
        let mut updates = Vec::new();
        for tab in &mut self.tabs {
            if let (Some(origin), Content::Text(view)) = (&tab.origin, tab.content.view_mut()) {
                if std::mem::take(&mut view.review_dirty) {
                    if let Some(review) = &view.review_decision {
                        updates.push((origin.clone(), (**review).clone()));
                    }
                }
            }
        }
        for (origin, review) in updates {
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == origin.folder) {
                if let Content::Folder(view) = &mut tab.content {
                    view.accept_review_update(origin.rel, review);
                }
            }
        }
    }

    /// If the tab at `idx` was opened from an archive-backed folder compare, repacks every
    /// archive side of that folder right away, so saving the tab also saves the archive it
    /// came from in the same step — not only once the folder tab is itself saved later, by
    /// `SaveAll`, say. Call after successfully saving a tab (this does not track which side
    /// exactly changed, only that the tab it came from did, so every archive side of the
    /// folder is repacked, not only the one the tab's file is on).
    /// What follows saving the tabs in `ids`: repacks the archives their files came from, and
    /// uploads their sides fetched over SFTP (and the archives just repacked, for an archive
    /// fetched that way) on another thread. Returns the errors to show.
    fn after_save(&mut self, ids: &[u64]) -> Vec<String> {
        let (mut errors, folders, slow) = self.save_origin_folder_archives(ids);
        for t in &mut self.tabs {
            if ids.contains(&t.id) {
                if let Content::Text(v) = t.content.view_mut() {
                    // Handled here; `refresh_saved_folders` need not repack again.
                    v.saved = false;
                }
            }
        }
        let mut up = ids.to_vec();
        up.extend(folders);
        up.extend(self.mark_origin_remote_folders(ids));
        if slow.is_empty() && self.repack.is_none() {
            errors.extend(self.queue_uploads(&up));
        } else {
            self.start_repacks(slow, up);
        }
        errors
    }

    /// The archive sides of the folder tabs in `folders` that saving left as slow to repack
    /// (see [`FolderView::slow_archives`]).
    fn slow_archives(&self, folders: &[u64]) -> Vec<Repack> {
        let mut slow = Vec::new();
        for t in self.tabs.iter().filter(|t| folders.contains(&t.id)) {
            if let Content::Folder(v) = t.content.view() {
                slow.extend(
                    v.slow_archives()
                        .into_iter()
                        .map(|(s, archive, root)| Repack {
                            side: Some((t.id, s)),
                            archive,
                            root,
                        }),
                );
            }
        }
        slow
    }

    /// Writes the archives `slow` back on another thread (after those already under way),
    /// then uploads the tabs in `up`.
    fn start_repacks(&mut self, slow: Vec<Repack>, up: Vec<u64>) {
        if let Some(run) = &mut self.repack {
            for r in slow {
                if !run.later.iter().any(|l| l.archive == r.archive) {
                    run.later.push(r);
                }
            }
            run.up.extend(up);
            return;
        }
        let names = slow
            .iter()
            .map(|r| match r.archive.file_name() {
                Some(n) => n.to_string_lossy().into_owned(),
                None => r.archive.display().to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let keep = self.tabs.iter().flat_map(Tab::shown_temps).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for r in slow {
                let result = archive::repack_all(&r.archive, &r.root).map_err(|e| format!("{e:#}"));
                if tx.send((r.side, result)).is_err() {
                    return;
                }
            }
        });
        self.repack = Some(RepackRun {
            rx,
            names,
            done: Vec::new(),
            later: Vec::new(),
            up,
            then: None,
            queued: false,
            _keep: keep,
        });
    }

    /// Shows the archives being written back, and once they are, takes in how each went and
    /// uploads what they touched.
    fn repack_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.repack.take() else {
            return;
        };
        let finished = loop {
            match run.rx.try_recv() {
                Ok(done) => run.done.push(done),
                Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        if !finished {
            egui::Modal::new(egui::Id::unique("repack_dialog")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(trf("msg.archive_writing", &[("name", run.names.clone())]));
                });
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            self.repack = Some(run);
            return;
        }
        let mut errors = Vec::new();
        let mut up = std::mem::take(&mut run.up);
        let mut closed = Vec::new();
        for (side, result) in std::mem::take(&mut run.done) {
            if let Err(e) = &result {
                errors.push(e.clone());
            }
            match side {
                Some((id, s)) => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else {
                        continue;
                    };
                    if let Content::Folder(v) = t.content.view_mut() {
                        if result.as_ref().is_ok_and(|w| !w.is_empty()) {
                            up.push(id);
                        }
                        v.archive_repacked(s, result);
                    }
                }
                None => closed.extend(result.unwrap_or_default()),
            }
        }
        up.extend(self.outer_archive_tabs(&closed));
        let later = std::mem::take(&mut run.later);
        if !later.is_empty() {
            self.start_repacks(later, up);
        } else {
            errors.extend(self.queue_uploads(&up));
        }
        let failed = !errors.is_empty();
        if failed {
            self.error(errors.join("\n"));
        }
        let Some(then) = run.then else {
            return;
        };
        if failed {
            self.close_queue.clear();
        } else if !self.after_uploads(then, run.queued) {
            self.after_written(then, run.queued, ctx);
        }
    }

    /// Marks the sides fetched from a server of the folder compares the tabs in `ids` were
    /// opened from, those the tabs' files are in, as edited; returns those folder tabs, for
    /// their write-back to upload the files just saved.
    fn mark_origin_remote_folders(&mut self, ids: &[u64]) -> Vec<u64> {
        let mut marked = Vec::new();
        for &id in ids {
            let Some(tab) = self.tabs.iter().find(|t| t.id == id) else {
                continue;
            };
            let Some(origin) = tab.origin.clone() else {
                continue;
            };
            let paths = tab.paths();
            let Some(folder) = self.tabs.iter_mut().find(|t| t.id == origin.folder) else {
                continue;
            };
            let Content::Folder(v) = folder.content.view_mut() else {
                continue;
            };
            for (s, root) in origin.roots.iter().enumerate() {
                if v.is_remote_side(s) && paths.iter().any(|p| p.starts_with(root)) {
                    v.mark_remote_dirty(s);
                    marked.push(origin.folder);
                }
            }
        }
        marked
    }

    /// Repacks every archive touched while saving the tabs in `ids`, each archive written at
    /// most once even when several of its tabs were saved together (`SaveAll`, or saving every
    /// tab before exit, with several files open from the same archive). Returns the errors,
    /// the folder tabs whose archives were written, and the archives slow to repack left to
    /// write on another thread (those of the folder tabs in `ids` too).
    fn save_origin_folder_archives(&mut self, ids: &[u64]) -> (Vec<String>, Vec<u64>, Vec<Repack>) {
        let mut folders = std::collections::HashSet::new();
        let mut closed = std::collections::HashSet::new();
        for &id in ids {
            let Some(origin) = self
                .tabs
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| t.origin.clone())
            else {
                continue;
            };
            if self.tabs.iter().any(|t| t.id == origin.folder) {
                folders.insert(origin.folder);
            } else {
                // The folder tab this file was opened from was closed meanwhile, so there is
                // no live `FolderView` left to mark dirty and save: repack straight from how
                // its sides looked when this tab was opened, recorded in `origin` for exactly
                // this case. Dedupe by archive path too, in case several sibling tabs from the
                // same now-closed folder are saved together.
                for (root, archive) in origin.roots.iter().zip(&origin.archives) {
                    if let Some(archive) = archive {
                        closed.insert((archive.clone(), root.clone()));
                    }
                }
            }
        }
        let mut errors = Vec::new();
        let mut written = Vec::new();
        let mut slow_from: Vec<u64> = ids.to_vec();
        slow_from.extend(&folders);
        for folder in folders {
            if let Some(Content::Folder(v)) = self
                .tabs
                .iter_mut()
                .find(|t| t.id == folder)
                .map(|t| t.content.view_mut())
            {
                for s in 0..v.n() {
                    if v.is_archive_side(s) {
                        v.mark_archive_dirty(s);
                    }
                }
                let (w, failed) = v.save_archives();
                if !w.is_empty() {
                    written.push(folder);
                }
                errors.extend(failed);
            }
        }
        let mut slow = self.slow_archives(&slow_from);
        let mut closed_written = Vec::new();
        for (archive, root) in closed {
            if archive::slow_to_repack(&archive) {
                slow.push(Repack {
                    side: None,
                    archive,
                    root,
                });
                continue;
            }
            match archive::repack_all(&archive, &root) {
                Ok(w) => closed_written.extend(w),
                Err(e) => errors.push(e.to_string()),
            }
        }
        written.extend(self.outer_archive_tabs(&closed_written));
        (errors, written, slow)
    }

    /// The folder tabs with a side extracted from an archive file that saving just wrote
    /// from another tab, an archive nested in it having been saved there (or from a closed
    /// tab, the archives in `also`): each is compared again, and returned to be uploaded
    /// if it came over SFTP. So is a folder tab with a side fetched from a server that such
    /// an archive is in, that side marked edited.
    fn outer_archive_tabs(&mut self, also: &[PathBuf]) -> Vec<u64> {
        let mut written: Vec<(Option<u64>, PathBuf)> =
            also.iter().map(|p| (None, p.clone())).collect();
        for t in &mut self.tabs {
            let id = t.id;
            if let Content::Folder(v) = t.content.view_mut() {
                written.extend(v.take_written().into_iter().map(|p| (Some(id), p)));
            }
        }
        if written.is_empty() {
            return Vec::new();
        }
        let (cfg, named) = (self.cfg.clone(), self.named_filters.clone());
        let mut tabs = Vec::new();
        for t in &mut self.tabs {
            let id = t.id;
            let Content::Folder(v) = t.content.view_mut() else {
                continue;
            };
            let holds = (0..v.n())
                .filter_map(|s| v.archive_source(s))
                .any(|a| written.iter().any(|(by, p)| *by != Some(id) && p == a));
            let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let remote: Vec<usize> = (0..v.n())
                .filter(|&s| v.is_remote_side(s))
                .filter(|&s| {
                    let root = real(&v.roots[s]);
                    written
                        .iter()
                        .any(|(by, p)| *by != Some(id) && real(p).starts_with(&root))
                })
                .collect();
            for &s in &remote {
                v.mark_remote_dirty(s);
            }
            if holds || !remote.is_empty() {
                v.refresh(&cfg, &named);
                tabs.push(id);
            }
        }
        tabs
    }

    /// Uploads the sides of the tabs in `ids` fetched over SFTP whose local copy changed since
    /// it was last sent, on another thread (or after the uploads already under way). Returns
    /// the errors to show.
    fn queue_uploads(&mut self, ids: &[u64]) -> Vec<String> {
        let mut errors = Vec::new();
        let mut jobs = Vec::new();
        let mut queued = std::collections::HashSet::new();
        for &id in ids {
            // A tab saved and also holding an archive written back is uploaded once.
            if !queued.insert(id) {
                continue;
            }
            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
                continue;
            };
            if tab.remote.iter().all(Option::is_none) {
                continue;
            }
            let paths = tab.paths();
            let source = tab.source.clone();
            let keep = tab.shown_temps();
            for (i, local) in paths.iter().enumerate() {
                if source.get(i) != Some(local) {
                    // Save As moved this side to a local path of its own; it is no longer
                    // read from or written to the server.
                    continue;
                }
                let Some(file) = tab.remote.get_mut(i).and_then(Option::as_mut) else {
                    continue;
                };
                if let Some(manifest) = &file.manifest {
                    let edited = match tab.content.view_mut() {
                        Content::Folder(v) => v.take_remote_dirty(i),
                        _ => false,
                    };
                    if !edited && !file.pending {
                        continue;
                    }
                    file.pending = true;
                    jobs.push(UploadJob {
                        tab: id,
                        side: i,
                        conn: file.conn.clone(),
                        local: local.clone(),
                        remote_path: file.remote_path.clone(),
                        expected: None,
                        sent: None,
                        manifest: Some(manifest.clone()),
                        hash: 0,
                        _keep: keep.clone(),
                    });
                    continue;
                }
                let Some(hash) = file_hash(local) else {
                    errors.push(trf(
                        "msg.sftp_cannot_read",
                        &[("path", local.display().to_string())],
                    ));
                    continue;
                };
                if Some(hash) == file.sent && !file.pending {
                    continue;
                }
                file.pending = true;
                jobs.push(UploadJob {
                    tab: id,
                    side: i,
                    conn: file.conn.clone(),
                    local: local.clone(),
                    remote_path: file.remote_path.clone(),
                    expected: file.stamp,
                    sent: file.sent,
                    manifest: None,
                    hash,
                    _keep: keep.clone(),
                });
            }
        }
        self.start_uploads(jobs);
        errors
    }

    fn start_uploads(&mut self, jobs: Vec<UploadJob>) {
        if jobs.is_empty() {
            return;
        }
        if self.upload.is_some() {
            for job in jobs {
                self.upload_queue
                    .retain(|q| (q.tab, q.side) != (job.tab, job.side));
                self.upload_queue.push(job);
            }
            return;
        }
        let names = jobs
            .iter()
            .map(|j| remote::label(&j.conn, &j.remote_path))
            .collect::<Vec<_>>()
            .join("\n");
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        let run = move || {
            for job in jobs {
                let result = match &job.manifest {
                    Some(manifest) => remote_sync::save_folder(
                        &job.conn,
                        &job.local,
                        &job.remote_path,
                        manifest,
                        &stop,
                    )
                    .map(Uploaded::Folder),
                    None => remote::upload(
                        &job.conn,
                        &job.local,
                        &job.remote_path,
                        job.expected,
                        job.sent,
                        &stop,
                    )
                    .map(Uploaded::File),
                };
                let done = UploadDone {
                    tab: job.tab,
                    side: job.side,
                    hash: job.hash,
                    label: remote::label(&job.conn, &job.remote_path),
                    result,
                };
                if tx.send(done).is_err() {
                    return;
                }
            }
        };
        // Tests stand in for the network with a thread-local fake server, which a spawned
        // thread would not see (see `open_remote`).
        #[cfg(test)]
        let on_this_thread = remote::fake::installed();
        #[cfg(not(test))]
        let on_this_thread = false;
        if on_this_thread {
            run();
        } else {
            std::thread::spawn(run);
        }
        self.upload = Some(UploadRun {
            rx,
            cancel,
            names,
            then: None,
            queued: false,
            errors: Vec::new(),
        });
    }

    /// *Reload from server*: fetches the active tab's sides that came from a server again,
    /// on another thread, once the user agreed (`confirmed`) to lose its changes, if it has any.
    fn reload_remote(&mut self, confirmed: bool) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let sides = reload_sides(tab);
        if sides.is_empty() || self.upload.is_some() || self.reload.is_some() {
            return;
        }
        if !confirmed && tab.is_modified() {
            self.reload_prompt = Some(tab.id);
            return;
        }
        let names = sides
            .iter()
            .map(|(_, conn, path, _)| remote::label(conn, path))
            .collect::<Vec<_>>()
            .join("\n");
        let (tab_id, keep) = (tab.id, tab.shown_temps());
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        let run = move || {
            for (side, conn, path, local) in sides {
                if tx
                    .send((side, remote::refetch(&conn, &path, &local, &stop)))
                    .is_err()
                {
                    return;
                }
            }
        };
        // Tests stand in for the network with a thread-local fake server, which a spawned
        // thread would not see (see `open_remote`).
        #[cfg(test)]
        let on_this_thread = remote::fake::installed();
        #[cfg(not(test))]
        let on_this_thread = false;
        if on_this_thread {
            run();
        } else {
            std::thread::spawn(run);
        }
        self.reload = Some(ReloadRun {
            tab: tab_id,
            rx,
            cancel,
            names,
            done: Vec::new(),
            _keep: keep,
        });
    }

    /// Asks whether to replace a tab's changes with the server's copy, for *Reload from server*.
    fn reload_prompt_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.reload_prompt else {
            return;
        };
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            self.reload_prompt = None;
            return;
        };
        let (mut go, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("reload_prompt")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(tr("menu.reload_remote"));
            ui.label(tr("msg.reload_remote_discards"));
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.reload_remote")).clicked() {
                    go = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if go {
            self.reload_prompt = None;
            self.active = i;
            self.reload_remote(true);
        } else if cancel || modal.should_close() {
            self.reload_prompt = None;
        }
    }

    /// Shows *Reload from server* under way, with a button to stop it, and once it is done
    /// shows the tab's sides as the server now has them.
    fn reload_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.reload.take() else {
            return;
        };
        let finished = loop {
            match run.rx.try_recv() {
                Ok(done) => run.done.push(done),
                Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        if !finished {
            let mut stop = false;
            egui::Modal::new(egui::Id::unique("reload_dialog")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(trf("msg.fetching_remote", &[("name", run.names.clone())]));
                });
                widgets::dialog_footer(ui, |ui| {
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        stop = true;
                    }
                });
            });
            if stop {
                run.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            self.reload = Some(run);
            return;
        }
        let stopped = run.cancel.load(std::sync::atomic::Ordering::Relaxed);
        let (cfg, named) = (self.cfg.clone(), self.named_filters.clone());
        let mut errors = Vec::new();
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == run.tab) {
            let paths = tab.paths();
            let mut reloaded = Vec::new();
            for (side, result) in run.done {
                match result {
                    Ok(staged) => {
                        if let Some(Some(r)) = tab.remote.get_mut(side) {
                            r.stamp = staged.stamp;
                            r.manifest = staged.manifest;
                            r.sent = if staged.is_dir {
                                None
                            } else {
                                paths.get(side).and_then(|p| file_hash(p))
                            };
                            r.pending = false;
                        }
                        reloaded.push(side);
                    }
                    Err(e) if stopped => drop(e),
                    Err(e) => errors.push(format!("{e:#}")),
                }
            }
            if !reloaded.is_empty() {
                match tab.content.view_mut() {
                    Content::Folder(v) => {
                        for &s in &reloaded {
                            v.take_remote_dirty(s);
                        }
                        v.refresh(&cfg, &named);
                    }
                    Content::Text(v) => {
                        let files: Vec<PathBuf> = reloaded
                            .iter()
                            .filter_map(|&s| paths.get(s).map(|p| p.to_path_buf()))
                            .collect();
                        v.reload_from_disk(&files);
                    }
                    Content::Binary(v) => {
                        let read_only = v.read_only.clone();
                        match BinaryView::load(tab.source.clone()) {
                            Ok(mut new) => {
                                new.read_only = read_only;
                                *v = new;
                            }
                            Err(e) => errors.push(format!("{e:#}")),
                        }
                    }
                    Content::Image(v) => {
                        let read_only = v.read_only.clone();
                        match ImageView::load(tab.source.clone(), &cfg) {
                            Ok(mut new) => {
                                new.read_only = read_only;
                                *v = new;
                            }
                            Err(e) => errors.push(format!("{e:#}")),
                        }
                    }
                    Content::Web(_) | Content::Diffs(_) => {}
                }
            }
        }
        // Files opened from a folder fetched again notice theirs changed.
        for t in &mut self.tabs {
            if let Content::Text(v) = t.content.view_mut() {
                v.check_disk();
            }
        }
        if !errors.is_empty() {
            self.error(errors.join("\n"));
        }
    }

    /// After the archives being written back and the uploads now under way succeed, does what
    /// `then` asked for (closing a tab or the program); returns false, doing nothing, when
    /// there are none.
    fn after_uploads(&mut self, then: Prompt, queued: bool) -> bool {
        if let Some(run) = &mut self.repack {
            run.then = Some(then);
            run.queued = queued;
            return true;
        }
        match &mut self.upload {
            Some(run) => {
                run.then = Some(then);
                run.queued = queued;
                true
            }
            None => false,
        }
    }

    /// Shows the uploads under way, with a button to stop them, and takes in how each ended.
    fn upload_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.upload.take() else {
            return;
        };
        let finished = loop {
            match run.rx.try_recv() {
                Ok(done) => {
                    let queued = self
                        .upload_queue
                        .iter()
                        .any(|q| (q.tab, q.side) == (done.tab, done.side));
                    let file = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id == done.tab)
                        .and_then(|t| t.remote.get_mut(done.side))
                        .and_then(Option::as_mut);
                    match done.result {
                        Ok(Uploaded::File(stamp)) => {
                            // A later upload of this side expects the server's file as this
                            // one left it, not as it was before.
                            for q in &mut self.upload_queue {
                                if (q.tab, q.side) == (done.tab, done.side) {
                                    q.expected = Some(stamp);
                                    q.sent = Some(done.hash);
                                }
                            }
                            if let Some(file) = file {
                                file.stamp = Some(stamp);
                                file.sent = Some(done.hash);
                                file.pending = queued;
                            }
                        }
                        Ok(Uploaded::Folder(saved)) => {
                            for q in &mut self.upload_queue {
                                if (q.tab, q.side) == (done.tab, done.side) {
                                    q.manifest = Some(saved.manifest.clone());
                                }
                            }
                            let failed = !saved.errors.is_empty();
                            if failed && !run.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                run.errors.push(trf(
                                    "msg.sftp_upload_failed",
                                    &[("name", done.label), ("error", saved.errors.join("\n"))],
                                ));
                            }
                            if let Some(file) = file {
                                file.manifest = Some(saved.manifest);
                                file.pending = queued || failed;
                            }
                        }
                        Err(e) => {
                            // Half written by this upload, the file is expected as it left
                            // it, so saving again finishes it rather than being refused.
                            let partly = e.downcast_ref::<remote::PartlyWritten>();
                            if let Some(stamp) = partly.and_then(|p| p.stamp) {
                                for q in &mut self.upload_queue {
                                    if (q.tab, q.side) == (done.tab, done.side) {
                                        q.expected = Some(stamp);
                                        q.sent = None;
                                    }
                                }
                                if let Some(file) = file {
                                    file.stamp = Some(stamp);
                                    file.sent = None;
                                }
                            }
                            if !run.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                run.errors.push(trf(
                                    "msg.sftp_upload_failed",
                                    &[("name", done.label), ("error", format!("{e:#}"))],
                                ));
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        if !finished {
            let mut stop = false;
            egui::Modal::new(egui::Id::unique("upload_dialog")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(trf("msg.sftp_uploading", &[("name", run.names.clone())]));
                });
                widgets::dialog_footer(ui, |ui| {
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                        || ui.input(|i| i.key_pressed(Key::Escape))
                    {
                        stop = true;
                    }
                });
            });
            if stop {
                run.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                self.upload_queue.clear();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            self.upload = Some(run);
            return;
        }
        let failed =
            !run.errors.is_empty() || run.cancel.load(std::sync::atomic::Ordering::Relaxed);
        for e in std::mem::take(&mut run.errors) {
            self.error(e);
        }
        if !self.upload_queue.is_empty() && !failed {
            let jobs = std::mem::take(&mut self.upload_queue);
            self.start_uploads(jobs);
            if let Some(then) = run.then {
                self.after_uploads(then, run.queued);
            }
            return;
        }
        self.upload_queue.clear();
        let Some(then) = run.then else {
            return;
        };
        if failed {
            self.close_queue.clear();
            return;
        }
        self.after_written(then, run.queued, ctx);
    }

    /// Does what `then` asked for once what it saved was written back and uploaded: closes the
    /// tab, or the program, unless something is still not saved. `queued`: whether more tabs
    /// wait to be asked about closing.
    fn after_written(&mut self, then: Prompt, queued: bool, ctx: &egui::Context) {
        match then {
            Prompt::Exit => {
                if !self.tabs.iter().any(|t| t.is_modified()) {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Prompt::CloseTab(id) | Prompt::SwitchFile(id, ..) => {
                let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
                    return;
                };
                if self.tabs[i].is_modified() {
                    self.close_queue.clear();
                    return;
                }
                match then {
                    Prompt::SwitchFile(_, forward, conflicts) => {
                        self.switch_file(i, forward, conflicts)
                    }
                    _ => self.close_tab(i, true),
                }
                if queued {
                    self.ask_next_close();
                }
            }
        }
    }

    fn refresh_saved_folders(&mut self) {
        let mut folders = Vec::new();
        let mut text_saved = Vec::new();
        let mut errors = Vec::new();
        for t in &mut self.tabs {
            match &mut t.content {
                Content::Diffs(v) => {
                    if std::mem::take(&mut v.saved) {
                        folders.push(v.folder);
                    }
                    errors.extend(v.error.take());
                }
                Content::Text(v) => {
                    if std::mem::take(&mut v.saved) {
                        text_saved.push(t.id);
                    }
                }
                _ => {}
            }
        }
        // A text tab's own buttons (the 3-way merge view's "Save result", say) can write a
        // pane straight to disk without going through `Cmd::Save`; repack the archive this
        // tab was opened from here too, the same as the `Cmd::Save` dispatch itself does.
        // Batched so saving several panes from the same archive in one frame repacks it only
        // once.
        errors.extend(self.after_save(&text_saved));
        if folders.is_empty() {
            for e in errors {
                self.error(e);
            }
            return;
        }
        let (cfg, named) = (self.cfg.clone(), self.named_filters.clone());
        let mut repacked = Vec::new();
        for t in &mut self.tabs {
            let id = t.id;
            match t.content.view_mut() {
                Content::Folder(f) if folders.contains(&id) => {
                    // "All differences" writes straight into the extracted copy, the same
                    // way a child file tab's own save does; repack any archive side it
                    // touched before refreshing the listing (see `save_origin_folder_archive`
                    // for the same pattern keyed by a tab's `Origin` instead of `DiffsView`'s
                    // `folder`).
                    for s in 0..f.n() {
                        if f.is_archive_side(s) {
                            f.mark_archive_dirty(s);
                        }
                        f.mark_remote_dirty(s);
                    }
                    let (w, failed) = f.save_archives();
                    if !w.is_empty() || f.has_remote_changes() {
                        repacked.push(id);
                    }
                    errors.extend(failed);
                    f.refresh(&cfg, &named);
                }
                Content::Text(v) => v.check_disk(),
                _ => {}
            }
        }
        repacked.extend(self.outer_archive_tabs(&[]));
        let slow = self.slow_archives(&folders);
        if slow.is_empty() && self.repack.is_none() {
            errors.extend(self.queue_uploads(&repacked));
        } else {
            self.start_repacks(slow, repacked);
        }
        for e in errors {
            self.error(e);
        }
    }

    /// Writes the HTML report of the active text compare to `path`, on another thread.
    fn export_text_html_to(&mut self, path: PathBuf) {
        let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            return;
        };
        let job = v.html_report_job(&self.cfg.rules);
        self.spawn_report(path, job);
    }

    /// Asks where to save an HTML report, suggesting `name`, then writes what `job` makes on
    /// another thread.
    fn write_html_report(
        &mut self,
        title: &str,
        name: &str,
        job: impl FnOnce() -> String + Send + 'static,
    ) {
        if let Some(path) = self.html_report_path(title, name) {
            self.spawn_report(path, job);
        }
    }

    /// Where to save an HTML report, unless one is still being written.
    fn html_report_path(&mut self, title: &str, name: &str) -> Option<PathBuf> {
        self.report_path(title, name, &[HTML])
    }

    /// Where to save a report of one of the types `filters` (name, extensions), unless one is
    /// still being written. A path without one of their extensions gets ".html".
    fn report_path(
        &mut self,
        title: &str,
        name: &str,
        filters: &[(&str, &[&str])],
    ) -> Option<PathBuf> {
        if self.report_job.is_some() {
            self.notify(tr("msg.report_writing"));
            return None;
        }
        // Without an extension, the dialog adds the one of the type picked.
        let mut dlg = rfd::FileDialog::new().set_title(title);
        for (kind, exts) in filters {
            dlg = dlg.add_filter(*kind, exts);
        }
        let path = dlg
            .set_file_name(name.strip_suffix(".html").unwrap_or(name))
            .save_file()?;
        let ext = crate::core::text::extension(&path);
        let known = filters.iter().any(|(_, exts)| exts.contains(&ext.as_str()));
        Some(if known { path } else { with_html_ext(path) })
    }

    fn spawn_report(&mut self, path: PathBuf, job: impl FnOnce() -> String + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let file = path.clone();
        std::thread::spawn(move || {
            let written = crate::core::fsutil::write_atomic_with(&file, job().as_bytes(), false);
            let _ = tx.send(written.map_err(|e| format!("{e:#}")));
        });
        self.report_job = Some((path, rx));
        self.notify(tr("msg.report_writing"));
    }

    /// Announces a saved report, and opens it in the browser when it is HTML and set to.
    fn report_saved(&mut self, path: &Path) {
        let print = std::mem::take(&mut self.report_opens);
        if !print {
            self.notify(trf("msg.saved", &[("file", path.display().to_string())]));
        }
        let html = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
        if print || (html && self.cfg.open_reports) {
            if let Err(e) = crate::ui::folder_view::system_open(path) {
                self.error(e);
            }
        }
    }

    /// Announces a report written on another thread once it is done.
    fn poll_report(&mut self, ctx: &egui::Context) {
        let Some((path, rx)) = &self.report_job else {
            return;
        };
        let done = match rx.try_recv() {
            Ok(r) => Some(r),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(format!("{}: {}", path.display(), tr("status.error"))))
            }
        };
        let path = path.clone();
        match done {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            Some(Ok(())) => {
                self.report_job = None;
                self.report_saved(&path);
            }
            Some(Err(e)) => {
                self.report_job = None;
                self.report_opens = false;
                self.error(e);
            }
        }
    }

    /// Saves the active text compare with its bookmarks and their comments as one file.
    fn save_comparison(&mut self) {
        if matches!(
            self.tabs.get(self.active).map(|t| &t.content),
            Some(Content::Folder(_))
        ) {
            self.save_folder_review(false);
            return;
        }
        let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            self.error(tr("msg.text_compare_only"));
            return;
        };
        let Some(mut path) = rfd::FileDialog::new()
            .set_title(tr("menu.save_comparison"))
            .add_filter(tr("label.saved_compare"), &[saved::EXT])
            .set_file_name(format!("comparison.{}", saved::EXT))
            .save_file()
        else {
            return;
        };
        if text::extension(&path) != saved::EXT {
            path.set_extension(saved::EXT);
        }
        match v.saved_compare().save(&path) {
            Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
            Err(e) => self.error(e.to_string()),
        }
    }

    fn save_folder_review(&mut self, selected_only: bool) {
        let Some(Content::Folder(view)) = self.tabs.get(self.active).map(|t| &t.content) else {
            return;
        };
        let Some(mut path) = rfd::FileDialog::new()
            .set_title(tr("review.save"))
            .add_filter(tr("review.label"), &[crate::core::review::EXT])
            .set_file_name(format!("review.{}", crate::core::review::EXT))
            .save_file()
        else {
            return;
        };
        path.set_extension(crate::core::review::EXT);
        match view.save_review(&path, selected_only) {
            Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn show_statistics(&mut self) {
        self.stats_dlg = match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => {
                let stats = v.statistics();
                let row = |key: &'static str, f: fn(&crate::core::diff::Stats) -> usize| {
                    (
                        tr(key).to_string(),
                        stats.iter().map(|(_, s)| f(s).to_string()).collect(),
                    )
                };
                Some(StatsDialog {
                    columns: stats.iter().map(|(name, _)| name.clone()).collect(),
                    rows: vec![
                        row("stats.identical_lines", |s| s.identical),
                        row("stats.changed_lines", |s| s.changed),
                        row("stats.added_lines", |s| s.added),
                        row("stats.removed_lines", |s| s.removed),
                        row("stats.ignored_lines", |s| s.ignored),
                        row("stats.differences", |s| s.differences),
                    ],
                })
            }
            Some(Content::Folder(v)) => Some(StatsDialog {
                columns: vec![String::new()],
                rows: v
                    .statistics()
                    .into_iter()
                    .map(|(k, v)| (k, vec![v]))
                    .collect(),
            }),
            _ => {
                self.error(tr("msg.stats_unavailable"));
                None
            }
        };
    }

    /// Offers the active folder comparison to save as a job file.
    fn begin_save_job(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Content::Folder(v) = tab.content.view() else {
            self.error(tr("msg.job_folder_only"));
            return;
        };
        // A revision, web page or archive is compared through temporary copies, which a job
        // run later would not find.
        let temps = tab.shown_temps();
        if v.roots
            .iter()
            .any(|r| temps.iter().any(|t| r.starts_with(t.path())))
        {
            self.error(tr("msg.job_temp_folders"));
            return;
        }
        let name = report::html_file_name(&v.display_roots);
        let stem = name.strip_suffix(".html").unwrap_or(&name).to_string();
        self.save_job = Some(crate::ui::job_dialog::SaveJob::new(
            v.as_job(&self.cfg),
            &stem,
        ));
    }

    fn save_job_dialog(&mut self, ctx: &egui::Context) {
        use crate::ui::job_dialog::SaveStep;
        let Some(d) = self.save_job.as_mut() else {
            return;
        };
        match d.show(ctx) {
            Some(SaveStep::Close) => self.save_job = None,
            Some(SaveStep::Save) => {
                let name = format!("{}.oxjob", d.name.trim()).replace(['/', '\\'], "_");
                let Some(path) = rfd::FileDialog::new()
                    .set_title(tr("dlg.save_job"))
                    .add_filter(tr("label.job"), &["oxjob"])
                    .set_file_name(name)
                    .save_file()
                else {
                    return;
                };
                match d.write(&path) {
                    Ok(()) => {
                        self.save_job = None;
                        self.notify(trf("msg.saved", &[("file", path.display().to_string())]));
                    }
                    Err(e) => d.failed(e),
                }
            }
            None => {}
        }
    }

    /// Reads the job file at `path` and runs it, or first asks when it changes folders.
    pub fn start_run_job(&mut self, path: PathBuf) {
        let mut run = crate::ui::job_dialog::RunJob::new(path);
        if !run.needs_confirmation() {
            run.start(self.stored_cfg(), false);
        }
        self.run_job = Some(run);
    }

    fn run_job_dialog(&mut self, ctx: &egui::Context) {
        use crate::ui::job_dialog::RunStep;
        let cfg = self.stored_cfg();
        let Some(d) = self.run_job.as_mut() else {
            return;
        };
        match d.show(ctx) {
            Some(RunStep::Close) => self.run_job = None,
            Some(RunStep::Start { yes }) => d.start(cfg, yes),
            Some(RunStep::Open(path)) => {
                if let Err(e) = crate::ui::folder_view::system_open(&path) {
                    self.error(e);
                }
            }
            Some(RunStep::Compare(paths)) => {
                self.run_job = None;
                self.open(OpenRequest {
                    paths,
                    ..Default::default()
                });
            }
            None => {}
        }
    }

    fn stats_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.stats_dlg else {
            return;
        };
        let mut keep = true;
        let modal = egui::Modal::new(egui::Id::unique("stats_dialog")).show(ctx, |ui| {
            ui.set_min_width(360.0);
            if widgets::dialog_title(ui, &plain(tr("menu.statistics"))) {
                keep = false;
            }
            egui::Grid::new("stats_grid")
                .num_columns(d.columns.len() + 1)
                .striped(true)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    if d.columns.iter().any(|c| !c.is_empty()) {
                        ui.label("");
                        for c in &d.columns {
                            ui.strong(c);
                        }
                        ui.end_row();
                    }
                    for (label, values) in &d.rows {
                        ui.label(label);
                        for v in values {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(egui::RichText::new(v).monospace());
                            });
                        }
                        ui.end_row();
                    }
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.ok")).clicked() {
                    keep = false;
                }
                if widgets::secondary_button(ui, tr("btn.copy")).clicked() {
                    let mut text = format!("\t{}\n", d.columns.join("\t"));
                    for (label, values) in &d.rows {
                        text.push_str(&format!("{label}\t{}\n", values.join("\t")));
                    }
                    ui.ctx().copy_text(text);
                }
            });
        });
        if !keep || modal.should_close() {
            self.stats_dlg = None;
        }
    }

    fn git_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.git_dlg.take() else {
            return;
        };
        let path = PathBuf::from(d.path.trim());
        if let Some(job) = &d.commits_job {
            match job.rx.try_recv() {
                Ok(commits) => {
                    d.commits = commits;
                    d.commits_for = job.path.clone();
                    d.commits_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => d.commits_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if let Some((for_path, rx)) = &d.hint_job {
            match rx.try_recv() {
                Ok(hint) => {
                    // a hint for a path typed over since is not shown
                    if *for_path == d.path {
                        d.rev_hint = hint;
                    }
                    d.hint_for = for_path.clone();
                    d.hint_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => d.hint_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        if d.typed.0 != d.path {
            d.typed = (d.path.clone(), std::time::Instant::now());
        }
        // The hint looks through every parent folder (slow on a network share), so it is found
        // once typing pauses, one path at a time, as the log is.
        let hint_wait = GIT_LOG_DELAY.saturating_sub(d.typed.1.elapsed());
        if d.hint_for != d.path && d.hint_job.is_none() {
            if hint_wait.is_zero() {
                let (tx, rx) = std::sync::mpsc::channel();
                let p = path.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(vcs::Vcs::of(&p).rev_hint());
                });
                d.hint_job = Some((d.path.clone(), rx));
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            } else {
                ctx.request_repaint_after(hint_wait);
            }
        }
        let started_for = d.commits_job.as_ref().map(|j| j.path.as_str());
        let pending = d.commits_for != d.path && started_for != Some(d.path.as_str());
        let running = d.commits_job.as_ref().map(|j| j.started.elapsed());
        match log_step(pending, d.typed.1.elapsed(), running) {
            LogStep::Idle => {}
            LogStep::WaitFor(wait) => ctx.request_repaint_after(wait),
            LogStep::Start => {
                let (tx, rx) = std::sync::mpsc::channel();
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (p, stop) = (path.clone(), cancel.clone());
                std::thread::spawn(move || {
                    let commits = if p.exists() {
                        vcs::recent_revisions(&p, 20, &stop)
                    } else {
                        Vec::new()
                    };
                    let _ = tx.send(commits);
                });
                // replacing an earlier path's job drops it, which stops its process
                d.commits_job = Some(LogJob {
                    path: d.path.clone(),
                    rx,
                    started: std::time::Instant::now(),
                    cancel,
                });
            }
        }
        if d.commits_for != d.path {
            d.commits.clear();
        }
        let (mut keep, mut go) = (true, false);
        let modal = egui::Modal::new(egui::Id::unique("git_dialog")).show(ctx, |ui| {
            ui.set_width(560.0);
            if widgets::dialog_title(ui, &plain(tr("menu.git_compare"))) {
                keep = false;
            }
            ui.label(egui::RichText::new(tr("hint.git_compare")).color(pal(ui).text_dim));
            ui.add_space(6.0);
            egui::Grid::new("git_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(tr("label.path"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.path)
                                .margin(vec2(8.0, 5.0))
                                .desired_width(ui.available_width() - 72.0),
                        );
                        if widgets::icon_button(ui, Icon::File, tr("btn.browse_file")).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                d.path = p.display().to_string();
                            }
                        }
                        if widgets::icon_button(ui, Icon::Folder, tr("btn.browse_folder")).clicked()
                        {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                d.path = p.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(tr("label.revision"));
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut d.rev)
                                .margin(vec2(8.0, 5.0))
                                .hint_text(d.rev_hint)
                                .desired_width(ui.available_width() - 36.0),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            go = true;
                        }
                        let commits = widgets::icon_button_enabled(
                            ui,
                            !d.commits.is_empty(),
                            Icon::Next,
                            tr("label.recent_commits"),
                        );
                        let close = egui::PopupCloseBehavior::CloseOnClickOutside;
                        let popup = egui::Popup::menu(&commits).close_behavior(close);
                        popup.show(|ui| {
                            widgets::menu_scroll(ui, |ui| {
                                ui.set_min_width(420.0);
                                for (hash, subject) in &d.commits {
                                    let text = egui::RichText::new(format!("{hash}  {subject}"));
                                    if ui.button(text).clicked() {
                                        d.rev = hash.clone();
                                        ui.close();
                                    }
                                }
                            });
                        });
                    });
                    ui.end_row();
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.compare")).clicked() {
                    go = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    keep = false;
                }
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if go && !d.path.trim().is_empty() {
            let rev = d.rev.clone();
            self.open_git_revision(&path, &rev, Some(d));
        } else if keep {
            self.git_dlg = Some(d);
        }
    }

    /// Saves what the active tab compares as a project file.
    fn save_project(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.source.len() < 2 {
            self.error(tr("msg.project_needs_files"));
            return;
        }
        let Some(proj) = tab.project() else {
            return;
        };
        let Some(mut path) = rfd::FileDialog::new()
            .set_title(tr("menu.save_project"))
            .add_filter(tr("label.project"), &[project::EXT])
            .set_file_name(format!("compare.{}", project::EXT))
            .save_file()
        else {
            return;
        };
        if text::extension(&path) != project::EXT {
            path.set_extension(project::EXT);
        }
        match proj.relative_to(&path).and_then(|p| p.save(&path)) {
            Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Applies a unified diff: every patched file opens next to its original, to be reviewed and
    /// saved.
    fn apply_patch(&mut self) {
        if let Some(patch_path) = rfd::FileDialog::new()
            .set_title(tr("menu.apply_patch"))
            .add_filter(tr("label.patch_files"), &["patch", "diff"])
            .pick_file()
        {
            self.apply_patch_file(&patch_path);
        }
    }

    /// Opens the files `patch_path` changes, patched, each in a tab to review before saving.
    fn apply_patch_file(&mut self, patch_path: &Path) {
        let parsed = std::fs::read(patch_path)
            .map_err(|e| e.to_string())
            .and_then(|b| patch::parse_unified(&text::decode(&b, None).0));
        let files = match parsed {
            Ok(f) => f,
            Err(e) => return self.error(format!("{}: {e}", patch_path.display())),
        };
        let mut base = patch_path.parent().unwrap_or(Path::new("")).to_path_buf();
        let mut targets: Vec<Option<PathBuf>> = files.iter().map(|f| f.locate(&base)).collect();
        if targets.iter().all(Option::is_none) && !files.iter().all(|f| f.creates_file()) {
            if files.len() == 1 {
                let Some(p) = rfd::FileDialog::new()
                    .set_title(trf("dlg.patch_target", &[("file", files[0].name().into())]))
                    .set_directory(&base)
                    .pick_file()
                else {
                    return;
                };
                targets[0] = Some(p);
            } else {
                let Some(dir) = rfd::FileDialog::new()
                    .set_title(tr("dlg.patch_folder"))
                    .set_directory(&base)
                    .pick_folder()
                else {
                    return;
                };
                targets = files.iter().map(|f| f.locate(&dir)).collect();
                base = dir;
            }
        }
        const MAX_TABS: usize = 20;
        let mut problems: Vec<String> = Vec::new();
        let mut opened = 0;
        for (f, target) in files.iter().zip(targets) {
            let path = match target {
                Some(p) => p,
                None if f.creates_file()
                    && Path::new(f.new_name.trim_start_matches("b/"))
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))) =>
                {
                    base.join(f.new_name.trim_start_matches("b/"))
                }
                None => {
                    problems.push(trf("msg.patch_no_target", &[("file", f.name().into())]));
                    continue;
                }
            };
            if opened == MAX_TABS {
                problems.push(trf("msg.patch_too_many", &[("n", MAX_TABS.to_string())]));
                break;
            }
            let original = if path.exists() {
                match text::load_doc(&path, None) {
                    Ok(d) => d,
                    Err(e) => {
                        problems.push(format!("{}: {e}", path.display()));
                        continue;
                    }
                }
            } else {
                TextDoc::empty(Some(path.clone()))
            };
            let (raw, original_newline) = match patch::document_lines(&original) {
                Ok(lines) => lines,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            let result = patch::apply(&raw, f);
            let mut left = original.clone();
            left.read_only = true;
            left.description = Some(format!("{} ({})", path.display(), tr("label.original")));
            let bytes = text::join_lines(
                &result.lines,
                text::Eol::Lf,
                result.final_newline.unwrap_or(original_newline),
            );
            let mut right = text::doc_from_bytes(
                original.path.clone(),
                bytes.as_bytes(),
                Some(text::TextEncoding::Utf8),
            );
            right.preserve_eols = true;
            right.modified = true;
            right.description = Some(format!("{} ({})", path.display(), tr("label.patched")));
            let mut v = TextView::new(vec![left, right], &self.cfg);
            v.status = if result.failed.is_empty() {
                trf("msg.patch_applied", &[("n", f.hunks.len().to_string())])
            } else {
                let list: Vec<String> = result.failed.iter().map(usize::to_string).collect();
                let msg = trf(
                    "msg.hunks_failed",
                    &[
                        ("file", path.display().to_string()),
                        ("hunks", list.join(", ")),
                    ],
                );
                problems.push(msg.clone());
                msg
            };
            self.push_tab(Content::Text(v), Vec::new());
            opened += 1;
        }
        if !problems.is_empty() {
            self.error(problems.join("\n"));
        } else if opened > 0 {
            self.notify(trf("msg.patch_review", &[("n", opened.to_string())]));
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = widgets::typing(ctx);
        // Open menus, context menus and popups take Escape to close.
        let escape_reserved = egui::Popup::is_any_open(ctx)
            || match self.tabs.get(self.active).map(|t| t.content.view()) {
                Some(Content::Text(v)) => v.handles_escape(),
                Some(Content::Binary(v)) => v.find.open || v.goto.is_some(),
                Some(Content::Folder(v)) => v.handles_escape(),
                _ => false,
            };
        let text_typed = matches!(
            self.tabs.get(self.active).map(|t| t.content.view()),
            Some(Content::Binary(v)) if v.takes_text()
        ) || matches!(
            self.tabs.get(self.active).map(|t| t.content.view()),
            Some(Content::Text(v)) if v.takes_text()
        );
        let types_into_pane = match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => v.types_into_pane(),
            _ => text_typed,
        };
        let caret = match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => Some(v),
            _ => None,
        };
        // keys the text caret takes (on macOS, Option+Left and Option+Right move by word)
        let caret_takes = |b: &commands::Binding| {
            caret.is_some_and(|v| v.takes_key(b.key.logical_key, b.key.modifiers))
        };
        let reserved: Vec<bool> = BINDINGS.iter().map(caret_takes).collect();
        let menus = self.menus();
        let mut cmds = std::mem::take(&mut self.tab_keys);
        ctx.input_mut(|i| {
            for (b, &reserved) in BINDINGS.iter().zip(&reserved) {
                if b.key.logical_key == Key::Escape && escape_reserved
                    || b.key.logical_key == Key::Questionmark && text_typed
                    || types_into_pane && commands::types_text(b)
                    || reserved
                {
                    continue;
                }
                let active = match b.scope {
                    Scope::Always => true,
                    Scope::View => !typing,
                    Scope::System => false,
                };
                // A key whose command does nothing here is still taken: left in, Ctrl+Shift+Z of a
                // disabled Redo would go on to match Ctrl+Z (Undo), which ignores Shift.
                if active && commands::consume(i, b) && menus.enabled(b.cmd) {
                    cmds.push(b.cmd);
                }
            }
        });
        for c in cmds {
            self.run(c, ctx);
        }
    }

    // ---------- menus ----------

    /// What the menus act on: the view of the active tab.
    fn menus(&self) -> Menus {
        let tab = self.tabs.get(self.active);
        let view = tab.map(|t| t.content.view());
        Menus {
            view: match view {
                None => ViewKind::Empty,
                Some(Content::Text(_)) => ViewKind::Text,
                Some(Content::Binary(_)) => ViewKind::Binary,
                Some(Content::Image(_)) => ViewKind::Image,
                Some(Content::Folder(_)) => ViewKind::Folder,
                Some(Content::Web(_)) => ViewKind::Web,
                Some(Content::Diffs(_)) => ViewKind::Diffs,
            },
            panes: match view {
                Some(Content::Text(v)) => v.n(),
                _ => 0,
            },
            sides: match view {
                Some(Content::Text(v)) => v.n(),
                Some(Content::Folder(v)) => v.n(),
                _ => 0,
            },
            can_take_both: matches!(view, Some(Content::Text(v)) if v.can_take_both()),
            inline_view: matches!(view, Some(Content::Text(v)) if v.shows_inline()),
            has_sync_points: matches!(view, Some(Content::Text(v)) if v.has_sync_points()),
            has_source: tab.is_some_and(|t| t.source.len() >= 2),
            folder_busy: matches!(view, Some(Content::Folder(v)) if v.is_running()),
            folder_job: matches!(view, Some(Content::Folder(v)) if v.has_file_job()),
            web: matches!(tab.map(|t| &t.content), Some(Content::Web(_))),
            formatted: matches!(view, Some(Content::Web(w)) if w.formatted.is_some()),
            tabs: self.tabs.len(),
            picked: None,
            rules: None,
            recent: None,
            clear_recent: false,
            has_workspaces: !self.cfg.workspaces.is_empty(),
            can_reload: tab.is_some_and(|t| !reload_sides(t).is_empty())
                && self.upload.is_none()
                && self.reload.is_none(),
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut m = self.menus();
        if cfg!(target_os = "macos") {
            // The menu bar is the title bar: its empty parts move and zoom the window.
            let bar = ui.interact(
                ui.max_rect().expand2(vec2(10.0, 5.0)),
                ui.scope_id().with("title_bar"),
                Sense::click_and_drag(),
            );
            bar.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TitleBar, true, "OxideMerge"));
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let zoomed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!zoomed));
            }
        }
        let width = self.menu_width(ctx, &mut m);
        widgets::set_menu_width(ctx, width);
        egui::MenuBar::new().ui(ui, |ui| {
            ui.spacing_mut().button_padding = vec2(9.0, 4.0);
            let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if cfg!(target_os = "macos") && !fullscreen {
                // room for the window buttons
                ui.add_space(64.0);
            }
            let logo = widgets::logo(ui, 22.0);
            if ui
                .interact(logo.rect, logo.id.with("about"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tr("menu.about"))
                .clicked()
            {
                self.about = true;
            }
            // Narrow windows give the name's room to the menus and icons.
            if ui.available_width() > 640.0 {
                ui.label(
                    egui::RichText::new("OxideMerge").font(theme::semibold(theme::size::SUBTITLE)),
                );
            }
            ui.add_space(12.0);
            let bar_bottom = ui.max_rect().bottom();
            for (i, key) in MENUS.into_iter().enumerate() {
                ui.menu_button(tr(key), |ui| {
                    widgets::bar_menu(ui, bar_bottom, |ui| self.menu(ui, &mut m, i));
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Help, &Cmd::Help.tip()).clicked() {
                    m.picked = Some(Cmd::Help);
                }
                let palette = Cmd::Palette.tip();
                // Narrow windows keep the icon; wide ones show a search field for commands.
                let wide = ui.available_width() > 420.0;
                // Icons that no longer fit are left out; the menus offer the same commands.
                let fits = |ui: &egui::Ui| ui.available_width() >= 36.0;
                if !wide && fits(ui) && widgets::icon_button(ui, Icon::Palette, &palette).clicked()
                {
                    m.picked = Some(Cmd::Palette);
                }
                let options = tr("menu.options").trim_end_matches(['.', '…']);
                if fits(ui) && widgets::icon_button(ui, Icon::Settings, options).clicked() {
                    m.picked = Some(Cmd::Options);
                }
                let theme_icon = if self.cfg.dark_mode {
                    Icon::Sun
                } else {
                    Icon::Moon
                };
                if fits(ui) && widgets::icon_button(ui, theme_icon, tr("view.dark_mode")).clicked()
                {
                    self.cfg.theme = if self.cfg.dark_mode {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    };
                }
                let open = tr("menu.open").trim_end_matches(['.', '…']);
                if fits(ui)
                    && widgets::icon_button(
                        ui,
                        Icon::Folder,
                        &widgets::with_key(open, Cmd::Open.key()),
                    )
                    .clicked()
                {
                    m.picked = Some(Cmd::Open);
                }
                if wide {
                    ui.add_space(8.0);
                    let hint = tr("palette.search");
                    if widgets::search_button(ui, 230.0, hint, Cmd::Palette.key(), &palette)
                        .clicked()
                    {
                        m.picked = Some(Cmd::Palette);
                    }
                }
            });
        });
        if let Some(name) = m.rules {
            self.use_rules(self.active, name);
        }
        if let Some(paths) = m.recent {
            self.open(OpenRequest {
                paths: paths.iter().map(PathBuf::from).collect(),
                ..Default::default()
            });
        }
        if m.clear_recent {
            self.clear_recent();
        }
        if let Some(c) = m.picked {
            self.run(c, ctx);
        }
    }

    /// The contents of menu `i` of [`MENUS`].
    fn menu(&mut self, ui: &mut egui::Ui, m: &mut Menus, i: usize) {
        match i {
            0 => self.file_menu(ui, m),
            1 => edit_menu(ui, m),
            2 => self.view_menu(ui, m),
            3 => self.compare_menu(ui, m),
            4 => merge_menu(ui, m),
            5 => self.tools_menu(ui, m),
            _ => {
                m.item(ui, Cmd::Help);
                m.item(ui, Cmd::Palette);
                ui.separator();
                if ui.button(tr("menu.about")).clicked() {
                    self.about = true;
                    ui.close();
                }
            }
        }
    }

    /// The width all menus open with, so that they match: that of the widest menu of the menu
    /// bar or of its submenus, measured once per language, fonts, text size, display scale and
    /// kind of view by laying the menus out unseen.
    fn menu_width(&mut self, ctx: &egui::Context, m: &mut Menus) -> f32 {
        let size = ctx.global_style().text_styles[&egui::TextStyle::Button].size;
        // The fonts in use: after a change of language they follow a frame later.
        let fonts = ctx.fonts(|f| egui::Id::unique(&f.definitions().families));
        // The Merge menu names its first item after the view.
        let id = egui::Id::unique("menu_width")
            .with(i18n::current_language())
            .with(fonts)
            .with(size.to_bits())
            .with(ctx.pixels_per_point().to_bits())
            .with(m.view == ViewKind::Folder);
        if let Some(width) = ctx.data(|d| d.get_temp::<f32>(id)) {
            return width;
        }
        let width = egui::Area::new(egui::Id::unique(MENU_MEASURE))
            .order(egui::Order::Background)
            .fixed_pos(egui::pos2(-10_000.0, -10_000.0))
            .constrain(false)
            .interactable(false)
            .show(ctx, |ui| {
                ui.scope_builder(egui::UiBuilder::new().invisible().sizing_pass(), |ui| {
                    // As the menus are styled (see `widgets::menu_setup`).
                    ui.spacing_mut().button_padding = widgets::MENU_ITEM_PADDING;
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    (0..MENUS.len())
                        .map(|i| ui.scope(|ui| self.menu(ui, m, i)).response.rect.width())
                        .fold(0.0, f32::max)
                })
                .inner
            })
            .inner;
        ctx.data_mut(|d| d.insert_temp(id, width));
        width
    }

    fn file_menu(&mut self, ui: &mut egui::Ui, m: &mut Menus) {
        m.item(ui, Cmd::New);
        m.item(ui, Cmd::CompareClipboard);
        m.item(ui, Cmd::Open);
        m.item(ui, Cmd::OpenRemote);
        m.item(ui, Cmd::ReloadRemote);
        let recent = &self.cfg.recent_compares;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            submenu_list(ui, tr("menu.open_recent"), |ui| {
                for paths in recent.iter().take(10) {
                    if ui.button(recent_label(paths)).clicked() {
                        m.recent = Some(paths.clone());
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(tr("menu.clear_recent")).clicked() {
                    m.clear_recent = true;
                    ui.close();
                }
            });
        });
        m.item(ui, Cmd::OpenConflict);
        m.item(ui, Cmd::GitCompare);
        ui.separator();
        m.item(ui, Cmd::OpenProject);
        m.item(ui, Cmd::SaveProject);
        m.item(ui, Cmd::SaveJob);
        m.item(ui, Cmd::RunJob);
        ui.separator();
        let workspaces = &self.cfg.workspaces;
        submenu_list(ui, tr("menu.workspaces"), |ui| {
            m.item(ui, Cmd::SaveWorkspace);
            if !workspaces.is_empty() {
                ui.separator();
                for (i, w) in workspaces.iter().enumerate().take(MAX_WORKSPACE_MENU) {
                    m.item_as(ui, &shorten(&w.name, 40), Cmd::OpenWorkspace(i));
                }
                ui.separator();
                m.item(ui, Cmd::ManageWorkspaces);
            }
        });
        ui.separator();
        m.item(ui, Cmd::Save);
        m.item(ui, Cmd::SaveAll);
        let panes = m.panes;
        ui.add_enabled_ui(panes > 0, |ui| {
            submenu(ui, tr("menu.save_as"), |ui| {
                for p in 0..panes {
                    let side = side_name(p, panes).into();
                    m.item_as(
                        ui,
                        &trf("menu.save_side_as", &[("side", side)]),
                        Cmd::SaveAs(p),
                    );
                }
            });
        });
        m.item(ui, Cmd::SaveComparison);
        ui.separator();
        submenu(ui, tr("menu.export"), |ui| {
            m.item(ui, Cmd::HtmlReport);
            m.item(ui, Cmd::Slideshow);
            m.item(ui, Cmd::ExportPdf);
            m.item(ui, Cmd::Patch);
            ui.separator();
            check(ui, &mut self.cfg.open_reports, tr("menu.open_reports"), "");
            check(ui, &mut self.cfg.pdf_letter, tr("menu.pdf_letter"), "");
            let hide = &mut self.cfg.report_hide_paths;
            if widgets::menu_check(ui, *hide, tr("menu.report_hide_paths"), "")
                .on_hover_text(tr("menu.report_hide_paths_tip"))
                .clicked()
            {
                *hide = !*hide;
            }
        });
        m.item(ui, Cmd::Print);
        m.item(ui, Cmd::ApplyPatch);
        ui.separator();
        m.item(ui, Cmd::CloseTab);
        m.item(ui, Cmd::CloseAllTabs);
        m.item(ui, Cmd::Exit);
    }

    fn view_menu(&mut self, ui: &mut egui::Ui, m: &mut Menus) {
        let cfg = &mut self.cfg;
        submenu(ui, tr("view.folder_menu"), |ui| {
            check(ui, &mut cfg.tree_mode, tr("view.tree_mode"), "");
            check(
                ui,
                &mut cfg.folder_file_pane,
                tr("view.folder_file_pane"),
                &Cmd::ToggleFilePane.shown_key(),
            );
            ui.separator();
            ui.label(egui::RichText::new(tr("view.show")).weak().small());
            check(ui, &mut cfg.show_identical, tr("view.identical"), "");
            check(ui, &mut cfg.show_different, tr("view.different"), "");
            check(ui, &mut cfg.show_unique[0], tr("view.left_only"), "");
            check(ui, &mut cfg.show_unique[1], tr("view.middle_only"), "");
            check(ui, &mut cfg.show_unique[2], tr("view.right_only"), "");
        });
        submenu(ui, tr("view.text_menu"), |ui| {
            submenu(ui, tr("view.text_layout"), |ui| {
                for layout in TextLayout::ALL {
                    ui.radio_value(&mut cfg.text_layout, layout, tr(layout.key()));
                }
            });
            check(
                ui,
                &mut cfg.show_location_pane,
                tr("view.location_pane"),
                "",
            );
            check(ui, &mut cfg.show_line_detail, tr("view.line_detail"), "");
            check(ui, &mut cfg.show_line_numbers, tr("view.line_numbers"), "");
            check(ui, &mut cfg.show_edit_markers, tr("view.edit_markers"), "");
            ui.separator();
            check(ui, &mut cfg.show_whitespace, tr("view.whitespace"), "");
            check(ui, &mut cfg.syntax_highlight, tr("view.syntax"), "");
            let collapse = Cmd::ToggleCollapse.shown_key();
            check(
                ui,
                &mut cfg.collapse_unchanged,
                tr("cmd.collapse_unchanged"),
                &collapse,
            );
            let wrap = Cmd::ToggleWrap.shown_key();
            check(ui, &mut cfg.word_wrap, tr("cmd.word_wrap"), &wrap);
        });
        ui.separator();
        submenu(ui, tr("label.zoom"), |ui| {
            m.item(ui, Cmd::ZoomIn);
            m.item(ui, Cmd::ZoomOut);
            m.item(ui, Cmd::ZoomReset);
        });
        submenu(ui, tr("view.theme"), |ui| {
            for mode in ThemeMode::ALL {
                if widgets::menu_check(ui, cfg.theme == mode, tr(mode.key()), "").clicked() {
                    cfg.theme = mode;
                    ui.close();
                }
            }
        });
        ui.separator();
        m.item(ui, Cmd::NextTab);
        m.item(ui, Cmd::PrevTab);
    }

    fn compare_menu(&mut self, ui: &mut egui::Ui, m: &mut Menus) {
        for c in [Cmd::NextDiff, Cmd::PrevDiff, Cmd::FirstDiff, Cmd::LastDiff] {
            m.item(ui, c);
        }
        m.item(ui, Cmd::NextConflict);
        m.item(ui, Cmd::PrevConflict);
        ui.separator();
        ui.add_enabled_ui(m.enabled(Cmd::CompareAs(FileKind::Text)), |ui| {
            submenu(ui, tr("cmd.compare_as"), |ui| {
                for kind in [
                    FileKind::Text,
                    FileKind::Binary,
                    FileKind::Image,
                    FileKind::Web,
                ] {
                    m.item_as(ui, kind_label(Some(kind)), Cmd::CompareAs(kind));
                }
            });
        });
        m.item(ui, Cmd::AllDiffs);
        let tab_rules = self.tab_rules(self.active);
        // Listed when the menu opens, not every frame it stays open.
        let names_id = egui::Id::unique("menu_rule_names");
        ui.add_enabled_ui(tab_rules.is_some(), |ui| {
            // Rule names are the user's, like recent paths: not measured, so a long one widens
            // this submenu only, and new ones need no measuring again.
            let shown = submenu_list(ui, tr("menu.rules"), |ui| {
                let current = tab_rules.as_deref().unwrap_or_default();
                if widgets::menu_check(ui, current.is_empty(), tr("rules.none"), "").clicked() {
                    m.rules = Some(String::new());
                }
                let names = ui.ctx().memory_mut(|mem| {
                    mem.data
                        .get_temp_mut_or_insert_with(names_id, || {
                            rules::list()
                                .into_iter()
                                .map(|s| s.name)
                                .collect::<Vec<_>>()
                        })
                        .clone()
                });
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let picked = rules::chosen(current, &refs).map(str::to_string);
                for name in names {
                    if widgets::menu_check(ui, picked.as_ref() == Some(&name), &name, "").clicked()
                    {
                        m.rules = Some(name);
                    }
                }
                ui.separator();
                m.item(ui, Cmd::EditRules);
                if m.rules.is_some() {
                    ui.close();
                }
            });
            if shown.inner.is_none() {
                ui.ctx()
                    .memory_mut(|mem| mem.data.remove::<Vec<String>>(names_id));
            }
        });
        let d = &mut self.cfg.diff;
        submenu(ui, tr("view.compare_menu"), |ui| {
            submenu(ui, tr("opt.whitespace"), |ui| {
                for (mode, key) in [
                    (WhitespaceMode::Compare, "opt.ws_compare"),
                    (WhitespaceMode::IgnoreChange, "opt.ws_change"),
                    (WhitespaceMode::IgnoreAll, "opt.ws_all"),
                ] {
                    if widgets::menu_check(ui, d.whitespace == mode, tr(key), "").clicked() {
                        d.whitespace = mode;
                    }
                }
            });
            submenu(ui, tr("opt.inline"), |ui| {
                for (mode, key) in [
                    (InlineMode::Off, "opt.inline_off"),
                    (InlineMode::Word, "opt.inline_word"),
                    (InlineMode::Char, "opt.inline_char"),
                ] {
                    if widgets::menu_check(ui, d.inline == mode, tr(key), "").clicked() {
                        d.inline = mode;
                    }
                }
            });
            ui.separator();
            check(ui, &mut d.ignore_case, tr("opt.ignore_case"), "");
            check(ui, &mut d.ignore_blank_lines, tr("opt.ignore_blank"), "");
            check(ui, &mut d.ignore_eol, tr("opt.ignore_eol"), "");
            check(ui, &mut d.ignore_numbers, tr("opt.ignore_numbers"), "");
            check(ui, &mut d.ignore_comments, tr("opt.ignore_comments"), "");
            ui.separator();
            check(ui, &mut d.prettify, tr("opt.prettify"), "");
            check(ui, &mut d.detect_moved, tr("opt.detect_moved"), "");
            ui.separator();
            m.item(ui, Cmd::CompareOptions);
        });
        ui.separator();
        m.item(ui, Cmd::Refresh);
    }

    fn tools_menu(&mut self, ui: &mut egui::Ui, m: &mut Menus) {
        m.item(ui, Cmd::Filters);
        m.item(ui, Cmd::Statistics);
        ui.separator();
        let [add, remove, added, removed] = SHELL_KEYS;
        let registered = crate::shell::is_registered();
        if ui
            .add_enabled(!registered, egui::Button::new(tr(add)))
            .clicked()
        {
            match crate::shell::register() {
                Ok(()) => self.notify(tr(added)),
                Err(e) => self.error(e.to_string()),
            }
            ui.close();
        }
        if ui
            .add_enabled(registered, egui::Button::new(tr(remove)))
            .clicked()
        {
            let _ = crate::shell::unregister();
            self.notify(tr(removed));
            ui.close();
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let mut close: Option<usize> = None;
        let mut activate: Option<usize> = None;
        let mut new_tab = false;
        let mut tab_cmd: Option<Cmd> = None;
        let mut drag_move: Option<(usize, usize)> = None;
        let n_tabs = self.tabs.len();
        egui::ScrollArea::horizontal()
            .id_salt("tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut rects = Vec::with_capacity(n_tabs);
                    let mut dragging: Option<usize> = None;
                    for (i, t) in self.tabs.iter().enumerate() {
                        let active = i == self.active;
                        let full = t.title();
                        let (title, folders) = tab_label(&full, &t.paths());
                        let font = if active {
                            theme::semibold(theme::size::DENSE)
                        } else {
                            FontId::proportional(theme::size::DENSE)
                        };
                        let galley = ui.painter().layout_no_wrap(
                            shorten(&title, 40),
                            font,
                            if active { p.text } else { p.text_dim },
                        );
                        let sub = folders.map(|f| {
                            ui.painter().layout_no_wrap(
                                shorten(&f, 36),
                                FontId::proportional(theme::size::CAPTION),
                                p.text_dim,
                            )
                        });
                        let sub_w = sub.as_ref().map_or(0.0, |g| g.size().x + 8.0);
                        let w = 40.0 + galley.size().x + sub_w + 30.0;
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
                        let resp = ui.interact(
                            rect,
                            egui::Id::unique(("tab", t.id)),
                            Sense::click_and_drag(),
                        );
                        resp.widget_info(|| {
                            egui::WidgetInfo::selected(egui::Role::Button, true, active, &full)
                        });
                        rects.push(rect);
                        if resp.dragged() {
                            dragging = Some(i);
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        }
                        let close_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 17.0, rect.center().y + 1.0),
                            vec2(20.0, 20.0),
                        );
                        let close_resp = ui.interact(
                            close_rect,
                            egui::Id::unique(("tab_close", t.id)),
                            Sense::click(),
                        );
                        let painter = ui.painter();
                        if active {
                            let cr = CornerRadius {
                                nw: 9,
                                ne: 9,
                                sw: 0,
                                se: 0,
                            };
                            painter.rect_filled(rect, cr, p.surface);
                            painter.hline(
                                rect.left() + 8.0..=rect.right() - 8.0,
                                rect.top() + 1.0,
                                Stroke::new(2.0_f32, p.accent),
                            );
                        } else if resp.hovered() || close_resp.hovered() {
                            painter.rect_filled(
                                rect.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, 1.0)),
                                8.0,
                                p.control_hover,
                            );
                        }
                        crate::ui::icons::paint(
                            painter,
                            egui::Rect::from_center_size(
                                egui::pos2(rect.left() + 20.0, rect.center().y + 1.0),
                                vec2(14.0, 14.0),
                            ),
                            t.icon(),
                            if active { p.accent } else { p.text_dim },
                        );
                        let title_w = galley.size().x;
                        painter.galley(
                            egui::pos2(
                                rect.left() + 36.0,
                                rect.center().y + 1.0 - galley.size().y / 2.0,
                            ),
                            galley,
                            p.text,
                        );
                        if let Some(sub) = sub {
                            painter.galley(
                                egui::pos2(
                                    rect.left() + 36.0 + title_w + 8.0,
                                    rect.center().y + 1.5 - sub.size().y / 2.0,
                                ),
                                sub,
                                p.text_dim,
                            );
                        }
                        // An unsaved tab shows a dot where its close button is, until pointed at.
                        let modified = t.is_modified();
                        let show_close =
                            (active && !modified) || resp.hovered() || close_resp.hovered();
                        if close_resp.hovered() {
                            painter.rect_filled(close_rect, theme::radius::SM, p.control_active);
                        }
                        if show_close {
                            crate::ui::icons::paint(
                                painter,
                                egui::Rect::from_center_size(close_rect.center(), vec2(11.0, 11.0)),
                                Icon::Close,
                                p.text_dim,
                            );
                        } else if modified {
                            painter.circle_filled(close_rect.center(), 4.0, p.text);
                        }
                        widgets::focus_ring(
                            ui,
                            &resp,
                            rect.shrink2(vec2(0.0, 3.0)),
                            theme::radius::MD,
                        );
                        let close_resp = close_resp.on_hover_text(Cmd::CloseTab.tip());
                        if close_resp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            activate = Some(i);
                        }
                        if active && self.shown_active != Some(i) {
                            resp.scroll_to_me(None);
                            self.shown_active = Some(i);
                        }
                        let paths: Vec<String> =
                            t.source.iter().map(|s| s.display().to_string()).collect();
                        let resp = resp.on_hover_text(if paths.is_empty() {
                            full
                        } else {
                            paths.join("\n")
                        });
                        let last = i + 1 == n_tabs;
                        resp.context_menu(|ui| {
                            widgets::menu_setup(ui);
                            let mut item =
                                |ui: &mut egui::Ui, label: &str, enabled: bool, c: Cmd| {
                                    // Closing this tab is the menu's Close (⌘W) on the tab clicked.
                                    let keys = match c {
                                        Cmd::CloseTabAt(_) => Cmd::CloseTab.shown_key(),
                                        c => c.shown_key(),
                                    };
                                    if widgets::menu_item(ui, enabled, label, &keys).clicked() {
                                        tab_cmd = Some(c);
                                        ui.close();
                                    }
                                };
                            item(ui, tr("menu.close_tab"), true, Cmd::CloseTabAt(i));
                            item(
                                ui,
                                tr("tab.close_others"),
                                n_tabs > 1,
                                Cmd::CloseOtherTabs(i),
                            );
                            item(ui, tr("tab.close_right"), !last, Cmd::CloseTabsToRight(i));
                            item(ui, tr("tab.close_all"), true, Cmd::CloseAllTabs);
                            if !paths.is_empty() {
                                ui.separator();
                                if ui.button(tr("cmd.copy_path")).clicked() {
                                    ui.ctx().copy_text(paths.join("\n"));
                                    ui.close();
                                }
                            }
                        });
                    }
                    if let (Some(from), Some(pos)) = (dragging, ui.ctx().pointer_interact_pos()) {
                        // Move only once the pointer passes the middle of the neighbor, so tabs of
                        // different widths don't swap back and forth.
                        let to = (0..n_tabs).find(|&j| {
                            let r = rects[j];
                            (j > from && pos.x > r.center().x && pos.x <= r.right() + 2.0)
                                || (j < from && pos.x < r.center().x && pos.x >= r.left() - 2.0)
                        });
                        let to = to.or_else(|| {
                            if pos.x > rects[n_tabs - 1].right() {
                                Some(n_tabs - 1)
                            } else if pos.x < rects[0].left() {
                                Some(0)
                            } else {
                                None
                            }
                        });
                        if let Some(to) = to.filter(|&to| to != from) {
                            drag_move = Some((from, to));
                        }
                    }
                    ui.add_space(4.0);
                    let open = tr("menu.open").trim_end_matches(['.', '…']);
                    if widgets::icon_button(
                        ui,
                        Icon::Plus,
                        &widgets::with_key(open, Cmd::Open.key()),
                    )
                    .clicked()
                    {
                        new_tab = true;
                    }
                });
            });
        if let Some(i) = activate {
            self.active = i;
        }
        if let Some((from, to)) = drag_move {
            let active_id = self.tabs.get(self.active).map(|t| t.id);
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            if let Some(i) = self.tabs.iter().position(|t| Some(t.id) == active_id) {
                self.active = i;
                self.shown_active = Some(i);
            }
        }
        if new_tab {
            self.open_dlg = Some(self.new_open_dialog(None));
        }
        if let Some(i) = close {
            self.close_tab(i, false);
        }
        if let Some(c) = tab_cmd {
            let ctx = ui.ctx().clone();
            self.run(c, &ctx);
        }
    }

    /// "File 8 / 42 · Difference 3 / 12" for a tab opened from a folder compare.
    fn file_nav_label(&self, tab: &Tab) -> Option<String> {
        let origin = tab.origin.as_ref()?;
        let Content::Folder(f) = self
            .tabs
            .iter()
            .find(|t| t.id == origin.folder)?
            .content
            .view()
        else {
            return None;
        };
        let mut label = f.file_label(&self.cfg, &origin.rel);
        let position = match tab.content.view() {
            Content::Text(v) => Some(v.diff_position()),
            _ => None,
        };
        if let Some((pos, total)) = position {
            let n = pos.map_or("–".to_string(), |p| (p + 1).to_string());
            label.push_str(" · ");
            label.push_str(&trf(
                "nav.diff_of",
                &[("n", n), ("total", total.to_string())],
            ));
        }
        Some(label)
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let s = self
                .tabs
                .get(self.active)
                .map(|t| match self.file_nav_label(t) {
                    Some(nav) => format!("{nav}  |  {}", t.content.status_bar_line()),
                    None => t.content.status_bar_line(),
                })
                .unwrap_or_else(|| tr("app.ready").to_string());
            let outcome = self.tabs.get(self.active).and_then(|t| t.content.outcome());
            let color = outcome.map_or(p.border_strong, |o| o.color(&p));
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, color);
            if outcome == Some(Outcome::Busy) && ui.style().animation_time > 0.0 {
                // Pulsing ring while a comparison or page load is running.
                let t = (ui.input(|i| i.time) % 1.2) as f32 / 1.2;
                ui.painter().circle_stroke(
                    dot.center(),
                    3.5 + 4.0 * t,
                    Stroke::new(1.5_f32, color.gamma_multiply(1.0 - t)),
                );
                ui.ctx().request_repaint();
            }
            for (k, part) in s.split("  |  ").enumerate() {
                if k > 0 {
                    ui.label(
                        egui::RichText::new("•")
                            .color(p.border_strong)
                            .size(theme::size::CAPTION),
                    );
                }
                let text = egui::RichText::new(part).size(theme::size::SMALL);
                ui.label(if k == 0 {
                    text.color(p.text)
                } else {
                    text.color(p.text_dim)
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let lang = i18n::languages()
                    .iter()
                    .find(|l| l.code == i18n::current_language())
                    .map(|l| l.name.as_str())
                    .unwrap_or_default();
                ui.label(
                    egui::RichText::new(lang)
                        .size(theme::size::SMALL)
                        .color(p.text_dim),
                );
                let (r, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                crate::ui::icons::paint(ui.painter(), r, Icon::Globe, p.text_dim);
            });
        });
    }

    fn start_page(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let mut open: Option<Option<String>> = None;
        let mut submit = false;
        let mut sample = false;
        let mut conflict = false;
        let mut compare: Option<Vec<String>> = None;
        let mut edit: Option<Vec<String>> = None;
        let mut remove: Option<Vec<String>> = None;
        let mut clear_recent = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let w = 760.0_f32.min(ui.available_width() - 48.0).max(320.0);
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.add_space(56.0);
                    ui.horizontal(|ui| {
                        widgets::logo(ui, 56.0);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                egui::RichText::new("OxideMerge")
                                    .font(theme::semibold(theme::size::DISPLAY)),
                            );
                            ui.label(
                                egui::RichText::new(tr("app.tagline"))
                                    .size(theme::size::SUBTITLE)
                                    .color(p.text_dim),
                            );
                        });
                    });
                    ui.add_space(24.0);

                    let (launch, more) = self.start_launcher(ui, w);
                    if launch {
                        submit = true;
                    }
                    if more {
                        open = Some(None);
                    }
                    ui.add_space(4.0);
                    widgets::card(ui)
                        .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
                        .show(ui, |ui| {
                            ui.set_width(w - 22.0);
                            ui.horizontal(|ui| {
                                let title = tr("menu.open_conflict").trim_end_matches(['.', '…']);
                                if widgets::ghost_icon_button(ui, Icon::Warning, title).clicked() {
                                    conflict = true;
                                }
                                ui.label(
                                    egui::RichText::new(tr("start.conflict_desc"))
                                        .size(theme::size::SMALL)
                                        .color(p.text_dim),
                                );
                            });
                        });
                    let mut captured = None;
                    widgets::card(ui)
                        .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
                        .show(ui, |ui| {
                            ui.set_width(w - 22.0);
                            let open = self.baseline_start.open;
                            ui.horizontal(|ui| {
                                if widgets::ghost_icon_button(ui, Icon::Clock, tr("baseline.title"))
                                    .clicked()
                                {
                                    self.baseline_start.open = !open;
                                }
                                ui.label(
                                    egui::RichText::new(tr("start.baselines_desc"))
                                        .size(theme::size::SMALL)
                                        .color(p.text_dim),
                                );
                            });
                            let panel_ui = if open { Some(&mut *ui) } else { None };
                            captured = self.baseline_start.ui(
                                panel_ui,
                                &[],
                                &self.cfg.comparison_options(),
                                &self.cfg.filter,
                            );
                        });
                    if let Some(comparison) = captured {
                        self.open_captured(comparison);
                    }

                    if self.cfg.recent_compares.is_empty() {
                        ui.add_space(16.0);
                        widgets::card(ui)
                            .inner_margin(Margin::same(theme::space::XL))
                            .show(ui, |ui| {
                                ui.set_width(w - 34.0);
                                ui.horizontal(|ui| {
                                    widgets::inline_icon(ui, Icon::Bolt, p.accent);
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.label(
                                            egui::RichText::new(tr("start.try_title"))
                                                .font(theme::semibold(theme::size::SUBTITLE)),
                                        );
                                        ui.label(
                                            egui::RichText::new(tr("start.try_desc"))
                                                .size(theme::size::SMALL)
                                                .color(p.text_dim),
                                        );
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if widgets::bar_button(
                                            ui,
                                            true,
                                            None,
                                            tr("start.try_button"),
                                            true,
                                        )
                                        .clicked()
                                        {
                                            sample = true;
                                        }
                                    });
                                });
                            });
                    }
                    if !self.cfg.recent_compares.is_empty() {
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.set_width(w);
                            widgets::caption(ui, tr("label.recent"));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let full = tr("menu.clear_recent");
                                if widgets::ghost_button_named(ui, tr("start.recent_clear"), full)
                                    .on_hover_text(full)
                                    .clicked()
                                {
                                    clear_recent = true;
                                }
                            });
                        });
                        let card_w = (w - 12.0) / 2.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                            for paths in self.cfg.recent_compares.iter().take(8) {
                                let resp = recent_card(ui, card_w, paths);
                                if resp.clicked() {
                                    compare = Some(paths.clone());
                                }
                                resp.context_menu(|ui| {
                                    widgets::menu_setup(ui);
                                    if ui.button(tr("btn.compare")).clicked() {
                                        compare = Some(paths.clone());
                                        ui.close();
                                    }
                                    if ui.button(tr("start.recent_edit")).clicked() {
                                        edit = Some(paths.clone());
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button(tr("start.recent_remove")).clicked() {
                                        remove = Some(paths.clone());
                                        ui.close();
                                    }
                                });
                            }
                        });
                    }
                    ui.add_space(32.0);
                });
            });
        });
        if let Some(left) = open {
            let mut d = self.new_open_dialog(left);
            d.paths = self.start_paths.clone();
            d.three_way = self.start_three_way;
            self.open_dlg = Some(d);
        }
        if sample {
            self.open_sample();
        }
        if submit {
            let mut d = self.new_open_dialog(None);
            d.paths = self.start_paths.clone();
            d.three_way = self.start_three_way;
            self.submit_open(d);
        }
        if conflict {
            self.run(Cmd::OpenConflict, &ui.ctx().clone());
        }
        if let Some(paths) = compare {
            self.open(OpenRequest {
                paths: paths.iter().map(PathBuf::from).collect(),
                ..Default::default()
            });
        }
        if let Some(paths) = edit {
            let mut d = self.new_open_dialog(None);
            d.three_way = paths.len() == 3;
            let slots: &[usize] = if d.three_way { &[0, 1, 2] } else { &[0, 2] };
            for (&s, p) in slots.iter().zip(&paths) {
                d.paths[s] = p.clone();
            }
            self.open_dlg = Some(d);
        }
        if let Some(paths) = remove {
            self.cfg.remove_recent_compare(&paths);
        }
        if clear_recent {
            self.clear_recent();
        }
    }

    /// Empties the recent comparisons, with a toast that can put them back, together with any
    /// cleared just before whose toast this one replaces.
    fn clear_recent(&mut self) {
        let mut cleared = std::mem::take(&mut self.cfg.recent_compares);
        let earlier = self.toasts.iter().find_map(|t| match &t.undo {
            Some(ToastUndo::Recent(earlier)) => Some(earlier.clone()),
            _ => None,
        });
        for paths in earlier.into_iter().flatten() {
            if !cleared.contains(&paths) {
                cleared.push(paths);
            }
        }
        if !cleared.is_empty() {
            self.push_toast(
                tr("msg.recent_cleared").to_owned(),
                false,
                Some(ToastUndo::Recent(cleared)),
            );
        }
    }

    /// Writes two small folders to a temporary place and compares them, for trying the app
    /// out. The copy lives as long as the tabs showing it.
    fn open_sample(&mut self) {
        let made = (|| -> std::io::Result<tempfile::TempDir> {
            let dir = tempfile::Builder::new()
                .prefix("oxidemerge-sample")
                .tempdir()?;
            let files: &[(&str, &str)] = &[
                ("left/README.md", "# Sample\n\nTwo folders to compare.\n"),
                ("right/README.md", "# Sample\n\nTwo folders to compare and merge.\n"),
                (
                    "left/src/main.rs",
                    "fn main() {\n    let name = \"world\";\n    println!(\"Hello, {}!\", name);\n}\n",
                ),
                (
                    "right/src/main.rs",
                    "fn main() {\n    let name = \"OxideMerge\";\n    println!(\"Hello, {name}!\");\n    println!(\"done\");\n}\n",
                ),
                ("left/same.txt", "Identical on both sides.\n"),
                ("right/same.txt", "Identical on both sides.\n"),
                ("left/notes/old.txt", "Only on the left.\n"),
                ("right/notes/new.txt", "Only on the right.\n"),
            ];
            for (name, text) in files {
                let path = dir.path().join(name);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(path, text)?;
            }
            Ok(dir)
        })();
        let dir = match made {
            Ok(dir) => std::sync::Arc::new(dir),
            Err(e) => return self.error(e.to_string()),
        };
        let tabs = self.tabs.len();
        self.open(OpenRequest {
            paths: vec![dir.path().join("left"), dir.path().join("right")],
            skip_recent: true,
            ..Default::default()
        });
        if self.tabs.len() > tabs {
            if let Some(tab) = self.tabs.last_mut() {
                tab.temps.push(dir);
            }
        }
    }

    /// The start page's compare form: a path field per side, then Compare, or More options
    /// for the Open dialog. Returns whether either button was used.
    fn start_launcher(&mut self, ui: &mut egui::Ui, w: f32) -> (bool, bool) {
        let p = pal(ui);
        let mut go = false;
        let mut more = false;
        widgets::card(ui)
            .inner_margin(Margin::same(theme::space::XL))
            .show(ui, |ui| {
                ui.set_width(w - 34.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    widgets::inline_icon(ui, Icon::Compare, p.accent);
                    ui.label(
                        egui::RichText::new(tr("start.compare"))
                            .font(theme::semibold(theme::size::SUBTITLE)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::toggle_button(
                            ui,
                            &mut self.start_three_way,
                            tr("start.three_way"),
                            tr("start.three_way_tip"),
                        );
                    });
                });
                let three = self.start_three_way;
                let slots: &[usize] = if three { &[0, 1, 2] } else { &[0, 2] };
                let sides = slots.len();
                // Side chip, gaps and the three browse buttons around the path field.
                let field_w = ui.available_width() - 60.0 - 4.0 - 8.0 - 3.0 * 30.0 - 16.0 - 40.0;
                for (k, &s) in slots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let chip = ui.allocate_ui_with_layout(
                            vec2(60.0, 28.0),
                            Layout::left_to_right(Align::Center),
                            |ui| widgets::side_chip(ui, k, sides),
                        );
                        ui.add_space((60.0 - chip.response.rect.width()).max(0.0));
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.start_paths[s])
                                .hint_text(
                                    egui::RichText::new(tr("dlg.path_hint")).color(p.text_dim),
                                )
                                .margin(PATH_FIELD_MARGIN)
                                .desired_width(field_w),
                        );
                        path_status(ui, &field, &self.start_paths[s]);
                        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            go = true;
                        }
                        ui.add_space(4.0);
                        if widgets::icon_button(ui, Icon::File, tr("btn.browse_file")).clicked() {
                            if let Some(f) = rfd::FileDialog::new().pick_file() {
                                self.start_paths[s] = f.display().to_string();
                            }
                        }
                        if widgets::icon_button(ui, Icon::Folder, tr("btn.browse_folder")).clicked()
                        {
                            if let Some(f) = rfd::FileDialog::new().pick_folder() {
                                self.start_paths[s] = f.display().to_string();
                            }
                        }
                        if widgets::icon_button(ui, Icon::Globe, tr("btn.browse_remote")).clicked()
                        {
                            self.start_remote_connect(RemoteTarget::Start(s));
                        }
                    });
                }
                let ready = slots
                    .iter()
                    .all(|&s| !self.start_paths[s].trim().is_empty());
                go &= ready;
                ui.horizontal(|ui| {
                    ui.add_space(64.0);
                    go |= widgets::bar_button(
                        ui,
                        ready,
                        Some(Icon::Compare),
                        tr("btn.compare"),
                        true,
                    )
                    .clicked();
                    more = widgets::ghost_button(ui, tr("start.more_options")).clicked();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(tr("start.drop_hint"))
                                .size(theme::size::SMALL)
                                .color(p.text_dim),
                        );
                        widgets::inline_icon(ui, Icon::Drop, p.text_dim);
                    });
                });
            });
        (go, more)
    }

    // ---------- dialogs ----------

    fn open_dialog(&mut self, ctx: &egui::Context) {
        // Paused (not drawn) while its own remote connection dialog is in front of it, rather
        // than showing two modals at once: `self.open_dlg` stays as it was meanwhile, and
        // `use_remote_fetch` fills in its path field directly once the fetch is done.
        if self.remote_dlg.is_some() || self.remote_load.is_some() {
            return;
        }
        let Some(mut d) = self.open_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut go = false;
        let mut remote_for: Option<usize> = None;
        let recent = self.cfg.recent_paths.clone();
        let mut filters: Vec<String> = self.cfg.recent_filters.clone();
        for f in &self.named_filters {
            filters.push(format!("{FLT_PREFIX}{}", f.name));
        }
        let modal = egui::Modal::new(egui::Id::unique("open_dialog")).show(ctx, |ui| {
            ui.set_width(660.0);
            let p = pal(ui);
            if widgets::dialog_title(ui, tr("dlg.open")) {
                keep = false;
            }
            let sides = if d.three_way { 3 } else { 2 };
            egui::Grid::new("open_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    // Overflowing rows widen the grid for later rows, so every row shares the first row's width.
                    let mut field_w: Option<f32> = None;
                    // Width of the buttons after the path field, measured on the previous frame.
                    let trailing_id = egui::Id::unique("open_dlg_trailing_w");
                    for s in 0..3 {
                        if !d.three_way && s == 1 {
                            continue;
                        }
                        let side_idx = if !d.three_way && s == 2 { 1 } else { s };
                        widgets::side_chip(ui, side_idx, sides);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let trailing =
                                ui.data(|m| m.get_temp::<f32>(trailing_id)).unwrap_or(150.0);
                            let w = *field_w.get_or_insert(ui.available_width() - trailing - 16.0);
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut d.paths[s])
                                    .hint_text(
                                        egui::RichText::new(tr("dlg.path_hint")).color(p.text_dim),
                                    )
                                    .margin(PATH_FIELD_MARGIN)
                                    .desired_width(w),
                            );
                            path_status(ui, &field, &d.paths[s]);
                            ui.add_space(4.0);
                            let recent_id = egui::Id::unique(("open_recent", s));
                            let r = widgets::icon_button_enabled(
                                ui,
                                !recent.is_empty(),
                                Icon::Clock,
                                tr("label.recent"),
                            );
                            egui::Popup::from_toggle_button_response(&r)
                                .id(recent_id)
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                .show(|ui| {
                                    widgets::menu_scroll(ui, |ui| {
                                        ui.set_min_width(320.0);
                                        for r in &recent {
                                            if ui.selectable_label(false, r).clicked() {
                                                d.paths[s] = r.clone();
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                            if widgets::icon_button(ui, Icon::File, tr("btn.browse_file")).clicked()
                            {
                                if let Some(p) = rfd::FileDialog::new().pick_file() {
                                    d.paths[s] = p.display().to_string();
                                }
                            }
                            if widgets::icon_button(ui, Icon::Folder, tr("btn.browse_folder"))
                                .clicked()
                            {
                                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                    d.paths[s] = p.display().to_string();
                                }
                            }
                            if widgets::icon_button(ui, Icon::Globe, tr("btn.browse_remote"))
                                .clicked()
                            {
                                remote_for = Some(s);
                            }
                            let lock = if d.read_only[s] {
                                Icon::Lock
                            } else {
                                Icon::Unlock
                            };
                            widgets::toggle_icon_button(
                                ui,
                                &mut d.read_only[s],
                                Some(lock),
                                "",
                                tr("label.read_only"),
                            );
                            let trailing = ui.min_rect().right() - field.rect.right();
                            ui.data_mut(|m| m.insert_temp(trailing_id, trailing));
                        });
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
            egui::Frame::new()
                .fill(p.surface_alt)
                .stroke(Stroke::new(1.0_f32, p.border))
                .corner_radius(theme::radius::LG)
                .inner_margin(Margin::same(theme::space::LG))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Grid::new("open_opts")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(tr("label.filter")).color(p.text_dim));
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.filter)
                                        .margin(vec2(8.0, 4.0))
                                        .desired_width(300.0),
                                );
                                egui::ComboBox::from_id_salt("open_filters")
                                    .selected_text("")
                                    .width(28.0)
                                    .show_ui(ui, |ui| {
                                        for f in &filters {
                                            if ui.selectable_label(d.filter == *f, f).clicked() {
                                                d.filter = f.clone();
                                            }
                                        }
                                    });
                            });
                            ui.end_row();
                            ui.label(egui::RichText::new(tr("label.method")).color(p.text_dim));
                            egui::ComboBox::from_id_salt("open_method")
                                .width(220.0)
                                .selected_text(tr(d.method.key()))
                                .show_ui(ui, |ui| {
                                    for m in CompareMethod::ALL {
                                        ui.selectable_value(&mut d.method, m, tr(m.key()));
                                    }
                                });
                            ui.end_row();
                            ui.label(egui::RichText::new(tr("label.compare_as")).color(p.text_dim));
                            let mut kind = d.kind;
                            widgets::segmented(
                                ui,
                                &mut kind,
                                &[
                                    (None, kind_label(None)),
                                    (Some(FileKind::Text), kind_label(Some(FileKind::Text))),
                                    (Some(FileKind::Binary), kind_label(Some(FileKind::Binary))),
                                    (Some(FileKind::Image), kind_label(Some(FileKind::Image))),
                                    (Some(FileKind::Web), kind_label(Some(FileKind::Web))),
                                ],
                            );
                            d.kind = kind;
                            ui.end_row();
                            ui.label("");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut d.recursive, tr("label.recursive"));
                                ui.add_space(12.0);
                                ui.checkbox(&mut d.three_way, tr("label.three_way"));
                            });
                            ui.end_row();
                        });
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.compare")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.any())
                {
                    go = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    keep = false;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if widgets::ghost_icon_button(ui, Icon::Swap, tr("btn.swap")).clicked() {
                        d.paths.swap(0, 2);
                        d.read_only.swap(0, 2);
                    }
                });
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if let Some(s) = remote_for {
            self.open_dlg = Some(d);
            self.start_remote_connect(RemoteTarget::Open(s));
        } else if go {
            self.submit_open(d);
        } else if keep {
            self.open_dlg = Some(d);
        }
    }

    /// Opens what the Open dialog (or the start page) holds; reopens the dialog when a path
    /// is missing.
    fn submit_open(&mut self, d: OpenDialog) {
        let idx: Vec<usize> = if d.three_way {
            vec![0, 1, 2]
        } else {
            vec![0, 2]
        };
        let raw: Vec<String> = idx
            .iter()
            .map(|&i| d.paths[i].trim().trim_matches('"').to_string())
            .collect();
        if raw.iter().any(|p| p.is_empty()) {
            self.open_dlg = Some(d);
            self.error(tr("msg.need_paths"));
            return;
        }
        self.cfg.filter = d.filter.clone();
        self.cfg.folder.method = d.method;
        self.cfg.folder.recursive = d.recursive;
        self.cfg.add_recent_filter(&d.filter);
        let req = OpenRequest {
            paths: raw.iter().map(PathBuf::from).collect(),
            read_only: idx.iter().map(|&i| d.read_only[i]).collect(),
            kind: d.kind,
            recursive: Some(d.recursive),
            filter: Some(d.filter.clone()),
            method: Some(d.method),
            ..Default::default()
        };
        self.open(req);
    }

    fn options_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.options_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut apply = false;
        let mut leave = false;
        let results = search_options(&d.search);
        let modal = egui::Modal::new(egui::Id::unique("options_dialog")).show(ctx, |ui| {
            ui.set_width(780.0);
            if widgets::dialog_title(ui, tr("menu.options")) {
                leave = true;
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(190.0);
                    widgets::search_edit(ui, &mut d.search, tr("opt.search"), 190.0);
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let searching = !d.search.trim().is_empty();
                    for (t, icon, key) in OptTab::ALL {
                        if searching && !results.iter().any(|(rt, _)| *rt == t) {
                            continue;
                        }
                        let sel = !searching && d.tab == t;
                        if widgets::nav_item(ui, sel, icon, tr(key)).clicked() {
                            d.tab = t;
                            d.search.clear();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_min_height(420.0);
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            if !d.search.trim().is_empty() {
                                if results.is_empty() {
                                    ui.add_space(24.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            egui::RichText::new(tr("opt.no_results"))
                                                .color(pal(ui).text_dim),
                                        );
                                    });
                                }
                                ui.spacing_mut().item_spacing.y = 2.0;
                                for (t, label) in &results {
                                    let (_, icon, key) =
                                        OptTab::ALL.into_iter().find(|x| x.0 == *t).unwrap();
                                    if option_result_row(ui, icon, label, tr(key)).clicked() {
                                        d.tab = *t;
                                        d.search.clear();
                                    }
                                }
                                return;
                            }
                            match d.tab {
                                OptTab::General => {
                                    let c = &mut d.cfg;
                                    egui::ComboBox::from_label(tr("menu.language"))
                                        .selected_text(
                                            i18n::languages()
                                                .iter()
                                                .find(|l| l.code == c.language)
                                                .map(|l| l.name.clone())
                                                .unwrap_or_else(|| tr("lang.system").into()),
                                        )
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut c.language,
                                                String::new(),
                                                tr("lang.system"),
                                            );
                                            for l in i18n::languages() {
                                                ui.selectable_value(
                                                    &mut c.language,
                                                    l.code.clone(),
                                                    &l.name,
                                                );
                                            }
                                        });
                                    egui::ComboBox::from_label(tr("view.theme"))
                                        .selected_text(tr(c.theme.key()))
                                        .show_ui(ui, |ui| {
                                            for m in ThemeMode::ALL {
                                                ui.selectable_value(&mut c.theme, m, tr(m.key()));
                                            }
                                        });
                                    ui.add(
                                        egui::Slider::new(&mut c.font_size, 8.0..=32.0)
                                            .text(tr("opt.font_size")),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut c.tab_size, 1..=16)
                                            .text(tr("opt.tab_size")),
                                    );
                                    ui.label(egui::RichText::new(tr("view.text_layout")).strong());
                                    for layout in TextLayout::ALL {
                                        ui.radio_value(
                                            &mut c.text_layout,
                                            layout,
                                            tr(layout.key()),
                                        );
                                    }
                                    ui.small(tr("view.layout_help"));
                                    ui.checkbox(&mut c.show_line_numbers, tr("view.line_numbers"));
                                    ui.checkbox(
                                        &mut c.show_location_pane,
                                        tr("view.location_pane"),
                                    );
                                    ui.checkbox(&mut c.show_whitespace, tr("view.whitespace"));
                                    ui.checkbox(&mut c.syntax_highlight, tr("view.syntax"));
                                    ui.label(egui::RichText::new(tr("opt.accessibility")).strong());
                                    ui.checkbox(&mut c.color_blind_colors, tr("opt.color_blind"));
                                    ui.checkbox(&mut c.high_contrast, tr("opt.high_contrast"));
                                    ui.checkbox(&mut c.reduce_motion, tr("opt.reduce_motion"));
                                    ui.checkbox(&mut c.compact_density, tr("opt.compact"));
                                    ui.checkbox(
                                        &mut c.collapse_unchanged,
                                        tr("cmd.collapse_unchanged"),
                                    );
                                    ui.checkbox(&mut c.word_wrap, tr("cmd.word_wrap"));
                                    ui.checkbox(&mut c.backup_on_save, tr("opt.backup_on_save"));
                                    ui.checkbox(&mut c.use_recycle_bin, tr("opt.recycle_bin"));
                                    ui.checkbox(&mut c.confirm_operations, tr("opt.confirm_ops"));
                                    ui.checkbox(&mut c.restore_session, tr("opt.restore_session"));
                                }
                                OptTab::Compare => {
                                    let o = &mut d.cfg.diff;
                                    ui.label(egui::RichText::new(tr("opt.whitespace")).strong());
                                    ui.radio_value(
                                        &mut o.whitespace,
                                        WhitespaceMode::Compare,
                                        tr("opt.ws_compare"),
                                    );
                                    ui.radio_value(
                                        &mut o.whitespace,
                                        WhitespaceMode::IgnoreChange,
                                        tr("opt.ws_change"),
                                    );
                                    ui.radio_value(
                                        &mut o.whitespace,
                                        WhitespaceMode::IgnoreAll,
                                        tr("opt.ws_all"),
                                    );
                                    ui.separator();
                                    ui.checkbox(&mut o.ignore_blank_lines, tr("opt.ignore_blank"));
                                    ui.checkbox(&mut o.ignore_case, tr("opt.ignore_case"));
                                    ui.checkbox(&mut o.ignore_eol, tr("opt.ignore_eol"));
                                    ui.checkbox(&mut o.ignore_numbers, tr("opt.ignore_numbers"));
                                    ui.checkbox(&mut o.ignore_comments, tr("opt.ignore_comments"))
                                        .on_hover_text(tr("opt.ignore_comments_hint"));
                                    ui.checkbox(&mut o.prettify, tr("opt.prettify"))
                                        .on_hover_text(tr("opt.prettify_hint"));
                                    ui.checkbox(&mut o.ignore_codepage, tr("opt.ignore_codepage"));
                                    ui.checkbox(&mut o.detect_moved, tr("opt.detect_moved"));
                                    let algorithm_name = |a: DiffAlgorithm| match a {
                                        DiffAlgorithm::Myers => "Myers",
                                        DiffAlgorithm::Patience => "Patience",
                                        DiffAlgorithm::Lcs => "LCS",
                                    };
                                    egui::ComboBox::from_label(tr("opt.algorithm"))
                                        .selected_text(algorithm_name(o.algorithm))
                                        .show_ui(ui, |ui| {
                                            for a in [
                                                DiffAlgorithm::Myers,
                                                DiffAlgorithm::Patience,
                                                DiffAlgorithm::Lcs,
                                            ] {
                                                ui.selectable_value(
                                                    &mut o.algorithm,
                                                    a,
                                                    algorithm_name(a),
                                                );
                                            }
                                        });
                                    egui::ComboBox::from_label(tr("opt.inline"))
                                        .selected_text(match o.inline {
                                            InlineMode::Off => tr("opt.inline_off"),
                                            InlineMode::Word => tr("opt.inline_word"),
                                            InlineMode::Char => tr("opt.inline_char"),
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut o.inline,
                                                InlineMode::Off,
                                                tr("opt.inline_off"),
                                            );
                                            ui.selectable_value(
                                                &mut o.inline,
                                                InlineMode::Word,
                                                tr("opt.inline_word"),
                                            );
                                            ui.selectable_value(
                                                &mut o.inline,
                                                InlineMode::Char,
                                                tr("opt.inline_char"),
                                            );
                                        });
                                    ui.horizontal(|ui| {
                                        ui.checkbox(
                                            &mut o.numeric_tolerance_enabled,
                                            tr("opt.numeric_tolerance"),
                                        )
                                        .on_hover_text(tr("opt.numeric_tolerance_hint"));
                                        ui.add_enabled(
                                            o.numeric_tolerance_enabled,
                                            egui::DragValue::new(&mut o.numeric_tolerance)
                                                .range(0.0..=f64::MAX)
                                                .speed(0.0001)
                                                .max_decimals(9),
                                        );
                                    });
                                }
                                OptTab::Rules => rules_options(ui, &mut d),
                                OptTab::Folder => {
                                    let f = &mut d.cfg.folder;
                                    egui::ComboBox::from_label(tr("label.method"))
                                        .selected_text(tr(f.method.key()))
                                        .show_ui(ui, |ui| {
                                            for m in CompareMethod::ALL {
                                                ui.selectable_value(&mut f.method, m, tr(m.key()));
                                            }
                                        });
                                    ui.checkbox(&mut f.recursive, tr("label.recursive"));
                                    ui.checkbox(
                                        &mut f.include_unique_subfolders,
                                        tr("opt.unique_subfolders"),
                                    );
                                    ui.checkbox(&mut f.skip_hidden, tr("opt.skip_hidden"));
                                    ui.add_enabled(
                                        f.method == CompareMethod::FullContents,
                                        egui::Checkbox::new(
                                            &mut f.structured,
                                            tr("opt.structured_equal"),
                                        ),
                                    )
                                    .on_hover_text(tr("opt.structured_equal_hint"))
                                    .on_disabled_hover_text(tr("opt.structured_equal_hint"));
                                    ui.add(
                                        egui::Slider::new(&mut f.time_tolerance_secs, 0..=10)
                                            .text(tr("opt.time_tolerance")),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut f.threads, 0..=32)
                                            .text(tr("opt.threads")),
                                    );
                                }
                                OptTab::Media => {
                                    let c = &mut d.cfg;
                                    ui.add(
                                        egui::Slider::new(&mut c.image_threshold, 0..=255)
                                            .text(tr("img.threshold")),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut c.image_block, 1..=128)
                                            .text(tr("img.block_size")),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut c.binary_bytes_per_line, 4..=64)
                                            .text(tr("opt.bytes_per_line")),
                                    );
                                }
                                OptTab::Plugins => {
                                    ui.checkbox(
                                        &mut d.cfg.diff.extract_documents,
                                        tr("opt.extract_documents"),
                                    )
                                    .on_hover_text(tr("opt.extract_documents_hint"));
                                    ui.separator();
                                    ui.label(tr("opt.unpackers_hint"));
                                    ui.horizontal_wrapped(|ui| {
                                        for (name, preset) in Unpacker::presets() {
                                            if ui.button(name).clicked() {
                                                d.cfg.unpackers.push(preset);
                                            }
                                        }
                                    });
                                    let mut remove = None;
                                    let pattern_w = 180.0;
                                    let command_w = (ui.available_width()
                                        - pattern_w
                                        - 24.0
                                        - 30.0
                                        - 3.0 * ui.spacing().item_spacing.x
                                        - 16.0)
                                        .max(260.0);
                                    egui::Grid::new("unpackers")
                                        .num_columns(4)
                                        .striped(true)
                                        .show(ui, |ui| {
                                            ui.label("");
                                            ui.label(tr("opt.pattern"));
                                            ui.label(tr("opt.command"));
                                            ui.label("");
                                            ui.end_row();
                                            for (i, u) in d.cfg.unpackers.iter_mut().enumerate() {
                                                ui.checkbox(&mut u.enabled, "").on_hover_text(
                                                    u.check_dependency()
                                                        .map(|_| {
                                                            tr("plugins.available").to_string()
                                                        })
                                                        .unwrap_or_else(|e| e.to_string()),
                                                );
                                                let h = ui.spacing().interact_size.y;
                                                ui.add_sized(
                                                    [pattern_w, h],
                                                    egui::TextEdit::singleline(&mut u.pattern),
                                                );
                                                ui.add_sized(
                                                    [command_w, h],
                                                    egui::TextEdit::singleline(&mut u.command),
                                                )
                                                .on_hover_text(u.command.as_str());
                                                if widgets::icon_button(
                                                    ui,
                                                    Icon::Close,
                                                    tr("cmd.delete"),
                                                )
                                                .clicked()
                                                {
                                                    remove = Some(i);
                                                }
                                                ui.end_row();
                                            }
                                        });
                                    if let Some(i) = remove {
                                        d.cfg.unpackers.remove(i);
                                    }
                                    if widgets::ghost_icon_button(ui, Icon::Plus, tr("btn.add"))
                                        .clicked()
                                    {
                                        d.cfg.unpackers.push(Unpacker {
                                            enabled: true,
                                            pattern: "*.ext".into(),
                                            command: "tool ${file}".into(),
                                        });
                                    }
                                }
                            }
                        });
                });
            });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.ok")).clicked() {
                    apply = true;
                    keep = false;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    leave = true;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if widgets::ghost_icon_button(ui, Icon::Undo, tr("btn.defaults")).clicked() {
                        let defaults = Config {
                            recent_paths: std::mem::take(&mut d.cfg.recent_paths),
                            recent_compares: std::mem::take(&mut d.cfg.recent_compares),
                            recent_filters: std::mem::take(&mut d.cfg.recent_filters),
                            ..Config::default()
                        };
                        d.cfg = defaults;
                    }
                });
            });
        });
        if (leave || modal.should_close()) && may_leave_rule_edit(&mut d) {
            keep = false;
        }
        if apply && d.rule_edit.as_ref().is_some_and(RuleEdit::changed) && !save_rule_edit(&mut d) {
            apply = false;
            keep = true;
            d.tab = OptTab::Rules;
        }
        if apply || !keep {
            for (old, new) in std::mem::take(&mut d.rules_renamed) {
                self.rules_renamed(&old, &new);
            }
        }
        if d.rules_touched && (apply || !keep) {
            self.rules_changed();
        }
        if apply {
            rules::apply_selected(&mut d.cfg);
            let folder_changed = d.cfg.folder != self.cfg.folder
                || d.cfg.diff != self.cfg.diff
                || d.cfg.unpackers != self.cfg.unpackers;
            self.cfg = d.cfg;
            if folder_changed {
                let named = self.named_filters.clone();
                for t in &mut self.tabs {
                    if let Content::Folder(v) = &mut t.content {
                        v.refresh(&self.cfg, &named);
                    }
                }
            }
        } else if keep {
            self.options_dlg = Some(d);
        }
    }

    fn filters_dialog(&mut self, ctx: &egui::Context) {
        if !self.filters_dlg {
            return;
        }
        let mut open = true;
        let mut reload = false;
        let mut failed = None;
        let mut apply: Option<String> = None;
        let default_filter = Config::default().filter;
        let can_clear = self.cfg.filter != default_filter
            || matches!(self.tabs.get(self.active).map(|t| t.content.view()), Some(Content::Folder(v)) if v.filter_spec != default_filter);
        egui::Window::new(tr("menu.filters"))
            .open(&mut open)
            .default_width(560.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(tr("filters.hint"));
                ui.separator();
                egui::Grid::new("filters_grid")
                    .num_columns(4)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong(tr("col.name"));
                        ui.strong(tr("filters.desc"));
                        ui.strong(tr("filters.source"));
                        ui.label("");
                        ui.end_row();
                        for f in &self.named_filters {
                            ui.label(&f.name);
                            ui.label(&f.desc);
                            ui.label(
                                f.source
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| tr("filters.builtin").into()),
                            );
                            if ui.button(tr("filters.use")).clicked() {
                                apply = Some(format!("{FLT_PREFIX}{}", f.name));
                            }
                            ui.end_row();
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(tr("filters.new")).clicked() {
                        if let Some(dir) = crate::config::config_dir().map(|d| d.join("Filters")) {
                            let mut k = 1;
                            let mut path = dir.join("MyFilter.flt");
                            while path.exists() {
                                k += 1;
                                path = dir.join(format!("MyFilter{k}.flt"));
                            }
                            let text = filter::FLT_TEMPLATE
                                .replace("My Filter", &format!("My Filter {k}"));
                            let written = std::fs::create_dir_all(&dir)
                                .and_then(|()| std::fs::write(&path, text));
                            match written {
                                Ok(()) => {
                                    #[cfg(windows)]
                                    let _ =
                                        std::process::Command::new("notepad").arg(&path).spawn();
                                    reload = true;
                                }
                                Err(e) => failed = Some(format!("{}: {e}", path.display())),
                            }
                        }
                    }
                    if ui.button(tr("filters.open_folder")).clicked() {
                        if let Some(dir) = crate::config::config_dir().map(|d| d.join("Filters")) {
                            failed = open_folder(&dir).err();
                        }
                    }
                    if ui.button(tr("filters.reload")).clicked() {
                        reload = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(format!("{}: {}", tr("filters.current"), self.cfg.filter));
                    if ui
                        .add_enabled(can_clear, egui::Button::new(tr("filters.clear")))
                        .clicked()
                    {
                        apply = Some(default_filter.clone());
                    }
                });
            });
        if reload {
            self.named_filters = filter::load_all_filters();
        }
        if let Some(e) = failed {
            self.error(e);
        }
        if let Some(spec) = apply {
            self.cfg.filter = spec;
            if let Some(d) = &mut self.open_dlg {
                d.filter = self.cfg.filter.clone();
            }
            if let Some(Content::Folder(v)) =
                self.tabs.get_mut(self.active).map(|t| t.content.view_mut())
            {
                v.filter_spec = self.cfg.filter.clone();
                v.refresh(&self.cfg, &self.named_filters);
            }
        }
        self.filters_dlg = open;
    }

    fn patch_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.patch_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut go = false;
        let modal = egui::Modal::new(egui::Id::unique("patch_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            if widgets::dialog_title(ui, tr("menu.patch").trim_end_matches(['.', '…'])) {
                keep = false;
            }
            egui::Grid::new("patch_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(tr("patch.output"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.path)
                                .margin(vec2(8.0, 5.0))
                                .desired_width(ui.available_width() - 40.0),
                        );
                        if widgets::icon_button(ui, Icon::Folder, tr("dlg.save_as")).clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("Patch", &["patch", "diff"])
                                .set_file_name("changes.patch")
                                .save_file()
                            {
                                d.path = p.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    widgets::segmented(
                        ui,
                        &mut d.style,
                        &[
                            (PatchStyle::Normal, tr("patch.normal")),
                            (PatchStyle::Context, tr("patch.context")),
                            (PatchStyle::Unified, tr("patch.unified")),
                        ],
                    );
                    ui.end_row();
                    ui.label(tr("patch.context_lines"));
                    ui.add(egui::Slider::new(&mut d.context, 0..=20));
                    ui.end_row();
                });
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, tr("btn.ok")).clicked() {
                    go = true;
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                    keep = false;
                }
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if go {
            self.cfg.patch_style = d.style;
            self.cfg.patch_context = d.context;
            let text = match self.tabs.get(self.active).map(|t| t.content.view()) {
                Some(Content::Text(v)) => v.make_patch(d.style, d.context),
                Some(Content::Folder(v)) if v.n() == 2 => {
                    v.make_patch(d.style, d.context).map(Some)
                }
                _ => Ok(None),
            };
            match text {
                Err(e) => self.error(e),
                Ok(None) => self.error(tr("msg.patch_two_way")),
                Ok(Some(_)) if d.path.trim().is_empty() => self.patch_dlg = Some(d),
                Ok(Some(t)) => {
                    let path = Path::new(d.path.trim());
                    match crate::core::fsutil::write_atomic_with(path, t.as_bytes(), false) {
                        Ok(()) => self.notify(trf("msg.saved", &[("file", d.path.clone())])),
                        Err(e) => self.error(format!("{e:#}")),
                    }
                }
            }
        } else if keep {
            self.patch_dlg = Some(d);
        }
    }

    fn prompts(&mut self, ctx: &egui::Context) {
        let Some(p) = self.prompt.take() else { return };
        let mut keep = true;
        match &p {
            Prompt::CloseTab(id) | Prompt::SwitchFile(id, ..) => {
                let Some(i) = self.tabs.iter().position(|t| t.id == *id) else {
                    if matches!(p, Prompt::CloseTab(_)) {
                        self.ask_next_close();
                    }
                    return;
                };
                let finish = |app: &mut Self| match p {
                    Prompt::SwitchFile(_, forward, conflicts) => {
                        app.switch_file(i, forward, conflicts)
                    }
                    _ => app.close_tab(i, true),
                };
                let title = self.tabs.get(i).map(|t| t.title()).unwrap_or_default();
                let mut action = 0;
                let modal = egui::Modal::new(egui::Id::unique("close_tab")).show(ctx, |ui| {
                    ui.set_min_width(420.0);
                    ui.heading(tr("dlg.unsaved"));
                    ui.label(trf("msg.save_changes", &[("name", title)]));
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("btn.save")).clicked() {
                            action = 1;
                        }
                        if widgets::secondary_button(ui, tr("btn.discard")).clicked() {
                            action = 2;
                        }
                        if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                            action = 3;
                        }
                    });
                });
                if action == 0 && modal.should_close() {
                    action = 3;
                }
                let queued = matches!(p, Prompt::CloseTab(_));
                match action {
                    1 => {
                        keep = false;
                        let id = self.tabs[i].id;
                        match self.tabs[i].save() {
                            Ok(()) => {
                                let errors = self.after_save(&[id]);
                                if !errors.is_empty() {
                                    self.close_queue.clear();
                                    self.error(errors.join("\n"));
                                } else if self.after_uploads(p, queued) {
                                    // Closed once its uploads succeed; see `upload_dialog`.
                                } else if !self.tabs[i].is_modified() {
                                    finish(self);
                                    if queued {
                                        self.ask_next_close();
                                    }
                                } else {
                                    self.close_queue.clear();
                                }
                            }
                            Err(e) => {
                                self.close_queue.clear();
                                // What was written is uploaded even so.
                                let mut errors = vec![e.to_string()];
                                errors.extend(self.after_save(&[id]));
                                self.error(errors.join("\n"));
                            }
                        }
                    }
                    2 => {
                        keep = false;
                        finish(self);
                        if queued {
                            self.ask_next_close();
                        }
                    }
                    3 => {
                        keep = false;
                        self.close_queue.clear();
                    }
                    _ => {}
                }
            }
            Prompt::Exit => {
                let mut action = 0;
                let modal = egui::Modal::new(egui::Id::unique("exit")).show(ctx, |ui| {
                    ui.set_min_width(420.0);
                    ui.heading(tr("dlg.unsaved"));
                    ui.label(tr("msg.exit_unsaved"));
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("btn.save_all")).clicked() {
                            action = 1;
                        }
                        if widgets::secondary_button(ui, tr("btn.discard")).clicked() {
                            action = 2;
                        }
                        if widgets::secondary_button(ui, tr("btn.cancel")).clicked() {
                            action = 3;
                        }
                    });
                });
                if action == 0 && modal.should_close() {
                    action = 3;
                }
                match action {
                    1 => {
                        keep = false;
                        let mut err = None;
                        let mut saved = Vec::new();
                        for i in 0..self.tabs.len() {
                            if let Err(e) = self.tabs[i].save() {
                                err = Some(e.to_string());
                            }
                            // What was written is uploaded even when another side failed.
                            saved.push(self.tabs[i].id);
                        }
                        if let Some(e) = self.after_save(&saved).into_iter().next() {
                            err = Some(e);
                        }
                        match err {
                            Some(e) => self.error(e),
                            None if self.after_uploads(Prompt::Exit, false) => {}
                            None => {
                                if !self.tabs.iter().any(|t| t.is_modified()) {
                                    self.allow_close = true;
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                            }
                        }
                    }
                    2 => {
                        keep = false;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    3 => keep = false,
                    _ => {}
                }
            }
        }
        if keep && self.prompt.is_none() {
            self.prompt = Some(p);
        }
    }

    /// Non-blocking notifications stacked above the status bar, newest at the bottom.
    fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let p = theme::palette(self.cfg.dark_mode);
        let now = ctx.input(|i| i.time);
        let mut close: Option<usize> = None;
        let mut undo: Option<ToastUndo> = None;
        let screen = ctx.content_rect();
        let mut bottom = screen.bottom() - 40.0;
        for i in (0..self.toasts.len()).rev() {
            let t = &mut self.toasts[i];
            let shown = *t.shown.get_or_insert(now);
            let age = now - shown;
            let life = if t.undo.is_some() {
                UNDO_TOAST_SECS
            } else {
                TOAST_SECS
            };
            let alpha = if t.error {
                1.0
            } else {
                ((life - age) / 0.3).clamp(0.0, 1.0) as f32
            };
            let accent = if t.error { p.danger } else { p.success };
            let area = egui::Area::new(egui::Id::unique(("toast", i)))
                .order(egui::Order::Tooltip)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(egui::pos2(screen.right() - 16.0, bottom))
                .show(ctx, |ui| {
                    ui.set_opacity(alpha);
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0_f32, p.border_strong))
                        .corner_radius(theme::radius::LG)
                        .shadow(ctx.global_style().visuals.popup_shadow)
                        .inner_margin(Margin {
                            left: 12,
                            right: 6,
                            top: 8,
                            bottom: 8,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.horizontal(|ui| {
                                if t.error {
                                    widgets::inline_icon(ui, Icon::Warning, accent);
                                } else {
                                    let (r, _) =
                                        ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                    ui.painter().circle_filled(r.center(), 4.0, accent);
                                }
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&t.text).size(theme::size::CONTROL),
                                    )
                                    .wrap(),
                                );
                                if t.undo.is_some()
                                    && widgets::ghost_icon_button(ui, Icon::Undo, tr("cmd.undo"))
                                        .clicked()
                                {
                                    undo = t.undo.clone();
                                    close = Some(i);
                                }
                                if widgets::icon_button(ui, Icon::Close, tr("btn.close")).clicked()
                                {
                                    close = Some(i);
                                }
                            });
                        });
                });
            let hovered = area.response.contains_pointer();
            if hovered {
                t.shown = Some(now);
            } else if !t.error && age >= life {
                close = close.or(Some(i));
            }
            bottom = area.response.rect.top() - 8.0;
        }
        if let Some(i) = close {
            self.toasts.remove(i);
        }
        match undo {
            Some(ToastUndo::Tab(id)) => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    match t.content.view_mut() {
                        Content::Text(v) => v.undo(),
                        Content::Binary(v) => v.undo(),
                        _ => {}
                    }
                }
            }
            Some(ToastUndo::Recent(cleared)) => self.cfg.restore_recent_compares(cleared),
            Some(ToastUndo::Workspace(at, w))
                if !self.cfg.workspaces.iter().any(|o| o.is_named(&w.name)) =>
            {
                let at = at.min(self.cfg.workspaces.len());
                self.cfg.workspaces.insert(at, w);
            }
            Some(ToastUndo::Workspace(..)) | None => {}
        }
        if self.toasts.iter().any(|t| !t.error) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if self.about && about::show(ctx) {
            self.about = false;
        }
        if self.help && help::show(ctx, crate::config::config_path().as_deref()) {
            self.help = false;
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let busy = self.prompt.is_some()
            || self.sync_recovery.is_some()
            || self.patch_dlg.is_some()
            || self.stats_dlg.is_some()
            || self.git_dlg.is_some()
            || self.rule_dlg.is_some()
            || self.palette.is_some()
            || self.help
            || self.about
            || matches!(
                self.tabs.get(self.active).map(|t| t.content.view()),
                Some(Content::Folder(v)) if v.has_dialog()
            );
        if dropped.is_empty() || busy {
            return;
        }
        // One rules file is imported, as are any dropped on the options; more are compared.
        let import = self.options_dlg.is_some() || dropped.len() == 1;
        let (rule_files, dropped): (Vec<PathBuf>, Vec<PathBuf>) = dropped
            .into_iter()
            .partition(|p| import && rules::is_rules_file(p));
        if !rule_files.is_empty() {
            let (msg, failed) = import_rules(&rule_files);
            if failed {
                self.error(msg);
            } else {
                self.notify(msg);
            }
            if let Some(d) = &mut self.options_dlg {
                d.rule_sets = rules::list();
                d.rules_touched = true;
            } else {
                self.rules_changed();
            }
        }
        if dropped.is_empty() || self.options_dlg.is_some() {
            return;
        }
        if let Some(d) = &mut self.open_dlg {
            let slots: Vec<usize> = if d.three_way {
                vec![0, 1, 2]
            } else {
                vec![0, 2]
            };
            let mut it = dropped.iter();
            for s in slots.iter() {
                if d.paths[*s].trim().is_empty() {
                    if let Some(p) = it.next() {
                        d.paths[*s] = p.display().to_string();
                    }
                }
            }
            return;
        }
        if dropped.len() >= 2 {
            // More than three are refused with a message rather than some silently left out.
            self.open(OpenRequest {
                paths: dropped,
                ..Default::default()
            });
        } else {
            self.open(OpenRequest {
                paths: dropped,
                ..Default::default()
            });
        }
    }
}

impl eframe::App for OxideMergeApp {
    /// A report still being written is finished before the program ends, which would cut it.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some((_, rx)) = self.report_job.take() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(30));
        }
        if self.cfg != self.saved_cfg {
            self.merge_saved_workspaces();
            let _ = self.stored_cfg().save();
        }
    }

    /// Takes Ctrl+Tab before egui sees it: egui moves the keyboard focus on Tab whatever the
    /// modifiers, which would leave a button focused and the view's keys off.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        raw.events.retain(|e| match e {
            egui::Event::Key {
                key: egui::Key::Tab,
                pressed,
                modifiers,
                ..
            } if modifiers.ctrl => {
                if *pressed {
                    self.tab_keys.push(if modifiers.shift {
                        Cmd::PrevTab
                    } else {
                        Cmd::NextTab
                    });
                }
                false
            }
            _ => true,
        });
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        self.cfg.dark_mode = match self.cfg.theme {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => ctx
                .system_theme()
                .map_or(self.cfg.dark_mode, |t| t == egui::Theme::Dark),
        };
        let look = (self.cfg.dark_mode, theme::Look::of(&self.cfg));
        if self.visuals_dark != Some(look) {
            self.visuals_dark = Some(look);
            theme::apply(ctx, look.0, look.1);
        }
        if self.applied_lang != self.cfg.language {
            i18n::set_language(&self.cfg.language);
            self.applied_lang = self.cfg.language.clone();
        }
        if self.font_lang != i18n::current_language() {
            self.font_lang = i18n::current_language().to_string();
            fonts::setup(ctx, &self.font_lang);
        }

        self.pass_on();
        let unsaved = self.tabs.iter().any(|t| t.is_modified());
        #[cfg(target_os = "macos")]
        crate::ui::macos::set_unsaved(unsaved && !self.allow_close);
        // Ending the program part way would leave a synchronization to recover next time.
        let syncing = self.sync_recovery.as_ref().is_some_and(|r| r.busy())
            || self
                .run_job
                .as_ref()
                .is_some_and(|r| r.is_running() && r.changes_folders())
            || self
                .tabs
                .iter()
                .any(|t| matches!(t.content.view(), Content::Folder(f) if f.syncing()));
        if ctx.input(|i| i.viewport().close_requested()) && syncing {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_queue.clear();
            self.allow_close = false;
            self.notify(tr("folder_sync.wait_to_close"));
        } else if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && unsaved {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_queue.clear();
            self.prompt = Some(Prompt::Exit);
        }

        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            // Toasts that appeared while away start their time when the user is back.
            for t in &mut self.toasts {
                t.shown = None;
            }
            for t in &mut self.tabs {
                if let Content::Text(v) = &mut t.content {
                    v.check_disk();
                }
            }
        }
        self.was_focused = focused;

        self.handle_drops(ctx);
        let modal = self.prompt.is_some()
            || self.sync_recovery.is_some()
            || self.open_dlg.is_some()
            || self.options_dlg.is_some()
            || self.patch_dlg.is_some()
            || self.stats_dlg.is_some()
            || self.git_dlg.is_some()
            || self.rule_dlg.is_some()
            || self.save_job.is_some()
            || self.run_job.is_some()
            || self.rev_load.is_some()
            || self.remote_dlg.is_some()
            || self.remote_load.is_some()
            || self.host_prompt.is_some()
            || self.ask.is_some()
            || self.archive_password.is_some()
            || self.repack.is_some()
            || self.upload.is_some()
            || self.reload_prompt.is_some()
            || self.reload.is_some()
            || self.workspace_save_dlg.is_some()
            || self.manage_workspaces
            || self.help
            || self.about
            || self.palette.is_some()
            || matches!(
                self.tabs.get(self.active).map(|t| t.content.view()),
                Some(Content::Folder(v)) if v.has_dialog()
            );
        widgets::set_modal_open(ctx, modal);
        if modal {
            self.tab_keys.clear();
        } else {
            self.handle_shortcuts(ctx);
        }

        let p = theme::palette(self.cfg.dark_mode);
        let chrome = egui::Frame::new().fill(p.chrome);
        egui::Panel::top("menu")
            .frame(chrome.inner_margin(Margin {
                left: 10,
                right: 8,
                top: 5,
                bottom: 3,
            }))
            .show_separator_line(false)
            .show(root, |ui| self.menu_bar(ui, ctx));
        if !self.tabs.is_empty() {
            egui::Panel::top("tabs")
                .frame(chrome.inner_margin(Margin {
                    left: 8,
                    right: 8,
                    top: 2,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(root, |ui| self.tab_bar(ui));
        }
        egui::Panel::bottom("status")
            .frame(chrome.inner_margin(Margin::symmetric(theme::space::LG, theme::space::XXS)))
            .show(root, |ui| self.status_bar(ui));

        let colors = theme::colors(self.cfg.dark_mode);
        let mut folder_action: Option<FolderAction> = None;
        let content_fill = if self.tabs.is_empty() {
            p.chrome
        } else {
            p.surface
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(content_fill))
            .show(root, |ui| {
                let cfg = self.cfg.clone();
                let named = self.named_filters.clone();
                if !self.tabs.is_empty() {
                    ui.spacing_mut().item_spacing.y = 0.0;
                }
                match self.tabs.get_mut(self.active).map(|t| &mut t.content) {
                    None => self.start_page(ui),
                    Some(c) => folder_action = content_ui(c, ui, &cfg, &colors, &named),
                }
            });
        if let Some(Content::Web(w)) = self.tabs.get(self.active).map(|t| &t.content) {
            self.cfg.web_mode = w.mode;
        }
        self.sync_text_layout();
        crate::core::fsutil::set_backup(self.cfg.backup_on_save);
        self.announce_finished(ctx);
        self.take_step_file();
        self.sync_diffs_selection();
        self.refresh_saved_folders();
        if let Some(a) = folder_action {
            match a {
                FolderAction::OpenCaptured { comparison } => self.open_captured(comparison),
                FolderAction::OpenFiles {
                    paths,
                    missing,
                    read_only,
                    descriptions,
                    kind,
                    rel,
                    rules,
                    snapshot_options,
                } => {
                    let folder = self.tabs.get(self.active).map(|t| match &t.content {
                        Content::Diffs(v) => v.folder,
                        _ => t.id,
                    });
                    let temps = self.tabs.get(self.active).map(Tab::shown_temps);
                    let (origin_roots, origin_archives) = folder
                        .and_then(|id| self.tabs.iter().find(|t| t.id == id))
                        .and_then(|t| match t.content.view() {
                            Content::Folder(v) => Some((
                                v.roots.clone(),
                                (0..v.n())
                                    .map(|s| v.archive_source(s).map(Path::to_path_buf))
                                    .collect(),
                            )),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let count = self.tabs.len();
                    self.open(OpenRequest {
                        paths,
                        missing,
                        read_only,
                        descriptions,
                        kind,
                        skip_recent: true,
                        rules: Some(rules),
                        snapshot_options,
                        ..Default::default()
                    });
                    if let (Some(folder), true) = (folder, self.tabs.len() > count) {
                        let review = self
                            .tabs
                            .iter()
                            .find(|t| t.id == folder)
                            .and_then(|t| match &t.content {
                                Content::Folder(v) => v.workspace.entries.get(&rel).cloned(),
                                _ => None,
                            })
                            .unwrap_or_default();
                        if let Some(t) = self.tabs.last_mut() {
                            if let Content::Text(view) = t.content.view_mut() {
                                view.review_decision = Some(Box::new(review));
                                view.restore_review_cursor();
                            }
                            t.origin = Some(Origin {
                                folder,
                                rel,
                                roots: origin_roots,
                                archives: origin_archives,
                            });
                            t.temps = temps.unwrap_or_default();
                        }
                    }
                }
                FolderAction::OpenFolders { paths } => {
                    self.open(OpenRequest {
                        paths,
                        skip_recent: true,
                        ..Default::default()
                    });
                }
                FolderAction::FilterChanged(spec) => {
                    self.cfg.add_recent_filter(&spec);
                    self.cfg.filter = spec;
                }
                FolderAction::OptionsChanged { method, recursive } => {
                    self.cfg.folder.method = method;
                    self.cfg.folder.recursive = recursive;
                }
                FolderAction::FilePane(on) => self.cfg.folder_file_pane = on,
                FolderAction::ShowAll => {
                    self.cfg.show_identical = true;
                    self.cfg.show_different = true;
                    self.cfg.show_unique = [true; 3];
                }
                FolderAction::Show {
                    identical,
                    different,
                    unique,
                } => {
                    self.cfg.show_identical = identical;
                    self.cfg.show_different = different;
                    self.cfg.show_unique = unique;
                }
                FolderAction::AddRule(draft) => self.open_rule_dialog(self.active, draft),
                FolderAction::EditRules => self.open_options(OptTab::Rules),
                FolderAction::HtmlReport => self.save_html_report(),
                FolderAction::AllDiffs => self.show_all_diffs(),
                FolderAction::SaveReview(selected_only) => self.save_folder_review(selected_only),
                FolderAction::ExportFileReport { name, job } => {
                    self.write_html_report(tr("menu.export_html"), &name, job);
                }
            }
        }
        let (request, export) = match self.tabs.get_mut(self.active).map(|t| t.content.view_mut()) {
            Some(Content::Text(v)) => {
                (v.rule_request.take(), std::mem::take(&mut v.export_request))
            }
            _ => (None, false),
        };
        if let Some(draft) = request {
            self.open_rule_dialog(self.active, draft);
        }
        if export {
            self.save_html_report();
        }

        self.palette_ui(ctx);
        self.open_dialog(ctx);
        self.options_dialog(ctx);
        self.filters_dialog(ctx);
        self.patch_dialog(ctx);
        self.rule_dialog(ctx);
        self.stats_dialog(ctx);
        self.save_job_dialog(ctx);
        self.run_job_dialog(ctx);
        self.rev_load_dialog(ctx);
        self.remote_connect_dialog(ctx);
        self.ask_dialog(ctx);
        self.archive_password_dialog(ctx);
        self.remote_load_dialog(ctx);
        self.host_prompt_dialog(ctx);
        self.repack_dialog(ctx);
        self.upload_dialog(ctx);
        self.reload_prompt_dialog(ctx);
        self.reload_dialog(ctx);
        self.gc_pending_remote();
        self.workspace_save_dialog(ctx);
        self.manage_workspaces_dialog(ctx);
        self.poll_clipboard(ctx);
        self.git_dialog(ctx);
        if let Some(r) = &mut self.sync_recovery {
            if !r.show(ctx) {
                let changed = r.changed;
                self.sync_recovery = None;
                if changed {
                    let (cfg, named) = (self.cfg.clone(), self.named_filters.clone());
                    for t in &mut self.tabs {
                        if let Content::Folder(f) = t.content.view_mut() {
                            f.refresh(&cfg, &named);
                        }
                    }
                }
            }
        }
        self.poll_report(ctx);
        self.about_window(ctx);
        self.prompts(ctx);
        let mut bulk = Vec::new();
        for t in &mut self.tabs {
            let msg = match t.content.view_mut() {
                Content::Text(v) => v.bulk_edit.take(),
                Content::Binary(v) => {
                    if let Some(on) = v.moved_choice.take() {
                        self.cfg.binary_moved_blocks = on;
                    }
                    v.bulk_edit.take()
                }
                _ => None,
            };
            if let Some(m) = msg {
                bulk.push((m, t.id));
            }
        }
        for (m, id) in bulk {
            self.push_toast(m, false, Some(ToastUndo::Tab(id)));
        }
        self.toasts_ui(ctx);
        drop_overlay(ctx, self.open_dlg.is_some());
        self.save_session();
        if self.pass_on.is_some() {
            ctx.request_repaint();
        }

        // Settings that could not be read are left alone, as the message at startup said.
        let refused = matches!(crate::config::unreadable(), Some(Err(_)));
        let now = ctx.input(|i| i.time);
        if self.cfg != self.saved_cfg && !refused && self.settings_retry.is_none_or(|t| now >= t) {
            self.merge_saved_workspaces();
            match self.stored_cfg().save() {
                Ok(()) => {
                    self.saved_cfg = self.cfg.clone();
                    self.settings_retry = None;
                }
                Err(e) => {
                    // Said once, then tried again now and then until it works.
                    if self.settings_retry.is_none() {
                        let msg = trf("msg.settings_not_saved", &[("err", format!("{e:#}"))]);
                        self.push_toast(msg, true, None);
                    }
                    self.settings_retry = Some(now + 5.0);
                    ctx.request_repaint_after(std::time::Duration::from_secs(5));
                }
            }
        }
        self.smoke_report(ctx);
        self.take_screenshot(ctx);
    }
}

impl OxideMergeApp {
    /// For automated checks of a real window: after a few frames, and once compares are done
    /// (or after about a minute), writes what the window shows and closes it.
    fn smoke_report(&mut self, ctx: &egui::Context) {
        let busy = self.busy();
        let Some((path, frames)) = &mut self.smoke else {
            return;
        };
        *frames += 1;
        ctx.request_repaint();
        let frames = *frames;
        if frames < 10 || (busy && frames < 3600) {
            return;
        }
        let path = path.clone();
        self.smoke = None;
        let mut out = format!("frames {frames}\nbusy {busy}\n");
        for (i, t) in self.tabs.iter().enumerate() {
            let outcome = t.content.outcome();
            let mark = if i == self.active { "*" } else { "" };
            out.push_str(&format!(
                "tab{mark} {} | {outcome:?} | {}\n",
                t.title(),
                t.status_line()
            ));
        }
        for t in &self.toasts {
            let kind = if t.error { "error" } else { "note" };
            out.push_str(&format!("{kind} {}\n", t.text.replace('\n', " / ")));
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("{}: {e}", path.display());
        }
        // An automated check never waits for an answer about unsaved files.
        self.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn busy(&self) -> bool {
        self.tabs
            .iter()
            .any(|t| matches!(t.content.outcome(), Some(Outcome::Busy)))
    }

    /// For documentation pictures: once compares are done (or after about a minute), runs the
    /// asked-for command, lets the window settle, saves a picture of it and closes it.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        // A command such as synchronizing opens a dialog that works out what it shows in the
        // background; the picture is of what it shows once it has.
        let busy = self.busy()
            || self
                .tabs
                .iter()
                .any(|t| matches!(t.content.view(), Content::Folder(f) if f.sync_settling()));
        let Some(shot) = &mut self.screenshot else {
            return;
        };
        shot.frames += 1;
        ctx.request_repaint();
        if let Some(saved) = &shot.taken {
            let saved = saved.load(std::sync::atomic::Ordering::Acquire);
            if saved || shot.frames > 600 {
                if !saved {
                    eprintln!("{}: the window gave no picture", shot.path.display());
                    SCREENSHOT_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.screenshot = None;
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if shot.frames < 10 || (busy && shot.frames < 3600) {
            return;
        }
        if let Some(cmd) = shot.command.take() {
            // Counted again from here, so what the command opens is drawn and done first.
            shot.frames = 0;
            self.run(cmd, ctx);
            return;
        }
        let saved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        shot.taken = Some(saved.clone());
        // Counted again from here, so the time allowed for saving is not spent waiting.
        shot.frames = 0;
        let path = shot.path.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
            egui::ScreenshotCallback::new(move |img| {
                let [w, h] = img.size;
                let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                let result = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                    .map(|i| i.save(&path).map_err(|e| e.to_string()))
                    .unwrap_or_else(|| Err("bad image size".into()));
                if let Err(e) = result {
                    eprintln!("{}: {e}", path.display());
                    SCREENSHOT_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                saved.store(true, std::sync::atomic::Ordering::Release);
            }),
        ));
    }
}

#[cfg(test)]
#[path = "gui_tests.rs"]
mod gui_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_name_a_shared_file_once_with_the_folders_that_differ() {
        let src = |a: &str, b: &str| vec![PathBuf::from(a), PathBuf::from(b)];
        let native = |p: &str| {
            Path::new(p)
                .components()
                .collect::<PathBuf>()
                .display()
                .to_string()
        };
        let sub = |a: &str, b: &str| native(&format!("{a}/{b}"));
        assert_eq!(
            tab_label(
                "main.rs* ↔ main.rs",
                &src("/w/left/src/main.rs", "/w/right/src/main.rs")
            ),
            (
                "main.rs".into(),
                Some(format!("{} ↔ {}", sub("left", "src"), sub("right", "src")))
            )
        );
        // A file compared with itself, and a relative path next to an absolute one.
        assert_eq!(
            tab_label("a.txt ↔ a.txt", &src("/w/a.txt", "/w/a.txt")),
            ("a.txt ↔ a.txt".into(), None)
        );
        let root = native("/w");
        assert_eq!(
            tab_label("a.txt ↔ a.txt", &src("a.txt", "/w/a.txt")),
            ("a.txt".into(), Some(format!(". ↔ {root}")))
        );
        assert_eq!(
            tab_label("a.txt ↔ a.txt", &src("/w/a.txt", "/w/old/a.txt")),
            ("a.txt".into(), Some(". ↔ old".into()))
        );
        assert_eq!(
            tab_label("a.txt ↔ b.txt", &src("/w/a.txt", "/w/b.txt")),
            ("a.txt ↔ b.txt".into(), None)
        );
    }

    #[test]
    fn options_of_the_command_line_are_not_kept_unless_changed() {
        use crate::core::diff::{DiffOptions, WhitespaceMode};
        let loaded = DiffOptions::default();
        let cli = DiffOptions {
            whitespace: WhitespaceMode::IgnoreAll,
            ..loaded.clone()
        };
        let now = DiffOptions {
            ignore_case: true,
            ..cli.clone()
        };
        let kept = unchanged_as_loaded(&now, &cli, &loaded);
        assert_eq!(kept.whitespace, loaded.whitespace);
        assert!(kept.ignore_case);
        let changed = DiffOptions {
            whitespace: WhitespaceMode::IgnoreChange,
            ..cli.clone()
        };
        let kept = unchanged_as_loaded(&changed, &cli, &loaded);
        assert_eq!(kept.whitespace, WhitespaceMode::IgnoreChange);
    }

    #[test]
    fn untouched_arrow_lines_keep_their_pairs() {
        let old = vec![
            (r"a\) => a".to_string(), "b) => b".to_string()),
            ("x".to_string(), "y".to_string()),
        ];
        let text = arrow_pairs(&old);
        assert_eq!(edited_pairs(&text, &old), old);
        assert!(unclear_arrow_lines(&text, &old, true).is_empty());
        let edited = format!("{text}\nv1 => v\nno arrow\np => q => r");
        let pairs = edited_pairs(&edited, &old);
        assert_eq!(pairs[..2], old[..]);
        assert_eq!(pairs[2], ("v1".to_string(), "v".to_string()));
        assert_eq!(unclear_arrow_lines(&edited, &old, true), [4, 5]);
    }

    #[test]
    fn arrows_without_spaces_and_empty_sides() {
        assert_eq!(
            parse_arrow_pairs("\\d+ =>\na=>b\nx =>y"),
            [
                (r"\d+".to_string(), String::new()),
                ("a".to_string(), "b".to_string()),
                ("x".to_string(), "y".to_string()),
            ]
        );
        let text = "start =>\n=> end\nfind =>";
        assert_eq!(unclear_arrow_lines(text, &[], true), [1, 2, 3]);
        assert_eq!(unclear_arrow_lines(text, &[], false), [2]);
    }
}
