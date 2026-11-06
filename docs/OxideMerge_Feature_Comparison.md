# Feature Comparison: OxideMerge vs. WinMerge vs. Meld vs. Beyond Compare vs. Araxis Merge

**Review date:** November 7, 2026  
**OxideMerge reference:** `main` at `024445c4a534a320dac29d890c4b3c951dbfa8f7`  
**Version:** 0.9.8 (`Unreleased` in CHANGELOG; not a claim that a v0.9.8 binary is published).

**Legend:** ✅ Documented support · ◐ Partial / platform- or edition-dependent / not fully verified · ❌ Not supported or not documented

> This is a documentation-based comparison, not hands-on feature or performance validation. ✅ does not imply equal depth, implementation quality, or usability. For commercial tools, ◐ frequently denotes an edition restriction. ❌ may mean documentation did not establish a capability; it is not proof that the capability is impossible.

## What changed from the November 5 table

- OxideMerge: **SFTP file/folder comparisons** are documented. Fetched remote **files** are editable and uploaded with remote-change checks; remote **folders** remain read-only. No general FTP support is documented.
- OxideMerge: **archive write-back** is available for selected unsigned ZIP/TAR/7z formats, with important exclusions such as nested archives, signed packages and TAR.XZ.
- OxideMerge: **Jupyter notebook** cell-by-cell text extraction (ignoring outputs and execution metadata), **clipboard comparison**, **named workspaces**, and **binary moved-block detection** are documented.
- Corrected overly broad edition-independent checks for **Beyond Compare three-way merging** (Pro) and **Araxis three-way comparison/merging** (Professional).
- Corrected the original `Perforce integration` OxideMerge check: Git, SVN and Mercurial are explicitly documented; Perforce remains ◐ pending verification of any dedicated integration.

## Comparison tables

### 1. Platform and General Features

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Windows | ✅ | ✅ | ✅ | ✅ | ✅ |
| macOS | ✅ | ❌ | ◐ | ✅ | ✅ |
| Linux | ✅ | ❌ | ✅ | ✅ | ❌ |
| Open source | ✅ | ✅ | ✅ | ❌ | ❌ |
| Free to use | ✅ | ✅ | ✅ | ❌ | ❌ |
| Portable execution | ✅ | ✅ | ◐ | ✅ | ◐ |
| Dark / Light theme | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multiple languages | ✅ | ✅ | ✅ | ✅ | ◐ |
| Command-line interface | ✅ | ✅ | ✅ | ✅ | ✅ |
| File manager integration | ✅ | ✅ | ✅ | ✅ | ✅ |

### 2. File and Text Comparison

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Two-way file comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Three-way file comparison | ✅ | ✅ | ✅ | ✅ | ◐ |
| Three-way merge | ✅ | ◐ | ✅ | ◐ | ◐ |
| Side-by-side comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Inline / unified differences | ✅ | ✅ | ✅ | ✅ | ✅ |
| Word-level differences | ✅ | ✅ | ✅ | ✅ | ✅ |
| Moved block detection | ✅ | ✅ | ◐ | ✅ | ✅ |
| Syntax highlighting | ✅ | ✅ | ✅ | ✅ | ✅ |
| Inline editing | ✅ | ✅ | ✅ | ✅ | ✅ |
| Undo / Redo | ✅ | ✅ | ✅ | ✅ | ✅ |
| Search and replace | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ignore whitespace | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ignore case | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ignore comments | ✅ | ✅ | ◐ | ✅ | ✅ |
| Regex-based ignore rules | ✅ | ✅ | ✅ | ✅ | ✅ |
| Manual alignment points | ✅ | ✅ | ◐ | ✅ | ✅ |
| Synchronized scrolling | ✅ | ✅ | ✅ | ✅ | ✅ |
| Aligned row layout | ✅ | ✅ | ✅ | ✅ | ✅ |
| Bookmarks | ✅ | ✅ | ❌ | ✅ | ✅ |
| Conflict marker resolution | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multi-cursor text editing | ✅ | ◐ | ◐ | ◐ | ◐ |
| Clipboard text comparison | ✅ | ✅ | ◐ | ✅ | ✅ |

