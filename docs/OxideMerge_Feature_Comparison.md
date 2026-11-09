# Feature Comparison: OxideMerge vs. WinMerge vs. Meld vs. Beyond Compare vs. Araxis Merge

**Review date:** November 9, 2026\
**OxideMerge reference:** `main` at `54a85e8`, with the changes not yet committed on November 9, 2026\
**Version:** 0.9.8 (`Unreleased` in CHANGELOG; not a claim that a v0.9.8 binary is published).\
**Other products, as documented on that date:** WinMerge 2.16.60, Meld 3.24.1, Beyond Compare 5.2.7 (Standard and Pro), Araxis Merge 2026.1 (Standard and Professional).

**Legend:**

| Mark | Meaning |
|:---:|---|
| ✅ | Documented by the vendor, in the base product |
| ◐ | Documented, but partial, limited to an edition (footnote), to one platform, to a plugin, or experimental |
| ❌ | Not found in the vendor's feature list or manual |
| ? | Not checked against the vendor's documentation |

Footnotes in a cell: **ᴾ** Beyond Compare Pro or Araxis Merge Professional only · **ᵂ** Windows only · **ᴱ** documented as experimental.

> This is a documentation-based comparison, not hands-on feature or performance validation. ✅ does not imply equal depth, implementation quality, or usability. ❌ means the documentation reviewed did not establish a capability; it is not proof that the capability is impossible. Rows describing OxideMerge-specific features (journals, review decisions, impact analysis and similar) are ❌ for another product whenever its documentation describes nothing comparable.

## Revision notes

**November 9, 2026**

- OxideMerge's notes were rechecked against the code: note 2 says unchanged ZipCrypto entries are encrypted again, note 6 says what a synchronization job needs (`"apply": true` and `--yes`), and note 7 describes how Windows Subversion reads a path that is not ASCII and that a Perforce server can ask for a sign-in too.
- OxideMerge on Linux was checked with OpenSSH 10.5p1 and vsftpd 3.0.5. SFTP with a private key refuses a server it has not been told to trust and shows the fingerprint `ssh-keygen -lf` prints; after `--trust-fingerprint`, an identical file exits 0, a different file exits 1, a folder compare names the changed file, and the window opens the file as writable. A sync job uploads a new file, a shorter upload replaces it in place, and an upload is refused when the server file has changed since the fetch. Anonymous FTP does the same for a file, a second sync updates the uploaded file, and `--ftp-active` compared an identical file (exit 0). An address that contains a password is refused. See note 1.
- OxideMerge on Linux was checked with Git 2.55.0, Subversion 1.14.5, Mercurial 7.2.4 and p4/p4d 2026.1 (3089746) against local repositories and a local p4d: *Compare with revision* for Git, Subversion and Mercurial, including a Subversion path containing `@` and letters that are not ASCII (passed to `svn` directly) and a Mercurial path with brackets, and merges from `git mergetool`, `svn update --accept launch`, `hg merge` and `p4 resolve`, both saved and closed without saving, with no wrapper. *Compare with revision* does not cover Perforce. On macOS and Linux, p4 names those temporary files like `tmp.1.2.3`, which the merge recognizer already accepts. Closing a `p4 resolve` without saving makes p4 report *Execution Failed* and *No such file or directory*; that is exit code 1, and the file stays unresolved. P4V 2026.1 (2933292) on Linux uses its own argument string: diff opens the depot revision on the left and the workspace file on the right, saving then Accept Merged resolves the file, and closing without saving leaves P4V's markers and the file unresolved. See note 7.
- OxideMerge: *Semantic three-way merge* moves from ✅ to ◐, because the three-way Semantic view resolves each path with *Use left* or *Use right* and has no apply-all; a new row, *Semantic three-way conflict resolution*, rates that as ✅ (note 11). Portable execution stays ✅ with a caveat (note 10).
- OxideMerge: rows were added for features the table left out: printing, ignoring line-ending differences, conversion through external programs (unpackers), OpenDocument and RTF documents, image blink and swipe, and password-protected archives. Their competitor cells are marked only where the vendor's pages say so.
- Competitor cells were rechecked against the current versions and the ? cells filled where the vendor's documentation decides them. Notable corrections:
  - WinMerge documents rename and move detection (folder compare options), an experimental patch-application plugin, and YAML only through pretty-print plugins; its version-control page names no Mercurial or Perforce integration, and its reports are printed, not written as PDF.
  - Meld does not align changes by inserting filler lines (its help says there is no option to), so *Aligned row layout* is ❌; Meld 3.23.0 added side-by-side image comparison (◐, no overlay or difference image). Its CSV, HTML and rename ratings were not backed by its documentation and are now ❌.
  - Araxis Merge 2026.1 removed its Perforce, Subversion and FTP file-system plugins; it remains an external diff and merge tool for Perforce and Subversion clients (note 12). Its folder synchronization copies files between hierarchies, as WinMerge's does, so both are ◐, and neither shows a synchronization preview.
  - Beyond Compare documents conversion of Word, PDF and RTF files, ignoring line-terminator style, and JSON and XML only as additional file formats that reformat the text (◐).

