//! Headless end-to-end tests of the main window. Each test drives the real application one
//! frame at a time with pointer, keyboard, text and file-drop input, finds widgets by the names
//! they give assistive technologies, and checks the views and files that result. A failing test
//! leaves what the window last showed in `target/gui-tests/<test>.txt`.

use super::*;
use crate::core::diff::BlockKind;
use egui::{accesskit, Event, Modifiers, PointerButton, Pos2, Rect};
use std::time::{Duration, Instant};

const SCREEN: egui::Vec2 = egui::vec2(1280.0, 820.0);

#[derive(Clone)]
struct Widget {
    role: accesskit::Role,
    label: String,
    rect: Rect,
    enabled: bool,
    /// Read out when it changes (a live region).
    live: Option<accesskit::Live>,
    /// The state of a check box or switch.
    toggled: Option<accesskit::Toggled>,
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

struct Gui {
    name: &'static str,
    /// The window's size in points.
    screen: egui::Vec2,
    ctx: egui::Context,
    app: OxideMergeApp,
    frame: eframe::Frame,
    time: f64,
    events: Vec<Event>,
    modifiers: Modifiers,
    dropped: Vec<PathBuf>,
    focused: bool,
    /// In drawing order, so later widgets are on top.
    widgets: Vec<Widget>,
    /// Every control that can be clicked or set, named or not.
    controls: Vec<Widget>,
    /// The widget with the keyboard, which a screen reader reads out.
    focus: Option<Widget>,
    copied: Vec<String>,
    closed: bool,
    /// Whether the window is asked to close on the next frame, as by its close button.
    close_asked: bool,
}

fn config() -> Config {
    Config {
        language: "en".into(),
        theme: ThemeMode::Light,
        restore_session: false,
        open_reports: false,
        // The Trash on macOS goes through Finder, which may wait for a permission prompt.
        use_recycle_bin: false,
        ..Default::default()
    }
}

impl Gui {
    fn start(name: &'static str, initial: Option<OpenRequest>) -> Gui {
        Gui::with_config(name, config(), initial)
    }

    fn with_config(name: &'static str, cfg: Config, initial: Option<OpenRequest>) -> Gui {
        i18n::set_language(&cfg.language);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = OxideMergeApp::new(&cc, cfg.clone(), cfg, initial);
        let mut gui = Gui {
            name,
            screen: SCREEN,
            ctx,
            app,
            frame: eframe::Frame::_new_kittest(),
            time: 0.0,
            events: Vec::new(),
            modifiers: Modifiers::NONE,
            dropped: Vec::new(),
            focused: true,
            widgets: Vec::new(),
            controls: Vec::new(),
            focus: None,
            copied: Vec::new(),
            closed: false,
            close_asked: false,
        };
        gui.settle();
        gui
    }

    fn frame(&mut self) {
        let mut events = vec![Event::ModifiersChanged(self.modifiers)];
        events.append(&mut self.events);
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.screen)),
            time: Some(self.time),
            events,
            dropped_files: self
                .dropped
                .drain(..)
                .map(|p| std::sync::Arc::new(Dropped(p)) as egui::DroppedFileHandle)
                .collect(),
            focused: self.focused,
            ..Default::default()
        };
        let close_asked = std::mem::take(&mut self.close_asked);
        if close_asked {
            let root = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
            root.events.push(egui::ViewportEvent::Close);
        }
        self.time += 1.0 / 60.0;
        eframe::App::raw_input_hook(&mut self.app, &self.ctx, &mut raw);
        let (app, frame) = (&mut self.app, &mut self.frame);
        let mut out = self.ctx.run_ui(raw, |ui| eframe::App::ui(app, ui, frame));
        out.textures_delta.clear();
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                self.copied.push(t.clone());
            }
        }
        if let Some(tree) = &out.platform_output.accesskit_update {
            self.widgets = widgets_of(tree);
            self.controls = controls_of(tree);
            self.focus = focused_of(tree);
        }
        let commands = |c: &egui::ViewportCommand| {
            out.viewport_output.values().any(|v| v.commands.contains(c))
        };
        // A window asked to close closes unless the program cancels it.
        self.closed |= commands(&egui::ViewportCommand::Close)
            || close_asked && !commands(&egui::ViewportCommand::CancelClose);
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    /// Runs frames until background work (compares, merges, reports) is done.
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.frame();
            if !self.busy() {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "{}: still busy after a minute",
                self.name
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        self.frames(3);
    }

    /// Runs frames until `done`, such as a background report being shown.
    fn wait_until(&mut self, what: &str, done: impl Fn(&Gui) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "{}: {what} not shown after a minute",
                self.name
            );
            std::thread::sleep(Duration::from_millis(1));
            self.frame();
        }
    }

    fn busy(&self) -> bool {
        self.app.report_job.is_some()
            || self.app.rev_load.is_some()
            || self.app.remote_load.is_some()
            || self.app.repack.is_some()
            || self
                .app
                .tabs
                .iter()
                .enumerate()
                .any(|(i, t)| match t.content.view() {
                    Content::Folder(v) => v.busy(),
                    // "All differences" reads its files in the background, taking them in only
                    // while it is shown.
                    Content::Diffs(v) => i == self.app.active && v.outcome() == Outcome::Busy,
                    _ => false,
                })
    }

    /// The topmost widget named `label`, with or without the key that runs it ("Open  (⌘O)").
    fn widget(&self, label: &str) -> &Widget {
        self.widgets
            .iter()
            .rev()
            .find(|w| named(w, label))
            .unwrap_or_else(|| panic!("{}: no widget {label:?}", self.name))
    }

    fn shows(&self, label: &str) -> bool {
        self.widgets.iter().any(|w| named(w, label))
    }

    fn shows_text(&self, part: &str) -> bool {
        self.widgets.iter().any(|w| w.label.contains(part))
    }

    fn click(&mut self, label: &str) {
        let w = self.widget(label);
        assert!(w.enabled, "{}: {label:?} is disabled", self.name);
        self.click_at(w.rect.center());
    }

    /// Clicks the widget of kind `role` named `label`, not a caption next to it.
    fn click_a(&mut self, role: accesskit::Role, label: &str) {
        let w = self
            .widgets
            .iter()
            .rev()
            .find(|w| w.role == role && named(w, label))
            .unwrap_or_else(|| panic!("{}: no {role:?} {label:?}", self.name));
        self.click_at(w.rect.center());
    }

    /// Opens or closes the menu of the menu bar named `label`, not a button of that name below.
    fn click_menu(&mut self, label: &str) -> Rect {
        let rect = self
            .widgets
            .iter()
            .find(|w| named(w, label) && w.rect.top() < 40.0)
            .unwrap_or_else(|| panic!("{}: no menu {label:?}", self.name))
            .rect;
        self.click_at(rect.center());
        rect
    }

    /// Picks an item of the menu bar: `["File", "Export", "Generate patch..."]`. Items are told
    /// from buttons of the same name by where they are: under their menu, or beside it. A menu
    /// too tall for the window under its button opens beside it instead.
    fn menu(&mut self, path: &[&str]) {
        let mut anchor = self.click_menu(path[0]);
        for (depth, item) in path.iter().enumerate().skip(1) {
            let found = self.widgets.iter().rev().find(|w| {
                let r = w.rect;
                let beside = r.left() >= anchor.right() - 4.0 && r.left() <= anchor.right() + 24.0;
                named(w, item)
                    && if depth == 1 {
                        ((anchor.left() - 2.0..=anchor.left() + 12.0).contains(&r.left())
                            && r.top() >= anchor.bottom())
                            || (beside && r.top() > anchor.top())
                    } else {
                        beside
                    }
            });
            let Some(w) = found else {
                panic!("{}: no item {item:?} in the menu", self.name);
            };
            assert!(w.enabled, "{}: {item:?} is disabled", self.name);
            anchor = w.rect;
            self.click_at(anchor.center());
        }
    }

    fn click_at(&mut self, at: Pos2) {
        self.events.push(Event::PointerMoved(at));
        self.frame();
        for pressed in [true, false] {
            self.events.push(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: self.modifiers,
            });
            self.frame();
        }
        self.settle();
    }

    /// Scrolls the view under the middle of the window until the widget `label` is in it: above
    /// the bottom edge and below the toolbars and headers over the list, unless the list goes no
    /// further. A list only draws the rows in view, so one not drawn yet is further down.
    fn scroll_to(&mut self, label: &str) {
        let middle = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0);
        let (top, bottom) = (SCREEN.y / 4.0, SCREEN.y - 40.0);
        for _ in 0..50 {
            let rect = self.resting_rect(label);
            let dy = if rect.is_none_or(|r| r.bottom() > bottom) {
                -120.0
            } else if rect.is_some_and(|r| r.top() < top) {
                120.0
            } else {
                return;
            };
            self.events.push(Event::PointerMoved(middle));
            self.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            });
            self.frames(3);
            if rect.is_some() && self.resting_rect(label) == rect {
                return;
            }
        }
        panic!("{}: could not scroll to {label:?}", self.name);
    }

    /// Where the widget `label` is once a scroll has ended, if it is drawn: egui spreads a wheel
    /// scroll over the next frames, and a click before it ends would miss the widget as it moves.
    fn resting_rect(&mut self, label: &str) -> Option<Rect> {
        let find = |gui: &Self| {
            gui.widgets
                .iter()
                .rev()
                .find(|w| named(w, label))
                .map(|w| w.rect)
        };
        let mut rect = find(self);
        for _ in 0..120 {
            self.frame();
            let now = find(self);
            if now == rect {
                break;
            }
            rect = now;
        }
        rect
    }

    fn key(&mut self, modifiers: Modifiers, key: Key) {
        self.modifiers = modifiers;
        for pressed in [true, false] {
            self.events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
            self.frame();
        }
        self.modifiers = Modifiers::NONE;
        self.settle();
    }

    /// Moves the keyboard focus on, as Tab does, without waiting for background work. egui
    /// hands the focus over on the frame after the key, so one more frame is run.
    fn tab(&mut self, back: bool) {
        let modifiers = if back {
            Modifiers::SHIFT
        } else {
            Modifiers::NONE
        };
        self.modifiers = modifiers;
        for pressed in [true, false] {
            self.events.push(Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
            self.frame();
        }
        self.modifiers = Modifiers::NONE;
        self.frames(2);
    }

    /// Tabs until the keyboard reaches a widget `wanted` accepts, naming where it went if it
    /// never does.
    fn tab_to(&mut self, what: &str, wanted: impl Fn(&Widget) -> bool) -> Widget {
        let mut seen = Vec::new();
        for _ in 0..200 {
            self.tab(false);
            let Some(f) = &self.focus else { continue };
            if wanted(f) {
                return f.clone();
            }
            let step = format!("{:?} {:?}", f.role, f.label);
            if seen.last() != Some(&step) {
                seen.push(step);
            }
        }
        panic!("{}: Tab never reached {what}; it went {seen:#?}", self.name);
    }

    fn type_text(&mut self, text: &str) {
        self.events.push(Event::Text(text.into()));
        self.frames(2);
    }

    fn drop_files(&mut self, paths: &[PathBuf]) {
        self.dropped.extend(paths.iter().cloned());
        self.settle();
    }

    /// Leaves the window and comes back, as switching to another program does.
    fn refocus(&mut self) {
        self.focused = false;
        self.frame();
        self.focused = true;
        self.settle();
    }

    fn text_view(&self) -> &TextView {
        match self.app.tabs.get(self.app.active).map(|t| t.content.view()) {
            Some(Content::Text(v)) => v,
            _ => panic!("{}: the active tab is not a text compare", self.name),
        }
    }

    fn toasts(&self) -> Vec<(bool, String)> {
        self.app
            .toasts
            .iter()
            .map(|t| (t.error, t.text.clone()))
            .collect()
    }

    fn errors(&self) -> Vec<String> {
        self.toasts()
            .into_iter()
            .filter_map(|(error, t)| error.then_some(t))
            .collect()
    }

    fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "test: {}", self.name);
        let _ = writeln!(out, "language: {}", i18n::current_language());
        for (i, t) in self.app.tabs.iter().enumerate() {
            let mark = if i == self.app.active { '*' } else { ' ' };
            let _ = writeln!(out, "tab{mark} {} | {}", t.title(), t.status_line());
        }
        for (error, t) in self.toasts() {
            let _ = writeln!(out, "toast{} {t}", if error { " (error)" } else { "" });
        }
        for w in &self.widgets {
            let _ = writeln!(
                out,
                "{:?} {:?} {:?}{}",
                w.role,
                w.label,
                w.rect,
                if w.enabled { "" } else { " disabled" }
            );
        }
        out
    }
}

/// When a test fails, keeps what the window showed for the report.
impl Drop for Gui {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
            .join("gui-tests");
        let file = dir.join(format!("{}.txt", self.name));
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&file, self.describe()).is_ok() {
            eprintln!("window state: {}", file.display());
        }
    }
}

/// What the keyboard is on and how a screen reader would announce it.
fn focused_of(tree: &accesskit::TreeUpdate) -> Option<Widget> {
    let nodes: std::collections::HashMap<accesskit::NodeId, &accesskit::Node> =
        tree.nodes.iter().map(|(id, n)| (*id, n)).collect();
    let n = nodes.get(&tree.focus)?;
    let caption = n
        .labelled_by()
        .iter()
        .find_map(|id| nodes.get(id).and_then(|c| c.label().or(c.value())));
    let b = n.bounds()?;
    Some(Widget {
        role: n.role(),
        label: n
            .label()
            .or(caption)
            .or(n.value())
            .or(n.placeholder())
            .unwrap_or("")
            .to_string(),
        rect: Rect::from_min_max(
            Pos2::new(b.x0 as f32, b.y0 as f32),
            Pos2::new(b.x1 as f32, b.y1 as f32),
        ),
        enabled: !n.is_disabled(),
        live: n.live(),
        toggled: n.toggled(),
    })
}

fn named(w: &Widget, label: &str) -> bool {
    w.label
        .strip_prefix(label)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("  ("))
}

/// Where the popups shown are, menus and their submenus among them.
fn popups(gui: &Gui) -> Vec<Rect> {
    gui.ctx.memory(|m| {
        m.layer_ids()
            .filter(|l| l.order == egui::Order::Foreground && m.areas().visible_last_frame(l))
            .filter_map(|l| m.area_rect(l.id))
            .collect()
    })
}

/// The buttons of the File menu's submenus, in sight or not.
fn file_submenus(gui: &Gui) -> impl Iterator<Item = &Widget> {
    let submenus = ["menu.projects_jobs", "menu.workspaces", "menu.export"];
    gui.widgets
        .iter()
        .filter(move |w| submenus.iter().any(|k| named(w, tr(k))))
}

/// Whether one of the File menu's submenus is at `at`, in sight or not.
fn file_submenu_at(gui: &Gui, at: Pos2) -> bool {
    file_submenus(gui).any(|w| w.rect.contains(at))
}

/// A point of `area` over one of the File menu's submenus, in sight or not. Not simply the
/// middle of `area`: where the buttons fall depends on the system's fonts.
fn file_submenu_within(gui: &Gui, area: Rect) -> Option<Pos2> {
    file_submenus(gui)
        .map(|w| w.rect.intersect(area))
        .find(|r| r.is_positive())
        .map(|r| r.center())
}

/// The named widgets of an accessibility tree, in drawing order.
fn widgets_of(tree: &accesskit::TreeUpdate) -> Vec<Widget> {
    let nodes: std::collections::HashMap<accesskit::NodeId, &accesskit::Node> =
        tree.nodes.iter().map(|(id, n)| (*id, n)).collect();
    let mut out = Vec::new();
    let mut stack = vec![tree.focus];
    if let Some(t) = &tree.tree {
        stack = vec![t.root];
    }
    let mut order = Vec::new();
    while let Some(id) = stack.pop() {
        let Some(n) = nodes.get(&id) else { continue };
        order.push(*n);
        stack.extend(n.children().iter().rev());
    }
    for n in order {
        // A combo box is named by the caption it is labelled by; plain text has its words as
        // the value.
        let caption = n
            .labelled_by()
            .iter()
            .find_map(|id| nodes.get(id).and_then(|c| c.label().or(c.value())));
        let (Some(label), Some(b)) = (n.label().or(caption).or(n.value()), n.bounds()) else {
            continue;
        };
        out.push(Widget {
            role: n.role(),
            label: label.to_string(),
            rect: Rect::from_min_max(
                Pos2::new(b.x0 as f32, b.y0 as f32),
                Pos2::new(b.x1 as f32, b.y1 as f32),
            ),
            enabled: !n.is_disabled(),
            live: n.live(),
            toggled: n.toggled(),
        });
    }
    out
}

/// The controls of an accessibility tree, with the name a screen reader gives each ("" for none).
fn controls_of(tree: &accesskit::TreeUpdate) -> Vec<Widget> {
    use accesskit::Role;
    let nodes: std::collections::HashMap<accesskit::NodeId, &accesskit::Node> =
        tree.nodes.iter().map(|(id, n)| (*id, n)).collect();
    tree.nodes
        .iter()
        .map(|(_, n)| n)
        .filter(|n| {
            n.supports_action(accesskit::Action::Click)
                || matches!(
                    n.role(),
                    Role::CheckBox | Role::RadioButton | Role::ComboBox | Role::Slider
                )
        })
        .filter_map(|n| {
            let caption = n
                .labelled_by()
                .iter()
                .find_map(|id| nodes.get(id).and_then(|c| c.label().or(c.value())));
            let b = n.bounds()?;
            Some(Widget {
                role: n.role(),
                label: n
                    .label()
                    .or(caption)
                    .or(n.value().filter(|v| !v.is_empty()))
                    .or(n.placeholder())
                    .unwrap_or("")
                    .to_string(),
                rect: Rect::from_min_max(
                    Pos2::new(b.x0 as f32, b.y0 as f32),
                    Pos2::new(b.x1 as f32, b.y1 as f32),
                ),
                enabled: !n.is_disabled(),
                live: n.live(),
                toggled: n.toggled(),
            })
        })
        .collect()
}

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Gives `p` a modification time of its own, as file systems may round close ones together.
fn touch_later(p: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(secs))
        .unwrap();
}

fn compare(paths: &[PathBuf]) -> Option<OpenRequest> {
    Some(OpenRequest {
        paths: paths.to_vec(),
        ..Default::default()
    })
}

const ALT: Modifiers = Modifiers::ALT;
const CMD: Modifiers = Modifiers::COMMAND;

#[test]
fn a_recent_path_picked_in_the_open_dialog_fills_its_field_and_closes_the_list() {
    let mut gui = Gui::start("open_dialog_recent", None);
    gui.app.cfg.recent_paths = vec!["/tmp/one.txt".into(), "/tmp/two.txt".into()];
    gui.key(CMD, Key::O);
    let open = |gui: &Gui| {
        [0, 2]
            .into_iter()
            .any(|s| egui::Popup::is_id_open(&gui.ctx, egui::Id::unique(("open_recent", s))))
    };
    gui.click(tr("label.recent"));
    gui.frames(2);
    assert!(open(&gui));
    gui.click("/tmp/two.txt");
    gui.frames(2);
    assert!(!open(&gui));
    let d = gui.app.open_dlg.as_ref().unwrap();
    assert!(d.paths.iter().any(|p| p == "/tmp/two.txt"), "{:?}", d.paths);
}

#[test]
fn files_dropped_on_the_open_dialog_are_compared_and_merged() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "one\n2\nthree\n")],
    );
    let mut gui = Gui::start("open_dialog", None);
    assert!(gui.app.tabs.is_empty());

    gui.key(CMD, Key::O);
    assert!(gui.app.open_dlg.is_some());
    gui.drop_files(&[d.join("a.txt"), d.join("b.txt")]);
    let paths = gui.app.open_dlg.as_ref().unwrap().paths.clone();
    assert_eq!(paths[0], d.join("a.txt").display().to_string());
    assert_eq!(paths[2], d.join("b.txt").display().to_string());
    gui.click(tr("btn.compare"));
    assert!(gui.app.open_dlg.is_none());
    assert_eq!(gui.app.tabs.len(), 1);
    assert_eq!(gui.text_view().diff.blocks.len(), 1);
    assert!(gui.shows(&gui.app.tabs[0].title()));

    // Keys move to the difference and copy it; the toolbar undoes and redoes.
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(gui.text_view().cur_block, Some(0));
    gui.key(ALT, Key::ArrowRight);
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "two");
    assert!(gui.text_view().diff.blocks.is_empty());
    gui.click(tr("cmd.undo"));
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "2");
    gui.click(tr("cmd.redo"));
    assert!(gui.app.tabs[0].is_modified());
    gui.key(CMD, Key::S);
    assert!(!gui.app.tabs[0].is_modified());
    assert_eq!(read(&d.join("b.txt")), "one\ntwo\nthree\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn three_way_conflicts_are_resolved_and_the_middle_saved() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left.txt", "a\nleft\nc\nd\ne\n"),
            ("base.txt", "a\nb\nc\nd\ne\n"),
            ("right.txt", "a\nright\nc\nd\nE\n"),
        ],
    );
    let paths = ["left.txt", "base.txt", "right.txt"].map(|f| d.join(f));
    let mut gui = Gui::start("three_way", compare(&paths));
    assert_eq!(gui.text_view().n(), 3);
    assert_eq!(gui.text_view().diff.blocks.len(), 2);

    gui.menu(&[tr("menu.merge"), tr("cmd.auto_merge")]);
    assert_eq!(gui.text_view().panes[1].doc.lines[4], "E");
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "b");

    gui.key(ALT, Key::PageDown);
    let b = gui.text_view().cur_block.expect("a conflict is selected");
    assert_eq!(gui.text_view().diff.blocks[b].kind, BlockKind::Conflict);
    gui.key(ALT, Key::ArrowRight);
    gui.key(CMD, Key::S);
    assert_eq!(read(&paths[1]), "a\nleft\nc\nd\nE\n");
    assert_eq!(read(&paths[0]), "a\nleft\nc\nd\ne\n");
    assert_eq!(read(&paths[2]), "a\nright\nc\nd\nE\n");
}

#[test]
fn the_first_start_offers_a_sample_that_leaves_no_trace() {
    let mut gui = Gui::start("start_sample", None);
    assert!(gui.app.cfg.recent_compares.is_empty());
    gui.click(tr("start.try_button"));
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(gui.shows("README.md") && gui.shows("src"));
    assert!(gui.app.cfg.recent_compares.is_empty());
    let root = gui.app.tabs[0].source[0].parent().unwrap().to_path_buf();
    assert!(root.exists());
    gui.key(CMD, Key::W);
    assert!(gui.app.tabs.is_empty());
    assert!(!root.exists(), "the sample is removed with its tab");
}

#[test]
fn the_recent_comparisons_are_cleared_from_the_start_page_and_the_file_menu() {
    let recent = vec![
        vec!["/tmp/a/old.txt".to_owned(), "/tmp/b/old.txt".to_owned()],
        vec!["/tmp/a/new.txt".to_owned(), "/tmp/b/new.txt".to_owned()],
    ];
    let cfg = Config {
        recent_compares: recent.clone(),
        ..config()
    };
    let mut gui = Gui::with_config("clear_recent", cfg, None);
    assert!(gui.shows_text("old.txt"));

    // the start page's short "Clear" is named in full for screen readers
    gui.click(tr("menu.clear_recent"));
    assert!(gui.app.cfg.recent_compares.is_empty());
    assert!(!gui.shows_text("old.txt"));
    assert!(gui.shows(tr("start.try_title")));
    assert_eq!(gui.toasts(), [(false, tr("msg.recent_cleared").to_owned())]);

    gui.click(tr("cmd.undo"));
    assert_eq!(gui.app.cfg.recent_compares, recent);
    assert!(gui.toasts().is_empty());
    assert!(gui.shows_text("old.txt"));

    // clearing again before the first toast is gone keeps both for one undo
    gui.click(tr("menu.clear_recent"));
    let newer = vec!["/tmp/a/newer.txt".to_owned(), "/tmp/b/newer.txt".to_owned()];
    gui.app.cfg.add_recent_compare(&newer);
    gui.settle();
    gui.click(tr("menu.clear_recent"));
    assert_eq!(gui.toasts().len(), 1);
    gui.click(tr("cmd.undo"));
    let mut all = vec![newer];
    all.extend(recent);
    assert_eq!(gui.app.cfg.recent_compares, all);

    gui.menu(&[
        tr("menu.file"),
        tr("menu.open_recent"),
        tr("menu.clear_recent"),
    ]);
    assert!(gui.app.cfg.recent_compares.is_empty());
    gui.click_menu(tr("menu.file"));
    assert!(!gui.widget(tr("menu.open_recent")).enabled);
}

/// Presses the key of command `cmd`, as the keyboard shortcuts list it.
fn press(gui: &mut Gui, cmd: crate::ui::commands::Cmd) {
    let b = crate::ui::commands::BINDINGS
        .iter()
        .find(|b| b.cmd == cmd)
        .unwrap_or_else(|| panic!("no key for {cmd:?}"));
    // A system key egui does not take itself (Alt+F4) reaches the program from the window
    // manager as a request to close the window.
    let egui_takes = gui.ctx.options(|o| o.quit_shortcuts.contains(&b.key));
    if matches!(b.scope, crate::ui::commands::Scope::System) && !egui_takes {
        gui.close_asked = true;
        gui.settle();
    } else {
        gui.key(b.key.modifiers, b.key.logical_key);
    }
}

/// The whole round of a comparison with no pointer at all: the paths typed on the start page,
/// the comparison started, a difference found, copied and saved, the tab closed and the
/// program left.
#[test]
fn a_comparison_runs_from_start_to_exit_with_the_keyboard_alone() {
    use crate::ui::commands::Cmd;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "same\nold\nsame\n"),
            ("b.txt", "same\nnew\nsame\n"),
        ],
    );
    let mut gui = Gui::start("keyboard_start_to_exit", None);
    let mut fields = Vec::new();
    for file in ["a.txt", "b.txt"] {
        let field = gui.tab_to("a path field not yet filled", |w| {
            w.role == accesskit::Role::TextInput && !fields.contains(&w.rect)
        });
        fields.push(field.rect);
        gui.type_text(&d.join(file).display().to_string());
        // A list of matching paths may open under the field; Escape closes only that.
        gui.key(Modifiers::NONE, Key::Escape);
    }
    assert_eq!(gui.app.tabs.len(), 0, "Escape in a field closes no compare");
    // The start page's button, below the fields, not the Compare menu of the menu bar.
    let below = fields[1].max.y;
    gui.tab_to("the Compare button", |w| {
        named(w, tr("btn.compare")) && w.rect.min.y > below
    });
    gui.key(Modifiers::NONE, Key::Enter);
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 1, "{}", gui.describe());
    assert_eq!(gui.text_view().panes[1].doc.lines, ["same", "new", "same"]);

    press(&mut gui, Cmd::NextDiff);
    assert!(gui.text_view().cur_block.is_some());
    press(&mut gui, Cmd::CopyRight);
    assert_eq!(gui.text_view().panes[1].doc.lines, ["same", "old", "same"]);
    press(&mut gui, Cmd::Save);
    assert_eq!(read(&d.join("b.txt")), "same\nold\nsame\n");
    assert_eq!(read(&d.join("a.txt")), "same\nold\nsame\n");

    gui.key(CMD, Key::W);
    assert_eq!(gui.app.tabs.len(), 0, "a saved tab closes without asking");
    assert!(!gui.closed);
    press(&mut gui, Cmd::Exit);
    gui.frames(3);
    assert!(gui.closed, "the exit key closes the window");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

/// A dialog opened from the keyboard gives the focus back to the control that opened it when
/// it closes, whether with Escape or its own button, so the keyboard carries on from there.
#[test]
fn closing_a_dialog_gives_the_focus_back_to_what_opened_it() {
    let mut gui = Gui::start("dialog_focus_back", None);
    let options = tr("menu.options").trim_end_matches(['.', '…']);
    let opener = gui.tab_to("the Options button", |w| named(w, options));
    for close in ["escape", "button"] {
        gui.key(Modifiers::NONE, Key::Enter);
        gui.frames(3);
        assert!(
            gui.app.options_dlg.is_some(),
            "{close}: Enter opens the dialog"
        );
        assert_ne!(
            gui.focus.as_ref().map(|f| f.rect),
            Some(opener.rect),
            "{close}: the focus moved into the dialog"
        );
        match close {
            "escape" => gui.key(Modifiers::NONE, Key::Escape),
            _ => {
                gui.tab_to("the dialog's Cancel button", |w| named(w, tr("btn.cancel")));
                gui.key(Modifiers::NONE, Key::Enter);
            }
        }
        gui.frames(3);
        assert!(gui.app.options_dlg.is_none(), "{close}: the dialog closed");
        let focus = gui.focus.clone();
        assert!(
            focus.as_ref().is_some_and(|f| f.rect == opener.rect),
            "{close}: the focus is on {:?}, not the Options button",
            focus.as_ref().map(|f| (f.role, f.label.clone()))
        );
    }
}

/// A snapshot left behind by a program that did not close normally comes back on *Restore*:
/// the comparison opens with its unsaved edit in its side, still unsaved, the files untouched,
/// and a file changed since is named.
#[test]
fn a_recovered_comparison_comes_back_unsaved_and_never_written() {
    use crate::core::recovery;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "same\nold\nsame\n"),
            ("b.txt", "same\nnew\nsame\n"),
        ],
    );
    let (a, b) = (d.join("a.txt"), d.join("b.txt"));
    let snaps = d.join("recovery");
    {
        let mut gui = Gui::start("recovery_before", compare(&[a.clone(), b.clone()]));
        gui.settle();
        press(&mut gui, crate::ui::commands::Cmd::NextDiff);
        press(&mut gui, crate::ui::commands::Cmd::CopyRight);
        assert!(gui.text_view().panes[1].doc.modified);
        if let Content::Text(v) = &mut gui.app.tabs[0].content {
            v.bookmarks[0].insert(2, "last".into());
            v.set_caret_line(1, 2);
        }
        let snapshot = gui.app.recovery_snapshot();
        assert_eq!(snapshot.tabs[0].caret, Some((1, 2)));
        // A tab left out of recovery is not in the snapshot.
        gui.app.tabs[0].no_recovery = true;
        assert!(gui.app.recovery_snapshot().tabs.is_empty());
        gui.app.tabs[0].no_recovery = false;
        assert_eq!(snapshot.unsaved_sides(), 1);
        assert_eq!(
            snapshot.tabs[0].unsaved[0], None,
            "the left side was not edited"
        );
        recovery::write(&snaps, "session-1-1.json", &snapshot).unwrap();
    }
    assert_eq!(
        read(&b),
        "same\nnew\nsame\n",
        "nothing saved before the crash"
    );
    write(d, &[("a.txt", "same\nold\nsame\nmore\n")]);

    let later = std::time::SystemTime::now() + recovery::ABANDONED_AFTER;
    let found = recovery::abandoned(&snaps, "session-2-2.json", later);
    assert_eq!(found.len(), 1);
    let mut gui = Gui::start("recovery_restore", None);
    // Every file counts as large, so an ordinary open would go to another thread: a restore
    // must still put the unsaved text back into the tab it opens.
    gui.app.background_open_bytes = 1;
    gui.app.recovery_offer = Some(RecoveryOffer {
        found,
        inspect: false,
    });
    gui.frames(2);
    assert!(gui.shows(tr("recovery.title")), "{}", gui.describe());
    gui.click(tr("recovery.inspect"));
    gui.frames(2);
    assert!(gui.shows_text(tr("recovery.changed")), "{}", gui.describe());
    gui.click(tr("recovery.restore"));
    gui.settle();

    assert!(gui.app.recovery_offer.is_none());
    assert_eq!(gui.app.tabs.len(), 1, "{}", gui.describe());
    let v = gui.text_view();
    assert_eq!(v.panes[1].doc.lines, ["same", "old", "same"]);
    assert!(v.panes[1].doc.modified, "the recovered edit is not saved");
    assert!(!v.panes[0].doc.modified);
    assert_eq!(v.panes[0].doc.lines, ["same", "old", "same", "more"]);
    assert_eq!(v.bookmarks[0].get(&2).map(String::as_str), Some("last"));
    assert_eq!(v.caret_line(), Some((1, 2)), "the caret where it was");
    assert_eq!(read(&b), "same\nnew\nsame\n", "restoring writes no file");
    assert!(
        gui.toasts()
            .iter()
            .any(|(warn, t)| *warn && t.contains(&a.display().to_string())),
        "the changed file is named: {:?}",
        gui.toasts()
    );
    assert!(
        !snaps.join("session-1-1.json").exists(),
        "a restored snapshot is gone"
    );
}

