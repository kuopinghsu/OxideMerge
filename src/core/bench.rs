//! Benchmarks of the compare engines, run by hand on an otherwise idle machine:
//!
//! ```sh
//! cargo test --release --bin oxidemerge bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each workload runs three times, each time in a test process of its own, so that its peak
//! memory can be measured (with `/usr/bin/time` where there is one). Inputs are generated once
//! into `target/bench-data`; results are printed and written to `target/bench/results.md`.
//!
//! - `OXIDEMERGE_BENCH_LARGE=1` adds 100 MB inputs, `=2` also a 1 GB binary compare and a
//!   synchronization of 2 GB files, `=3` a 1 GB text compare.
//! - `OXIDEMERGE_BENCH_ONLY=text,folder` runs the workloads whose names contain one of these.
//! - `OXIDEMERGE_BENCH_RUNS=<n>` sets the number of runs (the median time is reported).
//! - `OXIDEMERGE_BENCH_SAVE=<file>` saves the results as a baseline (after the comparison
//!   below has passed, so it can name the same file), and
//!   `OXIDEMERGE_BENCH_BASELINE=<file>` fails when a workload that took at least 50 ms in the
//!   baseline is now slower by more than `OXIDEMERGE_BENCH_TOLERANCE` (default 1.5) times.
//!   Baselines only compare runs on the same machine.

use super::diff::{self, DiffOptions};
use super::filter::FileFilter;
use super::folder::{FolderOptions, Progress, Scanner};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Workload {
    name: &'static str,
    what: &'static str,
    /// The `OXIDEMERGE_BENCH_LARGE` level it needs.
    level: u8,
    make: fn(&Path),
    run: fn(&Path) -> String,
}

const MB: usize = 1 << 20;

