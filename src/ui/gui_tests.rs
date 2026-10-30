//! Headless end-to-end tests of the main window. Each test drives the real application one
//! frame at a time with pointer, keyboard, text and file-drop input, finds widgets by the names
//! they give assistive technologies, and checks the views and files that result. A failing test
//! leaves what the window last showed in `target/gui-tests/<test>.txt`.

use super::*;
use crate::core::diff::BlockKind;
use egui::{accesskit, Event, Modifiers, PointerButton, Pos2, Rect};
use std::time::{Duration, Instant};

const SCREEN: egui::Vec2 = egui::vec2(1280.0, 820.0);

struct Widget {
    role: accesskit::Role,
    label: String,
    rect: Rect,
    enabled: bool,
}

struct Gui {
    name: &'static str,
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
    copied: Vec<String>,
    closed: bool,
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
            ctx,
            app,
            frame: eframe::Frame::_new_kittest(),
            time: 0.0,
            events: Vec::new(),
            modifiers: Modifiers::NONE,
            dropped: Vec::new(),
            focused: true,
            widgets: Vec::new(),
            copied: Vec::new(),
            closed: false,
        };
        gui.settle();
        gui
    }

    fn frame(&mut self) {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            modifiers: self.modifiers,
            events: std::mem::take(&mut self.events),
            dropped_files: self
                .dropped
                .drain(..)
                .map(|p| egui::DroppedFile {
                    path: Some(p),
                    ..Default::default()
                })
                .collect(),
            focused: self.focused,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        eframe::App::raw_input_hook(&mut self.app, &self.ctx, &mut raw);
        let (app, frame) = (&mut self.app, &mut self.frame);
        let out = self
            .ctx
            .run(raw, |ctx| eframe::App::update(app, ctx, frame));
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                self.copied.push(t.clone());
            }
        }
        if let Some(tree) = &out.platform_output.accesskit_update {
            self.widgets = widgets_of(tree);
        }
        self.closed |= out
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::Close));
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

    fn busy(&self) -> bool {
        self.app.report_job.is_some()
            || self
                .app
                .tabs
                .iter()
                .any(|t| matches!(t.content.view(), Content::Folder(v) if v.busy()))
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

    /// Picks an item of the menu bar: `["File", "Export", "Generate patch..."]`. Items are told
    /// from buttons of the same name by where they are: under their menu, or beside it.
    fn menu(&mut self, path: &[&str]) {
        let mut anchor = self.widget(path[0]).rect;
        self.click(path[0]);
        for (depth, item) in path.iter().enumerate().skip(1) {
            let found = self.widgets.iter().rev().find(|w| {
                let r = w.rect;
                named(w, item)
                    && if depth == 1 {
                        (r.left() - anchor.left()).abs() < 2.0 && r.top() >= anchor.bottom()
                    } else {
                        r.left() >= anchor.right() - 4.0 && r.left() <= anchor.right() + 24.0
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

fn named(w: &Widget, label: &str) -> bool {
    w.label
        .strip_prefix(label)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("  ("))
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
        // Plain text has its words as the value.
        let (Some(label), Some(b)) = (n.label().or(n.value()), n.bounds()) else {
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
        });
    }
    out
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

    // A file changed between the preview and OK is kept as it is, and named.
    gui.key(Modifiers::NONE, Key::F5);
    gui.click(tr("cmd.folder_merge"));
    let target = roots[1].join("changed.txt");
    std::fs::write(&target, "mine\n").unwrap();
    touch_later(&target, 5);
    gui.click(tr("btn.ok"));
    assert_eq!(read(&target), "mine\n");
    assert_eq!(read(&roots[1].join("sub/added.txt")), "added\n");
    assert!(!roots[1].join("gone.txt").exists());
    let status = gui.app.tabs[0].status_line();
    assert!(
        status.contains(tr("status.error")) && status.contains("changed.txt"),
        "{status}"
    );
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
    assert!(!gui.ctx.style().visuals.dark_mode);
    gui.click(tr("view.dark_mode"));
    assert!(gui.ctx.style().visuals.dark_mode);

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
        gui.click(tr(key));
        gui.frames(3);
        let open: Vec<f32> = gui.ctx.memory(|m| {
            m.layer_ids()
                .filter(|l| l.order == egui::Order::Foreground && m.areas().visible_last_frame(l))
                .filter_map(|l| m.area_rect(l.id))
                .map(|r| r.width())
                .collect()
        });
        assert_eq!(open.len(), 1, "{key}: {open:?}");
        widths.push((key, open[0]));
        gui.click(tr(key));
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
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            for (layer, list) in [(MENU_MEASURE, false), (MENU_MEASURE, true)] {
                let shown = egui::Area::new(egui::Id::new(layer).with(list)).show(ctx, |ui| {
                    let mut ui = ui.new_child(egui::UiBuilder::new().layer_id(egui::LayerId::new(
                        egui::Order::Middle,
                        egui::Id::new(layer),
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
        });
    }
    // The style first: `fonts` holds the context's lock, which `style` also needs.
    let font = egui::TextStyle::Button.resolve(&ctx.style());
    let text = ctx.fonts(|f| {
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
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::Area::new(egui::Id::new("checks")).show(ctx, |ui| {
            for on in [true, false] {
                rects.push(widgets::menu_check(ui, on, "Tree mode", "").rect);
            }
        });
    });
    assert_eq!(rects[0].width(), rects[1].width(), "{rects:?}");
}

#[test]
fn submenus_are_measured_with_their_arrow() {
    let ctx = egui::Context::default();
    let mut widths = Vec::new();
    for _ in 0..2 {
        widths.clear();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            for layer in [MENU_MEASURE, "elsewhere"] {
                let shown = egui::Area::new(egui::Id::new(layer)).show(ctx, |ui| {
                    submenu(ui, "Export", |_| ());
                });
                widths.push(shown.response.rect.width());
            }
        });
    }
    // Only where the menus are measured, as menus draw the arrow themselves.
    let spacing = ctx.style().spacing.item_spacing.x;
    assert!(widths[0] > widths[1] + spacing, "{widths:?}");
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
    let original = Gui::with_config(
        "converted_review_capture",
        cfg,
        compare(&[d.join("left"), d.join("right")]),
    );
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("review.oxmreview");
    let Content::Folder(view) = &original.app.tabs[0].content else {
        panic!("folder expected");
    };
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
        .fonts(|f| f.row_height(&egui::FontId::monospace(config().font_size)))
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
    gui.key(Modifiers::NONE, Key::F2);
    gui.key(CMD, Key::A);
    gui.type_text("edited common");
    gui.key(Modifiers::NONE, Key::Enter);
    assert_eq!(gui.text_view().panes[0].doc.lines[1], "edited common");
    gui.key(Modifiers::NONE, Key::ArrowDown);
    let v = gui.text_view();
    assert_eq!(v.diff.rows[v.cursor_row.unwrap()].line(0), Some(2));
}

#[test]
fn semantic_view_is_read_only_and_can_jump_back_to_source() {
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
    let start = Instant::now();
    while !gui.shows_text("timeout") || !gui.shows_text("30 → 60") {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "semantic worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
        gui.frame();
    }
    assert!(gui.shows(tr("analysis.semantic_read_only")));
    gui.click(&format!("{} 2:2", tr("analysis.source")));
    assert_eq!(gui.text_view().active_pane, 1);
    assert_eq!(gui.text_view().panes[1].doc.lines[1], "  \"timeout\": 60");
    assert_eq!(
        read(&dir.path().join("b.json")),
        "{\n  \"timeout\": 60\n}\n"
    );
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
    gui.click(tr("analysis.merge_verification"));
    gui.click(tr("analysis.preview_merge"));
    let start = Instant::now();
    while !gui.shows_text(tr("analysis.planned_changes")) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "merge preview did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
        gui.frame();
    }
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
    let mut gui = Gui::start("baseline_binary", None);
    gui.click(tr("baseline.title"));
    gui.click(tr("baseline.left"));
    gui.type_text(&format!("folder:{}", a.display()));
    gui.click(tr("baseline.right"));
    gui.type_text(&format!("folder:{}", b.display()));
    gui.click(tr("baseline.compare"));
    let start = Instant::now();
    while !gui.shows(tr("baseline.captured")) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "baseline worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
        gui.frame();
    }
    std::fs::remove_file(a.join("a.bin")).unwrap();
    std::fs::remove_file(b.join("a.bin")).unwrap();
    gui.click("BinaryChanged a.bin");
    let Content::Binary(view) = gui.app.tabs[gui.app.active].content.view() else {
        panic!("captured binary compare expected")
    };
    assert_eq!(view.data, [b"\0A".to_vec(), b"\0B".to_vec()]);
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
    let start = Instant::now();
    let source = format!("{} 2:6", tr("analysis.source"));
    while !gui.shows(&source) {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(1));
        gui.frame();
    }
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
        egui::menu::BarState::load(&gui.ctx, gui.app.menu_bar_id.unwrap()).is_some(),
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
