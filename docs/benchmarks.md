# Benchmarks

The benchmarks time the compare engines on generated inputs. Most of them don't run the
window, so they measure the work behind it: loading, comparing and merging. Two use a view
as well: **xml-tree-view-12k** draws the XML Tree view off screen, and
**folder-navigation-20k** steps through the folder compare's view without drawing it. Run
them on an idle machine:

```sh
cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The inputs are made once into `target/bench-data`: about 1 GB on disk, 1.3 GB with
`OXIDEMERGE_BENCH_LARGE=1` and about 14 GB with `=2`, of which 10 GiB are the five 2 GiB
files of sync-apply-large. That workload also needs about 10 GiB more free space while it
runs, for its working copy, the new and replacement files and the journal. `=3` adds about
2 GB for the 1 GB text pair and 1 GB for the 500 MB one (each `text-view-` and `text-cancel-`
workload has a copy of its own). Each workload then runs three times, each in a process of its
own; the median time and the highest peak memory are reported and written to
`target/bench/results.md`, with the machine, the commit and the build. Peak memory is
measured with `/usr/bin/time`, so it is missing on Windows, as is the resident memory of
each stage of the text-view workloads (read with `ps`). The files are read from the
system's file cache after the first run, so the times don't include disk reads.

| Variable | Effect |
| --- | --- |
| `OXIDEMERGE_BENCH_LARGE=1` | Adds the 100 MB text and binary compares; `=2` also a 1 GB binary compare and the synchronization of 2 GB files; `=3` also the 500 MB and 1 GB text inputs |
| `OXIDEMERGE_BENCH_ONLY=text,folder` | Runs only the workloads whose names contain one of these |
| `OXIDEMERGE_BENCH_RUNS=5` | Number of runs per workload (default 3) |
| `OXIDEMERGE_BENCH_SAVE=base.txt` | Saves the times as a baseline |
| `OXIDEMERGE_BENCH_BASELINE=base.txt` | Fails if a workload that took at least 50 ms is now more than `OXIDEMERGE_BENCH_TOLERANCE` (default 1.5) times slower |

Baselines only mean something on the machine that made them. Save one before a change and
compare after it:

```sh
OXIDEMERGE_BENCH_SAVE=/tmp/base.txt cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
# ... change the code ...
OXIDEMERGE_BENCH_BASELINE=/tmp/base.txt cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
```

In CI, start the workflow by hand with *Run benchmarks* checked to run them on Linux,
Windows and macOS; the results are kept as artifacts. Hosted runners vary too much for
fixed thresholds, so CI reports the numbers without failing on them.

## Workloads

- **text-10mb**, **text-100mb**: two text files of source-like lines, with one line in 20
  (or 200) changed, inserted or deleted.
- **text-repeated**: 300,000 equal lines with 300 inserted, which defeats line matching by
  content.
- **text-500mb**, **text-1gb** (`OXIDEMERGE_BENCH_LARGE=3`): two 500 MiB (or 1 GiB) UTF-8
  source-like files, with one line in 200 changed, inserted or deleted. Fixture generation writes incrementally, avoiding whole-file generation
  buffers.
- **table-10k**, **table-100k**: two CSV tables of 10,000 (or 100,000) records keyed by
  `id`, the right one shuffled, with 1% of the amounts changed, 0.5% of the records
  removed and as many added; parsed and matched by key.
- **table-10k-position**, **table-100k-position**: the same tables compared row by row, as
  a table is compared without a key.
- **table-cancel-1m**: two such tables of 1,000,000 records matched by key on another
  thread, stopped once half the records are read, as *Cancel* does. The result says how
  long stopping took, freeing what was built so far included.
- **xml-tree-12k**: two XML catalogs of 12,000 `<item>` elements (about 84,000 parser
  nodes, near the 100,000-node limit), 1% of the items changed and 0.5% inserted and
  removed, parsed and aligned as the XML Tree view does.
- **xml-tree-12k-keyed**: the same items with the right catalog shuffled, paired by the
  key rule `item@id`.
- **xml-tree-view-12k**: the same comparison put through the real Tree view, a frame at a
  time, off screen. Besides the comparison it reports the first rows drawn, a frame while
  scrolling (the viewport moved to a different stretch of the tree each frame, 100 of
  them), unfolding the whole tree and folding the unchanged rows again, and a step from
  one change to the next with the frame it is drawn in. These are the costs of using the
  view rather than of the engine behind it.
- **xml-semantic-12k**: the files of xml-tree-12k compared by the Semantic view's
  path-by-path engine, for comparison.
- **text-view-10mb**, **text-view-100mb** (`OXIDEMERGE_BENCH_LARGE=1`), **text-view-500mb**,
  **text-view-1gb** (`=3`): the files of text-10mb, text-100mb, text-500mb and text-1gb opened
  in the real text view, off screen. Besides the total it reports, each with the process's
  resident memory at that point: loading both files, the comparison and everything the view
  builds from it, the first frame drawn, a frame after jumping to a different line (100 of
  them, spread over the file), and a step to the next difference with its frame (100 steps).
  Last, *held* breaks down what the view keeps, by kind: the lines, the line ends, edit marks,
  highlighting, rows, layout, word-level differences and undo history (counted from their
  sizes and capacities, not measured by the allocator). The workload's peak memory is what a
  window showing the comparison needs, without the window itself.
- **text-cancel-100mb** (`=1`), **text-cancel-500mb** (`=3`): the same files opened as the
  window opens large ones, reading and comparing on another thread, and stopped as *Cancel*
  does at nine points from 10% to 90% of the time an open takes. It reports reading the files
  twice (the second time from the file cache for certain), the whole open, and for the nine
  cancels, the median and worst time until the work noticed and stopped, and until the thread
  had also freed what it had built. The window waits for neither: it lets go of the job at
  once and the thread frees its memory on its own. This workload's peak memory says nothing
  about one open (see the findings below).
- **merge3-10mb**: three 10 MB files compared as a 3-way compare, then merged
  automatically.
- **binary-10mb**, **binary-100mb**, **binary-1gb**: two random files with 100 bytes
  changed.
- **binary-moved-unrelated**: three unrelated 5 MiB panes, with no expected matches.
- **binary-moved-swap**: two 4 MiB files with two 64 KiB blocks swapped.
- **binary-moved-large**: two 64 MiB files with two 1 MiB blocks swapped.
- **binary-moved-short-shift**: two 4 MiB files with a 15-byte shifted run between edits.
- **binary-moved-wide-shift**: a 96-byte insertion shifts a 512 KiB repeated tail.
- **binary-moved-firmware**: the mixed-edit firmware samples, with 18 expected matches.
- **image-4k**: two 3840 x 2160 PNG images with 1% of the pixels changed, decoded and
  compared.
- **folder-small-files**: 20,000 files of 12 short lines (about 400 bytes) up to 8 folders
  deep, 1% changed and 1% on one side only. "First result" is the time until the first item reaches the folder list.
- **folder-large-files**: 20 files of 8 MB, two changed in their last byte.
- **folder-mixed**: 2,000 files of 100 bytes to 2 MB, some 60 folders deep.
- **sync-preview-20k**: the folders of folder-small-files compared, then a mirror plan
  made from the result. The plan reads every file it would touch, and every file on both
  sides whose size or time differs, to fingerprint it; "plan" is the time of that step
  alone.
- **sync-apply-large** (`OXIDEMERGE_BENCH_LARGE=2`): three random 2 GiB files mirrored
  into a copy of a folder that has one of them equal, one changed in its last byte and one
  missing. The time also covers copying the right folder to a fresh working folder before
  the scan, and removing it and the journal afterwards; "preview" and "apply" time the scan
  and plan, and the apply, which fingerprints, copies through a verified temporary file,
  keeps the replaced file in the journal and checks the result.
- **folder-navigation-20k**: the folders of folder-small-files compared, then stepped
  through from the first file with differences to the last, as *Next Difference* does
  without the file pane, reading the status bar position at each file.

## Moved-block detection

The `binary-moved-*` workloads time the moved-block search of the binary view on its own:

```sh
OXIDEMERGE_BENCH_ONLY=binary-moved OXIDEMERGE_BENCH_RUNS=9 cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