/// In every language, on a small window, the buttons of the recovery prompt and of the
/// Options dialog lie wholly inside the window and none covers another, so no translation
/// pushes a choice such as *Discard* out of sight.
#[test]
fn dialog_buttons_fit_a_small_window_in_every_language() {
    use crate::core::recovery;
    let small = egui::vec2(800.0, 600.0);
    let window = Rect::from_min_size(Pos2::ZERO, small);
    let snapshot = recovery::Snapshot::new(Vec::new(), 0, 0);
    for lang in i18n::languages().iter().map(|l| l.code.clone()) {
        let cfg = Config {
            language: lang.clone(),
            ..config()
        };
        let mut gui = Gui::with_config("dialog_buttons_fit", cfg, None);
        gui.screen = small;
        let check = |gui: &Gui, what: &str, labels: &[&str]| {
            let buttons: Vec<&Widget> = gui
                .widgets
                .iter()
                .filter(|w| w.role == accesskit::Role::Button && labels.contains(&w.label.as_str()))
                .collect();
            assert_eq!(
                buttons.len(),
                labels.len(),
                "{lang} {what}: {labels:?} not all shown\n{}",
                gui.describe()
            );
            for (i, b) in buttons.iter().enumerate() {
                assert!(
                    window.contains_rect(b.rect),
                    "{lang} {what}: {:?} at {:?} is outside the window",
                    b.label,
                    b.rect
                );
                for o in &buttons[i + 1..] {
                    assert!(
                        !b.rect.shrink(0.5).intersects(o.rect.shrink(0.5)),
                        "{lang} {what}: {:?} covers {:?}",
                        b.label,
                        o.label
                    );
                }
            }
        };

        gui.app.recovery_offer = Some(RecoveryOffer {
            found: vec![recovery::Found {
                path: "session-1-1.json".into(),
                snapshot: Ok(snapshot.clone()),
            }],
            inspect: false,
        });
        gui.frames(3);
        check(
            &gui,
            "recovery",
            &[
                tr("recovery.restore"),
                tr("recovery.inspect"),
                tr("recovery.discard"),
                tr("recovery.later"),
            ],
        );
        gui.app.recovery_offer = None;
        gui.frames(2);

        gui.app.open_options(OptTab::General);
        gui.frames(3);
        check(&gui, "options", &[tr("btn.ok"), tr("btn.cancel")]);
    }
}

/// In every language, each menu of the menu bar and each tab of the Options dialog fits an
/// 800×600 window: no item reaches past the window or its menu, and no two lie over one
/// another, however long the translation.
#[test]
fn menus_and_options_fit_a_small_window_in_every_language() {
    use accesskit::Role;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\n2\n")]);
    let small = egui::vec2(800.0, 600.0);
    let window = Rect::from_min_size(Pos2::ZERO, small).expand(1.0);
    let mut found = Vec::new();
    for lang in i18n::languages().iter().map(|l| l.code.clone()) {
        let cfg = Config {
            language: lang.clone(),
            ..config()
        };
        let mut gui = Gui::with_config(
            "menus_fit",
            cfg,
            compare(&[d.join("a.txt"), d.join("b.txt")]),
        );
        gui.screen = small;
        gui.settle();
        // What the window shows without a menu or dialog: covered by one, not part of it.
        let behind: Vec<(String, Rect)> = gui
            .controls
            .iter()
            .map(|w| (w.label.clone(), w.rect))
            .collect();
        let on_top = |w: &Widget| !behind.iter().any(|(l, r)| *l == w.label && *r == w.rect);
        for menu in MENUS {
            gui.click_menu(tr(menu));
            gui.frames(3);
            let shown = popups(&gui);
            let Some(&area) = shown.iter().max_by(|a, b| a.area().total_cmp(&b.area())) else {
                found.push(format!("{lang} {menu}: did not open"));
                continue;
            };
            if !window.contains_rect(area) {
                found.push(format!("{lang} {menu}: the menu at {area:?} is cut off"));
            }
            let items: Vec<&Widget> = gui
                .controls
                .iter()
                .filter(|w| {
                    matches!(
                        w.role,
                        Role::Button | Role::MenuItem | Role::CheckBox | Role::RadioButton
                    ) && w.rect.is_finite()
                        && w.rect.area() > 0.0
                        && area.contains(w.rect.center())
                        && on_top(w)
                })
                .collect();
            if items.len() < 2 {
                found.push(format!("{lang} {menu}: {} items", items.len()));
            }
            for (i, a) in items.iter().enumerate() {
                if a.rect.left() < area.left() - 1.0 || a.rect.right() > area.right() + 1.0 {
                    found.push(format!(
                        "{lang} {menu}: {:?} at {:?} reaches past the menu at {area:?}",
                        a.label, a.rect
                    ));
                }
                for b in &items[i + 1..] {
                    let both = a.rect.intersect(b.rect);
                    if both.width() > 2.0 && both.height() > 2.0 {
                        found.push(format!(
                            "{lang} {menu}: {:?} lies over {:?}",
                            a.label, b.label
                        ));
                    }
                }
            }
            gui.key(Modifiers::NONE, Key::Escape);
            gui.frames(3);
        }
        // The Rules tab is laid out differently with and without rules files: both are
        // checked, whatever files other tests left in the settings folder.
        let long_rules = vec![crate::core::rules::RuleSet {
            name: "Generated logs and build output of the nightly pipeline".into(),
            description: "Ignores timestamps, build numbers and absolute paths".into(),
            ..Default::default()
        }];
        let cases = OptTab::ALL.iter().flat_map(|&(tab, _, name)| {
            let rules: &[Option<&str>] = if matches!(tab, OptTab::Rules) {
                &[Some("no rules"), Some("rules")]
            } else {
                &[None]
            };
            rules.iter().map(move |&r| (tab, name, r))
        });
        for (tab, key, rules) in cases {
            let name = match rules {
                Some(r) => format!("{key} ({r})"),
                None => key.to_string(),
            };
            let behind: Vec<(String, Rect)> = gui
                .controls
                .iter()
                .map(|w| (w.label.clone(), w.rect))
                .collect();
            let on_top = |w: &Widget| !behind.iter().any(|(l, r)| *l == w.label && *r == w.rect);
            gui.app.open_options(tab);
            if let (Some(r), Some(d)) = (rules, gui.app.options_dlg.as_mut()) {
                d.rule_sets = if r == "rules" {
                    long_rules.clone()
                } else {
                    Vec::new()
                };
            }
            gui.frames(4);
            let Some(&dialog) = popups(&gui)
                .iter()
                .max_by(|a, b| a.area().total_cmp(&b.area()))
            else {
                found.push(format!("{lang} {name}: the Options dialog did not open"));
                continue;
            };
            if !window.contains_rect(dialog) {
                found.push(format!(
                    "{lang} {name}: the dialog at {dialog:?} is cut off"
                ));
            }
            // Only what is in the dialog: the window behind it is covered on purpose.
            let inside = gui
                .controls
                .iter()
                .filter(|w| dialog.contains(w.rect.center()) && on_top(w));
            crowded_among(inside, small.x, &format!("{lang} {name}"), &mut found);
            gui.app.options_dlg = None;
            gui.frames(2);
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

/// Reopening a saved session waits for each tab, large ones too: it counts what opened and
/// picks the tab to show from them.
#[test]
fn a_restored_session_opens_large_comparisons_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\n2\n")]);
    let mut gui = Gui::start("session_large", None);
    gui.app.background_open_bytes = 1;
    let tab = SessionTab {
        paths: vec![
            d.join("a.txt").display().to_string(),
            d.join("b.txt").display().to_string(),
        ],
        ..Default::default()
    };
    let (active, failed) = gui.app.open_saved_tabs(&[tab], 0);
    assert_eq!((active, failed), (Some(0), 0));
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(gui.app.opening.is_empty(), "nothing left on another thread");
}

/// Large files open on another thread: the window keeps drawing, a *Cancel* button shows,
/// and the tab opens once the comparison is done. A cancelled one never opens, not even after
/// a later comparison has.
#[test]
fn large_comparisons_open_in_the_background_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\nthree\n")]);
    // Big enough for the comparison to take a while in a test build.
    let big: String = (0..3_000_000).map(|i| format!("line {i}\n")).collect();
    write(
        d,
        &[
            ("big-a.txt", &big),
            ("big-b.txt", &big.replacen("line 7\n", "seven\n", 1)),
        ],
    );
    let mut gui = Gui::start("background_open", None);
    gui.app.background_open_bytes = 1;

    gui.app
        .open(compare(&[d.join("a.txt"), d.join("b.txt")]).unwrap());
    assert_eq!(gui.app.tabs.len(), 0, "not opened on the window's thread");
    assert_eq!(gui.app.opening.len(), 1);
    assert!(gui.app.busy(), "a running open counts as busy");
    let names = |a: &str, b: &str| {
        vec![
            d.join(a).display().to_string(),
            d.join(b).display().to_string(),
        ]
    };
    let recent = |g: &Gui, paths: &Vec<String>| g.app.cfg.recent_compares.contains(paths);
    assert!(
        !recent(&gui, &names("a.txt", "b.txt")),
        "remembered only once it opens"
    );
    gui.wait_until("the background comparison", |g| g.app.tabs.len() == 1);
    assert!(gui.app.opening.is_empty());
    assert_eq!(gui.text_view().panes[1].doc.lines, ["one", "three"]);
    assert!(recent(&gui, &names("a.txt", "b.txt")));

    gui.app
        .open(compare(&[d.join("big-a.txt"), d.join("big-b.txt")]).unwrap());
    gui.frames(2);
    assert!(gui.shows(tr("open.title")), "{}", gui.describe());
    let started = Instant::now();
    gui.click(tr("btn.cancel"));
    assert!(gui.app.opening.is_empty(), "cancelled at once");
    gui.app
        .open(compare(&[d.join("b.txt"), d.join("a.txt")]).unwrap());
    gui.wait_until("the later comparison", |g| g.app.tabs.len() == 2);
    // Long enough for the cancelled job to have finished, had it not stopped.
    while started.elapsed() < Duration::from_secs(4) {
        std::thread::sleep(Duration::from_millis(20));
        gui.frame();
    }
    assert_eq!(
        gui.app.tabs.len(),
        2,
        "the cancelled comparison never opens"
    );
    assert!(
        !recent(&gui, &names("big-a.txt", "big-b.txt")),
        "nor is it remembered"
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert!(!gui.shows(tr("open.title")));
}

/// A long session on large files: tabs switched every frame, closed and opened again, large
/// comparisons started and cancelled in the background, edits undone and redone. Nothing
/// fails, no file is written, and every tab ends as it began.
#[test]
fn a_long_session_of_switching_editing_and_cancelling_stays_consistent() {
    use crate::ui::commands::Cmd;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let side = |seed: usize| -> String {
        (0..40_000)
            .map(|i| {
                if i % 97 == seed {
                    format!("changed {i}\n")
                } else {
                    format!("line {i} of the file\n")
                }
            })
            .collect()
    };
    write(
        d,
        &[
            ("a.txt", &side(1)),
            ("b.txt", &side(2)),
            ("c.txt", &side(3)),
        ],
    );
    let pairs = [("a.txt", "b.txt"), ("b.txt", "c.txt"), ("a.txt", "c.txt")];
    let originals: Vec<String> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| read(&d.join(n)))
        .collect();
    let mut gui = Gui::start("long_session", None);
    for (l, r) in pairs {
        gui.app.open(compare(&[d.join(l), d.join(r)]).unwrap());
    }
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 3);
    // Each tab's lines, by its files: a tab opened again comes last.
    let lines_of = |gui: &Gui| -> std::collections::BTreeMap<Vec<PathBuf>, Vec<Vec<String>>> {
        gui.app
            .tabs
            .iter()
            .map(|t| match &t.content {
                Content::Text(v) => (
                    t.source.clone(),
                    v.panes.iter().map(|p| p.doc.lines.clone()).collect(),
                ),
                _ => panic!("a text tab"),
            })
            .collect()
    };
    let before = lines_of(&gui);

    for round in 0..12 {
        // Switching on every frame.
        for _ in 0..6 {
            press(&mut gui, Cmd::NextTab);
        }
        // Edits in the tab in front, undone and redone, then undone for good.
        let tab = gui.app.active;
        for _ in 0..3 {
            press(&mut gui, Cmd::NextDiff);
            press(&mut gui, Cmd::CopyRight);
        }
        assert!(gui.text_view().panes[1].doc.modified, "round {round}");
        for _ in 0..3 {
            press(&mut gui, Cmd::Undo);
        }
        for _ in 0..3 {
            press(&mut gui, Cmd::Redo);
        }
        for _ in 0..3 {
            press(&mut gui, Cmd::Undo);
        }
        assert_eq!(gui.app.active, tab);
        let source = gui.app.tabs[tab].source.clone();
        assert!(
            lines_of(&gui)[&source] == before[&source],
            "round {round}: {source:?} not undone to the start"
        );
        // A large comparison started in the background and cancelled.
        gui.app.background_open_bytes = 1;
        gui.app
            .open(compare(&[d.join("c.txt"), d.join("a.txt")]).unwrap());
        // Cancelled before a frame: a fast machine finishes it within one.
        let id = gui.app.opening.last().unwrap().id;
        gui.app.cancel_open(id);
        gui.frame();
        gui.app.background_open_bytes = BACKGROUND_OPEN_BYTES;
        // A tab closed and opened again, at the end.
        let (l, r) = pairs[round % 3];
        let at = gui
            .app
            .tabs
            .iter()
            .position(|t| t.source == [d.join(l), d.join(r)])
            .unwrap();
        gui.app.active = at;
        gui.frame();
        press(&mut gui, Cmd::CloseTab);
        assert_eq!(
            gui.app.tabs.len(),
            2,
            "round {round}: an unedited tab closes without asking"
        );
        gui.app.open(compare(&[d.join(l), d.join(r)]).unwrap());
        gui.frame();
        assert_eq!(gui.app.tabs.len(), 3);
    }
    gui.frames(30);
    assert_eq!(gui.app.tabs.len(), 3, "no cancelled comparison opened late");
    assert!(lines_of(&gui) == before, "every tab as it began");
    let now: Vec<String> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| read(&d.join(n)))
        .collect();
    assert_eq!(now, originals, "no file written");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

/// A menu opened from the keyboard and closed with Escape leaves the focus on its button, so
/// the keyboard carries on from there.
#[test]
fn closing_a_menu_with_escape_leaves_the_focus_on_its_button() {
    let mut gui = Gui::start("menu_focus_back", None);
    for menu in [tr("menu.file"), tr("menu.edit"), tr("menu.help")] {
        let button = gui.tab_to(menu, |w| {
            named(w, menu) && w.role == accesskit::Role::Button
        });
        gui.key(Modifiers::NONE, Key::Enter);
        gui.frames(3);
        assert!(!popups(&gui).is_empty(), "{menu}: Enter opens it");
        gui.key(Modifiers::NONE, Key::Escape);
        gui.frames(3);
        assert!(popups(&gui).is_empty(), "{menu}: Escape closes it");
        let focus = gui.focus.clone();
        assert!(
            focus.as_ref().is_some_and(|f| f.rect == button.rect),
            "{menu}: the focus is on {:?}",
            focus.as_ref().map(|f| (f.role, f.label.clone()))
        );
    }
}

/// A command run from a menu that puts the focus somewhere (Find, into its field) keeps it
/// there: the menu's button does not take it back.
#[test]
fn a_menu_command_that_moves_the_focus_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\n2\n")]);
    let mut gui = Gui::start(
        "menu_command_focus",
        compare(&[d.join("a.txt"), d.join("b.txt")]),
    );
    gui.settle();
    let find = tr(crate::ui::commands::Cmd::Find.label()).to_string();
    gui.menu(&[tr("menu.edit"), &find]);
    gui.frames(5);
    let focus = gui.focus.clone();
    assert!(
        focus
            .as_ref()
            .is_some_and(|f| f.role == accesskit::Role::TextInput),
        "the find field keeps the focus, not {:?}",
        focus.as_ref().map(|f| (f.role, f.label.clone()))
    );
}

#[test]
fn the_start_page_compares_the_typed_paths() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let mut gui = Gui::start("start_launcher", None);
    assert!(!gui.widget(tr("btn.compare")).enabled);
    let fields: Vec<_> = gui
        .widgets
        .iter()
        .filter(|w| w.role == accesskit::Role::TextInput)
        .map(|w| w.rect.center())
        .collect();
    assert_eq!(fields.len(), 2);
    for (at, file) in fields.into_iter().zip(["a.txt", "b.txt"]) {
        gui.click_at(at);
        gui.type_text(&d.join(file).display().to_string());
        gui.frames(2);
    }
    gui.click(tr("btn.compare"));
    assert_eq!(gui.app.tabs.len(), 1);
    assert_eq!(gui.text_view().panes[1].doc.lines, ["two"]);
}

#[test]
fn the_inline_view_keeps_navigation_and_copying() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "one\n2\nthree\n")],
    );
    let mut gui = Gui::start("inline_view", compare(&[d.join("a.txt"), d.join("b.txt")]));
    gui.click(tr("cmd.inline_view"));
    assert!(gui.text_view().unified);
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(gui.text_view().cur_block, Some(0));
    gui.key(ALT, Key::ArrowRight);
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "two");
    gui.click(tr("cmd.inline_view"));
    assert!(!gui.text_view().unified);

    gui.key(CMD, Key::K);
    gui.type_text(tr("cmd.inline_view"));
    gui.key(Modifiers::NONE, Key::Enter);
    assert!(gui.text_view().unified);
}

#[test]
fn the_inline_view_moves_the_cursor_but_does_not_edit() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let left: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let right = left
        .replace("line 2\n", "two\n")
        .replace("line 39\n", "thirty-nine\n");
    write(d, &[("a.txt", &left), ("b.txt", &right)]);
    let mut gui = Gui::start(
        "inline_view_keys",
        compare(&[d.join("a.txt"), d.join("b.txt")]),
    );
    gui.key(Modifiers::NONE, Key::ArrowDown);
    gui.key(Modifiers::SHIFT, Key::ArrowDown);
    gui.click(tr("cmd.inline_view"));
    let before = gui.text_view().panes[1].doc.text();
    // Neither pasting over the selection nor Enter edits; the arrow keys still move.
    gui.events.push(Event::Paste("x\n".into()));
    gui.frames(2);
    assert_eq!(gui.text_view().panes[1].doc.text(), before);
    gui.key(Modifiers::NONE, Key::Enter);
    let row = gui.text_view().cursor_row.unwrap();
    gui.key(Modifiers::NONE, Key::ArrowDown);
    assert!(gui.text_view().cursor_row.unwrap() > row);
    gui.key(CMD, Key::End);
    assert_eq!(gui.text_view().cursor_row, Some(39));
    assert_eq!(gui.text_view().panes[1].doc.text(), before);
}

#[test]
fn the_merge_bar_counts_conflicts_down_to_a_saved_result() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left.txt", "a\nleft\nc\nd\ne\nL2\n"),
            ("base.txt", "a\nb\nc\nd\ne\nb2\n"),
            ("right.txt", "a\nright\nc\nd\ne\nR2\n"),
        ],
    );
    let paths = ["left.txt", "base.txt", "right.txt"].map(|f| d.join(f));
    let mut gui = Gui::start("merge_bar", compare(&paths));
    let left = |n: usize| trf("merge.conflicts_left", &[("n", n.to_string())]);
    assert!(gui.shows_text(&left(2)));
    assert!(!gui.widget(tr("merge.save_result")).enabled);

    gui.click(tr("merge.next_conflict"));
    gui.click(tr("merge.take_both"));
    assert_eq!(gui.text_view().panes[1].doc.lines[5..7], ["L2", "R2"]);
    let one = trf("merge.conflicts_left_one", &[("n", "1".into())]);
    assert!(gui.shows_text(&one));

    gui.click(tr("merge.next_conflict"));
    let b = gui.text_view().cur_block.unwrap();
    assert!(gui.text_view().is_open_conflict(b));
    gui.click(tr("merge.mark_resolved"));
    assert!(gui.shows_text(tr("merge.no_conflicts")));
    assert_eq!(gui.text_view().conflicts_left(), 0);

    gui.click(tr("merge.save_result"));
    assert_eq!(read(&paths[1]), "a\nb\nc\nd\ne\nL2\nR2\n");
    assert!(!gui.app.tabs[0].is_modified());
}

/// `git mergetool` runs the configured tool and waits for it; the tool here notes the files Git
/// passes and waits for the test, which opens them as `-o "$MERGED" --wl --wr` would.
#[test]
fn git_mergetool_conflicts_are_resolved_into_the_merged_file() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", dir.path().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let commit = |file: &str, text: &str, message: &str| {
        std::fs::write(repo.join(file), text).unwrap();
        assert!(git(&["add", file]).status.success());
        assert!(git(&["commit", "-qm", message]).status.success());
    };
    assert!(git(&["init", "-q"]).status.success());
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.invalid"),
        ("core.autocrlf", "false"),
        ("mergetool.keepBackup", "false"),
        ("mergetool.oxm.trustExitCode", "false"),
        (
            "mergetool.oxm.cmd",
            "printf '%s\\n' \"$LOCAL\" \"$BASE\" \"$REMOTE\" \"$MERGED\" > ../paths.tmp && \
             mv ../paths.tmp ../paths && while [ ! -f ../done ]; do sleep 0.05; done",
        ),
    ] {
        assert!(git(&["config", key, value]).status.success());
    }
    let base = "a\nb\nc\nd\ne\nf\ng\nh\n";
    commit("file.txt", base, "base");
    assert!(git(&["checkout", "-qb", "theirs"]).status.success());
    commit("file.txt", "a\nTHEIRS\nc\nd\nTHEIRS-E\nf\ng\nh\n", "theirs");
    assert!(git(&["checkout", "-q", "-"]).status.success());
    commit("file.txt", "a\nOURS\nc\nd\ne\nf\nOURS-G\nh\n", "ours");
    assert!(!git(&["merge", "-q", "theirs"]).status.success());
    assert!(read(&repo.join("file.txt")).contains("<<<<<<<"));

    let mut tool = std::process::Command::new("git")
        .args(["mergetool", "--tool=oxm", "--no-prompt", "file.txt"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", dir.path().join(".gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    struct Release<'a>(&'a Path, &'a mut std::process::Child);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("done"), "");
            let start = Instant::now();
            while self.1.try_wait().ok().flatten().is_none() {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = self.1.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let release = Release(dir.path(), &mut tool);
    let start = Instant::now();
    let listed = loop {
        if let Ok(s) = std::fs::read_to_string(dir.path().join("paths")) {
            break s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "git mergetool never ran the tool"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let backup = Instant::now();
    let paths: Vec<PathBuf> = listed.lines().map(|p| repo.join(p)).collect();
    assert_eq!(paths.len(), 4, "{listed}");
    let (base_file, merged) = (paths[1].clone(), paths[3].clone());
    assert_eq!(read(&base_file), base);

    let mut gui = Gui::start(
        "git_mergetool",
        Some(OpenRequest {
            paths: paths[..3].to_vec(),
            descriptions: vec![Some("Local".into()), None, Some("Remote".into())],
            read_only: vec![true, false, true],
            output: Some(merged.clone()),
            ..Default::default()
        }),
    );
    gui.menu(&[tr("menu.merge"), tr("cmd.auto_merge")]);
    gui.click(tr("merge.next_conflict"));
    gui.click(tr("merge.take_both"));
    assert_eq!(gui.text_view().conflicts_left(), 0);
    // Git takes the merge as done when the result is newer than its backup, which some shells
    // compare in whole seconds.
    while backup.elapsed() < Duration::from_millis(1100) {
        std::thread::sleep(Duration::from_millis(50));
    }
    gui.click(tr("merge.save_result"));
    let expected = "a\nOURS\nTHEIRS\nc\nd\nTHEIRS-E\nf\nOURS-G\nh\n";
    assert_eq!(read(&merged), expected);
    assert_eq!(read(&base_file), base, "the common base was changed");
    drop(gui);
    drop(release);

    let unmerged = git(&["diff", "--name-only", "--diff-filter=U"]);
    assert!(unmerged.stdout.is_empty(), "Git still sees a conflict");
    let staged = git(&["show", ":file.txt"]);
    assert_eq!(String::from_utf8_lossy(&staged.stdout), expected);
    assert_eq!(read(&repo.join("file.txt")), expected);
}

#[test]
fn a_notebook_merged_into_an_output_file_is_merged_as_the_file_itself() {
    let dir = tempfile::tempdir().unwrap();
    let cell = |src: &str| {
        format!(
            r#"{{"cells":[{{"cell_type":"code","source":["{src}"],"outputs":[]}}],"nbformat":4,"nbformat_minor":5}}"#
        )
    };
    let (ours, base) = (cell("print(1)"), cell("print(0)"));
    write(
        dir.path(),
        &[
            ("local.ipynb", ours.as_str()),
            ("base.ipynb", base.as_str()),
            ("remote.ipynb", base.as_str()),
        ],
    );
    let merged = dir.path().join("merged.ipynb");
    std::fs::write(&merged, &base).unwrap();
    let mut gui = Gui::start(
        "notebook_mergetool",
        Some(OpenRequest {
            paths: ["local", "base", "remote"]
                .map(|n| dir.path().join(format!("{n}.ipynb")))
                .to_vec(),
            read_only: vec![true, false, true],
            output: Some(merged.clone()),
            ..Default::default()
        }),
    );
    let v = gui.text_view();
    for p in 0..3 {
        assert!(!v.panes[p].doc.compare_only, "pane {p} was read as cells");
    }
    assert!(!v.panes[1].doc.read_only);
    gui.menu(&[tr("menu.merge"), tr("cmd.auto_merge")]);
    gui.click(tr("merge.save_result"));
    let saved: serde_json::Value = serde_json::from_str(&read(&merged)).unwrap();
    assert_eq!(saved["cells"][0]["source"][0], "print(1)");
}

#[test]
fn text_read_out_of_a_notebook_is_never_saved_over_it() {
    let dir = tempfile::tempdir().unwrap();
    let nb = r#"{"cells":[{"cell_type":"code","source":["x"],"outputs":[]}],"nbformat":4}"#;
    write(dir.path(), &[("a.ipynb", nb), ("b.ipynb", nb)]);
    let mut docs: Vec<TextDoc> = ["a", "b"]
        .iter()
        .map(|n| {
            crate::core::unpacker::load_doc(
                &dir.path().join(format!("{n}.ipynb")),
                &Default::default(),
                None,
            )
            .unwrap()
        })
        .collect();
    let out = dir.path().join("b.ipynb");
    crate::ui::text_view::redirect_output(&mut docs, &out);
    assert!(
        docs[1].read_only,
        "an output side read as cells became writable"
    );
    let mut v = TextView::new(docs, &crate::config::Config::default());
    v.panes[1].doc.read_only = false;
    v.panes[1].doc.modified = true;
    assert!(v.save_pane(1).is_err());
    assert_eq!(read(&out), nb);
}

fn merge_folders(d: &Path) -> [PathBuf; 3] {
    write(
        d,
        &[
            ("left/same.txt", "same\n"),
            ("left/changed.txt", "left\n"),
            ("left/both.txt", "left\n"),
            ("left/gone.txt", "gone\n"),
            ("mid/same.txt", "same\n"),
            ("mid/changed.txt", "base\n"),
            ("mid/both.txt", "base\n"),
            ("mid/gone.txt", "gone\n"),
            ("right/same.txt", "same\n"),
            ("right/changed.txt", "base\n"),
            ("right/both.txt", "right\n"),
            ("right/sub/added.txt", "added\n"),
        ],
    );
    ["left", "mid", "right"].map(|f| d.join(f))
}

fn compare_folders(paths: &[PathBuf]) -> Option<OpenRequest> {
    Some(OpenRequest {
        paths: paths.to_vec(),
        recursive: Some(true),
        ..Default::default()
    })
}

#[test]
fn folder_merges_are_previewed_then_carried_out() {
    let dir = tempfile::tempdir().unwrap();
    let roots = merge_folders(dir.path());
    let mut gui = Gui::start("folder_merge", compare_folders(&roots));
    assert!(matches!(gui.app.tabs[0].content, Content::Folder(_)));
    let ascending = format!("{}, {}", tr("col.name"), tr("col.sorted_ascending"));
    assert!(gui.shows(&ascending), "the sort is not announced");
    gui.click(&ascending);
    assert!(gui.shows(&format!(
        "{}, {}",
        tr("col.name"),
        tr("col.sorted_descending")
    )));

    gui.menu(&[tr("menu.merge"), tr("cmd.folder_merge")]);
    let plan = trf(
        "msg.folder_merge_plan",
        &[
            ("copies", "3".into()),
            ("deletes", "1".into()),
            ("merges", "0".into()),
        ],
    );
    assert!(gui.shows(&plan), "the merge is not previewed");
    assert!(gui.shows_text("gone.txt"), "the deletion is not listed");
    assert!(gui.shows_text("both.txt"), "the conflict is not listed");
    let mid = &roots[1];
    assert_eq!(read(&mid.join("changed.txt")), "base\n", "merged before OK");

    gui.click(tr("btn.ok"));
    assert_eq!(read(&mid.join("changed.txt")), "left\n");
    assert_eq!(read(&mid.join("sub/added.txt")), "added\n");
    assert!(!mid.join("gone.txt").exists());
    assert_eq!(read(&mid.join("both.txt")), "base\n");
    let status = gui.app.tabs[0].status_line();
    assert!(
        status.contains(&trf("msg.op_done", &[("n", "4".into())])),
        "{status}"
    );
    assert!(
        !status.contains(&format!(" — {}:", tr("status.error"))),
        "{status}"
    );
}

#[test]
fn folders_are_synchronized_after_a_preview_and_a_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "new\n"),
            ("left/sub/b.txt", "b\n"),
            ("right/a.txt", "old\n"),
            ("right/extra.txt", "extra\n"),
        ],
    );
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::start("folder_sync", compare_folders(&roots));
    gui.wait_until("the compare", |g| !g.busy());
    gui.click(tr("cmd.sync_folders"));
    gui.wait_until("the preview", |g| {
        g.shows_text(tr("folder_sync.preview_only"))
    });
    let summary = trf(
        "folder_sync.summary",
        &[
            ("dirs", "1".into()),
            ("copies", "1".into()),
            ("replaces", "1".into()),
            ("deletes", "1".into()),
            ("bytes", crate::core::report::fmt_size(Some(6))),
        ],
    );
    // the preview is shown while the files are still being read
    gui.wait_until("the plan summary", |g| g.shows(&summary));
    assert!(gui.shows_text("extra.txt"), "the deletion is not listed");
    assert_eq!(
        read(&roots[1].join("a.txt")),
        "old\n",
        "changed by the preview"
    );

    gui.click(tr("folder_sync.apply"));
    let Content::Folder(v) = gui.app.tabs[0].content.view() else {
        panic!("not a folder compare");
    };
    let confirm = trf(
        "folder_sync.confirm",
        &[("n", "4".into()), ("root", v.display_roots[1].clone())],
    );
    assert!(gui.shows(&confirm), "the destination is not named");
    assert_eq!(
        read(&roots[1].join("a.txt")),
        "old\n",
        "changed before confirming"
    );
    gui.click(tr("folder_sync.apply_now"));
    let done = trf("folder_sync.done", &[("n", "4".into())]);
    gui.wait_until("the outcome", |g| g.shows(&done));
    // the dialog takes its new size on the next frame
    gui.frames(2);
    assert_eq!(read(&roots[1].join("a.txt")), "new\n");
    assert_eq!(read(&roots[1].join("sub/b.txt")), "b\n");
    assert!(!roots[1].join("extra.txt").exists());
    assert_eq!(read(&roots[0].join("a.txt")), "new\n");
    gui.click(tr("btn.close"));
    gui.wait_until("the compare again", |g| !g.busy());
    assert!(!gui.shows(&done));
}

