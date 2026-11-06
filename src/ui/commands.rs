//! What the menus, the command palette and the keyboard run: each command's label, icon and keys,
//! kept in one place so the three agree.

use crate::core::text::FileKind;
use crate::i18n::tr;
use crate::ui::icons::Icon;
use crate::ui::widgets;
use egui::{Key, KeyboardShortcut, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    New,
    /// A new text compare with the clipboard's current text on the left, to paste a second
    /// text into the right side and compare it right away.
    CompareClipboard,
    Open,
    /// Opens the connection dialog of *Compare over SFTP*, to stage a remote file or folder
    /// locally and compare it like any other path.
    OpenRemote,
    OpenConflict,
    OpenProject,
    SaveProject,
    SaveJob,
    RunJob,
    /// Saves the open tabs as a named workspace, to reopen with [`Cmd::OpenWorkspace`].
    SaveWorkspace,
    /// Opens every tab of the workspace at this index of [`crate::config::Config::workspaces`].
    OpenWorkspace(usize),
    ManageWorkspaces,
    /// Deletes the workspace at this index, from the "Manage workspaces" dialog.
    DeleteWorkspace(usize),
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
    /// Preview synchronizing the two compared folders.
    SyncFolders,
    Refresh,
    Patch,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NextTab,
    PrevTab,
    CompareAs(FileKind),
    Filters,
    EditRules,
    ToggleCollapse,
    ToggleWrap,
    Palette,
    Statistics,
    /// Indent (false) or outdent (true) the selected lines.
    Indent(bool),
    /// Put a copy of the selected lines under them.
    DuplicateLines,
    /// Move the selected lines up (true) or down (false).
    MoveLines(bool),
    BookmarkComment,
    SaveComparison,
    GitCompare,
    SelectPattern,
    HtmlReport,
    CompareOptions,
    /// Show the differences of every changed file of a folder compare in one tab.
    AllDiffs,
    /// Pick the line of the active pane for a synchronization point.
    SyncPoint,
    ClearSyncPoints,
    /// Save an HTML slideshow report: one difference per slide.
    Slideshow,
    /// Save the report as a PDF file.
    ExportPdf,
    /// Open the PDF report in the system's viewer, to print it from there.
    Print,
    /// Show or hide the file compare below the folder list.
    ToggleFilePane,
}