Each workload prints what it found. Measured on the machine of the baseline below, release
build, as the median of nine runs including reading the files from the cache:

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| binary-moved-unrelated | 0.056 s | 29 MB | 0 moved blocks |
| binary-moved-swap | 0.014 s | 22 MB | 2 moved blocks, 131,072 matched bytes |
| binary-moved-large | 0.202 s | 142 MB | 2 moved blocks, 2,097,152 matched bytes |
| binary-moved-short-shift | 0.007 s | 17 MB | 1 moved block, 15 matched bytes |
| binary-moved-wide-shift | 0.002 s | 13 MB | 1 moved block, 524,288 matched bytes |
| binary-moved-firmware | 0.001 s | 9 MB | 18 moved blocks, 4,014 matched bytes |

## Baseline

Measured on macOS aarch64, Apple M2 Max, 12 threads, 32 GB memory; OxideMerge 0.9.4,
release build.

The table is shortened from the one the program prints: the Input column (see the
workloads above) and counts of zero are left out, as is the first-result time of
folder-large-files, and large numbers have thousands separators.

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

Added with 0.9.8's table, navigation and synchronization changes (2026-11-02, same
machine):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| table-10k | 0.012 s | 24 MB | 95 modified, 50 added, 55 removed, 9,749 reordered |
| table-10k-position | 0.016 s | 22 MB | 197 differences |
| table-100k | 0.133 s | 135 MB | 1,030 modified, 500 added, 532 removed, 98,850 reordered |
| table-100k-position | 0.231 s | 125 MB | 614 differences |
| table-cancel-1m | 0.479 s | 748 MB | stopped 22.6 ms after asked, at 1,008,192 of 1,999,985 records |
| folder-navigation-20k | 0.653 s | 40 MB | 418 files in 79 ms, 0.19 ms a step, first 4 ms; compare 572 ms |

