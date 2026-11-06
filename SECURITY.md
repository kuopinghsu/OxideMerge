# Security

## Reporting a vulnerability

Please report security problems privately through
[GitHub's private vulnerability reporting](https://github.com/kuopinghsu/OxideMerge/security/advisories/new),
not in a public issue. Include the version (`oxidemerge --version`), the system, and the
steps or files that show the problem. Fixes are released as a new patch version; see
[CHANGELOG.md](CHANGELOG.md).

Only the latest release gets security fixes.

## What OxideMerge trusts

OxideMerge runs with your permissions and reads and writes the files you open. It treats
the following as **untrusted** and handles them without running anything they contain:

- **Files and folders you compare**, including their names. Compares only read them;
  nothing is written until you save, copy, delete or merge, and folder merges re-check
  every file before changing it (see the user guide).
- **Archives** (zip, tar, 7z) are extracted to a private temporary folder. Entries that
  would land outside it are skipped, and so are links that point outside the archive. An
  archive that unpacks to more than 8 GB or 500,000 files is refused. Edits are written
  back only into the archive they were extracted from, never next to it; signed packages,
  `.tar.xz` and archives nested in another archive stay read-only.
- **SFTP servers** you fetch from. The server's host key is checked before any password or
  key is sent: against your `~/.ssh/known_hosts` and OxideMerge's own `known_hosts` file
  next to its settings. A key that differs from one of the same type recorded in either
  file, or that `~/.ssh/known_hosts` revokes, stops the connection; a new server's
  fingerprint must be confirmed before it is trusted; and a `known_hosts` file, or a line in
  it, that can't be read stops every connection. Fetched names are checked to be plain file names before anything
  is written, and a fetch is limited in size, file count and depth. Uploads go to a temporary
  file that replaces the original, and are refused when the server's file changed since it
  was fetched. Passwords and passphrases stay in memory only, and are overwritten when no
  longer needed.
- **Web pages.** OxideMerge downloads the pages itself and never hands them to a browser,
  so their scripts never run. Such a web page may not load local files, and only a page on
  this computer or a private network may load resources from one or redirect to one.
  Responses are limited in size, count and time (see the user guide); the size counts what
  a compressed response unpacks to. Resource tree downloads are written only inside the
  folder you chose, whatever the addresses contain. A local page may load files from this
  computer, but not from another one (`file://host/share`).
- **Reports.** Every file name, path and line written into an HTML report, slideshow or
  XML report is escaped. Slideshows run only their own embedded script.
- **Language packs** added to a `Languages` folder: only text for known messages is used,
  and a translation that drops or adds a `{placeholder}` is replaced by the English text.

The following are **trusted**, because they configure what OxideMerge runs:

- **Your settings** (`config.json` in the configuration folder, or `oxidemerge.json` for
  portable use), including **unpacker plugins**: their command lines are run by the system
  shell. On Linux and macOS, file names are passed to the shell as arguments, never as
  part of the command, so a file name can't run commands. On Windows the command line
  names the path as an environment variable in double quotes (`"%OXIDEMERGE_FILE%"`): in
  quotes nothing in a Windows file name is special, and the name can't contain a quote to
  end them. Quotes written around `${file}` are dropped, as they would pair with these and
  leave the path unquoted.
- **Git configuration** (repository, user and system), including checkout/smudge filters.
  Git revision comparisons honor these filters, so configured filter commands can execute
  with your permissions. Repository attributes select configured filters; attributes alone
  do not define executable commands. Git LFS and custom filters may download content or
  otherwise access the network. Compare revisions only with Git configuration you trust.
- **Mercurial and Subversion configuration**, when you compare with a revision: Mercurial
  reads the repository's `.hg/hgrc`, whose hooks, extensions and decode filters can run
  commands, and Subversion its own configuration. The list of recent revisions, which
  *Compare with revision* reads by itself while you type a path, tells Mercurial to skip
  the repository's `.hg/hgrc` (Mercurial 5.4 or later), so browsing to a downloaded
  repository runs nothing from it. Only clicking *Compare* reads the repository with its
  configuration.
- **Compare rules and filter files** in the configuration folder.
- **Project files** (`.oxmproj`, `.winmerge`) only name paths and compare options; they
  can't name commands.

OxideMerge also starts `git` for Git revisions and conflict files, `svn` and `hg` for
Subversion and Mercurial revisions, `locale` on Linux to find a UTF-8 locale for `svn`,
and the system's file opener when you choose *Open with associated application* or *Show
in file manager*. On Windows files are opened through the shell's file associations, and
Explorer gets the path to show in quotes; neither goes through `cmd`.

OxideMerge has no application telemetry. Its own downloads are for the web addresses you
compare and the resources those pages name. Git LFS, configured Git filters and unpacker
commands can also access the network under their own configuration.

## Dependencies

[`deny.toml`](deny.toml) lists the licenses that may be shipped and the security
advisories that were reviewed, with the reason each one doesn't affect OxideMerge. CI runs
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) on every push and pull request,
and a release fails while any advisory is unreviewed. To check locally:

```sh
cargo install cargo-deny --locked
cargo deny check
```
