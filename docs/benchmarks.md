# Benchmarks

The benchmarks time the compare engines on generated inputs. Most of them don't run the window,
so they measure the work behind it: loading, comparing and merging. Two use a view as well:
**xml-tree-view-12k** draws the XML Tree view off screen, and **folder-navigation-20k** steps
through the folder compare's view without drawing it. Run them on an idle machine:

```sh
cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The inputs are made once into `target/bench-data`: about 1 GB on disk, 1.3 GB with
`OXIDEMERGE_BENCH_LARGE=1` and about 14 GB with `=2`, of which 10 GiB are the five 2 GiB files
of sync-apply-large. That workload also needs about 10 GiB more free space while it runs, for
its working copy, the new and replacement files and the journal. `=3` adds about 2 GB for the
1 GB text pair. Each workload then runs three times, each in a process of its own; the median
time and the highest peak memory are reported and written to `target/bench/results.md`, with the machine,
the commit and the build. Peak memory is measured with `/usr/bin/time`, so it is missing on
Windows. The files are read from the system's file cache after the first run, so the times
don't include disk reads.

| Variable | Effect |
| --- | --- |
| `OXIDEMERGE_BENCH_LARGE=1` | Adds the 100 MB text and binary compares; `=2` also a 1 GB binary compare and the synchronization of 2 GB files; `=3` also two 1 GB text inputs |
| `OXIDEMERGE_BENCH_ONLY=text,folder` | Runs only the workloads whose names contain one of these |
| `OXIDEMERGE_BENCH_RUNS=5` | Number of runs per workload (default 3) |
| `OXIDEMERGE_BENCH_SAVE=base.txt` | Saves the times as a baseline |
| `OXIDEMERGE_BENCH_BASELINE=base.txt` | Fails if a workload that took at least 50 ms is now more than `OXIDEMERGE_BENCH_TOLERANCE` (default 1.5) times slower |

Baselines only mean something on the machine that made them. Save one before a change and compare
after it:

```sh
OXIDEMERGE_BENCH_SAVE=/tmp/base.txt cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
# ... change the code ...
OXIDEMERGE_BENCH_BASELINE=/tmp/base.txt cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
```

In CI, start the workflow by hand with *Run benchmarks* checked to run them on Linux, Windows
and macOS; the results are kept as artifacts. Hosted runners vary too much for fixed thresholds,
so CI reports the numbers without failing on them.

## Workloads

- **text-10mb**, **text-100mb**: two text files of source-like lines, with one line in 20 (or 200)
  changed, inserted or deleted.
- **text-repeated**: 300,000 equal lines with 300 inserted, which defeats line matching by
  content.
- **text-1gb**: two 1 GiB UTF-8 source-like files, with one line in 200 changed, inserted or
  deleted. Fixture generation writes incrementally, avoiding whole-file generation buffers.
- **table-10k**, **table-100k**: two CSV tables of 10,000 (or 100,000) records keyed by `id`, the
  right one shuffled, with 1% of the amounts changed, 0.5% of the records removed and as many
  added; parsed and matched by key.
- **table-10k-position**, **table-100k-position**: the same tables compared row by row, as a table
  is compared without a key.
- **table-cancel-1m**: two such tables of 1,000,000 records matched by key on another thread,
  stopped once half the records are read, as *Cancel* does. The result says how long stopping
  took, freeing what was built so far included.
- **xml-tree-12k**: two XML catalogs of 12,000 `<item>` elements (about 84,000 parser nodes, near
  the 100,000-node limit), 1% of the items changed and 0.5% inserted and removed, parsed and
  aligned as the XML Tree view does.
- **xml-tree-12k-keyed**: the same items with the right catalog shuffled, paired by the key rule
  `item@id`.
- **xml-tree-view-12k**: the same comparison put through the real Tree view, a frame at a time,
  off screen. Besides the comparison it reports the first rows drawn, a frame while scrolling
  (the viewport moved to a different stretch of the tree each frame, 100 of them), unfolding the
  whole tree and folding the unchanged rows again, and a step from one change to the next with
  the frame it is drawn in. These are the costs of using the view rather than of the engine
  behind it.
- **xml-semantic-12k**: the files of xml-tree-12k compared by the Semantic view's path-by-path
  engine, for comparison.
- **merge3-10mb**: three 10 MB files compared as a 3-way compare, then merged automatically.
- **binary-10mb**, **binary-100mb**, **binary-1gb**: two random files with 100 bytes changed.
- **image-4k**: two 3840 x 2160 PNG images with 1% of the pixels changed, decoded and compared.
- **folder-small-files**: 20,000 files of 1 KB up to 8 folders deep, 1% changed and 1% on one
  side only. "First result" is the time until the first item reaches the folder list.
- **folder-large-files**: 20 files of 8 MB, two changed in their last byte.
- **folder-mixed**: 2,000 files of 100 bytes to 2 MB, some 60 folders deep.
- **sync-preview-20k**: the folders of folder-small-files compared, then a mirror plan made from
  the result. The plan reads every file it would touch, and every file on both sides whose size or
  time differs, to fingerprint it; "plan" is the time of that step alone.
- **sync-apply-large** (`OXIDEMERGE_BENCH_LARGE=2`): three random 2 GiB files mirrored into a
  copy of a folder that has one of them equal, one changed in its last byte and one missing. The
  time covers the scan, the plan and the apply, which fingerprints, copies through a verified
  temporary file, keeps the replaced file in the journal and checks the result.
- **folder-navigation-20k**: the folders of folder-small-files compared, then stepped through
  from the first file with differences to the last, as *Next Difference* does without the file
  pane, reading the status bar position at each file.

## Baseline

Measured on macOS aarch64, Apple M2 Max, 12 threads, 32 GB memory; OxideMerge 0.9.4, release
build.

The table is shortened from the one the program prints: the Input column (see the workloads
above) and counts of zero are left out, and large numbers have thousands separators.

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| text-10mb | 0.183 s | 157 MB | 14,305 differences |
| text-repeated | 0.034 s | 52 MB | 300 differences |
| text-100mb | 2.101 s | 1,492 MB | 15,086 differences |
| merge3-10mb | 0.559 s | 224 MB | 14,107 differences, conflicts left |
| binary-10mb | 0.003 s | 28 MB | 100 ranges |
| binary-100mb | 0.031 s | 208 MB | 100 ranges |
| binary-1gb | 0.366 s | 2,056 MB | 100 ranges |
| image-4k | 0.108 s | 104 MB | 82,116 pixels differ |
| folder-small-files | 0.568 s | 31 MB | 220 differ, 198 unique; first result after 2 ms |
| folder-large-files | 0.014 s | 138 MB | 2 differ |
| folder-mixed | 0.056 s | 36 MB | 80 differ; first result after 2 ms |

Added later, on the same machine with OxideMerge 0.9.8:

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| sync-preview-20k | 2.005 s | 33 MB | 198 copies, 220 replaces; plan 1,427 ms |

Added with 0.9.8's table, navigation and synchronization changes (2026-11-02, same machine):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| table-10k | 0.012 s | 24 MB | 95 modified, 50 added, 55 removed, 9,749 reordered |
| table-10k-position | 0.016 s | 22 MB | 197 differences |
| table-100k | 0.133 s | 135 MB | 1,030 modified, 500 added, 532 removed, 98,850 reordered |
| table-100k-position | 0.231 s | 125 MB | 614 differences |
| table-cancel-1m | 0.479 s | 748 MB | stopped 22.6 ms after asked, at 1,008,192 of 1,999,985 records |
| folder-navigation-20k | 0.653 s | 40 MB | 418 files in 79 ms, 0.19 ms a step, first 4 ms; compare 572 ms |

The table rows are from five runs each; the navigation row from three. Added with the XML Tree view (2026-11-03, same machine, three runs each):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| xml-tree-12k | 0.153 s | 116 MB | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.172 s | 120 MB | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-semantic-12k | 0.131 s | 128 MB | 29,430 changes |

By position, the Semantic view reports 29,430 changes for the same 0.5% insertions; the Tree view
reports about 380. In the shuffled catalog nearly every item is out of order, so most are moves. Before the order of the
files stepped through was kept between steps, a step took 2.48 ms (1,035 ms for the 418 files).

Added with the free-space check (2026-11-03, same machine, three runs):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| sync-apply-large | 74.653 s | 15 MB | 2 changes applied; preview 28,494 ms, apply 45,620 ms |

Files are streamed, so memory stays flat however large they are; the time is reading and
writing the files several times over (scan, fingerprints, copy, journal and final check).

Added with the XML Tree view's drawing and keyboard measurements (2026-11-03), on
**windows x86_64**, Intel Core (24 threads), OxideMerge 0.9.8, release build, three runs each.
Peak memory is measured with `/usr/bin/time`, which Windows has not, so it is missing:

| Workload | Time (median) | Result |
| --- | ---: | --- |
| xml-tree-12k | 0.226 s | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.260 s | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-tree-view-12k | 0.785 s | 72,297 rows, 376 changes; compare 218 ms, first rows 5 ms, a scrolled frame 0.29 ms, unfold 2 ms, fold 1 ms, a step 1.40 ms over 376 |
| xml-semantic-12k | 0.152 s | 29,430 changes |

Drawing is not what the Tree view costs: of the 785 ms, 218 ms is the comparison and 5 ms the
first rows, and a frame while scrolling 72,297 rows takes 0.29 ms because only the rows in view
are built. Folding and unfolding the whole tree cost 1–2 ms, since neither re-runs the
comparison. A step to the next change costs 1.40 ms, which includes unfolding the ancestors of
the row, scrolling it into view and drawing the frame.

These Windows numbers are not comparable with the macOS numbers above: another machine, and the
times there were measured with the memory tool attached.

## Findings

- Text comparisons retain decoded lines and alignment data. Memory depends on line count,
  comparison rules and editing history. Large valid UTF-8 files load incrementally, comparison
  keys borrow unchanged source text, and alignment uses compact IDs; legacy encodings still use
  the complete-buffer decoder. See the dated measurements below rather than assuming a fixed
  memory multiplier for every file.
- Binary and image compares hold every file whole: 2 GB for two 1 GB files.
- A folder compare lists its first items within milliseconds and keeps comparing in the
  background, so large trees are usable at once. *Stop* ends it between files, and the status
  bar then says the comparison was aborted. Folder merges refuse to run on an aborted comparison.
- Folder compares by contents read files of up to 16 MB whole, a few at a time, and larger ones
  in 1 MB chunks. Memory follows the number of threads (138 MB for 8 MB files on 12 threads),
  not the size of the tree.

## Recorded comparison: 0.9.6 (2026-10-30)

[Detailed measurements](analysis-benchmarks.json) compare commit
`c65dfe791a54de6cc9afeba517df169b3d59aa29` with the 0.9.6 working tree on the same
macOS aarch64 host (Apple M2 Max, 12 threads, 32 GB). The reference source was extracted
with `git archive`; both builds reused the same generated fixtures. Each full-run workload
used three independent child processes, reporting median time and highest peak RSS from
native `/usr/bin/time -l`.

To reproduce this comparison, save a reference with `OXIDEMERGE_BENCH_SAVE`, then run the
changed source with the same file as `OXIDEMERGE_BENCH_BASELINE`. Set
`OXIDEMERGE_BENCH_LARGE=2` and `OXIDEMERGE_BENCH_RUNS=3` for both runs, using the command
above with `--locked --offline` when dependencies are already available locally.

The full run exceeded the default 1.5x timing threshold for `merge3-10mb`: 0.592 → 1.071 s,
with peak RSS 224 → 226 MB. A seven-run focused diagnostic
(`OXIDEMERGE_BENCH_ONLY=merge3-10mb OXIDEMERGE_BENCH_RUNS=7`) measured 0.623 s / 226 MB
and passed. Both results are retained; the diagnostic does not erase the failed full-run gate.
The discrepancy does not establish a stable regression or prove its absence on this shared host.
Other full-run workloads stayed below the timing threshold. Text 100 MB measured
2.089 → 2.887 s (1492 → 1494 MB), binary 1 GB 0.406 → 0.401 s (2056 → 2056 MB), and
20,000-file folders 0.563 → 0.590 s (32 → 34 MB).

These are comparison-engine measurements. They do not measure semantic/impact analysis,
GUI rendering or the extra SHA-256 reads used by folder merge confirmation. They are not
cross-platform release validation.

## Recorded comparison: 0.9.7 impact and merge changes (2026-11-01)

Commit `291d184` (extracted with `git archive`) and the 0.9.7 working tree with all-or-nothing
folder merges, semantic pairing confidence and Tree-sitter impact analysis were measured on the
macOS aarch64 host above, with the default workloads, three runs each and the same generated
inputs. The working tree passed the 1.5x gate against the saved commit baseline with identical
results; for example text 10 MB 0.112 → 0.111 s (99 → 100 MB), 3-way merge 0.340 → 0.345 s
(150 MB), 20,000-file folders 0.602 → 0.570 s (36 → 32 MB).

The large workloads (`OXIDEMERGE_BENCH_LARGE=2 OXIDEMERGE_BENCH_ONLY=text-100mb,binary-1gb`)
were then run with frozen commit and working-tree test executables, alternating them twice with
five runs each on the idle host. Text 100 MB measured 1.054 and 1.073 s at the commit against
1.076 and 1.067 s in the working tree (619–620 MB each), and binary 1 GB 0.340 and 0.341 s
against 0.347 and 0.336 s (2,057 MB each), with the same 15,086 differences and 100 ranges. The
differences are within the variation between runs.

The engine workloads do not run impact analysis, so `--impact` was also timed on the egui 0.31.1
and 0.37.0 sources (88 and 132 Rust files, 106 files differing under `src`), three runs each after
a warm-up run: the median time was 0.08 → 0.18 s and the peak memory 28 → 47 MB. Files are parsed
with Tree-sitter on up to eight threads; on one thread the working tree took 0.50 s. The commit
reported 17 heuristic findings; the working tree reports 601 exact import, function, type and
value findings and 517 heuristic ones, with 1,724 more name and configuration key matches counted
in a note. Five files are listed as partly assessed because the Rust grammar does not read
`dyn 'a + Fn(…)` trait objects and some attribute positions. The release executable grew from
27.6 to 37.5 MB with the grammars.