The table rows are from five runs each; the navigation row from three. Added with the XML
Tree view (2026-11-03, same machine, three runs each):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| xml-tree-12k | 0.153 s | 116 MB | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.172 s | 120 MB | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-semantic-12k | 0.131 s | 128 MB | 29,430 changes |

By position, the Semantic view reports 29,430 changes for the same 0.5% insertions; the
Tree view reports about 380. In the shuffled catalog nearly every item is out of order, so
most are moves. Before the order of the files stepped through was kept between steps, a
step took 2.48 ms (1,035 ms for the 418 files).

Added with the free-space check (2026-11-03, same machine, three runs):

| Workload | Time (median) | Peak memory | Result |
| --- | ---: | ---: | --- |
| sync-apply-large | 74.653 s | 15 MB | 2 changes applied; preview 28,494 ms, apply 45,620 ms |

Files are streamed, so memory stays flat however large they are; the time is reading and
writing the files several times over (scan, fingerprints, copy, journal and final check).

Added with the XML Tree view's drawing and keyboard measurements (2026-11-03), on
**windows x86_64**, Intel Core (24 threads), OxideMerge 0.9.8, release build, three runs
each. Peak memory is measured with `/usr/bin/time`, which Windows has not, so it is
missing:

| Workload | Time (median) | Result |
| --- | ---: | --- |
| xml-tree-12k | 0.226 s | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.260 s | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-tree-view-12k | 0.785 s | 72,297 rows, 376 changes; compare 218 ms, first rows 5 ms, a scrolled frame 0.29 ms, unfold 2 ms, fold 1 ms, a step 1.40 ms over 376 |
| xml-semantic-12k | 0.152 s | 29,430 changes |

Drawing is not what the Tree view costs: of the 785 ms, 218 ms is the comparison and 5 ms
the first rows, and a frame while scrolling 72,297 rows takes 0.29 ms because only the
rows in view are built. Folding and unfolding the whole tree cost 1–2 ms, since neither
re-runs the comparison. A step to the next change costs 1.40 ms, which includes unfolding
the ancestors of the row, scrolling it into view and drawing the frame.

These Windows numbers are not comparable with the macOS numbers above: another machine,
and the times there were measured with the memory tool attached.

## Windows x86_64 (2026-11-11)

Every workload up to `OXIDEMERGE_BENCH_LARGE=1`, then the 500 MB and 1 GiB text workloads,
run natively on Windows 11 (build 26300), Intel Core i9-12900KF (24 threads), 32 GB memory;
OxideMerge 0.9.8 (`e6a117e`), rustc 1.99.0, release build, three runs each, median time:

