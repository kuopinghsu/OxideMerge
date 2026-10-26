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
    EditRules,
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
    ExportHtml,
    CompareOptions,
}

impl Cmd {
    /// Language key of the label.
    pub fn label(self) -> &'static str {
        use Cmd::*;
        match self {
            New => "menu.new",
            Open => "menu.open",
            OpenConflict => "menu.open_conflict",
            OpenProject => "menu.open_project",
            SaveProject => "menu.save_project",
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
            Refresh => "cmd.refresh",
            Patch => "menu.patch",
            Report => "menu.report",
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
            BookmarkComment => "cmd.bookmark_comment",
            SaveComparison => "menu.save_comparison",
            GitCompare => "menu.git_compare",
            ReportWithDiffs => "menu.report_diffs",
            SelectPattern => "cmd.select_pattern",
            ExportHtml => "menu.export_html",
            CompareOptions => "view.more_compare",
        }
    }

    /// Icon in the command palette.
    pub fn icon(self) -> Icon {
        use Cmd::*;
        match self {
            New => Icon::Plus,
            Open | OpenProject => Icon::Folder,
            OpenConflict => Icon::Warning,
            Save | SaveAll | SaveAs(_) | SaveProject | SaveComparison => Icon::File,
            Patch | ApplyPatch | Report | ReportWithDiffs | BookmarkComment => Icon::Text,
            ExportHtml => Icon::Export,
            GitCompare | TakeBoth(_) => Icon::Compare,
            Statistics | GotoLine => Icon::Method,
            Indent(false) | NextBookmark | FindNext | NextDiff | NextTab => Icon::Next,
            Indent(true) | PrevBookmark | FindPrev | PrevDiff | PrevTab => Icon::Prev,
            SelectPattern | ToggleBookmark => Icon::Check,
            ClearBookmarks | CloseTab | CloseTabAt(_) | CloseOtherTabs(_) | CloseTabsToRight(_)
            | CloseAllTabs | Exit => Icon::Close,
            Undo => Icon::Undo,
            Redo => Icon::Redo,
            Find | Replace => Icon::Search,
            FirstDiff => Icon::First,
            LastDiff => Icon::Last,
            NextConflict => Icon::NextConflict,
            PrevConflict => Icon::PrevConflict,
            CopyRight => Icon::CopyRight,
            CopyLeft => Icon::CopyLeft,
            CopyAllRight => Icon::CopyAllRight,
            CopyAllLeft => Icon::CopyAllLeft,
            AutoMerge => Icon::Bolt,
            ToggleCollapse => Icon::Fold,
            ToggleWrap => Icon::Wrap,
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
}

/// When a key runs its command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Also while a text field has keyboard focus.
    Always,
    /// Only while no text field has keyboard focus.
    View,
    /// Shown only: the system or window manager handles the key.
    System,
}

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
    use Scope::{Always as A, View as V};
    &[
        bind(A, CTRL, Key::K, "Ctrl+K", Palette),
        bind(A, CTRL_SHIFT, Key::P, "Ctrl+Shift+P", Palette),
        bind(A, CTRL, Key::N, "Ctrl+N", New),
        bind(A, CTRL, Key::O, "Ctrl+O", Open),
        bind(A, CTRL_SHIFT, Key::S, "Ctrl+Shift+S", SaveAll),
        bind(A, CTRL_SHIFT, Key::E, "Ctrl+Shift+E", ExportHtml),
        bind(A, CTRL, Key::S, "Ctrl+S", Save),
        bind(A, CTRL, Key::W, "Ctrl+W", CloseTab),
        bind(A, CTRL, Key::F4, "Ctrl+F4", CloseTab),
        REPLACE,
        bind(A, CTRL, Key::F, "Ctrl+F", Find),
        bind(A, CTRL, Key::G, "Ctrl+G", GotoLine),
        bind(A, CTRL_SHIFT, Key::Tab, "Ctrl+Shift+Tab", PrevTab),
        bind(A, CTRL, Key::Tab, "Ctrl+Tab", NextTab),
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
        bind(V, CTRL, Key::F2, "Ctrl+F2", ToggleBookmark),
        bind(V, CTRL, Key::CloseBracket, "Ctrl+]", Indent(false)),
        bind(V, CTRL, Key::OpenBracket, "Ctrl+[", Indent(true)),
        bind(V, SHIFT, Key::F4, "Shift+F4", PrevBookmark),
        bind(V, NONE, Key::F4, "F4", NextBookmark),
    ]
};

/// Shortcut of Replace, as the Help & Shortcuts dialog writes it.
pub const REPLACE_KEY: &str = REPLACE.written;

/// Commands offered by the command palette.
pub const PALETTE: &[Cmd] = {
    use Cmd::*;
    &[
        New,
        Open,
        OpenConflict,
        OpenProject,
        SaveProject,
        Save,
        SaveAll,
        Patch,
        ApplyPatch,
        ExportHtml,
        Report,
        ReportWithDiffs,
        SaveComparison,
        GitCompare,
        Statistics,
        Indent(false),
        Indent(true),
        BookmarkComment,
        SelectPattern,
        ToggleBookmark,
        NextBookmark,
        PrevBookmark,
        ClearBookmarks,
        TakeBoth(true),
        TakeBoth(false),
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
                    && later.key.modifiers.alt == b.key.modifiers.alt;
                assert!(!shifted, "{} must come before {}", later.written, b.written);
            }
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