### 3. Folder Comparison and Synchronization

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Two-way folder comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Three-way folder comparison | ✅ | ◐ | ◐ | ◐ | ◐ |
| Recursive comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder filters | ✅ | ✅ | ✅ | ✅ | ✅ |
| Content-based comparison | ✅ | ✅ | ✅ | ✅ | ✅ |
| Copy between folders | ✅ | ✅ | ✅ | ✅ | ✅ |
| Folder synchronization | ✅ | ✅ | ◐ | ✅ | ✅ |
| Mirror synchronization | ✅ | ✅ | ❌ | ✅ | ◐ |
| Update-only synchronization | ✅ | ✅ | ◐ | ✅ | ◐ |
| Synchronization preview | ✅ | ✅ | ◐ | ✅ | ✅ |
| Rename / move detection | ✅ | ◐ | ◐ | ✅ | ◐ |
| Stale-file revalidation | ✅ | ◐ | ❌ | ◐ | ◐ |
| Recovery journal | ✅ | ❌ | ❌ | ◐ | ◐ |
| Rollback on synchronization failure | ✅ | ◐ | ❌ | ◐ | ◐ |
| Three-way folder merge | ✅ | ◐ | ◐ | ◐ | ◐ |
| Remote folder comparison | ◐ | ◐ | ❌ | ✅ | ◐ |
| FTP / SFTP | ◐ | ◐ | ❌ | ✅ | ❌ |
| Cloud storage integration | ❌ | ❌ | ❌ | ◐ | ❌ |
| Remote file editing and upload (SFTP) | ✅ | ◐ | ❌ | ✅ | ◐ |

### 4. Specialized File Formats

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Binary comparison | ✅ | ✅ | ❌ | ✅ | ✅ |
| Hex viewer | ✅ | ✅ | ❌ | ✅ | ✅ |
| Binary editing | ✅ | ◐ | ❌ | ◐ | ◐ |
| Binary moved-block detection | ✅ | ◐ | ❌ | ◐ | ◐ |
| Image comparison | ✅ | ✅ | ❌ | ✅ | ✅ |
| Image overlay | ✅ | ✅ | ❌ | ✅ | ✅ |
| Image difference view | ✅ | ✅ | ❌ | ✅ | ✅ |
| CSV / TSV comparison | ✅ | ✅ | ◐ | ✅ | ◐ |
| Table cell editing | ✅ | ◐ | ❌ | ✅ | ◐ |
| Key-based row matching | ✅ | ◐ | ❌ | ✅ | ❌ |
| Numeric tolerance | ✅ | ◐ | ❌ | ✅ | ◐ |
| PDF text comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Word DOCX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Excel XLSX comparison | ✅ | ◐ | ❌ | ✅ | ✅ |
| Office layout fidelity | ◐ | ◐ | ❌ | ◐ | ◐ |
| ELF binary structure comparison | ✅ | ◐ | ❌ | ◐ | ❌ |
| HTML / web page comparison | ✅ | ✅ | ◐ | ✅ | ✅ |
| ZIP / TAR / 7z comparison | ✅ | ✅ | ❌ | ✅ | ◐ |
| Edit and save inside archives | ◐ | ◐ | ❌ | ✅ | ◐ |
| Jupyter notebook cell-aware comparison | ✅ | ❌ | ❌ | ◐ | ◐ |

### 5. Structured and Semantic Comparison

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| JSON structural comparison | ✅ | ◐ | ❌ | ◐ | ◐ |
| XML structural comparison | ✅ | ◐ | ❌ | ◐ | ◐ |
| YAML structural comparison | ✅ | ◐ | ❌ | ◐ | ◐ |
| TOML structural comparison | ✅ | ◐ | ❌ | ◐ | ◐ |
| Aligned tree viewer | ✅ | ❌ | ❌ | ◐ | ◐ |
| Key-based array matching | ✅ | ❌ | ❌ | ◐ | ❌ |
| Structural move detection | ✅ | ❌ | ❌ | ◐ | ◐ |
| XPath-style search | ✅ | ❌ | ❌ | ❌ | ◐ |
| Semantic three-way merge | ✅ | ❌ | ❌ | ◐ | ◐ |
| Source-linked tree navigation | ✅ | ❌ | ❌ | ◐ | ◐ |
| Structure-aware folder comparison | ✅ | ❌ | ❌ | ◐ | ❌ |