const WORKLOADS: &[Workload] = &[
    Workload {
        name: "text-10mb",
        what: "2 text files of 10 MB, 1 line in 20 changed",
        level: 0,
        make: |d| text_pair(d, 10 * MB, 20),
        run: |d| diff_files(d, &["a.txt", "b.txt"]),
    },
    Workload {
        name: "text-repeated",
        what: "2 text files of 300,000 equal lines with 300 inserted",
        level: 0,
        make: repeated_pair,
        run: |d| diff_files(d, &["a.txt", "b.txt"]),
    },
    Workload {
        name: "text-100mb",
        what: "2 text files of 100 MB, 1 line in 200 changed",
        level: 1,
        make: |d| text_pair(d, 100 * MB, 200),
        run: |d| diff_files(d, &["a.txt", "b.txt"]),
    },
    Workload {
        name: "text-1gb",
        what: "2 text files of 1 GB, 1 line in 200 changed",
        level: 3,
        make: |d| text_pair(d, 1024 * MB, 200),
        run: |d| diff_files(d, &["a.txt", "b.txt"]),
    },
    Workload {
        name: "table-10k",
        what: "2 CSV tables of 10,000 records in another order, matched by key, 1% changed",
        level: 0,
        make: |d| table_pair(d, 10_000),
        run: match_tables,
    },
    Workload {
        name: "table-10k-position",
        what: "the tables of table-10k compared row by row, as without a key",
        level: 0,
        make: |d| table_pair(d, 10_000),
        run: table_by_position,
    },
    Workload {
        name: "table-100k",
        what: "2 CSV tables of 100,000 records in another order, matched by key, 1% changed",
        level: 0,
        make: |d| table_pair(d, 100_000),
        run: match_tables,
    },
    Workload {
        name: "table-100k-position",
        what: "the tables of table-100k compared row by row, as without a key",
        level: 0,
        make: |d| table_pair(d, 100_000),
        run: table_by_position,
    },
    Workload {
        name: "table-cancel-1m",
        what: "2 CSV tables of 1,000,000 records matched by key, stopped half way",
        level: 0,
        make: |d| table_pair(d, 1_000_000),
        run: cancel_table_match,
    },
    Workload {
        name: "xml-tree-12k",
        what: "2 XML files of 12,000 elements, 1% changed, 0.5% inserted and removed, aligned as the Tree view does",
        level: 0,
        make: |d| xml_pair(d, false),
        run: |d| xml_tree(d, ""),
    },
    Workload {
        name: "xml-tree-12k-keyed",
        what: "the elements of xml-tree-12k in another order, paired by the key rule item@id",
        level: 0,
        make: |d| xml_pair(d, true),
        run: |d| xml_tree(d, "item@id"),
    },
    Workload {
        name: "xml-tree-view-12k",
        what: "the comparison of xml-tree-12k drawn in the Tree view: first rows, scrolling, folding and stepping",
        level: 0,
        make: |d| xml_pair(d, false),
        run: |d| crate::ui::xml_tree_view::render_bench(d),
    },
    Workload {
        name: "xml-semantic-12k",
        what: "the files of xml-tree-12k compared path by path, as the Semantic view does",
        level: 0,
        make: |d| xml_pair(d, false),
        run: xml_semantic,
    },
    Workload {
        name: "merge3-10mb",
        what: "3 text files of 10 MB, compared and merged",
        level: 0,
        make: merge_triple,
        run: merge_files,
    },
    Workload {
        name: "binary-10mb",
        what: "2 binary files of 10 MB, 100 bytes changed",
        level: 0,
        make: |d| binary_pair(d, 10 * MB),
        run: diff_binary,
    },
    Workload {
        name: "binary-100mb",
        what: "2 binary files of 100 MB, 100 bytes changed",
        level: 1,
        make: |d| binary_pair(d, 100 * MB),
        run: diff_binary,
    },
    Workload {
        name: "binary-1gb",
        what: "2 binary files of 1 GB, 100 bytes changed",
        level: 2,
        make: |d| binary_pair(d, 1024 * MB),
        run: diff_binary,
    },
    Workload {
        name: "image-4k",
        what: "2 PNG images of 3840 x 2160, 1% of the pixels changed",
        level: 0,
        make: image_pair,
        run: diff_images,
    },
    Workload {
        name: "folder-small-files",
        what: "2 folders of 20,000 files of 1 KB, 8 levels deep, 1% changed",
        level: 0,
        make: small_files,
        run: compare_folders,
    },
    Workload {
        name: "sync-preview-20k",
        what: "a mirror plan for 2 folders of 20,000 files of 1 KB, 1% changed: scan, then plan",
        level: 0,
        make: small_files,
        run: preview_sync,
    },
    Workload {
        name: "sync-apply-large",
        what: "a mirror of 3 files of 2 GiB, one new and one changed in its last byte: plan, then apply",
        level: 2,
        make: huge_files,
        run: apply_sync,
    },
    Workload {
        name: "folder-navigation-20k",
        what: "the folders of folder-small-files compared, then stepped through file by file",
        level: 0,
        make: small_files,
        run: |d| crate::ui::folder_view::navigation_bench(&[d.join("left"), d.join("right")]),
    },
    Workload {
        name: "folder-large-files",
        what: "2 folders of 20 files of 8 MB, 2 changed at the end",
        level: 0,
        make: large_files,
        run: compare_folders,
    },
    Workload {
        name: "folder-mixed",
        what: "2 folders of 2,000 files of 100 bytes to 2 MB, 60 levels deep in places",
        level: 0,
        make: mixed_files,
        run: compare_folders,
    },
];

// ---------- inputs ----------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "let", "value", "return", "if", "else", "for", "match", "self", "config", "result", "error",
    "index", "count", "name", "path", "line", "=", "+", "(", ")", "{", "}", ";", "0", "1", "42",
];