#[test]
fn folder_merges_leave_alone_what_changed_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let roots = merge_folders(dir.path());
    let mut gui = Gui::start("folder_merge_stale", compare_folders(&roots));

    // An item gone since the compare stops the merge until compared again.
    std::fs::remove_file(roots[0].join("gone.txt")).unwrap();
    gui.menu(&[tr("menu.merge"), tr("cmd.folder_merge")]);
    let stale = trf(
        "msg.folder_merge_stale",
        &[("n", "1".into()), ("file", "gone.txt".into())],
    );
    let status = gui.app.tabs[0].status_line();
    assert!(status.contains(&stale), "{status}");
    assert!(!gui.shows(tr("btn.ok")), "a merge was offered");

    // A file changed between the preview and OK is named, and nothing is merged.
    gui.key(Modifiers::NONE, Key::F5);
    gui.click(tr("cmd.folder_merge"));
    let target = roots[1].join("changed.txt");
    std::fs::write(&target, "mine\n").unwrap();
    touch_later(&target, 5);
    gui.click(tr("btn.ok"));
    assert_eq!(read(&target), "mine\n");
    assert!(!roots[1].join("sub/added.txt").exists());
    assert!(roots[1].join("gone.txt").exists());
    let status = gui.app.tabs[0].status_line();
    assert!(
        status.contains(tr("status.error"))
            && status.contains("changed.txt")
            && status.contains(tr("msg.folder_merge_not_applied")),
        "{status}"
    );
}

#[test]
fn formatted_web_pages_step_on_to_the_next_file() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/one.html", "<p>one</p><p>left</p>"),
            ("right/one.html", "<p>one</p><p>right</p>"),
            ("left/two.html", "<p>two</p><p>left</p>"),
            ("right/two.html", "<p>two</p><p>right</p>"),
        ],
    );
    let cfg = config();
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::with_config("web_step_file", cfg, compare_folders(&roots));
    let shown = |gui: &mut Gui| {
        let start = Instant::now();
        loop {
            gui.frame();
            if let Content::Web(w) = &gui.app.tabs[gui.app.active].content {
                if w.formatted.is_some() {
                    return w.urls[0].path().to_string();
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the pages never loaded"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    gui.click("one.html");
    gui.key(Modifiers::NONE, Key::Enter);
    assert!(shown(&mut gui).ends_with("/one.html"));
    gui.key(ALT, Key::ArrowDown);
    gui.key(ALT, Key::ArrowDown);
    assert!(shown(&mut gui).ends_with("/two.html"));
}

/// A Word document and a spreadsheet, built as the real formats are, so that the window is
/// driven over files OxideMerge has to read for itself.
#[cfg(test)]
fn zipped_document(path: &Path, parts: &[(&str, String)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (entry, text) in parts {
        zip.start_file(*entry, options).unwrap();
        std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

/// Documents and spreadsheets are compared as the text in them, without another program, and
/// what is shown cannot be written back over the file it was read from.
#[test]
fn documents_and_spreadsheets_are_compared_as_their_text_and_stay_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let document = |second: &str| {
        (
            "word/document.xml",
            format!(
                "<w:document xmlns:w=\"{w}\"><w:body>\
                 <w:p><w:r><w:t>Shared opening line</w:t></w:r></w:p>\
                 <w:p><w:r><w:t>{second}</w:t></w:r></w:p>\
                 </w:body></w:document>"
            ),
        )
    };
    zipped_document(&d.join("left.docx"), &[document("the left wording")]);
    zipped_document(&d.join("right.docx"), &[document("the right wording")]);

    let mut gui = Gui::start(
        "extract_docx",
        compare(&[d.join("left.docx"), d.join("right.docx")]),
    );
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let v = gui.text_view();
    assert_eq!(
        v.panes[0].doc.text(),
        "Shared opening line\nthe left wording\n"
    );
    assert_eq!(
        v.panes[1].doc.text(),
        "Shared opening line\nthe right wording\n"
    );
    // Read from the file, not the file itself: it cannot be edited or saved over.
    assert!(v.panes.iter().all(|p| p.doc.read_only), "not read-only");
    assert!(
        v.panes[0]
            .doc
            .description
            .as_ref()
            .is_some_and(|t| t.contains(tr("label.extracted_document"))),
        "{:?}",
        v.panes[0].doc.description
    );
    // One block differs, which is the point of reading the words rather than the bytes.
    assert_eq!(v.diff.significant_blocks(), 1, "{} rows", v.diff.rows.len());
}

#[test]
fn spreadsheets_are_compared_as_tables() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rels = "http://schemas.openxmlformats.org/package/2006/relationships";
    let book = |amount: &str| {
        vec![
            (
                "xl/workbook.xml",
                format!(
                    "<workbook xmlns=\"{ns}\" xmlns:r=\"{rels}\"><sheets>\
                     <sheet name=\"Prices\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                format!(
                    "<Relationships xmlns=\"{rels}\">\
                     <Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>"
                ),
            ),
            (
                "xl/sharedStrings.xml",
                format!("<sst xmlns=\"{ns}\"><si><t>item</t></si><si><t>price</t></si>\
                         <si><t>apple</t></si></sst>"),
            ),
            (
                "xl/worksheets/sheet1.xml",
                format!(
                    "<worksheet xmlns=\"{ns}\"><sheetData>\
                     <row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>1</v></c></row>\
                     <row r=\"2\"><c r=\"A2\" t=\"s\"><v>2</v></c><c r=\"B2\"><v>{amount}</v></c></row>\
                     </sheetData></worksheet>"
                ),
            ),
        ]
    };
    for (name, amount) in [("left.xlsx", "10"), ("right.xlsx", "12")] {
        zipped_document(&d.join(name), &book(amount));
    }
    let mut gui = Gui::start(
        "extract_xlsx",
        compare(&[d.join("left.xlsx"), d.join("right.xlsx")]),
    );
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let v = gui.text_view();
    assert_eq!(v.panes[0].doc.text(), "item,price\napple,10\n");
    assert!(v.panes.iter().all(|p| p.doc.read_only));
    // A spreadsheet is read as records, so the compare opens as a table with its columns.
    assert_eq!(v.table_delim, Some(','), "not compared as a table");
}

/// A page of tables is far taller than the estimate a row starts with, so scrolling through it
/// measures rows and moves everything below them. The difference marks beside the pages must
/// stay where they are all the same.
#[test]
fn the_formatted_overview_does_not_move_while_the_pages_are_scrolled() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let table = |n: usize, last: &str| {
        let mut t = String::from("<table>\n");
        for r in 0..n {
            let cell = if r + 1 == n { last } else { "same" };
            t.push_str(&format!("<tr><td>row {r}</td><td>{cell}</td></tr>\n"));
        }
        t.push_str("</table>\n");
        t
    };
    let page = |last: &str| {
        let mut p = String::from("<!DOCTYPE html>\n<html><body>\n<p>first</p>\n");
        for _ in 0..6 {
            p.push_str(&table(20, "same"));
            p.push_str("<p>between the tables</p>\n");
        }
        p.push_str(&table(20, last));
        p.push_str("<p>last</p>\n</body></html>\n");
        p
    };
    write(
        d,
        &[
            ("left/tables.html", &page("left")),
            ("right/tables.html", &page("right")),
        ],
    );
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::with_config("formatted_overview", config(), compare_folders(&roots));
    gui.click("tables.html");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.wait_until(
        "the pages",
        |g| matches!(&g.app.tabs[g.app.active].content, Content::Web(w) if w.formatted.is_some()),
    );
    gui.frames(5);
    let c = crate::ui::theme::colors(false);
    let marks = |g: &Gui| {
        let Content::Web(w) = &g.app.tabs[g.app.active].content else {
            panic!("not a web compare");
        };
        let v = w.formatted.as_ref().expect("no formatted view");
        v.location_marks(&c)
            .iter()
            .map(|m| (m.start, m.len))
            .collect::<Vec<_>>()
    };
    let before = marks(&gui);
    assert!(!before.is_empty(), "no difference was marked");

    // Scroll to the end, which lays out and measures every table on the way.
    let middle = Pos2::new(SCREEN.x / 2.0, SCREEN.y / 2.0);
    for _ in 0..40 {
        gui.events.push(Event::PointerMoved(middle));
        gui.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        });
        gui.frames(2);
    }
    gui.frames(5);
    let Content::Web(w) = &gui.app.tabs[gui.app.active].content else {
        panic!("not a web compare");
    };
    let v = w.formatted.as_ref().unwrap();
    assert!(
        v.heights.iter().any(|&h| h > 200.0),
        "no table was measured, so nothing was proved: {:?}",
        &v.heights[..v.heights.len().min(8)]
    );
    assert_eq!(
        marks(&gui),
        before,
        "the overview moved as the pages scrolled"
    );
}

/// HTML is not one of the formats the Semantic and Tree views can read, so a compare of HTML
/// source must not offer them.
#[test]
fn html_source_does_not_offer_the_semantic_and_tree_views() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let page = |head: &str, cell: &str| {
        format!("{head}<html><body>\n<table><tr><td>{cell}</td></tr></table>\n</body></html>\n")
    };
    // A plain page, and one opening with an XML declaration, as an XHTML page does.
    let heads = [
        ("plain", "<!DOCTYPE html>\n"),
        (
            "xhtml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n",
        ),
    ];
    for (name, head) in heads {
        write(
            d,
            &[
                (&format!("left/{name}.html"), &page(head, "left")),
                (&format!("right/{name}.html"), &page(head, "right")),
            ],
        );
    }
    let roots = [d.join("left"), d.join("right")];
    for (name, _) in heads {
        let mut gui = Gui::with_config("html_source_tabs", config(), compare_folders(&roots));
        gui.click(&format!("{name}.html"));
        gui.key(Modifiers::NONE, Key::Enter);
        gui.wait_until("the pages", |g| {
            matches!(&g.app.tabs[g.app.active].content, Content::Web(w) if w.formatted.is_some())
        });
        gui.click(tr("web.html"));
        gui.wait_until("the HTML source", |g| {
            matches!(&g.app.tabs[g.app.active].content,
                     Content::Web(w) if matches!(w.inner.as_deref(), Some(Content::Text(_))))
        });
        assert!(
            !gui.shows(tr("analysis.semantic")),
            "{name}.html source offers the Semantic view"
        );
        assert!(
            !gui.shows(tr("analysis.tree")),
            "{name}.html source offers the Tree view"
        );
        // The source itself is still there to compare as text.
        let Content::Web(w) = &gui.app.tabs[gui.app.active].content else {
            panic!("{name}.html is not a web compare");
        };
        let Some(Content::Text(v)) = w.inner.as_deref() else {
            panic!("{name}.html has no text compare");
        };
        assert!(
            v.panes[0].doc.text().contains("<td>left</td>"),
            "{name}.html lost its source"
        );
    }
}

#[test]
fn the_docked_file_compare_steps_through_differences_and_on_to_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "1\nx\n3\n4\n5\n6\n7\n8\ny\n10\n"),
            ("right/a.txt", "1\nX\n3\n4\n5\n6\n7\n8\nY\n10\n"),
            ("left/b.txt", "same\n"),
            ("right/b.txt", "same\n"),
            ("left/c.txt", "one\n"),
            ("right/c.txt", "two\n"),
        ],
    );
    let cfg = Config {
        folder_file_pane: true,
        ..config()
    };
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::with_config("dock_steps", cfg, compare_folders(&roots));
    let docked = |gui: &mut Gui, rel: &str| {
        let start = Instant::now();
        loop {
            gui.frame();
            if let Content::Folder(f) = &gui.app.tabs[0].content {
                if let Some((shown, pos)) = f.docked().filter(|(shown, _)| shown == rel) {
                    assert_eq!(shown, rel);
                    return pos;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "{rel} was never docked"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    gui.click("a.txt");
    assert_eq!(docked(&mut gui, "a.txt"), Some(0));
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(docked(&mut gui, "a.txt"), Some(1));
    gui.frames(2);
    assert!(gui.shows_text("File 1 / 2 · Difference 2 / 2"));
    // Past the last difference, the identical b.txt is skipped.
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(docked(&mut gui, "c.txt"), Some(0));
    gui.key(ALT, Key::ArrowDown);
    assert!(
        gui.toasts()
            .contains(&(false, tr("msg.no_next_file").to_string())),
        "{:?}",
        gui.toasts()
    );
    gui.key(ALT, Key::ArrowUp);
    assert_eq!(docked(&mut gui, "a.txt"), Some(1));

    // The docked compare's own buttons step on to the next file too.
    gui.click_a(accesskit::Role::Button, tr("cmd.next_diff"));
    assert_eq!(docked(&mut gui, "c.txt"), Some(0));
    gui.click_a(accesskit::Role::Button, tr("cmd.prev_diff"));
    assert_eq!(docked(&mut gui, "a.txt"), Some(1));
    gui.click_a(accesskit::Role::Button, tr("cmd.prev_diff"));
    assert_eq!(docked(&mut gui, "a.txt"), Some(0));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());

    // All differences opens at the file and difference shown, and the folder compare follows
    // the file it moves to.
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(docked(&mut gui, "a.txt"), Some(1));
    gui.app.run(Cmd::AllDiffs, &gui.ctx);
    let diffs_status = |gui: &Gui| match &gui.app.tabs[1].content {
        Content::Diffs(v) => v.status_line(),
        _ => String::new(),
    };
    let start = Instant::now();
    while !diffs_status(&gui).starts_with("File 1 / 2 · Difference 2 / 2  |  2 file(s)") {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{}",
            diffs_status(&gui)
        );
        std::thread::sleep(Duration::from_millis(5));
        gui.frame();
    }
    gui.app.run(Cmd::NextDiff, &gui.ctx);
    gui.frames(2);
    assert!(gui.shows_text("File 2 / 2 · Difference 1 / 1"));
    gui.app.active = 0;
    assert_eq!(docked(&mut gui, "c.txt"), Some(0));

    // A file opened in its own tab tells where it is among the files too.
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert!(gui.shows_text("File 2 / 2 · Difference 1 / 1"));
}

#[test]
fn next_conflict_in_a_tab_opened_from_a_folder_goes_on_to_files_with_open_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let far = |v: [&str; 2]| v.join(&format!("\n{}", "same\n".repeat(12))) + "\n";
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
            write(d, &[(&format!("{side}/{name}"), text.as_str())]);
        }
    }
    let roots: Vec<PathBuf> = sides.iter().map(|s| d.join(s)).collect();
    let mut gui = Gui::start("tab_conflicts", compare_folders(&roots));
    let shown = |gui: &Gui| {
        let v = gui.text_view();
        let name = v.titles()[0]
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .to_string();
        (name, v.diff_position())
    };

    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert_eq!(shown(&gui).0, "a.txt");
    // Through the two conflicts of a.txt, then on.
    for _ in 0..3 {
        if shown(&gui).0 != "a.txt" {
            break;
        }
        gui.key(ALT, Key::PageDown);
        gui.frames(2);
    }
    // b.txt merges cleanly, so the conflict of d.txt is next, a frame later.
    assert_eq!(shown(&gui), ("d.txt".into(), (Some(0), 1)));
    let passed = trf("msg.file_switched", &[("file", "b.txt".into())]);
    assert!(!gui.toasts().iter().any(|(_, t)| *t == passed));
    gui.key(ALT, Key::PageDown);
    gui.frames(2);
    assert!(gui
        .toasts()
        .contains(&(false, tr("msg.no_next_conflict_file").to_string())));
    gui.key(ALT, Key::PageUp);
    gui.frames(2);
    assert_eq!(shown(&gui), ("a.txt".into(), (Some(1), 2)));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn saving_a_file_opened_from_an_archive_compare_repacks_the_archive_right_away() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_zip = |path: &Path, content: &str| {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zw.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zw, content.as_bytes()).unwrap();
        zw.finish().unwrap();
    };
    let left_zip = d.join("left.zip");
    let right_zip = d.join("right.zip");
    make_zip(&left_zip, "left\n");
    make_zip(&right_zip, "right\n");

    let mut gui = Gui::start(
        "archive_save_repacks",
        compare(&[left_zip.clone(), right_zip.clone()]),
    );
    gui.settle();
    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert_eq!(
        gui.text_view().titles()[0].rsplit(['/', '\\']).next(),
        Some("a.txt")
    );

    // Make the right pane match the left, then save only this file tab — never the folder
    // tab itself. Saving the file alone must still repack `right.zip` right away, not only
    // once the folder tab is saved separately (see `App::save_origin_folder_archive`).
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.settle();

    let repacked = archive::extract_to_temp(&right_zip).unwrap();
    assert_eq!(read(&repacked.path().join("a.txt")), "left\n");
    // The left archive was not touched by this edit.
    let left_after = archive::extract_to_temp(&left_zip).unwrap();
    assert_eq!(read(&left_after.path().join("a.txt")), "left\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

/// An encrypted archive asks for its password, again when it was wrong, opens with the right
/// one, and is written back still encrypted.
#[test]
fn a_password_protected_archive_asks_for_its_password_and_saves_encrypted() {
    use zip::{write::SimpleFileOptions, AesMode};
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_zip = |path: &Path, content: &str, o: SimpleFileOptions| {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zw.start_file("a.txt", o).unwrap();
        std::io::Write::write_all(&mut zw, content.as_bytes()).unwrap();
        zw.finish().unwrap();
    };
    let left_zip = d.join("left.zip");
    let right_zip = d.join("right.zip");
    make_zip(&left_zip, "left\n", SimpleFileOptions::default());
    let locked = SimpleFileOptions::default().with_aes_encryption(AesMode::Aes256, "sesame");
    make_zip(&right_zip, "right\n", locked);

    let mut gui = Gui::start(
        "archive_password",
        compare(&[left_zip.clone(), right_zip.clone()]),
    );
    let file = || ("file", right_zip.display().to_string());
    assert!(gui.shows(tr("label.archive_password_title")));
    assert!(gui.shows_text(&trf("msg.archive_password_needed", &[file()])));
    assert!(!gui.widget(tr("btn.ok")).enabled);
    gui.type_text("open");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.settle();
    assert!(gui.shows_text(&trf("msg.archive_password_wrong", &[file()])));
    gui.type_text("sesame");
    gui.click(tr("btn.ok"));
    gui.settle();
    assert!(!gui.shows(tr("label.archive_password_title")));

    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());

    let mut ar = zip::ZipArchive::new(std::fs::File::open(&right_zip).unwrap()).unwrap();
    assert!(ar.by_index_raw(0).unwrap().encrypted());
    let mut text = String::new();
    std::io::Read::read_to_string(&mut ar.by_index_decrypt(0, b"sesame").unwrap(), &mut text)
        .unwrap();
    assert_eq!(text, "left\n");
}

#[test]
fn saving_a_file_still_repacks_its_archive_after_the_folder_tab_closed() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_zip = |path: &Path, content: &str| {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zw.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zw, content.as_bytes()).unwrap();
        zw.finish().unwrap();
    };
    let left_zip = d.join("left.zip");
    let right_zip = d.join("right.zip");
    make_zip(&left_zip, "left\n");
    make_zip(&right_zip, "right\n");

    let mut gui = Gui::start(
        "archive_save_after_folder_closed",
        compare(&[left_zip.clone(), right_zip.clone()]),
    );
    gui.settle();
    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert_eq!(
        gui.text_view().titles()[0].rsplit(['/', '\\']).next(),
        Some("a.txt")
    );
    assert_eq!(gui.app.tabs.len(), 2);

    // Close the folder tab the file was opened from, keeping only the file tab open.
    gui.app.close_tab(0, true);
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(matches!(gui.app.tabs[0].content, Content::Text(_)));

    // Saving the file must still repack `right.zip`: there is no folder tab left to ask,
    // so this relies on the root and archive path `Origin` recorded when it was opened.
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.settle();

    let repacked = archive::extract_to_temp(&right_zip).unwrap();
    assert_eq!(read(&repacked.path().join("a.txt")), "left\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn saving_a_file_from_a_tar_xz_writes_it_back_on_another_thread() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_txz = |path: &Path, content: &str| {
        let mut tar = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        tar.append_data(&mut h, "a.txt", content.as_bytes())
            .unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar.into_inner().unwrap().as_slice(), &mut xz).unwrap();
        std::fs::write(path, xz).unwrap();
    };
    let left = d.join("left.tar.xz");
    let right = d.join("right.tar.xz");
    make_txz(&left, "left\n");
    make_txz(&right, "right\n");

    let mut gui = Gui::start("archive_save_slow", compare(&[left.clone(), right.clone()]));
    gui.settle();
    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    // Left to another thread, the folder tab still has it to write meanwhile.
    assert!(gui.app.repack.is_some());
    assert!(gui.app.tabs[0].is_modified());
    gui.settle();

    assert!(gui.app.repack.is_none());
    assert!(!gui.app.tabs[0].is_modified());
    let repacked = archive::extract_to_temp(&right).unwrap();
    assert_eq!(read(&repacked.path().join("a.txt")), "left\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn save_all_with_several_files_from_the_same_archive_repacks_it_once_with_every_edit() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_zip = |path: &Path, files: &[(&str, &str)]| {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, content) in files {
            zw.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zw, content.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    };
    let left_zip = d.join("left.zip");
    let right_zip = d.join("right.zip");
    make_zip(&left_zip, &[("a.txt", "left a\n"), ("b.txt", "left b\n")]);
    make_zip(
        &right_zip,
        &[("a.txt", "right a\n"), ("b.txt", "right b\n")],
    );

    let mut gui = Gui::start(
        "archive_save_all_once",
        compare(&[left_zip.clone(), right_zip.clone()]),
    );
    gui.settle();

    // Open both files from the same folder compare (so both tabs share one `Origin::folder`
    // and one archive side each), make each tab's right pane match its left, then save both
    // at once with `SaveAll` rather than one at a time. This exercises the dedup in
    // `App::save_origin_folder_archives`: the archive must be repacked with both edits, not
    // only the one from whichever tab happened to run last.
    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    gui.app.run(Cmd::CopyRight, &gui.ctx);

    // Back to the folder tab to open the second file (clicking a row only works on the
    // currently active tab's own widgets).
    gui.app.active = 0;
    gui.frame();
    gui.click("b.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    gui.app.run(Cmd::CopyRight, &gui.ctx);

    assert_eq!(gui.app.tabs.len(), 3);
    gui.app.run(Cmd::SaveAll, &gui.ctx);
    gui.settle();

    let repacked = archive::extract_to_temp(&right_zip).unwrap();
    assert_eq!(read(&repacked.path().join("a.txt")), "left a\n");
    assert_eq!(read(&repacked.path().join("b.txt")), "left b\n");
    let left_after = archive::extract_to_temp(&left_zip).unwrap();
    assert_eq!(read(&left_after.path().join("a.txt")), "left a\n");
    assert_eq!(read(&left_after.path().join("b.txt")), "left b\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn a_workspace_saves_the_open_tabs_by_name_and_reopens_them_later() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let mut gui = Gui::start(
        "workspace_save_and_open",
        compare(&[d.join("a.txt"), d.join("a.txt")]),
    );
    gui.settle();
    gui.app.open(OpenRequest {
        paths: vec![d.join("b.txt"), d.join("b.txt")],
        ..Default::default()
    });
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 2);

    // Save the two open tabs as a named workspace, through the dialog.
    gui.app.run(Cmd::SaveWorkspace, &gui.ctx);
    gui.frames(2);
    gui.app.workspace_save_dlg.as_mut().unwrap().0 = "QA pass".to_string();
    gui.frame();
    gui.click(tr("btn.save"));
    assert!(gui.app.workspace_save_dlg.is_none());
    assert_eq!(gui.app.cfg.workspaces.len(), 1);
    assert_eq!(gui.app.cfg.workspaces[0].name, "QA pass");
    assert_eq!(gui.app.cfg.workspaces[0].tabs.len(), 2);

    // Close everything, then reopen the workspace by name: both tabs come back.
    gui.app.run(Cmd::CloseAllTabs, &gui.ctx);
    assert!(gui.app.tabs.is_empty());
    gui.app.run(Cmd::OpenWorkspace(0), &gui.ctx);
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 2);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());

    // Saving again under the same name, in any case, says so and replaces it once Replace is
    // clicked, rather than adding a second entry.
    gui.app.run(Cmd::SaveWorkspace, &gui.ctx);
    gui.frames(2);
    gui.app.workspace_save_dlg.as_mut().unwrap().0 = "qa PASS".to_string();
    // The message that appears grows the dialog, which centers itself again a frame later.
    gui.frames(2);
    assert!(gui.shows(&trf("msg.workspace_exists", &[("name", "QA pass".into())])));
    gui.click(tr("btn.replace"));
    assert_eq!(gui.app.cfg.workspaces.len(), 1);
    assert_eq!(gui.app.cfg.workspaces[0].name, "qa PASS");

    // Deleting it can be undone from its message.
    gui.app.run(Cmd::DeleteWorkspace(0), &gui.ctx);
    assert!(gui.app.cfg.workspaces.is_empty());
    gui.frames(3);
    gui.click(tr("cmd.undo"));
    assert_eq!(gui.app.cfg.workspaces.len(), 1);

    // A tab whose file is gone is not reopened as a compare with an empty side, and is
    // counted in a message.
    std::fs::remove_file(d.join("b.txt")).unwrap();
    gui.app.run(Cmd::CloseAllTabs, &gui.ctx);
    gui.app.run(Cmd::OpenWorkspace(0), &gui.ctx);
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 1);
    let want = trf(
        "msg.workspace_not_reopened",
        &[("n", "1".into()), ("total", "2".into())],
    );
    assert!(gui.errors().contains(&want), "{:?}", gui.errors());
}

#[test]
fn a_workspace_leaves_out_temporary_copies_and_is_not_saved_empty() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let zip_path = d.join("a.zip");
    {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zw.start_file("in.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zw, b"x\n").unwrap();
        zw.finish().unwrap();
    }
    let mut gui = Gui::start(
        "workspace_temporary_copies",
        compare(&[zip_path.clone(), zip_path.clone()]),
    );
    gui.settle();
    // A file tab opened from inside the extracted archive.
    let inner = match &gui.app.tabs[0].content {
        Content::Folder(v) => v.roots[0].join("in.txt"),
        _ => panic!("not a folder compare"),
    };
    gui.app.open(OpenRequest {
        paths: vec![inner.clone(), inner],
        ..Default::default()
    });
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 2);
    let saved = gui.app.session_tabs();
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0].paths[0], zip_path.display().to_string());

    // With nothing open that can be saved, the dialog says so and nothing is saved.
    gui.app.run(Cmd::CloseAllTabs, &gui.ctx);
    gui.app.run(Cmd::SaveWorkspace, &gui.ctx);
    gui.frames(2);
    gui.app.workspace_save_dlg.as_mut().unwrap().0 = "Empty".to_string();
    gui.frame();
    assert!(gui.shows(tr("msg.workspace_nothing")));
    gui.app.workspace_save_dlg = None;
    gui.app.save_workspace("Empty".into());
    assert!(gui.app.cfg.workspaces.is_empty());
    assert!(gui
        .errors()
        .contains(&tr("msg.workspace_nothing").to_string()));
}

#[test]
fn compare_clipboard_opens_the_clipboard_text_on_the_left_and_a_blank_right_side() {
    set_test_clipboard(Ok("one\ntwo\n"));
    let mut gui = Gui::start("compare_clipboard", None);
    gui.app.run(Cmd::CompareClipboard, &gui.ctx);
    gui.settle();

    let Content::Text(v) = &gui.app.tabs[0].content else {
        panic!("Expected a text compare");
    };
    assert_eq!(
        v.panes[0].doc.lines,
        vec!["one".to_string(), "two".to_string()]
    );
    assert_eq!(v.panes[1].doc.lines, vec![String::new()]);
    assert!(!v.panes[0].doc.modified && !v.panes[1].doc.modified);
    assert_eq!(v.panes[0].doc.path, None);
    assert!(gui.shows_text(tr("label.clipboard")));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());

    // The right side is fully editable, so a second text can be pasted in to compare it.
    let Content::Text(v) = &mut gui.app.tabs[0].content else {
        panic!("Expected a text compare");
    };
    v.active_pane = 1;
    gui.events.push(Event::Paste("one\nTWO\n".into()));
    gui.frames(2);
    let Content::Text(v) = &gui.app.tabs[0].content else {
        panic!("Expected a text compare");
    };
    assert_eq!(
        v.panes[1].doc.lines,
        vec!["one".to_string(), "TWO".to_string()]
    );
}