impl Cmd {
    /// Language key of the label.
    pub fn label(self) -> &'static str {
        use Cmd::*;
        match self {
            New => "menu.new",
            CompareClipboard => "menu.compare_clipboard",
            Open => "menu.open",
            OpenRemote => "menu.open_remote",
            OpenConflict => "menu.open_conflict",
            OpenProject => "menu.open_project",
            SaveProject => "menu.save_project",
            SaveJob => "menu.save_job",
            RunJob => "menu.run_job",
            SaveWorkspace => "menu.save_workspace",
            OpenWorkspace(_) => "menu.open_workspace",
            ManageWorkspaces => "menu.manage_workspaces",
            DeleteWorkspace(_) => "cmd.delete",
            ApplyPatch => "menu.apply_patch",
            ToggleBookmark => "cmd.toggle_bookmark",
            NextBookmark => "cmd.next_bookmark",
            PrevBookmark => "cmd.prev_bookmark",
            ClearBookmarks => "cmd.clear_bookmarks",
            TakeBoth(true) => "cmd.take_left_right",
            TakeBoth(false) => "cmd.take_right_left",
            Save => "menu.save",
            SaveAll => "btn.save_all",
            SaveAs(_) => "menu.save_side_as",
            CloseTab | CloseTabAt(_) => "menu.close_tab",
            CloseOtherTabs(_) => "tab.close_others",
            CloseTabsToRight(_) => "tab.close_right",
            CloseAllTabs => "tab.close_all",
            Exit => "menu.exit",
            Undo => "cmd.undo",
            Redo => "cmd.redo",
            Find => "cmd.find",
            Replace => "cmd.replace",
            FindNext => "cmd.find_next",
            FindPrev => "cmd.find_prev",
            GotoLine => "dlg.goto",
            Options => "menu.options",
            Help => "help.title",
            NextDiff => "cmd.next_diff",
            PrevDiff => "cmd.prev_diff",
            FirstDiff => "cmd.first_diff",
            LastDiff => "cmd.last_diff",
            NextConflict => "cmd.next_conflict",
            PrevConflict => "cmd.prev_conflict",
            CopyRight => "cmd.copy_right",
            CopyLeft => "cmd.copy_left",
            CopyAllRight => "cmd.copy_all_right",
            CopyAllLeft => "cmd.copy_all_left",
            AutoMerge => "cmd.auto_merge",
            SyncFolders => "cmd.sync_folders",
            Refresh => "cmd.refresh",
            Patch => "menu.patch",
            ZoomIn => "cmd.zoom_in",
            ZoomOut => "cmd.zoom_out",
            ZoomReset => "cmd.zoom_reset",
            NextTab => "menu.next_tab",
            PrevTab => "menu.prev_tab",
            CompareAs(_) => "cmd.compare_as",
            Filters => "menu.filters",
            EditRules => "cmd.edit_rules",
            ToggleCollapse => "cmd.collapse_unchanged",
            ToggleWrap => "cmd.word_wrap",
            Palette => "palette.title",
            Statistics => "menu.statistics",
            Indent(false) => "cmd.indent",
            Indent(true) => "cmd.outdent",
            DuplicateLines => "cmd.duplicate_lines",
            MoveLines(true) => "cmd.move_lines_up",
            MoveLines(false) => "cmd.move_lines_down",
            BookmarkComment => "cmd.bookmark_comment",
            SaveComparison => "menu.save_comparison",
            GitCompare => "menu.git_compare",
            SelectPattern => "cmd.select_pattern",
            HtmlReport => "menu.export_html",
            CompareOptions => "view.more_compare",
            AllDiffs => "cmd.all_diffs",
            SyncPoint => "cmd.sync_point",
            ClearSyncPoints => "cmd.clear_sync_points",
            Slideshow => "menu.slideshow",
            ExportPdf => "menu.export_pdf",
            Print => "menu.print",
            ToggleFilePane => "view.folder_file_pane",
        }
    }

    /// Icon in the command palette.
    pub fn icon(self) -> Icon {
        use Cmd::*;
        match self {
            New | CompareClipboard => Icon::Plus,
            Open | OpenProject | RunJob => Icon::Folder,
            OpenRemote => Icon::Globe,
            OpenConflict => Icon::Warning,
            Save | SaveAll | SaveAs(_) | SaveProject | SaveJob | SaveComparison | SaveWorkspace => {
                Icon::File
            }
            OpenWorkspace(_) | ManageWorkspaces => Icon::Folder,
            DeleteWorkspace(_) => Icon::Close,
            Patch | ApplyPatch | BookmarkComment => Icon::Text,
            Slideshow | ExportPdf | Print => Icon::Export,
            HtmlReport => Icon::Export,
            GitCompare | TakeBoth(_) | AllDiffs | SyncPoint => Icon::Compare,
            Statistics | GotoLine => Icon::Method,
            Indent(false) | NextBookmark | FindNext | NextDiff | NextTab => Icon::Next,
            Indent(true) | PrevBookmark | FindPrev | PrevDiff | PrevTab => Icon::Prev,
            DuplicateLines => Icon::Plus,
            MoveLines(true) => Icon::Prev,
            MoveLines(false) => Icon::Next,
            SelectPattern | ToggleBookmark => Icon::Check,
            ClearBookmarks | ClearSyncPoints | CloseTab | CloseTabAt(_) | CloseOtherTabs(_)
            | CloseTabsToRight(_) | CloseAllTabs | Exit => Icon::Close,
            Undo => Icon::Undo,
            Redo => Icon::Redo,
            Find | Replace => Icon::Search,
            FirstDiff => Icon::First,
            LastDiff => Icon::Last,
            NextConflict => Icon::NextConflict,
            PrevConflict => Icon::PrevConflict,
            CopyRight => Icon::CopyRight,
            CopyLeft => Icon::CopyLeft,
            CopyAllRight | SyncFolders => Icon::CopyAllRight,
            CopyAllLeft => Icon::CopyAllLeft,
            AutoMerge => Icon::Bolt,
            ToggleCollapse => Icon::Fold,
            ToggleWrap => Icon::Wrap,
            ToggleFilePane => Icon::BottomPane,
            Refresh => Icon::Refresh,
            ZoomIn => Icon::ZoomIn,
            ZoomOut => Icon::ZoomOut,
            ZoomReset => Icon::Fit,
            Filters => Icon::Filter,
            Options | CompareOptions | EditRules => Icon::Settings,
            Help => Icon::Help,
            Palette => Icon::Palette,
            CompareAs(FileKind::Text) => Icon::Text,
            CompareAs(FileKind::Binary) => Icon::Binary,
            CompareAs(FileKind::Image) => Icon::Image,
            CompareAs(FileKind::Web) => Icon::Globe,
        }
    }

    /// The first key of the command, written like "Ctrl+Shift+S" ("" when it has none).
    pub fn key(self) -> &'static str {
        BINDINGS
            .iter()
            .find(|b| b.cmd == self)
            .map_or("", |b| b.written)
    }

    /// The first key of the command as this platform shows it ("" when it has none).
    pub fn shown_key(self) -> String {
        widgets::key_text(self.key())
    }

    /// Tooltip of a toolbar button that runs the command: its label and key.
    pub fn tip(self) -> String {
        widgets::with_key(tr(self.label()), self.key())
    }

    /// [`Self::tip`], without the key when the keys don't reach the view, such as the file compare
    /// docked in a folder compare, where they act on the folder.
    pub fn tip_if(self, keys: bool) -> String {
        if keys {
            self.tip()
        } else {
            tr(self.label()).to_string()
        }
    }
}