fn source_line(rng: &mut Rng) -> String {
    let indent = "    ".repeat(rng.below(4));
    let words: Vec<&str> = (0..3 + rng.below(9))
        .map(|_| WORDS[rng.below(WORDS.len())])
        .collect();
    format!("{indent}{}\n", words.join(" "))
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

/// Two files of about `size` bytes; one line in `every` is changed, inserted or deleted.
fn text_pair(dir: &Path, size: usize, every: usize) {
    let mut rng = Rng(7);
    use std::io::Write;
    let mut a = std::io::BufWriter::new(std::fs::File::create(dir.join("a.txt")).unwrap());
    let mut b = std::io::BufWriter::new(std::fs::File::create(dir.join("b.txt")).unwrap());
    let mut written = 0;
    while written < size {
        let line = source_line(&mut rng);
        a.write_all(line.as_bytes()).unwrap();
        written += line.len();
        match (rng.below(every), rng.below(3)) {
            (0, 0) => b.write_all(source_line(&mut rng).as_bytes()).unwrap(),
            (0, 1) => {
                b.write_all(line.as_bytes()).unwrap();
                b.write_all(source_line(&mut rng).as_bytes()).unwrap();
            }
            (0, _) => {}
            _ => b.write_all(line.as_bytes()).unwrap(),
        }
    }
    a.flush().unwrap();
    b.flush().unwrap();
}

fn repeated_pair(dir: &Path) {
    let mut rng = Rng(11);
    let mut a = String::new();
    let mut b = String::new();
    for i in 0..300_000 {
        a.push_str("}\n");
        b.push_str("}\n");
        if i % 1000 == 0 {
            b.push_str(&source_line(&mut rng));
        }
    }
    write(&dir.join("a.txt"), a.as_bytes());
    write(&dir.join("b.txt"), b.as_bytes());
}

fn merge_triple(dir: &Path) {
    let mut rng = Rng(13);
    let (mut l, mut base, mut r) = (String::new(), String::new(), String::new());
    while base.len() < 10 * MB {
        let line = source_line(&mut rng);
        base.push_str(&line);
        let changed = rng.below(40);
        l.push_str(&if changed == 0 {
            source_line(&mut rng)
        } else {
            line.clone()
        });
        r.push_str(&if changed == 1 {
            source_line(&mut rng)
        } else {
            line
        });
    }
    for (name, text) in [("left.txt", l), ("base.txt", base), ("right.txt", r)] {
        write(&dir.join(name), text.as_bytes());
    }
}

/// Two tables of `n` records keyed by `id`: the right one shuffled, with 1% of the amounts
/// changed, 0.5% of the records removed and as many added.
fn table_pair(dir: &Path, n: usize) {
    let mut rng = Rng(37);
    let row = |id: usize, rng: &mut Rng| {
        format!(
            "{id},{},{}.{:02}\n",
            WORDS[rng.below(WORDS.len())],
            rng.below(10_000),
            rng.below(100)
        )
    };
    let records: Vec<String> = (0..n).map(|id| row(id, &mut rng)).collect();
    let mut right: Vec<String> = Vec::with_capacity(records.len());
    for (id, record) in records.iter().enumerate() {
        match rng.below(200) {
            0 => {}
            1 | 2 => right.push(row(id, &mut rng)),
            _ => right.push(record.clone()),
        }
    }
    right.extend((0..n / 200).map(|i| row(n + i, &mut rng)));
    for i in (1..right.len()).rev() {
        right.swap(i, rng.below(i + 1));
    }
    let header = "id,name,amount\n";
    write(
        &dir.join("a.csv"),
        (header.to_string() + &records.concat()).as_bytes(),
    );
    write(
        &dir.join("b.csv"),
        (header.to_string() + &right.concat()).as_bytes(),
    );
}

/// Two catalogs of 12,000 items, near the semantic parser's 100,000-node limit, written
/// `a.xml`/`b.xml` in order or `a.xml`/`c.xml` with the right one shuffled.
fn xml_pair(dir: &Path, shuffled: bool) {
    let mut rng = Rng(53);
    let n = 12_000;
    let item = |id: usize, rng: &mut Rng| {
        format!(
            "  <item id=\"{id}\" kind=\"{}\"><name>{}</name></item>\n",
            WORDS[rng.below(WORDS.len())],
            rng.below(1_000_000)
        )
    };
    let items: Vec<String> = (0..n).map(|id| item(id, &mut rng)).collect();
    let mut right = Vec::with_capacity(n);
    for (id, it) in items.iter().enumerate() {
        match rng.below(200) {
            0 => {}
            1 | 2 => right.push(item(id, &mut rng)),
            3 => {
                right.push(item(n + id, &mut rng));
                right.push(it.clone());
            }
            _ => right.push(it.clone()),
        }
    }
    if shuffled {
        for i in (1..right.len()).rev() {
            right.swap(i, rng.below(i + 1));
        }
    }
    let doc = |items: &[String]| format!("<catalog>\n{}</catalog>\n", items.concat());
    write(&dir.join("a.xml"), doc(&items).as_bytes());
    write(
        &dir.join(if shuffled { "c.xml" } else { "b.xml" }),
        doc(&right).as_bytes(),
    );
}

fn xml_tree(dir: &Path, keys: &str) -> String {
    use super::semantic::xml_tree::{TreeDiff, TreeOptions};
    let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
    let other = if keys.is_empty() { "b.xml" } else { "c.xml" };
    let options = TreeOptions::parse(keys).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let d = TreeDiff::compare(&read("a.xml"), &read(other), &options, &cancel).unwrap();
    let c = d.counts;
    format!(
        "{} rows; {} added, {} removed, {} modified, {} moved, {} ambiguous",
        d.rows.len(),
        c.added,
        c.removed,
        c.modified,
        c.moved,
        c.ambiguous
    )
}

fn xml_semantic(dir: &Path) -> String {
    let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
    let r = super::semantic::compare(&read("a.xml"), &read("b.xml"), super::semantic::Format::Xml);
    format!("{} changes, {} errors", r.changes.len(), r.errors.len())
}

fn binary_pair(dir: &Path, size: usize) {
    let mut rng = Rng(17);
    let mut a = vec![0u8; size];
    for chunk in a.chunks_mut(8) {
        let n = rng.next().to_le_bytes();
        chunk.copy_from_slice(&n[..chunk.len()]);
    }
    write(&dir.join("a.bin"), &a);
    for _ in 0..100 {
        let at = rng.below(size);
        a[at] = !a[at];
    }
    write(&dir.join("b.bin"), &a);
}

fn image_pair(dir: &Path) {
    let (w, h) = (3840, 2160);
    let mut a = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x / 15) as u8, (y / 9) as u8, ((x + y) / 23) as u8, 255])
    });
    a.save(dir.join("a.png")).unwrap();
    let mut rng = Rng(19);
    for _ in 0..(w * h / 100) {
        let (x, y) = (rng.below(w as usize) as u32, rng.below(h as usize) as u32);
        a.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
    }
    a.save(dir.join("b.png")).unwrap();
}

