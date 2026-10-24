# OxideMerge

English | [繁體中文](README_zh-TW.md)

![OxideMerge - the cross-platform diff and merge tool](images/banner.jpg)

OxideMerge is an open source, cross-platform differencing and merging tool written in Rust. It
brings the feature set of [WinMerge](https://winmerge.org/)
([source](https://github.com/WinMerge/winmerge)) to **Windows, macOS and Linux**. It compares and
merges folders, text files, binary files and images, and compares web pages, with a modern
multilingual UI.

The name comes from the language it is written in: rust is iron oxide.

![Start page](images/start.png)

| Folder compare | Text compare |
| --- | --- |
| ![Folder compare](images/folder.png) | ![Text compare](images/text.png) |

## Features

**Folder compare and merge**
- 2-way and 3-way folder comparison, recursive or flat, shown as a tree or a flat list.
- Results appear while the comparison runs: files are compared during the folder scan, and
  differences and unique items are listed as soon as they are found.
- Large files are compared in chunks, so identical and binary files are never loaded whole;
  files of different sizes are told apart without reading them in the quick and binary methods.
- Compare methods: full contents, quick contents, binary contents, modified date, date and size, size only.
- Show or hide identical, different and unique items. Copy items between sides or delete them,
  with optional Recycle Bin support and confirmation prompts.
- Copy the selected items of any side to another folder, keeping their relative paths, and select
  items by name with wildcards (`*.c; *.h`).
- File compare below the folder list (View menu): the selected file's differences are shown right
  under the list, without opening a tab.
- 3-way auto merge of folders into the middle (common ancestor) folder: changes made on one side
  are copied, removals are applied, and text files changed on both sides are merged line by line
  when the changes don't overlap. Conflicts are listed and left alone.
- Keyboard selection: arrow keys, Page Up/Down, Home/End, Shift to extend, Ctrl+A to select all.
- Quick preview: press Space to see a colored diff of the selected file without opening a tab.
- A long comparison that finishes while the window is in the background asks for attention.
- File filters: wildcard masks such as `*.c;*.h;!*.bak;!.git\` and WinMerge `.flt` filter files.
- Archives are extracted and compared as folders: zip-based formats (`.zip`, `.jar`, `.apk`,
  `.docx`, `.xlsx`, `.odt`, `.epub`, `.nupkg`, ...), `.tar`, `.tar.gz`/`.tgz`,
  `.tar.bz2`/`.tbz2`, `.tar.xz`/`.txz` and `.7z`.
- Reports in HTML, CSV and plain text. An HTML report can also include the side-by-side
  differences of every changed text file (File → Generate report with file differences).
- Statistics (Tools menu): counts of identical, different and unique files, and total sizes.
- Project files (`.oxmproj`) save what a tab compares (paths, filter, subfolders and compare
  method) to open it again later; WinMerge `.WinMerge` projects open too.

**Text compare and merge**
- 2-way and 3-way comparison with Myers, Patience or LCS diff algorithms. Large files are first
  split at lines that occur once on each side, so even files with tens of thousands of
  differences compare in a fraction of a second.
- Inline (word and character) differences, moved-block detection and a location pane overview.
- Collapse unchanged lines to a clickable bar (three lines of context are kept around each
  difference), and word wrap for long lines.
- Merge arrows on the divider copy the hovered or current difference in one click; the toolbar
  shows how many lines were added and removed.
- WinMerge-style ignore options: whitespace (changes or all), case, blank lines, end-of-line style,
  numbers and code page.
- Numeric tolerance: numbers at most a set amount apart (for example `0.001`) count as equal, for
  comparing measurements and generated data.
- Block filters: ignore everything between a start line and an end line matching regular
  expressions, such as generated sections or license headers.
- Ignore comment changes, for languages whose comment syntax is known (C-like languages, scripts,
  SQL, HTML/XML, CSS, Lua, Haskell, ...), and format JSON and XML before comparing (re-indented,
  JSON keys sorted) so only real content changes remain. Both also apply to folder compares.
- File → New (Ctrl+N) opens two empty sides to paste text into (Ctrl+V) or type; Ctrl+V also
  inserts the clipboard below the current line, or replaces the selected lines, in any editable side.
- Bookmarks: Ctrl+F2 sets or removes one, F4 / Shift+F4 jump to the next / previous; they are marked
  in the gutter and the location pane. A bookmark can carry a comment (Edit → Bookmark comment),
  shown when hovering the gutter and in HTML reports.
- Save comparison (File menu): the text of every side with its bookmarks and comments in one
  `.oxmcmp` file, to review later or send to someone else.
- Compare with a Git revision (File menu, or `--git-rev`): a file or folder against how it was
  in `HEAD`, a branch, tag or commit.
- Line detail pane (View menu): the current line of every side, one above the other and wrapped,
  to spot small differences in long lines.
- Edit markers in the gutter show which lines were changed since the files were opened.
- Statistics (Tools menu): identical, changed, added, removed and ignored lines, per pair of sides.
- Indent or unindent the selected lines (Ctrl+] / Ctrl+[).
- 3-way merges can take both sides of a difference (left then right, or right then left).
- Copy differences left or right, copy all, auto merge for 3-way compares, undo and redo, and in-place editing.
- Select lines by dragging, Shift+click or Shift+arrow keys; copy them to the clipboard with
  Ctrl+C, or to the other side with Alt+Right / Alt+Left.
- Resolve version-control conflict files containing `<<<<<<<`, `|||||||`, `=======` and `>>>>>>>` markers.
- Encoding and end-of-line detection, syntax highlighting, go to line, and CSV/TSV table mode.
- Find and replace, with match case, whole word and regular expression options (`$1` in the
  replacement refers to a captured group); Replace all is a single undo step.
- Saving writes a temporary file and renames it over the original, so a crash or a full disk never
  leaves a half-written file; optionally the previous contents are kept as `<name>.bak`.
- Patch generation in normal, context and unified formats, and side-by-side HTML reports.
- Apply patch (Tools menu): applies a unified diff (plain, `git diff` or `svn diff`), finding each
  hunk even when lines have moved, and opens every patched file next to its original to review and
  save. Hunks that don't fit are reported.
- Files changed by another program are detected when the window regains focus: unmodified files
  reload automatically, and files with unsaved edits show a bar offering to reload them.
- Unpacker plugins: run an external command (for example `pandoc -t plain ${file}`) to convert files to text before comparing.

**Binary compare**
- Side-by-side hex view with byte-level differences, navigation and synchronized scrolling.
- Find hex bytes (`DE AD BE EF`) or text in any pane, and go to an offset (`0x1F00`, `1F00h` or decimal).

**Image compare**
- Side-by-side, overlay, swipe, blink and difference modes, with zoom, fit and 1:1 views. Swipe
  shows one image left of a draggable divider and the other right of it; blink alternates them.
- Mouse wheel or pinch to zoom at the pointer; drag or scroll with two fingers to pan.
- Difference blocks highlighted with an adjustable color threshold and block size.

**Web page compare**
- Compare two or three `http://`, `https://` URLs or local HTML files, in four modes:
  - **Screenshot**: renders each page with a headless Chromium-based browser (Edge, Chrome,
    Chromium or Brave, found automatically) and opens the images in the image compare.
  - **HTML source**: the page source, with long minified lines split between tags, in the text compare.
  - **Text**: the visible text of each page (scripts, styles and markup removed), in the text compare.
  - **Resource tree**: downloads each page with its stylesheets, scripts, images, frames and media,
    and compares the downloaded files in the folder compare.
- Pages are decoded using the HTTP or `<meta>` charset. Non-2xx HTTP status codes are shown in the pane header.

**Interface**
- Tabbed documents, a start page with recent comparisons, and drag and drop of files and folders.
- Light and dark themes.
- 10 built-in languages: English, 繁體中文, 简体中文, 日本語, 한국어, Deutsch, Français, Español,
  Русский and Português (Brasil). You can add more as JSON files (see [Languages](#languages)).
- Windows Explorer context menu in the style of WinMerge: compare the selected files or folders,
  or select a left (and middle) side first and compare to it later. The same entries are available
  as Finder Quick Actions on macOS and in Nautilus, Nemo and Dolphin on Linux.

## Platforms

| Platform | Notes |
| --- | --- |
| Windows 10/11 | Explorer context menu, WinMerge-style `/switch` arguments, Recycle Bin |
| macOS 11+ | Native file dialogs, Trash, "Reveal in Finder", Finder Quick Actions |
| Linux (X11 and Wayland) | File dialogs through the XDG desktop portal, freedesktop Trash, Nautilus / Nemo / Dolphin context menus |

The same code base builds on all three. Fonts come from the system: Segoe UI and Cascadia Mono on
Windows, SF and Menlo on macOS, and Noto, Inter or DejaVu on Linux. For Chinese, Japanese and
Korean text on Linux, any CJK font that fontconfig knows about is used (Noto Sans CJK or WenQuanYi
recommended); under WSL the Windows fonts are used when no Linux CJK font is installed.

Under WSL2 the app selects Mesa's `d3d12` driver (GPU acceleration through WSLg) when it is
installed and silences Mesa's EGL driver-probing warnings. Set `GALLIUM_DRIVER` or `EGL_LOG_LEVEL`
yourself to override either.

## Installing

Every [GitHub release](https://github.com/kuopinghsu/OxideMerge/releases) has these packages, and
`SHA256SUMS.txt` to check them against:

| Platform | Package |
| --- | --- |
| Windows x64 | `oxidemerge-<version>-windows-x64.exe` (runs as is) or `.zip` |
| Linux x64 | `.AppImage` (Ubuntu 20.04, RHEL 8, Debian 10 and newer; no install needed), `.deb` (Ubuntu 22.04 and newer), or `.tar.gz` |
| macOS 11+ (Apple silicon and Intel) | `oxidemerge-<version>-macos-universal.dmg`, or the same app in a `.zip` |

- **Linux:** run `chmod +x oxidemerge-*.AppImage` before starting the AppImage. Without FUSE, start
  it with `--appimage-extract-and-run`. To install the Debian package, run
  `sudo apt install ./oxidemerge-*.deb`. File dialogs need `xdg-desktop-portal`, which desktop
  environments normally include.
- **macOS:** the app is ad-hoc signed but not notarized. The first time, Control-click it and choose
  **Open**, or allow it in **System Settings → Privacy & Security**. To use it from the terminal, run
  `sudo ln -s /Applications/OxideMerge.app/Contents/MacOS/oxidemerge /usr/local/bin/oxidemerge`.

## Building

You need a Rust toolchain ([rustup](https://rustup.rs/)). On Linux, a C compiler is also needed.
OxideMerge loads the X11, Wayland, keyboard and OpenGL libraries at runtime, so it needs no
development packages to build.

Then build:

```sh
cargo build --release          # binary: target/release/oxidemerge(.exe)
cargo test
```

### Make targets

A `Makefile` for GNU Make wraps the common tasks. It works on Windows and Unix.

| Target | Description |
| --- | --- |
| `make` / `make release` | Optimized release build |
| `make build` | Debug build |
| `make run ARGS="a.txt b.txt"` | Run the debug build with arguments |
| `make demo` | Folder compare of `samples/left` and `samples/right` |
| `make demo-text` / `demo-3way` / `demo-conflict` | Text, 3-way and conflict-file demos |
| `make test` / `check` / `lint` | Unit tests, type check, clippy with warnings as errors |
| `make ci` | `check`, `lint` and `test` |
| `make check-platforms` | Type-check for Windows, macOS and Linux (needs `rustup target add` for each) |
| `make install` / `uninstall` | `cargo install` into `~/.cargo/bin` |
| `make shell-register` | Install, then add the file manager entries (Explorer, Finder Quick Actions, Nautilus, Nemo, Dolphin) |
| `make dist` | Package the release binary into `dist/` (`.zip` on Windows, `.tar.gz` elsewhere) |
| `make appimage` | Linux x86_64 AppImage in `dist/` (same layout as CI; needs the X11, Wayland and keyboard libraries installed, and downloads `appimagetool`) |
| `make clean` | Remove build output |
| `make help` | List all targets |

### Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every push to `main` and on every
pull request. On Windows, Linux and macOS it runs clippy with warnings treated as errors, runs the
unit tests, builds a release, and smoke-tests the command line (`--version`, `--list-languages` and
the `--cli` exit codes). It then uploads these packages as workflow artifacts:

- **Windows:** the `.exe` and a `.zip`.
- **Linux:** a `.tar.gz` and a `.deb`, built on Ubuntu 22.04.
- **Linux AppImage:** built on AlmaLinux 8 (glibc 2.28). It bundles the X11, Wayland and keyboard
  libraries, and [`assets/AppRun`](assets/AppRun) uses them only when the system lacks them. The
  AppImage is then checked on Ubuntu 22.04 and 24.04 and on AlmaLinux 8 and 9: every bundled
  library must resolve, the command line must work, and the window must stay open under Xvfb.
- **macOS:** a universal (Apple silicon and Intel) `OxideMerge.app`, ad-hoc signed, in a `.dmg` and
  a `.zip`.

Pushing a tag such as `v0.9.2` publishes all packages and `SHA256SUMS.txt` on a GitHub release. The
tag must match the version in `Cargo.toml`, and tags containing `-` (for example `v1.0.0-rc1`) are
marked as pre-releases. The release notes are the version's section of
[`CHANGELOG.md`](CHANGELOG.md), followed by download instructions, so add that section before
tagging; the build fails early if it is missing. `scripts/release-notes.sh 0.9.2` prints the notes
for a version. CI only reads `CHANGELOG.md`; it never edits it.

To make a release:

1. Set `version` in `Cargo.toml` and run `cargo build` so `Cargo.lock` picks it up.
2. In `CHANGELOG.md`, change the version's heading from `## [0.9.2] - Unreleased` to the release
   date, for example `## [0.9.2] - 2026-10-24`, and check its entries.
3. Run `make fmt-check ci` and preview the notes with `scripts/release-notes.sh 0.9.2`.
4. Commit and push to `main`, then wait for CI to pass on every platform. Tagging builds the commit
   the tag points at, so a commit that fails CI fails the release too.
5. Tag that commit and push the tag:

   ```sh
   git tag -a v0.9.2 -m "OxideMerge 0.9.2"
   git push origin v0.9.2
   ```

6. Start the next version: add a `## [0.9.3] - Unreleased` section at the top of `CHANGELOG.md`
   and record changes there as they land.

If a release build fails, fix it on `main`, then move the tag to the new commit and push it again
(`git tag -f -a v0.9.2 -m "OxideMerge 0.9.2"` and `git push -f origin v0.9.2`).

## Usage

Start `oxidemerge` without arguments to open the start page. From there you can open files or
folders, drag them onto the window, or pick a recent comparison.

```sh
oxidemerge left.txt right.txt                   # 2-way text compare
oxidemerge mine.txt base.txt theirs.txt         # 3-way compare (middle = base)
oxidemerge -r C:\project\v1 C:\project\v2       # recursive folder compare
oxidemerge -r -f "*.rs;!target/" old new        # folder compare with a filter
oxidemerge conflict.txt                         # resolve a file with conflict markers
oxidemerge -t image a.png b.png                 # force the compare type
oxidemerge https://example.com https://example.org   # web page compare
oxidemerge -t web old.html new.html             # compare local HTML files as web pages
oxidemerge left.txt right.txt -o merged.txt     # save the merge result to another file
oxidemerge release.oxmproj                      # open a saved project (or a .WinMerge project)
oxidemerge -r old.tar.gz new.tar.gz             # compare archives as folders
oxidemerge --git-rev HEAD~1 src/main.rs         # compare a file with an earlier Git revision
oxidemerge review.oxmcmp                        # open a saved comparison with its bookmarks
```

A web page compare tab has a mode bar (Screenshot, HTML source, Text, Resource tree) and a reload
button. The last mode used is remembered. Screenshot size and the browser path are set under
**Options → Image / Binary / Web**. Leave the browser path empty to detect it automatically.

### Command line options

| Option | Description |
| --- | --- |
| `PATHS...` | 2 or 3 files, folders or URLs, or a single conflict file, project file or saved comparison |
| `-r, --recursive` | Include subfolders in folder compares |
| `-f, --filter <MASK>` | File filter, for example `*.c;*.h` or `!*.bak` |
| `-m, --method <M>` | Folder compare method: `full`, `quick`, `binary`, `date`, `datesize`, `size` |
| `-t, --compare-as <T>` | Force `text`, `binary`, `image` or `web` |
| `-o, --output <PATH>` | Output path for the merge result |
| `--dl/--dm/--dr <TEXT>` | Descriptions shown instead of the left, middle and right paths |
| `--wl/--wm/--wr` | Open the left, middle or right side read-only |
| `--ignore-ws <MODE>` | Ignore whitespace: `compare`, `change` or `all` |
| `--ignore-case`, `--ignore-blank`, `--ignore-eol`, `--ignore-numbers` | Ignore options |
| `--ignore-comments` | Ignore changes in comments |
| `--prettify` | Reformat JSON and XML before comparing |
| `--numeric-tolerance <AMOUNT>` | Treat numbers at most this far apart as equal |
| `--git-rev <REV>` | Compare the single path given with how it was in a Git revision |
| `--report <FILE>` | Write a report: folder compares as `.html`, `.csv` or `.txt`, text compares as side-by-side `.html` |
| `--report-diffs` | Add the differences of every changed text file to an HTML folder report |
| `--patch <FILE>` | Write a patch for two files or folders |
| `--patch-style <S>` | `normal`, `context` or `unified` (default) |
| `--context <N>` | Context lines for patches (default 3) |
| `--cli` | Run without a window; exit code 0 = identical, 1 = different, 2 = error |
| `--lang <CODE>` | UI language, for example `ja` or `zh-TW` |
| `--list-languages` | Print the available languages |
| `--register-shell` / `--unregister-shell` | Add or remove the file manager entries (Explorer, Finder, Nautilus, Nemo, Dolphin) |
| `--select-left <PATH>` / `--select-middle <PATH>` | Remember a side for a later `--compare-to` |
| `--compare-to <PATH>` | Compare a path with the remembered left (and middle) side |

On Windows, WinMerge-style switches are also accepted, so existing scripts keep working:
`/r`, `/f`, `/o`, `/dl`, `/dr`, `/wl`, `/wr` and so on.

### Scripting

```sh
# Exit code only, no window
oxidemerge --cli -r dirA dirB && echo identical

# HTML report of a folder compare (--report and --patch never open a window)
oxidemerge -r dirA dirB --report report.html

# HTML folder report followed by the differences of every changed file
oxidemerge -r dirA dirB --report report.html --report-diffs

# Side-by-side HTML report of two files, ignoring comment changes
oxidemerge old.c new.c --ignore-comments --report diff.html

# Unified diff with 5 lines of context
oxidemerge old.c new.c --patch changes.patch --context 5

# Compare the visible text of two web pages (the HTML source when the last mode was HTML source)
oxidemerge --cli https://staging.example.com/ https://www.example.com/
```

### Git integration

Use OxideMerge as Git's diff and merge tool:

```sh
git config --global diff.tool oxidemerge
git config --global difftool.oxidemerge.cmd 'oxidemerge "$LOCAL" "$REMOTE"'
git config --global merge.tool oxidemerge
git config --global mergetool.oxidemerge.cmd \
  'oxidemerge "$LOCAL" "$BASE" "$REMOTE" -o "$MERGED" --dl Local --dr Remote --wl --wr'
git config --global mergetool.oxidemerge.trustExitCode false
```

- `git difftool` opens each changed file; `git difftool --dir-diff` opens one folder compare of
  all of them, and Alt+Down past the last difference moves on to the next changed file.
- `git mergetool` opens a 3-way merge with the common ancestor in the middle. Auto merge (or copy
  differences into the middle), then save: the middle pane is written to the merged file, never
  to the base. Closing without saving leaves the conflict unresolved.
- On macOS, use the full path `/Applications/OxideMerge.app/Contents/MacOS/oxidemerge` unless you
  linked it into your `PATH` (see [Installing](#installing)); on Windows, the path to `oxidemerge.exe`.

### Keyboard shortcuts

Press `F1` or `?`, or click the `?` button at the top right, to open the Help & Shortcuts
dialog. It lists every keyboard shortcut and mouse action by view, and shows where the settings
file is stored.

| Keys | Action |
| --- | --- |
| `F1`, `?` | Show or hide Help & Shortcuts |
| `Ctrl+K`, `Ctrl+Shift+P` | Command palette: search commands, recent comparisons and settings |
| `Ctrl+N` | New text compare with two empty sides |
| `Ctrl+O` | Open |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save all |
| `Ctrl+W`, `Ctrl+F4` | Close tab (right-click a tab to close others, to the right, or all) |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | Next or previous tab |
| `Ctrl+F`, `F3`, `Shift+F3`, `Esc` | Find, find next, find previous, close the find bar |
| `Ctrl+H` (`Cmd+Alt+F` on macOS) | Find and replace |
| `Ctrl+G` | Go to line (go to offset in a binary compare) |
| `Shift+Up`, `Shift+Down`, `Shift+PgUp`, `Shift+PgDn` | Select lines |
| `Ctrl+C` | Copy the selected lines |
| `Ctrl+V` | Paste below the current line, or over the selected lines |
| `Ctrl+F2`, `F4`, `Shift+F4` | Set or remove a bookmark, next bookmark, previous bookmark |
| `F5` | Refresh |
| `Alt+Down`, `Alt+Up` | Next or previous difference; in a file opened from a folder compare, going past the last or first difference opens the next or previous file with differences |
| `Alt+Home`, `Alt+End` | First or last difference |
| `Alt+PgDn`, `Alt+PgUp` | Next or previous conflict |
| `Alt+Right`, `Alt+Left` | Copy the current difference, or the selected lines, right or left |
| `Ctrl+Alt+Right`, `Ctrl+Alt+Left` | Copy all differences right or left |
| `Ctrl+Z`, `Ctrl+Y` / `Ctrl+Shift+Z` | Undo, redo |
| `F2`, `Enter` | Edit the current line |
| `Ctrl+]`, `Ctrl+[` | Indent or unindent the selected lines |
| `Alt+C`, `Alt+Z` | Collapse unchanged lines, word wrap |
| `Space` | Quick preview of the selected file in a folder compare |
| `Ctrl++`, `Ctrl+-`, `Ctrl+0` | Zoom in, zoom out, reset zoom |

## Configuration

Options are stored as JSON in the user configuration folder:

- Windows: `%APPDATA%\OxideMerge\config.json`
- macOS: `~/Library/Application Support/OxideMerge/config.json`
- Linux: `~/.config/OxideMerge/config.json`

For a portable setup, create an empty `oxidemerge.json` next to the executable. OxideMerge then
stores its options in that file instead.

### Filters

Masks are separated by `;`. A leading `!` excludes matches, and a trailing `\` or `/` matches folders:
`*.rs;*.toml;!target\`. WinMerge `.flt` filter files placed in the `Filters` subfolder of the
configuration folder show up in the filter list as `[F] <name>`.

### Languages

Built-in language packs live in [`lang/`](lang) and are embedded in the executable. To add or
override a language, copy `lang/en.json` to `<code>.json`, translate the values (keep any
`{placeholder}` names unchanged), and put the file in a `Languages` folder next to the executable
or in the configuration folder. The unit tests check that every built-in pack has every key and keeps
all placeholders.

### Explorer integration (Windows)

`oxidemerge --register-shell` (or **Tools → Add to Explorer context menu**) adds an **OxideMerge**
submenu to the Explorer context menu of files, folders, drives and folder backgrounds, for the
current user. It works like WinMerge's shell extension:

- **Compare**: select two or three files or folders and compare them in one window (3-way for
  three). With a single item, it compares the item with the side chosen earlier with Select Left
  (and Select Middle), or opens the Open dialog with the item filled in.
- **Select Left** / **Select Middle**: remember an item, for example in another folder. The Compare
  entry then reads **Compare to "name"**.
- **Compare As** (files): compare as text, binary, image or web page.

Explorer starts one OxideMerge process per selected item; they find each other through a small
spool folder in the temp directory, and only the first one opens a window. The entries point at
the executable you run the command from, so run it from the installed copy (`make shell-register`
does this for you), and run it again after moving the program. On Windows 11 the entries are under
**Show more options**.

### Finder and Linux file manager integration

On macOS, **Tools → Add Finder Quick Actions** (or `oxidemerge --register-shell`) adds
**OxideMerge: Compare**, **OxideMerge: Select Left** and **OxideMerge: Select Middle** to Finder's
Quick Actions and Services menus. They are Automator workflows in `~/Library/Services`.

On Linux, **Tools → Add to file manager context menu** adds the same three entries to the file
managers that are installed: a Nautilus scripts submenu (`~/.local/share/nautilus/scripts`), Nemo
actions (`~/.local/share/nemo/actions`) and a Dolphin service menu
(`~/.local/share/kio/servicemenus`).

The entries work like the Explorer ones: Compare compares the two or three selected items, or one
item with the side chosen earlier with Select Left / Select Middle. As on Windows, they point at
the executable you register from.

## Project layout

```
src/
  main.rs           command line parsing and startup
  config.rs         persistent options
  i18n.rs           language packs
  shell.rs          Explorer context menu, Finder Quick Actions, Linux file manager entries
  core/             diff engine, folder scanning, binary/image/table compare,
                    conflict parsing, filters, archives, patches, reports, projects,
                    comment stripping, JSON/XML formatting,
                    web page download, text extraction and screenshots
  ui/               egui application, views, theme and widgets
build.rs            embeds the icon and version details in the Windows executable
assets/             application icon (.png, .ico), Linux desktop entry and AppImage launcher
scripts/            Linux AppImage packaging used by `make appimage` and CI, and release notes
                    taken from CHANGELOG.md
lang/               built-in translations
samples/            files used by the make demo targets
```

## Limitations

- Archive support covers zip-based formats, tar (plain, gzip, bzip2, xz) and 7z; rar is not
  supported.
- Formatted JSON and XML are shown read-only; turn the option off to edit and merge those files.
- Web page compare fetches pages without running their scripts, except in Screenshot mode. Text,
  HTML source and resource tree modes therefore see the HTML as served, not as rendered by
  JavaScript. Screenshots need a Chromium-based browser. There is no cookie or login support.
- Text editing is line-based, so it is not a full code editor.
- Unpacker plugins are external commands rather than WinMerge plugin DLLs or scripts.
- Machine-assisted translations have not been reviewed by native speakers.

## License

MIT. See [LICENSE](LICENSE).
