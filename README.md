# OxideMerge

English | [繁體中文](README_zh-TW.md)

![OxideMerge on Windows, macOS and Linux](images/banner.jpg)

OxideMerge is a free, open-source tool for comparing and merging files and folders on
**Windows, macOS and Linux**. Use it to review changes, combine edits and resolve merge
conflicts in a visual interface.

Inspired by [WinMerge](https://github.com/WinMerge/winmerge), OxideMerge is written in Rust.
It supports text and folder merging, as well as binary, image and web page comparison.

[Download](https://github.com/kuopinghsu/OxideMerge/releases) ·
[User guide](docs/user-guide.adoc) ·
[Release history](CHANGELOG.md)

![OxideMerge start page](images/start.png)

## Contents

- [Features](#features)
- [Installation](#installation)
- [Getting started](#getting-started)
- [Command-line usage](#command-line-usage)
- [Git and file manager integration](#git-and-file-manager-integration)
- [Configuration](#configuration)
- [Documentation](#documentation)
- [Limitations](#limitations)
- [Development](#development)
- [Releases](#releases)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

## Features

### Compare files and folders

- Compare two versions, or compare three versions with a shared original version.
- Scan folders and subfolders, filter files by name, and copy selected items between folders.
- Review changed text files together in the **All differences** view.
- Compare supported archives as folders, including ZIP, TAR and 7z.
- Compare a file or folder with a Git revision, such as a branch, tag or commit.

### Review and merge text

- See changes side by side, including changes within a line and moved blocks of text.
- Copy individual changes between versions, edit text, and undo or redo edits.
- Automatically merge compatible changes in a three-way comparison, then review conflicts.
- Open files that contain version-control conflict markers and resolve them visually.
- Focus on relevant changes with options to ignore whitespace, comments or selected patterns.
  Reusable comparison rules and numeric tolerance help with generated files and measurement data.
- Use syntax highlighting, search and replace, bookmarks, and CSV/TSV table views with
  multiline records, cell editing and cell merging.
- Set synchronization points to line up lines the compare pairs wrongly.

### Compare other content

- **Binary files:** inspect byte differences in a side-by-side hexadecimal view.
- **Images:** compare images side by side or use overlay, swipe, blink and difference views.
- **Web pages:** compare the text a page shows after its scripts run, full page screenshots with
  the text differences highlighted on the page, HTML source or downloaded resources.

### Save and share your work

- Export HTML reports for review in a browser, HTML slideshows that show one difference at a
  time, XML reports for other programs, or folder summaries in CSV and plain text.
- Create patches and apply unified patches for review before saving.
- Save comparison settings as project files, save annotated text comparisons, or capture portable
  folder reviews with file contents, comparison settings, bookmarks and comments.
- Use OxideMerge with Git, from the command line, or from your system's file manager.

The interface includes light and dark themes and 10 built-in languages: English, Traditional
Chinese, Simplified Chinese, Japanese, Korean, German, French, Spanish, Russian and Brazilian
Portuguese. Additional translations can be loaded from language files.

### Interface and screenshots

**Folder comparison**

![Folder comparison](images/folder.png)

**Text comparison**

![Text comparison](images/text.png)

## Installation

Download a package for your system from [GitHub Releases](https://github.com/kuopinghsu/OxideMerge/releases).
Release downloads also include PDF user guides in English and Traditional Chinese. The release
page lists each download's SHA-256 digest for verifying it.

### Windows

Supports Windows 10 and 11 on x64 and Arm64.

Download the `windows-x64.zip` or `windows-arm64.zip` package for your processor, unzip it and
run `oxidemerge.exe`. No installer is required. Release filenames include the version, for
example `oxidemerge-<version>-windows-x64.zip`.

### macOS

Supports macOS 11 or later on Apple silicon and Intel Macs.

Open the `macos-universal.dmg` package and drag **OxideMerge** to **Applications**.

The app is signed ad hoc and is not notarized. If macOS blocks the first launch, try opening
the app once, then choose **Open Anyway** in **System Settings → Privacy & Security**.
See [macOS signing](docs/macos-signing.md) for more information.

### Linux

Supports x64 and Arm64 systems with X11 or Wayland.

- **AppImage:** runs on Ubuntu 20.04, RHEL 8, Debian 10 and newer. Make the downloaded file
  executable with `chmod +x <filename>.AppImage`, then run it. If FUSE is unavailable, add
  `--appimage-extract-and-run` when starting it.
- **TAR archive:** the standalone executable, with a desktop file and icon, for users who
  prefer it.

File dialogs use `xdg-desktop-portal`, which desktop environments normally provide.
For Chinese, Japanese and Korean text, install a suitable font such as Noto Sans CJK.

## Getting started

Launch OxideMerge to open the start page. Select the files or folders to compare, drag them
onto the window, or reopen a recent comparison.

### Compare two files or folders

Open two files to see their differences side by side. Use **Compare** to move between
differences and **Merge** to copy changes to the other side. Save when you are ready.

For folders, enable subfolders when needed. Select an item to preview it or open it in a
separate comparison tab. Use **All differences** to review changed text files together.

### Merge three versions

Open the files in this order: **your version, the common ancestor, the other version**.
The common ancestor is the original version that both versions were based on; it appears in
the middle pane.

Choose **Merge → Auto merge** to combine compatible changes. Review the remaining conflicts,
then save the result. You can also open a single file that contains conflict markers.

Three-way folder merging writes into the middle folder. **Merge → Auto merge folders...**
shows the planned copies, deletions and merges for confirmation before applying them.

## Command-line usage

These examples assume that `oxidemerge` is available in your `PATH`. Otherwise, use the full
path to the executable. On macOS, the executable is normally at
`/Applications/OxideMerge.app/Contents/MacOS/oxidemerge`.

```sh
# Compare two text files
oxidemerge old.txt new.txt

# Compare folders, including subfolders
oxidemerge -r old-folder new-folder

# Merge three files and save the middle pane to a separate output file
oxidemerge mine.txt base.txt theirs.txt -o merged.txt

# Open a file with merge conflict markers
oxidemerge conflict.txt

# Export a folder report with text differences, without opening a window
oxidemerge -r old-folder new-folder --report report.html --report-diffs
```

For scripts, `--cli` runs without a window and returns `0` for identical content, `1` for
differences and `2` for an error. Run `oxidemerge --help` for all command-line options.
Common options include:

- `-r`: include subfolders; `-f "*.rs;!target/"`: filter folder contents.
- `-t text`, `-t binary`, `-t image`, `-t web`: choose the comparison type.
- `-o <file>`: save the merge result to a separate file.
- `--ignore-case`, `--ignore-eol`, `--ignore-comments`: ignore selected types of changes.
- `--rules <name-or-path>`: use a comparison rules file.
- `--git-rev <revision>`: compare a single file or folder with a Git revision.
- `--report <file>`: export a report as `.html`, `.xml`, or for folders also `.csv` or `.txt`; add
  `--report-diffs` for text differences in a folder HTML report, or `--slideshow` for an HTML
  slideshow with one difference per slide.
- `--patch <file>`: export a patch from a two-way file or folder comparison.

`--cli`, `--report` and `--patch` run without opening a window. On Windows, supported
WinMerge-style switches such as `/r`, `/f` and `/o` are also accepted.
See the [user guide](docs/user-guide.adoc) for the full option reference and scripting examples.

## Git and file manager integration

### Git

OxideMerge can serve as Git's visual diff and merge tool. With `oxidemerge` available in your
`PATH`, configure Git as follows in a POSIX-compatible shell, such as Bash or Git Bash:

```sh
git config --global diff.tool oxidemerge
git config --global difftool.oxidemerge.cmd 'oxidemerge "$LOCAL" "$REMOTE"'
git config --global merge.tool oxidemerge
git config --global mergetool.oxidemerge.cmd \
  'oxidemerge "$LOCAL" "$BASE" "$REMOTE" -o "$MERGED" --dl Local --dr Remote --wl --wr'
git config --global mergetool.oxidemerge.trustExitCode false
```

Use `git difftool` for individual files or `git difftool --dir-diff` for a folder comparison.
Use `git mergetool` to resolve conflicts: review the three versions, merge into the middle
pane and save. The result is written to Git's merge output file, leaving the base unchanged.

If the executable is outside your `PATH`, use its full path in the configured commands.
See the [user guide](docs/user-guide.adoc) for details.

### File managers

Register integration from **Tools**, or run `oxidemerge --register-shell` from the installed
copy. Use `oxidemerge --unregister-shell` to remove it.

- **Windows Explorer:** compare selected files or folders through the context menu.
  On Windows 11, the entries appear under **Show more options**.
- **macOS Finder:** use the OxideMerge Quick Actions and Services entries.
- **Linux:** use the available Nautilus, Nemo or Dolphin menu entries.

You can compare selected items immediately, or remember an item with **Select Left** or
**Select Middle** and choose another item later. Entries refer to the registered executable;
register again if you move it.

## Configuration

Use **Options** to adjust comparison behavior, appearance and language. Settings are stored
in the user configuration folder:

- **Windows:** `%APPDATA%\OxideMerge\config.json`
- **macOS:** `~/Library/Application Support/OxideMerge/config.json`
- **Linux:** `~/.config/OxideMerge/config.json`

For a portable setup, create an empty `oxidemerge.json` next to the executable.
For a separate configuration, set `OXIDEMERGE_CONFIG_DIR` to the desired folder.

### Filters and comparison rules

File filters select which items appear in folder comparisons. Separate patterns with `;`,
use `!` to exclude a match, and add a trailing `/` or `\` to match folders. For example,
`*.rs;*.toml;!target/` includes Rust and TOML files while excluding the `target` folder.
WinMerge `.flt` files in the configuration folder's `Filters` directory are also available.

Comparison rules control which content differences are ignored. Use **Options → Compare rules**
to manage `.oxrules` files with ignored lines, substitutions and ignored blocks. Rules can be
imported and exported for use across projects or teams. A folder comparison can select its own
rules file.

### Languages

Built-in translations are stored in [lang/](lang). To add or override a translation, copy
`lang/en.json` to `<code>.json`, translate the values and keep all `{placeholder}` names unchanged.
Place the file in a `Languages` folder next to the executable or inside the configuration folder.
Missing messages and translations with invalid placeholders fall back to English.

## Documentation

- [User guide — English](docs/user-guide.adoc): workflows, comparison options, keyboard
  shortcuts, Git integration and troubleshooting.
- [User guide — Traditional Chinese](docs/user-guide.zh-TW.adoc).
- [Benchmarks](docs/benchmarks.md): performance measurements and how to run them.
- [Changelog](CHANGELOG.md): changes by version.

Press **F1** or click the **?** button in the app for Help & Shortcuts. The dialog lists
keyboard and mouse actions and shows the active settings location. On macOS, most shortcuts
use Command where Windows and Linux use Ctrl; the interface displays the platform's keys.

## Limitations

- Archives are compared read-only; changes cannot be saved back into an archive. RAR is not
  supported.
- JSON and XML files are read-only while automatic formatting is enabled. Turn it off to
  edit or merge them.
- Web screenshots require an installed Chromium-based browser. Without one, the text and HTML
  modes do not run JavaScript and may miss dynamically loaded content. Authenticated browsing is
  not supported; pages that require a login may show the login page instead.
- Text editing is line-based and is intended for reviewing and merging changes.
- WinMerge project files and filter files are supported, but WinMerge plugin DLLs and scripts
  are not. Unpacker plugins use external commands consistently in file, full-content folder, CLI
  and report comparisons; PDF and Office presets require separately installed conversion tools.
- Machine-assisted translations have not been reviewed by native speakers.

## Development

### Build from source

Install a Rust toolchain with [rustup](https://rustup.rs/). Linux also requires a C compiler.
X11, Wayland, keyboard and OpenGL libraries are loaded at runtime; their development packages
are not required to build OxideMerge.

Run these commands from the repository root:

```sh
cargo build --release --locked
cargo test --locked
```

The executable is `target/release/oxidemerge` on macOS and Linux, or
`target/release/oxidemerge.exe` on Windows. To try the sample folder comparison, run `make demo`.

### Build and maintenance commands

The [Makefile](Makefile) provides shortcuts for common tasks and requires GNU Make.
Run `make help` to list all targets.

**Build and run**

- `make` or `make release`: build an optimized executable.
- `make build`: build a debug executable.
- `make run ARGS="a.txt b.txt"`: run the debug build with comparison paths.
- `make demo`, `make demo-text`, `make demo-3way`, `make demo-conflict`: open sample comparisons.
  For web page compare, see `samples/web/README.md`.
- `make install` or `make uninstall`: install to, or remove from, Cargo's binary directory
  (`~/.cargo/bin` by default).
- `make shell-register`: install the executable and register file manager integration.
- `make clean`: remove build output and local distribution packages.

**Check code quality**

- `make fmt-check`: check Rust formatting; `make fmt` applies formatting.
- `make check`: type-check all targets in the current build configuration.
- `make lint`: run Clippy with warnings treated as errors.
- `make test`: run unit, interface and command-line tests.
- `make ci`: run `check`, `lint` and `test`.
- `make gui-smoke`: build a release, open real windows and verify their comparison results.
- `make check-platforms`: type-check the Windows, macOS and Linux targets listed in the
  Makefile. Install their standard libraries with `rustup target add` first.

**Build distribution files**

- `make dist`: create a ZIP package on Windows or a TAR.GZ package on macOS and Linux in `dist/`.
- `make appimage`: create a Linux AppImage for the host architecture, x86_64 or aarch64.
  Requires the X11, Wayland and keyboard libraries and downloads `appimagetool`.
- `make user-guide`: create English and Traditional Chinese PDF guides in `dist/`.
  Requires Asciidoctor PDF, `curl`, `shasum`, Python 3 with `fontTools`, fontconfig and DejaVu Sans.
  The build script downloads the required Noto Sans CJK fonts.

### Testing

`cargo test` covers three areas:

- **Core behavior:** comparison, merging, patches, folders and file operations. Randomized tests
  in `src/core/regress.rs` also check generated diff, merge and patch cases.
- **Interface behavior:** tests drive the actual views without a display, including menus,
  editing, file drops and saved results. Failed window tests write diagnostic output to
  `target/gui-tests/<test>.txt`.
- **Command-line behavior:** tests in `tests/cli.rs` run the executable and verify exit codes,
  reports, patches and error messages.

Tests use a separate configuration folder and do not change your application settings.
For other isolated runs, set `OXIDEMERGE_CONFIG_DIR` to a dedicated configuration folder.
Some web tests start a local HTTP server, so the test environment must allow local listening
sockets.

Run `cargo test --release --locked format_regression -- --nocapture` for the focused
format checks. They cover BOM and line-ending patch round trips, ignored text changes,
mixed-ending merge/save/undo, and Git checkout and patch compatibility. Git must be installed.
CI includes these checks in the full test suite on Windows, Linux and macOS.

Randomized test failures print a seed that can be used to reproduce the case:

```sh
OXIDEMERGE_TEST_SEED=<seed> cargo test regress
OXIDEMERGE_TEST_CASES=20000 cargo test --release regress
```

For checks with real application windows, run `make gui-smoke` or
`scripts/gui-smoke.sh path/to/oxidemerge`. The script checks two-file, three-file and folder
comparisons. On Linux without a display, it starts Xvfb. With `OXIDEMERGE_SMOKE_REPORT=<file>`,
the application records its tabs and messages after comparison, then closes.

Benchmarks run separately. See [Benchmarks](docs/benchmarks.md) for commands, methodology and
baseline results.

### Continuous integration

The [CI workflow](.github/workflows/ci.yml) runs on pushes to `main` and on pull requests.
Routine runs check code with Clippy, run the complete test suite, build release executables,
and verify command-line and window behavior on Windows x64, Linux x64 and macOS Apple silicon.
Windows and Linux window checks use Mesa's software renderer; Linux also uses Xvfb. Failed test output is retained as `test-output-*`
artifacts. A separate `cargo deny` job checks dependencies; see [Security](SECURITY.md).

Release tags automatically enable the full packaging workflow, including native Windows/Linux
Arm64 checks and the Intel macOS build. To validate all packages without publishing a release,
start the workflow manually and enable **packages**. Ordinary pushes and pull requests skip
AppImage builds, PDF generation and distribution packaging, so slow package mirrors do not
hold up routine code checks. Full test coverage remains enabled on the three default platforms.

Manual runs can also enable randomized test cases and benchmarks. Release publishing does not
depend on these optional runs.

Release tags and manual packaging runs produce and validate these distribution files:

- **Windows x64 and Arm64:** ZIP packages holding only `oxidemerge.exe`, which is checked to be
  for the right processor and to run.
- **Linux x64 and Arm64:** TAR.GZ packages built on Ubuntu 22.04. The program in them is run
  and checked not to change application settings.
- **Linux AppImage:** packages built on AlmaLinux 8 with glibc 2.28. Required X11, Wayland and
  keyboard libraries are bundled and used when system libraries are unavailable. Packages are
  checked on Ubuntu 22.04 and AlmaLinux 8 for both architectures, including library resolution,
  command-line behavior and window startup.
- **macOS:** an ad-hoc signed universal application in a DMG package. Both Apple silicon
  and Intel architectures are verified, and the Intel executable is tested through Rosetta.
- **Documentation:** English and Traditional Chinese PDF guides. Missing font coverage fails
  the document build.

Windows and Linux Arm64 packages are built and tested on native Arm runners.

## Releases

### Release requirements

A tag such as `v0.9.6` triggers package publication on GitHub Releases. The tag must match the
version in `Cargo.toml`, and `CHANGELOG.md` must contain a section for that version. Tags with
`-`, such as `v1.0.0-rc1`, are published as pre-releases.

Release notes come from the matching changelog section, followed by download instructions.
The workflow reads the changelog without changing it. Before publishing, it checks that all
expected packages, and nothing else, exist and that the release has not already been published.
Each release includes PDF guides and the source commit used for the build, and GitHub lists the
SHA-256 digest of every file. A release holds exactly these files (for version 0.9.6):

```text
oxidemerge-0.9.6-linux-arm64.AppImage
oxidemerge-0.9.6-linux-arm64.tar.gz
oxidemerge-0.9.6-linux-x64.AppImage
oxidemerge-0.9.6-linux-x64.tar.gz
oxidemerge-0.9.6-macos-universal.dmg
oxidemerge-0.9.6-user-guide.pdf
oxidemerge-0.9.6-user-guide.zh-TW.pdf
oxidemerge-0.9.6-windows-arm64.zip
oxidemerge-0.9.6-windows-x64.zip
```

### Release procedure

The following example uses `0.9.6`; replace it with the version you intend to release.

1. Update `version` in `Cargo.toml` and run `cargo build` to update `Cargo.lock`.
2. Review the matching section in `CHANGELOG.md`. Replace `Unreleased` in its heading with the
   release date, using `YYYY-MM-DD`.
3. Run `make fmt-check ci`. Preview the release notes with
   `scripts/release-notes.sh 0.9.6`.
4. Commit and push to `main`. Wait for the required CI checks to pass on every platform.
5. Tag the verified commit and push the tag:

   ```sh
   git tag -a v0.9.6 -m "OxideMerge 0.9.6"
   git push origin v0.9.6
   ```

6. Add an `Unreleased` section for the next version to `CHANGELOG.md` and record subsequent
   changes there.

Do not move or reuse a pushed release tag. Downloaded packages must continue to match their
published checksums. If a release build fails or a published release needs a correction, fix
it on `main` and create a new patch version. Keep failed tags in place; a failed build does not
publish a partial release because publication waits for all required package jobs.

### Recover from a failed or incorrect release

For example, if `v0.9.5` fails to build or needs a correction after publication, release the
fix as `v0.9.6`. Choose a new, unused patch version; keep the original tag and published assets.

1. Start with a clean working tree and update `main`. If local changes are present, commit
   them or save them with `git stash` before switching branches.

   ```sh
   git status --short
   git switch main
   git pull --ff-only origin main
   ```

2. Fix the source code, packaging or workflow on `main`. Update `version` in `Cargo.toml`
   to `0.9.6`. Add or update the `0.9.6` section in `CHANGELOG.md`, describe the correction,
   and replace `Unreleased` with the release date. Keep the `0.9.5` history unchanged.

3. Update the lockfile, run the checks and review the new release notes:

   ```sh
   cargo build
   make fmt-check ci
   scripts/release-notes.sh 0.9.6
   git diff --check
   git diff
   ```

4. Stage the fixes and version files, review the staged changes, then commit and push.
   `git add -p` lets you select the changes that belong to this correction. Add any new files
   explicitly with `git add <path>`.

   ```sh
   git add -p
   git add Cargo.toml Cargo.lock CHANGELOG.md
   git diff --cached
   git commit -m "Fix release issues and prepare 0.9.6"
   git push origin main
   git rev-parse HEAD
   ```

5. Wait for the required CI checks to pass for the commit printed above on every platform.
   Confirm that `HEAD` still points to that verified commit before tagging. Do not tag a newer,
   unchecked commit.

   ```sh
   git log -1 --oneline
   git tag -a v0.9.6 -m "OxideMerge 0.9.6"
   git push origin v0.9.6
   ```

6. Check the tag's workflow and the new GitHub release. Confirm that all expected packages and
   PDF guides are available and that the release notes describe the fix.
   Then add an `Unreleased` section for the next version to `CHANGELOG.md`.

If the new release build also fails, repeat this process with another unused patch version.
Do not delete or force-push the old tag, or replace assets attached to the old release.

## Contributing

Bug reports, feature requests, documentation improvements and translations are welcome through
[GitHub Issues](https://github.com/kuopinghsu/OxideMerge/issues) and pull requests.
For code changes, follow the [development instructions](#development) and run the relevant
checks before submitting a pull request.

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting, the handling of untrusted input
and dependency checks.

## License

OxideMerge is licensed under the [MIT License](LICENSE).