fn small_files(dir: &Path) {
    let mut rng = Rng(23);
    for i in 0..20_000 {
        let depth = 1 + i % 8;
        let rel: PathBuf = (0..depth)
            .map(|d| format!("d{}", (i / (d + 1)) % 10))
            .collect();
        let rel = rel.join(format!("f{i}.txt"));
        let text: String = (0..12).map(|_| source_line(&mut rng)).collect();
        match rng.below(100) {
            0 => write(&dir.join("left").join(&rel), text.as_bytes()),
            1 => {
                write(&dir.join("left").join(&rel), text.as_bytes());
                write(
                    &dir.join("right").join(&rel),
                    format!("{text}x\n").as_bytes(),
                );
            }
            _ => {
                write(&dir.join("left").join(&rel), text.as_bytes());
                write(&dir.join("right").join(&rel), text.as_bytes());
            }
        }
    }
}

fn large_files(dir: &Path) {
    let mut rng = Rng(29);
    for i in 0..20 {
        let mut data = vec![0u8; 8 * MB];
        for chunk in data.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next().to_le_bytes());
        }
        write(&dir.join("left").join(format!("f{i}.bin")), &data);
        if i % 10 == 0 {
            *data.last_mut().unwrap() ^= 1;
        }
        write(&dir.join("right").join(format!("f{i}.bin")), &data);
    }
}

/// Three random 2 GiB files on the left; on the right one is equal, one differs in its last
/// byte and one is missing: 10 GiB in all.
fn huge_files(dir: &Path) {
    use std::io::Write as _;
    let mut rng = Rng(37);
    for i in 0..3 {
        let path = |side: &str| dir.join(side).join(format!("h{i}.bin"));
        std::fs::create_dir_all(dir.join("left")).unwrap();
        std::fs::create_dir_all(dir.join("right")).unwrap();
        let mut left = std::io::BufWriter::new(std::fs::File::create(path("left")).unwrap());
        let mut right =
            (i < 2).then(|| std::io::BufWriter::new(std::fs::File::create(path("right")).unwrap()));
        let mut chunk = vec![0u8; MB];
        for n in 0..2048 {
            for word in chunk.chunks_mut(8) {
                word.copy_from_slice(&rng.next().to_le_bytes());
            }
            left.write_all(&chunk).unwrap();
            if let Some(right) = &mut right {
                if i == 1 && n == 2047 {
                    *chunk.last_mut().unwrap() ^= 1;
                }
                right.write_all(&chunk).unwrap();
            }
        }
    }
}

