//! Main application window: menus, tabs, dialogs and command dispatch.

use crate::config::Config;
use crate::core::archive;
use crate::core::conflict;
use crate::core::diff::{DiffAlgorithm, InlineMode, WhitespaceMode};
use crate::core::filter::{self, FltFilter, FLT_PREFIX};
use crate::core::folder::CompareMethod;
use crate::core::patch::PatchStyle;
use crate::core::report::ReportFormat;
use crate::core::text::{self, FileKind, TextDoc};
use crate::core::unpacker::{self, Unpacker};
use crate::i18n::{self, tr, trf};
use crate::ui::binary_view::BinaryView;
use crate::ui::folder_view::{FolderAction, FolderView};
use crate::ui::image_view::ImageView;
use crate::ui::text_view::TextView;
use crate::ui::widgets::{self, pal};
use crate::ui::{fonts, theme};
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
}

pub enum Content {
    Folder(FolderView),
    Text(TextView),
    Binary(BinaryView),
    Image(ImageView),
}

pub struct Tab {
    pub content: Content,
    /// Paths used to open this tab (for "compare as" re-opening).
    pub source: Vec<PathBuf>,
}

impl Tab {
    fn title(&self) -> String {
        match &self.content {
            Content::Folder(v) => v.title(),
            Content::Text(v) => v.title(),
            Content::Binary(v) => v.title(),
            Content::Image(v) => v.title(),
        }
    }

