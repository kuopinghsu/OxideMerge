# Changelog

All notable changes to OxideMerge are listed here, newest first. Each release on GitHub uses its
section of this file as the release notes.

## [0.9.5] - 2026-10-29

### Added

- **Synchronization points** (*Merge → Set synchronization point*, Alt+S, or right-click a
  line): line up lines the compare pairs wrongly, as in WinMerge. The text before and after each
  point is compared separately. Points are marked in the gutter, follow edits and are kept in
  saved comparisons.
- **XML reports** of text and folder compares, for other programs to read: choose the `.xml`
  type in *Generate report...*, or `--report <name>.xml`. Text reports list every difference
  with its lines and the bookmarks; folder reports list every item with its status, dates and
  sizes. The layout is described in the user guide.
- **HTML slideshow reports** (*File → Export → Generate slideshow report...*, or `--report
  <name>.html --slideshow`): one difference per slide, with a title slide, keyboard navigation,
  full screen, a link for each slide and one slide per printed page, as Araxis Merge makes them.
  Folder slideshows cover every changed text file.
- **Web page compare, full page:** a new *Full page* mode captures each whole page, top to bottom,
  instead of only the first screen (up to 16384 pixels tall).
- **Web page compare, highlighted differences:** the screenshot modes compare the pages' text and
  highlight what differs on the pages themselves (changed blocks in yellow, changed words in
  orange), like WinMerge's *View Differences*, so a change that moves the page down no longer
  marks everything below it. *Highlight differences* in the toolbar turns this off.

### Changed

- **Web page compare** drives the browser over the DevTools protocol, with all pages loading at
  the same time in one browser. Text and HTML source now come from the page as the browser shows
  it, after its scripts have run, when a browser is found (*Render pages in the browser for text
  and HTML* in the options). Text puts each heading, paragraph, list item, table cell and form
  value on its own line and leaves out hidden elements. Text is the default mode.
- **Menus:** every pull-down menu opens equally wide: the menu bar's menus, their submenus, the
  right-click menus and the folder compare's drop-downs (the widest menu of the menu bar, its
  submenus included, sets the width, at least 220 points). Options that turn on or off and choices
  of one show a check mark in a column of their own, so labels line up and an option that is off
  no longer looks disabled. Shortcuts keep clear of their labels. The tab's right-click menu shows
  the Close shortcut, and a file's right-click *Compare as* offers Web page like the Compare menu.
- **Web page compare:** when the browser fails, Text and HTML source fall back to the pages as
  downloaded and the status bar says why, instead of showing only an error. The console compare
  (`--cli -t web`) also renders the pages in the browser, so it matches the app. On macOS and
  Linux the browser is driven over a private pipe instead of a local port. Tests that need a real
  browser run only when `OXIDEMERGE_TEST_BROWSER` is set; CI sets it on the Windows and Linux
  x64 runners.
- **Release files:** a release now holds nine files: a `.zip` per Windows architecture holding
  only `oxidemerge.exe`, an AppImage and a `.tar.gz` per Linux architecture, the macOS `.dmg`,
  and the two PDF user guides. The separate Windows `.exe` downloads, the Linux `.deb`
  packages, the macOS `.zip` and `SHA256SUMS.txt` are no longer published; GitHub lists the
  SHA-256 digest of each file on the release page. The release fails if any file is missing or
  an unexpected one is present.

### Fixed

- **UTF-8 patches:** inserting or deleting lines at the start of a file with a byte order
  mark now keeps exactly one mark at the file's start, including when ignore rules match
  the line that moves across that boundary.

- **Windows CI:** the Git revision CLI test explicitly checks LF and CRLF checkout modes
  instead of assuming LF output, and isolates the subprocess Git configuration from the runner.

- **Mixed line endings:** automatic merge combines independent terminator edits on aligned
  lines, including around insertions and deletions, and keeps the merged endings on save.
- **Patch ignore rules:** anchored filters, substitutions and ignored blocks are evaluated
  against decoded text so BOMs and CRLF bytes cannot change which edits are exported.