/// When a key runs its command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Also while a text field has keyboard focus.
    Always,
    /// Only while no text field has keyboard focus.
    View,
    /// Shown only: the system, or [`crate::ui::app`] before egui sees the key, handles it.
    System,
}

#[derive(Clone, Copy)]
pub struct Binding {
    pub key: KeyboardShortcut,
    /// The key as the Help & Shortcuts dialog writes it.
    pub written: &'static str,
    pub cmd: Cmd,
    pub scope: Scope,
}

const NONE: Modifiers = Modifiers::NONE;
const SHIFT: Modifiers = Modifiers::SHIFT;
const ALT: Modifiers = Modifiers::ALT;
const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CTRL_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);
const ALT_SHIFT: Modifiers = Modifiers::ALT.plus(Modifiers::SHIFT);
/// The Control key itself, also on macOS (where ⌘Tab switches applications).
const CONTROL: Modifiers = Modifiers::CTRL;
const CONTROL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

const fn bind(
    scope: Scope,
    modifiers: Modifiers,
    key: Key,
    written: &'static str,
    cmd: Cmd,
) -> Binding {
    Binding {
        key: KeyboardShortcut::new(modifiers, key),
        written,
        cmd,
        scope,
    }
}

const MAC: bool = cfg!(target_os = "macos");

const REPLACE: Binding = if MAC {
    bind(Scope::Always, CTRL_ALT, Key::F, "Ctrl+Alt+F", Cmd::Replace)
} else {
    bind(Scope::Always, CTRL, Key::H, "Ctrl+H", Cmd::Replace)
};
/// Redo as the platform usually writes it; the other key works as well.
const REDO: [Binding; 2] = {
    let y = bind(Scope::View, CTRL, Key::Y, "Ctrl+Y", Cmd::Redo);
    let z = bind(Scope::View, CTRL_SHIFT, Key::Z, "Ctrl+Shift+Z", Cmd::Redo);
    if MAC {
        [z, y]
    } else {
        [y, z]
    }
};
const EXIT: Binding = if MAC {
    bind(Scope::System, CTRL, Key::Q, "Ctrl+Q", Cmd::Exit)
} else {
    bind(Scope::System, ALT, Key::F4, "Alt+F4", Cmd::Exit)
};