## Recorded comparison: 0.9.8 table, navigation and synchronization changes (2026-11-02)

Commit `00eda61` (extracted with `git archive`) and the working tree were built as release test
executables, then run one after the other on the macOS aarch64 host above with the default
workloads, three runs each, reading the same generated inputs. The working tree passed the 1.5x
gate against the commit's saved baseline with the same result counts:

| Workload | Commit | Working tree |
| --- | ---: | ---: |
| text-10mb | 0.108 s, 98 MB | 0.112 s, 100 MB |
| text-repeated | 0.026 s, 42 MB | 0.025 s, 41 MB |
| merge3-10mb | 0.309 s, 148 MB | 0.321 s, 149 MB |
| binary-10mb | 0.003 s, 29 MB | 0.003 s, 29 MB |
| image-4k | 0.104 s, 105 MB | 0.103 s, 105 MB |
| folder-small-files | 0.567 s, 33 MB | 0.599 s, 33 MB |
| folder-large-files | 0.013 s, 139 MB | 0.013 s, 139 MB |
| folder-mixed | 0.054 s, 35 MB | 0.054 s, 37 MB |

The differences are within the variation between runs seen in the earlier records. The commit
has no table, synchronization or navigation workloads, so those have no before figures.

Matching by key is faster than comparing row by row on these shuffled tables, as the line
alignment of the positional compare has many more changes to work through. Tables of more than
100,000 records (both sides together) are matched on another thread, which checks for a stop
every 4,096 records; stopping one half way through 1,000,000 records took 23 ms, freeing the keys
it had built included. Stepping to the next file with differences among 20,000
files takes about 2.5 ms, as each step finds the file's place in the listed order again.