```powershell
$env:OXIDEMERGE_BENCH_LARGE = "1"
cargo test --release --locked --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
$env:OXIDEMERGE_BENCH_LARGE = "3"
$env:OXIDEMERGE_BENCH_ONLY = "text-500mb,text-1gb,text-view-500mb,text-view-1gb,text-cancel-500mb"
cargo test --release --locked --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

There is no `/usr/bin/time` on Windows, so peak memory and the resident memory of each
stage are missing. With `CARGO_TARGET_DIR` set to a long path (a sandboxed agent's, here),
OpenSSL's build for libssh2 failed in `nmake`; building into the repository's own `target`
worked.

| Workload | Time (median) | Result |
| --- | ---: | --- |
| text-10mb | 0.132 s | 14,305 differences |
| text-repeated | 0.041 s | 300 differences |
| text-100mb | 1.657 s | 15,086 differences |
| text-500mb | 10.979 s | 74,725 differences |
| text-1gb | 24.003 s | 152,307 differences (see below) |
| text-view-10mb | 0.457 s | 303,878 rows; load 46 ms, compare 112 ms, first frame 11 ms, a scrolled frame 1.34 ms, a step 1.32 ms |
| text-view-100mb | 2.498 s | 2,993,833 rows; load 429 ms, compare 1,483 ms, first frame 55 ms, a scrolled frame 1.52 ms, a step 1.47 ms |
| text-view-500mb | 15.032 s | 14,970,455 rows; load 2,031 ms, compare 9,986 ms, first frame 240 ms, a scrolled frame 8.85 ms, a step 8.94 ms |
| text-view-1gb | 33.254 s | 30,659,207 rows, 152,307 differences; load 4,290 ms, compare 22,623 ms, first frame 551 ms, a scrolled frame 18.74 ms, a step 18.51 ms |
| text-cancel-100mb | | read again 417 ms; open 1,938 ms; stopped in 3 ms median, 11 ms worst; freed in 126 ms median, 135 ms worst |
| text-cancel-500mb | | read again 2,125 ms; open 12,073 ms; stopped in 3 ms median, 13 ms worst; freed in 825 ms median, 1,083 ms worst |
| merge3-10mb | 0.441 s | 14,107 differences, conflicts left |
| table-10k | 0.021 s | 95 modified, 50 added, 55 removed, 9,749 reordered |
| table-10k-position | 0.023 s | 197 differences |
| table-100k | 0.288 s | 1,030 modified, 500 added, 532 removed, 98,850 reordered |
| table-100k-position | 0.324 s | 614 differences |
| table-cancel-1m | 0.902 s | stopped 44.4 ms after asked, at 1,004,096 of 1,999,985 records |
| xml-tree-12k | 0.230 s | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.263 s | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-tree-view-12k | 0.952 s | 72,297 rows, 376 changes; compare 219 ms, first rows 6 ms, a scrolled frame 0.29 ms, unfold 2 ms, fold 1 ms, a step 1.83 ms |
| xml-semantic-12k | 0.158 s | 29,430 changes |
| binary-10mb | 0.007 s | 100 ranges |
| binary-100mb | 0.074 s | 100 ranges |
| binary-moved-unrelated | 0.053 s | 0 moved blocks |
| binary-moved-swap | 0.014 s | 2 moved blocks, 131,072 matched bytes |
| binary-moved-large | 0.193 s | 2 moved blocks, 2,097,152 matched bytes |
| binary-moved-short-shift | 0.008 s | 1 moved block, 15 matched bytes |
| binary-moved-wide-shift | 0.003 s | 1 moved block, 524,288 matched bytes |
| binary-moved-firmware | 0.001 s | 18 moved blocks, 4,014 matched bytes |
| image-4k | 0.113 s | 82,116 pixels differ |
| folder-small-files | 0.220 s | 220 differ, 198 unique; first result after 2 ms |
| folder-large-files | 0.039 s | 2 differ |
| folder-mixed | 0.051 s | 80 differ; first result after 2 ms |
| sync-preview-20k | 2.292 s | 198 copies, 220 replaces; plan 2,109 ms |
| folder-navigation-20k | 0.416 s | 418 files in 181 ms, 0.43 ms a step, first 5 ms; compare 193 ms |

As above, the time of the text-cancel workloads is that of ten opens in one process and is
left out.

- Every result count matches the macOS runs except at 1 GiB. text-1gb and text-view-1gb
  compare on the calling thread, not in a job, and there the gaps between anchors share one
  2-second deadline (`walk_limits` in `src/core/diff.rs`). This machine went past it, and the
  gaps left were compared coarsely: 152,307 differences, not the 152,526 of every gap
  compared finely. The window opens large files in a job, where each gap has 2 seconds and
  the walk 20, so these two workloads do not show the count the window gets on this machine.
- What the view holds, counted from sizes and capacities, is the same as on macOS at every
  size (at 1 GiB: lines 3,391 MiB, edit marks 234 MiB, rows 486 MiB, layout 1,448 MiB).
- Against the absolute limits of the release gates below, this machine misses three at the
  largest sizes: the first frame at 1 GiB (551 ms against 500 ms), a scrolled frame and a
  step at 1 GiB (18.74 and 18.51 ms against 16 ms), and freeing the memory after *Cancel* at
  500 MB (1,083 ms worst against 1 s). At 500 MB and below, frames and steps stay under 9 ms
  and *Cancel* stops the work within 13 ms.
- The XML workloads took 1–21% longer than on 2026-11-03 above (xml-tree-view-12k 0.952
  against 0.785 s, a step 1.83 against 1.40 ms) with the same results. The two runs were not
  made one after the other, so this is not a measured regression.

### Line numbering on several threads (2026-11-11)

Before a text comparison, each line is given a number, the same for equal lines. This
step took most of the time of a large comparison: on the machine above, 0.86 s of the
100 MB engine run and 12.7 s of the 1 GiB one, against 2.7 s for the anchor search and 1.2 s
for the walk between anchors. From 262,144 lines (all panes together) it is now shared
among up to eight threads (`core::parallel::threads`, one per processor thread, as a folder
compare uses by default). The lines are hashed in parallel; then each thread owns the lines
whose hash starts with its bits and looks them up in a table of its own, so threads never
share a table. The numbers are the same as on one thread, checked by a unit test with
forced hash collisions, so every comparison result is unchanged. *Cancel* stops all the
threads.

The same machine and inputs, three runs each, median time, before (`e6a117e`, the table
above) and after:

| Workload | Before | After | Result (both) |
| --- | ---: | ---: | --- |
| text-10mb | 0.132 s | 0.099 s | 14,305 differences |
| text-repeated | 0.041 s | 0.039 s | 300 differences |
| text-100mb | 1.657 s | 0.804 s | 15,086 differences |
| text-500mb | 10.979 s | 5.854 s | 74,725 differences |
| text-1gb | 24.003 s | 12.899 s | 152,307 differences |
| merge3-10mb | 0.441 s | 0.318 s | 14,107 differences, conflicts left |
| text-view-10mb | 0.457 s (compare 112 ms) | 0.455 s (compare 86 ms) | 303,878 rows |
| text-view-100mb | 2.498 s (compare 1,483 ms) | 1.696 s (compare 756 ms) | 2,993,833 rows |
| text-view-500mb | 15.032 s (compare 9,986 ms) | 10.002 s (compare 5,138 ms) | 14,970,455 rows |
| text-view-1gb | 33.254 s (compare 22,623 ms) | 23.042 s (compare 11,563 ms) | 30,659,207 rows |
| text-cancel-100mb | open 1,938 ms; stopped 11 ms, freed 135 ms worst | open 1,183 ms; stopped 4 ms, freed 143 ms worst | |
| text-cancel-500mb | open 12,073 ms; stopped 13 ms, freed 1,083 ms worst | open 7,327 ms; stopped 5 ms, freed 828 ms worst | |

- Measured alone, numbering the lines took 0.13 s instead of 0.86 s at 100 MB, and 2.2 s
  instead of 12.7 s at 1 GiB.
- The stages of a workload are those of its last run. At 1 GiB, that run's first frame (713
  ms), scrolled frame (20.14 ms) and step (21.07 ms) were slower than before, but drawing
  is unchanged: three more runs gave 462–604 ms, 17.0–17.8 ms and 16.5–17.5 ms, and totals
  of 19.9–20.3 s.
- Freeing what a cancelled 500 MB open built now stays under the 1 s gate (828 ms worst).
  The first frame and a scrolled frame at 1 GiB are still near or over their limits on this
  machine.
- Reading the two files on two threads was tried and left out. It halved the reading time
  at 1 GiB (4.1 to 2.2 s), but on Windows freeing the lines then took 7.1 s instead of 1.5 s,
  so the run took longer overall (15.4 against 12.1 s), and the freeing after *Cancel* would
  have been slower too.
- The anchor search and the walk between anchors (2.5 and 1.2 s at 1 GiB) still run on one
  thread. The 2-second walk budget outside a job starts before the anchor search, so on
  this machine the 1 GiB count stays 152,307. With the clock started after the anchor
  search, it was 152,526.
- Peak memory was not measured (Windows). The threads' tables together hold as many entries
  as the one table did, and the list of numbers is the same size, so the peak should not
  change, but that was not checked on macOS or Linux.

## Findings

- Text comparisons retain decoded lines and alignment data. Memory depends on line count,
  comparison rules and editing history. Large valid UTF-8 files load incrementally,
  comparison keys borrow unchanged source text, and alignment uses compact IDs; legacy
  encodings still use the complete-buffer decoder. See the dated measurements below rather
  than assuming a fixed memory multiplier for every file.
- Binary and image compares hold every file whole: 2 GB for two 1 GB files.
- A folder compare lists its first items within milliseconds and keeps comparing in the
  background, so large trees are usable at once. *Stop* ends it between files, and the
  status bar then says the comparison was aborted. Folder merges refuse to run on an
  aborted comparison.
- Folder compares by contents read files of up to 16 MB whole, a few at a time, and larger
  ones in 1 MB chunks. Memory follows the number of threads (138 MB for 8 MB files on 12
  threads), not the size of the tree.
- Large files are first split at lines found once on each side, and Myers then runs
  between those anchors. Until 2026-11-10 one 2-second deadline covered the whole walk: once
  it was up, every gap still left was compared coarsely (as one change), so at 1 GiB the
  difference count depended on the machine's speed and load (152,307, 152,424 and 152,526 on
  the same inputs). Each gap now has 2 seconds of its own, with 20 seconds for the whole
  walk, and the 1 GiB count is 152,526 in every run. Comparing every gap finely costs about
  1.5 s more at 1 GiB (17.2 s against 15.5–16.2 s for the engine); 100 MB and 500 MB, which
  never reached the deadline, are unchanged.
- On the macOS 27.3 pre-release this was measured on (build 26C5035r), memory freed in
  large blocks is not reused or given back: loading the 100 MB pair, dropping it and loading
  it again adds about 137 MB of resident memory each time, with the same growth in a
  20-line Rust program outside OxideMerge (it is the blocks freed as the list of lines
  doubles), and none on Linux (glibc) in Docker. `footprint` counts it as dirty *Malloc
  Large* memory, not as reclaimable. A list sized up front does not grow. So on that
  system, opening large comparisons again and again in one window grows its memory until it
  is restarted; it was not checked on a released macOS.

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

The full run exceeded the default 1.5x timing threshold for `merge3-10mb`: 0.592 → 1.071
s, with peak RSS 224 → 226 MB. A seven-run focused diagnostic
(`OXIDEMERGE_BENCH_ONLY=merge3-10mb OXIDEMERGE_BENCH_RUNS=7`) measured 0.623 s / 226 MB
and passed. Both results are retained; the diagnostic does not erase the failed full-run
gate. The discrepancy does not establish a stable regression or prove its absence on this
shared host. Other full-run workloads stayed below the timing threshold. Text 100 MB
measured 2.089 → 2.887 s (1492 → 1494 MB), binary 1 GB 0.406 → 0.401 s (2056 → 2056 MB),
and 20,000-file folders 0.563 → 0.590 s (32 → 34 MB).

These are comparison-engine measurements. They do not measure semantic/impact analysis,
GUI rendering or the extra SHA-256 reads used by folder merge confirmation. They are not
cross-platform release validation.

## Recorded comparison: 0.9.7 impact and merge changes (2026-11-01)

Commit `291d184` (extracted with `git archive`) and the 0.9.7 working tree with
all-or-nothing folder merges, semantic pairing confidence and Tree-sitter impact analysis
were measured on the macOS aarch64 host above, with the default workloads, three runs each
and the same generated inputs. The working tree passed the 1.5x gate against the saved
commit baseline with identical results; for example text 10 MB 0.112 → 0.111 s (99 → 100
MB), 3-way merge 0.340 → 0.345 s (150 MB), 20,000-file folders 0.602 → 0.570 s (36 → 32
MB).

The large workloads
(`OXIDEMERGE_BENCH_LARGE=2 OXIDEMERGE_BENCH_ONLY=text-100mb,binary-1gb`) were then run
with frozen commit and working-tree test executables, alternating them twice with five
runs each on the idle host. Text 100 MB measured 1.054 and 1.073 s at the commit against
1.076 and 1.067 s in the working tree (619–620 MB each), and binary 1 GB 0.340 and 0.341 s
against 0.347 and 0.336 s (2,057 MB each), with the same 15,086 differences and 100
ranges. The differences are within the variation between runs.

The engine workloads do not run impact analysis, so `--impact` was also timed on the egui
0.31.1 and 0.37.0 sources (88 and 132 Rust files, 106 files differing under `src`), three
runs each after a warm-up run: the median time was 0.08 → 0.18 s and the peak memory 28 →
47 MB. Files are parsed with Tree-sitter on up to eight threads; on one thread the working
tree took 0.50 s. The commit reported 17 heuristic findings; the working tree reports 601
exact import, function, type and value findings and 517 heuristic ones, with 1,724 more
name and configuration key matches counted in a note. Five files are listed as partly
assessed because the Rust grammar does not read `dyn 'a + Fn(…)` trait objects and some
attribute positions. The release executable grew from 27.6 to 37.5 MB with the grammars.

