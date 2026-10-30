# Benchmarks

The benchmarks time the compare engines on generated inputs. They don't run the window, so they
measure the work behind it: loading, comparing and merging. Run them on an idle machine:

```sh
cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The inputs are made once into `target/bench-data`: about 850 MB on disk, 1.3 GB with
`OXIDEMERGE_BENCH_LARGE=1` and 3.2 GB with `=2`; `=3` adds about 2 GB for the 1 GB text pair. Each workload then runs three times, each in a process of its own; the median time and
the highest peak memory are reported and written to `target/bench/results.md`, with the machine,
the commit and the build. Peak memory is measured with `/usr/bin/time`, so it is missing on
Windows. The files are read from the system's file cache after the first run, so the times
don't include disk reads.

| Variable | Effect |
| --- | --- |
| `OXIDEMERGE_BENCH_LARGE=1` | Adds the 100 MB text and binary compares; `=2` also a 1 GB binary compare; `=3` also two 1 GB text inputs |
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
- **merge3-10mb**: three 10 MB files compared as a 3-way compare, then merged automatically.
- **binary-10mb**, **binary-100mb**, **binary-1gb**: two random files with 100 bytes changed.
- **image-4k**: two 3840 x 2160 PNG images with 1% of the pixels changed, decoded and compared.
- **folder-small-files**: 20,000 files of 1 KB up to 8 folders deep, 1% changed and 1% on one
  side only. "First result" is the time until the first item reaches the folder list.
- **folder-large-files**: 20 files of 8 MB, two changed in their last byte.
- **folder-mixed**: 2,000 files of 100 bytes to 2 MB, some 60 folders deep.

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
