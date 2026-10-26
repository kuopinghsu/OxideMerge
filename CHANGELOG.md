# Changelog

All notable changes to OxideMerge are listed here, newest first. Each release on GitHub uses its
section of this file as the release notes.

## [0.9.3] - Unreleased

### Added

- **All differences** (folder toolbar, Compare menu or command palette): every changed text file
  of a folder compare in one tab, with its differences side by side as in the HTML report, a file
  list with search, expand and collapse all, next and previous difference across files, copying a
  difference (or, with two sides, a whole file) to another side, undo and save (the folder compare
  then refreshes). The files are those the folder compare shows, its filters applied. Copying from
  a side where the file is missing deletes it when saved, with folders left empty (with backups
  on, the file is kept as `.bak`), a file changed on disk since it was read is never saved over
  and its card can discard the changes to read it again, text compares of saved files notice the
  change, opening the tab again lists files with differences found since, and the text of files
  far out of view is let go and read again when needed, keeping memory use down.
- **Numeric tolerance** (Options → Compare, or `--numeric-tolerance`): numbers at most a set amount
  apart count as equal, in text and folder compares.
- **Block filters** in compare rules files: ignore everything between a start line and an end line
  matching regular expressions.
- **Compare rules files**: lines to ignore, substitutions and blocks to ignore are kept in
  `.oxrules` files instead of the options. Options → Compare rules picks the default file and edits,
  creates or deletes them. A folder compare picks its own file from the toolbar, and Refresh (F5)
  reads it again. The context menu of a text compare adds a rule from the clicked or selected
  lines, and Compare → Compare rules switches the file of the active compare. Rules files are
  exported (one, or all to a folder, never replacing files already there) and imported in
  Options → Compare rules, or imported by dropping one `.oxrules` file on the window (or several on
  the options; two dropped on the window are compared). Renaming or deleting a rules file updates
  the compares using it, and restored session tabs keep their own file. A selected file that
  can't be read stays selected, with a message, and compares run without it. Projects and the
  `--rules` option name a file too (`--rules ""` for none). Rules set in the options of earlier versions move to a
  "Default" file.
- **Line detail pane** (View → Text compare): the current line of every side, one above the other and
  wrapped.
- **Edit markers** in the gutter for lines changed since the files were opened.
- **Statistics** (Tools menu): line counts per pair of sides for text compares, and file counts and
  sizes for folder compares.
- **Indent and unindent** the selected lines (`Ctrl+]` / `Ctrl+[`).
- **Bookmark comments**, shown when hovering the gutter, in the status bar and in HTML reports.
- **Save comparison** (File menu): every side's text with its bookmarks and comments in one
  `.oxmcmp` file, which opens again read-only.
- **Compare with a Git revision** (File menu, or `--git-rev`) for files and folders. The revision
  side is read-only and has the files as a checkout would, including Git LFS files and
  submodules.
- **File compare below the folder list** (View → Folder list): the selected file's differences without
  opening a tab.
- **Folder report with file differences** (File menu, or `--report-diffs`).
- **New HTML reports** for text and folder compares: one self-contained page listing every
  difference as a card with line numbers, context and highlighted words. Summary cards, an index of
  files and differences, side-by-side or unified view, light and dark themes, status filters,
  search, and `N` / `P` to jump between differences. Reports name the compare rules file used
  and count the differences it ignored. Folder reports with file differences list those files
  first, skip binary files and files over 16 MB, and cut lines after 2,000 characters; text
  reports stop after 20,000 rows and count the differences left out. Reports are made in the
  background. Printing opens every file and keeps the colors.
- **Export HTML** button on the toolbar of text and folder compares, also in the File menu and the
  command palette (`Ctrl+Shift+E`): saves the HTML report, with the differences of every changed
  file for a folder compare, under a name taken from the compared files (spaces written as `_`),
  and opens it in the browser (File → Export → Open reports in the browser after saving). The file
  compare docked below a folder compare exports its own report.
- **Copy to folder** and **select by name** in folder compares.
- **3-way folder auto merge** into the middle folder, merging text files changed on both sides when
  the changes don't overlap. Files hidden by the filter, and files changed after the merge was
  planned, are left alone.