/// Every key, in the order they are tried. A key without Shift also matches with Shift held, so
/// Shift variants come first. Menus and the palette show the first key of each command.
pub const BINDINGS: &[Binding] = {
    use Cmd::*;
    use Scope::{Always as A, System as S, View as V};
    &[
        bind(A, CTRL, Key::K, "Ctrl+K", Palette),
        bind(A, CTRL_SHIFT, Key::P, "Ctrl+Shift+P", Palette),
        bind(A, CTRL, Key::P, "Ctrl+P", Print),
        bind(A, CTRL, Key::N, "Ctrl+N", New),
        bind(A, CTRL, Key::O, "Ctrl+O", Open),
        bind(A, CTRL_SHIFT, Key::S, "Ctrl+Shift+S", SaveAll),
        bind(A, CTRL_SHIFT, Key::E, "Ctrl+Shift+E", HtmlReport),
        bind(A, CTRL, Key::S, "Ctrl+S", Save),
        bind(A, CTRL, Key::W, "Ctrl+W", CloseTab),
        bind(A, CTRL, Key::F4, "Ctrl+F4", CloseTab),
        bind(V, NONE, Key::Escape, "Esc", CloseTab),
        REPLACE,
        bind(A, CTRL, Key::F, "Ctrl+F", Find),
        bind(A, CTRL, Key::G, "Ctrl+G", GotoLine),
        // Taken from the input before egui moves the keyboard focus with Tab.
        bind(S, CONTROL_SHIFT, Key::Tab, "Control+Shift+Tab", PrevTab),
        bind(S, CONTROL, Key::Tab, "Control+Tab", NextTab),
        bind(A, CTRL, Key::Plus, "Ctrl++", ZoomIn),
        bind(A, CTRL, Key::Equals, "Ctrl+=", ZoomIn),
        bind(A, CTRL, Key::Minus, "Ctrl+-", ZoomOut),
        bind(A, CTRL, Key::Num0, "Ctrl+0", ZoomReset),
        bind(A, SHIFT, Key::F3, "Shift+F3", FindPrev),
        bind(A, NONE, Key::F3, "F3", FindNext),
        bind(A, NONE, Key::F5, "F5", Refresh),
        bind(A, NONE, Key::F1, "F1", Help),
        EXIT,
        bind(V, NONE, Key::Questionmark, "?", Help),
        REDO[0],
        REDO[1],
        bind(V, CTRL, Key::Z, "Ctrl+Z", Undo),
        bind(V, CTRL_ALT, Key::ArrowRight, "Ctrl+Alt+→", CopyAllRight),
        bind(V, CTRL_ALT, Key::ArrowLeft, "Ctrl+Alt+←", CopyAllLeft),
        // Before Alt+↓ and Alt+↑, which Alt+Shift+↓ and Alt+Shift+↑ would match too.
        bind(
            V,
            ALT_SHIFT,
            Key::ArrowDown,
            "Alt+Shift+↓",
            MoveLines(false),
        ),
        bind(V, ALT_SHIFT, Key::ArrowUp, "Alt+Shift+↑", MoveLines(true)),
        bind(V, ALT, Key::ArrowDown, "Alt+↓", NextDiff),
        bind(V, ALT, Key::ArrowUp, "Alt+↑", PrevDiff),
        bind(V, ALT, Key::Home, "Alt+Home", FirstDiff),
        bind(V, ALT, Key::End, "Alt+End", LastDiff),
        bind(V, ALT, Key::PageDown, "Alt+PgDn", NextConflict),
        bind(V, ALT, Key::PageUp, "Alt+PgUp", PrevConflict),
        bind(V, ALT, Key::ArrowRight, "Alt+→", CopyRight),
        bind(V, ALT, Key::ArrowLeft, "Alt+←", CopyLeft),
        bind(V, ALT, Key::C, "Alt+C", ToggleCollapse),
        bind(V, ALT, Key::Z, "Alt+Z", ToggleWrap),
        bind(V, ALT, Key::P, "Alt+P", ToggleFilePane),
        bind(V, ALT, Key::S, "Alt+S", SyncPoint),
        bind(V, CTRL, Key::F2, "Ctrl+F2", ToggleBookmark),
        bind(V, CTRL, Key::CloseBracket, "Ctrl+]", Indent(false)),
        bind(V, CTRL, Key::OpenBracket, "Ctrl+[", Indent(true)),
        bind(V, CTRL, Key::D, "Ctrl+D", DuplicateLines),
        bind(V, SHIFT, Key::F4, "Shift+F4", PrevBookmark),
        bind(V, NONE, Key::F4, "F4", NextBookmark),
    ]
};

