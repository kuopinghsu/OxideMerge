# Changelog

All notable changes to OxideMerge are listed here, newest first. Each release on GitHub
uses its section of this file as the release notes.

## [0.9.8] - Unreleased

### Added

- **Large comparisons open in the background:** text files that add up to 32 MB or more are
  read and compared on another thread. An *Opening* window names the file and counts the
  seconds while the rest of the window stays usable, and its *Cancel* button stops the work
  within a few milliseconds, even inside the line comparison of a 500 MB pair, without
  opening the tab.
- **Highlight limit:** *Options → General → Highlight files up to* (16 MB by default) sets
  the size above which a file is shown without syntax highlighting, and a message now says
  when that happens instead of the colors silently missing.
- **Release checks:** every release now publishes `SHA256SUMS`, written after each package
  is checked as a whole file of its kind (archives list and decompress, AppImage and disk
  image marks, PDF start and end). The macOS app can be signed with a Developer ID,
  notarized and stapled once the project's five `MACOS_*` secrets are set; without them it
  is still signed ad hoc, as before. Releases stay ad hoc and not notarized for now.
- **Translation checks:** tests now fail when a language names other keys than English,
  gives a key twice, or leaves a warning about losing work untranslated; when the
  Traditional Chinese guides lose a section or anchor of the English ones; and when a
  dialog button of the recovery prompt or Options leaves an 800 × 600 window in any language,
  or when an item of a menu or a control of an Options tab reaches past an 800 × 600 window,
  its menu or another control in any language. On macOS, each disk image built is installed
  as a download would be, judged by Gatekeeper, run and removed again
  (`scripts/macos-install-check.sh`).
- **Recovering unsaved work:** with *Options → General → Keep unsaved work for recovery*
  turned on (off by default), OxideMerge keeps a snapshot of its open comparisons and the
  text of unsaved edits in a private `recovery` folder of its settings, every 5 seconds while
  there are edits. After a window that did not close normally, the next start offers
  *Restore*, *Show details*, *Discard* or *Later*. Restoring reopens the comparisons with the
  unsaved text back in their sides, still unsaved, and names the files that changed since;
  it never writes a compared file. The text cursor's line and the bookmarks come back too.
  Snapshots keep at most 32 MB of unsaved text, five snapshots at most are kept, and one
  written by a later version is not restored. *Keep for recovery* in a tab's context menu
  leaves that one comparison out.
- **Compare over SFTP and FTP:** *File → Compare over SFTP/FTP...*, and a "Browse a server
  over SFTP/FTP" button beside every path on the start page and the *Open* dialog, connect
  to a server over SFTP, FTPS (explicit or implicit TLS) or plain FTP by host, port, user,
  and a password, a private key (with an optional passphrase) or, for SFTP, the keys
  `ssh-agent` holds, then fetch a file or folder to compare like any local path - open it
  twice to compare two remote locations, or once against a local one. The remote path can
  be typed or chosen from a list of the server's folders and files, opened from a button
  beside it: click a folder to open it or a file to choose it, or double-click a file to
  fetch it. The list keeps one connection open while it is shown. A later connection
  starts from the host, port, user and key path of the last one that succeeded, so
  fetching the other side of a compare from the same server does not need them retyped;
  the password or passphrase is never remembered this way. A fetched file is
  writable: *Save* re-uploads it to the server over a fresh connection, in the
  background with a *Cancel* button. Over SFTP it uploads a staged copy beside the file
  first, with the original's owner, group and permissions. It reads the copy back to check it,
  and renames it into place only once complete, so an interrupted save leaves the server's file
  as it was. *File → Save remote files in place* writes over the file directly instead, keeping
  access-control lists and hard links. FTP, a file owned by someone else, and a folder no file
  can be created in always use that in-place write. You can cancel before writing starts. An upload is refused
  when the file changed on the server since it was fetched, and the tab stays unsaved until an
  upload succeeds; closing it or quitting asks first. A fetched folder is writable too: *Save*
  uploads the files that are new or changed, creates new folders and deletes what was
  removed, but leaves alone and reports any file that changed on the server since the
  fetch; where the server gives no modified times (FTP without `MDTM`), a file's contents
  tell. An upload that stopped part way says so, and saving again finishes it. A local copy
  that is gone or cannot be read in full saves nothing, rather than deleting on the server
  what could not be read. Of names on the server this computer cannot keep apart (differing
  only in case, say), only the first is fetched, and no `.bak` backups are made inside a
  fetched copy. New files are created with the usual permissions (644, less the server's
  umask). *File → Reload from server* fetches the server's copy again, replacing the tab's
  unsaved edits of it once confirmed. A fetch is bounded to
  4 GiB, 50,000 files and folders and 64 levels deep so a very large folder cannot be
  fetched by mistake. Plain FTP is marked in the dialog as sending the password and files
  unencrypted. An FTPS server's certificate must be vouched for by a certificate authority,
  or else its fingerprint is shown to confirm, as for an SSH host key, and the certificate is
  then pinned in a `known_certs` file next to the settings. The server's host key is checked before the password or
  key is sent: a server whose key matches your `~/.ssh/known_hosts` (host patterns and
  hashed names included) is trusted if it also matches any OxideMerge pin. A new server shows
  its fingerprint and is trusted only once you confirm it, and is then remembered in
  OxideMerge's own `known_hosts` file next to
  its settings (two windows trusting servers at once both keep theirs); a key that differs from the one remembered there, or from one of the same
  type in `~/.ssh/known_hosts`, or that is marked `@revoked`, stops the connection instead
  of silently trusting it, and so does a `known_hosts` file that can't be read. A host
  certificate is accepted from a server a `@cert-authority` line names an authority for (see
  below). The connection,
  including the password or passphrase, is kept only in memory for the tab's lifetime; it
  is never written to the settings file, and fetched tabs are left out of saved
  workspaces, the restored session and the recent paths.
- **Server paths on the command line and in jobs:** a path can be
  `sftp://[user@]host[:port]/path`, `ftp://`, `ftpes://` (explicit FTPS) or `ftps://`
  (implicit FTPS), with `/~/` for the home folder. SFTP signs in with `--ssh-key FILE`,
  a password, `ssh-agent`, or the keys `~/.ssh/config` names; the password or passphrase comes from the
  `OXIDEMERGE_REMOTE_PASSWORD` environment variable (removed at startup, so programs
  OxideMerge starts do not inherit it), and one written in the address is
  refused. A server not trusted yet is trusted with `--trust-fingerprint SHA256:...`. What
  a run changes in a fetched copy, such as a `sync` job's destination, is written back to
  the server afterwards, with the same checks as *Save*; a synchronization that stopped
  part way (exit code 5) is undone in the copy instead, and nothing is written back. A `--sync-plan` made for a folder
  on a server names it by its address, so `--sync-apply` applies it to the copy fetched
  then. Job files take `ssh_key` and `trust_fingerprint`.