#[test]
fn compare_clipboard_reports_an_empty_clipboard_instead_of_opening_a_tab() {
    set_test_clipboard(Err("nope"));
    let mut gui = Gui::start("compare_clipboard_empty", None);
    gui.app.run(Cmd::CompareClipboard, &gui.ctx);
    gui.frames(2);

    assert!(gui.app.tabs.is_empty());
    assert!(gui.errors().iter().any(|e| e.contains("nope")));

    // Text that is there but empty is said to be empty, the same as no text at all.
    set_test_clipboard(Ok(""));
    gui.app.run(Cmd::CompareClipboard, &gui.ctx);
    gui.frames(2);
    assert!(gui.app.tabs.is_empty());
    assert!(gui
        .errors()
        .contains(&tr("msg.clipboard_empty").to_string()));
}

#[test]
fn compare_over_sftp_fetches_both_sides_edits_one_and_saves_it_back_to_the_server() {
    remote::fake::install(
        remote::fake::Server::new()
            .file("/srv/left.txt", "one\ntwo\n")
            .file("/srv/right.txt", "one\nTWO\n"),
    );
    let mut gui = Gui::start("compare_over_sftp", None);

    // File → Compare over SFTP opens the Open dialog and a connection dialog for its left side.
    gui.app.run(Cmd::OpenRemote, &gui.ctx);
    assert!(gui.app.open_dlg.is_some());
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/left.txt".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    assert!(gui.app.remote_dlg.is_none());
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let left_staged = gui.app.open_dlg.as_ref().unwrap().paths[0].clone();
    assert!(!left_staged.is_empty());
    assert!(!gui.app.open_dlg.as_ref().unwrap().read_only[0]);

    // A second connection, straight on the dialog (as the per-side "Browse a server over
    // SFTP" button would start it), fills in the right side the same way. (Side 2, not 1:
    // slot 1 is the hidden middle path of a 3-way compare, which this dialog is not.)
    gui.app.start_remote_connect(RemoteTarget::Open(2));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/right.txt".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());

    gui.click(tr("btn.compare"));
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 1);
    let Content::Text(v) = &gui.app.tabs[0].content else {
        panic!("Expected a text compare");
    };
    assert_eq!(v.panes[0].doc.lines, vec!["one", "two"]);
    assert_eq!(v.panes[1].doc.lines, vec!["one", "TWO"]);
    assert!(!v.panes[0].doc.read_only);

    // Editing the left (remote) side and saving uploads it back to the server, over a fresh
    // connection, leaving the unchanged right side alone.
    assert_eq!(remote::fake::upload_count("/srv/left.txt"), 0);
    text_view_mut(&mut gui).place_caret(0, (1, 3), false);
    gui.events.push(Event::Text("-edited".into()));
    gui.frames(2);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frame();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(remote::fake::upload_count("/srv/left.txt"), 1);
    let uploaded = remote::fake::uploaded("/srv/left.txt").unwrap();
    assert_eq!(String::from_utf8(uploaded).unwrap(), "one\ntwo-edited\n");
    assert_eq!(remote::fake::upload_count("/srv/right.txt"), 0);

    remote::fake::uninstall();
}

/// Fetches `remote_path` from the fake server into the start page's side `side`.
fn fetch_to_start_page(gui: &mut Gui, side: usize, remote_path: &str) {
    gui.app.start_remote_connect(RemoteTarget::Start(side));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = remote_path.to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    gui.settle();
}

/// Compares the remote file `/srv/a.txt` with a local one, from the start page.
fn compare_remote_with_local(gui: &mut Gui, local: &Path) {
    fetch_to_start_page(gui, 0, "/srv/a.txt");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    gui.app.start_paths[2] = local.display().to_string();
    gui.frame();
    gui.click(tr("btn.compare"));
    assert_eq!(gui.app.tabs.len(), 1);
}

#[test]
fn an_unknown_server_asks_to_trust_its_fingerprint_before_fetching() {
    remote::fake::install(
        remote::fake::Server::new()
            .file("/srv/a.txt", "a")
            .unknown_host(),
    );
    let mut gui = Gui::start("sftp_unknown_host", None);
    fetch_to_start_page(&mut gui, 0, "/srv/a.txt");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert!(gui.app.start_paths[0].is_empty(), "nothing fetched yet");
    assert!(gui.app.host_prompt.is_some());
    gui.frame();
    assert!(gui.shows_text("SHA256:fake"));

    gui.click(tr("btn.trust_connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert!(gui.app.host_prompt.is_none());
    assert!(Path::new(&gui.app.start_paths[0]).is_file());
    remote::fake::uninstall();
}

#[test]
fn a_file_changed_on_the_server_is_not_overwritten_and_its_tab_stays_unsaved() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\n"));
    let mut gui = Gui::start("sftp_conflict", None);
    compare_remote_with_local(&mut gui, &dir.path().join("b.txt"));

    remote::fake::change("/srv/a.txt", "theirs\n");
    text_view_mut(&mut gui).place_caret(0, (0, 3), false);
    gui.events.push(Event::Text("-mine".into()));
    gui.frames(2);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(2);
    assert_eq!(remote::fake::upload_count("/srv/a.txt"), 0);
    assert!(
        gui.errors().iter().any(|e| e.contains("/srv/a.txt")),
        "{:?}",
        gui.errors()
    );
    assert!(
        gui.app.tabs[0].is_modified(),
        "the edit is not on the server"
    );
    remote::fake::uninstall();
}

#[test]
fn a_remote_file_written_half_way_is_finished_by_saving_again() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(
        remote::fake::Server::new()
            .file("/srv/a.txt", "one\n")
            .partly_fails("/srv/a.txt"),
    );
    let mut gui = Gui::start("sftp_partly_written", None);
    compare_remote_with_local(&mut gui, &dir.path().join("b.txt"));

    text_view_mut(&mut gui).place_caret(0, (0, 3), false);
    gui.events.push(Event::Text("-mine".into()));
    gui.frames(2);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(3);
    let partly = trf(
        "msg.remote_partly_written",
        &[("path", "/srv/a.txt".into())],
    );
    assert!(
        gui.errors().iter().any(|e| e.contains(&partly)),
        "{:?}",
        gui.errors()
    );
    assert_eq!(
        remote::fake::entry("/srv/a.txt"),
        Some(Some(b"one-".to_vec()))
    );
    assert!(gui.app.tabs[0].is_modified());

    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(3);
    assert_eq!(
        remote::fake::entry("/srv/a.txt"),
        Some(Some(b"one-mine\n".to_vec()))
    );
    assert!(!gui.app.tabs[0].is_modified());
    remote::fake::uninstall();
}

#[test]
fn reload_from_server_replaces_the_unsaved_edits_of_a_remote_file_only() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\n"));
    let mut gui = Gui::start("sftp_reload_text", None);
    compare_remote_with_local(&mut gui, &dir.path().join("b.txt"));

    for pane in [0, 1] {
        text_view_mut(&mut gui).place_caret(pane, (0, 3), false);
        gui.events.push(Event::Text("-mine".into()));
        gui.frames(2);
    }
    remote::fake::change("/srv/a.txt", "theirs\n");
    gui.app.run(Cmd::ReloadRemote, &gui.ctx);
    gui.frames(2);
    assert!(gui.shows(tr("msg.reload_remote_discards")));
    gui.click(tr("btn.reload_remote"));
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let v = text_view_mut(&mut gui);
    assert_eq!(v.panes[0].doc.lines, vec!["theirs"]);
    assert!(!v.panes[0].doc.modified);
    assert_eq!(v.panes[1].doc.lines, vec!["one-mine"], "not the server's");
    assert!(v.panes[1].doc.modified);
    remote::fake::uninstall();
}

/// A save made while an upload of the same file is still under way waits for it, then
/// expects the server's file as that upload left it, not as it was fetched.
#[test]
fn saving_a_remote_tab_again_while_it_uploads_is_not_taken_for_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\n"));
    let mut gui = Gui::start("sftp_save_twice", None);
    compare_remote_with_local(&mut gui, &dir.path().join("b.txt"));

    text_view_mut(&mut gui).place_caret(0, (0, 3), false);
    gui.events.push(Event::Text("-mine".into()));
    gui.frames(2);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(remote::fake::upload_count("/srv/a.txt"), 2);
    assert!(!gui.app.tabs[0].is_modified());
    remote::fake::uninstall();
}

/// A side that was saved is uploaded even when the other side could not be saved.
#[cfg(unix)]
#[test]
fn a_remote_side_saved_is_uploaded_though_the_other_side_failed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\n"));
    let mut gui = Gui::start("sftp_partial_save", None);
    let local = dir.path().join("b.txt");
    compare_remote_with_local(&mut gui, &local);

    for pane in [0, 1] {
        text_view_mut(&mut gui).place_caret(pane, (0, 3), false);
        gui.events.push(Event::Text("-edited".into()));
        gui.frames(2);
    }
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o444)).unwrap();
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(3);
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        gui.errors().iter().any(|e| e.contains("b.txt")),
        "{:?}",
        gui.errors()
    );
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "one\n");
    assert_eq!(remote::fake::upload_count("/srv/a.txt"), 1);
    let uploaded = remote::fake::uploaded("/srv/a.txt").unwrap();
    assert_eq!(String::from_utf8(uploaded).unwrap(), "one-edited\n");
    remote::fake::uninstall();
}

#[test]
fn saving_a_remote_tab_while_closing_it_uploads_before_it_closes() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\n"));
    let mut gui = Gui::start("sftp_save_on_close", None);
    compare_remote_with_local(&mut gui, &dir.path().join("b.txt"));

    text_view_mut(&mut gui).place_caret(0, (0, 3), false);
    gui.events.push(Event::Text("-edited".into()));
    gui.frames(2);
    gui.key(CMD, Key::W);
    assert!(gui.shows(tr("dlg.unsaved")));
    gui.click(tr("btn.save"));
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        remote::fake::uploaded("/srv/a.txt").as_deref(),
        Some(&b"one-edited\n"[..])
    );
    assert!(gui.app.tabs.is_empty());
    remote::fake::uninstall();
}

fn active_text_view_mut(gui: &mut Gui) -> &mut TextView {
    let active = gui.app.active;
    match gui.app.tabs[active].content.view_mut() {
        Content::Text(v) => v,
        _ => panic!("Expected a text compare"),
    }
}

fn remote_folder_view(gui: &mut Gui) -> &mut FolderView {
    match gui.app.tabs[0].content.view_mut() {
        Content::Folder(v) => v,
        _ => panic!("Expected a folder compare"),
    }
}

#[test]
fn a_folder_fetched_from_the_start_page_is_written_back_on_save_and_reloads_from_the_server() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("a.txt", "local")]);
    remote::fake::install(
        remote::fake::Server::new()
            .dir("/srv/data")
            .file("/srv/data/a.txt", "remote")
            .file("/srv/data/old.txt", "old"),
    );
    let mut gui = Gui::start("sftp_start_page_folder", None);
    fetch_to_start_page(&mut gui, 0, "/srv/data");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    gui.app.start_paths[2] = dir.path().display().to_string();
    gui.frame();
    gui.click(tr("btn.compare"));
    gui.settle();
    let v = remote_folder_view(&mut gui);
    assert!(!v.read_only[0], "the fetched folder can be written back");
    assert!(v.is_remote_side(0) && !v.is_remote_side(1));
    assert!(
        gui.app.session_tabs().is_empty(),
        "a staged folder is not saved in the session"
    );
    assert!(!gui.app.tabs[0].is_modified());

    // Changes made here are written back by Save, and only those.
    let root = gui.app.tabs[0].source[0].clone();
    std::fs::write(root.join("a.txt"), "edited here").unwrap();
    std::fs::write(root.join("new.txt"), "new").unwrap();
    std::fs::remove_file(root.join("old.txt")).unwrap();
    remote_folder_view(&mut gui).mark_remote_dirty(0);
    assert!(gui.app.tabs[0].is_modified());
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        remote::fake::entry("/srv/data/a.txt"),
        Some(Some(b"edited here".to_vec()))
    );
    assert_eq!(
        remote::fake::entry("/srv/data/new.txt"),
        Some(Some(b"new".to_vec()))
    );
    assert!(remote::fake::was_removed("/srv/data/old.txt"));
    assert!(!gui.app.tabs[0].is_modified());

    // Reload from server shows what changed there since.
    remote::fake::change("/srv/data/a.txt", "theirs");
    gui.app.run(Cmd::ReloadRemote, &gui.ctx);
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "theirs"
    );

    // Changes not saved to the server yet are replaced only once agreed to.
    std::fs::write(root.join("a.txt"), "mine").unwrap();
    remote_folder_view(&mut gui).mark_remote_dirty(0);
    gui.app.run(Cmd::ReloadRemote, &gui.ctx);
    gui.frames(2);
    assert!(gui.shows(tr("msg.reload_remote_discards")));
    gui.click(tr("btn.cancel"));
    gui.frames(2);
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "mine");
    gui.app.run(Cmd::ReloadRemote, &gui.ctx);
    gui.frames(2);
    gui.click(tr("btn.reload_remote"));
    gui.frames(3);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "theirs"
    );
    assert!(!gui.app.tabs[0].is_modified());
    remote::fake::uninstall();
}

#[test]
fn a_file_opened_from_a_remote_folder_is_uploaded_when_saved_also_once_the_folder_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("a.txt", "local\n")]);
    remote::fake::install(
        remote::fake::Server::new()
            .dir("/srv/data")
            .file("/srv/data/a.txt", "remote\n"),
    );
    let mut gui = Gui::start("remote_folder_child_save", None);
    fetch_to_start_page(&mut gui, 0, "/srv/data");
    gui.app.start_paths[2] = dir.path().display().to_string();
    gui.frame();
    gui.click(tr("btn.compare"));
    gui.settle();
    gui.click("a.txt");
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert_eq!(gui.app.tabs.len(), 2);

    let Content::Text(v) = gui.app.tabs[1].content.view_mut() else {
        panic!("Expected the file in a text compare");
    };
    v.place_caret(0, (0, 6), false);
    gui.events.push(Event::Text("-edited".into()));
    gui.frames(2);
    gui.key(CMD, Key::S);
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        remote::fake::entry("/srv/data/a.txt"),
        Some(Some(b"remote-edited\n".to_vec()))
    );
    assert!(!gui.app.tabs.iter().any(Tab::is_modified));

    gui.app.close_tab(0, false);
    gui.frames(2);
    assert_eq!(gui.app.tabs.len(), 1);
    active_text_view_mut(&mut gui).place_caret(0, (0, 13), false);
    gui.events.push(Event::Text("!".into()));
    gui.frames(2);
    gui.key(CMD, Key::S);
    gui.frames(3);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        remote::fake::entry("/srv/data/a.txt"),
        Some(Some(b"remote-edited!\n".to_vec()))
    );
    remote::fake::uninstall();
}

#[test]
fn the_connection_dialog_offers_ftp_with_a_warning_and_sftp_with_ssh_agent() {
    let mut gui = Gui::start("remote_protocols", None);
    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    assert!(gui.shows(tr("label.remote_agent")));
    assert!(!gui.shows(tr("msg.ftp_plain_warning")));
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.sign_in = SignIn::Agent;
    }
    gui.frames(2);
    assert!(gui.shows(tr("msg.remote_agent_hint")));
    let d = gui.app.remote_dlg.as_ref().unwrap();
    assert!(matches!(d.conn().unwrap().auth, remote::Auth::Agent));

    // FTP signs in with a password only, on its own port, and warns it is not encrypted.
    gui.click(protocol_name(remote::Protocol::Sftp));
    gui.frames(2);
    gui.click(protocol_name(remote::Protocol::Ftp));
    gui.frames(2);
    assert!(gui.shows(tr("msg.ftp_plain_warning")));
    assert!(!gui.shows(tr("label.remote_agent")));
    let d = gui.app.remote_dlg.as_mut().unwrap();
    assert_eq!(d.port, "21");
    let c = d.conn().unwrap();
    assert_eq!(c.protocol, remote::Protocol::Ftp);
    assert!(matches!(c.auth, remote::Auth::Password(_)));
    // A port typed in is kept; the usual one follows the protocol.
    d.port = "2121".to_string();
    d.set_protocol(remote::Protocol::FtpsImplicit);
    assert_eq!(d.port, "2121");
    d.port = "990".to_string();
    d.set_protocol(remote::Protocol::Ftps);
    assert_eq!(d.port, "21");
    d.set_protocol(remote::Protocol::Sftp);
    assert_eq!(d.port, "22");
    assert_eq!(d.sign_in(), SignIn::Agent, "the SFTP sign-in is kept");

    // A password typed into the proxy URL moves to the field that hides it.
    d.proxy = "socks5://bob:s%40cret@proxy.example:1080".to_string();
    d.hide_proxy_password();
    assert_eq!(d.proxy, "socks5://bob@proxy.example:1080");
    assert_eq!(d.proxy_password, "s@cret");
    let proxy = d.proxy().unwrap().unwrap();
    assert_eq!(proxy.password.as_deref(), Some("s@cret"));
}

#[test]
fn compare_over_sftp_fetches_a_writable_folder_and_a_failure_reopens_the_dialog() {
    remote::fake::install(
        remote::fake::Server::new()
            .dir("/srv/data")
            .file("/srv/data/a.txt", "a")
            .file("/srv/data/b.txt", "b"),
    );
    let mut gui = Gui::start("compare_over_sftp_folder", None);
    gui.app.run(Cmd::OpenRemote, &gui.ctx);
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/data".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let d = gui.app.open_dlg.as_ref().unwrap();
    assert!(!d.read_only[0], "a fetched folder can be written back");
    assert!(std::path::Path::new(&d.paths[0]).join("a.txt").exists());

    // A connection that fails reopens the connection dialog with its fields kept, instead of
    // silently dropping what was typed. The slot index below is incidental to this scenario
    // (any would do); `0` just matches the Open dialog's left side, used above too.
    remote::fake::install(remote::fake::Server::new().failing("connection refused"));
    gui.app.start_remote_connect(RemoteTarget::Open(0));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "unreachable.example".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/anything".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui
        .errors()
        .iter()
        .any(|e| e.contains("connection refused")));
    assert_eq!(
        gui.app.remote_dlg.as_ref().unwrap().host,
        "unreachable.example"
    );

    remote::fake::uninstall();
}

#[test]
fn compare_over_sftp_from_the_start_page_fills_in_its_path_field_and_compares() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("b.txt", "one\ntwo\n")]);
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "one\ntwo\n"));
    let mut gui = Gui::start("compare_over_sftp_start_page", None);

    // The start page's own "Browse a server over SFTP" button, not the Open dialog's: fills
    // in the start page's own path field (`start_paths`), not `open_dlg`.
    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/a.txt".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    assert!(gui.app.remote_dlg.is_none());
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert!(gui.app.open_dlg.is_none(), "stayed on the start page");
    let staged = gui.app.start_paths[0].clone();
    assert!(Path::new(&staged).is_file());

    // Comparing from the start page claims the fetch the same way the Open dialog does.
    gui.app.start_paths[2] = dir.path().join("b.txt").display().to_string();
    gui.frame();
    gui.click(tr("btn.compare"));
    assert_eq!(gui.app.tabs.len(), 1);
    assert_eq!(gui.text_view().panes[0].doc.lines, ["one", "two"]);
    assert!(gui.app.pending_remote.is_empty(), "the fetch was claimed");

    remote::fake::uninstall();
}

#[test]
fn cancelling_the_open_dialog_after_a_fetch_drops_its_staged_copy() {
    remote::fake::install(remote::fake::Server::new().file("/srv/a.txt", "a"));
    let mut gui = Gui::start("compare_over_sftp_cancel_drops_fetch", None);

    gui.app.run(Cmd::OpenRemote, &gui.ctx);
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.remote_path = "/srv/a.txt".to_string();
    }
    gui.frame();
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(gui.app.pending_remote.len(), 1, "the fetch is staged");

    // Cancelling the Open dialog, instead of comparing, leaves the staged copy unclaimed: a
    // frame of normal upkeep (where `gc_pending_remote` runs) reaps it rather than leaking
    // its temporary folder for the rest of the session.
    gui.click(tr("btn.cancel"));
    assert!(gui.app.open_dlg.is_none());
    gui.frame();
    assert!(
        gui.app.pending_remote.is_empty(),
        "an unclaimed fetch is not kept once its dialog is gone"
    );

    remote::fake::uninstall();
}

#[test]
fn a_second_connection_starts_from_the_last_ones_server_but_not_its_password() {
    remote::fake::install(
        remote::fake::Server::new()
            .file("/srv/a.txt", "a")
            .file("/srv/b.txt", "b"),
    );
    let mut gui = Gui::start("compare_over_sftp_remembers_server", None);

    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.port = "2222".to_string();
        d.user = "ann".to_string();
        d.sign_in = SignIn::Key;
        d.key_path = "/home/ann/.ssh/id_ed25519".to_string();
        d.passphrase = "s3cret".to_string();
        d.remote_path = "/srv/a.txt".to_string();
    }
    gui.frames(2);
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert!(
        gui.app.last_remote.is_some(),
        "last_remote should be set after a successful connect"
    );

    // A second connection, for the other side, starts from the same server and key instead
    // of blank fields, but never from the passphrase just typed: that is kept only for the
    // tab the first fetch is in, the same as any other `RemoteConnection`.
    gui.app.start_remote_connect(RemoteTarget::Start(2));
    gui.frame();
    let d = gui.app.remote_dlg.as_ref().unwrap();
    assert_eq!(d.host, "example.com");
    assert_eq!(d.port, "2222");
    assert_eq!(d.user, "ann");
    assert_eq!(d.sign_in, SignIn::Key);
    assert_eq!(d.key_path, "/home/ann/.ssh/id_ed25519");
    assert_eq!(d.passphrase, "", "a passphrase is never remembered");
    assert_eq!(d.remote_path, "", "the remote path is always blank");

    remote::fake::uninstall();
}

#[test]
fn the_connection_dialog_lists_the_servers_folders_to_pick_the_remote_path_from() {
    remote::fake::install(
        remote::fake::Server::new()
            .home("/home/ann")
            .dir("/home")
            .dir("/home/ann")
            .file("/home/ann/notes.txt", "notes\n")
            .file(&format!("/home/ann/{}.txt", "long-name-".repeat(20)), "")
            .dir("/home/ann/srv")
            .file("/home/ann/srv/a.txt", "one\n")
            .file("/home/ann/srv/b.txt", "two\n"),
    );
    let mut gui = Gui::start("sftp_browse", None);
    let remote_path = |gui: &Gui| gui.app.remote_dlg.as_ref().unwrap().remote_path.clone();

    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    assert!(
        !gui.widget(tr("btn.list_remote")).enabled,
        "nothing to list until the server and user are filled in"
    );
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
        d.sign_in = SignIn::Agent;
    }
    // The agent hint makes the dialog taller, and a centered dialog moves to its new place a
    // frame later; clicking before then misses where the fonts wrap the hint to more lines.
    gui.frames(3);
    let width = |gui: &Gui| {
        gui.widget(tr("btn.connect")).rect.right()
            - gui.widget(tr("label.remote_protocol")).rect.left()
    };
    // Long hints wrap, and long names are cut short, rather than stretching the dialog across
    // the window.
    assert!(width(&gui) <= 610.0, "{} wide", width(&gui));

    // With no path typed, the list starts at the home folder, and the path field follows the
    // folder shown.
    gui.click(tr("btn.list_remote"));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(remote_path(&gui), "/home/ann");
    assert!(gui.shows("srv") && gui.shows("notes.txt"));
    gui.frames(2);
    assert!(width(&gui) <= 610.0, "{} wide with the list", width(&gui));

    // Clicking a folder opens it; clicking a file picks it; *Up* goes back.
    gui.click("srv");
    assert_eq!(remote_path(&gui), "/home/ann/srv");
    assert!(gui.shows("a.txt") && !gui.shows("notes.txt"));
    gui.click("b.txt");
    assert_eq!(remote_path(&gui), "/home/ann/srv/b.txt");
    gui.click(tr("btn.remote_up"));
    assert_eq!(remote_path(&gui), "/home/ann");
    gui.click("srv");
    gui.click("a.txt");
    gui.click(tr("btn.connect"));
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        std::fs::read_to_string(&gui.app.start_paths[0]).unwrap(),
        "one\n"
    );

    // A typed path opens the list at its folder, with the file picked; double-clicking a file
    // fetches it straight away.
    gui.app.start_remote_connect(RemoteTarget::Start(2));
    gui.frames(2);
    gui.app.remote_dlg.as_mut().unwrap().remote_path = "/home/ann/srv/b.txt".to_string();
    gui.frame();
    gui.click(tr("btn.list_remote"));
    assert_eq!(remote_path(&gui), "/home/ann/srv/b.txt");
    assert!(gui.shows("a.txt"));
    gui.click(tr("btn.remote_up"));
    // Long enough after the last click for the next two not to count as a triple-click.
    gui.time += 1.0;
    gui.click("notes.txt");
    gui.click("notes.txt");
    assert!(gui.app.remote_dlg.is_none());
    gui.settle();
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(
        std::fs::read_to_string(&gui.app.start_paths[2]).unwrap(),
        "notes\n"
    );

    remote::fake::uninstall();
}

#[test]
fn the_connection_dialogs_advanced_settings_reach_the_connection() {
    let mut gui = Gui::start("remote_advanced", None);
    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
    }
    gui.frame();
    assert!(!gui.shows(tr("label.remote_proxy")), "folded away at first");
    gui.click(tr("label.remote_advanced"));
    gui.frames(2);
    assert!(gui.shows(tr("label.remote_proxy")));
    assert!(!gui.shows(tr("label.remote_active")), "not over SFTP");
    assert!(
        !gui.shows(tr("label.remote_proxy_password")),
        "no proxy yet"
    );

    let conn = |gui: &Gui| gui.app.remote_dlg.as_ref().unwrap().conn();
    gui.app.remote_dlg.as_mut().unwrap().proxy = "ftp://proxy:21".to_string();
    gui.frame();
    assert!(conn(&gui).is_none(), "not a proxy");
    assert!(gui.shows_text("Not a proxy address"));
    gui.app.remote_dlg.as_mut().unwrap().proxy = "socks5://bob:pw@proxy:1080".to_string();
    gui.frame();
    let proxy = conn(&gui).unwrap().options.proxy.unwrap();
    assert_eq!(proxy.password.as_deref(), Some("pw"));
    assert_eq!(proxy.url(), "socks5://bob@proxy:1080", "shown without it");
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.proxy = "socks5://bob@proxy:1080".to_string();
        d.proxy_password = "pw".to_string();
        d.active = true;
        d.client_cert = "/keys/me.pem".to_string();
    }
    gui.frame();
    assert!(gui.shows(tr("label.remote_proxy_password")));
    let options = conn(&gui).unwrap().options;
    let proxy = options.proxy.unwrap();
    assert_eq!(proxy.password.as_deref(), Some("pw"));
    assert_eq!(proxy.url(), "socks5://bob@proxy:1080");
    assert!(
        !options.active && options.client_cert.is_none(),
        "neither over SFTP"
    );

    gui.app
        .remote_dlg
        .as_mut()
        .unwrap()
        .set_protocol(remote::Protocol::Ftps);
    gui.frames(2);
    assert!(gui.shows(tr("label.remote_active")) && gui.shows(tr("label.remote_client_cert")));
    let options = conn(&gui).unwrap().options;
    assert!(options.active);
    assert_eq!(options.client_cert, Some(PathBuf::from("/keys/me.pem")));
}

#[test]
fn a_servers_sign_in_questions_are_asked_in_a_window_until_answered_declined_or_stopped() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let mut gui = Gui::start("ssh_ask", None);
    let (ask, rx) = asker(&gui.ctx);
    gui.app.asks = Some(rx);
    let challenge = remote::Challenge {
        server: "example.com:22".into(),
        name: "Two-step sign-in".into(),
        instructions: "Open your authenticator.".into(),
        prompts: vec![
            remote::Prompt {
                text: "Code: ".into(),
                echo: false,
            },
            remote::Prompt {
                text: "Device: ".into(),
                echo: true,
            },
        ],
    };
    let asking = |cancel: Arc<AtomicBool>| {
        let (ask, challenge) = (ask.clone(), challenge.clone());
        std::thread::spawn(move || ask(&challenge, &cancel))
    };
    let shown = |gui: &Gui| gui.shows_text("Open your authenticator.");

    // The window's own thread is never kept waiting for itself.
    assert_eq!(ask(&challenge, &AtomicBool::new(false)), None);
    gui.frames(2);
    assert!(!shown(&gui));

    // The first field has the keyboard, and the answers come back in order.
    let answered = asking(Arc::default());
    gui.wait_until("the questions", shown);
    assert!(gui.shows_text("example.com:22") && gui.shows_text("Two-step sign-in"));
    gui.type_text("123456");
    gui.tab(false);
    gui.type_text("phone");
    gui.click(tr("btn.ok"));
    assert_eq!(
        answered.join().unwrap(),
        Some(vec!["123456".to_string(), "phone".to_string()])
    );
    gui.frames(2);
    assert!(!shown(&gui));

    let declined = asking(Arc::default());
    gui.wait_until("the questions", shown);
    gui.click(tr("btn.cancel"));
    assert_eq!(declined.join().unwrap(), None);

    // A connection stopped while asking takes its window with it.
    let cancel = Arc::new(AtomicBool::new(false));
    let stopped = asking(cancel.clone());
    gui.wait_until("the questions", shown);
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(stopped.join().unwrap(), None);
    gui.wait_until("the window closed", |g| !shown(g));
}