    fn icon(&self) -> &'static str {
        match &self.content {
            Content::Folder(_) => "📁",
            Content::Text(_) => "📝",
            Content::Binary(_) => "🔢",
            Content::Image(_) => "🖼",
        }
    }

    fn is_modified(&self) -> bool {
        match &self.content {
            Content::Folder(_) => false,
            Content::Text(v) => v.is_modified(),
            Content::Binary(v) => v.is_modified(),
            Content::Image(v) => v.is_modified(),
        }
    }

    fn status_line(&self) -> String {
        match &self.content {
            Content::Folder(v) => v.status_line(),
            Content::Text(v) => v.status_line(),
            Content::Binary(v) => v.status_line(),
            Content::Image(v) => v.status_line(),
        }
    }

    fn save(&mut self) -> anyhow::Result<()> {
        match &mut self.content {
            Content::Folder(_) => Ok(()),
            Content::Text(v) => v.save_modified(),
            Content::Binary(v) => v.save(),
            Content::Image(v) => v.save(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Open,
    OpenConflict,
    Save,
    SaveAs(usize),
    CloseTab,
    Exit,
    Undo,
    Redo,
    Find,
    FindNext,
    FindPrev,
    GotoLine,
    Options,
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

struct OptionsDialog {
    cfg: Config,
    tab: OptTab,
    line_filters: String,
    substitutions: String,
}

struct PatchDialog {
    style: PatchStyle,
    context: usize,
    path: String,
}

enum Prompt {
    CloseTab(usize),
    Exit,
    Message(String),
}

pub struct OxideMergeApp {
    cfg: Config,
    saved_cfg: Config,
    tabs: Vec<Tab>,
    active: usize,
    open_dlg: Option<OpenDialog>,
    options_dlg: Option<OptionsDialog>,
    patch_dlg: Option<PatchDialog>,
    filters_dlg: bool,
    about: bool,
    shortcuts: bool,
    named_filters: Vec<FltFilter>,
    prompt: Option<Prompt>,
    font_lang: String,
    applied_lang: String,
    visuals_dark: Option<bool>,
    allow_close: bool,
}

fn kind_label(k: Option<FileKind>) -> &'static str {
    match k {
        None => tr("kind.auto"),
        Some(FileKind::Text) => tr("kind.text"),
        Some(FileKind::Binary) => tr("kind.binary"),
        Some(FileKind::Image) => tr("kind.image"),
    }
}

/// Large clickable card on the start page.
fn action_card(ui: &mut egui::Ui, w: f32, icon: &str, title: &str, desc: &str) -> bool {
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
    let icon_rect = egui::Rect::from_min_size(rect.min + vec2(18.0, 18.0), vec2(36.0, 36.0));
    painter.rect_filled(icon_rect, 10.0, p.accent_soft);
    painter.text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(18.0),
        p.accent,
    );
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
            filters_dlg: false,
            about: false,
            shortcuts: false,
            named_filters: filter::load_all_filters(),
            prompt: None,
            font_lang: lang,
            applied_lang,
            visuals_dark: None,
            allow_close: false,
        };
        if let Some(req) = initial {
            app.open(req);
        }
        app
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.prompt = Some(Prompt::Message(msg.into()));
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
        if let Err(e) = self.try_open(req) {
            self.error(e.to_string());
        }
    }

    fn try_open(&mut self, req: OpenRequest) -> anyhow::Result<()> {
        let mut paths = req.paths.clone();
        if paths.is_empty() {
            return Ok(());
        }
        if paths.len() == 1 {
            let p = &paths[0];
            if p.is_file() {
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
                    let target = if docs.len() == 3 { 1 } else { docs.len() - 1 };
                    let d = &mut docs[target];
                    if d.description.is_none() {
                        d.description = Some(format!("{} → {}", d.title(), out.display()));
                    }
                    d.path = Some(out.clone());
                    d.read_only = false;
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
        };
        self.push_tab(content, source);
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
        }
        v.status = trf("msg.conflicts_found", &[("n", parts.conflicts.to_string())]);
        self.push_tab(Content::Text(v), vec![path.to_path_buf()]);
        Ok(())
    }

    fn push_tab(&mut self, content: Content, source: Vec<PathBuf>) {
        self.tabs.push(Tab { content, source });
        self.active = self.tabs.len() - 1;
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

    // ---------- commands ----------

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        let cfg = self.cfg.clone();
        match cmd {
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
            Cmd::Options => {
                self.options_dlg = Some(OptionsDialog {
                    line_filters: self.cfg.diff.line_filters.join("\n"),
                    substitutions: self
                        .cfg
                        .diff
                        .substitutions
                        .iter()
                        .map(|(a, b)| format!("{a} => {b}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    cfg: self.cfg.clone(),
                    tab: OptTab::General,
                })
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
            Cmd::Patch => {
                self.patch_dlg = Some(PatchDialog {
                    style: self.cfg.patch_style,
                    context: self.cfg.patch_context,
                    path: String::new(),
                })
            }
            Cmd::Report => self.save_report(),
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
                let mut err: Option<String> = None;
                match &mut tab.content {
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
                        Cmd::FindNext => v.find_next(true),
                        Cmd::FindPrev => v.find_next(false),
                        Cmd::GotoLine => v.goto = Some(String::new()),
                        Cmd::NextDiff => v.next_diff(false),
                        Cmd::PrevDiff => v.prev_diff(false),
                        Cmd::FirstDiff => v.first_diff(),
                        Cmd::LastDiff => v.last_diff(),
                        Cmd::NextConflict => v.next_diff(true),
                        Cmd::PrevConflict => v.prev_diff(true),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        Cmd::CopyAllRight => v.copy_all(0, 1),
                        Cmd::CopyAllLeft => {
                            let n = v.n();
                            v.copy_all(n - 1, n - 2);
                        }
                        Cmd::AutoMerge => v.auto_merge(),
                        Cmd::Refresh => v.refresh(),
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
                        Cmd::NextDiff => v.next_diff(),
                        Cmd::PrevDiff => v.prev_diff(),
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
                        Cmd::NextDiff => v.next_diff(),
                        Cmd::PrevDiff => v.prev_diff(),
                        Cmd::FirstDiff => v.first_diff(),
                        Cmd::LastDiff => v.last_diff(),
                        Cmd::CopyRight => v.copy_current(true),
                        Cmd::CopyLeft => v.copy_current(false),
                        _ => {}
                    },
                    Content::Folder(v) => match cmd {
                        Cmd::Refresh => v.refresh(&cfg, &named),
                        Cmd::CopyRight => v.copy_selected(true, &cfg),
                        Cmd::CopyLeft => v.copy_selected(false, &cfg),
                        _ => {}
                    },
                }
                if let Some(e) = err {
                    self.error(e);
                }
            }
        }
    }

    fn save_report(&mut self) {
        let Some(Tab {
            content: Content::Folder(v),
            ..
        }) = self.tabs.get(self.active)
        else {
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
            Ok(()) => {
                self.prompt = Some(Prompt::Message(trf(
                    "msg.saved",
                    &[("file", path.display().to_string())],
                )))
            }
            Err(e) => self.error(e.to_string()),
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
            sc(Modifiers::COMMAND, Key::O, Cmd::Open);
            sc(Modifiers::COMMAND, Key::S, Cmd::Save);
            sc(Modifiers::COMMAND, Key::W, Cmd::CloseTab);
            sc(Modifiers::COMMAND, Key::F4, Cmd::CloseTab);
            sc(Modifiers::COMMAND, Key::F, Cmd::Find);
            sc(Modifiers::COMMAND, Key::G, Cmd::GotoLine);
            sc(Modifiers::COMMAND, Key::Tab, Cmd::NextTab);
            sc(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Tab,
                Cmd::PrevTab,
            );
            sc(Modifiers::COMMAND, Key::Plus, Cmd::ZoomIn);
            sc(Modifiers::COMMAND, Key::Equals, Cmd::ZoomIn);
            sc(Modifiers::COMMAND, Key::Minus, Cmd::ZoomOut);
            sc(Modifiers::COMMAND, Key::Num0, Cmd::ZoomReset);
            sc(Modifiers::SHIFT, Key::F3, Cmd::FindPrev);
            sc(Modifiers::NONE, Key::F3, Cmd::FindNext);
            sc(Modifiers::NONE, Key::F5, Cmd::Refresh);
            if !typing {
                sc(Modifiers::COMMAND, Key::Z, Cmd::Undo);
                sc(Modifiers::COMMAND, Key::Y, Cmd::Redo);
                sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z, Cmd::Redo);
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
            }
        });
        for c in cmds {
            self.run(c, ctx);
        }
    }

    // ---------- menus ----------

    fn menu_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut cmd: Option<Cmd> = None;
        let n_panes = match self.tabs.get(self.active).map(|t| &t.content) {
            Some(Content::Text(v)) => v.n(),
            _ => 0,
        };
        let is_folder = matches!(
            self.tabs.get(self.active).map(|t| &t.content),
            Some(Content::Folder(_))
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
        egui::menu::bar(ui, |ui| {
            ui.spacing_mut().button_padding = vec2(9.0, 4.0);
            widgets::logo(ui, 22.0);
            ui.label(egui::RichText::new("OxideMerge").font(theme::semibold(14.5)));
            ui.add_space(12.0);
            ui.menu_button(tr("menu.file"), |ui| {
                item(ui, tr("menu.open"), "Ctrl+O", Cmd::Open, &mut cmd);
                item(
                    ui,
                    tr("menu.open_conflict"),
                    "",
                    Cmd::OpenConflict,
                    &mut cmd,
                );
                ui.separator();
                item(ui, tr("menu.save"), "Ctrl+S", Cmd::Save, &mut cmd);
                for p in 0..n_panes {
                    let label = trf(
                        "menu.save_side_as",
                        &[("side", crate::core::report::side_name(p, n_panes).into())],
                    );
                    if ui.button(label).clicked() {
                        cmd = Some(Cmd::SaveAs(p));
                        ui.close_menu();
                    }
                }
                ui.separator();
                item(ui, tr("menu.patch"), "", Cmd::Patch, &mut cmd);
                if is_folder {
                    item(ui, tr("menu.report"), "", Cmd::Report, &mut cmd);
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
                item(ui, tr("cmd.find_next"), "F3", Cmd::FindNext, &mut cmd);
                item(ui, tr("cmd.find_prev"), "Shift+F3", Cmd::FindPrev, &mut cmd);
                item(ui, tr("dlg.goto"), "Ctrl+G", Cmd::GotoLine, &mut cmd);
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
                ui.separator();
                ui.checkbox(&mut self.cfg.show_location_pane, tr("view.location_pane"));
                ui.checkbox(&mut self.cfg.show_line_numbers, tr("view.line_numbers"));
                ui.checkbox(&mut self.cfg.show_whitespace, tr("view.whitespace"));
                ui.checkbox(&mut self.cfg.syntax_highlight, tr("view.syntax"));
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
                ui.checkbox(&mut self.cfg.dark_mode, tr("view.dark_mode"));
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
                item(ui, tr("cmd.auto_merge"), "", Cmd::AutoMerge, &mut cmd);
            });
            ui.menu_button(tr("menu.tools"), |ui| {
                item(ui, tr("menu.filters"), "", Cmd::Filters, &mut cmd);
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
                });
                if cfg!(windows) {
                    ui.separator();
                    let registered = crate::shell::is_registered();
                    if ui
                        .add_enabled(!registered, egui::Button::new(tr("menu.shell_register")))
                        .clicked()
                    {
                        let r = crate::shell::register();
                        self.prompt = Some(Prompt::Message(match r {
                            Ok(()) => tr("msg.shell_registered").into(),
                            Err(e) => e.to_string(),
                        }));
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(registered, egui::Button::new(tr("menu.shell_unregister")))
                        .clicked()
                    {
                        let _ = crate::shell::unregister();
                        self.prompt = Some(Prompt::Message(tr("msg.shell_unregistered").into()));
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button(tr("menu.language"), |ui| {
                let cur = i18n::current_language();
                if ui
                    .radio(self.cfg.language.is_empty(), tr("lang.system"))
                    .clicked()
                {
                    self.cfg.language.clear();
                    ui.close_menu();
                }
                for l in i18n::languages() {
                    if ui
                        .radio(!self.cfg.language.is_empty() && cur == l.code, &l.name)
                        .clicked()
                    {
                        self.cfg.language = l.code.clone();
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button(tr("menu.help"), |ui| {
                if ui.button(tr("menu.shortcuts")).clicked() {
                    self.shortcuts = true;
                    ui.close_menu();
                }
                if ui.button(tr("menu.about")).clicked() {
                    self.about = true;
                    ui.close_menu();
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, "⚙", tr("menu.options")).clicked() {
                    cmd = Some(Cmd::Options);
                }
                let theme_icon = if self.cfg.dark_mode { "☀" } else { "🌙" };
                if widgets::icon_button(ui, theme_icon, tr("view.dark_mode")).clicked() {
                    self.cfg.dark_mode = !self.cfg.dark_mode;
                }
                if widgets::icon_button(ui, "📂", tr("menu.open")).clicked() {
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
        egui::ScrollArea::horizontal()
            .id_salt("tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
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
                        let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
                        let close_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 17.0, rect.center().y + 1.0),
                            vec2(20.0, 20.0),
                        );
                        let close_resp =
                            ui.interact(close_rect, ui.id().with(("tab_close", i)), Sense::click());
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
                        painter.text(
                            egui::pos2(rect.left() + 20.0, rect.center().y + 1.0),
                            Align2::CENTER_CENTER,
                            t.icon(),
                            FontId::proportional(14.0),
                            p.text_dim,
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
                            painter.text(
                                close_rect.center(),
                                Align2::CENTER_CENTER,
                                "✖",
                                FontId::proportional(10.0),
                                p.text_dim,
                            );
                        } else if t.is_modified() {
                            painter.circle_filled(close_rect.center(), 3.5, p.warning);
                        }
                        if close_resp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            activate = Some(i);
                        }
                        resp.on_hover_text(full);
                    }
                    ui.add_space(4.0);
                    if widgets::icon_button(ui, "➕", tr("menu.open")).clicked() {
                        new_tab = true;
                    }
                });
            });
        if let Some(i) = activate {
            self.active = i;
        }
        if new_tab {
            self.open_dlg = Some(self.new_open_dialog(None));
        }
        if let Some(i) = close {
            self.close_tab(i, false);
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
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, p.success);
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
                ui.label(
                    egui::RichText::new(format!("🌐 {lang}"))
                        .size(12.5)
                        .color(p.text_dim),
                );
            });
        });
    }

    fn start_page(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let mut open: Option<Option<String>> = None;
        let mut conflict = false;
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
                            "📂",
                            tr("start.compare"),
                            tr("start.compare_desc"),
                        ) {
                            open = Some(None);
                        }
                        if action_card(
                            ui,
                            card_w,
                            "⚠",
                            tr("menu.open_conflict").trim_end_matches(['.', '…']),
                            tr("start.conflict_desc"),
                        ) {
                            conflict = true;
                        }
                    });
                    ui.add_space(8.0);

                    let (rect, _) = ui.allocate_exact_size(vec2(w, 64.0), Sense::hover());
                    dashed_rect(ui, rect, p.border_strong);
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        format!("⬇  {}", tr("app.drop_hint")),
                        FontId::proportional(13.5),
                        p.text_dim,
                    );

                    if !self.cfg.recent_paths.is_empty() {
                        ui.add_space(16.0);
                        widgets::caption(ui, tr("label.recent"));
                        widgets::card(ui)
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(w - 14.0);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for path in self.cfg.recent_paths.iter().take(8) {
                                    let (rect, resp) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 34.0),
                                        Sense::click(),
                                    );
                                    if resp.hovered() {
                                        ui.painter().rect_filled(rect, 7.0, p.control_hover);
                                    }
                                    let icon = if Path::new(path).is_dir() {
                                        "📁"
                                    } else {
                                        "📄"
                                    };
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 18.0, rect.center().y),
                                        Align2::CENTER_CENTER,
                                        icon,
                                        FontId::proportional(14.0),
                                        p.text_dim,
                                    );
                                    let clip =
                                        ui.painter().with_clip_rect(egui::Rect::from_min_max(
                                            egui::pos2(rect.left() + 30.0, rect.top()),
                                            egui::pos2(rect.right() - 10.0, rect.bottom()),
                                        ));
                                    clip.text(
                                        egui::pos2(rect.left() + 36.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        path,
                                        FontId::proportional(13.5),
                                        p.text,
                                    );
                                    if resp
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .clicked()
                                    {
                                        open = Some(Some(path.clone()));
                                    }
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
                                        egui::RichText::new(tr("btn.browse_file"))
                                            .color(p.text_dim),
                                    )
                                    .margin(vec2(8.0, 5.0))
                                    .desired_width(w),
                            );
                            ui.add_space(4.0);
                            ui.menu_button("🕘", |ui| {
                                for r in &recent {
                                    if ui.button(r).clicked() {
                                        d.paths[s] = r.clone();
                                        ui.close_menu();
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tr("label.recent"));
                            if widgets::icon_button(ui, "📄", tr("btn.browse_file")).clicked() {
                                if let Some(p) = rfd::FileDialog::new().pick_file() {
                                    d.paths[s] = p.display().to_string();
                                }
                            }
                            if widgets::icon_button(ui, "📁", tr("btn.browse_folder")).clicked() {
                                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                    d.paths[s] = p.display().to_string();
                                }
                            }
                            let lock = if d.read_only[s] { "🔒" } else { "🔓" };
                            widgets::toggle_button(
                                ui,
                                &mut d.read_only[s],
                                lock,
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
                    if widgets::ghost_button(ui, &format!("⇅  {}", tr("btn.swap"))).clicked() {
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
        let modal = egui::Modal::new(egui::Id::new("options_dialog")).show(ctx, |ui| {
            ui.set_width(780.0);
            if widgets::dialog_title(ui, tr("menu.options")) {
                keep = false;
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(190.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (t, icon, key) in [
                        (OptTab::General, "⚙", "opt.tab_general"),
                        (OptTab::Compare, "⇆", "opt.tab_compare"),
                        (OptTab::Folder, "📁", "opt.tab_folder"),
                        (OptTab::Media, "🖼", "opt.tab_media"),
                        (OptTab::Plugins, "🔧", "opt.tab_plugins"),
                    ] {
                        if widgets::nav_item(ui, d.tab == t, icon, tr(key)).clicked() {
                            d.tab = t;
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
                                    ui.checkbox(&mut c.dark_mode, tr("view.dark_mode"));
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
                                    ui.checkbox(&mut c.use_recycle_bin, tr("opt.recycle_bin"));
                                    ui.checkbox(&mut c.confirm_operations, tr("opt.confirm_ops"));
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
                                }
                                OptTab::Plugins => {
                                    ui.label(tr("opt.unpackers_hint"));
                                    let mut remove = None;
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
                                                ui.add(
                                                    egui::TextEdit::singleline(&mut u.pattern)
                                                        .desired_width(140.0),
                                                );
                                                ui.add(
                                                    egui::TextEdit::singleline(&mut u.command)
                                                        .desired_width(260.0),
                                                );
                                                if ui.button("🗑").clicked() {
                                                    remove = Some(i);
                                                }
                                                ui.end_row();
                                            }
                                        });
                                    if let Some(i) = remove {
                                        d.cfg.unpackers.remove(i);
                                    }
                                    if ui.button(format!("➕ {}", tr("btn.add"))).clicked() {
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
                    if widgets::ghost_button(ui, &format!("↺  {}", tr("btn.defaults"))).clicked()
                    {
                        let recent = (d.cfg.recent_paths.clone(), d.cfg.recent_filters.clone());
                        d.cfg = Config::default();
                        d.cfg.recent_paths = recent.0;
                        d.cfg.recent_filters = recent.1;
                        d.line_filters.clear();
                        d.substitutions.clear();
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
            d.cfg.diff.substitutions = d
                .substitutions
                .lines()
                .filter_map(|l| {
                    l.split_once("=>")
                        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                })
                .filter(|(a, _)| !a.is_empty())
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
            || matches!(self.tabs.get(self.active).map(|t| &t.content), Some(Content::Folder(v)) if v.filter_spec != default_filter);
        egui::Window::new(tr("menu.filters"))
            .open(&mut open)
            .default_width(560.0)
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
            if let Some(Content::Folder(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.content)
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
                        if widgets::icon_button(ui, "📁", tr("dlg.save_as")).clicked() {
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
            let text = match self.tabs.get(self.active).map(|t| &t.content) {
                Some(Content::Text(v)) => v.make_patch(d.style, d.context),
                Some(Content::Folder(v)) if v.n() == 2 => Some(v.make_patch(d.style, d.context)),
                _ => None,
            };
            match text {
                None => self.error(tr("msg.patch_two_way")),
                Some(_) if d.path.trim().is_empty() => self.patch_dlg = Some(d),
                Some(t) => match std::fs::write(d.path.trim(), t) {
                    Ok(()) => {
                        self.prompt = Some(Prompt::Message(trf(
                            "msg.saved",
                            &[("file", d.path.clone())],
                        )))
                    }
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
            Prompt::Message(m) => {
                egui::Modal::new(egui::Id::new("msg")).show(ctx, |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(520.0);
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(m).size(14.5));
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("btn.ok")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape))
                        {
                            keep = false;
                        }
                    });
                });
            }
            Prompt::CloseTab(i) => {
                let i = *i;
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
                            Ok(()) if !self.tabs[i].is_modified() => self.close_tab(i, true),
                            Ok(()) => {}
                            Err(e) => self.error(e.to_string()),
                        }
                    }
                    2 => {
                        keep = false;
                        self.close_tab(i, true);
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

    fn about_window(&mut self, ctx: &egui::Context) {
        egui::Window::new(tr("menu.about"))
            .open(&mut self.about)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    widgets::logo(ui, 44.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("OxideMerge").font(theme::semibold(22.0)));
                        let p = pal(ui);
                        widgets::chip(
                            ui,
                            &format!("v{}", env!("CARGO_PKG_VERSION")),
                            p.accent,
                            p.accent_soft,
                        );
                    });
                });
                ui.add_space(6.0);
                ui.label(tr("app.tagline"));
                ui.label(tr("app.about"));
                ui.hyperlink_to("WinMerge", "https://winmerge.org/");
                ui.label(concat!("License: ", env!("CARGO_PKG_LICENSE")));
            });
        egui::Window::new(tr("menu.shortcuts"))
            .open(&mut self.shortcuts)
            .show(ctx, |ui| {
                egui::Grid::new("sc").striped(true).show(ui, |ui| {
                    for (k, v) in [
                        ("Ctrl+O", "menu.open"),
                        ("Ctrl+S", "menu.save"),
                        ("Ctrl+W", "menu.close_tab"),
                        ("Ctrl+Z / Ctrl+Y", "cmd.undo"),
                        ("Ctrl+F / F3 / Shift+F3", "cmd.find"),
                        ("Ctrl+G", "dlg.goto"),
                        ("F5", "cmd.refresh"),
                        ("Alt+Down / Alt+Up", "cmd.next_diff"),
                        ("Alt+Home / Alt+End", "cmd.first_diff"),
                        ("Alt+PgDn / Alt+PgUp", "cmd.next_conflict"),
                        ("Alt+Right", "cmd.copy_right"),
                        ("Alt+Left", "cmd.copy_left"),
                        ("Ctrl+Alt+Right / Left", "cmd.copy_all_right"),
                        ("F2 / Enter / Double-click", "cmd.edit_line"),
                        ("Ctrl+Tab", "menu.next_tab"),
                        ("Ctrl + / Ctrl -", "cmd.zoom_in"),
                    ] {
                        ui.monospace(k);
                        ui.label(tr(v));
                        ui.end_row();
                    }
                });
            });
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

        self.handle_drops(ctx);
        if self.prompt.is_none() && self.open_dlg.is_none() {
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
                    Some(Content::Text(v)) => {
                        v.set_options(&cfg.diff);
                        v.ui(ui, &cfg, &colors);
                    }
                    Some(Content::Binary(v)) => v.ui(ui, &cfg, &colors),
                    Some(Content::Image(v)) => v.ui(ui, &cfg, &colors),
                    Some(Content::Folder(v)) => folder_action = v.ui(ui, &cfg, &colors, &named),
                }
            });
        if let Some(a) = folder_action {
            match a {
                FolderAction::OpenFiles {
                    paths,
                    missing,
                    kind,
                } => {
                    self.open(OpenRequest {
                        paths,
                        missing,
                        kind,
                        ..Default::default()
                    });
                }
                FolderAction::OpenFolders { paths } => {
                    self.open(OpenRequest {
                        paths,
                        ..Default::default()
                    });
                }
            }
        }

        self.open_dialog(ctx);
        self.options_dialog(ctx);
        self.filters_dialog(ctx);
        self.patch_dialog(ctx);
        self.about_window(ctx);
        self.prompts(ctx);

        if self.cfg != self.saved_cfg {
            self.cfg.save();
            self.saved_cfg = self.cfg.clone();
        }
    }
}