### 6. Reports, Review, and Automation

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| HTML report | ✅ | ✅ | ◐ | ✅ | ✅ |
| PDF report | ✅ | ◐ | ❌ | ◐ | ◐ |
| XML report | ✅ | ◐ | ❌ | ✅ | ✅ |
| CSV summary | ✅ | ✅ | ❌ | ✅ | ◐ |
| Patch generation | ✅ | ✅ | ✅ | ✅ | ✅ |
| Patch application | ✅ | ✅ | ✅ | ✅ | ◐ |
| Saved comparison projects | ✅ | ✅ | ◐ | ✅ | ✅ |
| Review comments | ✅ | ◐ | ❌ | ◐ | ✅ |
| Per-change review decisions | ✅ | ❌ | ❌ | ◐ | ◐ |
| Portable review workspace | ✅ | ◐ | ❌ | ✅ | ✅ |
| Merge verification | ✅ | ◐ | ◐ | ◐ | ◐ |
| Release comparison summary | ✅ | ❌ | ❌ | ◐ | ◐ |
| Potential change-impact analysis | ✅ | ❌ | ❌ | ❌ | ❌ |
| Batch / scripted comparison | ✅ | ✅ | ◐ | ✅ | ✅ |
| Saved automation jobs | ◐ | ◐ | ❌ | ✅ | ◐ |
| Named multi-tab workspaces | ✅ | ◐ | ◐ | ✅ | ✅ |
| HTML difference slideshow | ✅ | ◐ | ❌ | ✅ | ✅ |

### 7. Version Control Integration

| Feature | OxideMerge | WinMerge | Meld | Beyond Compare | Araxis Merge |
|---|:---:|:---:|:---:|:---:|:---:|
| Git difftool | ✅ | ✅ | ✅ | ✅ | ✅ |
| Git mergetool | ✅ | ✅ | ✅ | ✅ | ✅ |
| Compare Git revisions | ✅ | ✅ | ✅ | ✅ | ✅ |
| Git folder comparison | ✅ | ✅ | ◐ | ✅ | ✅ |
| Subversion integration | ✅ | ✅ | ✅ | ✅ | ✅ |
| Mercurial integration | ✅ | ✅ | ✅ | ✅ | ✅ |
| Perforce integration | ◐ | ✅ | ✅ | ✅ | ✅ |

## Interpretation notes

1. **SFTP is not FTP:** OxideMerge explicitly documents SFTP with remote-folder reads and guarded file uploads, not generic FTP or remote-folder synchronization.
2. **Archive editing is not universal:** write-back is limited by archive format, signatures, nesting and read-only sources.
3. **Office / PDF comparison:** OxideMerge extracts text or presents formatted Markdown; original page-layout fidelity and write-back are not equivalent to native editing.
4. **Jupyter notebooks:** cell-aware extraction is read-only and intentionally ignores execution outputs; it is not a semantic notebook merge editor.
5. **Three-way features:** Beyond Compare requires Pro for text/folder merge and Araxis requires Professional for three-way comparisons. Some other vendor checks are retained as provisional and merit vendor-by-vendor review before marketing use.
6. **Saved automation jobs:** the OxideMerge documentation establishes CLI scripting and saved workspaces, not a dedicated scheduling/job runner; therefore this is marked ◐.
7. **Meld folder comparison:** do not conflate its three-way file comparison with complete three-way folder merge and synchronization.

## Sources

- [OxideMerge README](https://github.com/kuopinghsu/OxideMerge/blob/main/README.md)
- [OxideMerge CHANGELOG](https://github.com/kuopinghsu/OxideMerge/blob/main/CHANGELOG.md)
- [OxideMerge User Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/user-guide.adoc)
- [OxideMerge Analysis Guide](https://github.com/kuopinghsu/OxideMerge/blob/main/docs/analysis-guide.md)
- [WinMerge](https://winmerge.org/)
- [Meld](https://meldmerge.org/)
- [Beyond Compare editions](https://www.scootersoftware.com/kb/editions)
- [Araxis Merge feature editions](https://www.araxis.com/merge/features.en)

**Publication caution:** Recheck product version, licensing edition, operating-system support and undocumented competitor cells against current official sources before publishing comparative marketing claims.