#[test]
fn listing_an_untrusted_server_asks_to_trust_it_and_a_changed_field_closes_the_list() {
    remote::fake::install(
        remote::fake::Server::new()
            .home("/srv")
            .dir("/srv")
            .file("/srv/a.txt", "a")
            .unknown_host(),
    );
    let mut gui = Gui::start("sftp_browse_unknown_host", None);
    gui.app.start_remote_connect(RemoteTarget::Start(0));
    gui.frames(2);
    {
        let d = gui.app.remote_dlg.as_mut().unwrap();
        d.host = "example.com".to_string();
        d.user = "ann".to_string();
    }
    gui.frame();

    // A server not trusted yet asks first, then lists once trusted, back in the dialog.
    gui.click(tr("btn.list_remote"));
    assert!(gui.app.host_prompt.is_some());
    assert!(gui.app.remote_dlg.is_none());
    gui.click(tr("btn.trust_connect"));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let d = gui.app.remote_dlg.as_ref().unwrap();
    assert!(d.browse.is_some());
    assert_eq!(d.remote_path, "/srv");
    assert!(gui.shows("a.txt"));

    // Signing in as someone else closes the list made with the old connection.
    gui.app.remote_dlg.as_mut().unwrap().user = "bob".to_string();
    // A frame to close the list, and one for the dialog, now shorter, to center again.
    gui.frames(2);
    assert!(gui.app.remote_dlg.as_ref().unwrap().browse.is_none());
    assert!(!gui.shows("a.txt"));

    // A connection that fails says so in the list, leaving the dialog as it was.
    remote::fake::install(remote::fake::Server::new().failing("connection refused"));
    gui.click(tr("btn.list_remote"));
    assert!(gui.shows_text("connection refused"));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    assert_eq!(gui.app.remote_dlg.as_ref().unwrap().user, "bob");

    remote::fake::uninstall();
}

#[test]
fn saving_in_all_differences_repacks_the_archives_it_touched() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let make_zip = |path: &Path, content: &str| {
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zw.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zw, content.as_bytes()).unwrap();
        zw.finish().unwrap();
    };
    let left_zip = d.join("left.zip");
    let right_zip = d.join("right.zip");
    make_zip(&left_zip, "left\n");
    make_zip(&right_zip, "right\n");

    let mut gui = Gui::start(
        "archive_all_diffs_repacks",
        compare(&[left_zip.clone(), right_zip.clone()]),
    );
    gui.settle();
    gui.app.run(Cmd::AllDiffs, &gui.ctx);
    gui.settle();
    assert!(matches!(gui.app.tabs[1].content, Content::Diffs(_)));

    // Edit and save from "All differences" rather than a file tab opened from the folder;
    // this must repack `right.zip` too (see `App::refresh_saved_folders`).
    gui.app.run(Cmd::FirstDiff, &gui.ctx);
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    gui.settle();

    let repacked = archive::extract_to_temp(&right_zip).unwrap();
    assert_eq!(read(&repacked.path().join("a.txt")), "left\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn the_file_compare_below_the_folder_list_is_shown_and_hidden_from_the_toolbar_and_keyboard() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("left/a.txt", "one\n"), ("right/a.txt", "two\n")]);
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::start("file_pane_toggle", compare_folders(&roots));
    assert!(!gui.app.cfg.folder_file_pane);

    gui.click(tr("view.folder_file_pane"));
    assert!(gui.app.cfg.folder_file_pane);
    gui.click("a.txt");
    let start = Instant::now();
    while !matches!(&gui.app.tabs[0].content, Content::Folder(f) if f.docked().is_some()) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "a.txt was never docked"
        );
        std::thread::sleep(Duration::from_millis(5));
        gui.frame();
    }

    gui.key(ALT, Key::P);
    assert!(!gui.app.cfg.folder_file_pane);
    assert!(matches!(&gui.app.tabs[0].content, Content::Folder(f) if f.docked().is_none()));
    gui.key(ALT, Key::P);
    assert!(gui.app.cfg.folder_file_pane);
}

#[test]
fn tabs_switch_and_ask_before_closing_with_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a1.txt", "x\n1\n"),
            ("a2.txt", "x\n2\n"),
            ("b1.txt", "y\n1\n"),
            ("b2.txt", "y\n2\n"),
        ],
    );
    let mut gui = Gui::start("tabs", compare(&[d.join("a1.txt"), d.join("a2.txt")]));
    gui.drop_files(&[d.join("b1.txt"), d.join("b2.txt")]);
    assert_eq!(gui.app.tabs.len(), 2);
    assert_eq!(gui.app.active, 1);
    gui.key(Modifiers::CTRL, Key::Tab);
    assert_eq!(gui.app.active, 0);
    let first = gui.app.tabs[1].title();
    gui.click(&first);
    assert_eq!(gui.app.active, 1);

    gui.key(ALT, Key::ArrowDown);
    gui.key(ALT, Key::ArrowRight);
    gui.key(CMD, Key::W);
    assert!(gui.shows(tr("dlg.unsaved")));
    gui.click(tr("btn.cancel"));
    assert_eq!(gui.app.tabs.len(), 2);
    gui.key(CMD, Key::W);
    gui.click(tr("btn.discard"));
    assert_eq!(gui.app.tabs.len(), 1);
    assert_eq!(read(&d.join("b2.txt")), "y\n2\n");

    gui.key(ALT, Key::ArrowDown);
    gui.key(ALT, Key::ArrowRight);
    gui.key(CMD, Key::W);
    gui.click(tr("btn.save"));
    assert!(gui.app.tabs.is_empty());
    assert_eq!(read(&d.join("a2.txt")), "x\n1\n");
}

#[test]
fn lines_are_found_copied_and_pasted_with_the_keyboard() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "alpha\nbeta\ngamma\n"),
            ("b.txt", "alpha\ngamma\n"),
        ],
    );
    let mut gui = Gui::start("keyboard", compare(&[d.join("a.txt"), d.join("b.txt")]));
    gui.key(CMD, Key::F);
    assert!(gui.text_view().find.open);
    gui.type_text("gam");
    gui.key(Modifiers::NONE, Key::Enter);
    assert_eq!(gui.text_view().find.query, "gam");
    let row = gui.text_view().cursor_row.expect("the match is selected");
    assert_eq!(gui.text_view().diff.rows[row].line(0), Some(2));
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(!gui.text_view().find.open);

    gui.key(CMD, Key::Home);
    gui.key(Modifiers::SHIFT, Key::ArrowDown);
    gui.events.push(Event::Copy);
    gui.frames(2);
    assert_eq!(gui.copied.last().map(String::as_str), Some("alpha\nbeta\n"));

    gui.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!(gui.text_view().active_pane, 1);
    gui.key(CMD, Key::End);
    gui.events.push(Event::Paste("delta\n".into()));
    gui.frames(2);
    assert_eq!(
        gui.text_view().panes[1].doc.lines,
        ["alpha", "gamma", "delta"]
    );
}

#[test]
fn theme_and_language_follow_the_settings() {
    let mut gui = Gui::start("theme_language", None);
    assert!(!gui.ctx.global_style().visuals.dark_mode);
    gui.click(tr("view.dark_mode"));
    assert!(gui.ctx.global_style().visuals.dark_mode);

    gui.click(tr("menu.options").trim_end_matches(['.', '…']));
    gui.click_a(accesskit::Role::ComboBox, tr("menu.language"));
    gui.click("Deutsch");
    gui.click(tr("btn.ok"));
    assert_eq!(i18n::current_language(), "de");
    assert_eq!(gui.app.cfg.language, "de");
    assert!(gui.shows(tr("menu.file")));
    assert_ne!(tr("menu.file"), "File");
}

#[test]
fn patches_are_generated_and_applied_for_review() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "one\n2\nthree\n")],
    );
    let mut gui = Gui::start("patch", compare(&[d.join("a.txt"), d.join("b.txt")]));
    gui.menu(&[tr("menu.file"), tr("menu.export"), tr("menu.patch")]);
    let field = gui
        .widgets
        .iter()
        .rev()
        .find(|w| w.role == accesskit::Role::TextInput)
        .expect("the patch dialog asks for a file")
        .rect;
    gui.events.push(Event::PointerMoved(field.center()));
    gui.click_at(field.center());
    let out = d.join("changes.patch");
    gui.type_text(&out.display().to_string());
    gui.click(tr("btn.ok"));
    let written = read(&out);
    let parsed = patch::parse_unified(&written).unwrap();
    let original = vec!["one".to_string(), "two".into(), "three".into()];
    assert_eq!(
        patch::apply(&original, &parsed[0]).lines,
        ["one", "2", "three"]
    );

    // A patch whose names lead to a file next to it opens that file patched, to review.
    write(
        d,
        &[
            ("target.txt", "one\ntwo\nthree\n"),
            (
                "fix.patch",
                "--- a/target.txt\n+++ b/target.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n",
            ),
        ],
    );
    gui.app.apply_patch_file(&d.join("fix.patch"));
    gui.settle();
    assert_eq!(gui.app.tabs.len(), 2);
    assert_eq!(gui.text_view().panes[1].doc.lines, ["one", "2", "three"]);
    assert_eq!(read(&d.join("target.txt")), "one\ntwo\nthree\n");
    gui.key(CMD, Key::S);
    assert_eq!(read(&d.join("target.txt")), "one\n2\nthree\n");

    // Hunks that no longer fit are reported, naming the file.
    write(
        d,
        &[(
            "stale.patch",
            "--- a/target.txt\n+++ b/target.txt\n@@ -1,3 +1,3 @@\n one\n-zwei\n+2\n three\n",
        )],
    );
    gui.app.apply_patch_file(&d.join("stale.patch"));
    gui.settle();
    let errors = gui.errors();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("target.txt") && e.contains('1')),
        "{errors:?}"
    );
}

#[test]
fn html_reports_are_saved_or_their_failure_shown() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n<two>\n"), ("b.txt", "one\n2\n")]);
    let mut gui = Gui::start("report", compare(&[d.join("a.txt"), d.join("b.txt")]));
    let out = d.join("report.html");
    gui.app.export_text_html_to(out.clone());
    gui.settle();
    let html = read(&out);
    assert!(html.contains("a.txt") && html.contains("&lt;two&gt;"));
    let saved = trf("msg.saved", &[("file", out.display().to_string())]);
    assert!(gui.toasts().contains(&(false, saved)), "{:?}", gui.toasts());

    let bad = d.join("missing folder").join("report.html");
    gui.app.export_text_html_to(bad.clone());
    gui.settle();
    assert!(!bad.exists());
    let errors = gui.errors();
    assert!(
        errors.iter().any(|e| e.contains("missing folder")),
        "{errors:?}"
    );
}

#[test]
fn files_changed_outside_are_reloaded_unless_edited() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\n2\n")]);
    let mut gui = Gui::start(
        "outside_changes",
        compare(&[d.join("a.txt"), d.join("b.txt")]),
    );

    std::fs::write(d.join("b.txt"), "one\ntwo\n").unwrap();
    touch_later(&d.join("b.txt"), 5);
    gui.refocus();
    assert_eq!(gui.text_view().panes[1].doc.lines, ["one", "two"]);
    assert!(gui.text_view().diff.blocks.is_empty());

    // An edited side is kept, and reloading it is offered.
    gui.key(Modifiers::NONE, Key::ArrowRight);
    gui.key(CMD, Key::End);
    gui.events.push(Event::Paste("mine\n".into()));
    gui.frames(2);
    std::fs::write(d.join("b.txt"), "theirs\n").unwrap();
    touch_later(&d.join("b.txt"), 10);
    gui.refocus();
    assert_eq!(gui.text_view().panes[1].doc.lines, ["one", "two", "mine"]);
    gui.click(tr("btn.reload"));
    assert_eq!(gui.text_view().panes[1].doc.lines, ["theirs"]);
}

#[test]
fn a_missing_side_opens_empty_to_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n")]);
    let mut gui = Gui::start(
        "missing_side",
        compare(&[d.join("a.txt"), d.join("new.txt")]),
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    gui.key(CMD.plus(ALT), Key::ArrowRight);
    gui.key(CMD, Key::S);
    assert_eq!(read(&d.join("new.txt")), "one\n");
}

#[test]
fn more_than_three_sides_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let sides: Vec<PathBuf> = ["a", "b", "c", "d"].iter().map(|n| d.join(n)).collect();
    for s in &sides {
        std::fs::create_dir_all(s).unwrap();
        write(s, &[("same.txt", "one\n")]);
    }
    let gui = Gui::start("too_many_sides", compare(&sides));
    assert!(gui.app.tabs.is_empty());
    let errors = gui.errors();
    let expected = trf("msg.too_many_paths", &[("n", "4".into())]);
    assert!(errors.iter().any(|e| e.contains(&expected)), "{errors:?}");
}

#[cfg(unix)]
#[test]
fn files_that_cannot_be_read_are_reported() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n"), ("locked.txt", "two\n")]);
    let locked = d.join("locked.txt");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        return; // running as root
    }
    let gui = Gui::start("open_error", compare(&[d.join("a.txt"), locked]));
    assert!(gui.app.tabs.is_empty());
    let errors = gui.errors();
    assert!(
        errors.iter().any(|e| e.contains("locked.txt")),
        "{errors:?}"
    );
}

/// The widths of the menus of the menu bar, opened one by one.
fn menu_widths(gui: &mut Gui) -> Vec<(&'static str, f32)> {
    let mut widths = Vec::new();
    for key in MENUS {
        gui.click_menu(tr(key));
        gui.frames(3);
        let open: Vec<f32> = popups(gui).iter().map(|r| r.width()).collect();
        assert_eq!(open.len(), 1, "{key}: {open:?}");
        widths.push((key, open[0]));
        gui.click_menu(tr(key));
        gui.frames(3);
    }
    widths
}

#[test]
fn menus_are_measured_with_their_submenus_contents() {
    let ctx = egui::Context::default();
    let long = "A submenu item much longer than the title of its submenu";
    let mut widths = Vec::new();
    for _ in 0..2 {
        widths.clear();
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            for (layer, list) in [(MENU_MEASURE, false), (MENU_MEASURE, true)] {
                let shown = egui::Area::new(egui::Id::unique(layer).with(list)).show(ctx, |ui| {
                    let mut ui = ui.new_child(egui::UiBuilder::new().layer_id(egui::LayerId::new(
                        egui::Order::Middle,
                        egui::Id::unique(layer),
                    )));
                    // As the menus are measured (see `OxideMergeApp::menu_width`).
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    let add = |ui: &mut egui::Ui| {
                        let _ = ui.button(long);
                    };
                    let r = if list {
                        submenu_list(&mut ui, "Export", add)
                    } else {
                        submenu(&mut ui, "Export", add)
                    };
                    ui.min_rect().width().max(r.response.rect.width())
                });
                widths.push(shown.inner);
            }
        })
        .textures_delta
        .clear();
    }
    // The style first: `fonts` holds the context's lock, which `style` also needs.
    let font = egui::TextStyle::Button.resolve(&ctx.global_style());
    let text = ctx.fonts_mut(|f| {
        f.layout_no_wrap(long.into(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    // A submenu's items widen every menu; a list of recent paths never does.
    assert!(widths[0] >= text, "{widths:?} {text}");
    assert!(widths[1] < text, "{widths:?} {text}");
    // Menus never open narrower than the minimum.
    widgets::set_menu_width(&ctx, 10.0);
    assert_eq!(widgets::menu_width(&ctx), widgets::MENU_MIN_WIDTH);
}

#[test]
fn menu_checks_line_up_whether_on_or_off() {
    let ctx = egui::Context::default();
    let mut rects = Vec::new();
    ctx.run_pass(egui::RawInput::default(), |ctx| {
        egui::Area::new(egui::Id::unique("checks")).show(ctx, |ui| {
            for on in [true, false] {
                rects.push(widgets::menu_check(ui, on, "Tree mode", "").rect);
            }
        });
    })
    .textures_delta
    .clear();
    assert_eq!(rects[0].width(), rects[1].width(), "{rects:?}");
}

#[test]
fn submenus_are_measured_with_their_arrow() {
    let ctx = egui::Context::default();
    let mut widths = Vec::new();
    for _ in 0..2 {
        widths.clear();
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            for layer in [MENU_MEASURE, "elsewhere"] {
                let shown = egui::Area::new(egui::Id::unique(layer)).show(ctx, |ui| {
                    submenu(ui, "Export", |_| ());
                });
                widths.push(shown.response.rect.width());
            }
        })
        .textures_delta
        .clear();
    }
    // Only where the menus are measured, as menus draw the arrow themselves.
    let spacing = ctx.global_style().spacing.item_spacing.x;
    assert!(widths[0] > widths[1] + spacing, "{widths:?}");
}

#[test]
fn menus_scroll_under_their_button_in_short_windows() {
    let mut gui = Gui::start("short_menus", None);
    // The smallest window the app allows.
    gui.screen = egui::vec2(640.0, 400.0);
    gui.frames(3);
    let mut tallest = 0.0_f32;
    for key in MENUS {
        let button = gui.click_menu(tr(key));
        gui.frames(3);
        let open = popups(&gui);
        assert_eq!(open.len(), 1, "{key}: {open:?}");
        let menu = open[0];
        assert!(
            menu.top() >= button.bottom() - 1.0,
            "{key} at {menu:?} under {button:?}"
        );
        assert!(
            menu.bottom() <= gui.screen.y,
            "{key} at {menu:?} is cut off"
        );
        tallest = tallest.max(menu.bottom());
        gui.click_menu(tr(key));
        gui.frames(3);
    }
    // The File menu is longer than the window is tall, so it takes all the room there is.
    assert!(tallest > gui.screen.y - 20.0, "{tallest}");
    // Its items further down come into view as the arrow keys reach them.
    gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    let mut lowest = 0.0_f32;
    for _ in 0..40 {
        gui.key(Modifiers::NONE, Key::ArrowDown);
        gui.frames(2);
        let item = gui.focus.clone().expect("an item has the keyboard");
        assert!(
            item.rect.bottom() <= gui.screen.y,
            "{:?} at {:?} is out of view",
            item.label,
            item.rect
        );
        lowest = lowest.max(item.rect.bottom());
    }
    assert!(lowest > gui.screen.y - 60.0, "{lowest}");
}

#[test]
fn menus_too_tall_for_the_window_scroll_while_the_pointer_rests_on_their_arrows() {
    let mut gui = Gui::start("menu_arrows", None);
    gui.screen = egui::vec2(640.0, 400.0);
    gui.frames(3);
    gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    let menu = *popups(&gui).first().expect("the File menu is open");
    // Where the first item is, if it is still in view.
    let first = |gui: &Gui| {
        gui.widgets
            .iter()
            .find(|w| named(w, tr("menu.new")) && menu.contains(w.rect.center()))
            .map(|w| w.rect.top())
    };
    let top = first(&gui).expect("the first item shows");
    // A click on an arrow neither closes the menu nor picks the item under it.
    let shown = |gui: &Gui| popups(gui).len();
    let rest = |gui: &mut Gui, y: f32, frames: usize| {
        gui.events
            .push(Event::PointerMoved(egui::pos2(menu.center().x, y)));
        gui.frames(frames);
    };
    gui.click_at(egui::pos2(menu.center().x, menu.bottom() - 12.0));
    assert_eq!(shown(&gui), 1, "the menu stays open");
    // Scrolled a little, it has an arrow at the top too.
    rest(&mut gui, menu.bottom() - 12.0, 10);
    gui.click_at(egui::pos2(menu.center().x, menu.top() + 12.0));
    assert_eq!(shown(&gui), 1, "the menu stays open");
    rest(&mut gui, menu.bottom() - 12.0, 60);
    assert!(
        first(&gui).is_none_or(|y| y < top - 40.0),
        "{:?} from {top}",
        first(&gui)
    );
    assert!(
        gui.app.open_dlg.is_none(),
        "resting on the arrow picks nothing"
    );
    // Long enough to scroll all the way up from wherever it got to, however long the menu.
    rest(&mut gui, menu.top() + 12.0, 200);
    assert_eq!(first(&gui), Some(top));
    // Opened again, it starts at the top, however far down it was left.
    rest(&mut gui, menu.bottom() - 12.0, 60);
    assert_ne!(first(&gui), Some(top));
    gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    assert_eq!(first(&gui), Some(top));
    // The clicks taken back on the arrows leave the next click to work as ever.
    let tabs = gui.app.tabs.len();
    gui.click_at(egui::pos2(menu.center().x, top + 8.0));
    assert_eq!(shown(&gui), 0, "an item closes the menu");
    assert_eq!(gui.app.tabs.len(), tabs + 1, "New opened a tab");
}

#[test]
fn submenus_out_of_sight_or_under_an_arrow_do_not_open() {
    let mut gui = Gui::start("hidden_submenus", None);
    gui.screen = egui::vec2(640.0, 400.0);
    gui.frames(3);
    let file = gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    let menu = *popups(&gui).first().expect("the File menu is open");
    // Resting on the bottom arrow, the File menu scrolls its submenus under it.
    let at = egui::pos2(menu.center().x, menu.bottom() - 12.0);
    let mut under = false;
    for _ in 0..60 {
        gui.events.push(Event::PointerMoved(at));
        gui.frame();
        under |= file_submenu_at(&gui, at);
        assert_eq!(popups(&gui).len(), 1, "a submenu opened under the arrow");
    }
    assert!(under, "no submenu went under the arrow");
    // Scrolled down, the submenus near its top are out of sight, over the menu bar.
    let hidden = file_submenu_within(&gui, file)
        .expect("no submenu went out of sight under the File button");
    gui.events.push(Event::PointerMoved(hidden));
    gui.frames(10);
    assert_eq!(popups(&gui).len(), 1, "a submenu out of sight opened");
    // One in sight opens as ever.
    let export = gui
        .widgets
        .iter()
        .find(|w| named(w, tr("menu.export")) && menu.contains(w.rect.center()))
        .expect("Export is in sight")
        .rect;
    gui.events.push(Event::PointerMoved(export.center()));
    gui.frames(10);
    assert_eq!(popups(&gui).len(), 2, "Export opened");
}

#[test]
fn submenus_kept_from_opening_under_the_pointer_still_take_the_keyboard() {
    let mut gui = Gui::start("hidden_submenus_keyboard", None);
    gui.screen = egui::vec2(640.0, 400.0);
    gui.frames(3);
    let file = gui.click_menu(tr(MENUS[0]));
    gui.frames(3);
    let menu = *popups(&gui).first().expect("the File menu is open");
    let at = egui::pos2(menu.center().x, menu.bottom() - 12.0);
    gui.events.push(Event::PointerMoved(at));
    gui.frames(60);
    // The pointer left where the submenus near the top went out of sight, once they stop.
    gui.events.push(Event::PointerMoved(file.center()));
    gui.frames(10);
    let hidden = file_submenu_within(&gui, file)
        .expect("no submenu went out of sight under the File button");
    gui.events.push(Event::PointerMoved(hidden));
    gui.frames(3);
    assert!(
        file_submenu_at(&gui, hidden),
        "the submenu under the File button moved away"
    );
    let mut reached = Vec::new();
    for _ in 0..40 {
        gui.key(Modifiers::NONE, Key::ArrowUp);
        gui.frames(2);
        reached.extend(gui.focus.as_ref().map(|w| w.label.clone()));
    }
    assert!(
        reached.iter().any(|l| l == tr("menu.projects_jobs")),
        "{reached:?}"
    );
}

#[test]
fn menus_open_equally_wide() {
    let same = |widths: &[(&str, f32)]| widths.iter().all(|(_, w)| (w - widths[0].1).abs() < 0.01);
    let mut fresh = Vec::new();
    for lang in ["en", "ja", "zh-TW"] {
        let cfg = Config {
            language: lang.into(),
            ..config()
        };
        let widths = menu_widths(&mut Gui::with_config("menus_open_equally_wide", cfg, None));
        assert!(same(&widths), "{lang}: {widths:?}");
        fresh.push(widths[0].1);
    }
    // The fonts change a frame after the language.
    let cfg = Config {
        language: "ja".into(),
        ..config()
    };
    let mut gui = Gui::with_config("menus_open_equally_wide", cfg, None);
    gui.app.cfg.language = "zh-TW".into();
    gui.settle();
    let widths = menu_widths(&mut gui);
    assert!(same(&widths), "switched: {widths:?}");
    assert!(
        (widths[0].1 - fresh[2]).abs() < 0.01,
        "{widths:?} {fresh:?}"
    );
}

#[test]
fn requested_locks_reach_folder_and_image_views() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let roots: Vec<_> = ["left", "right"].iter().map(|n| d.join(n)).collect();
    for p in &roots {
        std::fs::create_dir(p).unwrap();
    }
    let request = |paths| {
        Some(OpenRequest {
            paths,
            read_only: vec![true, true],
            ..Default::default()
        })
    };
    let gui = Gui::start("locked_folders", request(roots));
    let Content::Folder(v) = &gui.app.tabs[0].content else {
        panic!("folder view");
    };
    assert_eq!(v.read_only, [true, true]);
    let images: Vec<_> = ["left.png", "right.png"]
        .iter()
        .map(|n| d.join(n))
        .collect();
    for (i, path) in images.iter().enumerate() {
        image::RgbaImage::from_pixel(1, 1, image::Rgba([i as u8 * 255, 0, 0, 255]))
            .save(path)
            .unwrap();
    }
    let before = std::fs::read(&images[0]).unwrap();
    let mut gui = Gui::start("locked_images", request(images.clone()));
    let Content::Image(v) = &mut gui.app.tabs[0].content else {
        panic!("image view");
    };
    assert_eq!(v.read_only, [true, true]);
    v.first_diff();
    v.copy_current(false);
    v.save().unwrap();
    assert!(!v.is_modified());
    assert_eq!(std::fs::read(&images[0]).unwrap(), before);
}

#[test]
fn projects_round_trip_labels_locks_and_move_with_their_files() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    std::fs::create_dir(&original).unwrap();
    write(&original, &[("a.txt", "old\n"), ("b.txt", "new\n")]);
    let paths = vec![original.join("a.txt"), original.join("b.txt")];
    let labels = vec![Some("Original".into()), Some("Edited".into())];
    let gui = Gui::start(
        "project_metadata",
        Some(OpenRequest {
            paths,
            descriptions: labels.clone(),
            read_only: vec![true, false],
            ..Default::default()
        }),
    );
    let file = original.join("compare.oxmproj");
    let project = gui.app.tabs[0]
        .project()
        .unwrap()
        .relative_to(&file)
        .unwrap();
    assert_eq!(project.paths, ["a.txt", "b.txt"]);
    assert_eq!(project.descriptions, labels);
    assert_eq!(project.read_only, [true, false]);
    project.save(&file).unwrap();
    let moved = dir.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    let request = project_request(&moved.join("compare.oxmproj")).unwrap();
    let gui = Gui::start("project_reopened", Some(request));
    let view = gui.text_view();
    assert!(view.panes[0].doc.read_only);
    assert!(!view.panes[1].doc.read_only);
    assert_eq!(view.panes[0].doc.description.as_deref(), Some("Original"));
    assert_eq!(view.panes[1].doc.description.as_deref(), Some("Edited"));
    assert_eq!(
        view.panes[0].doc.path.as_deref(),
        Some(moved.join("a.txt").as_path())
    );
}

#[test]
fn applying_byte_patches_preserves_boms_and_crlf() {
    for (old, new) in [
        ("foo\nbar\n", "foo"),
        ("one\r\n", "one\n"),
        ("", "\u{feff}"),
        ("\u{feff}old\r\n", "\u{feff}new\r\n"),
        ("one\r\ntwo\n", "ONE\r\ntwo\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, &[("target.txt", old)]);
        let a = text::doc_from_bytes(None, old.as_bytes(), Some(text::TextEncoding::Utf8));
        let b = text::doc_from_bytes(None, new.as_bytes(), Some(text::TextEncoding::Utf8));
        let input = patch::PatchInput {
            name_a: "a/target.txt",
            name_b: "b/target.txt",
            date_a: "",
            date_b: "",
            a: &a.lines,
            b: &b.lines,
            final_newline_a: a.final_newline,
            final_newline_b: b.final_newline,
        };
        let opts = crate::core::diff::DiffOptions {
            ignore_eol: false,
            ignore_codepage: false,
            ..Default::default()
        };
        let patch =
            patch::make_doc_patch(&input, &[&a, &b], &opts, PatchStyle::Unified, 3).unwrap();
        std::fs::write(d.join("fix.patch"), patch).unwrap();
        let mut gui = Gui::start("patch_formats", None);
        gui.app.apply_patch_file(&d.join("fix.patch"));
        assert!(gui.errors().is_empty(), "{:?}", gui.errors());
        gui.app.tabs.last_mut().unwrap().save().unwrap();
        assert_eq!(std::fs::read(d.join("target.txt")).unwrap(), new.as_bytes());
    }
}

#[test]
fn csv_multiline_cells_open_edit_and_save_in_the_real_view() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.csv", "id,note\r\n1,\"old\r\nworld\"\r\n"),
            ("b.csv", "id,note\r\n1,\"new\r\nworld\"\r\n"),
        ],
    );
    let mut gui = Gui::start(
        "table_cell_edit",
        compare(&[d.join("a.csv"), d.join("b.csv")]),
    );
    assert!(gui.shows("Record"));
    assert!(gui.shows_text("old"));
    gui.click("old\r\nworld");
    gui.click("old\r\nworld");
    assert!(gui.shows("Edit cell"));
    gui.key(CMD, Key::A);
    gui.type_text("edited, value\nnext");
    gui.click("OK");
    gui.key(CMD, Key::S);
    assert_eq!(
        read(&d.join("a.csv")),
        "id,note\r\n1,\"edited, value\nnext\"\r\n"
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn folder_review_reopens_without_sources_and_keeps_annotations_and_options() {
    let source = tempfile::tempdir().unwrap();
    let d = source.path();
    write(
        d,
        &[
            ("left/a.txt", "Hello\n"),
            ("right/a.txt", "hello\n"),
            ("left/skip.tmp", "skip"),
        ],
    );
    let mut cfg = config();
    cfg.diff.ignore_case = true;
    cfg.filter = "*.txt".into();
    let mut original = Gui::with_config(
        "review_capture",
        cfg,
        compare(&[d.join("left"), d.join("right")]),
    );
    let out = tempfile::tempdir().unwrap();
    let bundle = out.path().join("review.oxmreview");
    if let Content::Folder(view) = &mut original.app.tabs[0].content {
        view.notes
            .insert("a.txt".into(), "Check <this> & that".into());
        view.save_review(&bundle, false).unwrap();
    } else {
        panic!("Expected a folder comparison");
    }
    drop(original);
    drop(source);
    let mut gui = Gui::start("review_reopen", compare(&[bundle]));
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder review");
    };
    assert_eq!(view.notes["a.txt"], "Check <this> & that");
    assert!(view.read_only.iter().all(|&value| value));
    assert!(view.items.iter().all(|i| i.name != "skip.tmp"));
    assert_eq!(view.outcome(), Outcome::Identical);
    let html = view.report(ReportFormat::Html, &gui.app.cfg);
    assert!(html.contains("Check &lt;this&gt; &amp; that"));
    gui.app.cfg.diff.ignore_case = false;
    gui.app.run(Cmd::Refresh, &gui.ctx);
    gui.settle();
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder review");
    };
    assert_eq!(view.outcome(), Outcome::Identical);
    assert!(gui.shows_text("read-only snapshot"));
}