## Recorded comparison: 0.9.8 table, navigation and synchronization changes (2026-11-02)

Commit `00eda61` (extracted with `git archive`) and the working tree were built as release
test executables, then run one after the other on the macOS aarch64 host above with the
default workloads, three runs each, reading the same generated inputs. The working tree
passed the 1.5x gate against the commit's saved baseline with the same result counts:

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

The differences are within the variation between runs seen in the earlier records. The
commit has no table, synchronization or navigation workloads, so those have no before
figures.

Matching by key is faster than comparing row by row on these shuffled tables, as the line
alignment of the positional compare has many more changes to work through. Tables of more
than 100,000 records (both sides together) are matched on another thread, which checks for
a stop every 4,096 records; stopping one half way through 1,000,000 records took 23 ms,
freeing the keys it had built included. Stepping to the next file with differences among
20,000 files then took about 2.5 ms, as each step found the file's place in the listed
order again; keeping that order between steps has since brought a step to 0.19 ms (see
folder-navigation-20k above).

## Recorded comparison: text memory changes (2026-11-09)

Commit `1198f5d` with only the benchmark harness added (a `git worktree`), and the working
tree with the memory changes, were run one after the other on the macOS aarch64 host above
(Apple M2 Max, 32 GB), reading the same generated inputs: the 100 MB workloads five times,
the 1 GiB workloads three times, median time and highest peak memory.

