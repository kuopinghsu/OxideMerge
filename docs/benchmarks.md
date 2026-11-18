# Benchmarks

The benchmarks time OxideMerge's compare engines on generated inputs: loading, comparing, merging
and synchronizing, without the window. A few also drive a view off screen, to time what using it
costs: the first frame, scrolling and stepping to the next difference.

This page gives the latest results first, then the release gates they are checked against, what they
show, the remote-folder and executable-size measurements, a short history, and how to run the
benchmarks yourself. The workloads are described at the end ([Workloads](#workloads)).

## Machines

| Column | Machine | Build |
| --- | --- | --- |
| **Windows** | Windows 11 (build 26300), Intel Core i9-12900KF (24 threads), 32 GB | 2026-11-18: OxideMerge 0.9.9, built with `release-size` as the packages are; rustc 1.100.0 |
| **Linux** | Ubuntu in WSL2 on the same PC, 15 GB given to the virtual machine | 2026-11-18: OxideMerge at `b55dd64` (reports 0.9.10), built with `release-size`; rustc 1.100.0 |
| **macOS** | Apple M2 Max (12 threads), 32 GB; macOS 27.3 pre-release for the large text runs | 2026-11-02 to 2026-11-10, before line numbering on several threads |

Each figure is the median of three runs (five for some table and size runs), each run in a process
of its own, with the inputs already in the file cache. Peak memory is the highest resident size from
`/usr/bin/time`, which Windows does not have, so Windows has times only. The three machines are not
comparable with each other as hardware: compare a machine with itself over time.

The Windows column is one run, of the source of the `v0.9.9` tag (its build reports 0.9.10, from the
version bump not yet committed), with other programs open but idle. text-100mb is the median of nine
runs: three gave 1.627 s, from something else on the machine, and nine gave 0.811 s.

The Linux column is one pass of the workloads, three runs each, on an otherwise idle virtual machine.
text-view-10mb, xml-tree-view-12k and sync-preview-20k are the median of five runs made again
straight after: sync-preview-20k first took 7.354 s, from reading a cold cache (see
[Running the benchmarks](#running-the-benchmarks)).

The macOS text times are from before line numbering ran on several threads; on Windows that change
halved the time of the large text workloads (see [History](#history)). Every result
count is the same on all three machines, except the 1 GiB difference count explained under
[What the results show](#what-the-results-show).

## Results

Times are in seconds unless shown otherwise; memory is peak resident memory in MB. A dash means the
workload was not run on that machine.

### Text compare

| Workload | Windows | Linux | macOS | Result |
| --- | ---: | ---: | ---: | --- |
| text-10mb | 0.090 s | 0.085 s, 86 MB | 0.112 s, 100 MB | 14,305 differences |
| text-repeated | 0.041 s | 0.048 s, 52 MB | 0.025 s, 41 MB | 300 differences |
| text-100mb | 0.811 s | 0.728 s, 538 MB | 0.962 s, 552 MB | 15,086 differences |
| text-500mb | 5.147 s | 4.217 s, 2,562 MB | 7.697 s, 2,663 MB | 74,725 differences |
| text-1gb | 11.062 s | 9.295 s, 5,233 MB | 17.162 s, 5,440 MB | 152,526 differences (152,372 on Windows, 152,447 on Linux) |
| merge3-10mb | 0.284 s | 0.237 s, 125 MB | 0.321 s, 149 MB | 14,107 differences, conflicts left |

### Text view

The same files opened in the real text view, off screen. *Load* reads both files; *compare* includes
everything the view builds from the comparison; a *scrolled frame* is a frame after jumping to
another line, and a *step* moves to the next difference and draws it. One frame at 60 Hz is 16 ms.

| Workload | Machine | Total | Peak | Load | Compare | First frame | Scrolled frame | Step |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| text-view-10mb | Windows | 0.449 s | — | 44 ms | 79 ms | 14 ms | 1.41 ms | 1.40 ms |
| | Linux | 0.394 s | 114 MB | 49 ms | 68 ms | 12 ms | 1.24 ms | 1.24 ms |
| text-view-100mb | Windows | 1.676 s | — | 412 ms | 715 ms | 58 ms | 1.60 ms | 1.57 ms |
| | Linux | 1.522 s | 694 MB | 383 ms | 665 ms | 70 ms | 1.39 ms | 1.34 ms |
| | macOS | 1.376 s | 674 MB | 304 ms | 814 ms | 37 ms | 0.63 ms | 0.63 ms |
| text-view-500mb | Windows | 9.483 s | — | 2,055 ms | 4,537 ms | 229 ms | 9.99 ms | 9.24 ms |
| | Linux | 8.480 s | 3,225 MB | 1,826 ms | 4,033 ms | 394 ms | 8.48 ms | 8.36 ms |
| | macOS | 9.476 s | 3,433 MB | 1,566 ms | 6,607 ms | 172 ms | 4.03 ms | 3.85 ms |
| text-view-1gb | Windows | 20.164 s | — | 4,275 ms | 9,490 ms | 474 ms | 20.04 ms | 20.79 ms |
| | Linux | 18.410 s | 6,495 MB | 3,887 ms | 8,835 ms | 1,064 ms | 17.76 ms | 17.80 ms |
| | macOS | 23.750 s | 7,019 MB | 3,453 ms | 16,719 ms | 404 ms | 10.84 ms | 10.74 ms |

The rows are 303,878, 2,993,833, 14,970,455 and about 30,659,207 for the four sizes (at 1 GiB the
count follows the difference count, below). The 1 GiB frame times move between runs by 15–20%: the
quick `--release` build, the same day, drew the first frame in 606 ms, a scrolled frame in 17.49 ms
and a step in 17.20 ms. On Linux, the same source built with rustc 1.97.1 drew the first frame at
1 GiB in 916 ms and a step in 17.54 ms.

### Cancelling a large open

The window opens large files on another thread. The text-cancel workloads stop that open with
*Cancel* at nine points from 10% to 90% of the way through, and report the worst of the nine.
*Stopped* is when the work noticed and stopped; *freed* is when the thread had also given back what
it built. The window waits for neither.

| Workload | Machine | Whole open | Stopped (worst) | Freed (worst) |
| --- | --- | ---: | ---: | ---: |
| text-cancel-100mb | Windows | 1,137 ms | 4 ms | 129 ms |
| | Linux | 968 ms | 4 ms | 103 ms |
| | macOS | 1,252 ms | 5 ms | 83 ms |
| text-cancel-500mb | Windows | 6,435 ms | 4 ms | 791 ms |
| | Linux | 5,475 ms | 17 ms | 497 ms |
| | macOS | 7,546 ms | 6 ms | 403 ms |

### Tables and XML

| Workload | Windows | Linux | macOS | Result |
| --- | ---: | ---: | ---: | --- |
| table-10k | 0.018 s | 0.019 s, 28 MB | 0.012 s, 24 MB | 95 modified, 50 added, 55 removed, 9,749 reordered |
| table-10k-position | 0.024 s | 0.021 s, 27 MB | 0.016 s, 22 MB | 197 differences |
| table-100k | 0.285 s | 0.256 s, 160 MB | 0.133 s, 135 MB | 1,030 modified, 500 added, 532 removed, 98,850 reordered |
| table-100k-position | 0.350 s | 0.282 s, 142 MB | 0.231 s, 125 MB | 614 differences |
| table-cancel-1m | 0.938 s | 0.934 s, 944 MB | 0.479 s, 748 MB | stopped 44 / 34 / 23 ms after asked, half way through |
| xml-tree-12k | 0.227 s | 0.179 s, 102 MB | 0.153 s, 116 MB | 72,297 rows; 59 added, 64 removed, 252 modified, 1 moved, 1 ambiguous |
| xml-tree-12k-keyed | 0.264 s | 0.209 s, 102 MB | 0.172 s, 120 MB | 72,302 rows; 60 added, 65 removed, 248 modified, 11,718 moved, 96 ambiguous |
| xml-tree-view-12k | 0.966 s | 0.837 s, 110 MB | — | 72,297 rows, 376 changes |
| xml-semantic-12k | 0.161 s | 0.162 s, 121 MB | 0.131 s, 128 MB | 29,430 changes |

In the XML Tree view, drawing is cheap next to the comparison. On Linux, of 0.837 s the comparison
takes 167 ms and the first rows 5 ms; a frame while scrolling 72,297 rows takes 0.32 ms, because
only the rows in view are built; folding and unfolding the whole tree take 1–2 ms; and a step to the
next change, drawn, takes 1.66 ms (Windows: 220, 7, 0.35, 1–2 and 1.84 ms).

### Binary and images

| Workload | Windows | Linux | macOS | Result |
| --- | ---: | ---: | ---: | --- |
| binary-10mb | 0.007 s | 0.010 s, 31 MB | 0.003 s, 29 MB | 100 ranges |
| binary-100mb | 0.068 s | 0.081 s, 211 MB | 0.031 s, 208 MB | 100 ranges |
| binary-1gb | — | — | 0.34 s, 2,057 MB | 100 ranges |
| binary-moved-unrelated | 0.049 s | 0.050 s, 32 MB | 0.056 s, 29 MB | 0 moved blocks |
| binary-moved-swap | 0.013 s | 0.014 s, 24 MB | 0.014 s, 22 MB | 2 moved blocks, 131,072 matched bytes |
| binary-moved-large | 0.185 s | 0.190 s, 144 MB | 0.202 s, 142 MB | 2 moved blocks, 2,097,152 matched bytes |
| binary-moved-short-shift | 0.008 s | 0.008 s, 20 MB | 0.007 s, 17 MB | 1 moved block, 15 matched bytes |
| binary-moved-wide-shift | 0.003 s | 0.003 s, 14 MB | 0.002 s, 13 MB | 1 moved block, 524,288 matched bytes |
| binary-moved-firmware | 0.001 s | 0.001 s, 13 MB | 0.001 s, 9 MB | 18 moved blocks, 4,014 matched bytes |
| image-4k | 0.135 s | 0.163 s, 140 MB | 0.103 s, 105 MB | 82,116 pixels differ |

On Windows, image-4k takes 0.109 s in the quick `--release` build: the size-tuned profile the
packages use costs it about 25%. On Linux it went from 0.125 s with the old `--release` profile to
0.163 s with `release-size`, about 30% (see
[Executable size](#executable-size-and-the-release-profile-2026-11-16)).

### Folders and synchronization

| Workload | Windows | Linux | macOS | Result |
| --- | ---: | ---: | ---: | --- |
| folder-small-files | 0.214 s | 0.238 s, 30 MB | 0.599 s, 33 MB | 220 differ, 198 unique; first result after 2 ms |
| folder-large-files | 0.032 s | 0.030 s, 150 MB | 0.013 s, 139 MB | 2 differ |
| folder-mixed | 0.043 s | 0.025 s, 51 MB | 0.054 s, 37 MB | 80 differ; first result after 2 ms |
| folder-navigation-20k | 0.363 s | 0.341 s, 30 MB | 0.653 s, 40 MB | 418 files; a step 0.34 / 0.24 / 0.19 ms |
| sync-preview-20k | 2.375 s | 0.712 s, 28 MB | 2.005 s, 33 MB | 198 copies, 220 replaces; plan 2,169 / 489 / 1,427 ms |
| sync-apply-large | — | — | 74.653 s, 15 MB | 2 changes applied; preview 28.5 s, apply 45.6 s |

## Release gates

The gates were set on the macOS machine: a release candidate passes when, on the same machine and
inputs, it stays within these limits. On another machine, the limits that are absolute (frames,
*Cancel*, counts) still say whether the program feels responsive there; the time limit needs a
baseline made on that machine (see [Baselines](#baselines)).

| Gate | Limit | macOS | Windows | Linux |
| --- | --- | --- | --- | --- |
| Time of each workload (text-cancel left out) | at most 1.25 × the machine's baseline | pass | — | — |
| Peak of text-100mb / 500mb / 1gb | 610 / 2,930 / 5,990 MB | pass: 552 / 2,663 / 5,440 | not measured | pass: 538 / 2,562 / 5,233 |
| Peak of text-view-100mb / 500mb / 1gb | 745 / 3,780 / 7,720 MB | pass: 674 / 3,433 / 7,019 | not measured | pass: 694 / 3,225 / 6,495 |
| First frame | 100 ms at 100 MB, 500 ms at 1 GiB | pass: 37 / 404 ms | pass: 58 / 474 ms (606 ms in the quick build) | **over at 1 GiB**: 70 / 1,064 ms |
| A scrolled frame and a step | 16 ms at every size | pass: 10.84 ms | **over at 1 GiB**: 20.79 ms | **over at 1 GiB**: 17.80 ms |
| Work stopped after *Cancel* | 50 ms | pass: 6 ms | pass: 4 ms | pass: 17 ms |
| Memory freed after *Cancel* at 500 MB | 1 s | pass: 0.40 s | pass: 0.79 s | pass: 0.50 s |
| Difference count at 100 MB, 500 MB, 1 GiB | 15,086, 74,725, 152,526 | pass | 152,372 at 1 GiB | 152,447 at 1 GiB |

The harness checks only the time gate itself; the others are read from its output. The misses are
all at 1 GiB: a scrolled frame and a step on Windows (about 20 ms) and Linux (about 18 ms), and the
first frame on Linux (0.9–1.1 s). At 500 MB and below, every machine draws a scrolled frame or a step
in under 10 ms and stops after *Cancel* within 17 ms. The 1 GiB count on Windows and Linux comes from the benchmark, not the window:
see below.

## What the results show

- **Text memory follows the lines.** A 1 GiB pair peaks at about 5.2–5.4 GB in the engine and
  6.5–7.0 GB in the view. What the view holds at 1 GiB is the lines (3,391 MiB), the layout (1,448
  MiB), the rows (486 MiB) and the edit marks (234 MiB); at 500 MB about half of that, and at 100 MB
  a tenth. The lines are about half of the peak; keeping them in one buffer is the next large
  reduction and has not been made.
- **Using the view does not depend on the file size up to 500 MB.** Only the rows in view are built,
  so a scrolled frame or a step takes under 2 ms up to 100 MB and under 10 ms at 500 MB. At 1 GiB it
  is 11–21 ms, around the 16 ms of one frame.
- **Cancel is immediate.** The open stops within 17 ms on every machine; freeing what it built takes
  up to 0.8 s at 500 MB, but the window does not wait for it.
- **Binary and image compares hold the files whole**: two 1 GB files take 2 GB.
- **Folder compares are usable at once.** The first items are listed within 2 ms while the rest is
  compared in the background. Memory follows the number of threads (138 MB for 8 MB files on 12
  threads), not the size of the tree. *Stop* ends a comparison between files, and a folder merge
  refuses to run on a stopped comparison.
- **Synchronization streams.** Mirroring three 2 GiB files peaks at 15 MB; the time is reading and
  writing the files several times (scan, fingerprints, copy, journal and final check).
- **The 1 GiB difference count.** Large files are first split at lines found once on each side, and
  the gaps between are compared with a time limit. In the window, each gap has 2 seconds and the
  whole walk 20, so a 1 GiB pair always gives 152,526 differences. The text-1gb and text-view-1gb
  workloads compare on the calling thread, where the gaps share one 2-second limit; Windows and
  Linux pass it, and the gaps left are compared coarsely, so the count there depends on how far the
  walk got: 152,307 to 152,447 in these runs. Comparing every gap finely costs about 1.5 s at 1 GiB.
- **Linux is faster than Windows on the same CPU** for large text and synchronization: with the
  same build profile and compiler, text-1gb 9.30 against 11.06 s, and the sync plan 489 against
  2,169 ms, which is file system calls. Binary, image and table workloads are within a few percent
  of each other, and the 1 GiB view draws its first frame faster on Windows (474 against 1,064 ms).
- **On WSL, the timings need the inputs in the page cache.** The 2026-11-11 run loaded the 1 GiB
  view in 40.5 s, reading the files from disk again; in this run it took 3.9 s. The cache can also
  be emptied while the virtual machine sits idle (see
  [Running the benchmarks](#running-the-benchmarks)).
- **On the macOS 27.3 pre-release, large freed blocks are not reused.** Loading the 100 MB pair,
  dropping it and loading it again adds about 137 MB of resident memory each time, also in a 20-line
  Rust program outside OxideMerge, and not on Linux. Opening large comparisons again and again in
  one window grows its memory there until it is restarted. It was not checked on a released macOS.

## Remote folders

### SFTP

Measured on the Windows machine against OpenSSH in Docker Desktop on the same machine (`atmoz/sftp`,
password sign-in), with the FreeRTOS kernel sources: 886 files in 306 folders, 19.2 MB (2026-11-13).
Each step connects first. *Kept* takes the connections a step within 30 seconds before left open.

| Step | Before (`0ba994c`) | Now | Now, kept |
| --- | ---: | ---: | ---: |
| connect: sign in | 541 ms | 243 ms | 0 ms |
| scan: list both folders, as *List folders without downloading files* | 861 ms | 549 ms | 137 ms |
| verify: compare every file byte for byte, as *Verify All* | 2,048 ms | 663 ms | 236 ms |
| fetch: download the whole folder | 5,938 ms | 1,123 ms | 756 ms |

A whole command-line compare of the two folders, to a text report, went from 6.3 s to 1.3 s.

- Connecting to `localhost` used to wait 250 ms on `::1` before trying `127.0.0.1`; both are now
  tried at once, and `TCP_NODELAY` saves another 40 ms. Most of the rest is the server's.
- Up to six connections read at once. A small file takes about four round trips, so the gain grows
  with the network's round-trip time; on this machine two, four, six and nine connections fetched in
  1,448, 1,192, 1,099 and 1,043 ms (one connection: 2,159 ms).
- Each further connection signs in first (240 ms here), so they are made only once 32 files or 8
  folders are waiting. Kept connections take that cost out of every step after the first.

### FTP

Measured against the suite's own FTP server in the same process, which delays each answer to stand
in for a network: 100 folders of 3 small files fetched, signing in anew each time, on the Windows
machine (2026-11-13).

| Delay per command | MLSD, one connection | MLSD, four | LIST only, one | LIST only, four |
| ---: | ---: | ---: | ---: | ---: |
| 0 ms | 407 ms | 171 ms | 755 ms | 488 ms |
| 1 ms | 1,745 ms | 562 ms | 2,868 ms | 976 ms |
| 2 ms | 3,270 ms | 1,167 ms | 6,191 ms | 3,050 ms |
| 5 ms | 6,557 ms | 1,815 ms | 8,703 ms | 2,581 ms |

Run again on 2026-11-18 with the default 2 ms delay and the 0.9.9 build, the four columns took
2,507, 673, 4,361 and 1,175 ms.

A server without `MLSD` gives no sizes or times in its listing, so each file costs a `SIZE` and an
`MDTM` more. Windows sleeps in steps of its timer, so the delays are longer than asked. Every file
and listing takes a data connection, and Windows keeps each one's port for a while: run several
times in a row, the benchmark runs out of ports (error 10022), and several connections then measure
slower than one. Wait two minutes between runs.

## Executable size and the release profile (2026-11-16)

The release executable had grown to 55.3 MiB on Windows. Four release profiles were built from the
same source (OxideMerge 0.9.9, rustc 1.100.0) and timed with the default workloads on the Windows
machine, five runs each:

| Release profile | `oxidemerge.exe` | Change | Build from clean |
| --- | ---: | ---: | ---: |
| `opt-level = 3`, `lto = "thin"` (before) | 57,991,168 bytes (55.3 MiB) | | about 9 min |
| + `codegen-units = 1`, `strip = true` | 56,845,312 bytes (54.2 MiB) | −2.0% | 14 min |
| + `lto = "fat"` | 55,169,536 bytes (52.6 MiB) | −4.9% | 22 min |
| + `opt-level = "s"` (`release-size`) | 46,322,688 bytes (44.2 MiB) | −20.1% | 17 min |

The size figures are also kept in [`analysis-benchmarks.json`](analysis-benchmarks.json) under
`executable_size`. Since 2026-11-17 the last row is the `release-size` profile, which CI uses to
build and package the releases; `cargo build --release` is a quick profile for development
(`opt-level = 3`, no fat LTO, `strip`).

On 0.9.9 (2026-11-18, the Windows machine, three runs each), the two profiles take the same time
except where noted:

| Workload | Quick `release` | `release-size` (shipped) |
| --- | ---: | ---: |
| text-10mb | 0.099 s | 0.090 s |
| text-100mb (nine runs) | 0.806 s | 0.811 s |
| text-1gb | 12.007 s | 11.062 s |
| text-view-1gb | 20.572 s; first frame 606 ms, a step 17.20 ms | 20.164 s; first frame 474 ms, a step 20.79 ms |
| merge3-10mb | 0.323 s | 0.284 s |
| table-100k | 0.274 s | 0.285 s |
| xml-tree-view-12k | 1.056 s | 0.966 s |
| image-4k | 0.109 s | 0.135 s (25% slower) |
| folder-navigation-20k | 0.351 s | 0.363 s |
| sync-preview-20k | 2.348 s | 2.375 s |

- With the size-tuned profile, every workload is within run-to-run variation of the quick one,
  except image-4k, 25% slower, from decoding and comparing the images at the smaller optimization
  level. Building the `image` crates with `opt-level = 3` should win that back; it was not measured.
  The 1 GiB frame times differ between the two builds, but they move by as much between two runs of
  the same build.
- `panic = "abort"` was not tried: *Cancel* stops a job by unwinding out of it (`core::cancel`),
  which aborting would turn into a crash.
- Where the size goes (the old profile's linker map): of 34.3 MB of code, OxideMerge's own crate
  with what was inlined into it is about 18 MB; libssh2 with OpenSSL about 4 MB; the PDF export
  stack (krilla, skrifa, vello_cpu, harfrust, write-fonts, subsetter) about 3.5 MB; rustls, ring and
  ureq about 1.5 MB. Of 19.4 MB of read-only data, the Tree-sitter grammars' parse tables are 8.1
  MB. Every one of these backs a documented feature, so no dependency was removed.

## History

The measurements that changed the results above, newest first. Each was made on one machine, before
and after the change, with the same inputs.

- **2026-11-18, Linux with `release-size` and rustc 1.100.0:** the whole Linux column measured
  again, after line numbering on several threads and with the build the packages use. Against
  2026-11-11 (quick `--release`): text-500mb 4.76 → 4.22 s, text-1gb 11.07 → 9.30 s,
  text-view-500mb 9.21 → 8.48 s, text-view-1gb 55.7 → 18.4 s (the load 40.5 → 3.9 s, from the cache).
  Slower: image-4k 0.125 → 0.163 s, from the size-tuned profile; text-view-10mb 0.350 → 0.394 s and
  xml-tree-view-12k 0.793 → 0.837 s. The first frame at 1 GiB went from 800 to 1,064 ms and a step
  from 15.17 to 17.80 ms, now over the 16 ms frame. The same source built with rustc 1.97.1 was
  slower at the large sizes: text-1gb 10.54 s, text-view-1gb 19.44 s, text-100mb 0.777 s
  (1.100.0: 9.30, 18.41 and 0.728 s); the other workloads were within run-to-run variation.

- **2026-11-18, 0.9.9 on Windows:** the whole Windows column measured again, with the `release-size`
  build the packages use. Everything is the same as or slightly faster than the 2026-11-11 and
  2026-11-16 runs: text-500mb 5.85 → 5.15 s, text-1gb 12.90 → 11.06 s, text-view-1gb 23.04 → 20.16
  s, with the first frame at 1 GiB now under its limit (474 ms).
- **2026-11-17, release profiles:** `--release` became a quick profile for development, and the
  packages are built with `release-size`.
- **2026-11-16, release profile:** the Windows executable went from 55.3 to 44.2 MiB, with the same
  speed except image-4k.
- **2026-11-13, remote folders:** several connections at once, connections kept between steps and
  faster connecting. A command-line compare of 886 files over SFTP went from 6.3 to 1.3 s.
- **2026-11-11, line numbering on several threads:** giving each line its number took most of a
  large comparison (12.7 s of the 1 GiB one) and now runs on up to eight threads, with the same
  results. On Windows: text-100mb 1.657 → 0.804 s, text-500mb 10.98 → 5.85 s, text-1gb 24.00 → 12.90
  s, text-view-1gb 33.25 → 23.04 s, and freeing after *Cancel* at 500 MB 1,083 → 828 ms. Reading the
  two files on two threads was tried and left out: on Windows, freeing the lines then took longer
  than the reading saved.
- **2026-11-09 and 2026-11-10, text memory and *Cancel*:** smaller line tables and row data. On
  macOS, text-1gb 5,705 → 5,441 MB and text-view-1gb 7,725 → 7,017 MB at the same speed. Checks
  inside the line comparison brought the worst stop after *Cancel* at 500 MB from 2.1 s to 6 ms.
  Each gap between matched lines got a time limit of its own, so the 1 GiB count no longer depends
  on the machine's speed.
- **2026-11-03:** the XML Tree view and sync-apply-large workloads were added.
- **2026-11-02 (0.9.8):** the table, synchronization and folder-navigation workloads were added; the
  other workloads were unchanged. Keeping the order of the listed files between steps brought a step
  to the next file with differences from 2.48 to 0.19 ms.
- **2026-11-01 (0.9.7):** impact analysis with Tree-sitter grammars. The compare engines were
  unchanged; `--impact` on two egui releases went from 0.08 to 0.18 s while finding 601 exact and
  517 heuristic impacts instead of 17 heuristic ones, and the executable grew from 27.6 to 37.5 MB
  with the grammars.
- **2026-10-30 (0.9.6):** large UTF-8 files are read incrementally and compared with compact keys.
  On macOS, text-1gb went from 29.99 to 17.23 s and from 10,002 to 5,705 MiB peak.
- **0.9.4:** the first baseline, on the macOS machine. text-100mb took 2.10 s and 1,492 MB; it now
  takes under 1 s and 552 MB.

Twice, a full run flagged one workload as more than 1.5 times slower (merge3-10mb in 0.9.6,
folder-mixed on 2026-10-30), and a focused seven-run check of the same builds passed. Single full
runs on a busy machine are noisy; check a flagged workload with more runs before treating it as a
regression.

## Running the benchmarks

Run them on an idle machine, from the repository:

```sh
cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The inputs are made once into `target/bench-data`: about 1 GB on disk, 1.3 GB with
`OXIDEMERGE_BENCH_LARGE=1`, and about 14 GB with `=2`, of which 10 GiB are the five 2 GiB files of
sync-apply-large, which also needs about 10 GiB more free space while it runs. `=3` adds about 2 GB
for the 1 GB text pair and 1 GB for the 500 MB one. Each workload runs three times, each in a
process of its own; the median time and the highest peak memory are printed and written to
`target/bench/results.md`, with the machine, the commit and the build.

`--release` is the quick development profile. To measure the build the packages ship, use
`--profile release-size` instead (a clean build of it takes about 17 minutes); the results on this
page are of that build on Windows.

| Variable | Effect |
| --- | --- |
| `OXIDEMERGE_BENCH_LARGE=1` | Adds the 100 MB text and binary compares; `=2` also a 1 GB binary compare and the synchronization of 2 GB files; `=3` also the 500 MB and 1 GB text inputs |
| `OXIDEMERGE_BENCH_ONLY=text,folder` | Runs only the workloads whose names contain one of these |
| `OXIDEMERGE_BENCH_RUNS=5` | Number of runs per workload (default 3) |
| `OXIDEMERGE_BENCH_SAVE=base.txt` | Saves the times as a baseline |
| `OXIDEMERGE_BENCH_BASELINE=base.txt` | Fails if a workload that took at least 50 ms is now more than `OXIDEMERGE_BENCH_TOLERANCE` (default 1.5) times slower |
| `OXIDEMERGE_BENCH_PROFILE=file.log` | Records line lengths, capacities and resident memory at each stage of loading and comparing; it adds overhead, so leave it off for timing |

On WSL, keep the virtual machine busy between the build and the runs, or run each pass twice: when
it sits idle, the page cache can be emptied, and the first runs then read from the virtual disk. In
the 2026-11-18 Linux run, sync-preview-20k took 7.354 s for that reason (1 MB/s of reads with 70%
of the time waiting for I/O, as `/proc/pressure/io` showed) and 0.712 s run again straight after.

On Windows, peak memory and the resident memory of each stage are missing: they come from
`/usr/bin/time` and `ps`. Build from PowerShell, not Git Bash, whose Perl cannot build OpenSSL for
libssh2, and keep `CARGO_TARGET_DIR` short: with a long path, that build fails in `nmake`. The large
inputs are best run on their own:

```powershell
$env:OXIDEMERGE_BENCH_LARGE = "3"
$env:OXIDEMERGE_BENCH_ONLY = "text-500mb,text-1gb,text-view-500mb,text-view-1gb,text-cancel-500mb"
cargo test --release --locked --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

The moved-block search of the binary view can be timed on its own, with more runs:

```sh
OXIDEMERGE_BENCH_ONLY=binary-moved OXIDEMERGE_BENCH_RUNS=9 \
  cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
```

### Baselines

A baseline only means something on the machine that made it. Save one before a change and compare
after it:

```sh
OXIDEMERGE_BENCH_SAVE=/tmp/base.txt \
  cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
# ... change the code ...
OXIDEMERGE_BENCH_BASELINE=/tmp/base.txt \
  cargo test --release --bin oxidemerge bench -- --ignored --test-threads 1
```

To compare two commits fairly, build both as release test executables first (for example from a
`git worktree`), then run them one after the other, or alternately, on the same inputs.

In CI, start the workflow by hand with *Run benchmarks* checked to run them on Linux, Windows and
macOS; the results are kept as artifacts. Hosted runners vary too much for fixed limits, so CI
reports the numbers without failing on them.

### Remote folders

A folder on a server is timed against a real server, which the suite does not have. Name the folder,
the local folder to compare it with and the password:

```sh
OXIDEMERGE_BENCH_REMOTE=sftp://user@host:2222/path/to/folder \
OXIDEMERGE_BENCH_LOCAL=/path/to/its/local/copy \
OXIDEMERGE_REMOTE_PASSWORD=... \
cargo test --release --bin oxidemerge bench_remote_folder_compare -- --ignored --nocapture
```

It reports the median of three runs (`OXIDEMERGE_BENCH_RUNS`) of each step, signing in anew for each
run and then taking the connections the run before left open. The FTP benchmark uses the suite's own
server; `OXIDEMERGE_BENCH_DELAY_MS` sets the delay per command (default 2):

```sh
cargo test --release --bin oxidemerge bench_ftp_folder_fetch -- --ignored --nocapture
```

### Workloads

- **text-10mb**, **text-100mb**, **text-500mb**, **text-1gb**: two UTF-8 text files of source-like
  lines, with one line in 20 (10 MB) or 200 changed, inserted or deleted.
- **text-repeated**: 300,000 equal lines with 300 inserted, which defeats matching lines by their
  content.
- **merge3-10mb**: three 10 MB files compared as a 3-way compare, then merged automatically.
- **text-view-10mb**, **-100mb**, **-500mb**, **-1gb**: the files of the text workloads opened in
  the real text view, off screen. It reports, each with the process's resident memory: loading, the
  comparison with everything the view builds from it, the first frame, a frame after jumping to
  another line (100 of them), and a step to the next difference with its frame (100 steps). Last, it
  breaks down what the view holds by kind: lines, line ends, edit marks, highlighting, rows, layout,
  word-level differences and undo history, counted from their sizes and capacities. Its peak memory
  is what a window showing the comparison needs, without the window itself.
- **text-cancel-100mb**, **text-cancel-500mb**: the same files opened as the window opens large
  ones, on another thread, and stopped as *Cancel* does at nine points. It reports reading the files
  again, the whole open, and the median and worst time until the work stopped and until its memory
  was freed. Its time and peak are those of ten opens in one process, so they are left out.
- **table-10k**, **table-100k**: two CSV tables of 10,000 or 100,000 records keyed by `id`, the
  right one shuffled, with 1% of the amounts changed, 0.5% of the records removed and as many added,
  matched by key. **table-10k-position** and **table-100k-position** compare the same tables row by
  row, as a table without a key is compared.
- **table-cancel-1m**: two such tables of 1,000,000 records matched on another thread and stopped
  half way, as *Cancel* does; the result says how long stopping took, freeing what was built
  included.
- **xml-tree-12k**: two XML catalogs of 12,000 `<item>` elements (about 84,000 parser nodes), 1% of
  the items changed and 0.5% inserted and removed, aligned as the XML Tree view does.
  **xml-tree-12k-keyed** shuffles the right catalog and pairs items by the key rule `item@id`.
  **xml-semantic-12k** compares the same files with the Semantic view's path-by-path engine.
- **xml-tree-view-12k**: the same comparison put through the real Tree view, off screen: the first
  rows, a frame while scrolling, unfolding the whole tree and folding the unchanged rows, and a step
  from one change to the next with its frame.
- **binary-10mb**, **binary-100mb**, **binary-1gb**: two random files with 100 bytes changed.
- **binary-moved-*:** the moved-block search of the binary view. *unrelated*: three unrelated 5 MiB
  panes, no expected matches; *swap*: two 4 MiB files with two 64 KiB blocks swapped; *large*: two
  64 MiB files with two 1 MiB blocks swapped; *short-shift*: a 15-byte shifted run between edits;
  *wide-shift*: a 96-byte insertion that shifts a 512 KiB repeated tail; *firmware*: mixed-edit
  firmware samples with 18 expected matches.
- **image-4k**: two 3840 × 2160 PNG images with 1% of the pixels changed, decoded and compared.
- **folder-small-files**: 20,000 files of 12 short lines up to 8 folders deep, 1% changed and 1% on
  one side only. *First result* is the time until the first item reaches the list.
- **folder-large-files**: 20 files of 8 MB, two changed in their last byte.
- **folder-mixed**: 2,000 files of 100 bytes to 2 MB, some 60 folders deep.
- **folder-navigation-20k**: the folders of folder-small-files compared, then stepped through from
  the first file with differences to the last, as *Next Difference* does.
- **sync-preview-20k**: the same folders compared, then a mirror plan made from the result. The plan
  reads every file it would touch, and every file whose size or time differs, to fingerprint it;
  *plan* is the time of that step alone.
- **sync-apply-large**: three random 2 GiB files mirrored into a folder that has one of them equal,
  one changed in its last byte and one missing. *Preview* is the scan and plan; *apply*
  fingerprints, copies through a verified temporary file, keeps the replaced file in the journal and
  checks the result.