- **SSH configuration, jump hosts, proxies and more ways to sign in:** an SFTP server is
  reached as `~/.ssh/config` (and `/etc/ssh/ssh_config`) says, the way `ssh` reaches it:
  a `Host` alias, with its `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`,
  `HostKeyAlias` and `UserKnownHostsFile`, `Include`, and `Match` blocks on `host`,
  `originalhost`, `user`, `localuser` or `all` (never `exec`, which would run a command). The user name is optional, as with `ssh`. A server behind `ProxyJump` jump hosts
  (up to 8, each signing in with the SSH agent's and the configuration's keys) or a
  `ProxyCommand` is reached through them; each jump host's key is checked like the server's.
  A server that asks questions while signing in (keyboard-interactive: a one-time code, a
  second factor, a password change) has them put to you in a window, or on the terminal for
  the command line, and an encrypted private key's passphrase is asked for when none was
  given; when the key's `.pub` file is beside it, only once the server has said it would
  take that key. A password the server
  refused is not sent again, what a server writes is shown without terminal control
  characters, and a hidden answer's terminal echo comes back if the command line is
  interrupted or stopped (Ctrl+C, Ctrl+Z, a closed terminal). A host or user name with
  characters a shell would act on (`` ` ``, `$`, `;`, `|`, quotes, spaces and the like), or
  starting with `-`, is refused before any `ProxyCommand` runs. A host certificate is checked against the `@cert-authority` lines of the
  known hosts files for the server: signed by that authority (Ed25519, ECDSA P-256/P-384 or
  RSA with SHA-2), naming the server, valid now, and with no critical options; an authority
  or key marked `@revoked` stops the connection. Under the connection dialog's *Advanced*
  settings, and with `--proxy URL` on the command line or `proxy` in a job, SFTP and FTP
  both connect through a SOCKS5 (`socks5://[user@]host:port`) or HTTP CONNECT
  (`http://[user@]host:port`) proxy, every FTP data connection included; a proxy password
  comes from the dialog or the `OXIDEMERGE_PROXY_PASSWORD` environment variable, never from
  the address on the command line. FTP can use active mode (*Active mode*, `--ftp-active`,
  `ftp_active`): `EPRT`, or `PORT` for a server without it, accepting the data connection only
  from the server's address; it cannot be used through a proxy or from behind NAT. FTPS can
  present a client certificate (`--client-cert FILE` and `--client-key FILE`, or
  `client_cert` and `client_key`) in PEM, key and certificate in one file or two.
  Checked on Linux with OpenSSH 10.5p1 and vsftpd 3.0.5: SFTP with a private key refuses an
  untrusted server until `--trust-fingerprint` confirms the fingerprint `ssh-keygen -lf` prints,
  then an identical file exits 0 and a different one exits 1, a folder compare names the changed
  file, and the window opens the file as writable. A sync job uploads a new file. Uploading a
  shorter file replaces it in place, and an upload is refused when the server file has changed
  since the fetch. Anonymous FTP used a vsftpd process running as the current user, with no
  chroot: an identical file exits 0, a different file exits 1, the window opens it as writable,
  a sync job uploads a file and a second run updates it, and `--ftp-active` compared an identical
  file (exit 0). An address that contains a password is refused.
- **Workspaces:** *File → Workspaces → Save as workspace...* saves the open tabs under a
  name, kept until deleted; *File → Workspaces* then lists every saved workspace to reopen
  all of its tabs at once, alongside whatever is open already, and *Manage workspaces...*
  lists them to open or delete. Unlike the one, unnamed session the app already reopens on
  its own at startup, a workspace is kept and reopened only by name, whenever asked.
  Saving over an existing name asks first, a workspace is never saved empty, a deleted
  one can be brought back with *Undo*, tabs whose files are gone are counted rather than
  reopened against an empty side, and tabs of temporary copies (SFTP, revisions, files
  inside an archive compare) are left out. Another OxideMerge window running at the same
  time no longer writes its older list of workspaces over one just saved.
- **Compare clipboard:** *File → Compare clipboard* opens a new text compare with the
  clipboard's current text on the left and a blank, writable right side: paste a second
  text into it (`Ctrl+V`) to compare the two right away, with no files needed. Both sides
  edit normally; *Save* works like any other unsaved comparison and asks where to write
  the text, since there is no file to write back to.
- **Jupyter notebooks (`.ipynb`) read as text:** a notebook is compared cell by cell, each
  one under a line naming it a code cell, a Markdown cell or a raw cell (unnumbered, so an
  added cell doesn't relabel the ones after it), with that cell's own source and nothing
  else; outputs, execution counts and other metadata a notebook carries from being run are
  left out, so re-running a notebook without changing it compares equal. nbformat 3
  notebooks are read too; one that can't be read is compared as its JSON text. Read through
  the same `extract_documents` setting as the other built-in document readers, and
  read-only, like them; `git mergetool` (`-o`) still merges notebooks as the files they are,
  so the merged result stays valid. A saved review of a notebook shows it as changed once,
  since what it shows is now the cells. Sample: `samples/*/docs/report.ipynb`.
- **Archive sides are writable:** a file extracted from an archive compare (ZIP-based
  formats, TAR plain or compressed with gzip, bzip2 or xz, or 7z) can now be edited, copied
  to, merged into or deleted, like a plain folder. Saving a file, or *File → Save* / *Save
  All* on the folder compare, repacks the archive in place, keeping its permissions and
  every entry you didn't change exactly as it was (date, compression, order; in a 7z, every
  compressed block holding only such entries is copied as it was, and other blocks are
  compressed again with LZMA2, keeping their entries' dates and attributes); an archive
  with no changes is not rewritten. An archive inside another extracted archive can be
  edited too: saving it also writes the archive it came from, out to the outermost one,
  and an open compare of that archive is refreshed. A `.tar.xz` or 7z archive, slow to
  compress, is written in the background while a window shows it is being written; closing
  or quitting waits for it. Saving is refused when the archive was
  changed on disk since it was extracted, or is read-only, and no `.bak` backups are made
  inside it. An archive fetched over SFTP is uploaded again after it is saved. A review
  snapshot or a page tree compared from the web stays read-only, as before, since there is
  nowhere for its changes to go back to, and so do signed packages (`.apk`, `.ipa`,
  `.appx`, `.msix`, `.crx`, `.xpi`, or a zip with a `META-INF` signature) and archives
  inside them.
- **Password-protected archives, every ZIP compression method and `.tar.zst`:** a ZIP
  encrypted with ZipCrypto or WinZip AES, or a 7z encrypted with AES (its file names too or
  not), asks for its password in an *Archive password* window when it is opened, again if
  the password was wrong. Saving keeps it encrypted: changed and new entries are encrypted
  with the same password the way the archive was, unchanged AES entries and 7z blocks are
  copied as they were, and a 7z with encrypted names keeps them encrypted. The password is
  kept only in memory while the compare is open and is overwritten there afterwards. ZIP
  entries compressed with bzip2, LZMA, xz, Zstandard, PPMd or Deflate64 are now read, and a
  changed one keeps its method (Deflate for LZMA and Deflate64, which can only be read).
  `.tar.zst` / `.tzst` archives open and save like the other TAR formats, and `.xws` files
  open as ZIP archives.
- **Moved blocks in binary comparisons:** *Moved blocks* on the binary toolbar highlights
  identical bytes at different offsets, in the hexadecimal and text columns and in the
  location pane: data shifted up to 128 bytes by an insertion or deletion, and blocks of
  16 bytes or more moved farther or swapped. The toolbar shows how many it found, and *Go
  to matching block* jumps from a selected byte to the same byte on the other side. Two
  and three files are supported. Detection runs in the background and again after edits,
  undo and redo; an edit or closing the comparison stops it at once. The differences,
  copying and saving are unchanged. The checkbox is remembered. Large files are sampled,
  with up to 1,024 blocks for each pair. Sample: `samples/*/bin/firmware.bin`.

- **Tab and Shift+Tab indent in the text cursor:** `Tab` indents the selected lines when
  the selection goes over more than one line or is a whole line, and otherwise still types
  a tab; `Shift+Tab` outdents the selected lines or the cursor's line, and `Ctrl+]` and
  `Ctrl+[` do the same with the cursor in a pane. Indenting uses a tab in files indented
  with tabs and the tab size in spaces otherwise, leaves blank lines alone, and skips a
  line the selection only reaches the start of. The selection moves with its text, so
  `Shift+Tab` right after `Tab` gives back the same text and selection, each as one undo
  step.
- **Triple-click selects a line:** a triple-click in the text selects the line with its
  line break (the last line to its end), as the keys `Home` and `Shift+↓` do.
- **Fast typing in large files:** typing within a line no longer compares the files again
  at every key when that takes long (over 8 ms; about 35 ms for 200,000 lines): it waits
  for a 0.3 s pause or for the next key, click or command that is not typing, so the keys
  of a burst take about 2 ms at any size. The first key of a burst also keeps the text for
  *Undo*, which takes longer in files of millions of lines, and skips moving bookmarks and
  the view, which a change within a line cannot move. A new line, a paste and undo still
  compare at once.
- **Several text cursors:** Ctrl+click (⌘-click on macOS) adds a text cursor in a pane or
  removes one, and `Ctrl+Alt+↑`/`Ctrl+Alt+↓` (`⌘⌥↑`/`⌘⌥↓`) add one on the line above or
  below. Typing, deleting, Enter, the arrow keys, Home and End act at every cursor, and
  cursors that meet merge. Each edit is one undo step, and undo puts the cursors back.
  Pasting as many lines as cursors puts one at each, copying gives the selections one per
  line, and Tab, Shift+Tab, *Duplicate lines* and *Move lines* act on every cursor's lines
  once. `Esc` first goes back to one cursor. The column selection stays as it was,
  separate from these cursors.
- **Duplicate and move lines:** `Ctrl+D` (`⌘D` on macOS) puts a copy of the selected
  lines, or the text cursor's line, under them and selects the copy; `Alt+Shift+↑` and
  `Alt+Shift+↓` move them up or down with their selection. They take the same lines as
  `Tab` (or, without a text cursor, the active pane's lines in the selected rows), are one
  undo step each, and are in the *Edit* menu and the command palette. `Alt+Shift+↓` and
  `Alt+Shift+↑` no longer also step through the differences; `Alt+↓` and `Alt+↑` still do.
- **Where typing goes:** the text cursor is a solid bar only where typing goes into the
  text; it is thin and faint in a read-only pane, while a field such as the search bar has
  the keyboard, and while the window is in the background. Typing into a read-only pane
  says which side it is and that the lock in its header unlocks it, or, for text read from
  a document, that it can only be compared.
- **Edit side by side from the inline view:** the inline view says at its top that it only
  shows the files, with an *Edit side by side* button; it, a double-click on a line,
  `Enter` or `F2` opens the side-by-side view with the text cursor at the start of that
  line. Switching views keeps every edit, the selected difference and the text cursor with
  its selection.
- **Word moves and word deletes in the text cursor:** `Ctrl+←` and `Ctrl+→` (`Option+←`
  and `Option+→` on macOS) move by word, with `Shift` to select by word, and
  `Ctrl+Backspace` and `Ctrl+Delete` (`Option+Backspace` and `Option+Delete`) delete the
  word before or after the cursor as one undo step, or the selection when there is one.
  Words follow Unicode word boundaries, so accents, emoji sequences and Chinese, Japanese
  and Korean characters stay whole; letters, digits and `_` make one word and a run of
  punctuation is its own. On macOS the cursor takes `Option+←` and `Option+→` while it is
  in the text, so they no longer copy the difference there; `Esc` leaves the text.
- **Tree view:** *Tree*, beside *Text* and *Semantic* for two JSON, YAML, TOML or XML
  files, shows both documents as one aligned tree: matching nodes share a row, a node on
  one side only has a placeholder, and `+ − ~ ↕ ?` mark added, removed, modified, moved
  and ambiguous rows. Object members and attributes are paired by key or name, so key
  order, flow or block style, quoting and number spelling (`8_080`, `0x1F90`) are not
  changes, and a change of type is shown as such. Array elements and XML siblings are
  aligned, so inserting one is one addition rather than a change at every later position,
  and a reorder is a move. Next/previous difference visits each change once in tree order
  and unfolds collapsed parents; folded rows count the changes inside them under the
  current filter. The details pane shows each side's path, pairing, line and column and
  the source lines as written, and *Show source* opens the Text view there. Objects of an
  array are paired by a unique `id`, `name` or `key` member when every element has one,
  and the toolbar says so; key rules (`servers@name`, `spec/ports@name`, `*@id`,
  `book@id+lang`, `catalog/book@id`) pair other repeated entries and report reorders as
  moves, and duplicate or missing keys warn and fall back conservatively. Search by key,
  name or value, by a `$.path` prefix or with an XPath subset, and *View as collection*
  tables arrays of objects and repeated elements. Read-only, built in the background with
  *Cancel*, within the semantic parsers' limits. Sample: `samples/xml`.
- **Documents, spreadsheets and programs read as text**, with no other program to install:
  PDF is compared as the text of its pages, read through the file's own tables and turned
  back into characters with the `ToUnicode` table it carries for each font, so a page
  reads as it does on screen; an encrypted file says so rather than coming out as nothing,
  and a file whose tables are damaged is still read by looking for the objects themselves.
  Word (`.docx`), OpenDocument text (`.odt`) and Rich Text (`.rtf`) are compared as the
  words of the document, with the cells of a table separated by tabs; Excel (`.xlsx`,
  `.xlsm`) and OpenDocument spreadsheets (`.ods`) as their sheets, which opens as a
  **table compare** with the columns and the key matching it offers (a workbook of several
  sheets is compared as text); and ELF programs, shared objects and object files as their
  header, program headers (type, offset, addresses, file and memory sizes, alignment,
  `RWE` flags and the interpreter), section table and the symbols of `.symtab` and
  `.dynsym` sorted by name, with address, size, type, binding, visibility and section, so
  two builds line up function by function. Assembler mapping symbols (`$x`, `$d`, …) are
  left out, and at most 100,000 symbols a table are listed. A folder compare by contents
  still compares programs by their bytes. Text a PDF draws through forms is read too, and
  a PDF whose streams or fonts cannot all be read ends with a line saying the text is
  incomplete. The file itself is never written back: a compare read this way is read-only,
  and the pane says what was read. A converter under *Options → Plugins* is used in
  preference, so a better one can always be put in front, and *Read documents,
  spreadsheets and ELF files as text* there turns this off to compare such files as the
  archive or the bytes they are.
- **Formatted documents:** *Formatted*, beside *Text* above the panes, shows two or three
  Word documents, Excel workbooks (`.xlsx`, `.xlsm`) or tagged PDF files as formatted
  Markdown, block by block side by side, each changed block highlighted with its changed
  words. Headings, paragraphs with their bold, slanted and struck-through words, numbered
  and nested lists, and links keep their form. Tables are drawn as tables, with merged
  cells spanning the rows and columns they cover, so a row added inside a merged cell
  shows where it is. Each sheet of a workbook is a heading and a table, its dates, amounts
  and percentages shown as their number formats show them, runs of empty rows shown as
  one, and the pictures placed on it listed with their cells. An image or figure is shown
  with its description and a fingerprint of its picture, so a changed picture shows as a
  change. A PDF is read from the tags that most word processors write, which name its
  headings, lists, tables and cell spans; its running headers and page numbers are left
  out, and an untagged PDF says so. Next and previous difference step through the changed
  blocks. Fills, fonts, borders, formulas and charts are not compared. Samples:
  `samples/*/docs`.
- **Merged cells in web pages:** the Formatted web mode draws a table cell that spans rows
  or columns (`rowspan`, `colspan`) across the cells it covers, instead of shifting the
  cells after it into the wrong columns. Sample: `samples/*/web/handbook.html`.
- **Italian** interface language (`--lang it`, or *Italiano* in *Options → General*).
- A toolbar button in folder compares, and `Alt+P`, show or hide the *File compare below
  the folder list*.
- **Clear recent comparisons:** *Clear* beside the start page's *Recent* heading and *File
  → Open recent → Clear recent comparisons* empty the list, with *Undo* in the message
  that follows.
- **Binary editing:** click a byte in a binary compare and type hexadecimal digits, or
  text in the text column. *Insert* (or the `Insert` key) inserts bytes instead of
  replacing them; `Delete` and `Backspace` delete bytes; the arrow, page, `Home` and `End`
  keys move the cursor; pasting writes hexadecimal bytes in the hexadecimal column and
  text in the text column. Typing until the cursor moves is one undo step, and read-only
  sides refuse edits.
- **Table rows matched by key:** in a two-way CSV or TSV table compare, *Match rows by →
  Key columns* pairs records by one or more key columns instead of by position, *Columns
  by → Header name* pairs reordered columns, and *Numeric tolerance* (`0.01` or `1%`)
  counts close numbers as equal. A summary counts added, removed, modified, reordered and
  ambiguous records; next and previous difference go through the differing records;
  copying a cell or record uses the matching record and paired column, and records with an
  empty or duplicate key are never copied. A missing key column falls back to matching by
  position and says why. *Rows in* lists the matched rows in left, right or key order.
  Tables of more than 100,000 records are matched in the background with progress and
  *Cancel*. HTML and XML reports record the keys, column pairing, row order, tolerance,
  counts and warnings (a `<table-match>` element in XML). Projects save the settings.
- **One way through the differences of a folder:** next and previous difference go through
  the file compare below the folder list and on to the first (or last) difference of the
  next (or previous) changed file, in the order of the list, and *All differences* opens
  at the difference selected there. The status bar shows *File 2 / 5 · Difference 3 / 7*.
  Keys pressed while a file loads are taken once it is shown, a file that cannot be read
  offers *Retry* and *Skip*, and there is no wrapping around at the ends. In three-way
  folder compares, next and previous conflict go on to the files changed on both sides,
  passing over those that merge without conflicts, also from a tab opened from the folder
  compare. The navigation buttons' tooltips say what they go through and that they stop at
  the end, and the message at the end says when the folder compare was stopped before it
  finished.
- **Folder synchronization:** *Synchronize folders...* in a two-way folder compare
  (toolbar and *Merge* menu) previews mirroring one folder into the other, or updating it
  without deleting. The read-only preview lists each copy, replacement, deletion, skip and
  conflict with its reason, groups a copy and a deletion of the same contents as a
  *Rename* and applies it by moving the file in the destination (through a temporary name
  the journal records, so even a crash part way is undone), also for a rename only in
  letter case or accents on file systems that ignore them; warns when the filter leaves
  items out, and exports the plan as versioned JSON with relative paths, the filter and a
  fingerprint of the folder paths, so it is applied only to those folders. Applying asks
  for confirmation (twice for large deletions), checks every file against its SHA-256
  fingerprint first, so a file edited after the preview is never overwritten, copies
  through a verified temporary file, shows the item being changed, and keeps replaced and
  deleted files in a recovery journal outside both folders. A failure or *Stop* undoes the
  changes; a synchronization cut short is offered for undoing at the next start.
  File/folder clashes, folders renamed only in case and other names that differ only in
  case or in how accents on Latin letters are written are left alone, and so are symbolic
  links unless *Copy symbolic links as links* (`--sync-links`, `"copy_links"` in a job;
  macOS and Linux) is on: a link is then made, replaced or deleted as the link itself,
  with its target text compared, never followed, and journaled like a file so undoing puts
  it back; a link facing a file or folder stays a conflict. Folders inside each other are
  refused. On the command line: `--sync-plan mirror|update`, `--sync-apply <PLAN> --yes`
  and `--sync-recover`, with exit codes 4 (undone) and 5 (recovery required). A plan whose
  files or journal copies do not fit in the free space of their drives is refused before
  anything changes (on macOS and Linux; Windows does not measure free space), and the JSON
  result has a `problem` (`stale`, `unsupported`,
  `read_failure`, `write_failure`, `canceled`, `recovery_needed`) when it is not a plain
  success.
- **Edit state in the status bars:** the bar under each text pane shows *Writable*,
  *Read-only* (the pane is locked), *Read-only on disk* (the file cannot be written; save
  it under another name) or *Compare only* (text read from a document, spreadsheet, PDF,
  program or web page, or a formatted copy), and *Modified* while it has unsaved changes.
  The window's status bar shows the same for the active pane, and *Compare only* for the
  Tree and Formatted views and web pages shown as Markdown. A compare-only pane can no
  longer be unlocked, so its text cannot be saved over the file it was read from.
- **Editing at a cursor:** the side-by-side panes edit like a text editor. Click to place
  the cursor and type; Enter splits the line (keeping its indentation), Backspace and
  Delete join lines, and paste puts any number of lines at the cursor. Drag, Shift+click,
  Shift+arrows, double-click (a word) and Ctrl+A select text, which typing replaces; copy
  and cut take the selection or the whole line. Input methods compose at the cursor.
  Alt+drag (Option+drag on macOS) makes a column selection: typing, Backspace, Delete,
  copy, cut and paste work on the same columns of every line. Each action is one undo step
  (typing in a line is one until the cursor moves), and saving keeps each file's line
  endings, encoding and byte order mark.
- **PDF reports and printing:** *File → Export → PDF report...* and `--report out.pdf`
  write the report of a text or folder compare as a PDF, with no browser: the compared
  files, rules, counts and every difference with its context, line numbers and changed
  words, side by side or in one column (the inline view, `--unified`), with a page header
  that carries the app icon, page numbers, a footer whose *OxideMerge* links to the
  project page, and a bookmark for each file and difference. A folder report lists the
  files, then with `--report-diffs` (always from the menu) the differences of each changed
  text file, review notes and the review summary. The fonts are embedded as subsets, so
  Chinese, Japanese and Korean print. A sign before each changed line, underlined words
  and hatched gaps keep the report readable in black and white; the color-blind colors
  apply. *File → Print...* (Ctrl+P) opens the PDF in the system's viewer. Options:
  `--page-size a4|letter` (or *US Letter pages* in the menu), `--portrait`, `--whole-file`
  and `--context`; job files take the same fields.
- **Jobs:** `--job <FILE>` runs the compares, reports, sync plans and syncs listed in a
  versioned JSON job file (`.oxjob`) in order, each as its command line would, and prints
  a JSON summary whose exit code is the worst of the jobs. A job without a `kind` is a
  compare. Paths are relative to the job file, `${env:NAME}` in paths is the only
  expansion, and no shell or other program is run. `--job` takes no other options but
  `--yes` and `--lang`: each job's options are in the file. A sync job needs
  `"apply": true` in the file and `--yes`, and a large deletion also
  `"allow_large_deletion": true`. A report or patch written outside the job file's folder
  also needs `--yes`, and a report never replaces a link or a special file. The jobs after
  a failure are skipped unless `"continue_on_error": true`; unknown fields and newer
  versions are refused. *File → Projects and jobs → Save as job...* saves a folder
  comparison as a compare, sync plan or sync job, with its folders, filter, method, rules
  and options, and paths absolute or relative to the job file; a comparison of a revision,
  an archive or a web page, whose folders are temporary, cannot be saved.
  *File → Projects and jobs → Run job...* runs a job file
  as `--job` does, with your saved settings rather than this window's changes, and lists
  each job's result, with the report and the compare a click away. Before it runs, it
  lists the folders a sync job changes, the files written outside the job file's folder
  and any large deletion the file allows, and runs only after you allow it, each time.
  *Stop* stops a synchronization being applied, undoing what it changed, and skips the
  jobs after it; while a job is synchronizing, OxideMerge waits for it before it quits.
- **Copy values in the semantic view:** *Copy value to left* and *Copy value to right* on
  each JSON, XML, TOML or YAML change replace, add or remove that one value on the other
  side. Only the value's text changes; comments and layout stay. The edited file is parsed
  again and the copy is kept only when it has exactly the expected structure; otherwise
  nothing changes and the view says why. Moves are not copied. Each copy is one undo step.
- **Three-way semantic comparison:** in a three-way compare, *Semantic* lists each path
  that the left or right file changed from the middle one as *Only left differs*, *Only
  right differs*, *Both differ the same way* or *Conflict*, with a filter for each. *Use
  left* and *Use right* copy that side's value into the middle pane. An array that both
  sides changed, where elements pair ambiguously, is one conflict for the whole array.
  `--semantic left base right` writes the same report, with `state` per path.
- **Structured files in folder compares:** *Treat JSON, XML, TOML and YAML files with the
  same structure as identical* in *Options → Folder* (or `--structured`) counts such files
  as identical in the *Full contents* method when only key order, layout or comments
  differ.
- **Subversion, Mercurial and Perforce:** `-L <title>` (`--label`) titles the sides in
  order, as `diff -L` does, and `-u` is accepted and ignored, so
  `svn diff --diff-cmd oxidemerge` shows revision names in the pane headers; `--dl`,
  `--dm` and `--dr` take precedence. Subversion's `merge-tool-cmd` and Perforce's
  `P4MERGE` can be `oxidemerge` itself. Five files whose merged file is the working copy are
  read as Subversion's order (base, theirs, mine, merged, working copy), and four whose base,
  theirs and result carry the names p4 gives its temporary files (`t1t2t3.tmp` on Windows,
  `tmp.1.2.3` on macOS and Linux) as Perforce's (base, theirs, yours, result). Any other four
  or five files are still refused as too many paths, so a pattern such as `*.txt` never
  starts a merge that would save over one of them. The left pane is mine or yours, the
  middle is the base and the right is theirs, and the middle pane is saved to the result. If
  that file is unchanged when the window closes, the program exits with code 1 and the
  conflict stays unresolved. Any other option, including `-o`, uses the arguments as written,
  so an existing wrapper still works. Checked on Windows with SlikSVN 1.14.2, Mercurial
  7.1.2 and p4/p4d 2026.1, and on Linux with Git 2.55.0, Subversion 1.14.5, Mercurial 7.2.4
  and p4/p4d 2026.1 (3089746): `git mergetool`, `svn update --accept launch`, `hg merge` and
  `p4 resolve` against local repositories and a local server, both saved and closed without
  saving. On Linux, closing a `p4 resolve` without saving is reported as *Execution Failed*
  and *No such file or directory*; that is exit code 1. P4V keeps its own argument string.
  On Linux, P4V 2026.1 (2933292) was checked with it: diff opens the depot revision on the
  left and the workspace file on the right, saving then Accept Merged resolves the file, and
  closing without saving leaves P4V's markers and the file unresolved. The same P4V diff,
  and accepting a merge after saving, were tried earlier on macOS with P4V 2026.3 and were
  not repeated on Windows.
- **Compare with revision in Subversion and Mercurial:**
  *File → Compare with revision...* (formerly *Compare with Git revision...*) and `--rev`
  (`--git-rev` still works) also take Subversion and Mercurial working copies: the nearest
  `.git`, `.svn` or `.hg` above the path decides. Subversion revisions are `BASE` (the
  default), `PREV`, `HEAD`, a number, `r1234` as `svn log` prints it, or a date; the
  dialog lists the recent revisions from `svn log --xml`. On Windows, that `svn` keeps
  only ASCII arguments, so for a path that is not ASCII it is started in the folder, whose
  name it receives whole: a folder is given to it there as `.`, and a file by its
  repository URL pinned at the working copy's revision, so an older revision is found even
  when the file had another name then, as for an ASCII path. `BASE` of such a file is the
  pristine copy with the line endings its committed `svn:eol-style` gives it, as `svn cat`
  does, or comes from the server when it was committed with `svn:keywords`. Mercurial
  revisions are `.` (the default), `.^`, `tip`, a number, a hash, a tag or another revset,
  read from the local repository. On Windows, Mercurial is left on the ANSI code page:
  forcing UTF-8 makes every name that code page can spell fail, and a name it cannot spell
  is one `hg` itself cannot open. The error messages that `svn` and `hg` print in that code
  page, in the language of Windows, are shown readably instead of garbled. The list of
  recent revisions, read while you type a path, skips the repository's own `.hg/hgrc`, so
  browsing to a downloaded repository runs none of its hooks; only *Compare* reads it. A
  Subversion server reached over `svn+ssh` that would ask for a password fails instead of
  waiting for an answer that cannot come. With Git and Mercurial, a file whose name is not
  valid UTF-8 is refused rather than read under a changed name. Checked on Windows with the
  same SlikSVN and Mercurial versions, including a Subversion file whose name is not ASCII,
  and on Linux with Git 2.55.0, Subversion 1.14.5 and Mercurial 7.2.4, including a
  Subversion path that is not ASCII (passed to `svn` as itself) and a Mercurial path with
  brackets.
- **Docker test environment for Linux:** `scripts/docker-test.sh` (`make docker-test`)
  checks Linux from any computer with Docker. It runs Clippy, the tests, a release build and
  the command-line and window checks. It also runs tests against real servers: OpenSSH (keys,
  passphrases, host certificates, jump hosts and keyboard-interactive sign-in through PAM),
  vsftpd and ProFTPD (FTP, FTPS, active mode and client certificates), and Dante and
  tinyproxy (SOCKS5 and HTTP CONNECT proxies). The tests are built for release, as
  continuous integration builds them. See the development guide.
- **Folder reports without folder paths:** *File → Export → Hide folder paths in reports*,
  `--report-hide-paths`, or `report_hide_paths` in a job file names the compared folders Left,
  Middle and Right in HTML, PDF, slideshow, CSV, text and XML folder reports, and keeps their
  paths out of the review notes and review summary the reports carry. HTML and PDF reports keep
  their paths out of their notes, filter and settings too, and warn that file names and the
  differences shown can still be sensitive.

- **HTML report detail:** *File → Export → HTML report detail* (`--report-detail`, and
  `report_detail` in job files) chooses *Summary*, *Standard* or *Full*. *Summary* lists where
  each difference is without its lines, and a folder report made with it reads no file;
  *Standard* keeps the limits reports had (20,000 rows in all, 400 per diff block, 2,000
  characters per line); *Full* shows every row and whole lines, so the page grows with the
  differences. Every HTML report, text reports included, now opens with a box that says
  whether it is complete or counts the files, diff blocks and rows it left out or shortened, and
  states the policy it was exported with. A text report cut by its limit counts the differences left out
  in its stat cards too. Reports stay a single self-contained file; the guide explains why and
  what each detail costs.

### Changed

- **Less memory for large text comparisons:** lines are numbered through a smaller table,
  loaded files give back their spare room, and the text view keeps less per line. Measured
  on an Apple M2 Max: two 100 MB files peak at 561 MB instead of 620 MB and compare 7% faster;
  two 1 GiB files peak at 5.5 GB instead of 5.7 GB (engine) and 7.0 GB instead of 7.7 GB in
  the text view, with the same speed (22.9 s against 22.8 s for the 1 GiB view); see
  `docs/benchmarks.md`.
- **Focus after dialogs and menus:** closing a dialog or a menu with Esc or one of its
  buttons gives the keyboard focus back to the control that opened it, rather than leaving
  nothing focused; a menu command that moves the focus itself, such as *Find*, keeps it there.
- **Screen readers** now read out the difference counter, the merge status, the status bar
  and messages as they change (errors interrupt), and check boxes report whether they are on.
- **Memory of word-level differences** in the text view is now bounded (8,192 rows kept,
  those furthest from the view dropped first and worked out again when shown).
- **Report links:** a file's anchor in a folder report is now made from its path, as in
  `#f-3f9a0c12d4e7`, so a link to a file or one of its differences still works in the next
  report of the same folders; links copied from earlier reports (`#f7d0`) no longer resolve.
  Printed reports repeat the sides above a difference's rows on each page.
- **Menus in short windows:** a menu too tall for the window scrolls within it instead of
  being cut off. A menu of the menu bar stays under its button. Arrows at its top and bottom
  scroll it while the pointer rests on them, and the mouse wheel and the arrow keys scroll it
  too. *Open project* and *Save as project* moved to a new *File → Projects and jobs*
  submenu, with the new *Save as job* and *Run job*, so the File menu fits more windows
  whole.
- **HTML reports** are easier to read, search and share:
  - A folder report counts the files with different contents, the files on one side only and
    the differences exported apart. Its index lists every changed or one-sided file, and a file
    shown without its differences says *Summary only* and why.
  - Each file says how it was compared, such as *Text*, *Spreadsheet cells*, *Document text* or
    the converter program. Each difference gives its sheet and rows, its PDF pages, or its
    notebook cell, and a changed CSV, TSV or spreadsheet row lists the fields that changed, as
    in `Alice · score · 90 → 95`. A spreadsheet value reads as the sheet shows it, with the
    value it stores when that differs, as in `741,700 (741700)`; a sheet's title rows above its
    header and rows ending in empty cells no longer keep its fields from being listed, and a
    header over two rows names a column by both, as in `Quarter Q3`.
  - A changed binary file that OxideMerge or a configured converter reads as text, such as an
    ELF program, shows the differences of that text in HTML, PDF and slideshow reports and in
    the tab of all the differences, rather than only that it is binary. When that text is the
    same, the report says the files differ in bytes it leaves out.
  - A *Comparison settings* section shows the method, filter, options and report limits, and a
    note says what the report leaves out or shortens.
  - *Previous* and *Next* buttons with a counter go through the differences across files, and a
    link to a difference opens its file and scrolls it below the header.
  - Search covers file names and paths, or the text of the differences, with a count of matches
    and a *Clear* button.
  - On a phone the index folds away and the page fits the screen.
  - The page carries its language, labels its controls and their states for screen readers,
    shows keyboard focus, and has more contrast in the light theme.
- **PDF folder reports** hold what the HTML folder report holds:
  - The summary counts the files with different contents, the files on one side only, the
    differences exported and those left out.
  - A *File differences* index lists every changed or one-sided file with its differences
    drawn, or *Summary only* and why, and how it was compared; each entry links to the file.
  - Every changed file has its own part, a file shown without differences included. Each
    difference gives its sheet and rows, PDF pages or notebook cell, and the fields of a changed
    CSV, TSV or spreadsheet row.
  - The *Comparison settings* section and the note of what the report leaves out or shortens
    follow the summary, and the PDF carries the language of its text.
- **About** shows just the build date for a release build, and the build date together
  with the commit it was built from for any other build, instead of one or the other.
- **File → Export** has three reports: *HTML report...*, *Slideshow report...* and *PDF
  report...*. *Generate report...* and *Generate report with file differences...* are
  gone: *HTML report...* does what both did (a folder report includes the differences of
  every changed text file), and its save dialog also offers XML, and for a folder CSV and
  plain text.
- **Slideshow reports** carry the OxideMerge icon in their header, embedded in the file;
  the icon and the last slide link to the project page.
- **Folder slideshows** hold what the HTML folder report holds: the title slide counts the
  files with different contents, the files on one side only and the exported diff blocks, and
  says what the slideshow leaves out. Each slide says how its file was compared and gives the
  sheet and rows, PDF pages, notebook cell or changed fields of its difference. The last slide lists
  every changed or one-sided file, linked to its first slide or saying *Summary only* and why,
  followed by the comparison settings. The slide buttons have names for screen readers, the
  slide count is announced, and a long closing slide scrolls from its top.

- **HTML reports** carry the OxideMerge icon in their header, embedded in the file (about
  10 KB), and move: the sections and file cards fade in, the counts in the summary cards
  count up, cards lift under the pointer, opening a file slides its differences in, and
  the difference that N or P goes to pulses. A browser set to reduce motion, and printing,
  get a still page.

- The README shows the OxideMerge icon next to its title.
- The English and Traditional Chinese READMEs are reorganized for users and use simpler
  wording. The build, test, continuous integration and release instructions moved to a new
  development guide (`docs/development.md`, `docs/development.zh-TW.md`).
- **Semantic TOML and YAML:** numbers compare by value also between integers and decimals
  (`1` equals `1.0`, `1e23` equals `100000000000000000000000`) and exactly, not as 64-bit
  floats; YAML integers too large for 128 bits compare by their digits, and a TOML integer
  outside 64 bits is reported as out of range, as the TOML specification requires. TOML
  date-times with a time zone compare by the moment they name, and YAML keys by value
  (`0x10:` and `16:` are one key, so a mapping with both `0x10:` and `"16":` is refused).
  An empty YAML document is `null`; `!!int`, `!!float`, `!!bool` and `!!null` must fit
  their value; tags on keys other than `!!str`, and on mappings or sequences other than
  `!!map` and `!!seq`, are refused. TOML files are limited to 4 MiB like YAML.
- **Comparing with a revision:** exporting a revision's folder (Git, Subversion or
  Mercurial) is refused if it would come to more than 8 GB or 500,000 files, the same
  limit an archive extraction already had, so a mistaken or huge revision cannot fill
  the disk.
- **Release baselines:** a suspected rename with edits needs at least three shared lines,
  so two tiny files with one line in common are no longer paired. An important path also
  flags a move or rename from it. `--split-rename` and `--important-path` accept `\`
  between folders and a leading `./`; `--split-rename` reports a path that is not an added
  file, and both are refused outside baseline, release report and impact analysis.
- **Impact analysis:** Objective-C `#import "…"`, TypeScript `import x = require(…)` and
  `.js` imports of `.ts` sources are exact imports; a `.h` header that only reads as C++
  is read as C++; only the `<script>` blocks of Vue and Svelte components are read; Rust
  attributes such as `#[derive(…)]` count as part of a definition, and a name defined
  twice changes with either definition. Several reasons for one line are one finding (rule
  `literal-reference-v2`, so name and path findings dismissed before need dismissing
  again), and lines with an exact finding are dropped before the 500 name and key findings
  are counted. Files of up to 1 MiB are indexed in parallel.
- Every interface language now translates the *Text layout* choices (*Aligned rows*,
  *Compact rows*).
- **User guides:** reorganized by task (text, merging, folders, binary, images, web pages,
  filters and rules, saving and reports, analysis tools, then the reference chapters),
  with the long paragraphs split into steps, lists and tables. The English guide uses
  short sentences in plain English for readers whose first language is not English; the
  Traditional Chinese guide uses Taiwanese terms and the program's own wording, without
  mixed-in English. The guides now describe the *Important path fragments* field of the
  baseline panel. All screenshots were taken again from the current version, and new ones
  show the *Select files or folders* dialog, the file compare below the folder list, the
  inline view, the CSV table view, the Formatted web mode, merge verification, semantic
  comparison, the review workspace and the release summary.
- **Web compare no longer uses a browser:** the Full page and Screenshot (visible area)
  modes are removed, with the Edge, Chrome, Chromium or Brave they started headless and
  the *Browser used to render pages*, *Screenshot width* and *Screenshot height* options
  (the options tab is now *Image / Binary*). The Formatted, Text, HTML source and Resource
  tree modes convert the HTML as downloaded, in the window and with `--cli`, so
  `samples/web/left.html` against `right.html` opens in a few milliseconds instead of
  about 2 seconds. Page scripts never run, so content they add is not compared. Settings
  saved with a screenshot mode open web pages in Formatted. The Text mode leaves out
  hidden elements, as the Formatted mode does, and no longer includes the page title. The
  *Render pages in the browser for text and HTML* option is removed.
- **Formatted web compare is faster:** a large table lays out only the rows in view, and
  measures its column widths once, so scrolling a page with a table of 1,500 rows takes
  about 2 ms a frame instead of 35 ms. The pages are converted, lined up and compared on
  the loading thread, so opening a large page no longer holds up the window.
- **About from the title bar:** clicking the name *OxideMerge* next to the logo opens the
  About dialog too, as clicking the logo does. Screen readers name the two as a button.

### Fixed

- **The Rules tab of Options fits a small window:** in French, Japanese, Russian, Spanish
  and Portuguese its row of buttons widened the dialog past an 800 × 600 window when no rules
  file existed yet. The row now wraps.
- **Undoing a synchronization the program ended in no longer fails on a folder it made:**
  when OxideMerge ended while a copy was still under its temporary name in a folder the
  synchronization had just created, the first attempt to undo it could not remove that folder
  ("Directory not empty"). Undoing now removes the synchronization's temporary files first.
  When something still cannot be undone, the message now says what to do next, and choosing
  *Undo those changes* again is safe.
- **An FTP upload no longer risks losing its end:** the data connection was closed while
  the server might still send on it (a TLS session ticket, say). That reset the connection,
  and the server could throw away the end of the file before reading it. OxideMerge now
  waits for the server to close its side first.
- **PDF reports keep code in one font on a system without the code fonts they look for:**
  a font found for Hebrew, Arabic or Thai came before the built-in code font, so it drew
  the digits and spaces between letters in the built-in font. The built-in font now comes
  first.
- **An interrupted synchronization is no longer missed while another program starts:** the
  check of whether a synchronization journal is still in use could see it as in use for a
  moment after the synchronization had let it go, when a process (`git`, say) was being
  started at the same time, and leave it out of the list to undo. It now tries again briefly.
- **In-line highlighting now follows the rules' substitutions and *Ignore numbers*:** a
  changed line no longer highlights text that a substitution replaces (a timestamp, an
  address) or digits ignored by *Ignore numbers*, in the text view and in reports; only the
  parts that still differ are marked.
- **Comparing with a revision no longer follows a symbolic link out of it:** a tracked
  symbolic link pointing outside the exported copy (in Git, Subversion or Mercurial) is
  dropped rather than checked out, the same protection an archive extraction already had.
  Without this, opening a revision containing such a link could show the content of an
  unrelated file elsewhere on disk as if it were part of the revision.
- **A Git revision's submodules now count against the same 8 GB / 500,000-file export
  budget as the rest of it**, instead of each submodule getting that budget again on its
  own; a revision with many or deeply nested submodules could otherwise come to a multiple
  of the stated limit before being refused.
- **Web pages that could fill memory or never finish:** a table's merged cells are laid out
  within one limit for the whole table, its row groups and padded rows included, and a span
  over the rest of a long table no longer takes minutes; quotes and lists nested more than 32
  deep are read as their text, so a page of unclosed ones no longer grows with their number
  squared. *Stop* and changing the mode end the conversion of a page. A link opens only a web
  or mail address, or a file on this computer from a page on it, decided after the address is
  decoded, so `&#106;avascript:` and links to other computers' shares are plain text. A table
  whose first row is data with a merged cell gets an empty header row, so the rows that cell
  reaches are no longer drawn as header rows.
- **Folder synchronization in more unusual cases:** when links are copied, a folder that
  became a link after the compare is a conflict, and nothing in it is copied or deleted
  through the link. A renamed file is moved only when its old place is still inside the
  destination and its time and permissions match the source's; otherwise it is copied, so
  the destination gets the source's time and permissions. Files that are only moved no
  longer count against the free space. A name renamed only in case or accents is no longer
  deleted a second time under its other spelling. A file or link that appears at the
  target after the preview stops the synchronization instead of being overwritten.
- **PDF reports stay within their size and show every change they list:** a report draws at
  most 20,000 lines of compared text, a wrapped line counting for each line it takes, with
  `--whole-file` too, so two large logs no longer fill memory. A difference that is cut keeps
  at most 100 unchanged lines above its change, so a large `--context` no longer hides the
  change. Code fonts no longer join `!=` or `->` into one sign, and changed-word marks stay
  under their characters. Each column is labeled with its side, a difference continued on a
  new page names its file, the folder file list repeats its header on each page, and a cell
  too tall for a page is cut. Hebrew and Arabic text reads right to left, with its numbers
  (dates, phone numbers, percentages and prices kept whole) and brackets in place and Arabic
  and Persian letters joined, and Hebrew, Arabic and Thai print with an installed font whose
  licence allows embedding a subset of it. Characters no font has are listed at the end of the
  report and on standard error. Color-blind colors show changes in purple and conflicts in yellow, as
  in the program. The temporary file for *Print* is readable only by you and deleted when
  OxideMerge closes. The review summary section and the font and page-size errors are
  translated.
- **Formatted Word, workbook and PDF views of crafted files stay within memory:** a table is
  cut to 1,048,576 cells and 1,024 columns, including a merged area over a whole sheet or a
  Word row padded to the widest; a workbook's text is counted as it is read, number formats
  show at most 30 decimals, and tagged PDF text is counted across all pages. A part of more
  than 3 million elements is reported instead of read, and elements share their namespace
  names. Like the Text mode, the Formatted mode now ends with a line when it leaves some of a
  file out. *Cancel* works while tables, pictures and PDF pages are read, and each picture is
  read and fingerprinted once. Large tables no longer freeze the window while their columns are
  measured.
- **Document reading details:** text moved with tracked changes is read once, where it was
  moved to, in the Text mode as well. A line break inside the words of a table cell or a PDF
  cell no longer splits the table, and a heading or list item with one stays one line. A
  paragraph or cell ending in a backslash, such as `C:\temp\`, keeps it. Rows of a table
  nested in a content-controlled row no longer show again as rows of the outer table.
- **Undo in large files no longer keeps a copy of the text for each step:** only the latest
  undo and redo steps hold the whole text; each older step keeps
  only the lines it changed, so editing a pair of large files no longer holds ten or more
  copies of them in memory, and more steps are kept.
- **Compare with revision stays responsive on slow repositories:** revision hints are
  detected in the background. Closing the dialog or replacing an obsolete revision request
  cancels its Git, Subversion or Mercurial log process instead of leaving the request
  running. The revision itself is also read in the background, with *Cancel* for a server
  that is slow or does not answer; an error brings the dialog back with the path and
  revision you entered.

- Multi-cursor word deletion merges overlapping deletion ranges before editing, preventing
  extra text from being removed. Copy and Cut read the original selections and copy each
  whole line once; multiple cursors on one line no longer cut subsequent lines. Undo
  restores all original cursors in one step.

- **The view and the text cursor stay with their text after an edit:** copying a
  difference, Undo, Redo, a paste or a file reloaded from disk that adds or removes rows
  above the view no longer slides other text into it; the line at the top, the cursor row,
  the column selection and the text cursor with its selection move with their lines. A
  cursor moved or typed just past the top or bottom edge scrolls one line rather than
  jumping to a third of the way down. Clicking one of the last two lines in view, or
  dragging a selection to the bottom edge, no longer jumps the view either: the drag
  scrolls smoothly with the selection's end at the edge. With word wrap on, an edit above
  the view keeps it on the same display line of a wrapped line rather than the first one.
- **An edit makes only the hunks it changed need a new review:** editing a reviewed file
  still puts the file's decision out of date, but a decision on a difference goes out of
  date only when the edit changed its lines or the two lines around them. Before, every
  difference's decision did.
- **Saving no longer replaces a file that is read-only on disk:** *Save* and *Save As* in
  a text compare, and *Save* in *All differences*, used to replace such a file whenever
  its folder could be written, although the pane said it could not be. They now refuse
  with a message and keep the edits, so the file can be saved under another name. Folder
  synchronization still replaces read-only files, as before.
- **High contrast in the dark theme:** the changed-word highlights were made lighter,
  which left the light text on them at as little as 2.6:1. The high-contrast look now
  keeps the text on every line color at 4.5:1 or more; a test checks that, and that the
  text cursor has 3:1 or more on every line color in every look.
- **One thing per Escape:** with the search bar open and text selected, one `Esc` closed
  the bar and also ended the selection, and with a context menu open it ended the
  selection too. Now each `Esc` does one thing, in order: a menu, a dialog or a field
  being typed in, the search bar, the selection, then the text cursor; only then does
  `Esc` close the tab.
- **Option+letter on macOS:** with the text cursor in the text, `Option+Z`, `Option+S`,
  `Option+C` and `Option+P` typed their character (Ω, ß, ç, π) and also switched wrapping,
  set a synchronization point, collapsed the matching lines or showed the file pane. They
  now only type; the commands stay in the menus, and the keys work again once the cursor
  leaves the text.
- **Right-click and the text selection:** a right-click in the selected text keeps it, and
  *Copy text* copies just that text rather than the whole line; a right-click elsewhere
  moves the cursor there first, so the menu acts where it was opened. A selection that
  ends at the start of a line no longer counts that line among the selected rows, so
  copying the rows of a whole selected line to the other side copies only that line.
- **Keys in the inline view:** a text cursor left in the side-by-side view no longer takes
  keys while the inline view is shown, so `Esc` and, on macOS, `Option+←` and `Option+→`
  work there again, and `Ctrl+]`, `Ctrl+[` and *Replace* no longer change text the inline
  view does not edit.
- **Undo steps and unsaved state:** a paste in the text cursor is an undo step of its own
  rather than joining the typing before it, and a save ends the typing step, so Undo stops
  at the saved text and shows it as saved. Undo and Redo never write a read-only pane: a
  step that would change only read-only panes is refused with a message, and other steps
  leave those panes as they are. Choosing another encoding for a writable file can be
  undone, and text from before a file was read again from disk comes back as unsaved. Undo
  and Redo mark a review as out of date, as other edits do.
- **Accents and emoji stay whole in the text cursor:** the arrow keys, Backspace and
  Delete move over and remove whole characters, so a letter with a combining accent, a
  flag, an emoji sequence such as 👩‍💻 or a Hangul syllable written as jamo is never
  split, and a click, Up or Down never puts the cursor inside one.
- **Clicks on wrapped lines:** with word wrap on, a click to the right of a wrapped
  display line puts the cursor at the end of that line instead of on a character of the
  next one.
- **The text cursor on wrapped lines:** with word wrap on, the up and down arrows, `PgUp`
  and `PgDn` move by display line, keeping the column, instead of jumping over the rest of
  a long line, and `Home` and `End` (`⌘←` and `⌘→` on macOS) go to the start or end of the
  display line first, then of the whole line. With `Shift` they extend one selection, and
  the view follows the cursor down a line longer than the window.
- **Translations:** the message about skipped web resources now gives the right reason in
  every language, and *Saved as* no longer reads as a future action. English counts read
  correctly for one item (`1 difference(s)`). German, French and Russian messages read
  correctly with the side names filled in, and Russian counts agree in number. Terms are
  more consistent within each language (for example *tab*, *merge*, *fold* and *only on
  one side*), and Simplified Chinese uses *访达* for Finder. Every kind of outcome a
  comparison or a synchronization can report — *another reading is as likely*, *no longer
  as it was*, *not supported*, *could not be read*, *could not be written*, *stopped* and
  *needs putting back* — now has one name in each language, used wherever a feature leads
  with the kind of problem before its detail.
- **Web page compare:** *Stop* also ends the download of the pages and their resources as
  data arrives, instead of letting it run on in the background; a server that sends
  nothing is still waited for until its 30-second timeout.
- **Impact analysis:** a style sheet ending inside an `@import` no longer stops the
  analysis; files with lone carriage-return line endings report the right lines;
  cancelling stops the symbol phase too; the index cache no longer mixes up files read by
  different grammars; the first use of an imported name is no longer a property or key of
  the same name.
- **Semantic TOML:** changes in a table point at the table's own lines.
- **Semantic paths inside paired arrays:** a change inside an array paired by key or
  reordered now names the element on each side; before, nested paths and the old place of
  a move used the second side's position. An XML attribute change points at the attribute,
  not the whole tag.
- **File compare below the folder list:** *Next difference* and *Previous difference*
  (`Alt+↓`/`Alt+↑`, the *Compare* menu) now work in it, and past the last or first
  difference, they and its arrow buttons move on to the next or previous file with
  differences; *First difference* and *Last difference* (`Alt+Home`/`Alt+End`) work there
  too. A key pressed while a file is still loading waits for it. Without the docked
  compare, they select the next, previous, first or last file with differences, and with a
  review filter only the files it lists.
- The *Diff algorithm* option shows *LCS* instead of *Lcs*, and `--help` lists TOML and
  YAML for `--semantic`.
- **Merge verification** reports a merged preview that no longer parses as a semantic
  document.
- **Folder merge undo:** the copies are kept next to the merged files, so a deleted file
  is only moved, and moved back on undo, and no room is needed on another disk; files are
  put back atomically, a new `.bak` is removed only after its file was put back, removed
  folders come back with their permissions, and when a file cannot be put back the copies
  folder is kept, named in the error, and lists in `index.txt` which file each copy was.
  Each file is checked again just before it is written.
- **Review workspace:** saving waits until the decisions were checked against the files,
  and a decision on files that changed is saved stale instead of renewed. A file that
  cannot be read for a moment no longer passes its decision to another file with the same
  content.
- **Saving on Windows** no longer fails while a background re-read of the file is running,
  and retries briefly when another program, such as a virus scanner, has the file open.
- **Formatted web pages:** a list item, definition or block element now ends a hidden
  paragraph as in a browser; a stray end tag no longer ends a hidden element early; the
  `<base href>` in the page head resolves links; very deep hidden markup no longer slows
  the conversion.
- The *Array matching key* field stays in place and keeps its focus while the report is
  made.
- Folder column headers tell screen readers the sort direction again.
- *Review* links from impact findings do nothing for a file that is not in the comparison,
  instead of selecting a missing file.
- **Windows:** folder synchronization copies into a folder whose path is just under the
  old 260-character limit. The name of the temporary copy counts towards the limit as much
  as the folder does, so a folder of 224 to 259 characters was still refused with *The
  system cannot find the path specified*.
- **Formatted web pages:** the difference marks beside the pages stay where they are while
  the pages are scrolled. A row is measured only once it is drawn, so scrolling through a
  page of tables replaced each estimate with a real height and moved everything below it,
  taking the marks and the scroll thumb with it. The overview and the scroll bar now use a
  height worked out from each row's content, which drawing never changes; the pages
  themselves still scroll by their measured heights, and the viewport box is carried
  between the two.
- **HTML:** comparing pages as *HTML source* no longer offers the Semantic and Tree views.
  HTML is not XML, and a page that opens with an XML declaration, as an XHTML page does,
  was taken for an XML document; the views then had nothing useful to show.
- **Folder synchronization:** a synchronization that has just succeeded is no longer
  offered for undoing. A window looking for synchronizations that did not finish read each
  journal's header before taking its lock, so one that finished in between — removing its
  header and only then letting the lock go — was still listed, and undoing it would have
  taken back correct work. The header is now read again once the lock is held, and *Undo
  those changes* takes the lock before the header for the same reason. A journal that has
  gone by then is nothing to undo rather than a failure.
- **Windows:** folder synchronization copies and deletes files whose path is longer than
  the old 260-character limit. Such a path was staged correctly but could not be renamed
  into place, so the file was refused with *The system cannot find the path specified* and
  the changes before it were undone. Paths are now handed to the system in their extended
  form where they are too long for the old one.
- **Folder synchronization:** a file another program holds open is reported as one that
  cannot be read, rather than as one that changed after the preview. Nothing is touched
  either way, but the advice the message leads to is different. Where several things are
  wrong at once, the kind of problem reported is the one named by the first message rather
  than the last.
- **Tree view:** `Tab` reaches the tree, which is one stop in the tab order as a tree
  should be: it draws a focus outline, the arrow keys then move through the rows without
  clicking a row first, and what holds the keyboard is named after the current row, so a
  screen reader reads out the row's state and both of its sides. Previously the tab order
  went past the rows and the arrow keys only worked after a click.

## [0.9.7] - 2026-11-01

### Added

- **Inline view** for 2-way text comparisons (the toolbar toggle next to word wrap, or
  *Inline view* in the command palette): one column with removed lines above the added
  lines that replace them, old and new line numbers, and unchanged runs folded. Moving
  between differences and copying them to the other side work as in the side-by-side view;
  the arrow keys move along the lines and Copy copies the current line. Editing and
  selecting lines stay in the side-by-side view.
- **Merge bar** for 3-way comparisons: how many conflicts are left, *Next conflict*
  (wrapping around), *Take left*, *Take both*, *Take right*, *Mark resolved* for a
  conflict edited by hand, and *Save result*, which turns into the main button once no
  conflicts are left.
- **Start page launcher:** type, paste or browse two (or, with *3-way*, three) paths and
  compare them without opening a dialog; *More options…* opens the full Open dialog.
  Recent comparisons are cards. The first start offers a sample comparison in a temporary
  folder, which is removed when its tab closes and is not added to the recent list.
- **Accessibility options** in *Options → General* and the command palette: color-blind
  friendly diff colors (blue, orange, purple and yellow instead of green and red), high
  contrast, reduce motion and compact density.
- **Search commands** field (⌘K / Ctrl+K) in the title bar, which opens the command
  palette; narrow windows keep the icon.
- **Formatted web page compare:** the new *Formatted* mode, the default for web pages and
  HTML files (a mode chosen before is still remembered), turns each page into Markdown and
  shows it formatted, side by side, with headings, emphasis, links, lists, quotes, code
  blocks and tables. Pages are compared block by block; changed blocks are highlighted and
  lined up with the changed block of the same kind on the other pages (a heading with one
  of the same level first), with the changed words highlighted within them as in the text
  compare (following *Inline differences*). Tables are drawn with lines around their
  cells; their rows are lined up, with the changed words in each cell highlighted, and
  cells of rows or columns only one page has highlighted whole. The difference buttons and
  keys step through them, and as in the text compare the current difference is highlighted
  more strongly and the blocks being compared are outlined on every page; clicking a block
  makes it the compared one, and stepping goes on from there. The pages scroll only when
  the difference is out of view, and a location pane gives an overview of the differences,
  as in the text compare. Content the browser does not show, also when a style sheet hides
  it, is left out, and images appear as links. Text that reads like a character reference,
  such as `&copy;` on a page about HTML, is shown as written. The console compare
  (`--cli -t web`) compares the Markdown when this mode is selected. Sample pages with
  complex text, headings and tables are in `samples/web/formatted`.
- **HTML files open in the web page compare** (`.html` and `.htm` on every side), so they
  show formatted by default; `-t text` or *Compare as → Text* compares their source, and a
  merge with an output file (`-o`) or a console `--patch` still uses the text compare.
- **TOML and YAML semantic compare:** the *Semantic* view and `--semantic` also read TOML
  and YAML (`--format toml` or `yaml`, or from the extension), comparing tables and
  mappings without regard to order and numbers by value. YAML is read by the YAML 1.2 core
  schema, one document of at most 4 MiB without aliases, complex keys or custom tags,
  within the existing nesting and node limits.
- **Semantic changes as a path tree,** with a count on each branch, and a **Moved** kind
  for array elements that only changed place. *Array matching key* (`--array-key MEMBER`)
  pairs the object elements of arrays by a member such as `id` instead of by position.
- **Release summary:** the time each side was captured, category counts that filter the
  list, and a *High priority* list of changes with risk flags. A deleted and an added text
  file that share at least half of their lines are shown as a **suspected rename** with
  the share of matching lines; *Not a rename* (`--split-rename PATH`) keeps them apart.
- **Exact impact findings:** *Potential impact* recognizes JavaScript/TypeScript, Python,
  C/C++, CSS, Rust and JSON `$ref` imports of a changed file and labels them exact;
  changed nested JSON, TOML and YAML keys are also searched for as dotted paths. In a
  folder comparison, *Review* opens the file of a finding in the review workspace.
- **Merge plans warn** when both sides edit lines close together or change the same
  setting in different places.
- Review decisions follow files renamed without changes, and ambiguous renames are marked
  for review again; review bundles with unsafe or missing entries are refused.
- **Function and type impact:** for changed JavaScript/TypeScript, Python, C/C++ and Rust
  files, *Potential impact* compares the top-level functions, types and constants, and a
  file importing a changed or removed one by name (also under another name) gets an exact
  finding at its first use; the name's first use in other files of the same language is a
  heuristic finding. Matches of names and configuration keys stop at 500 with a note
  counting the rest, instead of failing the analysis when a broad change hits common
  words.
- **How sure a semantic change is:** each change says whether its array elements were
  paired by the array matching key, as a reordering or by position, and changes another
  reading explains as well, such as either element of a swap, are marked *ambiguous*. A
  note says when the array matching key cannot pair an array. Reports add `confidence`,
  `ambiguous` and `notes`.
- TOML and YAML samples: `config/service.toml` and `config/deploy.yaml` in `samples/left`
  and `samples/right` differ in layout almost everywhere but in only a few settings, and
  `samples/three/settings` holds the 3-way merge settings as TOML and YAML as well as INI.
- Saved JSON and XML semantic, release summary and impact reports in
  `tests/fixtures/reports`; the tests fail when a report of the same inputs differs from
  them.

### Changed

- **Folder auto merge is all or nothing:** every item is checked before anything is
  written, and if one changed since the plan nothing is merged. A *Stop* button ends a
  running merge, and stopping it or a failed change (a read-only folder, a full disk)
  undoes the changes made so far.
- *Potential impact* reads imports and definitions of JavaScript/TypeScript, Python, C/C++
  and Rust files from their syntax trees, parsed with Tree-sitter, and imports of other
  files from the code's tokens, so imports spanning lines are found and imports inside
  comments and strings are not; Rust `use crate::…`, `super::…` and `self::…` paths and
  JavaScript re-exports count as imports. A file with code the parser could not read is
  listed as partly assessed. The application is about 10 MB larger for the grammars.

- The footer of HTML reports of folder and file comparisons links *OxideMerge* to the
  project page, https://github.com/kuopinghsu/OxideMerge.
- CI builds, tests and packages the macOS app, and runs the macOS benchmarks, on the macOS
  15 runner image, since the macOS 14 image is retired on November 2, 2026. The app still
  runs on macOS 11 and later.
- Review, verification and baseline panels open in a side drawer, toggled by the button at
  the right end of the toolbar, instead of taking space below the comparison.
- **Folder comparison:** chips under the paths count *All*, *Different*, *Only in
  Left/Right* and *Identical* items and show or hide each kind with a click, replacing the
  counts in the status bar. Folder rows say how many items differ inside them; a colored
  bar at the left edge marks differences instead of tinting whole rows; one *Modified*
  column shows the newest date and which side is newer; the *Folder* column is left out in
  tree mode. In a 3-way comparison, an item missing from one side only is counted, shown
  and hidden with *Different* rather than with *Only in*, which now covers items on one
  side alone. Folders left uncompared by a comparison that is not recursive count only in
  *All*, as in the console comparison, and stay listed whichever kinds are hidden.
- The text comparison toolbar is grouped into Differences, Merge, Edit and View, with
  captions in wide windows. The status bar no longer repeats the difference count shown in
  the toolbar.
- The gutter between panes is calmer: the connector of the current difference stands out,
  the others are faded, and the copy arrows appear on the current or hovered difference.
- Tab titles name a file shared by both sides once, with the folders that differ as a
  subtitle. Tabs with unsaved changes show a dot in place of the close button until
  hovered.
- The user interface is built on egui and eframe 0.37 (from 0.31), which the formatted web
  page view needs. Menu items sit slightly inside their menu, and screen readers name menu
  items by their label alone, without the shortcut, and drop-downs by the caption beside
  them.
- Icons are drawn instead of taken from font symbols (sort arrows, tree arrows, warnings
  and the Help icon), so they look the same with every font, and colors, sizes, corners
  and spacing come from one set of theme values. The common icons (compare, folder, file,
  search, settings, theme, arrows, history, filter, export, image, binary, web page, add,
  check and side drawer) follow one outline set on a 24-unit grid, and all icons use its
  lighter line; *Options* now shows two sliders instead of a gear. Buttons, chips and
  toggles show distinct hover, pressed, selected, disabled and focus states in every theme
  and high contrast mode, and dialog buttons react to hover.
- Toolbars start a new row for a group that no longer fits instead of running past the
  window edge, and folder columns start narrower in narrow windows so every column stays
  in view. Narrow windows leave out the name next to the logo in the title bar.
- Screen readers name the tree arrows ("Expand …" / "Collapse …"), the Git revision
  picker, the scroll bars, the location pane and the comparison areas.

### Fixed

- The Formatted web page compare stops at every changed block when stepping through the
  differences. Changed blocks next to each other counted as one difference, so *Next
  difference* skipped all but the first, such as an item added after a changed one at the
  end of a list.
- Decisions on documents compared through a converter stay current when a review is
  reopened, instead of always being marked for review again.
- Merge plans no longer warn that an insertion on one side is a deletion.
- *Next unreviewed* and the links to a file in the review workspace open folders collapsed
  in the tree to show the file.
- The close button of the Help & Shortcuts dialog sits at the right edge of its title bar,
  as in the About dialog, instead of short of it: the dialog is now exactly as wide as its
  panels and scroll bar, and shortcuts too long for their column, such as
  `Shift+↑ ↓ / Shift+PgUp PgDn` on Windows and Linux, wrap instead of widening a panel.
- The close button of the About and Help & Shortcuts dialogs is announced as "Close" to
  screen readers.
- **Web page compare** no longer leaves out text inside `display: contents` elements,
  which hid it from the Text mode and the highlights altogether, nor the text of web
  components (open shadow roots, with their slotted content in place). Highlights are
  painted in shadow roots too.
- With *Render pages in the browser for text and HTML* off, the Text and HTML source modes
  show the pages as downloaded even after a screenshot mode has rendered them. Rendered
  pages are rendered again after the browser or the screenshot size changes in Options.
- **Web page compare** is sturdier with pages that misbehave:
  - A page that redirects after it loads, or opens an `alert`, `confirm`, `prompt` or
    print
    dialog, no longer fails the comparison or waits until the time limit: dialogs are dismissed
    and the page the redirect leads to is read.
  - A page that cannot be reached, does not load in time or crashes its tab is reported in
    its
    own pane header with a blank screenshot, and the other pages are still shown. Error pages
    with an empty body (such as an empty 404) show their status instead of failing.
  - The pages are asked for at the same time instead of one after the other, so slow pages
    no
    longer add up, and Stop or closing the tab ends the wait at once instead of after the time
    limit.
  - Text and highlights are read in a separate script context, so pages that redefine
    `Array`, `JSON` or other globals cannot break them, and a page whose highlights cannot be
    painted is still compared.
  - XHTML pages and text with unpaired surrogates are read correctly, and text hidden
    inside a
    closed `<details>` or `content-visibility: hidden` element is left out.
  - On macOS and Linux the browser no longer asks for keychain or password store access,
    its
    messages are limited in size, and its pipes are not passed on to other programs.
- The folder comparison toolbar no longer draws its filter, rules and method chips over
  *All differences* and *Export HTML* when they do not fit, and the image comparison
  toolbar no longer runs past the window edge in windows narrower than about 1200 points:
  both switch to icons and shorter chips. Title bar icons that no longer fit are left out
  rather than overlapping the menus.
- In 3-way text comparisons, windows narrower than about 780 points no longer push the
  panes, the pane footers and the scroll bar past the right edge.
- Very short panes no longer crash the custom scroll bars.
- In dark high contrast mode, hovered accent buttons are told apart from resting ones.

## [0.9.6] - 2026-10-30

### Added

- About and `--version` show a short Git revision for development builds, with `-dirty`
  for tracked source/index changes; clean optimized builds at the matching version tag
  retain the UTC build date. The copyright year always uses the calendar date.

- Escape closes the active comparison tab after open dialogs, search, previews and editors
  have handled it, retaining the existing unsaved-changes confirmation.

- Configurable compact/aligned text rows, including blank insertion/deletion spacing in
  aligned mode; text and binary location panes consistently appear on the right.

- Opt-in merge plans and saved-result byte verification using the existing merge engines,
  with explicit conflict, mechanical-check and heuristic-warning states; `--merge-plan`
  and `--validate-merge` provide versioned analysis reports without writing comparison
  inputs.
- Read-only JSON/XML semantic views and `--semantic`, with changed paths, decoded source
  spans, duplicate-key/entity/depth guards and textual fallback.
- Persistent file/hunk review decisions, comments, rationale, tags, stale-source detection
  and summaries in additive `.oxmreview` version 2 manifests; version 1 remains readable.
- Frozen folder/Git/snapshot baseline summaries, unique exact-content move detection,
  optional important-path flags and evidence-only potential-impact reports. All reference
  findings are heuristic and do not certify builds, tests or compatibility.

- **Document conversion throughout comparisons:** full-content folder scans, individual
  files, command-line comparisons, docked previews and folder reports share the same
  external converters. Options include presets for PDF text (pdftotext),
  Office/OpenDocument (Pandoc), and Apache Tika, with dependency hints. Converters are
  limited to 30 seconds and 16 MiB per output stream; stopping a folder scan cancels its
  running conversions. Failed conversions report errors even when they produced partial
  text. Extracted documents remain read-only.
- **CSV/TSV record editing:** table view handles quoted multiline records and escaped
  quotes, offers resizable columns, and lets you edit or copy individual cells through the
  context menu. Cell changes preserve untouched field quoting, encoding and line endings,
  with undo/redo.
- **Portable folder reviews:** save a folder comparison as `.oxmreview`, or right-click to
  save only selected files/folders. Bundles include captured contents, effective
  comparison settings, and folder bookmarks/comments, and reopen as read-only snapshots
  without the original files. Converted documents are captured as text; converter commands
  are not included or run on reopen. Folder comments are included in HTML, XML and text
  reports.

### Fixed

- Merge verification includes encoding/BOM/EOL conflicts and rejects matching base bytes
  while a format conflict remains unresolved; independent text/format changes remain
  mergeable.
- XML semantic parsing tracks child positions per parent, avoiding quadratic sibling scans
  on wide documents while preserving attribute, comment, mixed-content and reference
  semantics.

- Large UTF-8 text comparisons load incrementally, borrow unmodified comparison keys and
  use compact unique-line anchors with coalesced equal runs and in-place reconstruction.
  Alignment rows shrink from 64 to 16 bytes; pane display maps share rows. Release
  comparison dictionaries before alignment and avoid whole-document syntax-worker copies
  beyond 50,000 lines or 16 MiB.

- Folder merge confirmation rechecks SHA-256 content hashes so same-size edits with
  restored modification times are detected before overwriting a changed file.

- Merging a CSV/TSV difference preserves other changes in the same multiline record;
  partial merges that would break quoting are rejected without modifying the document.
- Table view supports record and column keyboard navigation, F2/Enter cell editing,
  clipboard copy/paste and Go to Line. Clicking a multiline record selects a difference
  within that record.
- Portable folder reviews retain captured line filters, substitutions and ignored blocks
  from the initial scan onward, including after refreshing on another machine.

## [0.9.5] - 2026-10-29

### Added

- **Synchronization points** (*Merge → Set synchronization point*, Alt+S, or right-click a
  line): line up lines the compare pairs wrongly. The text before and after each point is
  compared separately. Points are marked in the gutter, follow edits and are kept in saved
  comparisons.
- **XML reports** of text and folder compares, for other programs to read: choose the
  `.xml` type in *Generate report...*, or `--report <name>.xml`. Text reports list every
  difference with its lines and the bookmarks; folder reports list every item with its
  status, dates and sizes. The layout is described in the user guide.
- **HTML slideshow reports** (*File → Export → Generate slideshow report...*, or
  `--report <name>.html --slideshow`): one difference per slide, with a title slide,
  keyboard navigation, full screen, a link for each slide and one slide per printed page,
  as Araxis Merge makes them. Folder slideshows cover every changed text file.
- **Web page compare, full page:** a new *Full page* mode captures each whole page, top to
  bottom, instead of only the first screen (up to 16384 pixels tall).
- **Web page compare, highlighted differences:** the screenshot modes compare the pages'
  text and highlight what differs on the pages themselves (changed blocks in yellow,
  changed words in orange), so a change that moves the page down no longer marks
  everything below it. *Highlight differences* in the toolbar turns this off.

### Changed

- **Web page compare** drives the browser over the DevTools protocol, with all pages
  loading at the same time in one browser. Text and HTML source now come from the page as
  the browser shows it, after its scripts have run, when a browser is found (*Render pages
  in the browser for text and HTML* in the options). Text puts each heading, paragraph,
  list item, table cell and form value on its own line and leaves out hidden elements.
  Text is the default mode.
- **Menus:** every pull-down menu opens equally wide: the menu bar's menus, their
  submenus, the right-click menus and the folder compare's drop-downs (the widest menu of
  the menu bar, its submenus included, sets the width, at least 220 points). Options that
  turn on or off and choices of one show a check mark in a column of their own, so labels
  line up and an option that is off no longer looks disabled. Shortcuts keep clear of
  their labels. The tab's right-click menu shows the Close shortcut, and a file's
  right-click *Compare as* offers Web page like the Compare menu.
- **Web page compare:** when the browser fails, Text and HTML source fall back to the
  pages as downloaded and the status bar says why, instead of showing only an error. The
  console compare (`--cli -t web`) also renders the pages in the browser, so it matches
  the app. On macOS and Linux the browser is driven over a private pipe instead of a local
  port. Tests that need a real browser run only when `OXIDEMERGE_TEST_BROWSER` is set; CI
  sets it on the Windows and Linux x64 runners.
- **Release files:** a release now holds nine files: a `.zip` per Windows architecture
  holding only `oxidemerge.exe`, an AppImage and a `.tar.gz` per Linux architecture, the
  macOS `.dmg`, and the two PDF user guides. The separate Windows `.exe` downloads, the
  Linux `.deb` packages, the macOS `.zip` and `SHA256SUMS.txt` are no longer published;
  GitHub lists the SHA-256 digest of each file on the release page. The release fails if
  any file is missing or an unexpected one is present.

### Fixed

- **UTF-8 patches:** inserting or deleting lines at the start of a file with a byte order
  mark now keeps exactly one mark at the file's start, including when ignore rules match
  the line that moves across that boundary.

- **Windows CI:** the Git revision CLI test explicitly checks LF and CRLF checkout modes
  instead of assuming LF output, and isolates the subprocess Git configuration from the
  runner.

- **Mixed line endings:** automatic merge combines independent terminator edits on aligned
  lines, including around insertions and deletions, and keeps the merged endings on save.
- **Patch ignore rules:** anchored filters, substitutions and ignored blocks are evaluated
  against decoded text so BOMs and CRLF bytes cannot change which edits are exported.

- **Text formats and merging:** comparisons account for encodings, BOMs, line endings and
  final newlines. Copy All transfers selected formats across multiple differences,
  independent text and format edits merge together, and saving refreshes format
  differences.
- **Patches:** UTF-8 patches preserve BOMs, LF/CRLF endings and missing final newlines,
  including changes that move the last line. Unsupported encodings and lone-CR endings
  report an error.
- **Projects:** saved projects retain pane descriptions and read-only flags and use
  relative input paths where possible. The guides clarify save-shortcut scope, and the
  security document explains trusted Git filters and their possible command execution and
  network access.

- **Four or more paths:** opening four or more files or folders, from the command line, a
  drop or a file manager, no longer crashes the folder compare; it says that two or three
  paths are needed.
- **File safety:** saving and copying now report an error if a temporary file cannot be
  created or cannot replace the destination. They no longer fall back to overwriting the
  original file directly, which could truncate it if a subsequent write failed.

## [0.9.4] - 2026-10-27

### Added

- **Tests:** randomized diff, 3-way merge and patch tests that check every result against
  the inputs (`OXIDEMERGE_TEST_CASES` and `OXIDEMERGE_TEST_SEED` for longer runs or to
  repeat one), tests that draw the views without a window, and command line tests of the
  built program (`tests/cli.rs`). `OXIDEMERGE_CONFIG_DIR` points the program at another
  settings folder, window size and place included, so tests never touch the real one.
- **Window smoke test** (`make gui-smoke`, `scripts/gui-smoke.sh`): opens real windows
  with the release build, two files, three files, two folders, and checks what each tab
  shows. CI runs it on Windows (with Mesa's software renderer), Linux (Xvfb) and macOS and
  keeps the output of a failed test.
- **Benchmarks** of text, 3-way, binary, image and folder compares, timed with their peak
  memory (`OXIDEMERGE_BENCH_*`); `docs/benchmarks.md` has the method and a first baseline,
  and a CI run started by hand can run them on every platform.
- **Dependency checks** with `cargo deny` (`deny.toml`) for known vulnerabilities,
  licenses and sources, in CI and before every release, and `SECURITY.md` on reporting
  vulnerabilities and on which inputs OxideMerge treats as untrusted.
- **CI checks the packages:** the Windows `.zip` must hold the program, README and
  license; the Linux `.deb` is installed, run and removed, leaving the user's settings
  alone; the macOS app must hold both architectures. Before publishing, the release job
  checks that every package is there and that the version was not released before, and the
  release notes name the commit built.
- **Documentation:** a Quick start in the READMEs, a Compare types section and a longer
  Troubleshooting and limitations chapter in the user guides, and `docs/macos-signing.md`
  on Gatekeeper and notarization.

### Changed

- **Folder auto merge** checks each item again just before it changes it. Files are
  replaced through a temporary file, so a crash or a full disk leaves them whole, and are
  kept as `.bak` with backups on. Nothing is changed when the folders changed since they
  were compared. A file and a folder of the same name, symbolic links, special files and
  items the compare could not read are conflicts left alone; links are never followed, and
  names that would lead outside the middle folder are refused. A step that fails is
  reported and the others still run. Copies in the folder compare are made through a
  temporary file too, and keep the file's modification time.
- Reports, patches and saved comparisons are written through a temporary file, so a failed
  export never leaves half a file behind. Devices such as `/dev/stdout` are written
  directly.
- **Web page compare:** a web page may not load local files, and only a page on this
  computer or a private network may load resources from one or be redirected to one; such
  resources are skipped and counted. A page counts as local only when every address of its
  host is; IPv4 addresses inside IPv6 ones (mapped, NAT64, 6to4) are judged by the address
  they hold. Redirects are followed by OxideMerge itself, at most 10; a redirect without a
  target is shown as it is, and one to anything but a web address says so. A byte order
  mark now decides the encoding before the HTTP header and the `<meta>` charset.
- Language files in a `Languages` folder are checked: messages with other placeholders
  than the English ones, unknown keys and non-text values are left out, and English is
  shown instead.
- The menus of the menu bar all open as wide as the widest one, in every language.
- macOS instructions describe **Open Anyway** in System Settings, as Control-click → Open
  no longer bypasses Gatekeeper from macOS 15 on.

### Fixed

- Applying a patch made without context lines (`diff -U0`) put added lines one line too
  early, and in the wrong place when hunks before them were left out of the patch. A hunk
  found away from its stated line now moves the hunks after it by as much, as GNU patch
  does, so a line added after it no longer lands too early.
- A difference next to a moved insertion or deletion could point at the wrong line of the
  side where it is empty.
- A folder compare that broke off with an internal error showed the folders as identical;
  it now says it failed and counts as an error.
- A text compare whose syntax highlighting stopped with an error kept redrawing the
  window.
- Settings that could not be saved were dropped without a word; the error is now shown
  once, and saving is tried again until it works and when the program closes.
- Settings that could not be read were replaced by the defaults when settings were next
  saved. The file is now kept as `config.json.bad` (or `oxidemerge.json.bad`) and a
  message says so; a settings file that can't be opened at all is not saved over. An empty
  file still holds the defaults, as for portable use.
- With the Recycle Bin option on, a file or folder that could not be moved to the trash
  was deleted for good without a word; it is now left where it is and the error is shown.
- Folder auto merge could miss a file that changed while the merge was being planned.
- On Windows, copying a file over a read-only one in the folder compare or folder auto
  merge wrote into it in place; it is now replaced through a temporary file and stays
  whole if the copy fails.
- The Filters window opened folders with `xdg-open` on macOS; errors opening files and
  folders from the folder compare and the dialogs are now shown.
- On Windows, *Show in file manager* did not select files whose path has a space or a
  comma.
- Changing a file's line ending in a text compare could not be undone, and undoing an
  earlier change cleared the mark that the file was modified, so closing it lost the new
  line ending.
- Folder auto merge merged binary files as text, and dropped a change of encoding, byte
  order mark, line endings or final newline made on one side. Only text files that read
  back to the same bytes are merged now, the change of either side is kept, and different
  changes on both sides are a conflict. A side left without line breaks no longer counts
  as changing the line ending, and the encoding and byte order mark are merged together,
  so UTF-16 never loses its byte order mark.
- Folder auto merge with backups on could replace a compared file with another file's
  `.bak`; such a step is now left alone. A folder added on both sides is now made in the
  middle folder, even when it is empty, and its contents are merged item by item.
- A read-only `.bak` left from before stopped the next backup of that file. On Linux and
  macOS, a read-only file written in place is made writable for its owner only, and
  read-only again when the write fails.
- Files kept under `/dev/`, such as `/dev/shm`, were written in place and without a
  backup, like devices.
- Resources of a web page saved under one name were numbered `a~2`, `a~2~3`, ... until the
  name got too long; they are now `a~2`, `a~3`, ...
- On Windows, saving over a read-only file failed and left a temporary file next to it;
  the file is now replaced and stays read-only, as on Linux and macOS.
- Text copied into a new or empty file in a text compare got this computer's line endings;
  the file now takes the encoding and line endings of the file copied from, as in All
  differences. Take both and Auto merge into an empty middle file do the same, and undo
  puts back only what copying changed.

### Security

- Unpacker commands could run part of a file name as a command; the file path is now
  passed as an argument (`"$1"`) on Linux and macOS, and as an environment variable the
  command line quotes on Windows, where unpackers given the quoted path in the command
  line also failed to run. Quotes written around `${file}` are dropped, as they would
  leave the path unquoted.
- On Windows, *Open with* and opening a file from the folder compare went through
  `cmd start`, so a file name containing `&` could run a command; files are now opened by
  the shell directly.
- Resource tree downloads could write outside the chosen folder through an encoded `/`
  (`%2F`) in a resource address.
- The web size limit counted compressed bytes, so a small compressed response could unpack
  to fill memory; it now counts the bytes unpacked. A page of deeply nested `<head>` tags,
  or with a title tag left open in `<head>`, crashed the program.
- On Windows, a local page in a resource tree compare could load `file://host/share`
  resources, which opens a connection to that computer and hands it the user's
  credentials; files of other computers are now skipped.
- Archives that unpack to more than 8 GB or 500,000 files are refused, so a small archive
  can no longer fill the disk; `.tar.xz` archives are no longer unpacked into memory.
- Links in `.tar` archives that point outside the archive, directly or through other
  links, are no longer extracted.
- Web pages can no longer read local files or reach this computer and the private network
  through their resources or redirects (see Changed).

## [0.9.3] - 2026-10-26

### Added

- A user guide in AsciiDoc, in English (`docs/user-guide.adoc`) and Traditional Chinese
  (`docs/user-guide.zh-TW.adoc`), with screenshots of each view in both languages, linked
  from the READMEs. CI builds both as PDF files, attached to each GitHub release;
  `make user-guide` builds them locally.
- **All differences** (folder toolbar, Compare menu or command palette): every changed
  text file of a folder compare in one tab, with its differences side by side as in the
  HTML report, a file list with search, expand and collapse all, next and previous
  difference across files, copying a difference (or, with two sides, a whole file) to
  another side, undo and save (the folder compare then refreshes). The files are those the
  folder compare shows, its filters applied. Copying from a side where the file is missing
  deletes it when saved, with folders left empty (with backups on, the file is kept as
  `.bak`), a file changed on disk since it was read is never saved over and its card can
  discard the changes to read it again, text compares of saved files notice the change,
  opening the tab again lists files with differences found since, and the text of files
  far out of view is let go and read again when needed, keeping memory use down.
- **Numeric tolerance** (Options → Compare, or `--numeric-tolerance`): numbers at most a
  set amount apart count as equal, in text and folder compares.
- **Block filters** in compare rules files: ignore everything between a start line and an
  end line matching regular expressions.
- **Compare rules files**: lines to ignore, substitutions and blocks to ignore are kept in
  `.oxrules` files instead of the options. Options → Compare rules picks the default file
  and edits, creates or deletes them. A folder compare picks its own file from the
  toolbar, and Refresh (F5) reads it again. The context menu of a text compare adds a rule
  from the clicked or selected lines, and Compare → Compare rules switches the file of the
  active compare. Rules files are exported (one, or all to a folder, never replacing files
  already there) and imported in Options → Compare rules, or imported by dropping one
  `.oxrules` file on the window (or several on the options; two dropped on the window are
  compared). Renaming or deleting a rules file updates the compares using it, and restored
  session tabs keep their own file. A selected file that can't be read stays selected,
  with a message, and compares run without it. Projects and the `--rules` option name a
  file too (`--rules ""` for none). Rules set in the options of earlier versions move to a
  "Default" file.
- **Line detail pane** (View → Text compare): the current line of every side, one above
  the other and wrapped.
- **Edit markers** in the gutter for lines changed since the files were opened.
- **Statistics** (Tools menu): line counts per pair of sides for text compares, and file
  counts and sizes for folder compares.
- **Indent and unindent** the selected lines (`Ctrl+]` / `Ctrl+[`).
- **Bookmark comments**, shown when hovering the gutter, in the status bar and in HTML
  reports.
- **Save comparison** (File menu): every side's text with its bookmarks and comments in
  one `.oxmcmp` file, which opens again read-only.
- **Compare with a Git revision** (File menu, or `--git-rev`) for files and folders. The
  revision side is read-only and has the files as a checkout would, including Git LFS
  files and submodules.
- **File compare below the folder list** (View → Folder list): the selected file's
  differences without opening a tab.
- **Folder report with file differences** (File menu, or `--report-diffs`).
- **New HTML reports** for text and folder compares: one self-contained page listing every
  difference as a card with line numbers, context and highlighted words. Summary cards, an
  index of files and differences, side-by-side or unified view, light and dark themes,
  status filters, search, and `N` / `P` to jump between differences. Reports name the
  compare rules file used and count the differences it ignored. Folder reports with file
  differences list those files first, skip binary files and files over 16 MB, and cut
  lines after 2,000 characters; text reports stop after 20,000 rows and count the
  differences left out. Reports are made in the background. Printing opens every file and
  keeps the colors.
- **Export HTML** button on the toolbar of text and folder compares, also in the File menu
  and the command palette (`Ctrl+Shift+E`): saves the HTML report, with the differences of
  every changed file for a folder compare, under a name taken from the compared files
  (spaces written as `_`), and opens it in the browser (File → Export → Open reports in
  the browser after saving). The file compare docked below a folder compare exports its
  own report.
- **Copy to folder** and **select by name** in folder compares.
- **3-way folder auto merge** into the middle folder, merging text files changed on both
  sides when the changes don't overlap. Files hidden by the filter, and files changed
  after the merge was planned, are left alone.
- **Arm64 packages** for Windows (`.exe`, `.zip`) and Linux (AppImage, `.deb`, `.tar.gz`),
  built and tested on Arm machines, next to the x64 and macOS universal packages.

### Changed

- The sides of an archive compare are read-only: they are temporary copies, and changes to
  them were lost.
- **Menus regrouped** by what they act on. A new Compare menu holds stepping through
  differences, Compare as, Compare rules, the compare options and Refresh. File gains Open
  recent and Export (HTML report, reports, patch) submenus and takes Apply patch from
  Tools. Edit groups the bookmark commands in a submenu. View has the Folder list and Text
  compare submenus, Zoom, Theme, and next/previous tab. Commands that do nothing in the
  current tab are greyed out instead of hidden or silently ignored.
- Shortcuts in menus, the command palette and tooltips come from the same table as the
  keys themselves, and on macOS are written the macOS way (`⇧⌘S`, `⌥↓`, `⌘Q` for Exit)
  everywhere.
- Next and previous tab are `⌃Tab` and `⌃⇧Tab` on macOS, where `⌘Tab` switches
  applications.
- A key held with Alt no longer also runs the command of the key alone (`Alt+F4` no longer
  goes to the next bookmark first), and the command palette lists only what works in the
  current tab.

### Fixed

- `⌘Q` on macOS asks about unsaved changes like closing the window does, instead of
  quitting at once, also when the window is minimized or the app hidden and when quitting
  from the Dock, at logout or from a script.
- Saving text in a legacy encoding (Big5, Shift_JIS, ...) that can't hold some of its
  characters is refused with a message, instead of writing them as `&#...;` entities.
- A relative path given to `--rules` or in a project file is read from where it was given
  (the current folder, or the project's folder), and a missing rules file is reported.
- Rules files saved with a byte order mark are listed; a name ending in `.oxrules` is not
  doubled, names differing only in case (where file names keep case) are both listed, and
  names compare without case beyond ASCII. The rules editor shows the name a file will be
  saved under, takes `a =>` without a space after the arrow, marks lines with an empty
  side, refuses to save while lines are unclear, and asks before Cancel or closing the
  options throws edits away.
- Adding a rule to a rules file given by path that can't be read no longer writes another
  file.
- Close all, Close others and Close tabs to the right ask about each tab with unsaved
  changes in turn, not only the first; Esc cancels the close and exit prompts.
- New files are created with the usual permissions instead of readable only by their
  owner.
- Folder reports with file differences share one size limit across files, like text
  reports.
- Reports no longer name a compare rules file that could not be read, as the compare ran
  without it.
- Saving in All differences says which files had mixed line endings written as one kind,
  and a text compare stops showing "mixed" for a file once it is saved.
- A file with one difference says "1 difference" in All differences and in HTML reports.
- Three-way reports colour only the sides that changed. The CSV report starts with a byte
  order mark so spreadsheets read it as UTF-8, and keeps names like `-2+3+cmd|...` as
  text.
- Saving no longer empties a file when the disk is full or the disk quota is used up: a
  save that fails leaves the file as it was, and no empty file when it was new. The
  options are saved the same way.
- Compare options given on the command line (`--ignore-case`, `--lang`, ...) apply to that
  run only, instead of becoming the saved options the next time an option changed. Each
  one is kept only if it was changed in the program afterwards.
- Patches leave out the differences the compare options ignore, as `diff -B` does, except
  inside a hunk with other changes, where leaving them out would make the patch drop
  lines. The console compare of three binary files reads the third one, and its byte
  ranges end at the last byte that differs.
- Reports and patches of a folder compare wait until it has finished, and quitting waits
  for a report still being written. Report file names get `.html` when typed without it.
- Adding a rule to a typed name with a folder in it (`Team/Logs`) saves it in the rules
  folder (as `Team_Logs`) instead of a file without extension in the current folder.
  Importing a rules file named like one next to the program adds it under a free name
  instead of hiding that one, and a rules file dropped on the window updates the open
  compares. Renaming or deleting a rules file updates the compares using that very file,
  telling names apart by case where file names keep it. A project opened by a relative
  path reads its rules file from the project's folder. The Compare rules menus tick a
  rules file used by its path only when it is the one listed, not another file of the same
  name.
- The rules editor keeps the file of a rules file whose name is not a usual file name
  while the name is unchanged, renames it back when saving after a change of case fails,
  warns again about unsaved edits after further typing, and checks the patterns that will
  be saved.
- All differences reads files only as fast as the tab shows them and also lets go of their
  rows far out of view, keeping memory down; reads files changed on disk in the
  background, a few at a time; no longer replaces saved text with a reading made before
  the save; takes the line endings along with a whole-file copy; counts files saved rather
  than sides; says when files are still being compared, when an undo can't be applied, and
  drops the "stopped" note once the folder compare has finished.
- Closing several tabs and then quitting no longer goes back to asking about the other
  tabs, and files opened from an archive compare keep its extracted copy after the folder
  compare closes.
- The console compare exits with 2 when files could not be compared, and folders left
  uncompared (not recursive) no longer count as differences. `--numeric-tolerance` refuses
  NaN and infinity, and options that are ignored (`--patch` with three paths,
  `--report-diffs` without `--report` or for files, a text report not named `.html`) say
  so.
- Shortcuts of commands that do nothing in the current tab do nothing (`Ctrl+Shift+Z` no
  longer runs Undo where there is no Redo), F5 refreshes web pages shown as screenshots,
  the Filters window no longer turns the shortcuts off, files dropped while a dialog is
  open are ignored, and Enter in the command palette runs the command matching what was
  just typed.
- A text compare whose file changed on disk while one of its lines was being edited no
  longer says it reloaded it, and a file that could not be read when it changed is tried
  again instead of being reported as reloaded.
- `Ctrl+Tab` no longer leaves the keyboard focus on a button, which turned off the arrow
  keys and the other keys of the compare until it was clicked.
- `Ctrl+S`, closing a tab and quitting take in a line still being typed in place, instead
  of saving without it or closing without asking.
- Closing a tab left of the current one no longer switches to another tab.
- Keys no longer reach the compare behind the Add compare rule dialog.
- Copying a file or folder onto itself (both sides pointing at the same place) no longer
  empties the file; copying a folder into itself, or through a link back to itself, is
  refused.

## [0.9.2] - 2026-10-24

### Added

- **Command palette** (`Ctrl+K` or `Ctrl+Shift+P`): search commands, recent comparisons
  and settings in one place.
- **Find and replace** in text compares (`Ctrl+H`, `Cmd+Alt+F` on macOS), with match case,
  whole word and regular expression options. Replace all can be undone in one step.
- **Line selection** in text compares by dragging, Shift+click or Shift+arrow keys.
  `Ctrl+C` copies the selected lines, and `Alt+Right` / `Alt+Left` copies them to the
  other side.
- **Collapse unchanged lines** (`Alt+C`) to a clickable bar, keeping three lines of
  context around each difference, and **word wrap** (`Alt+Z`).
- **Merge arrows** on the divider between panes copy a difference in one click. The
  toolbar shows how many lines were added and removed.
- **Next file with differences:** in a file opened from a folder compare, `Alt+Down` past
  the last difference (or `Alt+Up` past the first) opens the next (or previous) file that
  differs, in the same tab.
- **Quick preview** in folder compares: press `Space` to see a colored diff of the
  selected file without opening a tab.
- **Find by name** in folder compares, with clear messages when nothing matches or every
  item is hidden by the View settings.
- **Results while comparing:** folder compares list differences and unique items as soon
  as they are found.
- A notification when a long comparison finishes while the window is in the background.
- **Binary compare:** find hex bytes (`DE AD BE EF`) or text, and go to an offset
  (`Ctrl+G`).
- **Image compare:** swipe mode with a draggable divider, and blink mode that alternates
  the images. Pinch to zoom and two-finger scrolling to pan.
- **Optional backup on save:** keep the previous contents as `<name>.bak` (Options →
  General).
- **Theme setting** with Light, Dark and Match system, and a search box in Options.
- **Reopen previous comparisons at startup** (Options → General).
- Recent comparisons on the start page can be edited or removed from their context menu.
- Dropping files on the window shows what will happen; dropping onto the Open dialog fills
  in the empty paths.
- **Windows Explorer context menu:** an OxideMerge submenu with
  Compare, Select Left, Select Middle and Compare As, also available from the command line
  as `--select-left`, `--select-middle` and `--compare-to`.
- **Finder Quick Actions and Linux file manager entries:** Tools → Add Finder Quick
  Actions on macOS, or Add to file manager context menu on Linux (Nautilus, Nemo and
  Dolphin), offers Compare, Select Left and Select Middle for the selected items.
- **New text compare** (`Ctrl+N`): two empty sides to paste (`Ctrl+V`) or type text into,
  saved wherever you choose.
- **Paste** (`Ctrl+V`) in text compares inserts the clipboard below the current line, or
  replaces the selected lines.
- **Bookmarks** in text compares: `Ctrl+F2` sets or removes one, `F4` / `Shift+F4` jump
  between them. They are marked in the gutter and the location pane, and move with their
  lines as you edit.
- **Take both sides** in 3-way merges: Merge → Take left, then right (or right, then left)
  puts both changes into the result.
- **Ignore comment changes** (Options → Compare): differences only in comments don't
  count, for C-like languages, scripts, SQL, HTML/XML, CSS, Lua, Haskell and more. Also in
  folder compares and as `--ignore-comments`.
- **Format JSON and XML before comparing** (Options → Compare): files are shown
  re-indented, with JSON keys sorted, so only real content changes remain. Also in folder
  compares and as `--prettify`.
- **Apply patch** (Tools → Apply patch): applies a unified diff (plain, `git diff` or
  `svn diff`) even when lines have moved, and opens each patched file next to its original
  for review before saving. Hunks that don't fit are reported.
- **HTML report for text compares:** File → Generate report saves a side-by-side colored
  report with word differences, also from the command line with `--report`.
- **Project files:** File → Save as project stores what a tab compares (paths, filter,
  subfolders, compare method) in a `.oxmproj` file; File → Open project, the command line
  and drag and drop open it again. WinMerge `.WinMerge` projects open too.
- **More archive formats compared as folders:** `.tar`, `.tar.gz` / `.tgz`, `.tar.bz2` /
  `.tbz2`, `.tar.xz` / `.txz` and `.7z`, besides zip-based formats.
- **Git integration** instructions for using OxideMerge as `git difftool` and
  `git mergetool`.
- `make appimage` builds the Linux AppImage locally.
- Traditional Chinese README (`README_zh-TW.md`).

### Changed

- Large text files with many differences compare up to a hundred times faster, and no
  comparison takes more than a few seconds.
- Folder compares read large files in chunks, so identical and binary files are never
  loaded whole, and files of different sizes are told apart without reading them.
- Saving is crash-safe: a crash or a full disk never leaves a half-written file. File
  permissions and symbolic links are kept.
- The Language menu was removed from the menu bar; choose the language in Options →
  General.
- The pattern and command fields in Options → Plugins are wider.

### Fixed

- When saving a merge result to another file with `-o` (as `git mergetool` does), the
  result is now always written there, even if it equals one of the inputs.
- The application icon no longer shows a white fringe on dark backgrounds.

## [0.9.1] - 2026-10-23

### Added

- **Web page compare** of two or three URLs or local HTML files, in four modes: Screenshot
  (rendered with Edge, Chrome, Chromium or Brave), HTML source, visible text, and the tree
  of downloaded resources.
- **Help & Shortcuts** dialog (`F1` or `?`) listing every keyboard shortcut and mouse
  action, and an About dialog with credits.
- Files changed by another program are detected when the window regains focus: unmodified
  files reload automatically, and files with unsaved edits offer to reload.
- Keyboard selection in folder compares: arrow keys, Page Up/Down, Home/End, Shift to
  extend and `Ctrl+A` to select all.
- Save all (`Ctrl+Shift+S`), redo with `Ctrl+Shift+Z`, `Esc` to close the find bar, and a
  tab context menu to close other tabs, tabs to the right, or all tabs.
- **Release packages:** Windows `.exe` and `.zip`; Linux AppImage, `.deb` and `.tar.gz`; a
  universal macOS app (Apple silicon and Intel) in a `.dmg` and a `.zip`; and
  `SHA256SUMS.txt`.
- The Windows executable has an icon and version details.

### Changed

- Linux file dialogs use the XDG desktop portal, and building on Linux no longer needs
  development packages.

## [0.9.0] - 2026-10-22

First release.

- **Folder compare and merge:** 2-way and 3-way, recursive or flat, as a tree or a flat
  list. Six compare methods, file filters (wildcard masks and WinMerge `.flt` files) with
  a Clear filter option, zip-based archives compared as folders, and HTML, CSV and text
  reports.
- **Text compare and merge:** 2-way and 3-way with Myers, Patience or LCS, inline word and
  character differences, moved blocks, a location pane, ignore options,
  copy and auto merge, undo and redo, in-place editing, conflict file resolution, syntax
  highlighting, find, go to line, CSV/TSV table mode, patches, and unpacker plugins.
- **Binary compare** in a side-by-side hex view.
- **Image compare** side by side, as an overlay or as a difference image, with highlighted
  difference blocks.
- Tabs, a start page with recent comparisons, drag and drop, light and dark themes, 10
  languages, and the Windows Explorer context menu.
- Command-line compares, reports and patches without opening a window, and WinMerge-style
  switches on Windows.
- Chinese, Japanese and Korean fonts on Linux and WSL, and GPU acceleration under WSL2.

[0.9.8]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kuopinghsu/OxideMerge/releases/tag/v0.9.0