- **Arm64 packages** for Windows (`.exe`, `.zip`) and Linux (AppImage, `.deb`, `.tar.gz`),
  built and tested on Arm machines, next to the x64 and macOS universal packages.

### Changed

- The sides of an archive compare are read-only: they are temporary copies, and changes to them
  were lost.
- **Menus regrouped** by what they act on. A new Compare menu holds stepping through
  differences, Compare as, Compare rules, the compare options and Refresh. File gains Open recent
  and Export (HTML report, reports, patch) submenus and takes Apply patch from Tools. Edit groups
  the bookmark commands in a submenu. View has the Folder list and Text compare submenus, Zoom,
  Theme, and next/previous tab. Commands that do nothing in the current tab are greyed out
  instead of hidden or silently ignored.
- Shortcuts in menus, the command palette and tooltips come from the same table as the keys
  themselves, and on macOS are written the macOS way (`⇧⌘S`, `⌥↓`, `⌘Q` for Exit) everywhere.
- Next and previous tab are `⌃Tab` and `⌃⇧Tab` on macOS, where `⌘Tab` switches applications.
- A key held with Alt no longer also runs the command of the key alone (`Alt+F4` no longer goes to
  the next bookmark first), and the command palette lists only what works in the current tab.

### Fixed

- `⌘Q` on macOS asks about unsaved changes like closing the window does, instead of quitting at
  once, also when the window is minimized and when quitting from the Dock, at logout or from a
  script.
- Saving text in a legacy encoding (Big5, Shift_JIS, ...) that can't hold some of its characters
  is refused with a message, instead of writing them as `&#...;` entities.
- A relative path given to `--rules` or in a project file is read from where it was given (the
  current folder, or the project's folder), and a missing rules file is reported.
- Rules files saved with a byte order mark are listed; a name ending in `.oxrules` is not doubled,
  names differing only in case (where file names keep case) are both listed, and names compare
  without case beyond ASCII. The rules editor shows the name a file will be saved under, takes
  `a =>` without a space after the arrow, marks lines with an empty side, refuses to save while
  lines are unclear, and asks before Cancel or closing the options throws edits away.
- Adding a rule to a rules file given by path that can't be read no longer writes another file.
- Close all, Close others and Close tabs to the right ask about each tab with unsaved changes in
  turn, not only the first; Esc cancels the close and exit prompts.
- New files are created with the usual permissions instead of readable only by their owner.
- Folder reports with file differences share one size limit across files, like text reports.
- Reports no longer name a compare rules file that could not be read, as the compare ran without
  it.
- Saving in All differences says which files had mixed line endings written as one kind, and a
  text compare stops showing "mixed" for a file once it is saved.
- Three-way reports colour only the sides that changed. The CSV report starts with a byte order
  mark so spreadsheets read it as UTF-8.
- The console compare exits with 2 when files could not be compared, and folders left uncompared
  (not recursive) no longer count as differences. `--numeric-tolerance` refuses NaN and infinity,
  and options that are ignored (`--patch` with three paths, `--report-diffs` without `--report` or
  for files, a text report not named `.html`) say so.
- Shortcuts of commands that do nothing in the current tab are left to the focused field, the
  Filters window no longer turns the shortcuts off, files dropped while a dialog is open are
  ignored, and Enter in the command palette runs the command matching what was just typed.
- A text compare whose file changed on disk while one of its lines was being edited no longer
  says it reloaded it.
- `Ctrl+Tab` no longer leaves the keyboard focus on a button, which turned off the arrow keys and
  the other keys of the compare until it was clicked.
- `Ctrl+S`, closing a tab and quitting take in a line still being typed in place, instead of
  saving without it or closing without asking.
- Closing a tab left of the current one no longer switches to another tab.
- Keys no longer reach the compare behind the Add compare rule dialog.
- Copying a file or folder onto itself (both sides pointing at the same place) no longer empties
  the file; copying a folder into itself, or through a link back to itself, is refused.

## [0.9.2] - 2026-10-24

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

[0.9.3]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.2...HEAD
[0.9.2]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kuopinghsu/OxideMerge/releases/tag/v0.9.0
