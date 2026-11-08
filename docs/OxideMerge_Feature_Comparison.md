# Feature Comparison: OxideMerge vs. WinMerge vs. Meld vs. Beyond Compare vs. Araxis Merge

**Review date:** November 8, 2026\
**OxideMerge reference:** `main` at `d11c369`, plus the not-yet-committed changes listed in the November 8 revision notes\
**Version:** 0.9.8 (`Unreleased` in CHANGELOG; not a claim that a v0.9.8 binary is published).\
**Other products, as documented on that date:** WinMerge 2.16.60, Meld 3.24.1, Beyond Compare 5 (Standard and Pro), Araxis Merge (Standard and Professional).

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

**November 8, 2026**

- Every competitor cell was rechecked against the vendor sources listed under [Sources](#sources), and the legend now separates "not documented" (❌) from "not checked" (?). Notable corrections:
  - WinMerge has no FTP/SFTP and no mirror or update-only synchronization (copy commands only); it does have XML folder reports, three-way file and folder comparison, three-way merge, editable table and binary views, synchronization points, and numeric differences only as *Ignore numbers*.
  - Meld does not support Perforce (Git, Mercurial, Bazaar and Subversion only), detects no moved blocks, writes no HTML reports, applies no patches and has no portable install; it does have synchronization points and three-way folder comparison.
  - Beyond Compare has no slideshow report and no moved-block detection; three-way text merge, three-way folder merge, SFTP and cloud storage are Pro-only; it does have editable table and hex views, XML reports, portable installs and Update/Mirror sync with a preview.
  - Araxis Merge documents HTML slideshow reports, numeric tolerance, read-only Git and Mercurial file-system plugins and Perforce integration in the Standard edition; it documents no FTP/SFTP, table comparison or multi-cursor editing.
- OxideMerge: **Subversion, Mercurial and Perforce** are all documented as experimental (◐ᴱ), including Perforce setup through `P4DIFF`, `P4MERGE` and P4V; *Compare with revision* covers Subversion and Mercurial only.
- OxideMerge: **saved automation jobs** are documented: job files (`--job`) run reports, patches and synchronizations in order and print a JSON summary.
- OxideMerge: **archive write-back** now covers `.tar.xz` and archives nested in other archives, and keeps a 7z's untouched compressed blocks as they were; a `.tar.xz` or 7z, slow to compress, is written in the background. Every archive format OxideMerge opens can now be edited and saved, so Edit and save inside archives moves from ◐ to ✅. Signed packages stay read-only on purpose, since any change would break their signature.
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
| Portable execution | ✅ | ✅ | ❌ | ✅ | ? |
| Dark / Light theme | ✅ | ✅ | ✅ | ✅ | ? |
| Multiple languages | ✅ | ✅ | ✅ | ? | ◐ |
| Command-line interface | ✅ | ✅ | ✅ | ✅ | ✅ |
| File manager integration | ✅ | ✅ | ? | ✅ | ? |

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
| Ignore case | ✅ | ✅ | ? | ✅ | ? |
| Ignore comments | ✅ | ✅ | ◐ | ✅ | ✅ |
| Regex-based ignore rules | ✅ | ✅ | ✅ | ✅ | ✅ |
| Manual alignment points | ✅ | ✅ | ✅ | ✅ | ✅ |
| Synchronized scrolling | ✅ | ✅ | ✅ | ✅ | ✅ |
| Aligned row layout | ✅ | ✅ | ✅ | ✅ | ✅ |
| Bookmarks | ✅ | ✅ | ❌ | ✅ | ✅ |
| Conflict marker resolution | ✅ | ✅ | ◐ | ? | ? |
| Multi-cursor text editing | ✅ | ? | ? | ❌ | ❌ |
| Clipboard text comparison | ✅ | ✅ | ? | ✅ | ✅ |

### 3. Folder Comparison and Synchronization

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Two-way folder comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Three-way folder comparison | ✅ | ✅ | ✅ | ◐ᴾ | ◐ᴾ |
| Recursive comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder filters | ✅ | ✅ | ✅ | ✅ | ✅ |
| Content-based comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Copy between folders | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder synchronization | ✅ | ◐ | ◐ | ✅ | ✅ |
| Mirror synchronization | ✅ | ❌ | ❌ | ✅ | ? |
| Update-only synchronization | ✅ | ❌ | ❌ | ✅ | ? |
| Synchronization preview | ✅ | ❌ | ❌ | ✅ | ? |
| Rename / move detection | ✅ | ❌ | ◐ | ◐ᴾ | ? |
| Stale-file revalidation | ✅ | ❌ | ❌ | ? | ? |
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
| Binary editing | ✅ | ✅ | ❌ | ✅ | ? |
| Binary moved-block detection | ✅ | ❌ | ❌ | ❌ | ❌ |
| Image comparison | ✅ | ✅ | ❌ | ✅ | ✅ |
| Image overlay | ✅ | ✅ | ❌ | ✅ | ? |
| Image difference view | ✅ | ✅ | ❌ | ✅ | ✅ |
| CSV / TSV comparison | ✅ | ✅ | ◐ | ✅ | ◐ |
| Table cell editing | ✅ | ✅ | ❌ | ✅ | ❌ |
| Key-based row matching | ✅ | ❌ | ❌ | ✅ | ◐ |
| Numeric tolerance | ✅ | ◐ | ❌ | ✅ | ✅ |
| PDF text comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Word DOCX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Excel XLSX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Office layout fidelity | ◐ | ◐ | ❌ | ◐ | ◐ |
| ELF binary structure comparison | ✅ | ❌ | ❌ | ❌ | ❌ |
| HTML / web page comparison | ✅ | ◐ᴱ | ◐ | ✅ | ✅ |
| ZIP / TAR / 7z comparison | ✅ | ✅ | ❌ | ✅ | ◐ᵂ |
| Edit and save inside archives | ✅ | ? | ❌ | ✅ | ? |
| Jupyter notebook cell-aware comparison | ✅ | ❌ | ❌ | ❌ | ❌ |

### 5. Structured and Semantic Comparison

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| JSON structural comparison | ✅ | ◐ | ❌ | ? | ❌ |
| XML structural comparison | ✅ | ◐ | ❌ | ? | ◐ |
| YAML structural comparison | ✅ | ❌ | ❌ | ❌ | ❌ |
| TOML structural comparison | ✅ | ❌ | ❌ | ❌ | ❌ |
| Aligned tree viewer | ✅ | ❌ | ❌ | ❌ | ❌ |
| Key-based array matching | ✅ | ❌ | ❌ | ❌ | ❌ |
| Structural move detection | ✅ | ❌ | ❌ | ❌ | ❌ |
| XPath-style search | ✅ | ❌ | ❌ | ❌ | ❌ |
| Semantic three-way merge | ✅ | ❌ | ❌ | ❌ | ❌ |
| Source-linked tree navigation | ✅ | ❌ | ❌ | ❌ | ❌ |
| Structure-aware folder comparison | ✅ | ❌ | ❌ | ❌ | ❌ |

### 6. Reports, Review, and Automation

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| HTML report | ✅ | ✅ | ❌ | ✅ | ✅ |
| PDF report | ✅ | ◐ | ❌ | ◐ | ◐ |
| XML report | ✅ | ✅ | ❌ | ✅ | ✅ |
| CSV summary | ✅ | ✅ | ❌ | ◐ | ❌ |
| Patch generation | ✅ | ✅ | ✅ | ✅ | ✅ |
| Patch application | ✅ | ? | ❌ | ◐ | ◐ᵂ |
| Saved comparison projects | ✅ | ✅ | ❌ | ✅ | ✅ |
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
| Subversion integration | ◐ᴱ | ✅ | ✅ | ✅ | ✅ |
| Mercurial integration | ◐ᴱ | ✅ | ✅ | ✅ | ✅ |
| Perforce integration | ◐ᴱ | ? | ❌ | ✅ | ✅ |

## Interpretation notes

1. **Remote comparison works on a fetched copy:** OxideMerge connects over SFTP, FTPS or plain FTP (the dialog warns that plain FTP is unencrypted), fetches the file or folder, and compares the local copy rather than browsing the server live. Saving writes the changes back, checking each file on the server first, and *Reload from server* fetches the server's copy again. The connection dialog can list the server's folders to pick the path to fetch. These connections, including those through proxies, are tested against OpenSSH, vsftpd, ProFTPD, Dante and tinyproxy on Linux. Beyond Compare supports FTP in Standard and SFTP/FTPS in Pro; WinMerge only reads HTTP(S) URLs through a plugin.
2. **Archive editing covers every archive format OxideMerge opens:** ZIP-based formats, TAR (plain or gzip, bzip2 or xz compressed) and 7z, including archives nested in other archives. Signed packages (APK, IPA, MSIX, signed JARs and the like) stay read-only on purpose, since any change would break their signature; review snapshots and web page trees opened as folders are read-only too, as they are not archives. RAR is not read at all. Araxis Merge's archive support is Windows-only.
3. **Office / PDF comparison:** OxideMerge extracts text, or shows documents as formatted Markdown block by block; original page-layout fidelity and write-back are not equivalent to native editing. WinMerge reads Word and Excel files through plugins that need Microsoft Office installed, PDF files through an Apache Tika plugin, and JSON and XML only by pretty-printing them through plugins.
4. **Jupyter notebooks:** cell-aware extraction is compare-only and intentionally ignores execution outputs; it is not a semantic notebook merge editor. The extracted text can be saved as a plain file and edited there.
5. **Three-way features:** Beyond Compare requires Pro for three-way text and folder merging, and Araxis Merge requires Professional for any three-way comparison or merge. WinMerge and Meld include three-way file and folder comparison in the free product.
6. **Saved automation jobs:** OxideMerge job files run a fixed list of reports, patches and synchronizations, and refuse destructive synchronizations without `--yes`. There is no built-in scheduler: run them from cron, launchd or Task Scheduler. Beyond Compare's equivalent is its script files; Araxis Merge offers an automation API (Windows) and AppleScript (macOS).
7. **Version control support in OxideMerge** is Git first: Subversion, Mercurial and Perforce setups are documented as experimental and were tried by hand on macOS only; their automated tests also run on Linux in the Docker test environment. *Compare with revision* covers Git, Subversion and Mercurial, not Perforce.
8. **Inline / unified differences:** OxideMerge has an inline view; WinMerge shows the current difference in a separate pane, Meld and Beyond Compare can save unified patches (Beyond Compare also has interleaved reports), and Araxis Merge has a line-over-line detail panel.
9. **Meld folder comparison:** do not conflate its three-way file and folder comparison with complete three-way folder merge and synchronization.

## Sources

OxideMerge:

- [OxideMerge README](https://github.com/kuopinghsu/OxideMerge/blob/main/README.md)
- [OxideMerge CHANGELOG](https://github.com/kuopinghsu/OxideMerge/blob/main/CHANGELOG.md)
- [OxideMerge User Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/user-guide.adoc)
- [OxideMerge Analysis Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/analysis-guide.md)

WinMerge:

- [About WinMerge (feature list)](https://winmerge.org/about/)
- [WinMerge 2.16 Manual](https://manual.winmerge.org/en/): [Comparing and merging folders](https://manual.winmerge.org/en/Compare_dirs.html), [Comparing in table format](https://manual.winmerge.org/en/Compare_table.html), [Comparing web pages](https://manual.winmerge.org/en/Compare_webpages.html), [Plugins](https://manual.winmerge.org/en/Plugins.html), [Command line](https://manual.winmerge.org/en/Command_line.html)
- [WinMerge change log](https://github.com/WinMerge/winmerge/blob/master/Docs/Users/ChangeLog.md)

Meld:

- [Meld home page (features, platforms, version control)](https://meldmerge.org/)
- [Meld Help: Synchronization Points](https://help.gnome.org/meld/syncpoints.html)

Beyond Compare:

- [Standard vs. Pro](https://www.scootersoftware.com/kb/editions)
- [Feature list by version](https://www.scootersoftware.com/kb/feature_compare)
- [Beyond Compare 4 User Guide (PDF)](https://www.scootersoftware.com/BC4Help.pdf): Folder Sync, Table Compare, scripting reference (`folder-report layout:xml`, `text-report layout:patch`)
- [Running Beyond Compare from removable media](https://www.scootersoftware.com/kb/removablemedia)
- [Picture Compare: Blend Mode](https://www.scootersoftware.com/v5help/pix_how_to_compare.html)

Araxis Merge:

- [Feature summary and differences between editions](https://www.araxis.com/merge/features.en)

**Publication caution:** cells marked ? were not checked against vendor documentation, and every competitor cell reflects that vendor's documentation on the review date above. Recheck product versions, editions, operating-system support and the ? cells against current official sources before publishing comparative marketing claims.