fn mixed_files(dir: &Path) {
    let mut rng = Rng(31);
    for i in 0..2000 {
        let depth = if i % 50 == 0 { 60 } else { i % 5 };
        let rel: PathBuf = (0..depth).map(|d| format!("n{d}")).collect();
        let rel = rel.join(format!("f{i}.dat"));
        let size = if i % 40 == 0 {
            2 * MB
        } else {
            [100, 4096, 65_536][rng.below(3)]
        };
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        write(&dir.join("left").join(&rel), &data);
        let mut right = data;
        if i % 25 == 0 {
            right[0] ^= 1;
        }
        write(&dir.join("right").join(&rel), &right);
    }
}

// ---------- measured work ----------

/// Optional local profiling, kept off during ordinary benchmarks.
pub fn profile(phase: &str, detail: impl FnOnce() -> String) {
    let Ok(path) = std::env::var("OXIDEMERGE_BENCH_PROFILE") else {
        return;
    };
    use std::io::Write;
    let rss = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .map(|v| String::from_utf8_lossy(&v.stdout).trim().to_string())
        .unwrap_or_default();
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(log, "{phase}: rss_kib={rss}; {}", detail());
    }
}

fn diff_files(dir: &Path, names: &[&str]) -> String {
    let docs: Vec<_> = names
        .iter()
        .map(|n| super::text::load_doc(&dir.join(n), None).unwrap())
        .collect();
    profile("loaded", || {
        format!(
            "{} lines; {} line bytes; {} line capacity; {} vector bytes",
            docs.iter().map(|d| d.lines.len()).sum::<usize>(),
            docs.iter()
                .flat_map(|d| &d.lines)
                .map(String::len)
                .sum::<usize>(),
            docs.iter()
                .flat_map(|d| &d.lines)
                .map(String::capacity)
                .sum::<usize>(),
            docs.iter()
                .map(|d| d.lines.capacity() * std::mem::size_of::<String>())
                .sum::<usize>()
        )
    });
    let panes: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
    let r = diff::compute(&panes, &DiffOptions::default());
    profile("aligned", || {
        format!(
            "rows={} capacity={} size={}",
            r.rows.len(),
            r.rows.capacity(),
            std::mem::size_of::<diff::Row>()
        )
    });
    format!("{} differences", r.blocks.len())
}

fn merge_files(dir: &Path) -> String {
    let found = diff_files(dir, &["left.txt", "base.txt", "right.txt"]);
    let load = |n| super::text::load_doc(&dir.join(n), None).unwrap().lines;
    let merged = diff::merge3(&load("left.txt"), &load("base.txt"), &load("right.txt"));
    let merged = if merged.is_some() {
        "merged"
    } else {
        "conflicts left"
    };
    format!("{found}, {merged}")
}

fn load_tables(dir: &Path) -> (super::table::Table, super::table::Table) {
    let load = |n| {
        let doc = super::text::load_doc(&dir.join(n), None).unwrap();
        super::table::Table::parse(super::table::source(&doc), ',').unwrap()
    };
    (load("a.csv"), load("b.csv"))
}

fn match_tables(dir: &Path) -> String {
    use super::table_match::{self, Compare, KeyColumn, TableMatch};
    let (a, b) = load_tables(dir);
    let opts = TableMatch {
        header: true,
        keys: vec![KeyColumn::Name("id".into())],
        ..Default::default()
    };
    let key = |s: &str| s.to_string();
    let eq = |x: &str, y: &str| x == y;
    let d = table_match::compare(&a, &b, &opts, &Compare { key: &key, eq: &eq }).unwrap();
    let n = d.counts;
    format!(
        "{} modified, {} added, {} removed, {} reordered",
        n.modified, n.added, n.removed, n.moved
    )
}

fn table_by_position(dir: &Path) -> String {
    let (a, b) = load_tables(dir);
    let opts = DiffOptions::default();
    let keys = [a.keys(opts.ignore_eol), b.keys(opts.ignore_eol)];
    let r = diff::compute_with(&[&keys[0][..], &keys[1][..]], &opts, None);
    format!("{} differences", r.blocks.len())
}

