# Benchmarks

The benchmarks time the compare engines on generated inputs. They don't run the window, so they
measure the work behind it: loading, comparing and merging. Run them on an idle machine:

```sh
cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The inputs are made once into `target/bench-data`: about 850 MB on disk, 1.3 GB with
`OXIDEMERGE_BENCH_LARGE=1` and 3.2 GB with `=2`. Each workload then runs three times, each in a process of its own; the median time and
the highest peak memory are reported and written to `target/bench/results.md`, with the machine,
the commit and the build. Peak memory is measured with `/usr/bin/time`, so it is missing on
Windows. The files are read from the system's file cache after the first run, so the times
don't include disk reads.

| Variable | Effect |
| --- | --- |
| `OXIDEMERGE_BENCH_LARGE=1` | Adds the 100 MB text and binary compares; `=2` also a 1 GB binary compare |
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

- A text compare holds both files as lines and needs about 7 times the size of its input in
  memory (1.5 GB for two 100 MB files). Files of a few hundred MB are the practical limit on
  a machine with 8 GB.
- Binary and image compares hold every file whole: 2 GB for two 1 GB files.
- A folder compare lists its first items within milliseconds and keeps comparing in the
  background, so large trees are usable at once. *Stop* ends it between files, and the status
  bar then says the comparison was aborted. Folder merges refuse to run on an aborted comparison.
- Folder compares by contents read files of up to 16 MB whole, a few at a time, and larger ones
  in 1 MB chunks. Memory follows the number of threads (138 MB for 8 MB files on 12 threads),
  not the size of the tree.
