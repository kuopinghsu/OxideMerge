# Changelog

All notable changes to OxideMerge are listed here, newest first. Each release on GitHub uses its
section of this file as the release notes.

## [0.9.2] - Unreleased

### Added

- **Command palette** (`Ctrl+K` or `Ctrl+Shift+P`): search commands, recent comparisons and
  settings in one place.
- **Find and replace** in text compares (`Ctrl+H`, `Cmd+Alt+F` on macOS), with match case, whole
  word and regular expression options. Replace all can be undone in one step.
- **Line selection** in text compares by dragging, Shift+click or Shift+arrow keys. `Ctrl+C` copies
  the selected lines, and `Alt+Right` / `Alt+Left` copies them to the other side.
- **Collapse unchanged lines** (`Alt+C`) to a clickable bar, keeping three lines of context around
  each difference, and **word wrap** (`Alt+Z`).
- **Merge arrows** on the divider between panes copy a difference in one click. The toolbar shows
  how many lines were added and removed.
- **Next file with differences:** in a file opened from a folder compare, `Alt+Down` past the last
  difference (or `Alt+Up` past the first) opens the next (or previous) file that differs, in the
  same tab.
- **Quick preview** in folder compares: press `Space` to see a colored diff of the selected file
  without opening a tab.
- **Find by name** in folder compares, with clear messages when nothing matches or every item is
  hidden by the View settings.
- **Results while comparing:** folder compares list differences and unique items as soon as they
  are found.
- A notification when a long comparison finishes while the window is in the background.
- **Binary compare:** find hex bytes (`DE AD BE EF`) or text, and go to an offset (`Ctrl+G`).
- **Image compare:** swipe mode with a draggable divider, and blink mode that alternates the
  images. Pinch to zoom and two-finger scrolling to pan.
- **Optional backup on save:** keep the previous contents as `<name>.bak` (Options → General).
- **Theme setting** with Light, Dark and Match system, and a search box in Options.
- **Reopen previous comparisons at startup** (Options → General).
- Recent comparisons on the start page can be edited or removed from their context menu.
- Dropping files on the window shows what will happen; dropping onto the Open dialog fills in the
  empty paths.
- **Windows Explorer context menu in the style of WinMerge:** an OxideMerge submenu with Compare,
  Select Left, Select Middle and Compare As, also available from the command line as
  `--select-left`, `--select-middle` and `--compare-to`.
- **Finder Quick Actions and Linux file manager entries:** Tools → Add Finder Quick Actions on
  macOS, or Add to file manager context menu on Linux (Nautilus, Nemo and Dolphin), offers
  Compare, Select Left and Select Middle for the selected items.
- **New text compare** (`Ctrl+N`): two empty sides to paste (`Ctrl+V`) or type text into, saved
  wherever you choose.
- **Paste** (`Ctrl+V`) in text compares inserts the clipboard below the current line, or replaces
  the selected lines.
- **Bookmarks** in text compares: `Ctrl+F2` sets or removes one, `F4` / `Shift+F4` jump between
  them. They are marked in the gutter and the location pane, and move with their lines as you edit.
- **Take both sides** in 3-way merges: Merge → Take left, then right (or right, then left) puts both
  changes into the result.
- **Ignore comment changes** (Options → Compare): differences only in comments don't count, for
  C-like languages, scripts, SQL, HTML/XML, CSS, Lua, Haskell and more. Also in folder compares and
  as `--ignore-comments`.
- **Format JSON and XML before comparing** (Options → Compare): files are shown re-indented, with
  JSON keys sorted, so only real content changes remain. Also in folder compares and as
  `--prettify`.
- **Apply patch** (Tools → Apply patch): applies a unified diff (plain, `git diff` or `svn diff`)
  even when lines have moved, and opens each patched file next to its original for review before
  saving. Hunks that don't fit are reported.
- **HTML report for text compares:** File → Generate report saves a side-by-side colored report
  with word differences, also from the command line with `--report`.