## Large UTF-8 text comparison

On 2026-10-30, two 1 GiB UTF-8 inputs (`text-1gb`, deterministic generator seed 7,
about 30.6 million lines per side, one line in 200 changed/inserted/deleted) were compared
on the same Apple M2 Max host described above. Frozen release test executables captured the
0.9.6 working tree immediately before and after the large-text changes. Both runs reused
identical fixture files, used default comparison rules, and ran sequentially after compilation.
Each version ran three times: the median elapsed time was **29.992 → 17.225 seconds**;
the maximum native peak RSS was **10,002 → 5,705 MiB** (about 43% lower for each metric).
Both produced **152,307 difference blocks**. These timings include loading and comparison,
not window rendering or editing. The initial single-run capture was 44.392 s / 4,946 MiB;
that differing result illustrates why the matched three-run measurements are reported here.
Resident memory varies with host memory pressure and should not be treated as a fixed file-size
multiplier.

Large valid UTF-8 documents load through a bounded read buffer, preserving BOM, CR/LF/CRLF,
mixed terminators and final-newline state. Invalid UTF-8 and other encodings fall back to the
existing detector. Unchanged keys borrow line text; dictionaries leave scope before alignment.
Alignment rows use 16 bytes instead of 64, dense anchor positions and predecessors use compact
indices, and anchor reconstruction reuses its pair buffer. Pane display maps reference shared
rows rather than copying every display line. Syntax workers reject oversized input before making
a full document copy. Documents and editing history still consume memory proportional to content.