/// Matches by key on another thread and stops it once half the records are keyed, as the
/// Cancel button does; reports how long stopping took.
fn cancel_table_match(dir: &Path) -> String {
    use super::table_match::{self, Compare, KeyColumn, Problem, TableMatch};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (a, b) = load_tables(dir);
    let total = a.records.len() + b.records.len();
    let opts = TableMatch {
        header: true,
        keys: vec![KeyColumn::Name("id".into())],
        ..Default::default()
    };
    let (cancel, done) = (AtomicBool::new(false), AtomicUsize::new(0));
    let key = |s: &str| s.to_string();
    let eq = |x: &str, y: &str| x == y;
    let (result, asked, stopped) = std::thread::scope(|s| {
        let job = s.spawn(|| {
            let stop = |n: usize| {
                done.store(n, Ordering::Relaxed);
                cancel.load(Ordering::Relaxed)
            };
            let r = table_match::compare_with_stop(
                &a,
                &b,
                &opts,
                &Compare { key: &key, eq: &eq },
                &stop,
            );
            (r, Instant::now())
        });
        while done.load(Ordering::Relaxed) < total / 2 && !job.is_finished() {
            std::thread::sleep(Duration::from_micros(200));
        }
        let asked = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let (r, stopped) = job.join().unwrap();
        (r, asked, stopped)
    });
    assert!(matches!(result, Err(Problem::Cancelled)), "not stopped");
    format!(
        "stopped {:.1} ms after asked, at {} of {total} records",
        stopped.duration_since(asked).as_secs_f64() * 1000.0,
        done.load(Ordering::Relaxed),
    )
}

fn diff_binary(dir: &Path) -> String {
    let a = std::fs::read(dir.join("a.bin")).unwrap();
    let b = std::fs::read(dir.join("b.bin")).unwrap();
    format!("{} ranges", super::binary::diff_ranges(&[&a, &b]).len())
}

fn diff_images(dir: &Path) -> String {
    let load = |n| image::open(dir.join(n)).unwrap().to_rgba8();
    let (a, b) = (load("a.png"), load("b.png"));
    let d = super::image_cmp::compare(&[&a, &b], 0, 8);
    format!("{} pixels differ", d.diff_pixels)
}

fn preview_sync(dir: &Path) -> String {
    use super::folder_sync::{self, Scope, Side, SyncMode, SyncPolicy};
    let roots = [dir.join("left"), dir.join("right")];
    let items = Scanner {
        roots: &roots,
        filter: &FileFilter::parse_mask("*.*"),
        fopts: &FolderOptions::default(),
        dopts: &DiffOptions::default(),
        progress: &Progress::default(),
    }
    .run();
    let started = Instant::now();
    let policy = SyncPolicy {
        mode: SyncMode::Mirror,
        source: Side::Left,
    };
    let scope = Scope {
        complete: true,
        ..Scope::default()
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let plan = folder_sync::plan(&items, &roots, policy, scope, &stop).unwrap();
    format!(
        "{} copies, {} replaces, plan {} ms",
        plan.estimate.copies,
        plan.estimate.replaces,
        started.elapsed().as_millis()
    )
}

/// Plans and applies a mirror into a fresh copy of the right folder, so every run starts the same.
fn apply_sync(dir: &Path) -> String {
    use super::folder_sync::{self, Scope, Side, SyncMode, SyncOutcome, SyncPolicy};
    let work = dir.join("work");
    let journal = dir.join("journal");
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&journal);
    std::fs::create_dir_all(&work).unwrap();
    for entry in std::fs::read_dir(dir.join("right")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), work.join(entry.file_name())).unwrap();
    }
    let roots = [dir.join("left"), work.clone()];
    let started = Instant::now();
    let items = Scanner {
        roots: &roots,
        filter: &FileFilter::parse_mask("*.*"),
        fopts: &FolderOptions::default(),
        dopts: &DiffOptions::default(),
        progress: &Progress::default(),
    }
    .run();
    let policy = SyncPolicy {
        mode: SyncMode::Mirror,
        source: Side::Left,
    };
    let scope = Scope {
        complete: true,
        ..Scope::default()
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let plan = folder_sync::plan(&items, &roots, policy, scope, &stop).unwrap();
    let previewed = started.elapsed();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let outcome = folder_sync::apply(&plan, &roots, &journal, &stop, &done);
    let applied = started.elapsed() - previewed;
    let result = match outcome {
        SyncOutcome::Applied { changes, .. } => format!("{changes} changes applied"),
        other => panic!("not applied: {:?}", other.problem()),
    };
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&journal);
    format!(
        "{result}; preview {} ms, apply {} ms",
        previewed.as_millis(),
        applied.as_millis()
    )
}

