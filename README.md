# OxideMerge

![OxideMerge - the cross-platform diff and merge tool](images/banner.jpg)

OxideMerge is an open source, cross-platform differencing and merging tool written in Rust. It
brings the feature set of [WinMerge](https://winmerge.org/)
([source](https://github.com/WinMerge/winmerge)) to **Windows, macOS and Linux**. It compares and
merges folders, text files, binary files and images, with a modern multilingual UI.

The name comes from the language it is written in: rust is iron oxide.

![Start page](images/start.png)

| Folder compare | Text compare |
| --- | --- |
| ![Folder compare](images/folder.png) | ![Text compare](images/text.png) |

## Features

**Folder compare and merge**
- 2-way and 3-way folder comparison, recursive or flat, shown as a tree or a flat list.
- Compare methods: full contents, quick contents, binary contents, modified date, date and size, size only.
- Show or hide identical, different and unique items. Copy, move or delete items between sides,
  with optional Recycle Bin support and confirmation prompts.
- File filters: wildcard masks such as `*.c;*.h;!*.bak;!.git\` and WinMerge `.flt` filter files.
- Zip-based archives (`.zip`, `.jar`, `.apk`, `.docx`, `.xlsx`, `.odt`, `.epub`, `.nupkg`, ...) are extracted and compared as folders.
- Reports in HTML, CSV and plain text.

**Text compare and merge**
- 2-way and 3-way comparison with Myers, Patience or LCS diff algorithms.
- Inline (word and character) differences, moved-block detection and a location pane overview.
- WinMerge-style ignore options: whitespace (changes or all), case, blank lines, end-of-line style,
  numbers and code page.
- Copy differences left or right, copy all, auto merge for 3-way compares, undo and redo, and in-place editing.
- Resolve version-control conflict files containing `<<<<<<<`, `|||||||`, `=======` and `>>>>>>>` markers.
- Encoding and end-of-line detection, syntax highlighting, find, go to line, and CSV/TSV table mode.
- Patch generation in normal, context and unified formats.
- Unpacker plugins: run an external command (for example `pandoc -t plain ${file}`) to convert files to text before comparing.

**Binary compare**
- Side-by-side hex view with byte-level differences, navigation and synchronized scrolling.

**Image compare**
- Side-by-side, overlay and difference modes, with zoom, fit and 1:1 views.
- Difference blocks highlighted with an adjustable color threshold and block size.

**Interface**
- Tabbed documents, a start page with recent comparisons, and drag and drop of files and folders.
- Light and dark themes.
- 10 built-in languages: English, 繁體中文, 简体中文, 日本語, 한국어, Deutsch, Français, Español,
  Русский and Português (Brasil). You can add more as JSON files (see [Languages](#languages)).
- Windows Explorer context menu integration (Windows only).

## Platforms

| Platform | Notes |
| --- | --- |
| Windows 10/11 | Explorer context menu, WinMerge-style `/switch` arguments, Recycle Bin |
| macOS 11+ | Native file dialogs, Trash, "Reveal in Finder" |
| Linux (X11 and Wayland) | GTK file dialogs, freedesktop Trash |

The same code base builds on all three. Fonts come from the system: Segoe UI and Cascadia Mono on
Windows, SF and Menlo on macOS, and Noto, Inter or DejaVu on Linux. For Chinese, Japanese and
Korean text on Linux, any CJK font that fontconfig knows about is used (Noto Sans CJK or WenQuanYi
recommended); under WSL the Windows fonts are used when no Linux CJK font is installed.

Under WSL2 the app selects Mesa's `d3d12` driver (GPU acceleration through WSLg) when it is
installed and silences Mesa's EGL driver-probing warnings. Set `GALLIUM_DRIVER` or `EGL_LOG_LEVEL`
yourself to override either.

## Building

You need a Rust toolchain ([rustup](https://rustup.rs/)).

On Linux, also install the development packages for GTK 3 (file dialogs), X11, Wayland and
OpenGL. On Debian or Ubuntu:

```sh
sudo apt install libgtk-3-dev libxkbcommon-dev libwayland-dev libxcursor-dev libxrandr-dev libxi-dev libgl-dev
```

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
| `make shell-register` | Install, then add the Explorer context menu (Windows) |
| `make dist` | Package the release binary into `dist/` (`.zip` on Windows, `.tar.gz` elsewhere) |
| `make clean` | Remove build output |
| `make help` | List all targets |

### Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every push to `main` and on every
pull request. It builds OxideMerge on Windows, macOS and Linux, runs clippy with warnings treated as
errors, runs the unit tests, and smoke-tests the command line (`--version`, `--list-languages` and
the `--cli` exit codes). The macOS job also builds a universal binary (Apple silicon and Intel),
ad-hoc signs it, and uploads `oxidemerge-<version>-macos.dmg` as a workflow artifact.

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
oxidemerge left.txt right.txt -o merged.txt     # save the merge result to another file
```

### Command line options

| Option | Description |
| --- | --- |
| `PATHS...` | 2 or 3 files or folders, or a single conflict file |
| `-r, --recursive` | Include subfolders in folder compares |
| `-f, --filter <MASK>` | File filter, for example `*.c;*.h` or `!*.bak` |
| `-m, --method <M>` | Folder compare method: `full`, `quick`, `binary`, `date`, `datesize`, `size` |
| `-t, --compare-as <T>` | Force `text`, `binary` or `image` |
| `-o, --output <PATH>` | Output path for the merge result |
| `--dl/--dm/--dr <TEXT>` | Descriptions shown instead of the left, middle and right paths |
| `--wl/--wm/--wr` | Open the left, middle or right side read-only |
| `--ignore-ws <MODE>` | Ignore whitespace: `compare`, `change` or `all` |
| `--ignore-case`, `--ignore-blank`, `--ignore-eol`, `--ignore-numbers` | Ignore options |
| `--report <FILE>` | Write a folder compare report (`.html`, `.csv` or `.txt`) |
| `--patch <FILE>` | Write a patch for two files or folders |
| `--patch-style <S>` | `normal`, `context` or `unified` (default) |
| `--context <N>` | Context lines for patches (default 3) |
| `--cli` | Run without a window; exit code 0 = identical, 1 = different, 2 = error |
| `--lang <CODE>` | UI language, for example `ja` or `zh-TW` |
| `--list-languages` | Print the available languages |
| `--register-shell` / `--unregister-shell` | Add or remove the Explorer context menu (Windows) |

On Windows, WinMerge-style switches are also accepted, so existing scripts keep working:
`/r`, `/f`, `/o`, `/dl`, `/dr`, `/wl`, `/wr` and so on.

### Scripting

```sh
# Exit code only, no window
oxidemerge --cli -r dirA dirB && echo identical

# HTML report of a folder compare (--report and --patch never open a window)
oxidemerge -r dirA dirB --report report.html

# Unified diff with 5 lines of context
oxidemerge old.c new.c --patch changes.patch --context 5
```

### Keyboard shortcuts

| Keys | Action |
| --- | --- |
| `Ctrl+O` | Open |
| `Ctrl+S` | Save |
| `Ctrl+W`, `Ctrl+F4` | Close tab |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | Next or previous tab |
| `Ctrl+F`, `F3`, `Shift+F3` | Find, find next, find previous |
| `Ctrl+G` | Go to line |
| `F5` | Refresh |
| `Alt+Down`, `Alt+Up` | Next or previous difference |
| `Alt+Home`, `Alt+End` | First or last difference |
| `Alt+PgDn`, `Alt+PgUp` | Next or previous conflict |
| `Alt+Right`, `Alt+Left` | Copy the current difference right or left |
| `Ctrl+Alt+Right`, `Ctrl+Alt+Left` | Copy all differences right or left |
| `Ctrl+Z`, `Ctrl+Y` | Undo, redo |
| `F2`, `Enter` | Edit the current line |
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

`oxidemerge --register-shell` adds **Select Left**, **Compare To** and **Compare** entries to the
Explorer context menu for the current user. These entries point at the executable you run the
command from, so run it from the installed copy (`make shell-register` does this for you).

## Project layout

```
src/
  main.rs           command line parsing and startup
  config.rs         persistent options
  i18n.rs           language packs
  shell.rs          Explorer context menu
  core/             diff engine, folder scanning, binary/image/table compare,
                    conflict parsing, filters, archives, patches, reports
  ui/               egui application, views, theme and widgets
lang/               built-in translations
samples/            files used by the make demo targets
```

## Limitations

- Archive support covers zip-based formats only; 7z, rar and tar are not supported.
- WinMerge's web page compare is not implemented.
- Text editing is line-based, so it is not a full code editor.
- Unpacker plugins are external commands rather than WinMerge plugin DLLs or scripts.
- Machine-assisted translations have not been reviewed by native speakers.

## License

MIT. See [LICENSE](LICENSE).