To run this workload without generating/running the other large cases:

```sh
OXIDEMERGE_BENCH_LARGE=3 OXIDEMERGE_BENCH_ONLY=text-1gb OXIDEMERGE_BENCH_RUNS=3 \
  cargo test --release --locked --bin oxidemerge core::bench::benchmarks \
  -- --ignored --nocapture --test-threads 1
```

Use `OXIDEMERGE_BENCH_SAVE` and `OXIDEMERGE_BENCH_BASELINE` as described above for local
before/after comparisons. Optional `OXIDEMERGE_BENCH_PROFILE=/path/to/local.log` records
loaded line lengths/capacities and process RSS at loading, interning and alignment stages;
profiling adds overhead and should be disabled for ordinary timing comparisons.

The 11 existing workloads also completed with unchanged result counts. Their full-run timing
gate reported `folder-mixed` at 0.086 s against an earlier 0.055 s reference, exceeding 1.5x.
A matched seven-run check using the frozen pre-change/current executables measured
0.054 → 0.053 s, both at 38 MiB, and passed. The full-run failure is retained in this record;
the focused check does not erase it. Other workloads stayed under the existing timing gate:
100 MiB text measured 1.221 s / 620 MiB and 10 MiB three-way merge 0.361 s / 150 MiB.

Validation includes formatting, all-target checking, Clippy with warnings denied, 278 unit/GUI
and 25 CLI tests, plus the four standard native GUI smoke cases on macOS. Windows/Linux native
release and memory measurements were not performed on this host.

The final native app also opened the same two 1 GiB files and completed its ten-frame smoke
run in 25.08 s, reporting `Some(Different)` and 152,307 differences without an error.
Its peak RSS was 7.32 GiB (the macOS peak memory footprint was 8.80 GiB); these GUI figures
include display maps and window state and are separate from the comparison-engine figures.
No pre-change GUI measurement was taken, so no GUI speed/memory ratio is claimed.

Frozen executable SHA-256 identifiers for the matched three-run engine comparison:

- Before: `fc8694e81e306ffda62b38a273367f08ddcd404f1a058028f5decb44a4759f02`.
- After: `6027340929251b22d0b19b325efaf582a6a7cd37855c25078ecfcee5a1403b56`.