fn compare_folders(dir: &Path) -> String {
    let roots = [dir.join("left"), dir.join("right")];
    let progress = Progress::live();
    let started = Instant::now();
    let items = std::thread::scope(|s| {
        let first = s.spawn(|| loop {
            let u = progress.take_updates();
            if !u.items.is_empty() || !u.results.is_empty() {
                return Some(started.elapsed());
            }
            if progress.abort.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        });
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::parse_mask("*.*"),
            fopts: &FolderOptions::default(),
            dopts: &DiffOptions::default(),
            progress: &progress,
        }
        .run();
        progress
            .abort
            .store(true, std::sync::atomic::Ordering::Relaxed);
        (items, first.join().unwrap())
    });
    let (items, first) = items;
    let (_, diff, unique, err) = super::folder::status_counts(&items);
    let first = first.map_or("-".into(), |t| format!("{} ms", t.as_millis()));
    format!("{diff} differ, {unique} unique, {err} errors; first result after {first}")
}

// ---------- driver ----------

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn data_dir(w: &Workload) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/bench-data")
        .join(w.name)
}

/// Runs the workload named by `OXIDEMERGE_BENCH_RUN`; started by `benchmarks`.
#[test]
#[ignore]
fn workload() {
    let Some(name) = env("OXIDEMERGE_BENCH_RUN") else {
        return;
    };
    let w = WORKLOADS.iter().find(|w| w.name == name).unwrap();
    let started = Instant::now();
    let detail = (w.run)(&data_dir(w));
    println!("BENCH {:.4} {detail}", started.elapsed().as_secs_f64());
}

struct Measured {
    name: &'static str,
    what: &'static str,
    seconds: f64,
    peak_mb: Option<f64>,
    detail: String,
}

/// Peak resident memory in MB from the output of `/usr/bin/time -l` (macOS) or `-v` (GNU).
fn peak_mb(stderr: &str) -> Option<f64> {
    stderr.lines().find_map(|l| {
        let l = l.trim();
        if let Some(n) = l.strip_suffix("maximum resident set size") {
            return n.trim().parse::<f64>().ok().map(|b| b / MB as f64);
        }
        let n = l.strip_prefix("Maximum resident set size (kbytes):")?;
        n.trim().parse::<f64>().ok().map(|kb| kb / 1024.0)
    })
}

fn measure(w: &Workload, runs: usize) -> Measured {
    let exe = std::env::current_exe().unwrap();
    let args = [
        "core::bench::workload",
        "--exact",
        "--ignored",
        "--nocapture",
    ];
    let time = Path::new("/usr/bin/time");
    let flag = if cfg!(target_os = "macos") {
        "-l"
    } else {
        "-v"
    };
    let mut times = Vec::new();
    let (mut peak, mut detail) = (None::<f64>, String::new());
    for _ in 0..runs {
        let mut cmd = if time.is_file() {
            let mut c = Command::new(time);
            c.arg(flag).arg(&exe);
            c
        } else {
            Command::new(&exe)
        };
        let out = cmd
            .args(args)
            .env("OXIDEMERGE_BENCH_RUN", w.name)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stdout
            .lines()
            .find_map(|l| Some(l.split_once("BENCH ")?.1))
            .unwrap_or_else(|| panic!("{}: no result\n{stdout}\n{stderr}", w.name));
        let (secs, rest) = line.split_once(' ').unwrap();
        times.push(secs.parse::<f64>().unwrap());
        detail = rest.to_string();
        if let Some(mb) = peak_mb(&stderr) {
            peak = Some(peak.map_or(mb, |p| p.max(mb)));
        }
    }
    times.sort_by(f64::total_cmp);
    Measured {
        name: w.name,
        what: w.what,
        seconds: times[times.len() / 2],
        peak_mb: peak,
        detail,
    }
}