/// Whether `b` is a key that types a character into text: on macOS, Option (Alt) with a letter,
/// a digit or a sign types one (Option+S types ß). While the text caret takes typing, such
/// shortcuts are left to it and stay in the menus.
pub fn types_text(b: &Binding) -> bool {
    types_text_on(MAC, b)
}

/// [`types_text`] on macOS (`mac`) or on Windows and Linux, for tests of both.
pub fn types_text_on(mac: bool, b: &Binding) -> bool {
    let m = b.key.modifiers;
    mac && m.alt
        && !m.command
        && !m.ctrl
        && !m.mac_cmd
        && b.key.logical_key.name().chars().count() == 1
}

/// Keys that some keyboard layouts type with AltGr, which arrives as Ctrl+Alt.
fn typed_with_alt(key: Key) -> bool {
    matches!(
        key,
        Key::Plus
            | Key::Equals
            | Key::Minus
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::Questionmark
    )
}

/// Takes a press of `b` from the input. Unlike [`egui::InputState::consume_shortcut`], a held Alt
/// that the key does not use keeps it from matching, so Alt+F4 is not also F4.
pub fn consume(i: &mut egui::InputState, b: &Binding) -> bool {
    let KeyboardShortcut {
        modifiers: want,
        logical_key,
    } = b.key;
    if typed_with_alt(logical_key) {
        return i.consume_shortcut(&b.key);
    }
    let mut hit = false;
    i.events.retain(|e| {
        let m = matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. }
            if *key == logical_key && modifiers.alt == want.alt && modifiers.matches_logically(want));
        hit |= m;
        !m
    });
    hit
}

/// Shortcut of Replace, as the Help & Shortcuts dialog writes it.
pub const REPLACE_KEY: &str = REPLACE.written;
/// Shortcut of Exit, as the Help & Shortcuts dialog writes it.
pub const EXIT_KEY: &str = EXIT.written;

