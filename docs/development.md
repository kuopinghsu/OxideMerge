# OxideMerge development guide

English | [繁體中文](development.zh-TW.md)

This guide is for people who build, test or release OxideMerge. To install and use the
program, see the [README](../README.md) and the [user guide](user-guide.adoc).

## Contents

- [Build from source](#build-from-source)
- [Make targets](#make-targets)
- [How outcomes are named](#how-outcomes-are-named)
- [Testing](#testing)
- [Continuous integration](#continuous-integration)
- [Releases](#releases)

## Build from source

You need a Rust toolchain from [rustup](https://rustup.rs/). On Linux, you also need a C
compiler. OxideMerge loads the X11, Wayland, keyboard and OpenGL libraries when it starts,
so you do not need their development packages to build it.

Run these commands in the repository folder:

```sh
cargo build --release --locked
cargo test --locked
```

The program is `target/release/oxidemerge` on macOS and Linux, and
`target/release/oxidemerge.exe` on Windows. To open the sample folder comparison, run
`make demo`.

### Version information

**About** and `--version` show which build you are running:

- A development build shows its short Git revision. If tracked files have changes that are
  not committed, `-dirty` follows the revision.
- A clean optimized build of a `vVERSION` tag shows its build date in UTC. To get the same
  date in every build (a reproducible build), set `SOURCE_DATE_EPOCH`.
- Without Git information, the build uses `GITHUB_SHA` if it is valid. Otherwise it shows
  `unknown`.

## Make targets

The [Makefile](../Makefile) has short commands for common tasks. It requires GNU Make.
Run `make help` to list all targets.

### Build and run

| Command | What it does |
| --- | --- |
| `make` or `make release` | Builds an optimized program. |
| `make build` | Builds a debug program. |
| `make run ARGS="a.txt b.txt"` | Runs the debug build with the given paths. |
| `make demo`, `make demo-text`, `make demo-3way`, `make demo-3way-merge`, `make demo-3way-folder`, `make demo-conflict` | Opens sample comparisons. |
| `make demo-web`, `make demo-web-text`, `make demo-web-headings`, `make demo-web-tables` | Opens sample web page comparisons. The last three show the *Formatted* mode. |
| `make install`, `make uninstall` | Installs the program into Cargo's program folder (`~/.cargo/bin` by default), or removes it. |
| `make shell-register` | Installs the program and registers the file manager integration. |
| `make clean` | Removes build output and local packages. |

[samples/README.md](../samples/README.md) lists every sample and the feature that it
shows. For the web page samples, see [samples/web/README.md](../samples/web/README.md).

### Check the code

| Command | What it does |
| --- | --- |
| `make fmt-check` | Checks the Rust code format. `make fmt` fixes the format. |
| `make check` | Type-checks all targets in the current build configuration. |
| `make lint` | Runs Clippy and treats warnings as errors. |
| `make test` | Runs the unit, interface and command-line tests. |
| `make ci` | Runs `check`, `lint` and `test`. |
| `make gui-smoke` | Builds a release, opens real windows and checks their comparison results. |
| `make check-platforms` | Type-checks the Windows, macOS and Linux targets that the Makefile lists. First install their standard libraries with `rustup target add`. |

### Build packages

| Command | What it does |
| --- | --- |
| `make dist` | Creates a package in `dist/`: a ZIP file on Windows, or a TAR.GZ file on macOS and Linux. |
| `make appimage` | Creates a Linux AppImage for this computer's processor (x86_64 or aarch64). It needs the X11, Wayland and keyboard libraries, and it downloads `appimagetool`. |
| `make user-guide` | Creates the English and Traditional Chinese PDF guides in `dist/`. |

`make user-guide` needs Asciidoctor PDF, `curl`, `shasum`, Python 3 with `fontTools`,
fontconfig and the DejaVu Sans font. The build script downloads the Noto Sans CJK fonts
that it needs.

## How outcomes are named

Every feature that can end in something other than plain success reports which of seven
outcomes it was. `core::problem::Problem` is that taxonomy, and nothing else should invent
a parallel one:

| Outcome | What it means | Where it comes from |
| --- | --- | --- |
| `ambiguous` | Another reading of the input is as likely; the result stands but is to be checked by hand | Repeated items that cannot be told apart: the semantic and Tree views mark such rows themselves |
| `stale` | The input is no longer as it was when the result or the plan was made | A file changed after a synchronization preview |
| `unsupported` | The input or the request is of a kind this feature refuses | A format, version or file type it does not handle; a key column that is missing or repeated; a table past the record limit |
| `read_failure` | Reading failed, or would | No permission; a file another program holds open without sharing it |
| `write_failure` | Writing failed, or would | No permission; no room on the drive |
| `canceled` | The user stopped it | *Stop* or *Cancel* |
| `recovery_needed` | A change was cut short and could not be wholly undone; a recovery journal is kept | A rollback that could not put everything back |

A feature with outcomes of its own maps them onto these rather than replacing them:
`Problem::of_io` for an I/O error (which differs by whether it was reading or writing) and
`Problem::of_table` for a key or table that cannot be used as asked. Add a mapping
function beside those rather than a new enum.

The names reach users two ways. `--sync-apply` and `--sync-recover` put the outcome in
their JSON as `problem`, which is the serialized name in the table above, so scripts can
branch on it; this is a compatibility surface, so do not rename a variant without a
release note. `Problem::label()` gives the translated short name (`problem.*` in
`lang/*.json`) for a view that leads with the kind of problem before the detail of what
went wrong. The detail stays the feature's own wording: the taxonomy says what kind of
thing happened, not what happened.

## Testing

`cargo test` checks three areas:

- **Core behavior:** comparing, merging, patches, folders and file operations. Random
  tests in `src/core/regress.rs` also check generated diff, merge and patch cases.
- **Interface behavior:** the tests use the real views without a display. They cover
  menus, editing, dropped files and saved results. When a window test fails, it writes
  details to `target/gui-tests/<test>.txt`.
- **Command-line behavior:** the tests in `tests/cli.rs` run the program and check exit
  codes, reports, patches and error messages.

The tests use their own settings folder, so they do not change your settings. For other
test runs that must not touch your settings, set `OXIDEMERGE_CONFIG_DIR` to a separate
folder. Some web tests start a local HTTP server, so the test system must allow programs
to listen on local network ports.

### Saved reports

The semantic, release summary and impact reports of fixed inputs must match the JSON and
XML reports in `tests/fixtures/reports`. Only the capture times may differ. If you change
a report on purpose, update the saved reports with this command, then review their
changes:

```sh
OXIDEMERGE_UPDATE_REPORTS=1 cargo test --test cli reports_match
```

### Format checks

This command runs the format checks only:

```sh
cargo test --release --locked format_regression -- --nocapture
```

They check that patches keep the byte order mark (BOM) and line endings, that ignored text
changes stay ignored, and that merging, saving and undoing work with mixed line endings.
They also check that the results work with Git checkout and Git patches, so Git must be
installed. Continuous integration runs these checks on Windows, Linux and macOS as part of
the full test suite.

### Random tests

When a random test fails, it prints a seed. Use the seed to run the same case again, or
run more cases:

```sh
OXIDEMERGE_TEST_SEED=<seed> cargo test regress
OXIDEMERGE_TEST_CASES=20000 cargo test --release regress
```

### Window tests

To test with real program windows, run `make gui-smoke` or
`scripts/gui-smoke.sh path/to/oxidemerge`. The script checks two-file, three-file and
folder comparisons. On Linux without a display, it starts Xvfb. If you set
`OXIDEMERGE_SMOKE_REPORT=<file>`, the program writes its tabs and messages to that file
after the comparison, and then closes.

For documentation pictures, `OXIDEMERGE_SCREENSHOT=<file.png>` saves the window as the
program draws it, without a screen grab, once the comparison is done, and then closes. Set
`OXIDEMERGE_SCREENSHOT_COMMAND` to a command's label key, such as `cmd.sync_folders`, to
run it first. Use `OXIDEMERGE_CONFIG_DIR` with a `config.json` that sets `language` to
take a picture in another language.

### Benchmarks

Benchmarks are not part of `cargo test`. For the commands, the method and the reference
results, see [Benchmarks](benchmarks.md).

## Continuous integration

The [CI workflow](../.github/workflows/ci.yml) runs for pushes to `main` and for pull
requests.

| Event | What runs |
| --- | --- |
| Pull request | Clippy, the full test suite, release builds, and command-line and window checks on Windows x64, Linux x64 and macOS on Apple silicon. |
| Push to `main` | The pull request checks, plus all packages and their checks. The packages are kept as build files. Nothing is published. |
| Push of a release tag | The same as a push to `main`. Then the release is published. |
| Manual run | The pull request checks. Turn on `packages` to build and check all packages. Turn on `stress_cases` or `benchmarks` for more random test cases or benchmarks. |

Some more details:

- The Windows and Linux window checks use Mesa's software renderer. Linux also uses Xvfb.
- When a test fails, its output is kept in a `test-output-*` file of the workflow run.
- A separate `cargo deny` job checks the dependencies. See [SECURITY.md](../SECURITY.md).
- The extra random tests and benchmarks are optional. Releases do not depend on them.

### Packages

Pushes to `main`, release tags and manual runs with `packages` build and check these
packages:

- **Windows x64 and Arm64:** ZIP packages with only `oxidemerge.exe`. Each program is
  checked for the right processor and is started.
- **Linux x64 and Arm64:** TAR.GZ packages built on Ubuntu 22.04. The program is started,
  and the check confirms that it does not change the settings.
- **Linux AppImage:** packages built on AlmaLinux 8 with glibc 2.28. They include the X11,
  Wayland and keyboard libraries and use them when the system does not have them. Both
  processor types are checked on Ubuntu 22.04 and AlmaLinux 8: library loading,
  command-line behavior and window start.
- **macOS:** a universal app in a DMG package, with an ad hoc signature. Both Apple
  silicon and Intel code are checked. The Intel program is tested with Rosetta.
- **Documentation:** the English and Traditional Chinese PDF guides. The build fails if a
  font is missing characters.

The Windows and Linux Arm64 packages are built and tested on Arm computers, not emulated.

## Releases

### Release rules

- Pushing a tag such as `v0.9.8` publishes the packages on GitHub Releases.
- The tag must match the version in `Cargo.toml`.
- `CHANGELOG.md` must have a section for that version.
- A tag that contains `-`, such as `v1.0.0-rc1`, is published as a pre-release.

The release notes are the matching changelog section, followed by download instructions.
The workflow reads the changelog but does not change it.

Before publishing, the workflow checks that the release does not exist yet. It also checks
that all expected packages exist, and no other files. Each release names the source commit
that it was built from, and GitHub lists the SHA-256 checksum of every file. For version
0.9.8, a release has exactly these files:

```text
oxidemerge-0.9.8-linux-arm64.AppImage
oxidemerge-0.9.8-linux-arm64.tar.gz
oxidemerge-0.9.8-linux-x64.AppImage
oxidemerge-0.9.8-linux-x64.tar.gz
oxidemerge-0.9.8-macos-universal.dmg
oxidemerge-0.9.8-user-guide.pdf
oxidemerge-0.9.8-user-guide.zh-TW.pdf
oxidemerge-0.9.8-windows-arm64.zip
oxidemerge-0.9.8-windows-x64.zip
```

### Publish a release

These steps use version `0.9.8` as an example. Use the version that you want to release.

1. Change `version` in `Cargo.toml`. Run `cargo build` to update `Cargo.lock`.
2. Check the `0.9.8` section in `CHANGELOG.md`. In its heading, replace `Unreleased` with
   the release date in the form `YYYY-MM-DD`.
3. Run `make fmt-check ci`. Check the release notes with `scripts/release-notes.sh 0.9.8`.
4. Commit, and push to `main`. Wait until the required CI checks pass on all platforms.
5. Tag the checked commit and push the tag. First make sure with `git log -1 --oneline`
   that `HEAD` is the commit that passed the checks.

   ```sh
   git tag -a v0.9.8 -m "OxideMerge 0.9.8"
   git push origin v0.9.8
   ```

6. When the workflow finishes, check the new GitHub release. Make sure that all packages
   and PDF guides are there.
7. Add an `Unreleased` section for the next version to `CHANGELOG.md`. Record later
   changes there.

### Fix a failed release build

The workflow publishes a release only after all build and test jobs pass. So if one of
these jobs fails for a release tag, nothing is published, and you can publish the same
version again from a fixed commit. These steps use `v0.9.8` as an example. Use the version
whose build failed.

1. Make sure that GitHub has no `v0.9.8` release:

   ```sh
   gh release view v0.9.8
   ```

   This command needs the GitHub CLI, signed in. It must answer `release not found`. You
   can also look for the release on the
   [Releases page](https://github.com/kuopinghsu/OxideMerge/releases). If the release
   exists, even with only some files, do not move the tag. See
   [Fix a published release](#fix-a-published-release) instead.

2. Fix the problem on `main`. Keep the version `0.9.8` in `Cargo.toml` and in the
   changelog. If the release date changes, update it in the changelog heading. Run
   `make fmt-check ci`, and check the release notes with `scripts/release-notes.sh 0.9.8`.
   Commit, and push to `main`. Wait until the required CI checks pass on all platforms.

3. Move the tag to the fixed commit, and push it again:

   ```sh
   git switch main
   git pull --ff-only origin main
   git log -1 --oneline
   git tag -a -f v0.9.8 -m "OxideMerge 0.9.8"
   git push --force origin v0.9.8
   ```

   `git log` must show the commit that passed the checks. The tag push starts a new
   workflow run for the fixed commit.

   Do not use **Re-run jobs** on the failed run of the old tag. That run builds the old
   commit, and it could publish the old packages under the moved tag.

4. When the workflow finishes, check the new GitHub release. Make sure that all packages
   and PDF guides are there.

If the build fails again, repeat steps 2 to 4. Other clones still have the old tag. To
update it there, run `git fetch --tags --force`.

### Fix a published release

Never change a release after it is published. Packages that people have downloaded must
always match the published checksums. The workflow also refuses to publish a version that
already has a release. Instead, publish a new patch version:

1. Fix the problem on `main`.
2. Follow the steps in [Publish a release](#publish-a-release) with the next patch
   version, for example `0.9.9` after `0.9.8`. Describe the fix in the changelog section
   of the new version.

Keep the old tag and the files of the old release.