| Workload | Before | First version | After the fix (2026-11-10) |
| --- | ---: | ---: | ---: |
| text-100mb | 1.181 s, 620 MB | 1.099 s, 561 MB | 1.004 s, 552 MB |
| text-view-100mb | 1.742 s, 722 MB | 1.671 s, 693 MB | 1.464 s, 673 MB |
| text-1gb | 16.434 s, 5,705 MB | 16.348 s, 5,480 MB | 16.173 s, 5,441 MB |
| text-view-1gb | 22.820 s, 7,725 MB | 24.279 s, 7,007 MB | 22.883 s, 7,017 MB |

The last column was run on 2026-11-10 on the same host and inputs, three runs each.

The view's stages at 1 GiB, before → first version → after the fix: load 3.25 → 3.26 →
3.33 s; comparison and what the view builds from it 16.1 → 17.5 → 16.2 s (5.9 → 5.5 → 5.6 GiB
resident afterwards); first frame 424 → 398 → 423 ms; a frame after jumping to another line
11.1 → 11.4 → 10.2 ms; a step to the next difference 11.0 → 11.2 → 10.0 ms. All versions
found 152,307 differences in 30,659,207 rows.

What changed: lines are numbered through a table of 8-byte slots (where a line was first
seen and 32 bits of its hash) instead of a map holding a 16-byte `&str` per distinct line;
the list of loaded lines gives back its spare room; the view keeps one 32-bit hash per line
instead of 64 bits; and a pane whose rows are one per line keeps no table of row boundaries.

The first version made the 1 GiB view about 6% slower. Timing the numbering alone on the
1 GiB pair, six alternating runs each, gave a median of 7.9–8.9 s for the old map and 8.9–10.1 s
for the first table: its slots held a line's number, and the line to compare against had to
be found through a list of first lines, two more scattered reads for each of the ~55% of
lines that repeat an earlier one. The fixed table holds the place where a line was first
seen, so the line is read straight from it, and the places are turned into numbers in a
second, in-place pass: 8.0 s against 7.9 s for the old map in the same runs, with the same
numbers (checked line for line, and by a unit test).

Limits of this record:

