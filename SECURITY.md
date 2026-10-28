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
  outside it are skipped, and so are links that point outside the archive. An archive that
  unpacks to more than 8 GB or 500,000 files is refused. Extracted sides are read-only.
- **Web pages.** When a Chromium-based browser is found, the text, HTML source and screenshot
  modes hand the pages to it: it runs their scripts, headless, in a fresh profile that is deleted
  afterwards, with extensions and sync off. What such a page may load is up to the browser's own
  security, not the limits below. OxideMerge talks to the browser over a private pipe on macOS
  and Linux; on Windows, over a port on `127.0.0.1` that is open only while the pages load, which
  other programs on the same computer could reach meanwhile. Turn off *Render pages in the
  browser for text and HTML* to keep the text and HTML modes from running scripts.
  Pages OxideMerge downloads itself (the resource tree, and text and HTML without a browser) run
  no scripts. Such a web page may not load local files, and only a page on this computer or a
  private network may load resources from one or redirect to one. Responses are limited in size,
  count and time (see the user guide); the size counts what a compressed response unpacks to.
  Resource tree downloads are written only inside the folder you chose, whatever the addresses
  contain. A local page may load files from this computer, but not from another one
  (`file://host/share`).
- **Reports.** Every file name, path and line written into an HTML report, slideshow or XML
  report is escaped. Slideshows run only their own embedded script.
- **Language packs** added to a `Languages` folder: only text for known messages is used, and a
  translation that drops or adds a `{placeholder}` is replaced by the English text.

The following are **trusted**, because they configure what OxideMerge runs:

- **Your settings** (`config.json` in the configuration folder, or `oxidemerge.json` for
  portable use), including **unpacker plugins**: their command lines are run by the system
  shell. On Linux and macOS, file names are passed to the shell as arguments, never as part of
  the command, so a file name can't run commands. On Windows the command line names the path
  as an environment variable in double quotes (`"%OXIDEMERGE_FILE%"`): in quotes nothing in a
  Windows file name is special, and the name can't contain a quote to end them. Quotes written
  around `${file}` are dropped, as they would pair with these and leave the path unquoted.
- **Compare rules and filter files** in the configuration folder.
- **Project files** (`.oxmproj`, `.winmerge`) only name paths and compare options; they can't
  name commands.

OxideMerge also starts `git` for Git revisions and conflict files, the browser for web pages,
and the system's file opener when you choose *Open with associated application* or *Show in file
manager*. On Windows files are opened through the shell's file associations, and Explorer gets
the path to show in quotes; neither goes through `cmd`.

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