#[test]
fn review_pane_decisions_survive_a_saved_review_and_follow_renamed_files() {
    use crate::core::review_state::State;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "a old\n"),
            ("right/a.txt", "a new\n"),
            ("left/b.txt", "b old\n"),
            ("right/b.txt", "b new\n"),
            ("left/c.txt", "c old\n"),
            ("right/c.txt", "c new\n"),
        ],
    );
    let mut gui = Gui::with_config(
        "review_pane",
        config(),
        compare(&[d.join("left"), d.join("right")]),
    );
    let counts = |unreviewed: usize, accepted: usize, needs: usize| {
        format!(
            "{}: {unreviewed} · {}: {accepted} · {}: {needs} ·",
            tr("workspace.unreviewed"),
            tr("workspace.accepted"),
            tr("workspace.needs_action")
        )
    };
    gui.click(tr("drawer.toggle"));
    assert!(gui.shows(tr("workspace.title")));
    assert!(gui.shows_text(&counts(3, 0, 0)));
    gui.click(tr("workspace.next"));
    gui.click(tr("workspace.accepted"));
    assert!(gui.shows_text(&counts(2, 1, 0)));
    gui.click(tr("workspace.next"));
    gui.click(tr("workspace.needs_action"));
    let comment = gui
        .widgets
        .iter()
        .find(|w| w.role == accesskit::Role::MultilineTextInput)
        .expect("a review comment box")
        .rect;
    gui.click_at(comment.center());
    gui.type_text("Check the wording");
    gui.settle();
    assert!(gui.shows_text(&counts(1, 1, 1)));
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder comparison");
    };
    assert_eq!(view.workspace.entries["a.txt"].state, State::Accepted);
    assert_eq!(view.workspace.entries["b.txt"].state, State::NeedsAction);
    assert_eq!(view.workspace.entries["b.txt"].comment, "Check the wording");
    let filter = gui
        .widgets
        .iter()
        .find(|w| named(w, tr("workspace.accepted")))
        .unwrap()
        .rect;
    gui.click_at(filter.center());
    assert!(gui.shows("a.txt") && !gui.shows("c.txt"));
    gui.click(tr("analysis.all"));
    assert!(gui.shows("c.txt"));
    gui.click(tr("workspace.next"));
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder comparison");
    };
    assert_eq!(view.workspace.current.as_deref(), Some("c.txt"));
    let out = tempfile::tempdir().unwrap();
    let bundle = out.path().join("review.oxmreview");
    view.save_review(&bundle, false).unwrap();
    drop(gui);

    write(
        d,
        &[
            ("next-left/renamed.txt", "a old\n"),
            ("next-right/renamed.txt", "a new\n"),
            ("next-left/b.txt", "b old\n"),
            ("next-right/b.txt", "b newer\n"),
            ("next-left/c.txt", "c old\n"),
            ("next-right/c.txt", "c new\n"),
        ],
    );
    let mut gui = Gui::start("review_pane_reopen", compare(&[bundle]));
    let Content::Folder(view) = &mut gui.app.tabs[0].content else {
        panic!("Expected a folder review");
    };
    assert_eq!(view.workspace.entries["a.txt"].state, State::Accepted);
    assert_eq!(view.workspace.entries["b.txt"].comment, "Check the wording");
    assert!(view.workspace.entries.values().all(|e| !e.stale));
    view.roots = vec![d.join("next-left"), d.join("next-right")];
    view.display_roots = view.roots.iter().map(|p| p.display().to_string()).collect();
    view.read_only = vec![true; 2];
    view.start_compare();
    gui.settle();
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder review");
    };
    let renamed = &view.workspace.entries["renamed.txt"];
    assert_eq!(renamed.state, State::Accepted);
    assert_eq!(renamed.renamed_from.as_deref(), Some("a.txt"));
    assert!(!renamed.stale && !renamed.ambiguous_rename);
    assert!(!view.workspace.entries.contains_key("a.txt"));
    assert!(view.workspace.entries["b.txt"].stale);
    assert!(!view.workspace.entries.contains_key("c.txt"));
    gui.click("renamed.txt");
    gui.click(tr("drawer.toggle"));
    assert!(gui.shows(&trf("workspace.renamed_from", &[("path", "a.txt".into())])));
    gui.click("b.txt");
    assert!(gui.shows(tr("workspace.stale")));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[cfg(unix)]
#[test]
fn converted_documents_stay_read_only_in_file_and_all_differences_views() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.docx", "\0container\nOLD\n"),
            ("right/a.docx", "\0container\nNEW\n"),
        ],
    );
    let mut cfg = config();
    cfg.unpackers = vec![Unpacker {
        enabled: true,
        pattern: "*.docx".into(),
        command: "tail -n +2 ${file}".into(),
    }];
    let mut gui = Gui::with_config(
        "converter_readonly",
        cfg.clone(),
        compare(&[d.join("left/a.docx"), d.join("right/a.docx")]),
    );
    assert!(gui.text_view().panes.iter().all(|p| p.doc.read_only));
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.app.run(Cmd::Save, &gui.ctx);
    assert_eq!(read(&d.join("right/a.docx")), "\0container\nNEW\n");
    let mut folder = Gui::with_config(
        "converter_folder",
        cfg,
        compare(&[d.join("left"), d.join("right")]),
    );
    folder.app.run(Cmd::AllDiffs, &folder.ctx);
    folder.settle();
    folder.app.run(Cmd::CopyRight, &folder.ctx);
    folder.app.run(Cmd::Save, &folder.ctx);
    folder.settle();
    assert_eq!(read(&d.join("right/a.docx")), "\0container\nNEW\n");
}

#[cfg(unix)]
#[test]
fn converted_review_opens_documents_without_running_local_converters() {
    let source = tempfile::tempdir().unwrap();
    let d = source.path();
    write(
        d,
        &[
            ("left/a.docx", "\0container-one\nold text\n"),
            ("right/a.docx", "\0container-two\nnew text\n"),
        ],
    );
    let mut cfg = config();
    cfg.unpackers = vec![Unpacker {
        enabled: true,
        pattern: "*.docx".into(),
        command: "tail -n +2 ${file}".into(),
    }];
    let mut original = Gui::with_config(
        "converted_review_capture",
        cfg,
        compare(&[d.join("left"), d.join("right")]),
    );
    original.click(tr("drawer.toggle"));
    original.click(tr("workspace.next"));
    original.click(tr("workspace.accepted"));
    original.settle();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("review.oxmreview");
    let Content::Folder(view) = &original.app.tabs[0].content else {
        panic!("folder expected");
    };
    assert_eq!(
        view.workspace.entries["a.docx"].state,
        crate::core::review_state::State::Accepted
    );
    view.save_review(&path, false).unwrap();
    drop(original);
    drop(source);
    let mut cfg = config();
    cfg.unpackers = vec![Unpacker {
        enabled: true,
        pattern: "*.docx".into(),
        command: "exit 99 # ${file}".into(),
    }];
    let mut gui = Gui::with_config("converted_review_reopen", cfg, compare(&[path]));
    gui.settle();
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("folder review expected");
    };
    let decision = &view.workspace.entries["a.docx"];
    assert_eq!(decision.state, crate::core::review_state::State::Accepted);
    assert!(
        !decision.stale,
        "a decision on a converted document survives the reopen"
    );
    gui.click("a.docx");
    gui.click("a.docx");
    assert_eq!(gui.text_view().panes[0].doc.lines, ["old text"]);
    assert!(gui.text_view().panes.iter().all(|p| p.doc.read_only));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn table_keyboard_navigation_cell_editing_and_go_to_line_work() {
    let dir = tempfile::tempdir().unwrap();
    let left = format!(
        "id,note\n{}",
        (0..24)
            .map(|i| format!("{i},left-{i}\n"))
            .collect::<String>()
    );
    let right = left.replace("left-", "right-");
    write(dir.path(), &[("left.csv", &left), ("right.csv", &right)]);
    let mut gui = Gui::start(
        "table_keyboard",
        compare(&[dir.path().join("left.csv"), dir.path().join("right.csv")]),
    );
    gui.click("left-0");
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let line = |gui: &Gui| {
        let v = gui.text_view();
        v.diff.rows[v.cursor_row.unwrap()]
            .line(v.active_pane)
            .unwrap()
    };
    assert_eq!(line(&gui), 2);
    gui.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(line(&gui), 1);
    gui.key(Modifiers::NONE, Key::PageDown);
    assert!(line(&gui) > 1);
    gui.key(CMD, Key::Home);
    assert_eq!(line(&gui), 0);
    gui.key(CMD, Key::End);
    assert_eq!(line(&gui), 24);
    gui.key(Modifiers::NONE, Key::F2);
    assert!(gui.shows("Edit cell"));
    gui.key(CMD, Key::A);
    gui.type_text("edited-last");
    gui.click("OK");
    assert_eq!(gui.text_view().panes[0].doc.lines[24], "23,edited-last");
    gui.key(Modifiers::NONE, Key::Enter);
    assert!(gui.shows("Edit cell"));
    gui.click("Cancel");
    gui.key(CMD, Key::G);
    assert!(gui.text_view().goto.is_some());
    gui.type_text("3");
    gui.click("OK");
    assert!(gui.text_view().goto.is_none());
    assert_eq!(line(&gui), 2);
    gui.key(Modifiers::NONE, Key::PageUp);
    assert_eq!(line(&gui), 0);
}

#[test]
fn table_rows_matched_by_key_from_the_toolbar_copy_and_reopen_from_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let left = "id,name,price\n1,Ann,10\n2,Bob,20\n3,Cy,30\n";
    let right = "id,name,price\n3,Cy,30\n2,Bob,20\n1,Ann,11\n";
    write(dir.path(), &[("left.csv", left), ("right.csv", right)]);
    let mut gui = Gui::start(
        "table_match_by_key",
        compare(&[dir.path().join("left.csv"), dir.path().join("right.csv")]),
    );
    assert!(gui.shows("Match rows by"));
    gui.click("Position");
    gui.click("Key columns");
    assert!(gui.text_view().table_match.by_key());
    assert!(gui.shows_text("Modified 1 · Reordered 2 · Ambiguous 0"));
    assert_eq!(gui.text_view().diff_position(), (None, 2));
    gui.click("Ignore row order");
    assert!(gui.shows("(row order ignored)"));
    assert_eq!(gui.text_view().diff_position(), (None, 1));

    gui.app.run(Cmd::NextDiff, &gui.ctx);
    gui.settle();
    assert_eq!(gui.text_view().diff_position(), (Some(0), 1));
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.settle();
    assert_eq!(
        gui.text_view().panes[1].doc.text(),
        right.replace("1,Ann,11", "1,Ann,10")
    );
    assert_eq!(gui.text_view().diff_position().1, 0);

    let file = dir.path().join("table.oxmproj");
    let project = gui.app.tabs[0]
        .project()
        .unwrap()
        .relative_to(&file)
        .unwrap();
    assert!(project.table.as_ref().is_some_and(|m| m.ignore_order));
    project.save(&file).unwrap();
    let gui = Gui::start(
        "table_match_reopened",
        Some(project_request(&file).unwrap()),
    );
    assert!(gui.text_view().table_match.by_key());
    assert!(gui.shows("(row order ignored)"));
}

#[test]
fn clicking_multiline_cell_selects_and_merges_a_later_line_difference() {
    let dir = tempfile::tempdir().unwrap();
    let left = "id,note\n1,\"same\nold\"\n";
    let right = "id,note\n1,\"same\nnew\"\n";
    write(dir.path(), &[("left.csv", left), ("right.csv", right)]);
    let mut gui = Gui::start(
        "table_record_selection",
        compare(&[dir.path().join("left.csv"), dir.path().join("right.csv")]),
    );
    gui.click("same\nold");
    assert!(gui.text_view().cur_block.is_some());
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.settle();
    assert_eq!(gui.text_view().panes[1].doc.text(), left);
}

#[test]
fn meld_link_arrows_and_compact_pane_clicks_target_the_correct_change() {
    let dir = tempfile::tempdir().unwrap();
    let left = "start\ncommon\nend\nleft value\n";
    let right = "start\ninserted\nextra\ncommon\nend\nright value\n";
    write(dir.path(), &[("left.txt", left), ("right.txt", right)]);
    let mut gui = Gui::start(
        "meld_link_map",
        compare(&[dir.path().join("left.txt"), dir.path().join("right.txt")]),
    );
    // The last button belongs to the later block, below the unequal-length insertion.
    gui.click(tr("cmd.copy_right"));
    assert_eq!(
        gui.text_view().panes[1].doc.text(),
        right.replace("right value", "left value")
    );
    gui.key(CMD, Key::Z);
    assert_eq!(gui.text_view().panes[1].doc.text(), right);

    let header = gui
        .widgets
        .iter()
        .find(|w| w.label.starts_with("left.txt   "))
        .unwrap()
        .rect;
    let row_h = gui
        .ctx
        .fonts_mut(|f| f.row_height(&egui::FontId::monospace(config().font_size)))
        + 2.0;
    // Left line 4 is directly below line 3, with no two-row insertion spacer.
    gui.click_at(Pos2::new(
        header.left() + 100.0,
        header.center().y + 18.0 + 3.5 * row_h,
    ));
    let v = gui.text_view();
    let r = v.cursor_row.expect("clicked a real line");
    assert_eq!(v.active_pane, 0);
    assert_eq!(v.diff.rows[r].line(0), Some(3));
    assert!(
        v.cur_block.is_some(),
        "row: {:?}, blocks: {:?}",
        v.diff.rows[r],
        v.diff.blocks
    );
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.settle();
    assert_eq!(
        gui.text_view().panes[1].doc.text(),
        right.replace("right value", "left value")
    );
}

#[test]
fn compact_pane_keyboard_skips_the_other_sides_insertions() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("left.txt", "start\ncommon\nend\n"),
            ("right.txt", "start\ninserted\nextra\ncommon\nend\n"),
        ],
    );
    let mut gui = Gui::start(
        "meld_keyboard",
        compare(&[dir.path().join("left.txt"), dir.path().join("right.txt")]),
    );
    gui.key(CMD, Key::Home);
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let v = gui.text_view();
    assert_eq!(v.diff.rows[v.cursor_row.unwrap()].line(0), Some(1));
    // F2 puts the caret at the end of the line; Shift+Home selects it, and typing replaces it.
    gui.key(Modifiers::NONE, Key::F2);
    gui.key(Modifiers::SHIFT, Key::Home);
    gui.type_text("edited common");
    assert_eq!(gui.text_view().panes[0].doc.lines[1], "edited common");
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let v = gui.text_view();
    assert_eq!(v.diff.rows[v.cursor_row.unwrap()].line(0), Some(2));
}

#[test]
fn semantic_values_copy_between_sides_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let left = "{\n  \"name\": \"svc\",\n  \"timeout\": 30,\n  \"old\": true\n}\n";
    let right = "{\n  \"name\": \"svc\",\n  \"timeout\": 60\n}\n";
    write(dir.path(), &[("a.json", left), ("b.json", right)]);
    let mut gui = Gui::start(
        "semantic_copy",
        compare(&[dir.path().join("a.json"), dir.path().join("b.json")]),
    );
    gui.click(tr("analysis.semantic"));
    gui.wait_until("semantic report", |gui| gui.shows_text("30 → 60"));
    // the last button belongs to the last change, the timeout
    gui.click(tr("analysis.copy_to_left"));
    gui.wait_until("copied", |gui| {
        gui.text_view().panes[0].doc.text().contains("60")
    });
    gui.wait_until("one change left", |gui| {
        !gui.shows_text("30 → 60") && gui.shows_text("true → ∅")
    });
    gui.click(tr("analysis.copy_to_right"));
    gui.wait_until("same", |gui| gui.shows(tr("analysis.semantic_identical")));
    assert_eq!(
        gui.text_view().panes[0].doc.text(),
        left.replace("30", "60")
    );
    assert_eq!(
        gui.text_view().panes[1].doc.text(),
        "{\n  \"name\": \"svc\",\n  \"timeout\": 60,\n  \"old\": true\n}\n"
    );
    gui.app.run(Cmd::Undo, &gui.ctx);
    gui.settle();
    assert_eq!(gui.text_view().panes[1].doc.text(), right);
    // nothing is saved until asked
    assert_eq!(read(&dir.path().join("b.json")), right);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn semantic_three_way_shows_conflicts_and_takes_a_side_into_the_middle() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("left.yaml", "port: 8080\nmode: fast\nname: a\n"),
            ("base.yaml", "port: 80\nmode: slow\nname: a\n"),
            ("right.yaml", "port: 80\nmode: safe\nname: b\n"),
        ],
    );
    let mut gui = Gui::start(
        "semantic_three_way",
        compare(&[
            dir.path().join("left.yaml"),
            dir.path().join("base.yaml"),
            dir.path().join("right.yaml"),
        ]),
    );
    gui.click(tr("analysis.semantic"));
    let conflict = format!("{} · $[\"mode\"]", tr("analysis.state_conflict"));
    gui.wait_until("three-way report", |gui| gui.shows(&conflict));
    assert!(gui.shows(&format!("{} · $[\"port\"]", tr("analysis.state_left"))));
    assert!(gui.shows(&format!("{} · $[\"name\"]", tr("analysis.state_right"))));
    assert!(gui.shows_text(&trf(
        "analysis.three_way_summary",
        &[("n", "3".into()), ("conflicts", "1".into())]
    )));
    gui.click(tr("analysis.state_conflict"));
    assert!(!gui.shows(&format!("{} · $[\"port\"]", tr("analysis.state_left"))));
    gui.click(tr("analysis.use_right"));
    gui.wait_until("taken", |gui| {
        gui.text_view().panes[1].doc.text() == "port: 80\nmode: safe\nname: a\n"
    });
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn semantic_view_jumps_back_to_source_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("a.json", "{\n  \"timeout\": 30\n}\n"),
            ("b.json", "{\n  \"timeout\": 60\n}\n"),
        ],
    );
    let mut gui = Gui::start(
        "semantic_view",
        compare(&[dir.path().join("a.json"), dir.path().join("b.json")]),
    );
    gui.click(tr("analysis.semantic"));
    gui.wait_until("semantic report", |gui| {
        gui.shows_text("timeout") && gui.shows_text("30 → 60")
    });
    assert!(gui.shows(tr("analysis.semantic_policy")));
    gui.click(&format!("{} 2:2", tr("analysis.source")));
    assert_eq!(gui.text_view().active_pane, 1);
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "  \"timeout\": 60");
    assert_eq!(
        read(&dir.path().join("b.json")),
        "{\n  \"timeout\": 60\n}\n"
    );
}

#[test]
fn xml_tree_view_aligns_rows_navigates_and_shows_source_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let a = "<catalog>\n  <book id=\"1\">\n    <title>Rust</title>\n  </book>\n  <book id=\"2\" year=\"2020\"/>\n</catalog>\n";
    let b = "<catalog>\n  <book id=\"0\"/>\n  <book id=\"1\">\n    <title>Rust</title>\n  </book>\n  <book id=\"2\" year=\"2021\"/>\n</catalog>\n";
    write(dir.path(), &[("a.xml", a), ("b.xml", b)]);
    let mut gui = Gui::start(
        "xml_tree_view",
        compare(&[dir.path().join("a.xml"), dir.path().join("b.xml")]),
    );
    gui.click(tr("analysis.tree"));
    let summary = trf(
        "analysis.tree_summary",
        &[
            ("added", "1".into()),
            ("removed", "0".into()),
            ("modified", "1".into()),
            ("moved", "0".into()),
            ("ambiguous", "0".into()),
        ],
    );
    gui.wait_until("tree summary", |gui| gui.shows(&summary));
    assert!(gui.shows(tr("analysis.tree_policy")));
    // the inserted book is one row with a placeholder on the left
    assert!(gui.shows_text(&format!(
        "{}: {} | <book>",
        tr("analysis.added"),
        tr("analysis.tree_missing")
    )));
    gui.key(Modifiers::ALT, Key::End);
    assert!(gui.text_view().tree.view.selected.is_some());
    assert!(gui.shows(&trf(
        "analysis.tree_position",
        &[("n", "2".into()), ("total", "2".into())]
    )));
    assert!(gui.shows_text("@year = \"2021\""));
    gui.key(Modifiers::ALT, Key::ArrowUp);
    assert!(gui.shows(&trf(
        "analysis.tree_position",
        &[("n", "1".into()), ("total", "2".into())]
    )));
    gui.key(Modifiers::ALT, Key::End);
    gui.click(&format!("{} 2:6", tr("analysis.source")));
    assert_eq!(gui.text_view().active_pane, 1);
    assert!(!gui.shows(tr("analysis.tree_policy")));
    assert_eq!(read(&dir.path().join("a.xml")), a);
    assert_eq!(read(&dir.path().join("b.xml")), b);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn value_tree_view_aligns_json_yaml_and_toml_by_key() {
    let dir = tempfile::tempdir().unwrap();
    let a = "{\"name\":\"w\",\"server\":{\"port\":8080,\"timeout\":30},\"tags\":[\"x\",\"y\"],\"users\":[{\"id\":1},{\"id\":2}]}";
    let b = "{\n  \"server\": {\"timeout\": 45, \"port\": 8080},\n  \"name\": \"w\",\n  \"tags\": [\"x\", \"y\", \"z\"],\n  \"users\": [{\"id\": 1}, {\"id\": 2}]\n}\n";
    write(dir.path(), &[("a.json", a), ("b.json", b)]);
    let mut gui = Gui::start(
        "value_tree_view",
        compare(&[dir.path().join("a.json"), dir.path().join("b.json")]),
    );
    gui.click(tr("analysis.tree"));
    let summary = trf(
        "analysis.tree_summary",
        &[
            ("added", "1".into()),
            ("removed", "0".into()),
            ("modified", "1".into()),
            ("moved", "0".into()),
            ("ambiguous", "0".into()),
        ],
    );
    gui.wait_until("tree summary", |gui| gui.shows(&summary));
    assert!(gui.shows(tr("analysis.tree_policy_values")));
    assert!(gui.shows(&trf(
        "analysis.tree_identified",
        &[("n", "2".into()), ("member", "id".into())]
    )));
    assert!(gui.shows_text(&format!(
        "{}: timeout: 30 | timeout: 45",
        tr("analysis.modified")
    )));
    assert!(gui.shows_text(&format!(
        "{}: {} | [2]: \"z\"",
        tr("analysis.added"),
        tr("analysis.tree_missing")
    )));
    gui.key(Modifiers::ALT, Key::Home);
    assert!(gui.shows_text("$.server.timeout"));
    assert_eq!(read(&dir.path().join("a.json")), a);
    assert_eq!(read(&dir.path().join("b.json")), b);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn xml_tree_key_rules_pair_reordered_elements_and_warn_about_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (
                "a.xml",
                "<lib><book id='1'>A</book><book id='2'>B</book></lib>",
            ),
            (
                "b.xml",
                "<lib><book id='2'>B</book><book id='1'>A2</book></lib>",
            ),
            (
                "c.xml",
                "<lib><book id='1'>A</book><book id='1'>B</book></lib>",
            ),
        ],
    );
    let mut gui = Gui::start(
        "xml_tree_keys",
        compare(&[dir.path().join("a.xml"), dir.path().join("b.xml")]),
    );
    gui.click(tr("analysis.tree"));
    gui.wait_until("tree", |gui| gui.shows(tr("analysis.tree_expand_changes")));
    gui.click(tr("analysis.tree_keys"));
    gui.type_text("book@id");
    let keyed = trf("analysis.tree_keyed", &[("n", "2".into())]);
    gui.wait_until("keyed pairing", |gui| gui.shows(&keyed));
    assert!(gui.shows_text(tr("analysis.moved")));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
    let mut gui = Gui::start(
        "xml_tree_duplicates",
        compare(&[dir.path().join("a.xml"), dir.path().join("c.xml")]),
    );
    gui.click(tr("analysis.tree"));
    gui.wait_until("tree", |gui| gui.shows(tr("analysis.tree_expand_changes")));
    gui.click(tr("analysis.tree_keys"));
    gui.type_text("book@id");
    let warning = trf(
        "analysis.tree_key_warning",
        &[
            ("rule", "book@id".into()),
            ("duplicates", "2".into()),
            ("missing", "0".into()),
        ],
    );
    gui.wait_until("duplicate key warning", |gui| gui.shows(&warning));
}

#[test]
fn semantic_tree_groups_paths_and_an_array_key_pairs_moved_elements() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            (
                "a.json",
                r#"{"items":[{"id":"a","v":1},{"id":"b","v":2}],"name":"x"}"#,
            ),
            (
                "b.json",
                r#"{"items":[{"id":"b","v":2},{"id":"a","v":10}],"name":"y"}"#,
            ),
        ],
    );
    let mut gui = Gui::start(
        "semantic_tree",
        compare(&[dir.path().join("a.json"), dir.path().join("b.json")]),
    );
    gui.click(tr("analysis.semantic"));
    let wait_for = |gui: &mut Gui, label: &str| gui.wait_until(label, |gui| gui.shows(label));
    wait_for(&mut gui, "[\"items\"] (4)");
    assert!(gui.shows("[\"name\"] (1)"));
    assert!(gui.shows("[0] (2)"));
    gui.click(tr("analysis.array_key"));
    gui.type_text("id");
    // In a swap of two, either element may be the one that moved.
    let moved = format!(
        "{} · {} · {}",
        tr("analysis.moved"),
        trf("analysis.moved_from", &[("path", "$[\"items\"][0]".into())]),
        tr("analysis.ambiguous")
    );
    wait_for(&mut gui, &moved);
    assert!(gui.shows(tr("analysis.ambiguous_hint")));
    assert!(gui.shows(tr("analysis.paired_by_key")));
    assert!(gui.shows("[\"items\"][1] (2)"));
    assert!(gui.shows("[\"v\"] (1)"));
    assert!(gui.shows("1 → 10"));
    gui.click(tr("analysis.moved"));
    assert!(gui.shows(&moved));
    assert!(!gui.shows("[\"name\"] (1)"));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn yaml_and_toml_compares_offer_the_semantic_view() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("a.yaml", "db:\n  port: 5432\n"),
            ("b.yaml", "db:\n  port: 6543\n"),
            ("a.toml", "[db]\nport = 5432\n"),
            ("b.toml", "[db]\nport = 6543\n"),
        ],
    );
    for (name, ext, before) in [
        ("semantic_yaml", "yaml", "5432 → 6543"),
        ("semantic_toml", "toml", "5432 → 6543"),
    ] {
        let mut gui = Gui::start(
            name,
            compare(&[
                dir.path().join(format!("a.{ext}")),
                dir.path().join(format!("b.{ext}")),
            ]),
        );
        gui.click(tr("analysis.semantic"));
        gui.wait_until(before, |gui| gui.shows(before));
        assert!(gui.shows("[\"db\"][\"port\"] (1)"));
        assert!(gui.shows(tr("analysis.array_key")));
        gui.click(&format!("{} 2:2", tr("analysis.source")));
        assert_eq!(gui.text_view().active_pane, 1);
    }
}

#[test]
fn merge_verification_preview_never_edits_the_middle_pane() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("left.txt", "a\nLEFT\nc\nd\n"),
            ("base.txt", "a\nb\nc\nd\n"),
            ("right.txt", "a\nb\nc\nRIGHT\n"),
        ],
    );
    let mut gui = Gui::start(
        "merge_verification",
        compare(&[
            dir.path().join("left.txt"),
            dir.path().join("base.txt"),
            dir.path().join("right.txt"),
        ]),
    );
    gui.click(tr("drawer.toggle"));
    assert!(gui.shows(tr("analysis.merge_verification")));
    gui.click(tr("analysis.preview_merge"));
    gui.wait_until("merge preview", |gui| {
        gui.shows_text(tr("analysis.planned_changes"))
    });
    assert!(gui.shows(tr("analysis.not_run")));
    assert_eq!(gui.text_view().panes[1].doc.text(), "a\nb\nc\nd\n");
    assert_eq!(read(&dir.path().join("base.txt")), "a\nb\nc\nd\n");
    gui.click(tr("analysis.validate_result"));
    assert!(
        gui.shows(tr("analysis.failed")),
        "unmerged base cannot validate as merged output"
    );
}

#[test]
fn start_page_baselines_open_frozen_binary_bytes_without_original_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("left");
    let b = dir.path().join("right");
    for path in [&a, &b] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(a.join("a.bin"), b"\0A").unwrap();
    std::fs::write(b.join("a.bin"), b"\0B").unwrap();
    let config: String = (0..10).map(|i| format!("key{i} = {i}\n")).collect();
    std::fs::write(a.join("old.cfg"), &config).unwrap();
    std::fs::write(b.join("new.cfg"), config.replace("key3 = 3", "key3 = 30")).unwrap();
    std::fs::write(a.join("merge.txt"), "one\n").unwrap();
    std::fs::write(b.join("merge.txt"), "<<<<<<< ours\none\n").unwrap();
    let mut gui = Gui::start("baseline_binary", None);
    gui.click(tr("baseline.title"));
    gui.click(tr("baseline.left"));
    gui.type_text(&format!("folder:{}", a.display()));
    gui.click(tr("baseline.right"));
    gui.type_text(&format!("folder:{}", b.display()));
    gui.click(tr("baseline.compare"));
    let wait_for = |gui: &mut Gui, label: &str| gui.wait_until(label, |gui| gui.shows(label));
    wait_for(&mut gui, tr("baseline.captured"));
    let category = |key: &'static str, rest: &str| format!("{} {rest}", tr(key));
    let renamed = category("baseline.cat.renamed_modified", "new.cfg");
    let priority = format!("{} 1", tr("baseline.high_priority"));
    assert!(gui.shows(&category("baseline.cat.binary_changed", "1")));
    assert!(gui.shows(&priority));
    assert!(gui.shows(&renamed));
    assert!(gui.shows(&trf(
        "baseline.renamed_from",
        &[("path", "old.cfg".into()), ("n", "90".into())]
    )));
    assert!(gui.shows_text(&trf("baseline.captured_at", &[("time", String::new())])));
    let click = |gui: &mut Gui, label: &str| {
        gui.scroll_to(label);
        gui.click(label);
    };
    click(&mut gui, &category("baseline.cat.renamed_modified", "1"));
    assert!(gui.shows(&renamed));
    assert!(!gui.shows(&category("baseline.cat.binary_changed", "a.bin")));
    click(&mut gui, tr("baseline.not_rename"));
    wait_for(&mut gui, &category("baseline.cat.added", "new.cfg"));
    assert!(gui.shows(&category("baseline.cat.deleted", "old.cfg")));
    let restore = trf("baseline.restore_renames", &[("n", "1".into())]);
    click(&mut gui, &restore);
    wait_for(&mut gui, &renamed);
    assert!(!gui.shows(&restore));
    click(&mut gui, &priority);
    assert!(gui.shows(&category("baseline.cat.modified", "merge.txt")));
    assert!(gui.shows(tr("baseline.high_priority")));
    assert!(!gui.shows(&renamed));
    click(&mut gui, tr("analysis.all"));
    std::fs::remove_file(a.join("a.bin")).unwrap();
    std::fs::remove_file(b.join("a.bin")).unwrap();
    let binary = category("baseline.cat.binary_changed", "a.bin");
    gui.scroll_to(&binary);
    gui.click(&binary);
    let Content::Binary(view) = gui.app.tabs[gui.app.active].content.view() else {
        panic!("captured binary compare expected")
    };
    assert_eq!(
        view.data
            .iter()
            .map(|data| data.as_slice())
            .collect::<Vec<_>>(),
        [b"\0A", b"\0B"]
    );
    assert!(view.read_only.iter().all(|r| *r));
}

