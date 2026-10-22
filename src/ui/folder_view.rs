//! Folder compare view.

use crate::config::Config;
use crate::core::diff::DiffOptions;
use crate::core::filter::{FileFilter, FltFilter};
use crate::core::folder::{self, CmpStatus, DirItem, FolderOptions, Progress, Scanner};
use crate::core::patch::{self, PatchStyle};
use crate::core::report::{self, fmt_size, fmt_time, side_name, status_text, ReportFormat};
use crate::core::text::FileKind;
use crate::i18n::{tr, trf};
use crate::ui::theme::{self, Colors};
use crate::ui::widgets;
use egui::{vec2, Align, Color32, FontId, Key, Layout, Margin, Modifiers, Sense, Stroke, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub enum FolderAction {
    OpenFiles { paths: Vec<PathBuf>, missing: Vec<bool>, kind: Option<FileKind> },
    OpenFolders { paths: Vec<PathBuf> },
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
}

struct Running {
    progress: Arc<Progress>,
    handle: JoinHandle<Vec<DirItem>>,
    started: Instant,
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
    anchor: Option<usize>,
    sort: (Col, bool),
    visible: Vec<usize>,
    view_key: Option<(bool, bool, bool, [bool; 3])>,
    pub status: String,
    confirm: Option<PendingOp>,
    scroll_to: Option<usize>,
    id: egui::Id,
}

fn system_open(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
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
            sort: (Col::Name, true),
            visible: Vec::new(),
            view_key: None,
            status: String::new(),
            confirm: None,
            scroll_to: None,
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
        let progress = Arc::new(Progress::default());
        let (roots, filter, fopts, dopts, pr) =
            (self.roots.clone(), self.filter.clone(), self.fopts.clone(), self.dopts.clone(), progress.clone());
        let handle = std::thread::spawn(move || {
            Scanner { roots: &roots, filter: &filter, fopts: &fopts, dopts: &dopts, progress: &pr }.run()
        });
        self.running = Some(Running { progress, handle, started: Instant::now() });
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

    pub fn stop(&mut self) {
        if let Some(r) = &self.running {
            r.progress.abort.store(true, Ordering::Relaxed);
        }
    }

    fn poll(&mut self) {
        let finished = self.running.as_ref().is_some_and(|r| r.handle.is_finished());
        if finished {
            let r = self.running.take().unwrap();
            let aborted = r.progress.abort.load(Ordering::Relaxed);
            self.items = r.handle.join().unwrap_or_default();
            self.elapsed = Some(r.started.elapsed());
            self.selected.clear();
            self.view_key = None;
            if aborted {
                self.status = tr("msg.aborted").into();
            }
        }
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
            CmpStatus::Unique => {
                let unique_flag = |s: usize| if n == 2 && s == 1 { cfg.show_unique[2] } else { cfg.show_unique[s] };
                (0..n).any(|s| it.exists[s] && unique_flag(s))
            }
        }
    }

    fn cmp_items(&self, a: usize, b: usize) -> std::cmp::Ordering {
        let (ia, ib) = (&self.items[a], &self.items[b]);
        let (col, asc) = self.sort;
        let name = || ia.name.to_lowercase().cmp(&ib.name.to_lowercase());
        let o = match col {
            Col::Name => ib.is_dir.cmp(&ia.is_dir).then_with(name),
            Col::Folder => ia.rel_dir.to_lowercase().cmp(&ib.rel_dir.to_lowercase()).then_with(name),
            Col::Result => status_text(ia, self.n()).cmp(&status_text(ib, self.n())).then_with(name),
            Col::Date(s) => ia.mtime[s].cmp(&ib.mtime[s]).then_with(name),
            Col::Size(s) => ia.size[s].cmp(&ib.size[s]).then_with(name),
            Col::Ext => ia.ext().to_lowercase().cmp(&ib.ext().to_lowercase()).then_with(name),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    }

    fn visibility(&self, cfg: &Config) -> Vec<bool> {
        let mut vis = vec![false; self.items.len()];
        for i in (0..self.items.len()).rev() {
            let it = &self.items[i];
            vis[i] = if it.is_dir && !it.children.is_empty() {
                it.children.iter().any(|&c| vis[c]) || self.shown(it, cfg) && it.status != CmpStatus::Identical
            } else {
                self.shown(it, cfg)
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
                self.items[a]
                    .rel_dir
                    .to_lowercase()
                    .cmp(&self.items[b].rel_dir.to_lowercase())
                    .then_with(|| self.cmp_items(a, b))
            } else {
                self.cmp_items(a, b)
            }
        });
        v
    }

    fn rebuild_visible(&mut self, cfg: &Config) {
        if !cfg.tree_mode {
            self.visible = self.flat_list(cfg);
            return;
        }
        let vis = self.visibility(cfg);
        let mut out = Vec::new();
        let mut roots: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].parent.is_none() && vis[i]).collect();
        roots.sort_by(|&a, &b| self.cmp_items(a, b));
        let mut stack: Vec<usize> = roots.into_iter().rev().collect();
        while let Some(i) = stack.pop() {
            out.push(i);
            let it = &self.items[i];
            if it.is_dir && self.expanded.contains(&it.rel_path()) {
                let mut ch: Vec<usize> = it.children.iter().copied().filter(|&c| vis[c]).collect();
                ch.sort_by(|&a, &b| self.cmp_items(a, b));
                stack.extend(ch.into_iter().rev());
            }
        }
        self.visible = out;
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
        let targets: Vec<usize> = self.selected_targets().into_iter().filter(|&i| self.items[i].exists[from]).collect();
        if targets.is_empty() {
            return;
        }
        let op = PendingOp { kind: OpKind::Copy { from, to }, targets };
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
        let mut errors = Vec::new();
        let mut any_dir = false;
        let mut done = 0;
        for &i in &op.targets {
            let it = &self.items[i];
            any_dir |= it.is_dir;
            let r = match op.kind {
                OpKind::Copy { from, to } => folder::copy_recursive(&it.path(&self.roots[from]), &it.path(&self.roots[to])),
                OpKind::Delete { side } => {
                    let sides: Vec<usize> = match side {
                        Some(s) => vec![s],
                        None => (0..self.n()).filter(|&s| it.exists[s]).collect(),
                    };
                    sides
                        .into_iter()
                        .filter(|&s| it.exists[s])
                        .try_for_each(|s| folder::delete_path(&it.path(&self.roots[s]), cfg.use_recycle_bin))
                }
            };
            match r {
                Ok(()) => done += 1,
                Err(e) => errors.push(format!("{}: {e}", it.rel_path())),
            }
        }
        self.status = trf("msg.op_done", &[("n", done.to_string())]);
        if !errors.is_empty() {
            self.status.push_str(&format!(" — {}: {}", tr("status.error"), errors.join("; ")));
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
        Some(FolderAction::OpenFiles { paths, missing, kind })
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
        patch::folder_patch(&self.roots, &self.display_roots, &self.items, &self.dopts, style, context)
    }

    pub fn report(&self, format: ReportFormat, cfg: &Config) -> String {
        let list = self.flat_list(cfg);
        let items: Vec<&DirItem> = list.iter().map(|&i| &self.items[i]).collect();
        let roots: Vec<PathBuf> = self.display_roots.iter().map(PathBuf::from).collect();
        report::generate(&items, &roots, format)
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
            &[("same", same.to_string()), ("diff", diff.to_string()), ("unique", uniq.to_string()), ("err", err.to_string())],
        );
        if let Some(e) = self.elapsed {
            s.push_str(&format!("  |  {:.2}s", e.as_secs_f32()));
        }
        if self.selected.len() > 1 {
            s.push_str(&format!("  |  {} {}", self.selected.len(), tr("label.selected")));
        }
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    pub fn expand_all(&mut self, expand: bool) {
        if expand {
            self.expanded = self.items.iter().filter(|i| i.is_dir).map(|i| i.rel_path()).collect();
        } else {
            self.expanded.clear();
        }
        self.view_key = None;
    }

    // ---------- UI ----------

    fn header_bar(&mut self, ui: &mut Ui, cfg: &Config) -> bool {
        let mut refresh = false;
        let n = self.n();
        let sel = !self.selected.is_empty();
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            if widgets::ghost_button(ui, &format!("⟳  {}", tr("cmd.refresh"))).on_hover_text("F5").clicked() {
                refresh = true;
            }
            if cfg.tree_mode {
                if widgets::icon_button(ui, "⊞", tr("cmd.expand_all")).clicked() {
                    self.expand_all(true);
                }
                if widgets::icon_button(ui, "⊟", tr("cmd.collapse_all")).clicked() {
                    self.expand_all(false);
                }
            }
            widgets::toolbar_sep(ui);
            if n == 2 {
                if widgets::icon_button_enabled(ui, sel, "→", tr("cmd.copy_right")).clicked() {
                    self.request_op_copy(0, 1, cfg);
                }
                if widgets::icon_button_enabled(ui, sel, "←", tr("cmd.copy_left")).clicked() {
                    self.request_op_copy(1, 0, cfg);
                }
            } else {
                if widgets::icon_button_enabled(ui, sel, "L→M", tr("cmd.copy_from_left")).clicked() {
                    self.request_op_copy(0, 1, cfg);
                }
                if widgets::icon_button_enabled(ui, sel, "M←R", tr("cmd.copy_from_right")).clicked() {
                    self.request_op_copy(2, 1, cfg);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                widgets::chip(ui, &format!("{}  {}", tr("label.method"), tr(self.fopts.method.key())), pal.text_dim, pal.control);
                widgets::chip(ui, &format!("{}  {}", tr("label.filter"), self.filter_spec), pal.text_dim, pal.control);
            });
        });
        let pal = widgets::pal(ui);
        let paths = egui::Frame::new().fill(pal.surface).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.columns(n, |cols| {
                for (s, col) in cols.iter_mut().enumerate() {
                    col.horizontal(|ui| {
                        widgets::side_chip(ui, s, n);
                        ui.add(egui::Label::new(egui::RichText::new(&self.display_roots[s]).size(13.0)).truncate())
                            .on_hover_text(&self.display_roots[s]);
                    });
                }
            });
        });
        let rect = paths.response.rect;
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, pal.border));
        if let Some(r) = &self.running {
            let done = r.progress.done.load(Ordering::Relaxed);
            let total = r.progress.total.load(Ordering::Relaxed);
            let mut stop = false;
            egui::Frame::new().inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    let text = if total > 0 {
                        format!("{done} / {total}")
                    } else {
                        format!("{} {}", tr("msg.scanning"), r.progress.current.lock().map(|c| c.clone()).unwrap_or_default())
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
        refresh
    }

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors, named: &[FltFilter]) -> Option<FolderAction> {
        self.poll();
        if self.running.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if self.header_bar(ui, cfg) {
            self.refresh(cfg, named);
        }
        let key = (cfg.tree_mode, cfg.show_identical, cfg.show_different, cfg.show_unique);
        if self.view_key != Some(key) {
            self.view_key = Some(key);
            self.rebuild_visible(cfg);
        }

        let mut result: Option<FolderAction> = None;
        let mut toggle: Option<usize> = None;
        let mut click: Option<(usize, Modifiers)> = None;
        let mut double: Option<usize> = None;
        let mut menu: Option<MenuCmd> = None;
        let mut new_sort: Option<Col> = None;

        // keyboard navigation
        if !ui.ctx().wants_keyboard_input() && self.confirm.is_none() && !self.visible.is_empty() {
            let cur = self.anchor.unwrap_or(0).min(self.visible.len() - 1);
            let mut target = None;
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    target = Some((cur + 1).min(self.visible.len() - 1));
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    target = Some(cur.saturating_sub(1));
                }
                if i.consume_key(Modifiers::NONE, Key::Home) {
                    target = Some(0);
                }
                if i.consume_key(Modifiers::NONE, Key::End) {
                    target = Some(self.visible.len() - 1);
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
                self.selected.clear();
                self.selected.insert(self.visible[t]);
                self.anchor = Some(t);
                self.scroll_to = Some(t);
            }
        }

        let n = self.n();
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 12.0;
        let pal = widgets::pal(ui);
        let side_cols: Vec<Color32> = (0..n).map(|s| widgets::side_color(ui, s, n)).collect();
        let status_color = |it: &DirItem| -> Color32 {
            match it.status {
                CmpStatus::Identical => pal.success,
                CmpStatus::Different => pal.warning,
                CmpStatus::Error => pal.danger,
                CmpStatus::NotCompared => pal.text_dim,
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
        let mut cols = vec![(Col::Name, tr("col.name").to_string()), (Col::Folder, tr("col.folder").into()), (Col::Result, tr("col.result").into())];
        for s in 0..n {
            cols.push((Col::Date(s), format!("{} {}", side_name(s, n), tr("col.date"))));
        }
        for s in 0..n {
            cols.push((Col::Size(s), format!("{} {}", side_name(s, n), tr("col.size"))));
        }
        cols.push((Col::Ext, tr("col.ext").into()));

        let sort = self.sort;
        let items = &self.items;
        let visible = &self.visible;
        let selected = &self.selected;
        let expanded = &self.expanded;
        let tree = cfg.tree_mode;
        table
            .header(row_h, |mut header| {
                for (col, label) in &cols {
                    header.col(|ui| {
                        ui.painter().rect_filled(ui.max_rect().expand2(vec2(4.0, 3.0)), 0.0, pal.surface_alt);
                        let r = ui.max_rect().expand2(vec2(4.0, 3.0));
                        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0_f32, pal.border));
                        let sorted = sort.0 == *col;
                        let arrow = if sorted { if sort.1 { "  ▲" } else { "  ▼" } } else { "" };
                        let color = if sorted { pal.text } else { pal.text_dim };
                        let text = egui::RichText::new(format!("{label}{arrow}")).font(theme::semibold(12.5)).color(color);
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
                                let open = expanded.contains(&it.rel_path());
                                let arrow = egui::RichText::new(if open { "⏷" } else { "⏵" }).color(pal.text_dim);
                                if ui.add(egui::Button::new(arrow).frame(false).small()).clicked() {
                                    toggle = Some(idx);
                                }
                            } else {
                                ui.add_space(16.0);
                            }
                        }
                        let (icon, icon_color) = if it.is_dir { ("📁", Color32::from_rgb(0xE0, 0xA4, 0x2E)) } else { ("📄", pal.text_dim) };
                        ui.add(egui::Label::new(txt(icon.to_string()).color(icon_color)).selectable(false));
                        ui.add(egui::Label::new(txt(it.name.clone())).selectable(false).truncate());
                    });
                    row.col(|ui| {
                        paint_bg(ui);
                        ui.add(egui::Label::new(txt(it.rel_dir.clone()).color(pal.text_dim)).selectable(false).truncate());
                    });
                    row.col(|ui| {
                        paint_bg(ui);
                        let sc = status_color(it);
                        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, sc);
                        ui.add(egui::Label::new(txt(status_text(it, n)).font(FontId::proportional(13.5))).selectable(false).truncate());
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
                                ui.add(egui::Label::new(txt(fmt_size(it.size[s]))).selectable(false));
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
                                    let label = trf("cmd.copy_from_to", &[("from", side_name(from, n).into()), ("to", side_name(to, n).into())]);
                                    if ui.add_enabled(it.exists[from], egui::Button::new(label)).clicked() {
                                        menu = Some(MenuCmd::Op(OpKind::Copy { from, to }));
                                    }
                                }
                            }
                        }
                        ui.menu_button(tr("cmd.delete"), |ui| {
                            for s in 0..n {
                                if ui.add_enabled(it.exists[s], egui::Button::new(side_name(s, n))).clicked() {
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
                                if ui.add_enabled(it.exists[s], egui::Button::new(side_name(s, n))).clicked() {
                                    menu = Some(MenuCmd::SystemOpen(idx, s));
                                }
                            }
                        });
                        ui.menu_button(tr("cmd.reveal"), |ui| {
                            for s in 0..n {
                                if ui.add_enabled(it.exists[s], egui::Button::new(side_name(s, n))).clicked() {
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
                        if menu.is_some() {
                            ui.close_menu();
                        }
                    });
                });
            });

        if let Some(col) = new_sort {
            self.sort = if self.sort.0 == col { (col, !self.sort.1) } else { (col, true) };
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
            }
        }
        if let Some(m) = menu {
            match m {
                MenuCmd::Open(i, kind) => result = self.open_action(i, kind),
                MenuCmd::Op(kind) => self.request_op(kind, cfg),
                MenuCmd::SystemOpen(i, s) => system_open(&self.items[i].path(&self.roots[s])),
                MenuCmd::Reveal(i, s) => reveal(&self.items[i].path(&self.roots[s])),
                MenuCmd::CopyPath(i, s) => ui.ctx().copy_text(self.items[i].path(&self.roots[s]).display().to_string()),
            }
        }

        // confirmation dialog
        if let Some(op) = self.confirm.take() {
            let mut keep = true;
            let mut run = false;
            let msg = match op.kind {
                OpKind::Copy { from, to } => trf(
                    "msg.confirm_copy",
                    &[("n", op.targets.len().to_string()), ("from", side_name(from, n).into()), ("to", side_name(to, n).into())],
                ),
                OpKind::Delete { side } => trf(
                    "msg.confirm_delete",
                    &[("n", op.targets.len().to_string()), ("side", side.map(|s| side_name(s, n)).unwrap_or(tr("side.all")).into())],
                ),
            };
            egui::Modal::new(self.id.with("confirm")).show(ui.ctx(), |ui| {
                ui.set_min_width(400.0);
                ui.set_max_width(520.0);
                let pal = widgets::pal(ui);
                ui.heading(tr("dlg.confirm"));
                ui.label(msg);
                ui.add_space(4.0);
                egui::Frame::new().fill(pal.surface_alt).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for &i in op.targets.iter().take(10) {
                        ui.label(egui::RichText::new(self.items[i].rel_path()).monospace().color(pal.text_dim));
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
                    if widgets::secondary_button(ui, tr("btn.cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
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
        result
    }
}
