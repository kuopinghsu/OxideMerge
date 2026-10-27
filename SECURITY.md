# Security

## Reporting a vulnerability

Please report security problems privately through
[GitHub's private vulnerability reporting](https://github.com/kuopinghsu/OxideMerge/security/advisories/new),
not in a public issue. Include the version (`oxidemerge --version`), the system, and the steps
or files that show the problem. Fixes are released as a new patch version; see
[CHANGELOG.md](CHANGELOG.md).

Only the latest release gets security fixes.

## What OxideMerge trusts

OxideMerge runs with your permissions and reads and writes the files you open. It treats the
following as **untrusted** and handles them without running anything they contain:

- **Files and folders you compare**, including their names. Compares only read them; nothing is
  written until you save, copy, delete or merge, and folder merges re-check every file before
  changing it (see the user guide).
- **Archives** (zip, tar, 7z) are extracted to a private temporary folder. Entries that would land
  outside it are skipped, and so are links that point outside the archive. Extracted sides are
  read-only.
- **Web pages.** Text, HTML source and resource tree modes don't run scripts. A web page may not
  load local files, and only a page on this computer or a private network may load resources from
  one or redirect to one. Responses are limited in size, count and time (see the user guide).
  Screenshot mode hands the page to your Chromium-based browser, in a fresh profile, so the
  browser's own security applies there.
- **Reports.** Every file name, path and line written into an HTML report is escaped.
- **Language packs** added to a `Languages` folder: only text for known messages is used, and a
  translation that drops or adds a `{placeholder}` is replaced by the English text.

The following are **trusted**, because they configure what OxideMerge runs:

- **Your settings** (`config.json` in the configuration folder, or `oxidemerge.json` for
  portable use), including **unpacker plugins**: their command lines are run by the system
  shell. On Linux and macOS, file names are passed to the shell as arguments, never as part of
  the command, so a file name can't run commands. On Windows the path is put into the command
  line in double quotes, which Windows file names cannot contain.
- **Compare rules and filter files** in the configuration folder.
- **Project files** (`.oxmproj`, `.winmerge`) only name paths and compare options; they can't
  name commands.

OxideMerge also starts `git` for Git revisions and conflict files, the browser for screenshots,
and the system's file opener when you choose *Open with* or *Show in folder*.

OxideMerge doesn't collect or send anything. It connects to the network only for the web
addresses you compare and the resources those pages name.

## Dependencies

[`deny.toml`](deny.toml) lists the licenses that may be shipped and the security advisories
that were reviewed, with the reason each one doesn't affect OxideMerge. CI runs
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) on every push and pull request, and a
release fails while any advisory is unreviewed. To check locally:

```sh
cargo install cargo-deny --locked
cargo deny check
```