#[test]
fn semantic_source_navigation_merges_the_selected_later_block_only() {
    let dir = tempfile::tempdir().unwrap();
    let left = "{\n  \"a\": 1,\n  \"same1\": true,\n  \"same2\": true,\n  \"same3\": true,\n  \"b\": 2\n}\n";
    let right = left
        .replace("\"a\": 1", "\"a\": 10")
        .replace("\"b\": 2", "\"b\": 20");
    write(dir.path(), &[("a.json", left), ("b.json", &right)]);
    let mut gui = Gui::start(
        "semantic_source_merge",
        compare(&[dir.path().join("a.json"), dir.path().join("b.json")]),
    );
    gui.click(tr("analysis.semantic"));
    let source = format!("{} 2:6", tr("analysis.source"));
    gui.wait_until(&source, |gui| gui.shows(&source));
    gui.click(&source);
    gui.app.run(Cmd::CopyRight, &gui.ctx);
    gui.settle();
    assert_eq!(
        gui.text_view().panes[1].doc.text(),
        right.replace("\"b\": 20", "\"b\": 2")
    );
}

#[test]
fn captured_impact_source_opens_at_its_evidence_line() {
    let mut gui = Gui::start("impact_source", None);
    let mut doc = text::doc_from_bytes(None, b"first\nreference here\nlast\n", None);
    doc.read_only = true;
    gui.app
        .open_captured(super::super::baseline_panel::CapturedComparison::Source {
            docs: vec![doc.clone(), doc],
            line: 2,
        });
    gui.settle();
    let view = gui.text_view();
    assert_eq!(view.cursor_row, Some(1));
    assert!(view.panes.iter().all(|pane| pane.doc.read_only));
}

#[test]
fn exact_impact_findings_link_to_the_folder_review_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/src/util.ts", "export const x = 1;\n"),
            ("right/src/util.ts", "export const x = 2;\n"),
            ("left/src/app.ts", "import { x } from \"./util\";\n"),
            ("right/src/app.ts", "import { x } from \"./util\";\n"),
        ],
    );
    let mut gui = Gui::with_config(
        "impact_review_link",
        config(),
        compare(&[d.join("left"), d.join("right")]),
    );
    gui.click(tr("drawer.toggle"));
    gui.click(tr("baseline.title"));
    gui.click(tr("impact.title"));
    gui.click(tr("baseline.compare"));
    let finding = format!("{} · src/app.ts:1", tr("impact.exact"));
    gui.wait_until(&finding, |gui| gui.shows(&finding));
    gui.scroll_to(tr("impact.review"));
    gui.click(tr("impact.review"));
    let Content::Folder(view) = &gui.app.tabs[0].content else {
        panic!("Expected a folder comparison");
    };
    assert_eq!(
        view.drawer_tab,
        super::super::folder_view::FolderPanel::Review
    );
    assert_eq!(view.workspace.current.as_deref(), Some("src/app.ts"));
    assert!(gui.shows(tr("workspace.next")));
    assert!(gui.shows("src/app.ts"));
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn text_layout_settings_apply_immediately_and_keep_merge_content_correct() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("a.txt", "start\nend\n"),
            ("b.txt", "start\ninserted\nend\n"),
        ],
    );
    let mut gui = Gui::start(
        "text_layout",
        compare(&[dir.path().join("a.txt"), dir.path().join("b.txt")]),
    );
    gui.click(tr("menu.options").trim_end_matches(['.', '…']));
    gui.click(tr("view.layout_aligned"));
    gui.click(tr("btn.cancel"));
    assert_eq!(gui.app.cfg.text_layout, TextLayout::Compact);
    gui.click(tr("menu.options").trim_end_matches(['.', '…']));
    gui.click(tr("view.layout_aligned"));
    gui.click(tr("btn.ok"));
    assert_eq!(gui.app.cfg.text_layout, TextLayout::Aligned);
    gui.app.run(Cmd::FirstDiff, &gui.ctx);
    gui.app.run(Cmd::CopyLeft, &gui.ctx);
    gui.settle();
    assert_eq!(
        gui.text_view().panes[0].doc.text(),
        "start\ninserted\nend\n"
    );
    gui.app.cfg.text_layout = TextLayout::Compact;
    gui.settle();
    assert_eq!(
        gui.text_view().panes[0].doc.text(),
        "start\ninserted\nend\n"
    );
}

#[test]
fn escape_closes_only_the_active_compare_and_leaves_the_start_page_open() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "a\n"),
            ("b.txt", "b\n"),
            ("c.txt", "c\n"),
            ("d.txt", "d\n"),
        ],
    );
    let mut gui = Gui::start("escape_tabs", compare(&[d.join("a.txt"), d.join("b.txt")]));
    let first = gui.app.tabs[0].id;
    gui.drop_files(&[d.join("c.txt"), d.join("d.txt")]);
    assert_eq!(gui.app.tabs.len(), 2);
    gui.click(tr("menu.file"));
    assert!(
        egui::Popup::is_any_open(&gui.ctx),
        "clicked menu should be tracked as open"
    );
    gui.key(Modifiers::NONE, Key::Escape);
    assert_eq!(
        gui.app.tabs.len(),
        2,
        "menu dismissal must not also close a tab"
    );
    gui.key(CMD, Key::O);
    assert!(gui.app.open_dlg.is_some());
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.app.open_dlg.is_none());
    assert_eq!(
        gui.app.tabs.len(),
        2,
        "dialog dismissal must not also close a tab"
    );
    gui.key(Modifiers::NONE, Key::Escape);
    assert_eq!(gui.app.tabs.len(), 1);
    assert_eq!(gui.app.tabs[0].id, first);
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.app.tabs.is_empty());
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(!gui.closed);
}

#[test]
fn escape_search_and_unsaved_confirmation_take_priority_over_closing() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\nleft\n"), ("b.txt", "one\nright\n")]);
    let mut gui = Gui::start(
        "escape_priority",
        compare(&[d.join("a.txt"), d.join("b.txt")]),
    );
    gui.key(CMD, Key::F);
    gui.type_text("one");
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(!gui.text_view().find.open);
    assert_eq!(gui.app.tabs.len(), 1);
    gui.key(ALT, Key::ArrowRight);
    assert!(gui.text_view().panes[1].doc.modified);
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.shows(tr("dlg.unsaved")));
    assert_eq!(gui.app.tabs.len(), 1);
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.app.prompt.is_none());
    assert_eq!(gui.app.tabs.len(), 1);
    gui.key(Modifiers::NONE, Key::Escape);
    gui.click(tr("btn.discard"));
    assert!(gui.app.tabs.is_empty());
    assert_eq!(read(&d.join("b.txt")), "one\nright\n");
}

#[test]
fn escape_closes_binary_and_folder_comparisons() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("left/a.bin", "\0left"), ("right/a.bin", "\0right")]);
    for paths in [
        [d.join("left/a.bin"), d.join("right/a.bin")],
        [d.join("left"), d.join("right")],
    ] {
        let mut gui = Gui::start("escape_other_views", compare(&paths));
        assert_eq!(gui.app.tabs.len(), 1);
        gui.key(Modifiers::NONE, Key::Escape);
        assert!(gui.app.tabs.is_empty());
        assert!(!gui.closed);
    }
}

#[test]
fn folder_chips_count_each_kind_and_hide_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/same.txt", "x\n"),
            ("right/same.txt", "x\n"),
            ("left/changed.txt", "a\n"),
            ("right/changed.txt", "b\n"),
            ("left/gone.txt", "old\n"),
        ],
    );
    let mut gui = Gui::start("folder_chips", compare(&[d.join("left"), d.join("right")]));
    gui.settle();
    let identical = format!("{}  1", tr("view.identical"));
    let only_left = format!(
        "{}  1",
        trf("stats.only_in", &[("side", tr("side.left").into())])
    );
    assert!(gui.shows(&format!("{}  3", tr("analysis.all"))));
    assert!(gui.shows(&format!("{}  1", tr("view.different"))));
    assert!(gui.shows(&only_left));
    assert!(gui.shows("same.txt"));

    gui.click(&identical);
    gui.frames(3);
    assert!(!gui.app.cfg.show_identical);
    assert!(!gui.shows("same.txt"));
    assert!(gui.shows("changed.txt"));

    gui.click(&only_left);
    gui.frames(3);
    assert!(!gui.shows("gone.txt"));

    gui.click(&format!("{}  3", tr("analysis.all")));
    gui.frames(3);
    assert!(gui.shows("same.txt") && gui.shows("gone.txt"));
}

#[test]
fn scrolling_reaches_a_row_the_list_has_not_drawn() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for side in ["left", "right"] {
        std::fs::create_dir(d.join(side)).unwrap();
        for i in 0..120 {
            std::fs::write(d.join(side).join(format!("f{i:03}.txt")), "x\n").unwrap();
        }
    }
    let mut gui = Gui::start("folder_scroll", compare(&[d.join("left"), d.join("right")]));
    gui.settle();
    assert!(gui.shows("f000.txt") && !gui.shows("f119.txt"));
    gui.scroll_to("f119.txt");
    assert!(gui.widget("f119.txt").rect.bottom() < SCREEN.y);
}

#[test]
fn the_logo_and_the_name_beside_it_open_the_about_dialog() {
    let mut gui = Gui::start("about_logo", None);
    gui.frames(3);
    let name = gui
        .widgets
        .iter()
        .find(|w| w.label == "OxideMerge" && w.rect.width() < 200.0)
        .expect("the name shows next to the logo")
        .rect;
    gui.click_at(name.center());
    assert!(gui.app.about, "the name opens About");
    gui.app.about = false;
    gui.frames(2);
    let logo = egui::pos2(name.left() - 14.0, name.center().y);
    gui.click_at(logo);
    assert!(gui.app.about, "the logo opens About");
}

#[test]
fn about_displays_the_build_identifier_and_keeps_the_calendar_copyright_year() {
    let mut gui = Gui::start("about_build", None);
    gui.menu(&[tr("menu.help"), tr("menu.about")]);
    let build = env!("OXIDEMERGE_BUILD_ID");
    let date = env!("OXIDEMERGE_BUILD_DATE");
    assert!(gui.shows_text(build));
    // A release build's id is its date and nothing more; this test binary is never that (see
    // `build_info::identifier`), so it must show the date *and* the commit it was built from
    // together, not the commit alone.
    assert_ne!(build, date, "this test assumes a non-release build");
    assert!(gui.shows_text(date));
    assert!(gui.shows_text(&format!("{date}, {build}")));
    let copyright = trf(
        "about.copyright",
        &[
            ("year", date[..4].to_owned()),
            ("license", env!("CARGO_PKG_LICENSE").to_owned()),
        ],
    );
    assert!(gui.shows_text(&copyright));
}

#[test]
fn dialogs_put_their_close_button_at_the_right_edge() {
    let mut gui = Gui::start("dialog_close_buttons", None);
    // How far the close button is from the right edge of the dialog, as each lays it out.
    let gap = |gui: &mut Gui, open: &[&str], id: &str| {
        gui.menu(open);
        gui.frames(3);
        let dialog = gui
            .ctx
            .memory(|m| m.area_rect(egui::Id::unique(id)))
            .expect(id);
        let close = gui.widget(tr("btn.close")).rect;
        let gap = dialog.right() - close.right();
        gui.click(tr("btn.close"));
        gui.frames(3);
        gap
    };
    let about = gap(
        &mut gui,
        &[tr("menu.help"), tr("menu.about")],
        "about_dialog",
    );
    let help = gap(
        &mut gui,
        &[tr("menu.help"), tr("help.title")],
        "help_dialog",
    );
    assert!((0.0..20.0).contains(&about), "about: {about}");
    assert!((help - about).abs() < 0.5, "help {help}, about {about}");
}

/// Notes the controls of `view` that a screen reader would announce without a word. Text,
/// scroll bars, rows (named by their cells), the window itself and the column splitters of
/// tables, which egui_extras leaves unnamed, are not controls here.
fn unnamed_controls(gui: &Gui, view: &str, found: &mut Vec<String>) {
    use accesskit::Role;
    for w in &gui.controls {
        let window = w.rect.width() >= SCREEN.x && w.rect.height() >= SCREEN.y;
        let splitter = w.role == Role::Unknown && w.rect.width() <= 8.0;
        if matches!(
            w.role,
            Role::Label | Role::ScrollBar | Role::GenericContainer
        ) || window
            || splitter
            || !w.rect.is_finite()
        {
            continue;
        }
        if !w.label.chars().any(char::is_alphanumeric) {
            found.push(format!(
                "{view}: {:?} {:?} at {:?} size {:?}",
                w.role,
                w.label,
                w.rect.min,
                w.rect.size()
            ));
        }
    }
}

#[test]
fn every_control_is_named_for_screen_readers() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "one\ntwo\nthree\n"),
            ("base.txt", "one\n2\nthree\n"),
            ("right/a.txt", "one\n2\n3\n"),
            ("left/sub/b.txt", "x\n"),
            ("right/sub/b.txt", "y\n"),
        ],
    );
    for (name, shade) in [("a.png", 10u8), ("b.png", 200)] {
        image::RgbaImage::from_pixel(4, 4, image::Rgba([shade, 0, 0, 255]))
            .save(d.join(name))
            .unwrap();
    }
    std::fs::write(d.join("a.bin"), [0u8, 1, 2, 255]).unwrap();
    std::fs::write(d.join("b.bin"), [0u8, 9, 2, 255]).unwrap();
    let mut found = Vec::new();

    let mut gui = Gui::start("named_controls_start", None);
    unnamed_controls(&gui, "start page", &mut found);
    for (open, id) in [
        (&[tr("menu.help"), tr("menu.about")], "about"),
        (&[tr("menu.help"), tr("help.title")], "help"),
    ] {
        gui.menu(open);
        gui.frames(3);
        unnamed_controls(&gui, id, &mut found);
        gui.click(tr("btn.close"));
        gui.frames(3);
    }
    gui.click(tr("menu.options").trim_end_matches(['.', '…']));
    gui.frames(3);
    unnamed_controls(&gui, "options", &mut found);

    let views: [(&'static str, Vec<PathBuf>); 5] = [
        (
            "named_controls_text",
            vec![d.join("left/a.txt"), d.join("right/a.txt")],
        ),
        (
            "named_controls_merge",
            vec![
                d.join("left/a.txt"),
                d.join("base.txt"),
                d.join("right/a.txt"),
            ],
        ),
        (
            "named_controls_folders",
            vec![d.join("left"), d.join("right")],
        ),
        (
            "named_controls_images",
            vec![d.join("a.png"), d.join("b.png")],
        ),
        (
            "named_controls_binary",
            vec![d.join("a.bin"), d.join("b.bin")],
        ),
    ];
    for (name, paths) in views {
        let mut gui = Gui::start(name, compare(&paths));
        gui.settle();
        unnamed_controls(&gui, name, &mut found);
        if gui.shows(tr("drawer.toggle")) {
            gui.click(tr("drawer.toggle"));
            gui.frames(3);
            unnamed_controls(&gui, &format!("{name} with the side panel"), &mut found);
        }
    }
    assert!(found.is_empty(), "controls without a name: {found:#?}");
}

/// A tree is one stop in the Tab order, as a tree widget should be: the keyboard reaches it
/// without a click, the row it is on is what a screen reader reads, the arrow keys move from
/// there, and Tab leaves the tree rather than walking through every row.
#[test]
fn the_xml_tree_takes_the_keyboard_with_tab_alone_and_moves_with_the_arrow_keys() {
    let dir = tempfile::tempdir().unwrap();
    let a = "<catalog>
  <book id=\"1\">
    <title>Rust</title>
  </book>
  <book id=\"2\" year=\"2020\"/>
</catalog>
";
    let b = "<catalog>
  <book id=\"0\"/>
  <book id=\"1\">
    <title>Rust</title>
  </book>
  <book id=\"2\" year=\"2021\"/>
</catalog>
";
    write(dir.path(), &[("a.xml", a), ("b.xml", b)]);
    let mut gui = Gui::start(
        "xml_tree_keyboard",
        compare(&[dir.path().join("a.xml"), dir.path().join("b.xml")]),
    );
    gui.click(tr("analysis.tree"));
    gui.wait_until("the tree rows", |gui| {
        gui.widgets
            .iter()
            .filter(|w| w.role == accesskit::Role::ListItem)
            .count()
            > 2
    });
    assert!(
        gui.text_view().tree.view.selected.is_none(),
        "a row was selected before the keyboard or the mouse picked one"
    );

    let tree = gui.tab_to("the tree", |w| w.role == accesskit::Role::Tree);
    assert_eq!(tree.label, tr("analysis.tree"), "the tree is not named");

    // The arrow keys move through the rows from there, with no click first, and what the
    // keyboard is on is named after the row, which is what a screen reader reads out.
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let first = gui
        .focus
        .as_ref()
        .expect("nothing has the keyboard")
        .clone();
    assert_eq!(first.role, accesskit::Role::Tree, "{:?}", first.label);
    assert!(
        gui.text_view().tree.view.selected.is_some(),
        "the arrow keys did not reach the rows"
    );
    // What it says is the state of the row and then both of its sides.
    let (status, sides) = first
        .label
        .split_once(": ")
        .unwrap_or_else(|| panic!("{:?} does not say what the row is", first.label));
    assert!(
        status.chars().any(char::is_alphanumeric),
        "{:?}",
        first.label
    );
    assert!(sides.contains(" | "), "{:?} is not both sides", first.label);

    gui.key(Modifiers::NONE, Key::ArrowDown);
    let next = gui
        .focus
        .as_ref()
        .expect("nothing has the keyboard")
        .clone();
    assert_ne!(
        next.label, first.label,
        "the keyboard stayed on the same row"
    );

    // Every row is still there to be read, one by one, even though only the tree is a stop.
    assert!(
        gui.widgets
            .iter()
            .filter(|w| w.role == accesskit::Role::ListItem)
            .count()
            > 2
    );

    // One stop: Tab leaves the tree instead of stepping from row to row.
    gui.tab(false);
    assert!(
        gui.focus
            .as_ref()
            .is_none_or(|f| f.role != accesskit::Role::Tree),
        "Tab stayed in the tree: {:?}",
        gui.focus.as_ref().map(|f| f.label.clone())
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

/// Difference navigation without the mouse: the file list takes the arrow keys as it is, the
/// Previous and Next buttons can be reached with Tab and run with the space bar, and the count
/// of files and differences follows along for anyone reading the window out.
#[test]
fn differences_are_walked_with_the_keyboard_alone() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            (
                "left/a.txt",
                "1
x
3
4
5
6
7
8
y
10
",
            ),
            (
                "right/a.txt",
                "1
X
3
4
5
6
7
8
Y
10
",
            ),
            (
                "left/b.txt",
                "same
",
            ),
            (
                "right/b.txt",
                "same
",
            ),
            (
                "left/c.txt",
                "one
",
            ),
            (
                "right/c.txt",
                "two
",
            ),
        ],
    );
    let cfg = Config {
        folder_file_pane: true,
        ..config()
    };
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::with_config("keyboard_only_nav", cfg, compare_folders(&roots));
    gui.settle();

    // The list takes the arrow keys without being clicked into first.
    gui.key(Modifiers::NONE, Key::Home);
    gui.settle();
    gui.wait_until("the first file", |gui| {
        gui.shows_text(&format!(
            "{} · {}",
            trf("nav.file_of", &[("n", "1".into()), ("total", "2".into())]),
            trf("nav.diff_of", &[("n", "1".into()), ("total", "2".into())])
        ))
    });

    // Alt+Down steps through the differences and on to the next changed file.
    gui.key(ALT, Key::ArrowDown);
    gui.frames(3);
    assert!(gui.shows_text(&trf(
        "nav.diff_of",
        &[("n", "2".into()), ("total", "2".into())]
    )));

    // The buttons are reachable with Tab and run with the space bar.
    let next = gui.tab_to(&format!("the {} button", tr("cmd.next_diff")), |w| {
        w.role == accesskit::Role::Button && named(w, tr("cmd.next_diff"))
    });
    assert!(next.enabled, "{:?} is disabled", next.label);
    gui.key(Modifiers::NONE, Key::Space);
    gui.frames(3);
    assert!(
        gui.shows_text(&trf(
            "nav.file_of",
            &[("n", "2".into()), ("total", "2".into())]
        )),
        "the space bar did not step on to the next file"
    );
    let previous = gui.tab_to(&format!("the {} button", tr("cmd.prev_diff")), |w| {
        w.role == accesskit::Role::Button && named(w, tr("cmd.prev_diff"))
    });
    assert!(previous.enabled);
    gui.key(Modifiers::NONE, Key::Enter);
    gui.frames(3);
    assert!(
        gui.shows_text(&trf(
            "nav.file_of",
            &[("n", "1".into()), ("total", "2".into())]
        )),
        "Enter did not step back"
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

/// Notes controls of `view` that reach past the window's edges or lie over one another.
fn crowded_controls(gui: &Gui, view: &str, found: &mut Vec<String>) {
    crowded_among(gui.controls.iter(), gui.screen.x, view, found);
}

/// [`crowded_controls`] for some of the controls, in a window `width` wide.
fn crowded_among<'a>(
    controls: impl Iterator<Item = &'a Widget>,
    width: f32,
    view: &str,
    found: &mut Vec<String>,
) {
    use accesskit::Role;
    let controls: Vec<&Widget> = controls
        .filter(|w| {
            w.rect.is_finite()
                && w.rect.area() > 0.0
                && !matches!(
                    w.role,
                    Role::Label
                        | Role::ScrollBar
                        | Role::GenericContainer
                        | Role::Document
                        | Role::Image
                        | Role::TitleBar
                        | Role::Unknown
                )
        })
        .collect();
    for w in &controls {
        if w.rect.left() < -1.0 || w.rect.right() > width + 1.0 {
            found.push(format!(
                "{view}: {:?} {:?} is cut off at {:?}",
                w.role, w.label, w.rect
            ));
        }
    }
    for (i, a) in controls.iter().enumerate() {
        for b in &controls[i + 1..] {
            let both = a.rect.intersect(b.rect);
            if both.width() > 2.0 && both.height() > 2.0 {
                found.push(format!(
                    "{view}: {:?} at {:?} lies over {:?} at {:?}",
                    a.label, a.rect, b.label, b.rect
                ));
            }
        }
    }
}

#[test]
fn controls_fit_scaled_and_narrow_windows() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "one\ntwo\nthree\n"),
            ("base.txt", "one\n2\nthree\n"),
            ("right/a.txt", "one\n2\n3\n"),
            ("left/sub/b.txt", "x\n"),
            ("right/sub/b.txt", "y\n"),
        ],
    );
    let views: [(&'static str, Option<OpenRequest>); 4] = [
        ("fit_start", None),
        (
            "fit_text",
            compare(&[d.join("left/a.txt"), d.join("right/a.txt")]),
        ),
        (
            "fit_merge",
            compare(&[
                d.join("left/a.txt"),
                d.join("base.txt"),
                d.join("right/a.txt"),
            ]),
        ),
        ("fit_folders", compare(&[d.join("left"), d.join("right")])),
    ];
    // Window sizes in points with their scale: the default window at 100%, 150% and 200%,
    // a smaller one at 150%, and the smallest window the app allows.
    let sizes = [
        (egui::vec2(1280.0, 820.0), 1.0),
        (egui::vec2(1280.0, 820.0), 1.5),
        (egui::vec2(1280.0, 820.0), 2.0),
        (egui::vec2(960.0, 640.0), 1.5),
        (egui::vec2(640.0, 400.0), 2.0),
    ];
    let mut found = Vec::new();
    for (name, open) in views {
        let mut gui = Gui::start(name, open);
        for (size, scale) in sizes {
            gui.screen = size;
            gui.ctx.set_pixels_per_point(scale);
            gui.frames(4);
            crowded_controls(
                &gui,
                &format!("{name} {}x{} at {}%", size.x, size.y, scale * 100.0),
                &mut found,
            );
        }
    }
    assert!(found.is_empty(), "crowded controls: {found:#?}");
}

/// What a screen reader is told without being asked: the status bar is a polite live region,
/// so moving to another difference, or the conflicts left in a merge, are read out; an error
/// is an assertive one, read out at once.
#[test]
fn changes_and_errors_are_read_out_by_screen_readers() {
    use crate::ui::commands::Cmd;
    use accesskit::Live;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\n"),
            ("b.txt", "one\n2\nthree\nfour\nfive\n6\nseven\n"),
            ("base.txt", "a\nb\nc\n"),
            ("left.txt", "a\nleft\nc\n"),
            ("right.txt", "a\nright\nc\n"),
        ],
    );
    let live = |gui: &Gui, kind: Live| -> Vec<String> {
        gui.widgets
            .iter()
            .filter(|w| w.live == Some(kind))
            .map(|w| w.label.clone())
            .collect()
    };
    let mut gui = Gui::start("live_regions", compare(&[d.join("a.txt"), d.join("b.txt")]));
    gui.settle();
    let before = live(&gui, Live::Polite);
    assert!(
        !before.is_empty(),
        "the status bar is a live region\n{}",
        gui.describe()
    );
    press(&mut gui, Cmd::NextDiff);
    press(&mut gui, Cmd::NextDiff);
    gui.frames(2);
    let after = live(&gui, Live::Polite);
    assert_ne!(
        after, before,
        "moving to a difference changes what is read out"
    );
    let second = trf("msg.diff_of", &[("n", "2".into()), ("total", "2".into())]);
    assert!(after.contains(&second), "{after:?}");

    gui.app.error("Could not save b.txt");
    gui.frames(2);
    assert!(
        live(&gui, Live::Assertive)
            .iter()
            .any(|t| t.contains("Could not save b.txt")),
        "an error is read out at once: {:?}",
        live(&gui, Live::Assertive)
    );
    assert!(
        !live(&gui, Live::Polite)
            .iter()
            .any(|t| t.contains("Could not save")),
        "and only once"
    );

    let mut merge = Gui::start(
        "live_regions_merge",
        compare(&[d.join("left.txt"), d.join("base.txt"), d.join("right.txt")]),
    );
    merge.settle();
    let said = live(&merge, Live::Polite);
    let conflicts = trf("merge.conflicts_left_one", &[("n", "1".into())]);
    assert!(
        said.contains(&conflicts),
        "the conflicts left are read out: {said:?}"
    );
}

/// Check boxes tell a screen reader whether they are on, and the state follows a click.
#[test]
fn check_boxes_report_their_state() {
    let mut gui = Gui::start("check_box_states", None);
    gui.click(tr("menu.options").trim_end_matches(['.', '…']));
    gui.frames(3);
    let boxes: Vec<Widget> = gui
        .controls
        .iter()
        .filter(|w| w.role == accesskit::Role::CheckBox)
        .cloned()
        .collect();
    assert!(boxes.len() > 5, "{}", gui.describe());
    let unknown: Vec<&String> = boxes
        .iter()
        .filter(|w| w.toggled.is_none())
        .map(|w| &w.label)
        .collect();
    assert!(
        unknown.is_empty(),
        "check boxes without a state: {unknown:?}"
    );
    let first = &boxes[0];
    gui.click_at(first.rect.center());
    gui.frames(2);
    let now = gui
        .controls
        .iter()
        .find(|w| w.role == accesskit::Role::CheckBox && w.label == first.label)
        .unwrap();
    assert_ne!(
        now.toggled, first.toggled,
        "{} did not change state",
        first.label
    );
}

/// Each accessibility look (colour-blind colours, high contrast, reduced motion, compact rows),
/// alone, with large text on a short window: no control is crowded or cut off. Reduced motion
/// also turns animation off.
#[test]
fn accessibility_looks_fit_large_text_and_short_windows() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "one\n2\nthree\n")],
    );
    type Look = (&'static str, fn(&mut Config));
    let looks: [Look; 4] = [
        ("color blind", |c| c.color_blind_colors = true),
        ("high contrast", |c| c.high_contrast = true),
        ("reduced motion", |c| c.reduce_motion = true),
        ("compact", |c| c.compact_density = true),
    ];
    let mut found = Vec::new();
    for (look, set) in looks {
        let mut cfg = config();
        set(&mut cfg);
        let mut gui = Gui::with_config(
            "accessibility_looks",
            cfg,
            compare(&[d.join("a.txt"), d.join("b.txt")]),
        );
        gui.settle();
        if look == "reduced motion" {
            let style = gui.ctx.global_style();
            assert_eq!(style.animation_time, 0.0, "no animation");
        }
        for (size, scale) in [
            (egui::vec2(1024.0, 560.0), 1.5),
            (egui::vec2(800.0, 480.0), 2.0),
        ] {
            gui.screen = size;
            gui.ctx.set_pixels_per_point(scale);
            gui.frames(4);
            crowded_controls(
                &gui,
                &format!("{look}, {}x{} at {}%", size.x, size.y, scale * 100.0),
                &mut found,
            );
        }
    }
    assert!(found.is_empty(), "crowded controls: {found:#?}");
}

#[test]
fn clicked_bytes_take_typed_digits_and_text_and_save() {
    let dir = tempfile::tempdir().unwrap();
    let paths = [dir.path().join("a.bin"), dir.path().join("b.bin")];
    std::fs::write(&paths[0], [0x00, 0x11, 0x22]).unwrap();
    std::fs::write(&paths[1], [0x00, 0x11, 0x33]).unwrap();
    let mut gui = Gui::start("binary_typing", compare(&paths));
    let body = gui.widget(tr("help.binary_compare")).rect;
    let font = egui::FontId::monospace(config().font_size);
    let (char_w, row_h) = gui
        .ctx
        .fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font) + 2.0));
    let bpl = config().binary_bytes_per_line as f32;
    // Each line is the offset, a hexadecimal "HH " for each byte, a space and the text.
    let at = |col: f32| {
        Pos2::new(
            body.left() + body.width() / 2.0 + 4.0 + (col + 0.5) * char_w,
            body.top() + row_h / 2.0,
        )
    };
    let binary = |gui: &Gui| match gui.app.tabs[gui.app.active].content.view() {
        Content::Binary(v) => (v.data[1].to_vec(), v.pane, v.is_modified()),
        _ => panic!("binary compare expected"),
    };

    gui.click_at(at(10.0 + 3.0));
    gui.type_text("4");
    gui.type_text("1");
    assert_eq!(binary(&gui), (vec![0x00, 0x41, 0x33], Some(1), true));

    gui.click_at(at(10.0 + 3.0 * bpl + 1.0));
    for pressed in [true, false] {
        gui.events.push(Event::Key {
            key: Key::Questionmark,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        });
    }
    gui.type_text("?");
    assert!(!gui.app.help, "? in the text column is typed, not Help");
    assert_eq!(binary(&gui).0, [b'?', 0x41, 0x33]);

    gui.key(CMD, Key::S);
    assert_eq!(std::fs::read(&paths[1]).unwrap(), [b'?', 0x41, 0x33]);
    assert_eq!(std::fs::read(&paths[0]).unwrap(), [0x00, 0x11, 0x22]);
}