**November 8, 2026**

- Every competitor cell was rechecked against the vendor sources listed under [Sources](#sources), and the legend now separates "not documented" (❌) from "not checked" (?). Notable corrections:
  - WinMerge has no FTP/SFTP and no mirror or update-only synchronization (copy commands only); it does have XML folder reports, three-way file and folder comparison, three-way merge, editable table and binary views, synchronization points, and numeric differences only as *Ignore numbers*.
  - Meld does not support Perforce (Git, Mercurial, Bazaar and Subversion only), detects no moved blocks, writes no HTML reports, applies no patches and has no portable install; it does have synchronization points and three-way folder comparison.
  - Beyond Compare has no slideshow report and no moved-block detection; three-way text merge, three-way folder merge, SFTP and cloud storage are Pro-only; it does have editable table and hex views, XML reports, portable installs and Update/Mirror sync with a preview.
  - Araxis Merge documents HTML slideshow reports, numeric tolerance, read-only Git and Mercurial file-system plugins and Perforce integration in the Standard edition; it documents no FTP/SFTP, table comparison or multi-cursor editing.
- OxideMerge: **Subversion, Mercurial and Perforce** are supported (✅). On Windows this was checked with SlikSVN 1.14.2, Mercurial 7.1.2 and p4/p4d 2026.1: *Compare with revision* for Subversion and Mercurial, including a Subversion path Subversion itself cannot take as an argument, and merges from `svn update --accept launch`, `hg merge` and `p4 resolve` against a local p4d, both saved and closed without saving, with no wrapper. *Compare with revision* does not cover Perforce. P4V's own argument string was not rechecked on Windows. See note 7.
- OxideMerge: **saved automation jobs** are documented: job files (`--job`) run reports, patches and synchronizations in order and print a JSON summary.
- OxideMerge: **archive write-back** now covers `.tar.xz` and archives nested in other archives, and keeps a 7z's untouched compressed blocks as they were; a `.tar.xz` or 7z, slow to compress, is written in the background. Password-protected ZIP (ZipCrypto and WinZip AES) and 7z archives (encrypted names included) now open after asking for the password and are saved still encrypted with it; ZIP entries compressed with bzip2, LZMA, xz, Zstandard, PPMd or Deflate64 are read, a changed entry keeps its method (Deflate for LZMA and Deflate64, which can only be read), and `.tar.zst` was added. Edit and save inside archives moves from ◐ to ✅; signed packages stay read-only on purpose, since any change would break their signature, and RAR is not supported (see note 2).
- OxideMerge: **FTP and FTPS** (explicit and implicit TLS) were added alongside SFTP, SFTP can sign in through `ssh-agent`, the remote path can be chosen from a list of the server's folders and files instead of typed, and **remote folders are writable**: *Save* uploads new and changed files and deletes removed ones, refusing any file changed on the server meanwhile, and *Reload from server* fetches again. Server paths (`sftp://`, `ftp://`, `ftpes://`, `ftps://`) also work on the command line and in job files. SFTP follows `~/.ssh/config` (aliases, keys, `ProxyJump`, `ProxyCommand`, `Include`, `Match`), answers keyboard-interactive sign-in questions such as one-time codes, and accepts host certificates from a `@cert-authority`; SFTP and FTP can go through SOCKS5 or HTTP CONNECT proxies, FTP supports active mode, and FTPS can present a client certificate. FTP / SFTP and Remote folder comparison move from ◐ to ✅.
- OxideMerge: **word-level differences** now follow the comparison rules' substitutions and *Ignore numbers*, so text a rule treats as equal, such as a timestamp, is no longer highlighted inside a changed line. No rating changed.
- OxideMerge: the **Docker test environment** (`scripts/docker-test.sh`) now runs the remote tests against real servers on Linux:
  - OpenSSH: keys, passphrases, host certificates, jump hosts, `ProxyCommand` and keyboard-interactive sign-in.
  - vsftpd and ProFTPD: FTP, explicit and implicit FTPS, active mode and a client certificate.
  - Dante and tinyproxy: SFTP, FTP and FTPS through SOCKS5 and HTTP CONNECT proxies.

  The tests are built for release, as continuous integration builds them. One fix came out of this: an HTTP proxy that refuses a sign-in with `401` rather than `407`, as tinyproxy does, is now reported as a proxy sign-in failure. No rating changed.

**November 7, 2026**

- OxideMerge: SFTP file and folder comparisons, archive write-back for selected unsigned ZIP/TAR/7z formats, Jupyter notebook cell-by-cell text extraction, clipboard comparison, named workspaces and binary moved-block detection were added.
- Beyond Compare three-way merging (Pro) and Araxis three-way comparison and merging (Professional) were marked as edition-limited.

## Comparison tables

### 1. Platform and General Features

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Windows | ✅ | ✅ | ✅ | ✅ | ✅ |
| macOS | ✅ | ❌ | ◐ | ✅ | ✅ |
| Linux | ✅ | ❌ | ✅ | ✅ | ❌ |
| Open source | ✅ | ✅ | ✅ | ❌ | ❌ |
| Free to use | ✅ | ✅ | ✅ | ❌ | ❌ |
| Portable execution | ✅ | ✅ | ❌ | ✅ | ❌ |
| Dark / Light theme | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multiple languages | ✅ | ✅ | ✅ | ◐ | ◐ |
| Command-line interface | ✅ | ✅ | ✅ | ✅ | ✅ |
| File manager integration | ✅ | ✅ | ❌ | ✅ | ✅ |
| Printing | ✅ | ✅ | ? | ✅ | ✅ |

### 2. File and Text Comparison

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Two-way file comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Three-way file comparison | ✅ | ✅ | ✅ | ◐ᴾ | ◐ᴾ |
| Three-way merge | ✅ | ✅ | ✅ | ◐ᴾ | ◐ᴾ |
| Side-by-side comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Inline / unified differences | ✅ | ◐ | ◐ | ◐ | ◐ |
| Word-level differences | ✅ | ✅ | ✅ | ✅ | ✅ |
| Moved block detection | ✅ | ✅ | ❌ | ❌ | ❌ |
| Syntax highlighting | ✅ | ✅ | ✅ | ✅ | ✅ |
| Inline editing | ✅ | ✅ | ✅ | ✅ | ✅ |
| Undo / Redo | ✅ | ✅ | ✅ | ✅ | ✅ |
| Search and replace | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ignore whitespace | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ignore line-ending differences | ✅ | ✅ | ? | ✅ | ✅ |
| Ignore case | ✅ | ✅ | ◐ | ✅ | ✅ |
| Ignore comments | ✅ | ✅ | ◐ | ✅ | ✅ |
| Regex-based ignore rules | ✅ | ✅ | ✅ | ✅ | ✅ |
| Manual alignment points | ✅ | ✅ | ✅ | ✅ | ✅ |
| Synchronized scrolling | ✅ | ✅ | ✅ | ✅ | ✅ |
| Aligned row layout | ✅ | ✅ | ❌ | ✅ | ✅ |
| Bookmarks | ✅ | ✅ | ❌ | ✅ | ✅ |
| Conflict marker resolution | ✅ | ✅ | ◐ | ❌ | ❌ |
| Multi-cursor text editing | ✅ | ❌ | ❌ | ❌ | ❌ |
| Clipboard text comparison | ✅ | ✅ | ❌ | ✅ | ✅ |

### 3. Folder Comparison and Synchronization

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Two-way folder comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Three-way folder comparison | ✅ | ✅ | ✅ | ◐ᴾ | ◐ᴾ |
| Recursive comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder filters | ✅ | ✅ | ✅ | ✅ | ✅ |
| Content-based comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Copy between folders | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder synchronization | ✅ | ◐ | ◐ | ✅ | ◐ |
| Mirror synchronization | ✅ | ❌ | ❌ | ✅ | ❌ |
| Update-only synchronization | ✅ | ❌ | ❌ | ✅ | ❌ |
| Synchronization preview | ✅ | ❌ | ❌ | ✅ | ❌ |
| Rename / move detection | ✅ | ✅ | ❌ | ◐ᴾ | ❌ |
| Stale-file revalidation | ✅ | ❌ | ❌ | ❌ | ❌ |
| Recovery journal | ✅ | ❌ | ❌ | ❌ | ❌ |
| Rollback on synchronization failure | ✅ | ❌ | ❌ | ❌ | ❌ |
| Three-way folder merge | ✅ | ◐ | ◐ | ◐ᴾ | ◐ᴾ |
| Remote folder comparison | ✅ | ❌ | ❌ | ✅ | ❌ |
| FTP / SFTP | ✅ | ❌ | ❌ | ◐ᴾ | ❌ |
| Cloud storage integration | ❌ | ❌ | ❌ | ◐ᴾ | ❌ |
| Remote file editing and upload (SFTP/FTP) | ✅ | ❌ | ❌ | ◐ᴾ | ❌ |

### 4. Specialized File Formats

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Binary comparison | ✅ | ✅ | ❌ | ✅ | ✅ |
| Hex viewer | ✅ | ✅ | ❌ | ✅ | ✅ |
| Binary editing | ✅ | ✅ | ❌ | ✅ | ❌ |
| Binary moved-block detection | ✅ | ❌ | ❌ | ❌ | ❌ |
| Image comparison | ✅ | ✅ | ◐ | ✅ | ✅ |
| Image overlay | ✅ | ✅ | ❌ | ✅ | ✅ |
| Image blink / swipe | ✅ | ◐ | ❌ | ? | ? |
| Image difference view | ✅ | ✅ | ❌ | ✅ | ✅ |
| CSV / TSV comparison | ✅ | ✅ | ❌ | ✅ | ◐ |
| Table cell editing | ✅ | ✅ | ❌ | ✅ | ❌ |
| Key-based row matching | ✅ | ❌ | ❌ | ✅ | ◐ |
| Numeric tolerance | ✅ | ◐ | ❌ | ✅ | ✅ |
| PDF text comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Word DOCX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Excel XLSX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| OpenDocument and RTF comparison | ✅ | ◐ | ❌ | ◐ | ✅ |
| Office layout fidelity | ◐ | ◐ | ❌ | ◐ | ◐ |
| Conversion through external programs | ✅ | ✅ | ? | ✅ | ? |
| ELF binary structure comparison | ✅ | ❌ | ❌ | ❌ | ❌ |
| HTML / web page comparison | ✅ | ◐ᴱ | ❌ | ✅ | ✅ |
| ZIP / TAR / 7z comparison | ✅ | ✅ | ❌ | ✅ | ◐ᵂ |
| Password-protected archives | ✅ | ? | ❌ | ? | ? |
| Edit and save inside archives | ✅ | ◐ | ❌ | ✅ | ❌ |
| Jupyter notebook cell-aware comparison | ✅ | ❌ | ❌ | ❌ | ❌ |

### 5. Structured and Semantic Comparison

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| JSON structural comparison | ✅ | ◐ | ❌ | ◐ | ❌ |
| XML structural comparison | ✅ | ◐ | ❌ | ◐ | ◐ |
| YAML structural comparison | ✅ | ◐ | ❌ | ❌ | ❌ |
| TOML structural comparison | ✅ | ❌ | ❌ | ❌ | ❌ |
| Aligned tree viewer | ✅ | ❌ | ❌ | ❌ | ❌ |
| Key-based array matching | ✅ | ❌ | ❌ | ❌ | ❌ |
| Structural move detection | ✅ | ❌ | ❌ | ❌ | ❌ |
| XPath-style search | ✅ | ❌ | ❌ | ❌ | ❌ |
| Semantic three-way conflict resolution | ✅ | ❌ | ❌ | ❌ | ❌ |
| Semantic three-way merge | ◐ | ❌ | ❌ | ❌ | ❌ |
| Source-linked tree navigation | ✅ | ❌ | ❌ | ❌ | ❌ |
| Structure-aware folder comparison | ✅ | ❌ | ❌ | ❌ | ❌ |

### 6. Reports, Review, and Automation

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| HTML report | ✅ | ✅ | ❌ | ✅ | ✅ |
| PDF report | ✅ | ❌ | ❌ | ◐ | ◐ |
| XML report | ✅ | ✅ | ❌ | ✅ | ✅ |
| CSV summary | ✅ | ✅ | ❌ | ◐ | ❌ |
| Patch generation | ✅ | ✅ | ✅ | ✅ | ✅ |
| Patch application | ✅ | ◐ᴱ | ❌ | ◐ | ◐ᵂ |
| Saved comparison projects | ✅ | ✅ | ◐ | ✅ | ✅ |
| Review comments | ✅ | ❌ | ❌ | ❌ | ✅ |
| Per-change review decisions | ✅ | ❌ | ❌ | ❌ | ◐ |
| Portable review workspace | ✅ | ❌ | ❌ | ❌ | ✅ |
| Merge verification | ✅ | ❌ | ❌ | ❌ | ❌ |
| Release comparison summary | ✅ | ❌ | ❌ | ❌ | ❌ |
| Potential change-impact analysis | ✅ | ❌ | ❌ | ❌ | ❌ |
| Batch / scripted comparison | ✅ | ✅ | ❌ | ✅ | ✅ |
| Saved automation jobs | ✅ | ❌ | ❌ | ✅ | ◐ |
| Named multi-tab workspaces | ✅ | ◐ | ❌ | ✅ | ❌ |
| HTML difference slideshow | ✅ | ❌ | ❌ | ❌ | ✅ |

### 7. Version Control Integration

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Git difftool | ✅ | ✅ | ✅ | ✅ | ✅ |
| Git mergetool | ✅ | ✅ | ✅ | ✅ | ✅ |
| Compare Git revisions | ✅ | ◐ | ◐ | ◐ | ✅ |
| Git folder comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Subversion integration | ✅ | ✅ | ✅ | ✅ | ✅ |
| Mercurial integration | ✅ | ❌ | ✅ | ✅ | ✅ |
| Perforce integration | ✅ | ❌ | ❌ | ✅ | ✅ |

## Interpretation notes

1. **Remote comparison works on a fetched copy:** OxideMerge connects over SFTP, FTPS or plain FTP (the dialog warns that plain FTP is unencrypted), fetches the file or folder, and compares the local copy rather than browsing the server live. Saving writes the changes back, checking each file on the server first, and *Reload from server* fetches the server's copy again. The connection dialog can list the server's folders to pick the path to fetch. These connections, including those through proxies, are tested against OpenSSH, vsftpd, ProFTPD, Dante and tinyproxy on Linux. On this computer, SFTP was also run by hand against OpenSSH 10.5p1 and anonymous FTP, including active mode, against vsftpd 3.0.5. Beyond Compare supports FTP in Standard and SFTP/FTPS in Pro; WinMerge only reads HTTP(S) URLs through a plugin.
2. **Archive editing scope:** OxideMerge writes back ZIP-based formats (entries stored or compressed with Deflate, Deflate64, bzip2, LZMA, xz, Zstandard or PPMd), TAR (plain or gzip, bzip2, xz or Zstandard compressed) and 7z, including password-protected ZIP (ZipCrypto, WinZip AES) and 7z archives, which stay encrypted with the same password, and archives nested in other archives, keeping most untouched entries as they were. A changed ZIP entry compressed with LZMA or Deflate64, which can only be read, is written with Deflate, and so is an unchanged LZMA entry, because other tools refuse a copied one; an unchanged ZipCrypto entry is encrypted again, because a copy would lose the mark that says it is encrypted. RAR and split archives are not supported. Signed packages (APK, IPA, MSIX, signed JARs and the like) stay read-only on purpose, since any change would break their signature; review snapshots and web page trees opened as folders are read-only too, as they are not archives. Araxis Merge's archive support is Windows-only.
3. **Office / PDF comparison:** OxideMerge extracts text, or shows documents as formatted Markdown block by block; original page-layout fidelity and write-back are not equivalent to native editing. WinMerge reads Word and Excel files through plugins that need Microsoft Office installed, PDF files through an Apache Tika plugin, OpenDocument files only as the archives they are, and JSON, XML and YAML only by pretty-printing them through plugins. Beyond Compare converts Word, PDF and RTF files to text, and reads JSON and XML through additional file formats that reformat them before a text comparison.
4. **Jupyter notebooks:** cell-aware extraction is compare-only and intentionally ignores execution outputs; it is not a semantic notebook merge editor. The extracted text can be saved as a plain file and edited there.
5. **Three-way features:** Beyond Compare requires Pro for three-way text and folder merging, and Araxis Merge requires Professional for any three-way comparison or merge. WinMerge and Meld include three-way file and folder comparison in the free product.
6. **Saved automation jobs:** OxideMerge job files run a fixed list of reports, patches and synchronizations. A synchronization step applies only with `"apply": true` in the job; applying it, or writing a report or patch outside the job file's folder, needs `--yes`, and a synchronization that would delete many items also needs `"allow_large_deletion": true`. There is no built-in scheduler: run them from cron, launchd or Task Scheduler. Beyond Compare's equivalent is its script files; Araxis Merge offers an automation API (Windows) and AppleScript (macOS).
7. **Version control support in OxideMerge** is Git, Subversion, Mercurial and Perforce. *Compare with revision* covers Git, Subversion and Mercurial, not Perforce. On Windows, Subversion is given a folder whose path is not ASCII as `.` from inside it, and such a file through its repository URL pinned at the working copy's revision, with `BASE` from the pristine copy (from the server when the file was committed with `svn:keywords`); Mercurial can only open a name the Windows ANSI code page can spell, which is Mercurial's own limit. Subversion and Mercurial tests also run on Linux in the Docker test environment. The same compare-with-revision and merge flows, plus Git's difftool and mergetool, were run on Linux with Git 2.55.0, Subversion 1.14.5, Mercurial 7.2.4 and p4/p4d 2026.1; a non-ASCII Subversion path is given to `svn` as itself there. Perforce's `P4DIFF` is a two-file compare and `P4MERGE` is a four-file merge; with no other option, an unchanged merge result exits 1 so the conflict stays unresolved. On Linux that exit is reported as *Execution Failed* and *No such file or directory*. P4V keeps its own argument string. On Linux, P4V 2026.1 (2933292) was checked with it: diff opens the depot revision on the left, saving then Accept Merged resolves the file, and closing without saving leaves P4V's markers and the file unresolved. That check was not repeated on Windows. A Subversion or Perforce server can still ask for a sign-in that OxideMerge cannot give; sign in with `svn` or `p4` first.
8. **Inline / unified differences:** OxideMerge has an inline view; WinMerge shows the current difference in a separate pane, Meld and Beyond Compare can save unified patches (Beyond Compare also has interleaved reports), and Araxis Merge has a line-over-line detail panel.
9. **Meld folder comparison:** do not conflate its three-way file and folder comparison with complete three-way folder merge and synchronization.
10. **OxideMerge portable use:** an `oxidemerge.json` next to the program keeps the options, the `Rules` folder and the trusted server keys and certificates with the program. Synchronization recovery journals, WinMerge `Filters` and added `Languages` still go to the user's configuration folder; set `OXIDEMERGE_CONFIG_DIR` to keep those elsewhere too.
11. **Semantic three-way:** OxideMerge's three-way Semantic view lists each JSON, XML, TOML or YAML path that changed on one side, both sides the same way, or in conflict, and *Use left* or *Use right* copies that side's value into the base pane, checking the result parses to the expected structure. There is no apply-all and moves are refused, so as an automatic merge it is ◐.
12. **Araxis Merge 2026.1** removed the Perforce, Subversion and FTP file-system plugins (and the credential store they used); its read-only Git and Mercurial plugins remain, and it still works as the external comparison and merge tool of Perforce and Subversion clients, which is what its ✅ for those rows reflects.

## Sources

OxideMerge:

- [OxideMerge README](https://github.com/kuopinghsu/OxideMerge/blob/main/README.md)
- [OxideMerge CHANGELOG](https://github.com/kuopinghsu/OxideMerge/blob/main/CHANGELOG.md)
- [OxideMerge User Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/user-guide.adoc)
- [OxideMerge Analysis Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/analysis-guide.md)

WinMerge:

- [About WinMerge (feature list)](https://winmerge.org/about/)
- [WinMerge 2.16 Manual](https://manual.winmerge.org/en/): [Comparing and merging folders](https://manual.winmerge.org/en/Compare_dirs.html), [Comparing in table format](https://manual.winmerge.org/en/Compare_table.html), [Comparing images](https://manual.winmerge.org/en/Compare_images.html), [Comparing web pages](https://manual.winmerge.org/en/Compare_webpages.html), [Plugins](https://manual.winmerge.org/en/Plugins.html), [Configuration (rename/move detection)](https://manual.winmerge.org/en/Configuration.html), [Using WinMerge with other tools (version control)](https://manual.winmerge.org/en/Version_control.html), [FAQ (printing, patches, line endings, OpenOffice files)](https://manual.winmerge.org/en/Faq.html), [Command line](https://manual.winmerge.org/en/Command_line.html)
- [WinMerge change log](https://github.com/WinMerge/winmerge/blob/master/Docs/Users/ChangeLog.md)

Meld:

- [Meld home page (features, platforms, version control, current release)](https://meldmerge.org/)
- [Meld Help: Synchronization Points](https://help.gnome.org/meld/syncpoints.html)
- [Meld Help: Things that Meld doesn't do](https://help.gnome.org/meld/missing-functionality.html)
- [Meld 3.23.0 release notes (image comparison)](https://download.gnome.org/sources/meld/3.23/meld-3.23.0.news)

Beyond Compare:

- [Standard vs. Pro](https://www.scootersoftware.com/kb/editions)
- [Feature list by version](https://www.scootersoftware.com/kb/feature_compare)
- [Beyond Compare 5 Help](https://www.scootersoftware.com/v5help/index.html): the current manual, covering Standard and Pro
- [Beyond Compare 4 User Guide (PDF)](https://www.scootersoftware.com/BC4Help.pdf): Folder Sync, Table Compare, scripting reference (`folder-report layout:xml`, `text-report layout:patch`); version 4, so recheck these against the version 5 help
- [Running Beyond Compare from removable media](https://www.scootersoftware.com/kb/removablemedia)
- [Picture Compare: Blend Mode](https://www.scootersoftware.com/v5help/pix_how_to_compare.html)
- [Beyond Compare 5 change log](https://www.scootersoftware.com/download/v5changelog): 5.2.7, build 32913

Araxis Merge:

- [Feature summary and differences between editions](https://www.araxis.com/merge/features.en)
- [Release notes](https://www.araxis.com/merge/release-notes.en): 2026.1, including the removal of the Perforce, Subversion and FTP plugins

**Publication caution:** cells marked ? were not checked against vendor documentation, and every competitor cell reflects that vendor's documentation on the review date above. Recheck product versions, editions, operating-system support and the ? cells against current official sources before publishing comparative marketing claims.
