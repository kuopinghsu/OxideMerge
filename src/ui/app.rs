//! Main application window: menus, tabs, dialogs and command dispatch.

use crate::config::{Config, SessionTab, ThemeMode};
use crate::core::archive;
use crate::core::conflict;
use crate::core::diff::{DiffAlgorithm, InlineMode, WhitespaceMode};
use crate::core::filter::{self, FltFilter, FLT_PREFIX};
use crate::core::folder::CompareMethod;
use crate::core::git;
use crate::core::patch::{self, PatchStyle};
use crate::core::project::{self, Project};
use crate::core::report::{side_name, ReportFormat};
use crate::core::saved::{self, SavedCompare};
use crate::core::text::{self, FileKind, TextDoc};
use crate::core::unpacker::{self, Unpacker};
use crate::core::web;
use crate::i18n::{self, tr, trf};
use crate::ui::binary_view::BinaryView;
use crate::ui::folder_view::{FolderAction, FolderView};
use crate::ui::image_view::ImageView;
use crate::ui::palette::{Entry as PaletteEntry, Outcome as PaletteOutcome, Palette};
use crate::ui::text_view::{TextView, REPLACE_KEY};
use crate::ui::web_view::WebView;
use crate::ui::widgets::{self, pal, Icon, Outcome};
use crate::ui::{about, fonts, help, theme};
use egui::{
    vec2, Align, Align2, CornerRadius, FontId, Key, KeyboardShortcut, Layout, Margin, Modifiers,
    Sense, Stroke,
};
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
}

pub enum Content {
    Folder(FolderView),
    Text(TextView),
    Binary(BinaryView),
    Image(ImageView),
    Web(WebView),
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
        }
    }

    fn outcome(&self) -> Option<Outcome> {
        match self {
            Content::Folder(v) => Some(v.outcome()),
            Content::Text(v) => Some(v.outcome()),
            Content::Binary(v) => Some(v.outcome()),
            Content::Image(v) => Some(v.outcome()),
            Content::Web(v) => v.outcome(v.inner.as_deref().and_then(Content::outcome)),
        }
    }
}

pub struct Tab {
    /// Stays the same while the tab lives, wherever it moves in the tab strip.
    pub id: u64,
    pub content: Content,
    /// Paths used to open this tab (for "compare as" re-opening).
    pub source: Vec<PathBuf>,
    /// The folder compare this tab was opened from, for moving on to its next or previous file.
    origin: Option<Origin>,
    /// Temporary copy of a side (a Git revision), deleted with the tab.
    _temp: Option<tempfile::TempDir>,
}

#[derive(Clone)]
struct Origin {
    /// Id of the folder compare tab.
    folder: u64,
    /// Relative path of the item in that folder compare.
    rel: String,
}

impl Tab {
    fn title(&self) -> String {
        match &self.content {
            Content::Folder(v) => v.title(),
            Content::Text(v) => v.title(),
            Content::Binary(v) => v.title(),
            Content::Image(v) => v.title(),
            Content::Web(v) => v.title(),
        }
    }

    fn icon(&self) -> Icon {
        match &self.content {
            Content::Folder(_) => Icon::Folder,
            Content::Text(_) => Icon::Text,
            Content::Binary(_) => Icon::Binary,
            Content::Image(_) => Icon::Image,
            Content::Web(_) => Icon::Globe,
        }
    }

    fn is_modified(&self) -> bool {
        match &self.content {
            Content::Folder(_) | Content::Web(_) => false,
            Content::Text(v) => v.is_modified(),
            Content::Binary(v) => v.is_modified(),
            Content::Image(v) => v.is_modified(),
        }
    }

    fn status_line(&self) -> String {
        self.content.status_line()
    }