- **Project files:** File → Save as project stores what a tab compares (paths, filter, subfolders,
  compare method) in a `.oxmproj` file; File → Open project, the command line and drag and drop open
  it again. WinMerge `.WinMerge` projects open too.
- **More archive formats compared as folders:** `.tar`, `.tar.gz` / `.tgz`, `.tar.bz2` / `.tbz2`,
  `.tar.xz` / `.txz` and `.7z`, besides zip-based formats.
- **Git integration** instructions for using OxideMerge as `git difftool` and `git mergetool`.
- `make appimage` builds the Linux AppImage locally.
- Traditional Chinese README (`README_zh-TW.md`).

### Changed

- Large text files with many differences compare up to a hundred times faster, and no comparison
  takes more than a few seconds.
- Folder compares read large files in chunks, so identical and binary files are never loaded
  whole, and files of different sizes are told apart without reading them.
- Saving is crash-safe: a crash or a full disk never leaves a half-written file. File permissions
  and symbolic links are kept.
- The Language menu was removed from the menu bar; choose the language in Options → General.
- The pattern and command fields in Options → Plugins are wider.

### Fixed

- When saving a merge result to another file with `-o` (as `git mergetool` does), the result is
  now always written there, even if it equals one of the inputs.
- The application icon no longer shows a white fringe on dark backgrounds.

## [0.9.1] - 2026-10-23

### Added

- **Web page compare** of two or three URLs or local HTML files, in four modes: Screenshot
  (rendered with Edge, Chrome, Chromium or Brave), HTML source, visible text, and the tree of
  downloaded resources.
- **Help & Shortcuts** dialog (`F1` or `?`) listing every keyboard shortcut and mouse action, and
  an About dialog with credits.
- Files changed by another program are detected when the window regains focus: unmodified files
  reload automatically, and files with unsaved edits offer to reload.
- Keyboard selection in folder compares: arrow keys, Page Up/Down, Home/End, Shift to extend and
  `Ctrl+A` to select all.
- Save all (`Ctrl+Shift+S`), redo with `Ctrl+Shift+Z`, `Esc` to close the find bar, and a tab
  context menu to close other tabs, tabs to the right, or all tabs.
- **Release packages:** Windows `.exe` and `.zip`; Linux AppImage, `.deb` and `.tar.gz`; a
  universal macOS app (Apple silicon and Intel) in a `.dmg` and a `.zip`; and `SHA256SUMS.txt`.
- The Windows executable has an icon and version details.

### Changed

- Linux file dialogs use the XDG desktop portal, and building on Linux no longer needs
  development packages.

## [0.9.0] - 2026-10-22

First release.

- **Folder compare and merge:** 2-way and 3-way, recursive or flat, as a tree or a flat list. Six
  compare methods, file filters (wildcard masks and WinMerge `.flt` files) with a Clear filter
  option, zip-based archives compared as folders, and HTML, CSV and text reports.
- **Text compare and merge:** 2-way and 3-way with Myers, Patience or LCS, inline word and character
  differences, moved blocks, a location pane, WinMerge-style ignore options, copy and auto merge,
  undo and redo, in-place editing, conflict file resolution, syntax highlighting, find, go to line,
  CSV/TSV table mode, patches, and unpacker plugins.
- **Binary compare** in a side-by-side hex view.
- **Image compare** side by side, as an overlay or as a difference image, with highlighted
  difference blocks.
- Tabs, a start page with recent comparisons, drag and drop, light and dark themes, 10 languages,
  and the Windows Explorer context menu.
- Command-line compares, reports and patches without opening a window, and WinMerge-style switches
  on Windows.
- Chinese, Japanese and Korean fonts on Linux and WSL, and GPU acceleration under WSL2.

[0.9.2]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.1...HEAD
[0.9.1]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kuopinghsu/OxideMerge/releases/tag/v0.9.0
