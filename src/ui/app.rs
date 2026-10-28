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
use crate::core::report::{self, side_name, ReportFormat};
use crate::core::rules::{self, RuleDraft, RuleKind, RuleSet};
use crate::core::saved::{self, SavedCompare};
use crate::core::text::{self, FileKind, TextDoc};
use crate::core::unpacker::{self, Unpacker};
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
    /// The folder compare this tab was opened from, for moving on to its next or previous file.
    origin: Option<Origin>,
    /// Temporary copies of sides (a Git revision, extracted archives), deleted with the last
    /// tab showing them: the folder compare and the files opened from it.
    temps: Vec<std::sync::Arc<tempfile::TempDir>>,
}

#[derive(Clone)]
struct Origin {
    /// Id of the folder compare tab.
    folder: u64,
    /// Relative path of the item in that folder compare.
    rel: String,
}

impl Tab {
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
        match &self.content {
            Content::Folder(_) | Content::Web(_) => false,
            Content::Text(v) => v.is_modified(),
            Content::Binary(v) => v.is_modified(),
            Content::Image(v) => v.is_modified(),
            Content::Diffs(v) => v.is_modified(),
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
            match v.options_for(&cfg.diff) {
                Some(o) => v.set_options(&o),
                None => v.set_options(&cfg.diff),
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
    (OptTab::Compare, "opt.numeric_tolerance"),
    (OptTab::Rules, "opt.rules"),
    (OptTab::Rules, "rules.lines"),
    (OptTab::Rules, "rules.substitutions"),
    (OptTab::Rules, "rules.blocks"),
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
                for set in &d.rule_sets {
                    if ui.button(&set.name).clicked() {
                        export = Some(Some(set.clone()));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(tr("rules.export_all")).clicked() {
                    export = Some(None);
                    ui.close_menu();
                }
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

struct GitDialog {
    path: String,
    rev: String,
    /// Recent commits of `commits_for`: short hash and subject.
    commits: Vec<(String, String)>,
    commits_for: String,
    /// Recent commits being read with `git log`, for the path it was started for.
    commits_job: Option<(String, CommitsRx)>,
    /// The path as last typed and when, so `git log` waits until typing pauses.
    typed: (String, std::time::Instant),
}

/// How long the Git dialog's path must stay unchanged before its commits are read.
const GIT_LOG_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

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
    SwitchFile(u64, bool),
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
    /// The config with the options of the command line, and as loaded without them.
    cli_options: Option<(Config, Config)>,
    tabs: Vec<Tab>,
    active: usize,
    open_dlg: Option<OpenDialog>,
    options_dlg: Option<OptionsDialog>,
    rule_dlg: Option<RuleDialog>,
    patch_dlg: Option<PatchDialog>,
    stats_dlg: Option<StatsDialog>,
    git_dlg: Option<GitDialog>,
    /// A folder report with file differences being written on another thread: its file.
    report_job: Option<(PathBuf, std::sync::mpsc::Receiver<Result<(), String>>)>,
    /// Ctrl+Tab presses taken from the input before egui saw them.
    tab_keys: Vec<Cmd>,
    filters_dlg: bool,
    about: bool,
    help: bool,
    named_filters: Vec<FltFilter>,
    prompt: Option<Prompt>,
    /// Tabs with unsaved changes still to ask about after the one prompted, to close them all.
    close_queue: Vec<u64>,
    toasts: Vec<Toast>,
    font_lang: String,
    applied_lang: String,
    visuals_dark: Option<bool>,
    allow_close: bool,
    /// When to try saving the settings again after saving them failed.
    settings_retry: Option<f64>,
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
    /// `OXIDEMERGE_SMOKE_REPORT`: where to write what the window shows once its compares are
    /// done, then close; and the frames drawn so far.
    smoke: Option<(PathBuf, u32)>,
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
    /// The text compare has synchronization points, or a line picked for one.
    has_sync_points: bool,
    /// The tab compares two or more paths.
    has_source: bool,
    /// The folder compare of the tab is still running.
    folder_busy: bool,
    /// The tab compares web pages, whichever way it shows them.
    web: bool,
    tabs: usize,
    picked: Option<Cmd>,
    /// Compare rules file picked for the active tab ("" = none).
    rules: Option<String>,
    recent: Option<Vec<String>>,
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
            (NextConflict | PrevConflict, Text) | (AutoMerge, Text | Folder) => self.sides == 3,
            (
                Replace | ToggleBookmark | NextBookmark | PrevBookmark | ClearBookmarks
                | BookmarkComment | Indent(_) | SaveComparison | SyncPoint,
                Text,
            ) => true,
            (ClearSyncPoints, Text) => self.has_sync_points,
            (Find | CopyRight | CopyLeft | SelectPattern | Statistics, Folder) => true,
            // A report or patch of a folder compare still running would miss files.
            (ReportWithDiffs | ExportHtml | Report | Slideshow, Folder) => !self.folder_busy,
            (Patch, Folder) => self.sides == 2 && !self.folder_busy,
            (Patch, Text) => self.sides == 2,
            (ExportHtml | Report | Slideshow | Statistics, Text) => true,
            (Refresh, Text | Folder | Web) => true,
            (AllDiffs, Folder) => !self.folder_busy,
            (
                Save | Undo | NextDiff | PrevDiff | FirstDiff | LastDiff | CopyRight | CopyLeft
                | Refresh | Find,
                Diffs,
            ) => true,
            (
                New | Open | OpenConflict | OpenProject | GitCompare | ApplyPatch | Options
                | CompareOptions | Filters | EditRules | Help | Palette | Exit | ZoomIn | ZoomOut
                | ZoomReset | ToggleCollapse | ToggleWrap | CloseTabAt(_) | CloseOtherTabs(_)
                | CloseTabsToRight(_),
                _,
            ) => true,
            _ => false,
        }
    }

    /// A menu item that runs `c` with its key, greyed out when it does nothing for the active tab.
    fn item(&mut self, ui: &mut egui::Ui, c: Cmd) {
        self.item_as(ui, tr(c.label()), c);
    }

    fn item_as(&mut self, ui: &mut egui::Ui, label: &str, c: Cmd) {
        let button =
            egui::Button::new(label).shortcut_text(widgets::menu_shortcut(ui, &c.shown_key()));
        if ui.add_enabled(self.enabled(c), button).clicked() {
            self.picked = Some(c);
            ui.close_menu();
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
    if ui.layer_id().id != egui::Id::new(MENU_MEASURE) {
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
    /// `stored` is the config as loaded, before the options of the command line went into `cfg`.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        stored: Config,
        initial: Option<OpenRequest>,
    ) -> Self {
        let lang = i18n::current_language().to_string();
        fonts::setup(&cc.egui_ctx, &lang);
        #[cfg(target_os = "macos")]
        crate::ui::macos::ask_before_quitting();
        let applied_lang = cfg.language.clone();
        let mut app = OxideMergeApp {
            saved_cfg: cfg.clone(),
            cli_options: (cfg != stored).then(|| (cfg.clone(), stored)),
            cfg,
            tabs: Vec::new(),
            active: 0,
            open_dlg: None,
            options_dlg: None,
            patch_dlg: None,
            stats_dlg: None,
            git_dlg: None,
            report_job: None,
            tab_keys: Vec::new(),
            filters_dlg: false,
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
            was_focused: true,
            next_tab_id: 0,
            track_session: initial.is_none(),
            palette: None,
            text_layout: (false, false),
            smoke: std::env::var_os("OXIDEMERGE_SMOKE_REPORT")
                .filter(|p| !p.is_empty())
                .map(|p| (PathBuf::from(p), 0)),
        };
        app.text_layout = (app.cfg.collapse_unchanged, app.cfg.word_wrap);
        crate::core::fsutil::set_backup(app.cfg.backup_on_save);
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
                rules: s.rules.clone(),
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
            if let Some(r) = &req.rules {
                v.set_rules(&app.cfg, r.clone());
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
                let mut v = TextView::new(docs, &self.cfg);
                if req.rules.is_some() {
                    v.set_rules(req.rules.clone());
                    if let Some(o) = v.options_for(&self.cfg.diff) {
                        v.set_options(&o);
                    }
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
            let mut v = FolderView::new(
                vec![copy, path.to_path_buf()],
                display,
                Vec::new(),
                &self.cfg.filter,
                &self.named_filters,
                &self.cfg,
            );
            v.read_only[0] = true;
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

    fn push_tab(&mut self, content: Content, source: Vec<PathBuf>) {
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id: self.next_tab_id,
            content,
            source,
            origin: None,
            temps: Vec::new(),
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
            .map(|t| t.content.view())
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
            self.prompt = Some(Prompt::SwitchFile(self.tabs[ti].id, forward));
        } else {
            self.switch_file(ti, forward);
        }
    }

    fn switch_file(&mut self, ti: usize, forward: bool) {
        let Some(Origin { folder, rel }) = self.tabs.get(ti).and_then(|t| t.origin.clone()) else {
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
        let Some(FolderAction::OpenFiles {
            paths,
            missing,
            read_only,
            descriptions,
            kind,
            rel,
            rules,
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
            read_only,
            descriptions,
            kind,
            skip_recent: true,
            rules: Some(rules),
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
        tab.temps = temps;
        self.tabs[ti] = tab;
        self.active = ti;
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
        self.refresh_saved_folders();
        self.tabs.remove(i);
        self.shown_active = None;
        if i < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Closes the tabs selected by `close`. Tabs with unsaved changes stay open and ask, one after
    /// the other, whether to save.
    fn close_tabs(&mut self, close: impl Fn(usize) -> bool) {
        let mut kept = Vec::new();
        let mut active = 0;
        let mut ask = Vec::new();
        for (i, t) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if close(i) {
                if !t.is_modified() {
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
        let modal = egui::Modal::new(egui::Id::new("rule_dialog")).show(ctx, |ui| {
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
        let toggles: [(&str, ConfigFlag); 9] = [
            ("view.line_numbers", |c| &mut c.show_line_numbers),
            ("view.location_pane", |c| &mut c.show_location_pane),
            ("view.line_detail", |c| &mut c.show_line_detail),
            ("view.edit_markers", |c| &mut c.show_edit_markers),
            ("view.folder_file_pane", |c| &mut c.folder_file_pane),
            ("view.whitespace", |c| &mut c.show_whitespace),
            ("view.syntax", |c| &mut c.syntax_highlight),
            ("view.tree_mode", |c| &mut c.tree_mode),
            ("menu.open_reports", |c| &mut c.open_reports),
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
            Cmd::ExportHtml
                | Cmd::Report
                | Cmd::Slideshow
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
            Cmd::CompareOptions => self.open_options(OptTab::Compare),
            Cmd::ToggleCollapse => self.cfg.collapse_unchanged ^= true,
            Cmd::ToggleWrap => self.cfg.word_wrap ^= true,
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
            Cmd::Patch | Cmd::ExportHtml | Cmd::Report | Cmd::ReportWithDiffs | Cmd::Slideshow
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
            Cmd::ExportHtml => self.export_html(),
            Cmd::AllDiffs => self.show_all_diffs(),
            Cmd::Report => self.save_report(),
            Cmd::ReportWithDiffs => self.save_report_with_diffs(),
            Cmd::Slideshow => self.save_slideshow(),
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
                    typed: (String::new(), std::time::Instant::now()),
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
                        self.open(OpenRequest {
                            paths,
                            kind: Some(kind),
                            rules,
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
                        Cmd::SyncPoint => v.set_sync_point(),
                        Cmd::ClearSyncPoints => v.clear_sync_points(),
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
            // A text compare's report is HTML or XML, by the type picked.
            let name = report::html_file_name(&v.titles());
            let Some(path) = self.report_path(tr("menu.report"), &name, &[HTML, XML]) else {
                return;
            };
            let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view())
            else {
                return;
            };
            let job = v.report_job(ReportFormat::from_path(&path), &self.cfg.rules);
            self.spawn_report(path, job);
            return;
        }
        let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            self.error(tr("msg.report_folder_only"));
            return;
        };
        let name = report::html_file_name(&v.display_roots);
        let filters = [HTML, ("CSV", &["csv"][..]), ("Text", &["txt"][..]), XML];
        let Some(path) = self.report_path(tr("menu.report"), &name, &filters) else {
            return;
        };
        let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            return;
        };
        let text = v.report(ReportFormat::from_path(&path), &self.cfg);
        self.spawn_report(path, move || text);
    }

    fn save_report_with_diffs(&mut self) {
        if let Some(Content::Text(_)) = self.tabs.get(self.active).map(|t| t.content.view()) {
            self.export_text_html(tr("menu.report_diffs"));
            return;
        }
        let Some(Content::Folder(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            self.error(tr("msg.report_folder_only"));
            return;
        };
        let name = report::html_file_name(&v.display_roots);
        let job = v.report_with_diffs_job(&self.cfg);
        self.write_html_report(tr("menu.report_diffs"), &name, job);
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
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.content, Content::Diffs(v) if v.folder == folder))
        {
            self.active = i;
            let Content::Diffs(v) = &mut self.tabs[i].content else {
                return;
            };
            if v.lists_all(&src) {
                v.set_stopped(src.stopped);
            } else if v.is_modified() || v.can_undo() {
                self.notify(tr("diffs.outdated"));
            } else {
                v.update_from(src);
            }
            return;
        }
        let view = DiffsView::new(src, folder);
        if view.is_empty() {
            self.notify(tr("diffs.empty"));
            return;
        }
        let temps = tab.temps.clone();
        self.push_tab(Content::Diffs(view), Vec::new());
        if let Some(t) = self.tabs.last_mut() {
            t.temps = temps;
        }
    }

    /// Shows the errors of tabs of all differences, and after they saved files, compares their
    /// folder compares again and has text compares notice the files that changed.
    fn refresh_saved_folders(&mut self) {
        let mut folders = Vec::new();
        let mut errors = Vec::new();
        for t in &mut self.tabs {
            if let Content::Diffs(v) = &mut t.content {
                if std::mem::take(&mut v.saved) {
                    folders.push(v.folder);
                }
                errors.extend(v.error.take());
            }
        }
        for e in errors {
            self.error(e);
        }
        if folders.is_empty() {
            return;
        }
        let (cfg, named) = (self.cfg.clone(), self.named_filters.clone());
        for t in &mut self.tabs {
            let id = t.id;
            match t.content.view_mut() {
                Content::Folder(f) if folders.contains(&id) => f.refresh(&cfg, &named),
                Content::Text(v) => v.check_disk(),
                _ => {}
            }
        }
    }

    fn export_html(&mut self) {
        match self.tabs.get(self.active).map(|t| t.content.view()) {
            Some(Content::Text(_)) => self.export_text_html(tr("menu.export_html")),
            Some(Content::Folder(_)) => self.save_report_with_diffs(),
            _ => self.error(tr("msg.report_folder_only")),
        }
    }

    /// Saves the HTML report of the active text compare, made on another thread once the file is
    /// picked.
    fn export_text_html(&mut self, title: &str) {
        let Some(Content::Text(v)) = self.tabs.get(self.active).map(|t| t.content.view()) else {
            return;
        };
        if let Some(path) = self.html_report_path(title, &report::html_file_name(&v.titles())) {
            self.export_text_html_to(path);
        }
    }

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
        self.notify(trf("msg.saved", &[("file", path.display().to_string())]));
        let html = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
        if html && self.cfg.open_reports {
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
                self.error(e);
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
        if d.typed.0 != d.path {
            d.typed = (d.path.clone(), std::time::Instant::now());
        }
        let wait = GIT_LOG_DELAY.saturating_sub(d.typed.1.elapsed());
        let started_for = d.commits_job.as_ref().map(|(p, _)| p.as_str());
        let pending = d.commits_for != d.path && started_for != Some(d.path.as_str());
        if pending && !wait.is_zero() {
            ctx.request_repaint_after(wait);
        } else if pending && path.exists() {
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
                proj.rules = Some(v.rules.clone());
            }
            Content::Text(v) => {
                proj.kind = Some(kind_name(FileKind::Text).into());
                proj.rules = v.rules.clone();
            }
            Content::Binary(_) => proj.kind = Some(kind_name(FileKind::Binary).into()),
            Content::Image(_) => proj.kind = Some(kind_name(FileKind::Image).into()),
            Content::Web(_) => proj.kind = Some(kind_name(FileKind::Web).into()),
            Content::Diffs(_) => return,
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
        let typing = widgets::typing(ctx);
        let menus = self.menus();
        let mut cmds = std::mem::take(&mut self.tab_keys);
        ctx.input_mut(|i| {
            for b in BINDINGS {
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
            has_sync_points: matches!(view, Some(Content::Text(v)) if v.has_sync_points()),
            has_source: tab.is_some_and(|t| t.source.len() >= 2),
            folder_busy: matches!(view, Some(Content::Folder(v)) if v.is_running()),
            web: matches!(tab.map(|t| &t.content), Some(Content::Web(_))),
            tabs: self.tabs.len(),
            picked: None,
            rules: None,
            recent: None,
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut m = self.menus();
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
        let width = self.menu_width(ctx, &mut m);
        widgets::set_menu_width(ctx, width);
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
            for (i, key) in MENUS.into_iter().enumerate() {
                ui.menu_button(tr(key), |ui| {
                    widgets::menu_setup(ui);
                    self.menu(ui, &mut m, i);
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Help, &Cmd::Help.tip()).clicked() {
                    m.picked = Some(Cmd::Help);
                }
                if widgets::icon_button(ui, Icon::Palette, &Cmd::Palette.tip()).clicked() {
                    m.picked = Some(Cmd::Palette);
                }
                let options = tr("menu.options").trim_end_matches(['.', '…']);
                if widgets::icon_button(ui, Icon::Settings, options).clicked() {
                    m.picked = Some(Cmd::Options);
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
                if widgets::icon_button(ui, Icon::Folder, &widgets::with_key(open, Cmd::Open.key()))
                    .clicked()
                {
                    m.picked = Some(Cmd::Open);
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
                    ui.close_menu();
                }
            }
        }
    }

    /// The width all menus open with, so that they match: that of the widest menu of the menu
    /// bar or of its submenus, measured once per language, fonts, text size, display scale and
    /// kind of view by laying the menus out unseen.
    fn menu_width(&mut self, ctx: &egui::Context, m: &mut Menus) -> f32 {
        let size = ctx.style().text_styles[&egui::TextStyle::Button].size;
        // The fonts in use: after a change of language they follow a frame later.
        let fonts = ctx.fonts(|f| egui::Id::new(&f.lock().fonts.definitions().families));
        // The Merge menu names its first item after the view.
        let id = egui::Id::new("menu_width")
            .with(i18n::current_language())
            .with(fonts)
            .with(size.to_bits())
            .with(ctx.pixels_per_point().to_bits())
            .with(m.view == ViewKind::Folder);
        if let Some(width) = ctx.data(|d| d.get_temp::<f32>(id)) {
            return width;
        }
        let width = egui::Area::new(egui::Id::new(MENU_MEASURE))
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
        m.item(ui, Cmd::Open);
        let recent = &self.cfg.recent_compares;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            submenu_list(ui, tr("menu.open_recent"), |ui| {
                for paths in recent.iter().take(10) {
                    if ui.button(recent_label(paths)).clicked() {
                        m.recent = Some(paths.clone());
                        ui.close_menu();
                    }
                }
            });
        });
        m.item(ui, Cmd::OpenConflict);
        m.item(ui, Cmd::GitCompare);
        ui.separator();
        m.item(ui, Cmd::OpenProject);
        m.item(ui, Cmd::SaveProject);
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
            m.item(ui, Cmd::ExportHtml);
            m.item(ui, Cmd::Report);
            m.item(ui, Cmd::ReportWithDiffs);
            m.item(ui, Cmd::Slideshow);
            m.item(ui, Cmd::Patch);
            ui.separator();
            check(ui, &mut self.cfg.open_reports, tr("menu.open_reports"), "");
        });
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
                "",
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
                    ui.close_menu();
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
        let names_id = egui::Id::new("menu_rule_names");
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
                    ui.close_menu();
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
                                    let button = egui::Button::new(label)
                                        .shortcut_text(widgets::menu_shortcut(ui, &keys));
                                    if ui.add_enabled(enabled, button).clicked() {
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
                                        widgets::menu_setup(ui);
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
        let mut leave = false;
        let results = search_options(&d.search);
        let modal = egui::Modal::new(egui::Id::new("options_dialog")).show(ctx, |ui| {
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
                                    ui.checkbox(&mut c.web_render, tr("opt.web_render"));
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
                Some(t) => {
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
            Prompt::CloseTab(id) | Prompt::SwitchFile(id, _) => {
                let Some(i) = self.tabs.iter().position(|t| t.id == *id) else {
                    if matches!(p, Prompt::CloseTab(_)) {
                        self.ask_next_close();
                    }
                    return;
                };
                let finish = |app: &mut Self| match p {
                    Prompt::SwitchFile(_, forward) => app.switch_file(i, forward),
                    _ => app.close_tab(i, true),
                };
                let title = self.tabs.get(i).map(|t| t.title()).unwrap_or_default();
                let mut action = 0;
                let modal = egui::Modal::new(egui::Id::new("close_tab")).show(ctx, |ui| {
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
                        match self.tabs[i].save() {
                            Ok(()) if !self.tabs[i].is_modified() => {
                                finish(self);
                                if queued {
                                    self.ask_next_close();
                                }
                            }
                            Ok(()) => self.close_queue.clear(),
                            Err(e) => {
                                self.close_queue.clear();
                                self.error(e.to_string());
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
                let modal = egui::Modal::new(egui::Id::new("exit")).show(ctx, |ui| {
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
        let busy = self.prompt.is_some()
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

        let unsaved = self.tabs.iter().any(|t| t.is_modified());
        #[cfg(target_os = "macos")]
        crate::ui::macos::set_unsaved(unsaved && !self.allow_close);
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && unsaved {
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
            || self.open_dlg.is_some()
            || self.options_dlg.is_some()
            || self.patch_dlg.is_some()
            || self.stats_dlg.is_some()
            || self.git_dlg.is_some()
            || self.rule_dlg.is_some()
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
            self.cfg.web_highlight = w.highlight;
        }
        self.sync_text_layout();
        crate::core::fsutil::set_backup(self.cfg.backup_on_save);
        self.announce_finished(ctx);
        self.take_step_file();
        self.refresh_saved_folders();
        if let Some(a) = folder_action {
            match a {
                FolderAction::OpenFiles {
                    paths,
                    missing,
                    read_only,
                    descriptions,
                    kind,
                    rel,
                    rules,
                } => {
                    let folder = self.tabs.get(self.active).map(|t| match &t.content {
                        Content::Diffs(v) => v.folder,
                        _ => t.id,
                    });
                    let temps = self.tabs.get(self.active).map(Tab::shown_temps);
                    let count = self.tabs.len();
                    self.open(OpenRequest {
                        paths,
                        missing,
                        read_only,
                        descriptions,
                        kind,
                        skip_recent: true,
                        rules: Some(rules),
                        ..Default::default()
                    });
                    if let (Some(folder), true) = (folder, self.tabs.len() > count) {
                        if let Some(t) = self.tabs.last_mut() {
                            t.origin = Some(Origin { folder, rel });
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
                FolderAction::ShowAll => {
                    self.cfg.show_identical = true;
                    self.cfg.show_different = true;
                    self.cfg.show_unique = [true; 3];
                }
                FolderAction::AddRule(draft) => self.open_rule_dialog(self.active, draft),
                FolderAction::EditRules => self.open_options(OptTab::Rules),
                FolderAction::ExportHtml => self.export_html(),
                FolderAction::AllDiffs => self.show_all_diffs(),
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
            self.export_html();
        }

        self.palette_ui(ctx);
        self.open_dialog(ctx);
        self.options_dialog(ctx);
        self.filters_dialog(ctx);
        self.patch_dialog(ctx);
        self.rule_dialog(ctx);
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

        // Settings that could not be read are left alone, as the message at startup said.
        let refused = matches!(crate::config::unreadable(), Some(Err(_)));
        let now = ctx.input(|i| i.time);
        if self.cfg != self.saved_cfg && !refused && self.settings_retry.is_none_or(|t| now >= t) {
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
    }
}

impl OxideMergeApp {
    /// For automated checks of a real window: after a few frames, and once compares are done
    /// (or after about a minute), writes what the window shows and closes it.
    fn smoke_report(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = &mut self.smoke else {
            return;
        };
        *frames += 1;
        ctx.request_repaint();
        let busy = self
            .tabs
            .iter()
            .any(|t| matches!(t.content.outcome(), Some(Outcome::Busy)));
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
}

#[cfg(test)]
#[path = "gui_tests.rs"]
mod gui_tests;

#[cfg(test)]
mod tests {
    use super::*;

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
