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
  back only into the archive they were extracted from (and, for an archive nested in
  another, into the archives it came from), never next to it; signed packages and archives
  nested in them stay read-only.
- **SFTP and FTP servers** you fetch from. An SFTP server's host key is checked before any
  password or key is sent: against your `~/.ssh/known_hosts` and OxideMerge's own `known_hosts` file
  next to its settings, matching host patterns and hashed names as `ssh` does. A key that
  differs from the one OxideMerge remembered, or from one of the same type in
  `~/.ssh/known_hosts`, or that `~/.ssh/known_hosts` marks `@revoked`, stops the connection;
  a new server's fingerprint must be confirmed before it is trusted; a host certificate is
  trusted only when a `@cert-authority` line names its authority for the server, its
  signature checks out (Ed25519, ECDSA P-256/P-384, or RSA with SHA-2; SHA-1 and others are
  refused), it names the server, it is valid now and it has no critical options, and else
  its key is checked as a plain host key; and a `known_hosts` file, or a line in it for a key
  type that is checked, that can't be read stops every connection. Every jump host a
  connection goes through (`ProxyJump` in `~/.ssh/config`) has its key checked the same way
  before anything is sent to it. A `ProxyCommand` from `~/.ssh/config` is run as `ssh` would
  run it, by the shell, so a configuration file is trusted like a script; as with OpenSSH, a
  host or user name holding characters the shell would act on, or starting with `-`, is
  refused before the command runs. Questions a server asks while signing in are shown with
  the server's name and without terminal control characters, answers are not echoed unless
  the server says they may be, terminal echo is turned back on if the program is
  interrupted or stopped while asking, and answers are overwritten once sent. A password
  the server refused is never sent again, and an encrypted key with its `.pub` file beside it
  has its passphrase asked for only after the server has said it would take that key. Fetched names are checked to be plain file names before anything
  is written, and a fetch is limited in size, file count and depth. Uploads update the original
  file in place to preserve ownership and ACLs. Before writing, they check that its size and
  modification time (or, where the server gives no time, its contents) still match what was
  fetched. An interrupted write can leave partial contents; it is reported, and saving
  again finishes it. Passwords and passphrases stay in memory only, and are overwritten when no
  longer needed. An FTPS server's certificate is checked before the password is sent, either
  against the bundled certificate authorities or against the certificate the user confirmed
  and pinned in `known_certs` next to the settings; a changed pin, or a server that offers no
  TLS, stops the connection. A client certificate's private key is read from the file named
  and kept in memory only. Plain FTP is unencrypted, and the dialog says so. Passive FTP data
  connections always go to the address the sign-in reached, whatever a `PASV` reply names;
  in active mode, a data connection is accepted only from that address, and active mode is
  refused through a proxy. A proxy password is read from the dialog or the
  `OXIDEMERGE_PROXY_PASSWORD` environment variable (removed at startup), never from a proxy
  address on the command line or in a job, and is left out of error messages and the
  remembered connection. Writing a remote folder back checks each file on the server before
  overwriting or deleting it, and writes nothing when its local copy cannot be read in full,
  so what could not be read is never deleted on the server. On the command line and in job
  files, a password in a server address is refused (it would be visible to others); it is
  read from the `OXIDEMERGE_REMOTE_PASSWORD` environment variable instead, which is removed
  at startup so programs OxideMerge starts do not inherit it, and an untrusted server is
  trusted only with `--trust-fingerprint` naming its exact fingerprint.
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