- The engine timing of `text-1gb` did not show the first version's slowdown (16.43 against
  16.35 s) although it numbers lines the same way; run-to-run noise at this size is about
  ±0.5 s, so differences under ~3% are not established either way.
- The lines themselves (a 24-byte `String` and its text for each of 30 million lines a side)
  remain most of the peak; storing them in one buffer would be the next large reduction, and
  it touches several hundred places that read `lines`.
- Peak memory is the process's maximum resident size from `/usr/bin/time -l`; it varies with
  memory pressure on the host. Only macOS was measured; no Windows or Linux figures, no
  500 MiB pair, and no native window run.
- Results were checked to be the same difference count; the unit tests compare diff content.

## Large text results and release gates (2026-11-10)

The working tree after the text memory changes, the background open with *Cancel*, the
highlight limit and the bounded word-difference cache, run on the macOS aarch64 host above
(Apple M2 Max, 12 threads, 32 GB, macOS 27.3 pre-release), three runs each, median time and
highest peak memory, nothing else running:

```sh
OXIDEMERGE_BENCH_LARGE=3 OXIDEMERGE_BENCH_RUNS=3 \
OXIDEMERGE_BENCH_ONLY=text-100mb,text-500mb,text-1gb,text-view-100mb,text-view-500mb,text-view-1gb,text-cancel-100mb,text-cancel-500mb \
  cargo test --release --locked --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

| Workload | Time | Peak | Result |
| --- | ---: | ---: | --- |
| text-100mb | 0.962 s | 552 MB | 15,086 differences |
| text-500mb | 7.697 s | 2,663 MB | 74,725 differences |
| text-1gb | 17.162 s | 5,440 MB | 152,526 differences |
| text-view-100mb | 1.376 s | 674 MB | load 304 ms, compare 814 ms, first frame 37 ms, a scrolled frame 0.63 ms, a step 0.63 ms |
| text-view-500mb | 9.476 s | 3,433 MB | load 1,566 ms, compare 6,607 ms, first frame 172 ms, a scrolled frame 4.03 ms, a step 3.85 ms |
| text-view-1gb | 23.750 s | 7,019 MB | load 3,453 ms, compare 16,719 ms, first frame 404 ms, a scrolled frame 10.84 ms, a step 10.74 ms |
| text-cancel-100mb | | | read again 307 ms; open 1,252 ms; stopped in 2 ms median, 5 ms worst; freed in 79 ms median, 83 ms worst |
| text-cancel-500mb | | | read again 1,501 ms; open 7,546 ms; stopped in 1 ms median, 6 ms worst; freed in 378 ms median, 403 ms worst |

The time and peak of the text-cancel workloads are those of ten opens in one process and are
left out; their peak is also inflated by the allocator finding above.

What the view holds, by kind (from `TextView::memory_use`, counted from sizes and
capacities): at 1 GiB, lines 3,391 MiB, edit marks 234 MiB, rows 486 MiB, layout 1,448 MiB;
at 500 MB, 1,656 + 114 + 237 + 713 MiB; at 100 MB, 331 + 23 + 47 + 155 MiB. Line ends,
highlighting, word-level differences and undo history were 0 in these runs (one line-end
style, no syntax for `.txt`, nothing scrolled into word-level differences yet, no edits). The
lines are about half of the peak; a store that keeps them in one buffer is the next large
reduction and has not been made (several hundred places read `lines`).

Cancelling used to wait for the line diff: the 500 MB open, cancelled halfway through, took
up to 2.1 s to stop. Checks inside the anchor search and the walk between anchors brought the
worst case to 6 ms. Freeing what was built takes longer (0.4 s at 500 MB), but the window does
not wait for it.

### Linux in Docker on the same Mac

The same workloads, except the 1 GiB view, which needs about 7 GB, were run on Linux
aarch64 (glibc) in a Docker VM on the same Mac (12 threads, 7.7 GiB for the VM), three runs
each. It is the same CPU through a virtual machine, not a native Linux computer and not x86:
use it for correctness and for memory, which the Linux kernel measures with glibc's
allocator, not for Linux timings.

| Workload | Time | Peak | Result |
| --- | ---: | ---: | --- |
| text-100mb | 2.263 s | 532 MB | 15,086 differences |
| text-500mb | 13.444 s | 2,562 MB | 74,725 differences |
| text-1gb | 16.448 s | 5,236 MB | 152,526 differences |
| text-view-100mb | 1.593 s | 693 MB | load 356 ms, compare 942 ms, first frame 51 ms, a scrolled frame 0.84 ms, a step 0.83 ms |
| text-view-500mb | 9.738 s | 3,207 MB | load 1,693 ms, compare 6,124 ms, first frame 242 ms, a scrolled frame 4.67 ms, a step 4.72 ms |
| text-cancel-100mb | | 854 MB | stopped in 2 ms median, 5 ms worst; freed in 124 ms median, 126 ms worst |
| text-cancel-500mb | | 3,577 MB | stopped in 2 ms median, 4 ms worst; freed in 362 ms median, 868 ms worst |

- The difference counts match macOS at every size, 1 GiB included (152,526, the count
  every gap compared finely gives; this run was made before the per-gap deadline, when the
  macOS runs still varied).
- Peaks are within 8% of macOS for one open. The cancel workloads, ten opens in one process,
  peak at 854 and 3,577 MB here against 1,774 and 7,095 MB on macOS: on Linux the memory of
  one open is reused by the next.
- The times are noisy: the engine alone (text-100mb, text-500mb) took longer than the view,
  which does the same work and more, so the VM's timings are not reliable at this size.
  Native Linux timings were not measured; for native Windows timings, see
  [Windows x86_64 (2026-11-11)](#windows-x86_64-2026-11-11).

### Release gates

Measured on this host; other machines need a baseline of their own (`OXIDEMERGE_BENCH_SAVE`
on the commit before, then `OXIDEMERGE_BENCH_BASELINE` on the change). A release candidate
passes when, on the same host and inputs:

| Gate | Limit | Now |
| --- | --- | --- |
| Time of each workload above (text-cancel excluded) | at most 1.25 × the table (`OXIDEMERGE_BENCH_TOLERANCE=1.25`) | — |
| Peak of text-100mb / 500mb / 1gb | at most 610 / 2,930 / 5,990 MB (+10%) | 552 / 2,663 / 5,441 MB |
| Peak of text-view-100mb / 500mb / 1gb | at most 745 / 3,780 / 7,720 MB (+10%) | 674 / 3,433 / 7,019 MB |
| First frame | at most 100 ms at 100 MB, 500 ms at 1 GiB | 37 / 404 ms |
| A scrolled frame and a step to the next difference | at most 16 ms (one frame at 60 Hz) at every size | 10.84 ms worst (1 GiB) |
| Work stopped after *Cancel* | at most 50 ms at worst | 6 ms |
| Memory freed after *Cancel* at 500 MB | at most 1 s | 0.40 s |
| Difference count at 100 MB, 500 MB and 1 GiB | exactly 15,086, 74,725 and 152,526 | same |

The harness checks only the time gate on its own; the others are read from its output. The
1 GiB rows were measured again after the per-gap deadline (see the findings); the other rows
are from the run above.

## Large UTF-8 text comparison

On 2026-10-30, two 1 GiB UTF-8 inputs (`text-1gb`, deterministic generator seed 7, about
30.6 million lines per side, one line in 200 changed/inserted/deleted) were compared on
the same Apple M2 Max host described above. Frozen release test executables captured the
0.9.6 working tree immediately before and after the large-text changes. Both runs reused
identical fixture files, used default comparison rules, and ran sequentially after
compilation. Each version ran three times: the median elapsed time was
**29.992 → 17.225 seconds**; the maximum native peak RSS was **10,002 → 5,705 MiB** (about
43% lower for each metric). Both produced **152,307 difference blocks**. These timings
include loading and comparison, not window rendering or editing. The initial single-run
capture was 44.392 s / 4,946 MiB; that differing result illustrates why the matched
three-run measurements are reported here. Resident memory varies with host memory pressure
and should not be treated as a fixed file-size multiplier.

Large valid UTF-8 documents load through a bounded read buffer, preserving BOM,
CR/LF/CRLF, mixed terminators and final-newline state. Invalid UTF-8 and other encodings
fall back to the existing detector. Unchanged keys borrow line text; dictionaries leave
scope before alignment. Alignment rows use 16 bytes instead of 64, dense anchor positions
and predecessors use compact indices, and anchor reconstruction reuses its pair buffer.
Pane display maps reference shared rows rather than copying every display line. Syntax
workers reject oversized input before making a full document copy. Documents and editing
history still consume memory proportional to content.

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

The 11 existing workloads also completed with unchanged result counts. Their full-run
timing gate reported `folder-mixed` at 0.086 s against an earlier 0.055 s reference,
exceeding 1.5x. A matched seven-run check using the frozen pre-change/current executables
measured 0.054 → 0.053 s, both at 38 MiB, and passed. The full-run failure is retained in
this record; the focused check does not erase it. Other workloads stayed under the
existing timing gate: 100 MiB text measured 1.221 s / 620 MiB and 10 MiB three-way merge
0.361 s / 150 MiB.

Validation includes formatting, all-target checking, Clippy with warnings denied, 278
unit/GUI and 25 CLI tests, plus the four standard native GUI smoke cases on macOS.
Windows/Linux native release and memory measurements were not performed on this host.

The final native app also opened the same two 1 GiB files and completed its ten-frame
smoke run in 25.08 s, reporting `Some(Different)` and 152,307 differences without an
error. Its peak RSS was 7.32 GiB (the macOS peak memory footprint was 8.80 GiB); these GUI
figures include display maps and window state and are separate from the comparison-engine
figures. No pre-change GUI measurement was taken, so no GUI speed/memory ratio is claimed.

Frozen executable SHA-256 identifiers for the matched three-run engine comparison:

- Before: `fc8694e81e306ffda62b38a273367f08ddcd404f1a058028f5decb44a4759f02`.
- After: `6027340929251b22d0b19b325efaf582a6a7cd37855c25078ecfcee5a1403b56`.