- **Text formats and merging:** comparisons account for encodings, BOMs, line endings and
  final newlines. Copy All transfers selected formats across multiple differences, independent
  text and format edits merge together, and saving refreshes format differences.
- **Patches:** UTF-8 patches preserve BOMs, LF/CRLF endings and missing final newlines, including
  changes that move the last line. Unsupported encodings and lone-CR endings report an error.
- **Projects:** saved projects retain pane descriptions and read-only flags and use relative
  input paths where possible. The guides clarify save-shortcut scope, and the security document
  explains trusted Git filters and their possible command execution and network access.

- **Four or more paths:** opening four or more files or folders, from the command line, a drop
  or a file manager, no longer crashes the folder compare; it says that two or three paths are
  needed.
- **File safety:** saving and copying now report an error if a temporary file cannot be created
  or cannot replace the destination. They no longer fall back to overwriting the original file
  directly, which could truncate it if a subsequent write failed.

## [0.9.4] - 2026-10-27

### Added

- **Tests:** randomized diff, 3-way merge and patch tests that check every result against the
  inputs (`OXIDEMERGE_TEST_CASES` and `OXIDEMERGE_TEST_SEED` for longer runs or to repeat one),
  tests that draw the views without a window, and command line tests of the built program
  (`tests/cli.rs`). `OXIDEMERGE_CONFIG_DIR` points the program at another settings folder, window
  size and place included, so tests never touch the real one.