/// Commands offered by the command palette.
pub const PALETTE: &[Cmd] = {
    use Cmd::*;
    &[
        New,
        CompareClipboard,
        Open,
        OpenRemote,
        OpenConflict,
        OpenProject,
        SaveProject,
        SaveJob,
        RunJob,
        SaveWorkspace,
        ManageWorkspaces,
        Save,
        SaveAll,
        Patch,
        ApplyPatch,
        HtmlReport,
        Slideshow,
        ExportPdf,
        Print,
        AllDiffs,
        SaveComparison,
        GitCompare,
        Statistics,
        Indent(false),
        Indent(true),
        DuplicateLines,
        MoveLines(true),
        MoveLines(false),
        BookmarkComment,
        SelectPattern,
        ToggleBookmark,
        NextBookmark,
        PrevBookmark,
        ClearBookmarks,
        TakeBoth(true),
        TakeBoth(false),
        SyncPoint,
        ClearSyncPoints,
        CloseTab,
        CloseAllTabs,
        NextTab,
        PrevTab,
        Undo,
        Redo,
        Find,
        Replace,
        FindNext,
        FindPrev,
        GotoLine,
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
        SyncFolders,
        AutoMerge,
        ToggleCollapse,
        ToggleWrap,
        Refresh,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Filters,
        EditRules,
        Options,
        Help,
        Exit,
    ]
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Key names as written, for the keys the bindings use.
    fn parse(written: &str) -> KeyboardShortcut {
        let mut modifiers = Modifiers::NONE;
        let mut rest = written;
        for (prefix, m) in [
            ("Control+", Modifiers::CTRL),
            ("Ctrl+", Modifiers::COMMAND),
            ("Alt+", Modifiers::ALT),
            ("Shift+", Modifiers::SHIFT),
        ] {
            if let Some(r) = rest.strip_prefix(prefix) {
                modifiers = modifiers.plus(m);
                rest = r;
            }
        }
        let key = match rest {
            "→" => Key::ArrowRight,
            "←" => Key::ArrowLeft,
            "↑" => Key::ArrowUp,
            "↓" => Key::ArrowDown,
            "PgDn" => Key::PageDown,
            "PgUp" => Key::PageUp,
            _ => Key::from_name(rest).unwrap_or_else(|| panic!("unknown key {written}")),
        };
        KeyboardShortcut::new(modifiers, key)
    }

    #[test]
    fn written_keys_match_the_bindings() {
        for b in BINDINGS {
            assert_eq!(parse(b.written), b.key, "{}", b.written);
        }
    }

    #[test]
    fn shift_variants_are_tried_first() {
        for (i, b) in BINDINGS.iter().enumerate() {
            if b.key.modifiers.shift {
                continue;
            }
            for later in &BINDINGS[i + 1..] {
                let shifted = later.key.modifiers.shift
                    && later.key.logical_key == b.key.logical_key
                    && later.key.modifiers.command == b.key.modifiers.command
                    && later.key.modifiers.ctrl == b.key.modifiers.ctrl
                    && later.key.modifiers.alt == b.key.modifiers.alt;
                assert!(!shifted, "{} must come before {}", later.written, b.written);
            }
        }
    }

    #[test]
    fn a_held_alt_the_key_does_not_use_keeps_it_from_matching() {
        let press = |key, modifiers| {
            let mut i = egui::InputState::default();
            i.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
            i
        };
        let f4 = BINDINGS.iter().find(|b| b.written == "F4").unwrap();
        assert!(!consume(&mut press(Key::F4, Modifiers::ALT), f4));
        assert!(consume(&mut press(Key::F4, Modifiers::NONE), f4));
        let find = BINDINGS.iter().find(|b| b.written == "Ctrl+F").unwrap();
        assert!(!consume(&mut press(Key::F, CTRL_ALT), find));
        let indent = BINDINGS.iter().find(|b| b.written == "Ctrl+]").unwrap();
        assert!(consume(&mut press(Key::CloseBracket, CTRL_ALT), indent));
    }

    #[test]
    fn option_with_a_letter_types_on_macos_and_stays_a_shortcut_elsewhere() {
        let find = |w: &str| BINDINGS.iter().find(|b| b.written == w).unwrap();
        for w in ["Alt+S", "Alt+Z", "Alt+C", "Alt+P"] {
            assert!(types_text_on(true, find(w)), "{w}");
            assert!(!types_text_on(false, find(w)), "{w}");
        }
        // Option with an arrow, Home or a page key moves; ⌘ and Ctrl keys never type.
        for w in [
            "Alt+↓",
            "Alt+→",
            "Alt+Home",
            "Alt+PgUp",
            "Ctrl+Alt+→",
            "Ctrl+S",
            "F4",
        ] {
            assert!(!types_text_on(true, find(w)), "{w}");
        }
    }

    #[test]
    fn every_label_is_translated() {
        let en: serde_json::Value =
            serde_json::from_str(include_str!("../../lang/en.json")).unwrap();
        for c in PALETTE.iter().chain(BINDINGS.iter().map(|b| &b.cmd)) {
            assert!(en.get(c.label()).is_some(), "missing {}", c.label());
        }
    }
}