#[test]
fn a_folder_compare_saves_as_a_job_and_a_sync_job_runs_only_once_allowed() {
    use crate::ui::job_dialog::{SaveJob, SaveKind};
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("left/a.txt", "new\n"),
            ("right/a.txt", "old\n"),
            ("right/extra.txt", "extra\n"),
        ],
    );
    let roots = [d.join("left"), d.join("right")];
    let mut gui = Gui::start("save_job", compare_folders(&roots));
    gui.wait_until("the compare", |g| !g.busy());

    // Save as job offers the three kinds and names the folders.
    gui.menu(&[
        tr("menu.file"),
        tr("menu.projects_jobs"),
        tr("menu.save_job"),
    ]);
    assert!(gui.shows(tr("job.kind_compare")), "{}", gui.describe());
    assert!(gui.shows_text(&roots[0].display().to_string()));
    gui.click(tr("job.kind_sync"));
    assert!(gui.shows_text(tr("job.sync_note")));
    assert!(gui.shows(tr("job.mirror")));
    gui.click(tr("btn.cancel"));
    assert!(!gui.shows(tr("job.kind_sync")));

    // The tab as a mirror job, written as the dialog's Save would.
    let Content::Folder(v) = gui.app.tabs[0].content.view() else {
        panic!("not a folder compare");
    };
    let mut save = SaveJob::new(v.as_job(&gui.app.cfg), "mirror");
    save.kind = SaveKind::Sync;
    save.relative = true;
    let file = d.join("mirror.oxjob");
    save.write(&file).unwrap();

    gui.app.start_run_job(file);
    gui.frames(2);
    assert!(gui.shows_text(tr("job.confirm")), "{}", gui.describe());
    assert_eq!(
        read(&roots[1].join("a.txt")),
        "old\n",
        "changed before allowed"
    );
    gui.click(tr("job.run_changes"));
    let applied = trf("job.applied", &[("n", "2".into())]);
    gui.wait_until("the results", |g| g.shows_text(&applied));
    assert_eq!(read(&roots[1].join("a.txt")), "new\n");
    assert!(!roots[1].join("extra.txt").exists());
    // Compare opens the job's folders in a tab.
    gui.click(tr("job.open_compare"));
    gui.wait_until("the compare tab", |g| g.app.tabs.len() == 2 && !g.busy());
    assert!(!gui.shows_text(&applied), "the results close");
}

#[test]
fn compare_with_revision_lists_and_opens_subversion_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let svn = |cwd: &Path, program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let repo = d.join("repo");
    if svn(d, "svnadmin", &["create", repo.to_str().unwrap()]).is_err() {
        eprintln!("Subversion is not installed; skipped");
        return;
    }
    let url = format!(
        "file:///{}",
        repo.to_str()
            .unwrap()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let wc = d.join("wc");
    let ok = |cwd: &Path, args: &[&str]| {
        let out = svn(cwd, "svn", args).unwrap();
        assert!(
            out.status.success(),
            "svn {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    ok(d, &["checkout", "-q", &url, wc.to_str().unwrap()]);
    write(&wc, &[("f.txt", "one\n")]);
    ok(&wc, &["add", "-q", "f.txt"]);
    ok(&wc, &["commit", "-q", "-m", "first"]);
    write(&wc, &[("f.txt", "two\n")]);
    ok(&wc, &["commit", "-q", "-m", "second"]);
    ok(&wc, &["update", "-q"]);
    write(&wc, &[("f.txt", "three\n")]);
    let file = wc.join("f.txt");

    let mut gui = Gui::start("svn_revision", None);
    gui.menu(&[tr("menu.file"), tr("menu.git_compare")]);
    gui.app.git_dlg.as_mut().unwrap().path = file.display().to_string();
    gui.wait_until("the revisions", |g| {
        g.app.git_dlg.as_ref().is_some_and(|d| d.commits.len() == 2)
    });
    let commits = gui.app.git_dlg.as_ref().unwrap().commits.clone();
    assert_eq!(
        commits,
        vec![
            ("r2".into(), "second".into()),
            ("r1".into(), "first".into())
        ]
    );
    // An empty revision is BASE, the revision the working copy was updated to.
    gui.click(tr("btn.compare"));
    gui.wait_until("the compare tab", |g| g.app.tabs.len() == 1 && !g.busy());
    let v = gui.text_view();
    assert_eq!(v.panes[0].doc.text(), "two\n");
    assert_eq!(v.panes[1].doc.text(), "three\n");
    assert!(v.panes[0].doc.read_only, "the revision is read-only");
    assert!(
        v.panes[0]
            .doc
            .description
            .as_ref()
            .is_some_and(|t| t.ends_with("@ BASE")),
        "{:?}",
        v.panes[0].doc.description
    );

    // A revision picked from the list, as svn log names it.
    gui.menu(&[tr("menu.file"), tr("menu.git_compare")]);
    gui.app.git_dlg.as_mut().unwrap().path = file.display().to_string();
    gui.wait_until("the revisions", |g| {
        g.app.git_dlg.as_ref().is_some_and(|d| d.commits.len() == 2)
    });
    // Once typing pauses, the example revisions are those of Subversion.
    let svn_hint = crate::core::vcs::Vcs::Subversion.rev_hint();
    gui.wait_until("the Subversion hint", |g| {
        g.app
            .git_dlg
            .as_ref()
            .is_some_and(|d| d.rev_hint == svn_hint)
    });
    gui.click(tr("label.recent_commits"));
    gui.click("r1  first");
    assert_eq!(gui.app.git_dlg.as_ref().unwrap().rev, "r1");
    gui.click(tr("btn.compare"));
    gui.wait_until("the second tab", |g| g.app.tabs.len() == 2 && !g.busy());
    assert_eq!(gui.text_view().panes[0].doc.text(), "one\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn compare_with_revision_lists_and_opens_mercurial_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let hgrc = dir.path().join("hgrc");
    std::fs::write(&hgrc, "").unwrap();
    let hg = |args: &[&str]| {
        std::process::Command::new("hg")
            .args(args)
            .current_dir(&repo)
            .env("HGRCPATH", &hgrc)
            .env("HGUSER", "test")
            .stdin(std::process::Stdio::null())
            .output()
    };
    if hg(&["init"]).is_err() {
        eprintln!("Mercurial is not installed; skipped");
        return;
    }
    let ok = |args: &[&str]| {
        let out = hg(args).unwrap();
        assert!(
            out.status.success(),
            "hg {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    write(&repo, &[("f.txt", "one\n")]);
    ok(&["add", "-q", "f.txt"]);
    ok(&["commit", "-q", "-m", "first"]);
    write(&repo, &[("f.txt", "two\n")]);
    ok(&["commit", "-q", "-m", "second"]);
    write(&repo, &[("f.txt", "three\n")]);
    let file = repo.join("f.txt");
    let first = String::from_utf8(
        hg(&["log", "-r", "0", "-T", "{node|short}"])
            .unwrap()
            .stdout,
    )
    .unwrap();

    let mut gui = Gui::start("hg_revision", None);
    gui.menu(&[tr("menu.file"), tr("menu.git_compare")]);
    gui.app.git_dlg.as_mut().unwrap().path = file.display().to_string();
    gui.wait_until("the revisions", |g| {
        g.app.git_dlg.as_ref().is_some_and(|d| d.commits.len() == 2)
    });
    let commits = gui.app.git_dlg.as_ref().unwrap().commits.clone();
    assert_eq!(commits[0].1, "second");
    assert_eq!(commits[1], (first.clone(), "first".into()));
    // An empty revision is the working copy's parent.
    gui.click(tr("btn.compare"));
    gui.wait_until("the compare tab", |g| g.app.tabs.len() == 1 && !g.busy());
    let v = gui.text_view();
    assert_eq!(v.panes[0].doc.text(), "two\n");
    assert_eq!(v.panes[1].doc.text(), "three\n");
    assert!(v.panes[0].doc.read_only, "the revision is read-only");
    assert!(
        v.panes[0]
            .doc
            .description
            .as_ref()
            .is_some_and(|t| t.ends_with("@ .")),
        "{:?}",
        v.panes[0].doc.description
    );

    gui.menu(&[tr("menu.file"), tr("menu.git_compare")]);
    gui.app.git_dlg.as_mut().unwrap().path = file.display().to_string();
    gui.wait_until("the revisions", |g| {
        g.app.git_dlg.as_ref().is_some_and(|d| d.commits.len() == 2)
    });
    gui.click(tr("label.recent_commits"));
    gui.click(&format!("{first}  first"));
    assert_eq!(gui.app.git_dlg.as_ref().unwrap().rev, first);
    gui.click(tr("btn.compare"));
    gui.wait_until("the second tab", |g| g.app.tabs.len() == 2 && !g.busy());
    assert_eq!(gui.text_view().panes[0].doc.text(), "one\n");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn a_revision_is_read_on_another_thread_and_its_error_brings_the_dialog_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("loose.txt");
    std::fs::write(&file, "text\n").unwrap();
    let mut gui = Gui::start("rev_thread", None);
    gui.menu(&[tr("menu.file"), tr("menu.git_compare")]);
    let dialog = gui.app.git_dlg.take();
    gui.app.open_git_revision(&file, "HEAD", dialog);
    assert!(
        gui.app.rev_load.is_some(),
        "reading goes on without the window"
    );
    assert!(gui.app.git_dlg.is_none());
    gui.wait_until("the error", |g| g.app.rev_load.is_none());
    assert!(!gui.errors().is_empty());
    assert!(gui.app.git_dlg.is_some(), "the dialog is back to try again");
    assert!(gui.app.tabs.is_empty());
}

#[test]
fn the_revision_list_waits_for_typing_and_drops_a_stale_log() {
    let ms = Duration::from_millis;
    // the list is for the typed path, or its log is already running
    assert_eq!(log_step(false, ms(5000), None), LogStep::Idle);
    // typing has not paused yet
    assert_eq!(
        log_step(true, ms(100), None),
        LogStep::WaitFor(GIT_LOG_DELAY - ms(100))
    );
    assert_eq!(log_step(true, GIT_LOG_DELAY, None), LogStep::Start);
    // an earlier path's log is let finish for a while, then replaced
    assert_eq!(
        log_step(true, ms(5000), Some(ms(500))),
        LogStep::WaitFor(STALE_LOG - ms(500))
    );
    assert_eq!(log_step(true, ms(5000), Some(STALE_LOG)), LogStep::Start);

    // replacing a job drops it, which stops its log
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (_tx, rx) = std::sync::mpsc::channel();
    drop(LogJob {
        path: "old".into(),
        rx,
        started: std::time::Instant::now(),
        cancel: cancel.clone(),
    });
    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn the_text_caret_takes_the_word_keys_before_the_difference_commands() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one two\nsame\n"), ("b.txt", "one 2\nsame\n")],
    );
    let mut gui = Gui::start("word_keys", compare(&[d.join("a.txt"), d.join("b.txt")]));
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(gui.text_view().cur_block, Some(0));
    match gui.app.tabs[0].content.view_mut() {
        Content::Text(v) => v.place_caret(1, (0, 0), false),
        _ => unreachable!(),
    }
    let word = if cfg!(target_os = "macos") {
        ALT
    } else {
        Modifiers::CTRL.plus(CMD)
    };
    gui.key(word, Key::ArrowRight);
    assert_eq!(gui.text_view().caret().unwrap().pos, (0, 3));
    assert_eq!(
        gui.text_view().panes[1].doc.lines[0],
        "one 2",
        "Alt+Right did not copy the difference while the caret had it"
    );
    // Without the caret, Alt+Right copies the difference as before.
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.text_view().caret().is_none());
    gui.key(ALT, Key::ArrowRight);
    assert_eq!(gui.text_view().panes[1].doc.lines[0], "one two");
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn navigation_keys_in_the_text_move_the_caret_and_never_the_difference() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let same: String = (0..60).map(|i| format!("same {i}\n")).collect();
    write(
        d,
        &[
            ("a.txt", &format!("one two\n{same}old\n{same}")),
            ("b.txt", &format!("one 2\n{same}new\n{same}")),
        ],
    );
    let mut gui = Gui::start("caret_nav", compare(&[d.join("a.txt"), d.join("b.txt")]));
    assert_eq!(
        gui.text_view().cur_block,
        Some(0),
        "the first difference is selected"
    );
    match gui.app.tabs[0].content.view_mut() {
        Content::Text(v) => v.place_caret(1, (0, 2), false),
        _ => unreachable!(),
    }
    let mac = cfg!(target_os = "macos");
    // ⌘ on macOS is reported as both the Mac command key and the command modifier.
    let (word, line) = if mac {
        (ALT, Modifiers::MAC_CMD.plus(CMD))
    } else {
        (Modifiers::CTRL.plus(CMD), Modifiers::CTRL.plus(CMD))
    };
    let shift = Modifiers::SHIFT;
    let keys = [
        (Modifiers::NONE, Key::ArrowDown),
        (Modifiers::NONE, Key::ArrowUp),
        (Modifiers::NONE, Key::ArrowRight),
        (Modifiers::NONE, Key::ArrowLeft),
        (shift, Key::ArrowDown),
        (shift, Key::ArrowUp),
        (Modifiers::NONE, Key::PageDown),
        (shift, Key::PageUp),
        (Modifiers::NONE, Key::End),
        (Modifiers::NONE, Key::Home),
        (word, Key::ArrowRight),
        (word.plus(shift), Key::ArrowLeft),
        (line, Key::ArrowRight),
        (line.plus(shift), Key::ArrowLeft),
        (Modifiers::NONE, Key::Escape),
    ];
    let mut lines_seen = Vec::new();
    for (m, k) in keys {
        gui.key(m, k);
        let v = gui.text_view();
        let c = v
            .caret()
            .unwrap_or_else(|| panic!("{m:?}+{k:?} ended the caret"));
        lines_seen.push(c.pos.0);
        // The current difference follows the caret's row (or is none); no key jumps to one.
        if let Some(b) = v.cur_block {
            assert!(
                v.diff.blocks[b].ranges[1].contains(&c.pos.0),
                "{m:?}+{k:?} selected difference {b} away from the caret at {:?}",
                c.pos
            );
        }
        assert_eq!(
            v.panes[1].doc.lines[0], "one 2",
            "{m:?}+{k:?} changed the text"
        );
    }
    assert_eq!(lines_seen[..4], [1, 0, 0, 0], "Down, Up, Right, Left");
    assert_eq!(
        gui.app.tabs.len(),
        1,
        "Esc ended the selection, not the tab"
    );
    // Alt+Down is still the next difference, with the caret in the text.
    match gui.app.tabs[0].content.view_mut() {
        Content::Text(v) => v.select_block(0),
        _ => unreachable!(),
    }
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(gui.text_view().cur_block, Some(1));
    assert!(gui.text_view().caret().is_some());
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn shift_tab_outdents_in_the_text_rather_than_moving_the_focus() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[("a.txt", "one\ntwo\n"), ("b.txt", "one\n    two\nthree\n")],
    );
    let mut gui = Gui::start("shift_tab", compare(&[d.join("a.txt"), d.join("b.txt")]));
    match gui.app.tabs[0].content.view_mut() {
        Content::Text(v) => {
            v.place_caret(1, (1, 6), false);
            v.place_caret(1, (2, 2), true);
        }
        _ => unreachable!(),
    }
    gui.key(Modifiers::SHIFT, Key::Tab);
    let v = gui.text_view();
    assert_eq!(v.panes[1].doc.lines[1..], ["two", "three"]);
    assert_eq!(v.caret_text().as_deref(), Some("o\nth"));
    gui.key(Modifiers::NONE, Key::Tab);
    let v = gui.text_view();
    assert_eq!(v.panes[1].doc.lines[1..], ["    two", "    three"]);
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

fn text_view_mut(gui: &mut Gui) -> &mut TextView {
    match gui.app.tabs[0].content.view_mut() {
        Content::Text(v) => v,
        _ => unreachable!(),
    }
}

#[test]
fn option_with_a_letter_types_into_the_text_on_macos_instead_of_running_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "one\n"), ("b.txt", "two\n")]);
    let mut gui = Gui::start("option_types", compare(&[d.join("a.txt"), d.join("b.txt")]));
    let wrap = gui.app.cfg.word_wrap;
    // Without the caret in the text, Alt+Z switches wrapping.
    gui.key(ALT, Key::Z);
    assert_eq!(gui.app.cfg.word_wrap, !wrap);
    text_view_mut(&mut gui).place_caret(1, (0, 3), false);
    // Option+Z on macOS: the key and the character it types come together.
    gui.modifiers = ALT;
    gui.events.push(Event::Key {
        key: Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: ALT,
    });
    gui.events.push(Event::Text("Ω".into()));
    gui.frames(2);
    gui.modifiers = Modifiers::NONE;
    if cfg!(target_os = "macos") {
        assert_eq!(
            gui.app.cfg.word_wrap, !wrap,
            "Option+Z typed, it did not wrap"
        );
        assert_eq!(gui.text_view().panes[1].doc.lines[0], "twoΩ");
    } else {
        assert_eq!(
            gui.app.cfg.word_wrap, wrap,
            "Alt+Z is still the wrap command"
        );
    }
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn line_keys_duplicate_and_move_lines_and_leave_the_difference_keys_alone() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "one\ntwo\nthree\nx\n"),
            ("b.txt", "one\ntwo\nthree\ny\n"),
        ],
    );
    let mut gui = Gui::start("line_keys", compare(&[d.join("a.txt"), d.join("b.txt")]));
    text_view_mut(&mut gui).place_caret(1, (0, 1), false);
    let shift_alt = ALT.plus(Modifiers::SHIFT);
    gui.key(CMD, Key::D);
    assert_eq!(
        gui.text_view().panes[1].doc.lines[..3],
        ["one", "one", "two"]
    );
    gui.key(shift_alt, Key::ArrowDown);
    gui.key(shift_alt, Key::ArrowDown);
    assert_eq!(
        gui.text_view().panes[1].doc.lines[..4],
        ["one", "two", "three", "one"]
    );
    assert_eq!(gui.text_view().caret().map(|c| c.pos), Some((3, 1)));
    gui.key(shift_alt, Key::ArrowUp);
    assert_eq!(
        gui.text_view().panes[1].doc.lines[..4],
        ["one", "two", "one", "three"]
    );
    // Each is one undo step.
    gui.key(CMD, Key::Z);
    assert_eq!(
        gui.text_view().panes[1].doc.lines[..4],
        ["one", "two", "three", "one"]
    );
    // Alt+Down still goes to the next difference.
    text_view_mut(&mut gui).select_block(0);
    let before = gui.text_view().panes[1].doc.lines.clone();
    gui.key(ALT, Key::ArrowDown);
    assert_eq!(gui.text_view().panes[1].doc.lines, before);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn carets_added_above_and_below_type_together_and_escape_steps_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, &[("a.txt", "a\nb\nc\nd\n"), ("b.txt", "a\nb\nc\ne\n")]);
    let mut gui = Gui::start("multi_caret", compare(&[d.join("a.txt"), d.join("b.txt")]));
    text_view_mut(&mut gui).place_caret(1, (1, 1), false);
    let add = CMD.plus(ALT);
    gui.key(add, Key::ArrowDown);
    gui.key(add, Key::ArrowUp);
    assert_eq!(gui.text_view().carets().len(), 3);
    gui.type_text("!");
    assert_eq!(gui.text_view().panes[1].doc.lines, ["a!", "b!", "c!", "e"]);
    gui.key(CMD, Key::Z);
    assert_eq!(gui.text_view().panes[1].doc.lines, ["a", "b", "c", "e"]);
    // Esc: one caret, then none, then the tab.
    gui.key(Modifiers::NONE, Key::Escape);
    assert_eq!(gui.text_view().carets().len(), 1);
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.text_view().caret().is_none());
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn a_file_can_be_edited_and_saved_with_the_keyboard_alone() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "same\nold\nsame\n"),
            ("b.txt", "same\nnew\nsame\n"),
        ],
    );
    let mut gui = Gui::start(
        "keyboard_only",
        compare(&[d.join("a.txt"), d.join("b.txt")]),
    );
    // To the difference, into the text at its line end, and edit.
    gui.key(ALT, Key::ArrowDown);
    gui.key(Modifiers::NONE, Key::F2);
    assert!(
        gui.text_view().caret().is_some(),
        "F2 puts the text cursor in"
    );
    gui.type_text("er");
    gui.key(Modifiers::SHIFT, Key::Home);
    gui.key(Modifiers::NONE, Key::Tab);
    // Out of the text again, and save.
    gui.key(Modifiers::NONE, Key::Escape);
    gui.key(Modifiers::NONE, Key::Escape);
    assert!(gui.text_view().caret().is_none());
    gui.key(CMD, Key::S);
    let read = |f: &str| std::fs::read_to_string(d.join(f)).unwrap();
    let (a, b) = (read("a.txt"), read("b.txt"));
    // F2 goes into the active side: one file has the edit, saved, and the other is unchanged.
    assert!(
        (a == "same\n    older\nsame\n" && b == "same\nnew\nsame\n")
            || (a == "same\nold\nsame\n" && b == "same\n    newer\nsame\n"),
        "{a:?} {b:?}"
    );
    assert!(!gui.text_view().panes.iter().any(|p| p.doc.modified));
    assert_eq!(gui.app.tabs.len(), 1);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn the_inline_view_is_read_only_and_switching_views_keeps_every_edit() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(
        d,
        &[
            ("a.txt", "one\ntwo\nthree\nfour\n"),
            ("b.txt", "one\nTWO\nthree\nfour\nfive\n"),
        ],
    );
    let mut gui = Gui::start("inline_edit", compare(&[d.join("a.txt"), d.join("b.txt")]));
    // A typed edit and a selection over several lines, then the inline view.
    text_view_mut(&mut gui).place_caret(1, (2, 5), false);
    gui.type_text("!");
    let v = text_view_mut(&mut gui);
    v.place_caret(1, (0, 1), false);
    v.place_caret(1, (2, 2), true);
    let selection = v.caret().map(|c| (c.anchor, c.pos));
    let block = v.cur_block;
    v.unified = true;
    gui.frames(3);
    assert!(gui.shows_text(tr("msg.inline_read_only")));
    let v = gui.text_view();
    assert!(v.shows_inline() && !v.takes_text() && !v.handles_escape());
    assert_eq!(v.panes[1].doc.lines[2], "three!", "the edit is kept");
    // Indenting, Replace and typing do nothing here.
    gui.key(Modifiers::CTRL.plus(CMD), Key::CloseBracket);
    gui.type_text("x");
    assert_eq!(
        gui.text_view().panes[1].doc.lines,
        ["one", "TWO", "three!", "four", "five"]
    );
    // Back with the toggle: the selection and the selected difference are as they were.
    text_view_mut(&mut gui).unified = false;
    gui.frames(3);
    let v = gui.text_view();
    assert_eq!(v.caret().map(|c| (c.anchor, c.pos)), selection);
    assert_eq!(v.cur_block, block);
    assert!(v.takes_text());
    // Edit side by side from another line of the inline view: the caret goes to its start.
    text_view_mut(&mut gui).unified = true;
    gui.frames(3);
    gui.key(Modifiers::NONE, Key::ArrowDown);
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let (row, pane) = {
        let v = gui.text_view();
        (v.cursor_row.unwrap(), v.active_pane)
    };
    gui.key(Modifiers::NONE, Key::Enter);
    let v = gui.text_view();
    assert!(!v.unified);
    let line = v.diff.rows[row].line(pane).unwrap();
    assert_eq!(v.caret().map(|c| (c.pane, c.pos)), Some((pane, (line, 0))));
    // The button does the same, and typing then edits that line.
    text_view_mut(&mut gui).unified = true;
    gui.frames(3);
    gui.click(tr("cmd.edit_side_by_side"));
    assert!(!gui.text_view().unified);
    assert!(gui.text_view().caret().is_some());
    gui.type_text("+");
    let v = gui.text_view();
    let c = v.caret().unwrap();
    assert!(v.panes[c.pane].doc.lines[c.pos.0].starts_with('+'));
    // Every edit is still one undo step away.
    text_view_mut(&mut gui).undo();
    text_view_mut(&mut gui).undo();
    assert_eq!(
        gui.text_view().panes[1].doc.lines,
        ["one", "TWO", "three", "four", "five"]
    );
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}

#[test]
fn ime_selection_commit_cancel_and_save_keep_unicode_and_format_in_both_themes() {
    use crate::config::TextLayout;
    for theme in [ThemeMode::Light, ThemeMode::Dark] {
        for layout in [TextLayout::Aligned, TextLayout::Compact] {
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path();
            let original = "\u{feff}keep alpha e\u{301} 😀\r\n中文 tail";
            write(d, &[("left.txt", original), ("right.txt", original)]);
            let cfg = Config {
                theme,
                text_layout: layout,
                ..config()
            };
            let mut gui = Gui::with_config(
                "ime_unicode_format",
                cfg,
                compare(&[d.join("left.txt"), d.join("right.txt")]),
            );
            text_view_mut(&mut gui).place_caret(1, (0, 5), false);
            text_view_mut(&mut gui).place_caret(1, (0, 10), true);
            let preedit = |text: &str| {
                Event::Ime(egui::ImeEvent::Preedit {
                    text: text.into(),
                    active_range_chars: None,
                })
            };
            gui.events.push(preedit("ni"));
            gui.frame();
            assert_eq!(gui.text_view().caret_text().as_deref(), Some("alpha"));
            assert!(!gui.text_view().panes[1].doc.modified);
            // A comparison refresh during preedit keeps the selection and the text.
            text_view_mut(&mut gui).rediff();
            gui.events.push(preedit(""));
            gui.frame();
            assert_eq!(gui.text_view().caret_text().as_deref(), Some("alpha"));
            assert!(!gui.text_view().panes[1].doc.modified);
            gui.events.push(preedit("ni"));
            gui.frame();
            let committed = "你好 e\u{301} 👩\u{200d}💻";
            gui.events
                .push(Event::Ime(egui::ImeEvent::Commit(committed.into())));
            gui.frame();
            let expected = original.replacen("alpha", committed, 1);
            assert_eq!(
                gui.text_view().panes[1].doc.lines[0],
                "keep 你好 e\u{301} 👩\u{200d}💻 e\u{301} 😀"
            );
            gui.key(CMD, Key::Z);
            assert_eq!(
                gui.text_view().panes[1].doc.lines[0],
                "keep alpha e\u{301} 😀"
            );
            gui.key(CMD.plus(Modifiers::SHIFT), Key::Z);
            gui.key(CMD, Key::S);
            assert_eq!(
                std::fs::read(d.join("right.txt")).unwrap(),
                expected.as_bytes()
            );
            assert_eq!(
                std::fs::read(d.join("left.txt")).unwrap(),
                original.as_bytes()
            );
            assert!(gui.errors().is_empty(), "{:?}", gui.errors());
        }
    }
}

#[test]
fn binary_moved_blocks_toggle_and_navigate_without_changing_merge_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (a, b, c): (Vec<u8>, Vec<u8>, Vec<u8>) =
        ((0..64).collect(), (64..128).collect(), (128..192).collect());
    let left = [a.clone(), b.clone(), c.clone()].concat();
    let right = [a, c, b].concat();
    std::fs::write(d.join("left.bin"), &left).unwrap();
    std::fs::write(d.join("right.bin"), &right).unwrap();
    let mut gui = Gui::start(
        "binary_moved_blocks",
        compare(&[d.join("left.bin"), d.join("right.bin")]),
    );
    let binary = |g: &Gui| match g.app.tabs[g.app.active].content.view() {
        Content::Binary(v) => (
            v.data.clone(),
            v.diffs.clone(),
            v.moved.clone(),
            v.pane,
            v.cursor,
        ),
        _ => panic!("expected a binary comparison"),
    };
    let wait_for_moves = |g: &mut Gui| {
        g.frame();
        if let Content::Binary(v) = g.app.tabs[g.app.active].content.view_mut() {
            v.wait_for_moved_scan();
        }
        g.frame();
    };
    wait_for_moves(&mut gui);
    let original = binary(&gui);
    assert_eq!(original.2.len(), 2);
    match gui.app.tabs[gui.app.active].content.view_mut() {
        Content::Binary(v) => {
            v.pane = Some(0);
            v.move_cursor(69);
        }
        _ => unreachable!(),
    }
    gui.frames(2);
    gui.tab_to("go to matching block", |w| {
        w.role == accesskit::Role::Button && w.label == tr("binary.moved_partner")
    });
    gui.key(Modifiers::NONE, Key::Enter);
    let after = binary(&gui);
    assert_eq!((after.3, after.4), (Some(1), Some(133)));
    assert_eq!(after.0, original.0);
    assert_eq!(after.1, original.1);
    gui.key(Modifiers::NONE, Key::ArrowRight);
    assert_eq!((binary(&gui).3, binary(&gui).4), (Some(1), Some(134)));
    gui.click(tr("binary.moved_blocks"));
    assert!(binary(&gui).2.is_empty());
    assert!(!gui.app.cfg.binary_moved_blocks);
    gui.click(tr("binary.moved_blocks"));
    assert!(gui.app.cfg.binary_moved_blocks);
    wait_for_moves(&mut gui);
    assert_eq!(binary(&gui).2, original.2);
    if let Content::Binary(v) = gui.app.tabs[gui.app.active].content.view_mut() {
        v.ascii = true;
    }
    gui.tab_to("moved-block checkbox", |w| {
        w.role == accesskit::Role::CheckBox && w.label == tr("binary.moved_blocks")
    });
    gui.events.push(Event::Text(" ".into()));
    gui.key(Modifiers::NONE, Key::Space);
    assert_eq!(
        binary(&gui).0,
        original.0,
        "Space on a toolbar control must not type into the byte pane"
    );
    assert!(binary(&gui).2.is_empty());
    gui.click(tr("binary.moved_blocks"));
    wait_for_moves(&mut gui);
    assert_eq!(binary(&gui).2, original.2);
    assert_eq!(std::fs::read(d.join("left.bin")).unwrap(), left);
    assert_eq!(std::fs::read(d.join("right.bin")).unwrap(), right);
    assert!(gui.errors().is_empty(), "{:?}", gui.errors());
}