    fn save(&mut self) -> anyhow::Result<()> {
        match &mut self.content {
            Content::Folder(_) | Content::Web(_) => Ok(()),
            Content::Text(v) => v.save_modified(),
            Content::Binary(v) => v.save(),
            Content::Image(v) => v.save(),
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
            v.set_options(&cfg.diff);
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
        Content::Web(v) => {
            v.header(ui, cfg, named);
            v.inner
                .as_deref_mut()
                .and_then(|c| content_ui(c, ui, cfg, colors, named))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    New,
    Open,
    OpenConflict,
    OpenProject,
    SaveProject,
    ApplyPatch,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,
    /// Three-way: take the left then the right lines (true), or right then left.
    TakeBoth(bool),
    Save,
    SaveAll,
    SaveAs(usize),
    CloseTab,
    CloseTabAt(usize),
    CloseOtherTabs(usize),
    CloseTabsToRight(usize),
    CloseAllTabs,
    Exit,
    Undo,
    Redo,
    Find,
    Replace,
    FindNext,
    FindPrev,
    GotoLine,
    Options,
    Help,
    NextDiff,
    PrevDiff,
    FirstDiff,
    LastDiff,
    NextConflict,
    PrevConflict,
    CopyRight,
    CopyLeft,
    CopyAllRight,
    CopyAllLeft,
    AutoMerge,
    Refresh,
    Patch,
    Report,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NextTab,
    PrevTab,
    CompareAs(FileKind),
    Filters,
    ToggleCollapse,
    ToggleWrap,
    Palette,
    Statistics,
    /// Indent (false) or outdent (true) the selected lines.
    Indent(bool),
    BookmarkComment,
    SaveComparison,
    GitCompare,
    ReportWithDiffs,
    SelectPattern,
}

/// What a command palette entry does.
#[derive(Clone)]
enum PaletteAction {
    Cmd(Cmd),
    Compare(Vec<String>),
    Settings(OptTab),
    Theme(ThemeMode),
    Toggle(ConfigFlag),
}

/// Selects one on/off setting.
type ConfigFlag = fn(&mut Config) -> &mut bool;

/// Menu commands offered by the command palette: label key, shortcut, command, icon.
const PALETTE_CMDS: &[(&str, &str, Cmd, Icon)] = &[
    ("menu.new", "Ctrl+N", Cmd::New, Icon::Plus),
    ("menu.open", "Ctrl+O", Cmd::Open, Icon::Folder),
    ("menu.open_conflict", "", Cmd::OpenConflict, Icon::Warning),
    ("menu.open_project", "", Cmd::OpenProject, Icon::Folder),
    ("menu.save_project", "", Cmd::SaveProject, Icon::File),
    ("menu.save", "Ctrl+S", Cmd::Save, Icon::File),
    ("btn.save_all", "Ctrl+Shift+S", Cmd::SaveAll, Icon::File),
    ("menu.patch", "", Cmd::Patch, Icon::Text),
    ("menu.apply_patch", "", Cmd::ApplyPatch, Icon::Text),
    ("menu.report", "", Cmd::Report, Icon::Text),
    ("menu.report_diffs", "", Cmd::ReportWithDiffs, Icon::Text),
    ("menu.save_comparison", "", Cmd::SaveComparison, Icon::File),
    ("menu.git_compare", "", Cmd::GitCompare, Icon::Compare),
    ("menu.statistics", "", Cmd::Statistics, Icon::Method),
    ("cmd.indent", "Ctrl+]", Cmd::Indent(false), Icon::Next),
    ("cmd.outdent", "Ctrl+[", Cmd::Indent(true), Icon::Prev),
    ("cmd.bookmark_comment", "", Cmd::BookmarkComment, Icon::Text),
    ("cmd.select_pattern", "", Cmd::SelectPattern, Icon::Check),
    (
        "cmd.toggle_bookmark",
        "Ctrl+F2",
        Cmd::ToggleBookmark,
        Icon::Check,
    ),
    ("cmd.next_bookmark", "F4", Cmd::NextBookmark, Icon::Next),
    (
        "cmd.prev_bookmark",
        "Shift+F4",
        Cmd::PrevBookmark,
        Icon::Prev,
    ),
    ("cmd.clear_bookmarks", "", Cmd::ClearBookmarks, Icon::Close),
    (
        "cmd.take_left_right",
        "",
        Cmd::TakeBoth(true),
        Icon::Compare,
    ),
    (
        "cmd.take_right_left",
        "",
        Cmd::TakeBoth(false),
        Icon::Compare,
    ),
    ("menu.close_tab", "Ctrl+W", Cmd::CloseTab, Icon::Close),
    ("cmd.undo", "Ctrl+Z", Cmd::Undo, Icon::Undo),
    ("cmd.redo", "Ctrl+Y", Cmd::Redo, Icon::Redo),
    ("cmd.find", "Ctrl+F", Cmd::Find, Icon::Search),
    ("cmd.replace", REPLACE_KEY, Cmd::Replace, Icon::Search),
    ("cmd.find_next", "F3", Cmd::FindNext, Icon::Next),
    ("cmd.find_prev", "Shift+F3", Cmd::FindPrev, Icon::Prev),
    ("dlg.goto", "Ctrl+G", Cmd::GotoLine, Icon::Method),
    ("cmd.next_diff", "Alt+Down", Cmd::NextDiff, Icon::Next),
    ("cmd.prev_diff", "Alt+Up", Cmd::PrevDiff, Icon::Prev),
    ("cmd.first_diff", "Alt+Home", Cmd::FirstDiff, Icon::First),
    ("cmd.last_diff", "Alt+End", Cmd::LastDiff, Icon::Last),
    (
        "cmd.next_conflict",
        "Alt+PgDn",
        Cmd::NextConflict,
        Icon::NextConflict,
    ),
    (
        "cmd.prev_conflict",
        "Alt+PgUp",
        Cmd::PrevConflict,
        Icon::PrevConflict,
    ),
    (
        "cmd.copy_right",
        "Alt+Right",
        Cmd::CopyRight,
        Icon::CopyRight,
    ),
    ("cmd.copy_left", "Alt+Left", Cmd::CopyLeft, Icon::CopyLeft),
    (
        "cmd.copy_all_right",
        "Ctrl+Alt+Right",
        Cmd::CopyAllRight,
        Icon::CopyAllRight,
    ),
    (
        "cmd.copy_all_left",
        "Ctrl+Alt+Left",
        Cmd::CopyAllLeft,
        Icon::CopyAllLeft,
    ),
    ("cmd.auto_merge", "", Cmd::AutoMerge, Icon::Bolt),
    (
        "cmd.collapse_unchanged",
        "Alt+C",
        Cmd::ToggleCollapse,
        Icon::Fold,
    ),
    ("cmd.word_wrap", "Alt+Z", Cmd::ToggleWrap, Icon::Wrap),
    ("cmd.refresh", "F5", Cmd::Refresh, Icon::Refresh),
    ("cmd.zoom_in", "Ctrl++", Cmd::ZoomIn, Icon::ZoomIn),
    ("cmd.zoom_out", "Ctrl+-", Cmd::ZoomOut, Icon::ZoomOut),
    ("cmd.zoom_reset", "Ctrl+0", Cmd::ZoomReset, Icon::Fit),
    ("menu.filters", "", Cmd::Filters, Icon::Filter),
    ("menu.options", "", Cmd::Options, Icon::Settings),
    ("help.title", "F1", Cmd::Help, Icon::Help),
    ("menu.exit", "Alt+F4", Cmd::Exit, Icon::Close),
];

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
    Folder,
    Media,
    Plugins,
}

impl OptTab {
    const ALL: [(OptTab, Icon, &'static str); 5] = [
        (OptTab::General, Icon::Settings, "opt.tab_general"),
        (OptTab::Compare, Icon::Compare, "opt.tab_compare"),
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
    (OptTab::General, "view.line_numbers"),
    (OptTab::General, "view.location_pane"),
    (OptTab::General, "view.whitespace"),
    (OptTab::General, "view.syntax"),
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
    (OptTab::Compare, "opt.line_filters"),
    (OptTab::Compare, "opt.substitutions"),
    (OptTab::Compare, "opt.numeric_tolerance"),
    (OptTab::Compare, "opt.block_filters"),
    (OptTab::Folder, "label.method"),
    (OptTab::Folder, "label.recursive"),
    (OptTab::Folder, "opt.unique_subfolders"),
    (OptTab::Folder, "opt.skip_hidden"),
    (OptTab::Folder, "opt.time_tolerance"),
    (OptTab::Folder, "opt.threads"),
    (OptTab::Media, "img.threshold"),
    (OptTab::Media, "img.block_size"),
    (OptTab::Media, "opt.bytes_per_line"),
    (OptTab::Media, "opt.web"),
    (OptTab::Media, "opt.web_browser"),
    (OptTab::Media, "opt.web_width"),
    (OptTab::Media, "opt.web_height"),
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

fn parse_arrow_pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            l.split_once("=>")
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
        })
        .filter(|(a, _)| !a.is_empty())
        .collect()
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
    line_filters: String,
    substitutions: String,
    block_filters: String,
}

/// Line or item counts of the active compare.
struct StatsDialog {
    columns: Vec<String>,
    rows: Vec<(String, Vec<String>)>,
}

type CommitsRx = std::sync::mpsc::Receiver<Vec<(String, String)>>;

struct GitDialog {
    path: String,
    rev: String,
    /// Recent commits of `commits_for`: short hash and subject.
    commits: Vec<(String, String)>,
    commits_for: String,
    /// Recent commits being read with `git log`, for the path it was started for.
    commits_job: Option<(String, CommitsRx)>,
}

struct PatchDialog {
    style: PatchStyle,
    context: usize,
    path: String,
}

#[derive(Clone, Copy)]
enum Prompt {
    CloseTab(usize),
    /// Replace the tab with the next (true) or previous (false) file of its folder compare.
    SwitchFile(usize, bool),
    Exit,
}

/// How long an information toast stays up; error toasts stay until they are closed.
const TOAST_SECS: f64 = 4.0;
/// Toasts offering to undo an edit stay longer, to leave time to reach the button.
/// Folder compares that take at least this long announce when they finish unattended.
const LONG_COMPARE: std::time::Duration = std::time::Duration::from_secs(5);
const UNDO_TOAST_SECS: f64 = 8.0;

struct Toast {
    text: String,
    error: bool,
    /// When the toast appeared, or when the pointer last left it, in `egui::InputState::time`.
    shown: Option<f64>,
    /// Tab whose last edit the toast offers to undo.
    undo_tab: Option<u64>,
}

pub struct OxideMergeApp {
    cfg: Config,
    saved_cfg: Config,
    tabs: Vec<Tab>,
    active: usize,
    open_dlg: Option<OpenDialog>,
    options_dlg: Option<OptionsDialog>,
    patch_dlg: Option<PatchDialog>,
    stats_dlg: Option<StatsDialog>,
    git_dlg: Option<GitDialog>,
    /// A folder report with file differences being written on another thread: its file.
    report_job: Option<(PathBuf, std::sync::mpsc::Receiver<std::io::Result<()>>)>,
    filters_dlg: bool,
    about: bool,
    help: bool,
    named_filters: Vec<FltFilter>,
    prompt: Option<Prompt>,
    toasts: Vec<Toast>,
    font_lang: String,
    applied_lang: String,
    visuals_dark: Option<bool>,
    allow_close: bool,
    /// Tab last scrolled into view in the tab strip.
    shown_active: Option<usize>,
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

/// What opening the project file `path` compares.
fn project_request(path: &Path) -> anyhow::Result<OpenRequest> {
    let p = Project::load(path)?;
    Ok(OpenRequest {
        paths: p.resolved_paths(path),
        descriptions: p.descriptions.clone(),
        read_only: p.read_only.clone(),
        kind: p.kind.as_deref().and_then(kind_from_name),
        recursive: p.recursive,
        filter: p.filter.clone(),
        method: p.method,
        ..Default::default()
    })
}

/// An empty editable document for File → New.
fn blank_doc() -> TextDoc {
    let mut d = TextDoc::empty(None);
    d.lines.push(String::new());
    d.final_newline = false;
    d
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

/// Large clickable card on the start page.
fn action_card(ui: &mut egui::Ui, w: f32, icon: Icon, title: &str, desc: &str) -> bool {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 120.0), Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();
    painter.rect(
        rect,
        12.0,
        if hovered { p.surface_alt } else { p.surface },
        Stroke::new(1.0_f32, if hovered { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    widgets::focus_ring(ui, &resp, rect, 12.0);
    let painter = ui.painter();
    let icon_rect = egui::Rect::from_min_size(rect.min + vec2(18.0, 18.0), vec2(36.0, 36.0));
    painter.rect_filled(icon_rect, 10.0, p.accent_soft);
    crate::ui::icons::paint(painter, icon_rect.shrink(9.0), icon, p.accent);
    painter.text(
        egui::pos2(icon_rect.right() + 12.0, icon_rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(15.5),
        p.text,
    );
    let galley = painter.layout(
        desc.to_string(),
        FontId::proportional(13.0),
        p.text_dim,
        w - 36.0,
    );
    painter.galley(
        egui::pos2(rect.left() + 18.0, icon_rect.bottom() + 12.0),
        galley,
        p.text_dim,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Row of the start page's recent list: the compared names, with where they are below.
fn recent_row(ui: &mut egui::Ui, paths: &[String]) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, 7.0, p.control_hover);
    }
    let first = paths.first().map(String::as_str).unwrap_or_default();
    let icon = if web::is_url(first) {
        Icon::Globe
    } else if Path::new(first).is_dir() {
        Icon::Folder
    } else {
        Icon::File
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
    let name = |s: &String| -> String {
        if web::is_url(s) {
            return s.clone();
        }
        Path::new(s)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.clone())
    };
    let dir = |s: &String| -> String {
        if web::is_url(s) {
            return String::new();
        }
        Path::new(s)
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
    };
    let names: Vec<String> = paths.iter().map(name).collect();
    let mut dirs: Vec<String> = paths.iter().map(dir).collect();
    dirs.dedup();
    let clip = ui.painter().with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(rect.left() + 30.0, rect.top()),
        egui::pos2(rect.right() - 10.0, rect.bottom()),
    ));
    let x = rect.left() + 36.0;
    clip.text(
        egui::pos2(x, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        names.join("  ↔  "),
        theme::semibold(13.5),
        p.text,
    );
    clip.text(
        egui::pos2(x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        dirs.join("  ↔  "),
        FontId::proportional(12.0),
        p.text_dim,
    );
    widgets::focus_ring(ui, &resp, rect, 7.0);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, names.join(" ↔ "))
    });
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

fn path_status(ui: &egui::Ui, field: &egui::Response, text: &str) {
    let raw = text.trim().trim_matches('"');
    if raw.is_empty() {
        return;
    }
    let p = pal(ui);
    let path = Path::new(raw);
    let (icon, color, missing) = if web::is_url(raw) {
        (Icon::Globe, p.accent, false)
    } else if path.is_dir() {
        (Icon::Folder, p.success, false)
    } else if path.exists() {
        (Icon::File, p.success, false)
    } else {
        (Icon::Warning, p.danger, true)
    };
    // The response covers only the text, inside the margin.
    let r = field.rect + PATH_FIELD_MARGIN;
    let icon_rect =
        egui::Rect::from_center_size(egui::pos2(r.right() - 15.0, r.center().y), vec2(14.0, 14.0));
    crate::ui::icons::paint(ui.painter(), icon_rect, icon, color);
    if missing {
        ui.painter().rect_stroke(
            r,
            7.0,
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
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{label} — {tab}"))
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, 7.0, p.control_hover);
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
        FontId::proportional(13.5),
        p.text,
    );
    clip.text(
        egui::pos2(x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        tab,
        FontId::proportional(12.0),
        p.text_dim,
    );
    widgets::focus_ring(ui, &resp, rect, 7.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Full-window highlight while files are dragged over the window.
fn drop_overlay(ctx: &egui::Context, filling_dialog: bool) {
    let count = ctx.input(|i| i.raw.hovered_files.len());
    if count == 0 {
        return;
    }
    let p = theme::palette(ctx.style().visuals.dark_mode);
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("drop_overlay"),
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
        theme::semibold(20.0),
        p.text,
    );
    if !filling_dialog {
        painter.text(
            c + vec2(0.0, 42.0),
            Align2::CENTER_CENTER,
            tr("app.drop_overlay_desc"),
            FontId::proportional(14.0),
            p.text_dim,
        );
    }
}

fn dashed_rect(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    let pts = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    ui.painter().extend(egui::Shape::dashed_line(
        &pts,
        Stroke::new(1.2_f32, color),
        6.0,
        4.0,
    ));
}

impl OxideMergeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        initial: Option<OpenRequest>,
    ) -> Self {
        let lang = i18n::current_language().to_string();
        fonts::setup(&cc.egui_ctx, &lang);
        let applied_lang = cfg.language.clone();
        let mut app = OxideMergeApp {
            saved_cfg: cfg.clone(),
            cfg,
            tabs: Vec::new(),
            active: 0,
            open_dlg: None,
            options_dlg: None,
            patch_dlg: None,
            stats_dlg: None,
            git_dlg: None,
            report_job: None,
            filters_dlg: false,
            about: false,
            help: false,
            named_filters: filter::load_all_filters(),
            prompt: None,
            toasts: Vec::new(),
            font_lang: lang,
            applied_lang,
            visuals_dark: None,
            allow_close: false,
            shown_active: None,
            was_focused: true,
            next_tab_id: 0,
            track_session: initial.is_none(),
            palette: None,
            text_layout: (false, false),
        };
        app.text_layout = (app.cfg.collapse_unchanged, app.cfg.word_wrap);
        crate::core::fsutil::set_backup(app.cfg.backup_on_save);
        if let Some(req) = initial {
            app.open(req);
        } else if app.cfg.restore_session {
            app.restore_session();
        }
        app
    }

    /// Reopens the tabs of the previous session; those that can't be opened are left out.
    fn restore_session(&mut self) {
        let session = self.cfg.session.clone();
        let recent_paths = self.cfg.recent_paths.clone();
        let mut active = None;
        for (i, s) in session.iter().enumerate() {
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
                ..Default::default()
            };
            if self.try_open(req).is_ok()
                && self.tabs.len() > before
                && i <= self.cfg.session_active
            {
                active = Some(self.tabs.len() - 1);
            }
        }
        // a single file without conflicts opens the Open dialog instead of a tab
        self.open_dlg = None;
        self.cfg.recent_paths = recent_paths;
        if let Some(a) = active {
            self.active = a;
        }
    }

    fn save_session(&mut self) {
        if !self.track_session {
            return;
        }
        let tab = |t: &Tab| -> Option<SessionTab> {
            if t.source.is_empty() {
                return None;
            }
            let kind = match &t.content {
                Content::Folder(_) => None,
                Content::Text(_) => Some("text"),
                Content::Binary(_) => Some("binary"),
                Content::Image(_) => Some("image"),
                Content::Web(_) => Some("web"),
            };
            Some(SessionTab {
                paths: t.source.iter().map(|p| p.display().to_string()).collect(),
                kind: kind.map(str::to_string),
            })
        };
        let session: Vec<SessionTab> = if self.cfg.restore_session {
            self.tabs.iter().filter_map(tab).collect()
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

    fn error(&mut self, msg: impl Into<String>) {
        self.push_toast(msg.into(), true, None);
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.push_toast(msg.into(), false, None);
    }

    fn push_toast(&mut self, text: String, error: bool, undo_tab: Option<u64>) {
        self.toasts
            .retain(|t| t.text != text && (undo_tab.is_none() || t.undo_tab != undo_tab));
        self.toasts.push(Toast {
            text,
            error,
            shown: None,
            undo_tab,
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
            d.description = Some(format!("{} ({})", p.display(), tr("label.missing")));
            return Ok(d);
        }
        if let Some(u) = unpacker::find(&self.cfg.unpackers, p) {
            let bytes = u.run(p)?;
            let mut d = text::doc_from_bytes(Some(p.to_path_buf()), &bytes, None);
            d.read_only = true;
            d.description = Some(format!("{} [{}]", p.display(), tr("label.unpacked")));
            return Ok(d);
        }
        text::load_doc(p, None)
    }

    pub fn open(&mut self, req: OpenRequest) {
        let recent: Option<Vec<String>> = (!req.skip_recent && req.paths.len() >= 2)
            .then(|| req.paths.iter().map(|p| p.display().to_string()).collect());
        match self.try_open(req) {
            Ok(()) => {
                if let Some(r) = recent {
                    self.cfg.add_recent_compare(&r);
                }
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    fn try_open(&mut self, req: OpenRequest) -> anyhow::Result<()> {
        let mut paths = req.paths.clone();
        if paths.is_empty() {
            return Ok(());
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
            return self.open_git_revision(path, rev);
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
            self.cfg.add_recent_path(&p.display().to_string());
        }
        if req.kind == Some(FileKind::Web)
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
            let mut restart = false;
            if let Some(r) = req.recursive {
                v.set_recursive(r);
                restart = true;
            }
            if let Some(m) = req.method {
                v.set_method(m);
                restart = true;
            }
            if restart {
                v.start_compare();
            }
            app.cfg.add_recent_filter(&filter_spec);
            app.push_tab(Content::Folder(v), source.clone());
        };
        if req.kind.is_none() && paths.iter().all(|p| p.is_dir()) {
            let display = paths.iter().map(|p| p.display().to_string()).collect();
            make_folder(self, paths, display, Vec::new());
            return Ok(());
        }
        if req.kind.is_none() && paths.iter().all(|p| archive::is_archive(p)) {
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
                unpacker::find(&self.cfg.unpackers, p).is_none()
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
                for (i, p) in paths.iter().enumerate() {
                    let mut d = self.load_text(p, missing[i])?;
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
                Content::Text(TextView::new(docs, &self.cfg))
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
                Content::Image(ImageView::load(paths, &self.cfg)?)
            }
            FileKind::Web => unreachable!("web pages are opened above"),
        };
        self.push_tab(content, source);
        Ok(())
    }

    /// Compares `path` with how it was in Git revision `rev` (the left side, read-only).
    fn open_git_revision(&mut self, path: &Path, rev: &str) -> anyhow::Result<()> {
        let rev = match rev.trim() {
            "" => "HEAD",
            r => r,
        };
        let label = format!("{} @ {rev}", path.display());
        let (temp, content) = if path.is_dir() {
            let (temp, copy) = git::export_folder(path, rev)?;
            let display = vec![label, path.display().to_string()];
            let v = FolderView::new(
                vec![copy, path.to_path_buf()],
                display,
                Vec::new(),
                &self.cfg.filter,
                &self.named_filters,
                &self.cfg,
            );
            (temp, Content::Folder(v))
        } else {
            let bytes = git::show_file(path, rev)?;
            let temp = tempfile::tempdir()?;
            let copy = temp.path().join(path.file_name().unwrap_or_default());
            std::fs::write(&copy, bytes)?;
            let paths = vec![copy.clone(), path.to_path_buf()];
            let content = if text::is_image_path(path) {
                Content::Image(ImageView::load(paths, &self.cfg)?)
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
            };
            (temp, content)
        };
        self.cfg.add_recent_path(&path.display().to_string());
        self.push_tab(content, Vec::new());
        if let Some(t) = self.tabs.last_mut() {
            t._temp = Some(temp);
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

    fn push_tab(&mut self, content: Content, source: Vec<PathBuf>) {
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id: self.next_tab_id,
            content,
            source,
            origin: None,
            _temp: None,
        });
        self.active = self.tabs.len() - 1;
    }

    /// Moves on from the active tab to the next or previous file with differences in the folder
    /// compare it was opened from.
    fn step_file(&mut self, forward: bool) {
        let ti = self.active;
        let Some(Origin { folder, rel }) = self.tabs.get(ti).and_then(|t| t.origin.clone()) else {
            return;
        };
        let Some(Content::Folder(f)) = self
            .tabs
            .iter()
            .find(|t| t.id == folder)
            .map(|t| &t.content)
        else {
            return;
        };
        if f.adjacent_different_file(&self.cfg, &rel, forward)
            .is_none()
        {
            self.notify(tr(if forward {
                "msg.no_next_file"
            } else {
                "msg.no_prev_file"
            }));
        } else if self.tabs[ti].is_modified() {
            self.prompt = Some(Prompt::SwitchFile(ti, forward));
        } else {
            self.switch_file(ti, forward);
        }
    }

    fn switch_file(&mut self, ti: usize, forward: bool) {
        let Some(Origin { folder, rel }) = self.tabs.get(ti).and_then(|t| t.origin.clone()) else {
            return;
        };
        let cfg = &self.cfg;
        let Some(Content::Folder(f)) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == folder)
            .map(|t| &mut t.content)
        else {
            return;
        };
        let Some(FolderAction::OpenFiles {
            paths,
            missing,
            kind,
            rel,
        }) = f
            .adjacent_different_file(cfg, &rel, forward)
            .and_then(|i| f.reveal_and_open(i, cfg))
        else {
            return;
        };
        let count = self.tabs.len();
        self.open(OpenRequest {
            paths,
            missing,
            kind,
            skip_recent: true,
            ..Default::default()
        });
        if self.tabs.len() == count {
            return;
        }
        let Some(mut tab) = self.tabs.pop() else {
            return;
        };
        match tab.content.view_mut() {
            Content::Text(v) if forward => v.first_diff(),
            Content::Text(v) => v.last_diff(),
            Content::Binary(v) if forward => v.first_diff(),
            Content::Binary(v) => v.last_diff(),
            Content::Image(v) if forward => v.first_diff(),
            Content::Image(v) => v.last_diff(),
            _ => {}
        }
        self.notify(trf("msg.file_switched", &[("file", rel.clone())]));
        tab.origin = Some(Origin { folder, rel });
        self.tabs[ti] = tab;
        self.active = ti;
    }

    fn close_tab(&mut self, i: usize, force: bool) {
        if i >= self.tabs.len() {
            return;
        }
        if !force && self.tabs[i].is_modified() {
            self.prompt = Some(Prompt::CloseTab(i));
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Closes the tabs selected by `close`. Tabs with unsaved changes stay open and the first of
    /// them asks whether to save.
    fn close_tabs(&mut self, close: impl Fn(usize) -> bool) {
        let mut kept = Vec::new();
        let mut active = 0;
        let mut ask = None;
        for (i, t) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if close(i) {
                if !t.is_modified() {
                    continue;
                }
                ask.get_or_insert(kept.len());
            }
            if i <= self.active {
                active = kept.len();
            }
            kept.push(t);
        }
        self.tabs = kept;
        self.active = active;
        if let Some(i) = ask {
            self.active = i;
            self.prompt = Some(Prompt::CloseTab(i));
        }
    }

    // ---------- commands ----------

    fn open_options(&mut self, tab: OptTab) {
        self.options_dlg = Some(OptionsDialog {
            line_filters: self.cfg.diff.line_filters.join("\n"),
            substitutions: arrow_pairs(&self.cfg.diff.substitutions),
            block_filters: arrow_pairs(&self.cfg.diff.block_filters),
            cfg: self.cfg.clone(),
            tab,
            search: String::new(),
        });
    }

    fn palette_entries(&self) -> Vec<PaletteEntry<PaletteAction>> {
        let entry = |icon, label: String, detail: String, action| PaletteEntry {
            icon,
            label,
            detail,
            action,
        };
        let mut out: Vec<_> = PALETTE_CMDS
            .iter()
            .map(|&(key, sc, c, icon)| {
                entry(icon, plain(tr(key)), sc.to_string(), PaletteAction::Cmd(c))
            })
            .collect();
        let compare_as = plain(tr("cmd.compare_as"));
        for (k, icon) in [
            (FileKind::Text, Icon::Text),
            (FileKind::Binary, Icon::Binary),
            (FileKind::Image, Icon::Image),
            (FileKind::Web, Icon::Globe),
        ] {
            let label = format!("{compare_as}: {}", kind_label(Some(k)));
            out.push(entry(
                icon,
                label,
                String::new(),
                PaletteAction::Cmd(Cmd::CompareAs(k)),
            ));
        }
        let on = tr("palette.on").to_string();
        let toggles: [(&str, ConfigFlag); 8] = [
            ("view.line_numbers", |c| &mut c.show_line_numbers),
            ("view.location_pane", |c| &mut c.show_location_pane),
            ("view.line_detail", |c| &mut c.show_line_detail),
            ("view.edit_markers", |c| &mut c.show_edit_markers),
            ("view.folder_file_pane", |c| &mut c.folder_file_pane),
            ("view.whitespace", |c| &mut c.show_whitespace),
            ("view.syntax", |c| &mut c.syntax_highlight),
            ("view.tree_mode", |c| &mut c.tree_mode),
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
            let name = |s: &String| {
                if web::is_url(s) {
                    return s.clone();
                }
                Path::new(s)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.clone())
            };
            let label = paths.iter().map(name).collect::<Vec<_>>().join(" ↔ ");
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
        let cfg = self.cfg.clone();
        match cmd {
            Cmd::New => {
                let mut v = TextView::new(vec![blank_doc(), blank_doc()], &self.cfg);
                v.status = tr("msg.new_hint").to_string();
                self.push_tab(Content::Text(v), Vec::new());
            }
            Cmd::OpenProject => {
                if let Some(p) = rfd::FileDialog::new()
                    .set_title(tr("menu.open_project"))
                    .add_filter(
                        tr("label.project"),
                        &[project::EXT, "WinMerge", project::WINMERGE_EXT],
                    )
                    .add_filter(tr("label.saved_compare"), &[saved::EXT])
                    .pick_file()
                {
                    self.open(OpenRequest {
                        paths: vec![p],
                        ..Default::default()
                    });
                }
            }
            Cmd::SaveProject => self.save_project(),
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
            Cmd::ToggleCollapse => self.cfg.collapse_unchanged ^= true,
            Cmd::ToggleWrap => self.cfg.word_wrap ^= true,
            Cmd::Palette => {
                let entries = self.palette_entries();
                self.palette = Some((Palette::default(), entries));
            }
            Cmd::Filters => self.filters_dlg = true,
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
                let errors: Vec<String> = self
                    .tabs
                    .iter_mut()
                    .filter(|t| t.is_modified())
                    .filter_map(|t| t.save().err().map(|e| e.to_string()))
                    .collect();
                if !errors.is_empty() {
                    self.error(errors.join("\n"));
                }
            }
            Cmd::Patch => {
                self.patch_dlg = Some(PatchDialog {
                    style: self.cfg.patch_style,
                    context: self.cfg.patch_context,
                    path: String::new(),
                })
            }
            Cmd::Report => self.save_report(),
            Cmd::ReportWithDiffs => self.save_report_with_diffs(),
            Cmd::SaveComparison => self.save_comparison(),
            Cmd::Statistics => self.show_statistics(),
            Cmd::GitCompare => {
                let path = self
                    .tabs
                    .get(self.active)
                    .and_then(|t| t.source.last())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.git_dlg = Some(GitDialog {
                    path,
                    rev: "HEAD".into(),
                    commits: Vec::new(),
                    commits_for: String::new(),
                    commits_job: None,
                });
            }
            Cmd::CompareAs(kind) => {
                if let Some(tab) = self.tabs.get(self.active) {
                    if !matches!(tab.content, Content::Folder(_)) {
                        let paths = tab.source.clone();
                        self.open(OpenRequest {
                            paths,
                            kind: Some(kind),
                            ..Default::default()
                        });
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
                        Cmd::NextConflict => {
                            v.next_diff(true);
                        }
                        Cmd::PrevConflict => {
                            v.prev_diff(true);
                        }
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
                        Cmd::TakeBoth(left_first) => v.take_both(left_first),
                        Cmd::Indent(outdent) => v.indent(outdent),
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
                        Cmd::Refresh => v.refresh(&cfg, &named),
                        Cmd::Find => v.focus_search(),
                        Cmd::CopyRight => v.copy_selected(true, &cfg),
                        Cmd::CopyLeft => v.copy_selected(false, &cfg),
                        Cmd::AutoMerge => v.request_merge(),
                        Cmd::SelectPattern => v.open_select_pattern(),
                        _ => {}
                    },
                    Content::Web(_) => {}
                }
                if let Some(e) = err {
                    self.error(e);
                }
                self.take_step_file();
            }
        }
    }

    /// Acts on a view of the active tab having run past its last or first difference.
    fn take_step_file(&mut self) {
        let step = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| match t.content.view_mut() {
                Content::Text(v) => v.step_file.take(),
                Content::Binary(v) => v.step_file.take(),
                Content::Image(v) => v.step_file.take(),
                _ => None,
            });
        if let Some(forward) = step {
            self.step_file(forward);
        }
    }

    fn save_report(&mut self) {
        if let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) {
            let Some(path) = rfd::FileDialog::new()
                .set_title(tr("menu.report"))
                .add_filter("HTML", &["html", "htm"])
                .set_file_name("report.html")
                .save_file()
            else {
                return;
            };
            match std::fs::write(&path, v.html_report()) {
                Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
                Err(e) => self.error(e.to_string()),
            }
            return;
        }
        let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            self.error(tr("msg.report_folder_only"));
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title(tr("menu.report"))
            .add_filter("HTML", &["html", "htm"])
            .add_filter("CSV", &["csv"])
            .add_filter("Text", &["txt"])
            .set_file_name("report.html")
            .save_file()
        else {
            return;
        };
        let text = v.report(ReportFormat::from_path(&path), &self.cfg);
        match std::fs::write(&path, text) {
            Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
            Err(e) => self.error(e.to_string()),
        }
    }

    fn save_report_with_diffs(&mut self) {
        let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            self.error(tr("msg.report_folder_only"));
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title(tr("menu.report_diffs"))
            .add_filter("HTML", &["html", "htm"])
            .set_file_name("report.html")
            .save_file()
        else {
            return;
        };
        let job = v.report_with_diffs_job(&self.cfg);
        let (tx, rx) = std::sync::mpsc::channel();
        let file = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(std::fs::write(&file, job()));
        });
        self.report_job = Some((path, rx));
        self.notify(tr("msg.report_writing"));
    }

    /// Announces a report written on another thread once it is done.
    fn poll_report(&mut self, ctx: &egui::Context) {
        let Some((path, rx)) = &self.report_job else {
            return;
        };
        let done = match rx.try_recv() {
            Ok(r) => Some(r.map_err(|e| e.to_string())),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(tr("status.error").to_string()))
            }
        };
        let file = path.display().to_string();
        match done {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            Some(Ok(())) => {
                self.report_job = None;
                self.notify(trf("msg.saved", &[("file", file)]));
            }
            Some(Err(e)) => {
                self.report_job = None;
                self.error(format!("{file}: {e}"));
            }
        }
    }

    /// Saves the active text compare with its bookmarks and their comments as one file.
    fn save_comparison(&mut self) {
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

    fn stats_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.stats_dlg else {
            return;
        };
        let mut keep = true;
        let modal = egui::Modal::new(egui::Id::new("stats_dialog")).show(ctx, |ui| {
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
        if let Some((for_path, rx)) = &d.commits_job {
            match rx.try_recv() {
                Ok(commits) => {
                    d.commits = commits;
                    d.commits_for = for_path.clone();
                    d.commits_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => d.commits_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let started_for = d.commits_job.as_ref().map(|(p, _)| p.as_str());
        if d.commits_for != d.path && started_for != Some(d.path.as_str()) && path.exists() {
            let (tx, rx) = std::sync::mpsc::channel();
            let p = path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(git::recent_commits(&p, 20));
            });
            d.commits_job = Some((d.path.clone(), rx));
        }
        if d.commits_for != d.path {
            d.commits.clear();
        }
        let (mut keep, mut go) = (true, false);
        let modal = egui::Modal::new(egui::Id::new("git_dialog")).show(ctx, |ui| {
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
                                .hint_text("HEAD, HEAD~1, main, v1.0 …")
                                .desired_width(ui.available_width() - 36.0),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            go = true;
                        }
                        ui.add_enabled_ui(!d.commits.is_empty(), |ui| {
                            ui.menu_button("⏷", |ui| {
                                ui.set_min_width(420.0);
                                for (hash, subject) in &d.commits {
                                    let text = egui::RichText::new(format!("{hash}  {subject}"));
                                    if ui.button(text).clicked() {
                                        d.rev = hash.clone();
                                        ui.close_menu();
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tr("label.recent_commits"));
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
            if let Err(e) = self.open_git_revision(&path, &rev) {
                self.error(e.to_string());
                self.git_dlg = Some(d);
            }
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
        let mut proj = Project {
            paths: tab.source.iter().map(|p| p.display().to_string()).collect(),
            ..Default::default()
        };
        match &tab.content {
            Content::Folder(v) => {
                let (recursive, method) = v.scan_settings();
                proj.filter = Some(v.filter_spec.clone());
                proj.recursive = Some(recursive);
                proj.method = Some(method);
            }
            Content::Text(_) => proj.kind = Some(kind_name(FileKind::Text).into()),
            Content::Binary(_) => proj.kind = Some(kind_name(FileKind::Binary).into()),
            Content::Image(_) => proj.kind = Some(kind_name(FileKind::Image).into()),
            Content::Web(_) => proj.kind = Some(kind_name(FileKind::Web).into()),
        }
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
        match proj.save(&path) {
            Ok(()) => self.notify(trf("msg.saved", &[("file", path.display().to_string())])),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Applies a unified diff: every patched file opens next to its original, to be reviewed and
    /// saved.
    fn apply_patch(&mut self) {
        let Some(patch_path) = rfd::FileDialog::new()
            .set_title(tr("menu.apply_patch"))
            .add_filter(tr("label.patch_files"), &["patch", "diff"])
            .pick_file()
        else {
            return;
        };
        let parsed = std::fs::read(&patch_path)
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
            let result = patch::apply(&original.lines, f);
            let mut left = original.clone();
            left.read_only = true;
            left.description = Some(format!("{} ({})", path.display(), tr("label.original")));
            let mut right = original;
            right.lines = result.lines;
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
        let typing = ctx.wants_keyboard_input();
        let mut cmds = Vec::new();
        ctx.input_mut(|i| {
            let mut sc = |m: Modifiers, k: Key, c: Cmd| {
                if i.consume_shortcut(&KeyboardShortcut::new(m, k)) {
                    cmds.push(c);
                }
            };
            sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::P, Cmd::Palette);
            sc(Modifiers::COMMAND, Key::K, Cmd::Palette);
            sc(Modifiers::COMMAND, Key::N, Cmd::New);
            sc(Modifiers::COMMAND, Key::O, Cmd::Open);
            sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::S, Cmd::SaveAll);
            sc(Modifiers::COMMAND, Key::S, Cmd::Save);
            sc(Modifiers::COMMAND, Key::W, Cmd::CloseTab);
            sc(Modifiers::COMMAND, Key::F4, Cmd::CloseTab);
            if cfg!(target_os = "macos") {
                sc(Modifiers::COMMAND | Modifiers::ALT, Key::F, Cmd::Replace);
            } else {
                sc(Modifiers::COMMAND, Key::H, Cmd::Replace);
            }
            sc(Modifiers::COMMAND, Key::F, Cmd::Find);
            sc(Modifiers::COMMAND, Key::G, Cmd::GotoLine);
            // A pattern without Shift also matches with Shift held, so Shift variants go first.
            sc(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Tab,
                Cmd::PrevTab,
            );
            sc(Modifiers::COMMAND, Key::Tab, Cmd::NextTab);
            sc(Modifiers::COMMAND, Key::Plus, Cmd::ZoomIn);
            sc(Modifiers::COMMAND, Key::Equals, Cmd::ZoomIn);
            sc(Modifiers::COMMAND, Key::Minus, Cmd::ZoomOut);
            sc(Modifiers::COMMAND, Key::Num0, Cmd::ZoomReset);
            sc(Modifiers::SHIFT, Key::F3, Cmd::FindPrev);
            sc(Modifiers::NONE, Key::F3, Cmd::FindNext);
            sc(Modifiers::NONE, Key::F5, Cmd::Refresh);
            sc(Modifiers::NONE, Key::F1, Cmd::Help);
            if !typing {
                sc(Modifiers::NONE, Key::Questionmark, Cmd::Help);
                sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z, Cmd::Redo);
                sc(Modifiers::COMMAND, Key::Z, Cmd::Undo);
                sc(Modifiers::COMMAND, Key::Y, Cmd::Redo);
                sc(
                    Modifiers::COMMAND | Modifiers::ALT,
                    Key::ArrowRight,
                    Cmd::CopyAllRight,
                );
                sc(
                    Modifiers::COMMAND | Modifiers::ALT,
                    Key::ArrowLeft,
                    Cmd::CopyAllLeft,
                );
                sc(Modifiers::ALT, Key::ArrowDown, Cmd::NextDiff);
                sc(Modifiers::ALT, Key::ArrowUp, Cmd::PrevDiff);
                sc(Modifiers::ALT, Key::Home, Cmd::FirstDiff);
                sc(Modifiers::ALT, Key::End, Cmd::LastDiff);
                sc(Modifiers::ALT, Key::PageDown, Cmd::NextConflict);
                sc(Modifiers::ALT, Key::PageUp, Cmd::PrevConflict);
                sc(Modifiers::ALT, Key::ArrowRight, Cmd::CopyRight);
                sc(Modifiers::ALT, Key::ArrowLeft, Cmd::CopyLeft);
                sc(Modifiers::ALT, Key::C, Cmd::ToggleCollapse);
                sc(Modifiers::ALT, Key::Z, Cmd::ToggleWrap);
                sc(Modifiers::COMMAND, Key::F2, Cmd::ToggleBookmark);
                sc(Modifiers::COMMAND, Key::CloseBracket, Cmd::Indent(false));
                sc(Modifiers::COMMAND, Key::OpenBracket, Cmd::Indent(true));
                sc(Modifiers::SHIFT, Key::F4, Cmd::PrevBookmark);
                sc(Modifiers::NONE, Key::F4, Cmd::NextBookmark);
            }
        });
        for c in cmds {
            self.run(c, ctx);
        }
    }

    // ---------- menus ----------

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut cmd: Option<Cmd> = None;
        let n_panes = match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => v.n(),
            _ => 0,
        };
        let is_folder = matches!(
            self.tabs.get(self.active).map(|t| t.content.view()),
            Some(Content::Folder(_))
        );
        let has_source = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.source.len() >= 2);
        let can_take_both = matches!(
            self.tabs.get(self.active).map(|t| t.content.view()),
            Some(Content::Text(v)) if v.can_take_both()
        );
        let item =
            |ui: &mut egui::Ui, label: &str, shortcut: &str, c: Cmd, cmd: &mut Option<Cmd>| {
                if ui
                    .add(egui::Button::new(label).shortcut_text(shortcut))
                    .clicked()
                {
                    *cmd = Some(c);
                    ui.close_menu();
                }
            };
        if cfg!(target_os = "macos") {
            // The menu bar is the title bar: its empty parts move and zoom the window.
            let bar = ui.interact(
                ui.max_rect().expand2(vec2(10.0, 5.0)),
                ui.id().with("title_bar"),
                Sense::click_and_drag(),
            );
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let zoomed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!zoomed));
            }
        }
        egui::menu::bar(ui, |ui| {
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
            ui.label(egui::RichText::new("OxideMerge").font(theme::semibold(14.5)));
            ui.add_space(12.0);
            ui.menu_button(tr("menu.file"), |ui| {
                item(ui, tr("menu.new"), "Ctrl+N", Cmd::New, &mut cmd);
                item(ui, tr("menu.open"), "Ctrl+O", Cmd::Open, &mut cmd);
                item(
                    ui,
                    tr("menu.open_conflict"),
                    "",
                    Cmd::OpenConflict,
                    &mut cmd,
                );
                item(ui, tr("menu.open_project"), "", Cmd::OpenProject, &mut cmd);
                item(ui, tr("menu.git_compare"), "", Cmd::GitCompare, &mut cmd);
                if ui
                    .add_enabled(has_source, egui::Button::new(tr("menu.save_project")))
                    .clicked()
                {
                    cmd = Some(Cmd::SaveProject);
                    ui.close_menu();
                }
                ui.separator();
                item(ui, tr("menu.save"), "Ctrl+S", Cmd::Save, &mut cmd);
                item(
                    ui,
                    tr("btn.save_all"),
                    "Ctrl+Shift+S",
                    Cmd::SaveAll,
                    &mut cmd,
                );
                for p in 0..n_panes {
                    let label = trf(
                        "menu.save_side_as",
                        &[("side", side_name(p, n_panes).into())],
                    );
                    if ui.button(label).clicked() {
                        cmd = Some(Cmd::SaveAs(p));
                        ui.close_menu();
                    }
                }
                if n_panes > 0 {
                    item(
                        ui,
                        tr("menu.save_comparison"),
                        "",
                        Cmd::SaveComparison,
                        &mut cmd,
                    );
                }
                ui.separator();
                item(ui, tr("menu.patch"), "", Cmd::Patch, &mut cmd);
                if is_folder || n_panes > 0 {
                    item(ui, tr("menu.report"), "", Cmd::Report, &mut cmd);
                }
                if is_folder {
                    item(
                        ui,
                        tr("menu.report_diffs"),
                        "",
                        Cmd::ReportWithDiffs,
                        &mut cmd,
                    );
                }
                ui.separator();
                item(ui, tr("menu.close_tab"), "Ctrl+W", Cmd::CloseTab, &mut cmd);
                item(ui, tr("menu.exit"), "Alt+F4", Cmd::Exit, &mut cmd);
            });
            ui.menu_button(tr("menu.edit"), |ui| {
                item(ui, tr("cmd.undo"), "Ctrl+Z", Cmd::Undo, &mut cmd);
                item(ui, tr("cmd.redo"), "Ctrl+Y", Cmd::Redo, &mut cmd);
                ui.separator();
                item(ui, tr("cmd.find"), "Ctrl+F", Cmd::Find, &mut cmd);
                item(ui, tr("cmd.replace"), REPLACE_KEY, Cmd::Replace, &mut cmd);
                item(ui, tr("cmd.find_next"), "F3", Cmd::FindNext, &mut cmd);
                item(ui, tr("cmd.find_prev"), "Shift+F3", Cmd::FindPrev, &mut cmd);
                item(ui, tr("dlg.goto"), "Ctrl+G", Cmd::GotoLine, &mut cmd);
                if n_panes > 0 {
                    ui.separator();
                    item(
                        ui,
                        tr("cmd.toggle_bookmark"),
                        "Ctrl+F2",
                        Cmd::ToggleBookmark,
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("cmd.next_bookmark"),
                        "F4",
                        Cmd::NextBookmark,
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("cmd.prev_bookmark"),
                        "Shift+F4",
                        Cmd::PrevBookmark,
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("cmd.bookmark_comment"),
                        "",
                        Cmd::BookmarkComment,
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("cmd.clear_bookmarks"),
                        "",
                        Cmd::ClearBookmarks,
                        &mut cmd,
                    );
                    ui.separator();
                    item(ui, tr("cmd.indent"), "Ctrl+]", Cmd::Indent(false), &mut cmd);
                    item(ui, tr("cmd.outdent"), "Ctrl+[", Cmd::Indent(true), &mut cmd);
                }
                if is_folder {
                    ui.separator();
                    item(
                        ui,
                        tr("cmd.select_pattern"),
                        "",
                        Cmd::SelectPattern,
                        &mut cmd,
                    );
                }
                ui.separator();
                item(ui, tr("menu.options"), "", Cmd::Options, &mut cmd);
            });
            ui.menu_button(tr("menu.view"), |ui| {
                ui.checkbox(&mut self.cfg.tree_mode, tr("view.tree_mode"));
                ui.checkbox(&mut self.cfg.show_identical, tr("view.show_identical"));
                ui.checkbox(&mut self.cfg.show_different, tr("view.show_different"));
                ui.checkbox(&mut self.cfg.show_unique[0], tr("view.show_left_unique"));
                ui.checkbox(&mut self.cfg.show_unique[1], tr("view.show_middle_unique"));
                ui.checkbox(&mut self.cfg.show_unique[2], tr("view.show_right_unique"));
                ui.checkbox(&mut self.cfg.folder_file_pane, tr("view.folder_file_pane"));
                ui.separator();
                ui.checkbox(&mut self.cfg.show_location_pane, tr("view.location_pane"));
                ui.checkbox(&mut self.cfg.show_line_numbers, tr("view.line_numbers"));
                ui.checkbox(&mut self.cfg.show_whitespace, tr("view.whitespace"));
                ui.checkbox(&mut self.cfg.syntax_highlight, tr("view.syntax"));
                ui.checkbox(&mut self.cfg.show_line_detail, tr("view.line_detail"));
                ui.checkbox(&mut self.cfg.show_edit_markers, tr("view.edit_markers"));
                ui.checkbox(
                    &mut self.cfg.collapse_unchanged,
                    tr("cmd.collapse_unchanged"),
                );
                ui.checkbox(&mut self.cfg.word_wrap, tr("cmd.word_wrap"));
                ui.separator();
                ui.menu_button(tr("opt.whitespace"), |ui| {
                    ui.radio_value(
                        &mut self.cfg.diff.whitespace,
                        WhitespaceMode::Compare,
                        tr("opt.ws_compare"),
                    );
                    ui.radio_value(
                        &mut self.cfg.diff.whitespace,
                        WhitespaceMode::IgnoreChange,
                        tr("opt.ws_change"),
                    );
                    ui.radio_value(
                        &mut self.cfg.diff.whitespace,
                        WhitespaceMode::IgnoreAll,
                        tr("opt.ws_all"),
                    );
                });
                ui.checkbox(&mut self.cfg.diff.ignore_case, tr("opt.ignore_case"));
                ui.checkbox(
                    &mut self.cfg.diff.ignore_blank_lines,
                    tr("opt.ignore_blank"),
                );
                ui.checkbox(&mut self.cfg.diff.ignore_eol, tr("opt.ignore_eol"));
                ui.checkbox(&mut self.cfg.diff.ignore_numbers, tr("opt.ignore_numbers"));
                ui.checkbox(
                    &mut self.cfg.diff.ignore_comments,
                    tr("opt.ignore_comments"),
                );
                ui.checkbox(&mut self.cfg.diff.prettify, tr("opt.prettify"));
                ui.checkbox(&mut self.cfg.diff.detect_moved, tr("opt.detect_moved"));
                ui.menu_button(tr("opt.inline"), |ui| {
                    ui.radio_value(
                        &mut self.cfg.diff.inline,
                        InlineMode::Off,
                        tr("opt.inline_off"),
                    );
                    ui.radio_value(
                        &mut self.cfg.diff.inline,
                        InlineMode::Word,
                        tr("opt.inline_word"),
                    );
                    ui.radio_value(
                        &mut self.cfg.diff.inline,
                        InlineMode::Char,
                        tr("opt.inline_char"),
                    );
                });
                ui.separator();
                item(ui, tr("cmd.zoom_in"), "Ctrl++", Cmd::ZoomIn, &mut cmd);
                item(ui, tr("cmd.zoom_out"), "Ctrl+-", Cmd::ZoomOut, &mut cmd);
                item(ui, tr("cmd.zoom_reset"), "Ctrl+0", Cmd::ZoomReset, &mut cmd);
                ui.menu_button(tr("view.theme"), |ui| {
                    for m in ThemeMode::ALL {
                        if ui
                            .radio_value(&mut self.cfg.theme, m, tr(m.key()))
                            .clicked()
                        {
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                item(ui, tr("cmd.refresh"), "F5", Cmd::Refresh, &mut cmd);
            });
            ui.menu_button(tr("menu.merge"), |ui| {
                item(ui, tr("cmd.next_diff"), "Alt+Down", Cmd::NextDiff, &mut cmd);
                item(ui, tr("cmd.prev_diff"), "Alt+Up", Cmd::PrevDiff, &mut cmd);
                item(
                    ui,
                    tr("cmd.first_diff"),
                    "Alt+Home",
                    Cmd::FirstDiff,
                    &mut cmd,
                );
                item(ui, tr("cmd.last_diff"), "Alt+End", Cmd::LastDiff, &mut cmd);
                item(
                    ui,
                    tr("cmd.next_conflict"),
                    "Alt+PgDn",
                    Cmd::NextConflict,
                    &mut cmd,
                );
                item(
                    ui,
                    tr("cmd.prev_conflict"),
                    "Alt+PgUp",
                    Cmd::PrevConflict,
                    &mut cmd,
                );
                ui.separator();
                item(
                    ui,
                    tr("cmd.copy_right"),
                    "Alt+Right",
                    Cmd::CopyRight,
                    &mut cmd,
                );
                item(ui, tr("cmd.copy_left"), "Alt+Left", Cmd::CopyLeft, &mut cmd);
                item(
                    ui,
                    tr("cmd.copy_all_right"),
                    "Ctrl+Alt+Right",
                    Cmd::CopyAllRight,
                    &mut cmd,
                );
                item(
                    ui,
                    tr("cmd.copy_all_left"),
                    "Ctrl+Alt+Left",
                    Cmd::CopyAllLeft,
                    &mut cmd,
                );
                let merge_label = if is_folder {
                    tr("cmd.folder_merge")
                } else {
                    tr("cmd.auto_merge")
                };
                item(ui, merge_label, "", Cmd::AutoMerge, &mut cmd);
                if n_panes == 3 {
                    ui.separator();
                    for (key, left_first) in [
                        ("cmd.take_left_right", true),
                        ("cmd.take_right_left", false),
                    ] {
                        if ui
                            .add_enabled(can_take_both, egui::Button::new(tr(key)))
                            .clicked()
                        {
                            cmd = Some(Cmd::TakeBoth(left_first));
                            ui.close_menu();
                        }
                    }
                }
            });
            ui.menu_button(tr("menu.tools"), |ui| {
                item(ui, tr("menu.filters"), "", Cmd::Filters, &mut cmd);
                item(ui, tr("menu.apply_patch"), "", Cmd::ApplyPatch, &mut cmd);
                item(ui, tr("menu.statistics"), "", Cmd::Statistics, &mut cmd);
                ui.menu_button(tr("cmd.compare_as"), |ui| {
                    item(
                        ui,
                        tr("kind.text"),
                        "",
                        Cmd::CompareAs(FileKind::Text),
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("kind.binary"),
                        "",
                        Cmd::CompareAs(FileKind::Binary),
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("kind.image"),
                        "",
                        Cmd::CompareAs(FileKind::Image),
                        &mut cmd,
                    );
                    item(
                        ui,
                        tr("kind.web"),
                        "",
                        Cmd::CompareAs(FileKind::Web),
                        &mut cmd,
                    );
                });
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
                    ui.close_menu();
                }
                if ui
                    .add_enabled(registered, egui::Button::new(tr(remove)))
                    .clicked()
                {
                    let _ = crate::shell::unregister();
                    self.notify(tr(removed));
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("menu.help"), |ui| {
                item(ui, tr("help.title"), "F1", Cmd::Help, &mut cmd);
                if ui.button(tr("menu.about")).clicked() {
                    self.about = true;
                    ui.close_menu();
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Help, &widgets::with_key(tr("help.title"), "F1"))
                    .clicked()
                {
                    cmd = Some(Cmd::Help);
                }
                if widgets::icon_button(
                    ui,
                    Icon::Palette,
                    &widgets::with_key(tr("palette.title"), "Ctrl+K"),
                )
                .clicked()
                {
                    cmd = Some(Cmd::Palette);
                }
                let options = tr("menu.options").trim_end_matches(['.', '…']);
                if widgets::icon_button(ui, Icon::Settings, options).clicked() {
                    cmd = Some(Cmd::Options);
                }
                let theme_icon = if self.cfg.dark_mode {
                    Icon::Sun
                } else {
                    Icon::Moon
                };
                if widgets::icon_button(ui, theme_icon, tr("view.dark_mode")).clicked() {
                    self.cfg.theme = if self.cfg.dark_mode {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    };
                }
                let open = tr("menu.open").trim_end_matches(['.', '…']);
                if widgets::icon_button(ui, Icon::Folder, &widgets::with_key(open, "Ctrl+O"))
                    .clicked()
                {
                    cmd = Some(Cmd::Open);
                }
            });
        });
        if let Some(c) = cmd {
            self.run(c, ctx);
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
                        let mut title = full.clone();
                        if title.chars().count() > 40 {
                            title = title.chars().take(38).collect::<String>() + "…";
                        }
                        let font = if active {
                            theme::semibold(13.0)
                        } else {
                            FontId::proportional(13.0)
                        };
                        let galley = ui.painter().layout_no_wrap(
                            title,
                            font,
                            if active { p.text } else { p.text_dim },
                        );
                        let w = 40.0 + galley.size().x + 30.0;
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
                        let resp = ui.interact(
                            rect,
                            egui::Id::new(("tab", t.id)),
                            Sense::click_and_drag(),
                        );
                        resp.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                true,
                                active,
                                &full,
                            )
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
                            egui::Id::new(("tab_close", t.id)),
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
                        painter.galley(
                            egui::pos2(
                                rect.left() + 36.0,
                                rect.center().y + 1.0 - galley.size().y / 2.0,
                            ),
                            galley,
                            p.text,
                        );
                        let show_close = active || resp.hovered() || close_resp.hovered();
                        if close_resp.hovered() {
                            painter.rect_filled(close_rect, 5.0, p.control_active);
                        }
                        if show_close {
                            crate::ui::icons::paint(
                                painter,
                                egui::Rect::from_center_size(close_rect.center(), vec2(11.0, 11.0)),
                                Icon::Close,
                                p.text_dim,
                            );
                        } else if t.is_modified() {
                            painter.circle_filled(close_rect.center(), 3.5, p.warning);
                        }
                        widgets::focus_ring(ui, &resp, rect.shrink2(vec2(0.0, 3.0)), 8.0);
                        let close_resp = close_resp
                            .on_hover_text(widgets::with_key(tr("menu.close_tab"), "Ctrl+W"));
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
                            let mut item =
                                |ui: &mut egui::Ui, label: &str, enabled: bool, c: Cmd| {
                                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                        tab_cmd = Some(c);
                                        ui.close_menu();
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
                                    ui.close_menu();
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
                    if widgets::icon_button(ui, Icon::Plus, &widgets::with_key(open, "Ctrl+O"))
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

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let s = self
                .tabs
                .get(self.active)
                .map(|t| t.status_line())
                .unwrap_or_else(|| tr("app.ready").to_string());
            let outcome = self.tabs.get(self.active).and_then(|t| t.content.outcome());
            let color = outcome.map_or(p.border_strong, |o| o.color(&p));
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, color);
            if outcome == Some(Outcome::Busy) {
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
                    ui.label(egui::RichText::new("•").color(p.border_strong).size(11.0));
                }
                let text = egui::RichText::new(part).size(12.5);
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
                ui.label(egui::RichText::new(lang).size(12.5).color(p.text_dim));
                let (r, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                crate::ui::icons::paint(ui.painter(), r, Icon::Globe, p.text_dim);
            });
        });
    }

    fn start_page(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let mut open: Option<Option<String>> = None;
        let mut conflict = false;
        let mut compare: Option<Vec<String>> = None;
        let mut edit: Option<Vec<String>> = None;
        let mut remove: Option<Vec<String>> = None;
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
                            ui.label(egui::RichText::new("OxideMerge").font(theme::semibold(30.0)));
                            ui.label(
                                egui::RichText::new(tr("app.tagline"))
                                    .size(15.0)
                                    .color(p.text_dim),
                            );
                        });
                    });
                    ui.add_space(24.0);

                    let card_w = (w - 16.0) / 2.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;
                        if action_card(
                            ui,
                            card_w,
                            Icon::Compare,
                            tr("start.compare"),
                            tr("start.compare_desc"),
                        ) {
                            open = Some(None);
                        }
                        if action_card(
                            ui,
                            card_w,
                            Icon::Warning,
                            tr("menu.open_conflict").trim_end_matches(['.', '…']),
                            tr("start.conflict_desc"),
                        ) {
                            conflict = true;
                        }
                    });
                    ui.add_space(8.0);