- **Window smoke test** (`make gui-smoke`, `scripts/gui-smoke.sh`): opens real windows with the
  release build, two files, three files, two folders, and checks what each tab shows. CI runs it on
  Windows (with Mesa's software renderer), Linux (Xvfb) and macOS and keeps the output of a failed
  test.
- **Benchmarks** of text, 3-way, binary, image and folder compares, timed with their peak memory
  (`OXIDEMERGE_BENCH_*`); `docs/benchmarks.md` has the method and a first baseline, and a CI run
  started by hand can run them on every platform.
- **Dependency checks** with `cargo deny` (`deny.toml`) for known vulnerabilities, licenses and
  sources, in CI and before every release, and `SECURITY.md` on reporting vulnerabilities and on
  which inputs OxideMerge treats as untrusted.
- **CI checks the packages:** the Windows `.zip` must hold the program, README and license; the
  Linux `.deb` is installed, run and removed, leaving the user's settings alone; the macOS app must
  hold both architectures. Before publishing, the release job checks that every package is there
  and that the version was not released before, and the release notes name the commit built.
- **Documentation:** a Quick start in the READMEs, a Compare types section and a longer
  Troubleshooting and limitations chapter in the user guides, and `docs/macos-signing.md` on
  Gatekeeper and notarization.

### Changed

- **Folder auto merge** checks each item again just before it changes it. Files are replaced
  through a temporary file, so a crash or a full disk leaves them whole, and are kept as `.bak`
  with backups on. Nothing is changed when the folders changed since they were compared. A file
  and a folder of the same name, symbolic links, special files and items the compare could not
  read are conflicts left alone; links are never followed, and names that would lead outside the
  middle folder are refused. A step that fails is reported and the others still run. Copies in
  the folder compare are made through a temporary file too, and keep the file's modification
  time.
- Reports, patches and saved comparisons are written through a temporary file, so a failed export
  never leaves half a file behind. Devices such as `/dev/stdout` are written directly.
- **Web page compare:** a web page may not load local files, and only a page on this computer or a
  private network may load resources from one or be redirected to one; such resources are skipped
  and counted. A page counts as local only when every address of its host is; IPv4 addresses
  inside IPv6 ones (mapped, NAT64, 6to4) are judged by the address they hold. Redirects are
  followed by OxideMerge itself, at most 10; a redirect without a target is shown as it is, and
  one to anything but a web address says so. A byte order mark now
  decides the encoding before the HTTP header and the `<meta>` charset.
- Language files in a `Languages` folder are checked: messages with other placeholders than the
  English ones, unknown keys and non-text values are left out, and English is shown instead.
- The menus of the menu bar all open as wide as the widest one, in every language.
- macOS instructions describe **Open Anyway** in System Settings, as Control-click → Open no longer
  bypasses Gatekeeper from macOS 15 on.

### Fixed

- Applying a patch made without context lines (`diff -U0`) put added lines one line too early,
  and in the wrong place when hunks before them were left out of the patch. A hunk found away
  from its stated line now moves the hunks after it by as much, as GNU patch does, so a line
  added after it no longer lands too early.
- A difference next to a moved insertion or deletion could point at the wrong line of the side
  where it is empty.
- A folder compare that broke off with an internal error showed the folders as identical; it now
  says it failed and counts as an error.
- A text compare whose syntax highlighting stopped with an error kept redrawing the window.
- Settings that could not be saved were dropped without a word; the error is now shown once,
  and saving is tried again until it works and when the program closes.
- Settings that could not be read were replaced by the defaults when settings were next saved.
  The file is now kept as `config.json.bad` (or `oxidemerge.json.bad`) and a message says so; a
  settings file that can't be opened at all is not saved over. An empty file still holds the
  defaults, as for portable use.
- With the Recycle Bin option on, a file or folder that could not be moved to the trash was
  deleted for good without a word; it is now left where it is and the error is shown.
- Folder auto merge could miss a file that changed while the merge was being planned.
- On Windows, copying a file over a read-only one in the folder compare or folder auto merge
  wrote into it in place; it is now replaced through a temporary file and stays whole if the
  copy fails.
- The Filters window opened folders with `xdg-open` on macOS; errors opening files and folders
  from the folder compare and the dialogs are now shown.
- On Windows, *Show in file manager* did not select files whose path has a space or a comma.
- Changing a file's line ending in a text compare could not be undone, and undoing an earlier
  change cleared the mark that the file was modified, so closing it lost the new line ending.
- Folder auto merge merged binary files as text, and dropped a change of encoding, byte order
  mark, line endings or final newline made on one side. Only text files that read back to the
  same bytes are merged now, the change of either side is kept, and different changes on both
  sides are a conflict. A side left without line breaks no longer counts as changing the line
  ending, and the encoding and byte order mark are merged together, so UTF-16 never loses its
  byte order mark.
- Folder auto merge with backups on could replace a compared file with another file's `.bak`;
  such a step is now left alone. A folder added on both sides is now made in the middle folder,
  even when it is empty, and its contents are merged item by item.
- A read-only `.bak` left from before stopped the next backup of that file. On Linux and macOS,
  a read-only file written in place is made writable for its owner only, and read-only again
  when the write fails.
- Files kept under `/dev/`, such as `/dev/shm`, were written in place and without a backup, like
  devices.
- Resources of a web page saved under one name were numbered `a~2`, `a~2~3`, ... until the name
  got too long; they are now `a~2`, `a~3`, ...
- On Windows, saving over a read-only file failed and left a temporary file next to it; the file
  is now replaced and stays read-only, as on Linux and macOS.
- Text copied into a new or empty file in a text compare got this computer's line endings; the
  file now takes the encoding and line endings of the file copied from, as in All differences.
  Take both and Auto merge into an empty middle file do the same, and undo puts back only what
  copying changed.

### Security

- Unpacker commands could run part of a file name as a command; the file path is now passed as an
  argument (`"$1"`) on Linux and macOS, and as an environment variable the command line quotes on
  Windows, where unpackers given the quoted path in the command line also failed to run. Quotes
  written around `${file}` are dropped, as they would leave the path unquoted.
- On Windows, *Open with* and opening a file from the folder compare went through `cmd start`, so
  a file name containing `&` could run a command; files are now opened by the shell directly.
- Resource tree downloads could write outside the chosen folder through an encoded `/` (`%2F`) in
  a resource address.
- The web size limit counted compressed bytes, so a small compressed response could unpack to
  fill memory; it now counts the bytes unpacked. A page of deeply nested `<head>` tags, or with a
  title tag left open in `<head>`, crashed the program.
- On Windows, a local page in a resource tree compare could load `file://host/share` resources,
  which opens a connection to that computer and hands it the user's credentials; files of other
  computers are now skipped.
- Archives that unpack to more than 8 GB or 500,000 files are refused, so a small archive can no
  longer fill the disk; `.tar.xz` archives are no longer unpacked into memory.
- Links in `.tar` archives that point outside the archive, directly or through other links, are
  no longer extracted.
- Web pages can no longer read local files or reach this computer and the private network
  through their resources or redirects (see Changed).

## [0.9.3] - 2026-10-26

### Added

- A user guide in AsciiDoc, in English (`docs/user-guide.adoc`) and Traditional Chinese
  (`docs/user-guide.zh-TW.adoc`), with screenshots of each view in both languages, linked from the
  READMEs. CI builds both as PDF files, attached to each GitHub release; `make user-guide` builds
  them locally.
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
  once, also when the window is minimized or the app hidden and when quitting from the Dock, at
  logout or from a script.
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
- A file with one difference says "1 difference" in All differences and in HTML reports.
- Three-way reports colour only the sides that changed. The CSV report starts with a byte order
  mark so spreadsheets read it as UTF-8, and keeps names like `-2+3+cmd|...` as text.
- Saving no longer empties a file when the disk is full or the disk quota is used up: a save that
  fails leaves the file as it was, and no empty file when it was new. The options are saved the
  same way.
- Compare options given on the command line (`--ignore-case`, `--lang`, ...) apply to that run
  only, instead of becoming the saved options the next time an option changed. Each one is kept
  only if it was changed in the program afterwards.
- Patches leave out the differences the compare options ignore, as `diff -B` does, except inside
  a hunk with other changes, where leaving them out would make the patch drop lines. The console
  compare of three binary files reads the third one, and its byte ranges end at the last byte that
  differs.
- Reports and patches of a folder compare wait until it has finished, and quitting waits for a
  report still being written. Report file names get `.html` when typed without it.
- Adding a rule to a typed name with a folder in it (`Team/Logs`) saves it in the rules folder
  (as `Team_Logs`) instead of a file without extension in the current folder. Importing a rules
  file named like one next to the program adds it under a free name instead of hiding that one,
  and a rules file dropped on the window updates the open compares. Renaming or deleting a rules
  file updates the compares using that very file, telling names apart by case where file names
  keep it. A project opened by a relative path reads its rules file from the project's folder.
  The Compare rules menus tick a rules file used by its path only when it is the one listed, not
  another file of the same name.
- The rules editor keeps the file of a rules file whose name is not a usual file name while the
  name is unchanged, renames it back when saving after a change of case fails, warns again about
  unsaved edits after further typing, and checks the patterns that will be saved.
- All differences reads files only as fast as the tab shows them and also lets go of their rows
  far out of view, keeping memory down; reads files changed on disk in the background, a few at a
  time; no longer replaces saved text with a reading made before the save; takes the line endings
  along with a whole-file copy; counts files saved rather than sides; says when files are still
  being compared, when an undo can't be applied, and drops the "stopped" note once the folder
  compare has finished.
- Closing several tabs and then quitting no longer goes back to asking about the other tabs, and
  files opened from an archive compare keep its extracted copy after the folder compare closes.
- The console compare exits with 2 when files could not be compared, and folders left uncompared
  (not recursive) no longer count as differences. `--numeric-tolerance` refuses NaN and infinity,
  and options that are ignored (`--patch` with three paths, `--report-diffs` without `--report` or
  for files, a text report not named `.html`) say so.
- Shortcuts of commands that do nothing in the current tab do nothing (`Ctrl+Shift+Z` no longer
  runs Undo where there is no Redo), F5 refreshes web pages shown as screenshots, the
  Filters window no longer turns the shortcuts off, files dropped while a dialog is open are
  ignored, and Enter in the command palette runs the command matching what was just typed.
- A text compare whose file changed on disk while one of its lines was being edited no longer
  says it reloaded it, and a file that could not be read when it changed is tried again instead of
  being reported as reloaded.
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

[0.9.5]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kuopinghsu/OxideMerge/releases/tag/v0.9.0