fn command_line(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn machine() -> String {
    let cpu = command_line("sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo")
                .ok()?
                .lines()
                .find_map(|l| {
                    Some(
                        l.strip_prefix("model name")?
                            .split(':')
                            .nth(1)?
                            .trim()
                            .to_string(),
                    )
                })
        })
        .or_else(|| env("PROCESSOR_IDENTIFIER"))
        .unwrap_or_else(|| "unknown CPU".into());
    let memory = command_line("sysctl", &["-n", "hw.memsize"])
        .and_then(|b| b.parse::<u64>().ok())
        .or_else(|| {
            std::fs::read_to_string("/proc/meminfo")
                .ok()?
                .lines()
                .find_map(|l| {
                    let kb = l.strip_prefix("MemTotal:")?.trim().strip_suffix("kB")?;
                    kb.trim().parse::<u64>().ok().map(|kb| kb * 1024)
                })
        })
        .map_or("unknown".into(), |b| format!("{} GB", b >> 30));
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    let rev = command_line("git", &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let dirty = command_line("git", &["status", "--porcelain", "--untracked-files=no"])
        .map_or("", |_| " with uncommitted changes");
    format!(
        "{} {}, {cpu}, {threads} threads, {memory} memory; OxideMerge {} ({rev}{dirty}), {} build",
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    )
}

#[test]
#[ignore]
fn benchmarks() {
    if env("OXIDEMERGE_BENCH_RUN").is_some() {
        return;
    }
    let level: u8 = env("OXIDEMERGE_BENCH_LARGE").map_or(0, |l| l.parse().unwrap());
    let runs: usize = env("OXIDEMERGE_BENCH_RUNS").map_or(3, |r| r.parse().unwrap());
    let only = env("OXIDEMERGE_BENCH_ONLY");
    let picked = WORKLOADS.iter().filter(|w| {
        w.level <= level
            && only
                .as_deref()
                .is_none_or(|o| o.split(',').any(|p| w.name.contains(p.trim())))
    });
    if cfg!(debug_assertions) {
        eprintln!("note: a debug build; run with --release for meaningful numbers");
    }
    let mut results = Vec::new();
    for w in picked {
        let dir = data_dir(w);
        let done = dir.join(".complete");
        if !done.is_file() {
            eprintln!("bench: making the {} inputs", w.name);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            (w.make)(&dir);
            std::fs::write(&done, "").unwrap();
        }
        let m = measure(w, runs);
        eprintln!("bench: {} {:.3} s, {}", m.name, m.seconds, m.detail);
        results.push(m);
    }

    let mut md = format!("Measured on {}.\n\n", machine());
    md.push_str("| Workload | Input | Time (median) | Peak memory | Result |\n");
    md.push_str("| --- | --- | ---: | ---: | --- |\n");
    for m in &results {
        let peak = m.peak_mb.map_or("-".into(), |mb| format!("{mb:.0} MB"));
        let _ = writeln!(
            md,
            "| {} | {} | {:.3} s | {peak} | {} |",
            m.name, m.what, m.seconds, m.detail
        );
    }
    println!("\n{md}");
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench/results.md");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, &md).unwrap();

    if let Some(path) = env("OXIDEMERGE_BENCH_BASELINE") {
        let tolerance: f64 = env("OXIDEMERGE_BENCH_TOLERANCE").map_or(1.5, |t| t.parse().unwrap());
        let baseline = std::fs::read_to_string(&path).unwrap();
        let slower: Vec<String> = baseline
            .lines()
            .filter_map(|l| {
                let (name, secs) = l.split_once(' ')?;
                let before: f64 = secs.trim().parse().ok()?;
                let now = results.iter().find(|m| m.name == name)?.seconds;
                (before >= 0.05 && now > before * tolerance)
                    .then(|| format!("{name}: {before:.3} s -> {now:.3} s"))
            })
            .collect();
        assert!(
            slower.is_empty(),
            "slower than {path} by more than {tolerance}x:\n{}",
            slower.join("\n")
        );
    }
    if let Some(path) = env("OXIDEMERGE_BENCH_SAVE") {
        let lines: String = results
            .iter()
            .map(|m| format!("{} {:.4}\n", m.name, m.seconds))
            .collect();
        std::fs::write(path, lines).unwrap();
    }
}