                    let (rect, _) = ui.allocate_exact_size(vec2(w, 64.0), Sense::hover());
                    dashed_rect(ui, rect, p.border_strong);
                    let hint = ui.painter().layout_no_wrap(
                        tr("app.drop_hint").to_string(),
                        FontId::proportional(13.5),
                        p.text_dim,
                    );
                    let left = rect.center().x - (hint.size().x + 22.0) / 2.0;
                    crate::ui::icons::paint(
                        ui.painter(),
                        egui::Rect::from_min_size(
                            egui::pos2(left, rect.center().y - 8.0),
                            vec2(16.0, 16.0),
                        ),
                        Icon::Drop,
                        p.text_dim,
                    );
                    ui.painter().galley(
                        egui::pos2(left + 22.0, rect.center().y - hint.size().y / 2.0),
                        hint,
                        p.text_dim,
                    );

                    if !self.cfg.recent_compares.is_empty() {
                        ui.add_space(16.0);
                        widgets::caption(ui, tr("label.recent"));
                        widgets::card(ui)
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(w - 14.0);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for paths in self.cfg.recent_compares.iter().take(8) {
                                    let resp = recent_row(ui, paths);
                                    if resp.clicked() {
                                        compare = Some(paths.clone());
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button(tr("btn.compare")).clicked() {
                                            compare = Some(paths.clone());
                                            ui.close_menu();
                                        }
                                        if ui.button(tr("start.recent_edit")).clicked() {
                                            edit = Some(paths.clone());
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui.button(tr("start.recent_remove")).clicked() {
                                            remove = Some(paths.clone());
                                            ui.close_menu();
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
            self.open_dlg = Some(self.new_open_dialog(left));
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
    }

    // ---------- dialogs ----------

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.open_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut go = false;
        let recent = self.cfg.recent_paths.clone();
        let mut filters: Vec<String> = self.cfg.recent_filters.clone();
        for f in &self.named_filters {
            filters.push(format!("{FLT_PREFIX}{}", f.name));
        }
        let modal = egui::Modal::new(egui::Id::new("open_dialog")).show(ctx, |ui| {
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
                    let trailing_id = egui::Id::new("open_dlg_trailing_w");
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
                            let recent_id = egui::Id::new(("open_recent", s));
                            let r = widgets::icon_button_enabled(
                                ui,
                                !recent.is_empty(),
                                Icon::Clock,
                                tr("label.recent"),
                            );
                            if r.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(recent_id));
                            }
                            egui::popup::popup_below_widget(
                                ui,
                                recent_id,
                                &r,
                                egui::PopupCloseBehavior::CloseOnClick,
                                |ui| {
                                    ui.set_min_width(320.0);
                                    for r in &recent {
                                        if ui.selectable_label(false, r).clicked() {
                                            d.paths[s] = r.clone();
                                        }
                                    }
                                },
                            );
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
                .corner_radius(10)
                .inner_margin(Margin::same(12))
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
        if go {
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
        } else if keep {
            self.open_dlg = Some(d);
        }
    }

    fn options_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.options_dlg.take() else {
            return;
        };
        let mut keep = true;
        let mut apply = false;
        let results = search_options(&d.search);
        let modal = egui::Modal::new(egui::Id::new("options_dialog")).show(ctx, |ui| {
            ui.set_width(780.0);
            if widgets::dialog_title(ui, tr("menu.options")) {
                keep = false;
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
                                    ui.checkbox(&mut c.show_line_numbers, tr("view.line_numbers"));
                                    ui.checkbox(
                                        &mut c.show_location_pane,
                                        tr("view.location_pane"),
                                    );
                                    ui.checkbox(&mut c.show_whitespace, tr("view.whitespace"));
                                    ui.checkbox(&mut c.syntax_highlight, tr("view.syntax"));
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
                                    egui::ComboBox::from_label(tr("opt.algorithm"))
                                        .selected_text(format!("{:?}", o.algorithm))
                                        .show_ui(ui, |ui| {
                                            for a in [
                                                DiffAlgorithm::Myers,
                                                DiffAlgorithm::Patience,
                                                DiffAlgorithm::Lcs,
                                            ] {
                                                ui.selectable_value(
                                                    &mut o.algorithm,
                                                    a,
                                                    format!("{a:?}"),
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
                                    ui.separator();
                                    ui.checkbox(
                                        &mut o.line_filters_enabled,
                                        tr("opt.line_filters"),
                                    );
                                    ui.label(
                                        egui::RichText::new(tr("opt.line_filters_hint")).small(),
                                    );
                                    ui.add(
                                        egui::TextEdit::multiline(&mut d.line_filters)
                                            .code_editor()
                                            .desired_rows(3)
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.checkbox(
                                        &mut o.substitutions_enabled,
                                        tr("opt.substitutions"),
                                    );
                                    ui.label(
                                        egui::RichText::new(tr("opt.substitutions_hint")).small(),
                                    );
                                    ui.add(
                                        egui::TextEdit::multiline(&mut d.substitutions)
                                            .code_editor()
                                            .desired_rows(3)
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.checkbox(
                                        &mut o.block_filters_enabled,
                                        tr("opt.block_filters"),
                                    );
                                    ui.label(
                                        egui::RichText::new(tr("opt.block_filters_hint")).small(),
                                    );
                                    ui.add(
                                        egui::TextEdit::multiline(&mut d.block_filters)
                                            .code_editor()
                                            .desired_rows(2)
                                            .desired_width(f32::INFINITY),
                                    );
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
                                    ui.separator();
                                    ui.label(egui::RichText::new(tr("opt.web")).strong());
                                    ui.label(tr("opt.web_browser"));
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut c.web_browser)
                                                .hint_text(tr("opt.web_browser_hint"))
                                                .desired_width(420.0),
                                        );
                                        if widgets::icon_button(
                                            ui,
                                            Icon::File,
                                            tr("btn.browse_file"),
                                        )
                                        .clicked()
                                        {
                                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                                c.web_browser = p.display().to_string();
                                            }
                                        }
                                    });
                                    let found = web::find_browser(&c.web_browser);
                                    ui.label(
                                        egui::RichText::new(match &found {
                                            Some(p) => p.display().to_string(),
                                            None => tr("web.no_browser").to_string(),
                                        })
                                        .small()
                                        .color(pal(ui).text_dim),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut c.web_width, 320..=3840)
                                            .text(tr("opt.web_width")),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut c.web_height, 240..=8000)
                                            .text(tr("opt.web_height")),
                                    );
                                }
                                OptTab::Plugins => {
                                    ui.label(tr("opt.unpackers_hint"));
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
                                                ui.checkbox(&mut u.enabled, "");
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
                    keep = false;
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
                        d.line_filters.clear();
                        d.substitutions.clear();
                        d.block_filters.clear();
                    }
                });
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if apply {
            d.cfg.diff.line_filters = d
                .line_filters
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
            d.cfg.diff.substitutions = parse_arrow_pairs(&d.substitutions);
            d.cfg.diff.block_filters = parse_arrow_pairs(&d.block_filters)
                .into_iter()
                .filter(|(_, b)| !b.is_empty())
                .collect();
            let folder_changed = d.cfg.folder != self.cfg.folder || d.cfg.diff != self.cfg.diff;
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
                            let _ = std::fs::create_dir_all(&dir);
                            let mut k = 1;
                            let mut path = dir.join("MyFilter.flt");
                            while path.exists() {
                                k += 1;
                                path = dir.join(format!("MyFilter{k}.flt"));
                            }
                            let _ = std::fs::write(
                                &path,
                                filter::FLT_TEMPLATE
                                    .replace("My Filter", &format!("My Filter {k}")),
                            );
                            #[cfg(windows)]
                            let _ = std::process::Command::new("notepad").arg(&path).spawn();
                            reload = true;
                        }
                    }
                    if ui.button(tr("filters.open_folder")).clicked() {
                        if let Some(dir) = crate::config::config_dir().map(|d| d.join("Filters")) {
                            let _ = std::fs::create_dir_all(&dir);
                            #[cfg(windows)]
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                            #[cfg(not(windows))]
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
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
        let modal = egui::Modal::new(egui::Id::new("patch_dialog")).show(ctx, |ui| {
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
                Some(Content::Folder(v)) if v.n() == 2 => Some(v.make_patch(d.style, d.context)),
                _ => None,
            };
            match text {
                None => self.error(tr("msg.patch_two_way")),
                Some(_) if d.path.trim().is_empty() => self.patch_dlg = Some(d),
                Some(t) => match std::fs::write(d.path.trim(), t) {
                    Ok(()) => self.notify(trf("msg.saved", &[("file", d.path.clone())])),
                    Err(e) => self.error(e.to_string()),
                },
            }
        } else if keep {
            self.patch_dlg = Some(d);
        }
    }

    fn prompts(&mut self, ctx: &egui::Context) {
        let Some(p) = self.prompt.take() else { return };
        let mut keep = true;
        match &p {
            Prompt::CloseTab(i) | Prompt::SwitchFile(i, _) => {
                let i = *i;
                let finish = |app: &mut Self| match p {
                    Prompt::SwitchFile(_, forward) => app.switch_file(i, forward),
                    _ => app.close_tab(i, true),
                };
                let title = self.tabs.get(i).map(|t| t.title()).unwrap_or_default();
                let mut action = 0;
                egui::Modal::new(egui::Id::new("close_tab")).show(ctx, |ui| {
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
                match action {
                    1 => {
                        keep = false;
                        match self.tabs[i].save() {
                            Ok(()) if !self.tabs[i].is_modified() => finish(self),
                            Ok(()) => {}
                            Err(e) => self.error(e.to_string()),
                        }
                    }
                    2 => {
                        keep = false;
                        finish(self);
                    }
                    3 => keep = false,
                    _ => {}
                }
            }
            Prompt::Exit => {
                let mut action = 0;
                egui::Modal::new(egui::Id::new("exit")).show(ctx, |ui| {
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
                match action {
                    1 => {
                        keep = false;
                        let mut err = None;
                        for t in &mut self.tabs {
                            if let Err(e) = t.save() {
                                err = Some(e.to_string());
                            }
                        }
                        match err {
                            Some(e) => self.error(e),
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
        let mut undo: Option<u64> = None;
        let screen = ctx.screen_rect();
        let mut bottom = screen.bottom() - 40.0;
        for i in (0..self.toasts.len()).rev() {
            let t = &mut self.toasts[i];
            let shown = *t.shown.get_or_insert(now);
            let age = now - shown;
            let life = if t.undo_tab.is_some() {
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
            let area = egui::Area::new(egui::Id::new(("toast", i)))
                .order(egui::Order::Tooltip)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(egui::pos2(screen.right() - 16.0, bottom))
                .show(ctx, |ui| {
                    ui.set_opacity(alpha);
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0_f32, p.border_strong))
                        .corner_radius(10)
                        .shadow(ctx.style().visuals.popup_shadow)
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
                                    egui::Label::new(egui::RichText::new(&t.text).size(13.5))
                                        .wrap(),
                                );
                                if let Some(tab) = t.undo_tab {
                                    if widgets::ghost_icon_button(ui, Icon::Undo, tr("cmd.undo"))
                                        .clicked()
                                    {
                                        undo = Some(tab);
                                        close = Some(i);
                                    }
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
        if let Some(id) = undo {
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                match t.content.view_mut() {
                    Content::Text(v) => v.undo(),
                    Content::Binary(v) => v.undo(),
                    _ => {}
                }
            }
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
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
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
            self.open(OpenRequest {
                paths: dropped.into_iter().take(3).collect(),
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.cfg.dark_mode = match self.cfg.theme {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => ctx
                .system_theme()
                .map_or(self.cfg.dark_mode, |t| t == egui::Theme::Dark),
        };
        if self.visuals_dark != Some(self.cfg.dark_mode) {
            self.visuals_dark = Some(self.cfg.dark_mode);
            theme::apply(ctx, self.cfg.dark_mode);
        }
        if self.applied_lang != self.cfg.language {
            i18n::set_language(&self.cfg.language);
            self.applied_lang = self.cfg.language.clone();
        }
        if self.font_lang != i18n::current_language() {
            self.font_lang = i18n::current_language().to_string();
            fonts::setup(ctx, &self.font_lang);
        }

        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.tabs.iter().any(|t| t.is_modified())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
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
            || self.open_dlg.is_some()
            || self.options_dlg.is_some()
            || self.patch_dlg.is_some()
            || self.stats_dlg.is_some()
            || self.git_dlg.is_some()
            || self.filters_dlg
            || self.help
            || self.about
            || self.palette.is_some()
            || matches!(
                self.tabs.get(self.active).map(|t| t.content.view()),
                Some(Content::Folder(v)) if v.has_dialog()
            );
        widgets::set_modal_open(ctx, modal);
        if !modal {
            self.handle_shortcuts(ctx);
        }

        let p = theme::palette(self.cfg.dark_mode);
        let chrome = egui::Frame::new().fill(p.chrome);
        egui::TopBottomPanel::top("menu")
            .frame(chrome.inner_margin(Margin {
                left: 10,
                right: 8,
                top: 5,
                bottom: 3,
            }))
            .show_separator_line(false)
            .show(ctx, |ui| self.menu_bar(ui, ctx));
        if !self.tabs.is_empty() {
            egui::TopBottomPanel::top("tabs")
                .frame(chrome.inner_margin(Margin {
                    left: 8,
                    right: 8,
                    top: 2,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(ctx, |ui| self.tab_bar(ui));
        }
        egui::TopBottomPanel::bottom("status")
            .frame(chrome.inner_margin(Margin::symmetric(12, 3)))
            .show(ctx, |ui| self.status_bar(ui));

        let colors = theme::colors(self.cfg.dark_mode);
        let mut folder_action: Option<FolderAction> = None;
        let content_fill = if self.tabs.is_empty() {
            p.chrome
        } else {
            p.surface
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(content_fill))
            .show(ctx, |ui| {
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
        if let Some(a) = folder_action {
            match a {
                FolderAction::OpenFiles {
                    paths,
                    missing,
                    kind,
                    rel,
                } => {
                    let folder = self.tabs.get(self.active).map(|t| t.id);
                    let count = self.tabs.len();
                    self.open(OpenRequest {
                        paths,
                        missing,
                        kind,
                        skip_recent: true,
                        ..Default::default()
                    });
                    if let (Some(folder), true) = (folder, self.tabs.len() > count) {
                        if let Some(t) = self.tabs.last_mut() {
                            t.origin = Some(Origin { folder, rel });
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
                FolderAction::ShowAll => {
                    self.cfg.show_identical = true;
                    self.cfg.show_different = true;
                    self.cfg.show_unique = [true; 3];
                }
            }
        }

        self.palette_ui(ctx);
        self.open_dialog(ctx);
        self.options_dialog(ctx);
        self.filters_dialog(ctx);
        self.patch_dialog(ctx);
        self.stats_dialog(ctx);
        self.git_dialog(ctx);
        self.poll_report(ctx);
        self.about_window(ctx);
        self.prompts(ctx);
        let mut bulk = Vec::new();
        for t in &mut self.tabs {
            let msg = match t.content.view_mut() {
                Content::Text(v) => v.bulk_edit.take(),
                Content::Binary(v) => v.bulk_edit.take(),
                _ => None,
            };
            if let Some(m) = msg {
                bulk.push((m, t.id));
            }
        }
        for (m, id) in bulk {
            self.push_toast(m, false, Some(id));
        }
        self.toasts_ui(ctx);
        drop_overlay(ctx, self.open_dlg.is_some());
        self.save_session();

        if self.cfg != self.saved_cfg {
            self.cfg.save();
            self.saved_cfg = self.cfg.clone();
        }
    }
}
