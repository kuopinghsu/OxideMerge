//! Regression tests of diffs, 3-way merges and patches together: randomized cases checked
//! against properties every result must have, and fixed cases with known outputs.
//!
//! Each randomized case prints its seed when it fails; `OXIDEMERGE_TEST_SEED=<seed>` runs
//! that case alone; `OXIDEMERGE_TEST_CASES=<n>` runs `n` cases of each instead of the default.

use super::diff::{
    compute, compute_with, inline_diff, merge3, BlockKind, DiffAlgorithm, DiffOptions, DiffResult,
    WhitespaceMode,
};
use super::patch::{apply, make_patch, parse_unified, PatchInput, PatchStyle};
use super::text;

/// xorshift64*: small, fast and the same on every platform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The seeds of the randomized cases: `count` of them (`OXIDEMERGE_TEST_CASES` for a longer
/// run), or the one to replay.
fn seeds(count: u64) -> Vec<u64> {
    let var = |name: &str| {
        std::env::var(name).ok().map(|s| {
            s.parse::<u64>()
                .unwrap_or_else(|_| panic!("{name} is a number"))
        })
    };
    match var("OXIDEMERGE_TEST_SEED") {
        Some(seed) => vec![seed],
        None => (1..=var("OXIDEMERGE_TEST_CASES").unwrap_or(count)).collect(),
    }
}

/// Lines that look like parts of a patch, repeat, are blank or are not ASCII, to catch a
/// patch that confuses its own syntax with the text.
const LINES: &[&str] = &[
    "a",
    "b",
    "c",
    "dup",
    "dup",
    "",
    " ",
    "-",
    "+x",
    "@@ -1 +1 @@",
    "--- x",
    "+++ y",
    "\\ No newline at end of file",
    "\tq",
    "naïve",
    "漢字",
];

fn random_lines(rng: &mut Rng, max: usize) -> Vec<String> {
    let n = rng.below(max + 1);
    (0..n)
        .map(|_| LINES[rng.below(LINES.len())].to_string())
        .collect()
}

/// `lines` with random lines removed, replaced and inserted.
fn mutate(rng: &mut Rng, lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for l in lines {
        match rng.below(10) {
            0 => {}
            1 => out.push(LINES[rng.below(LINES.len())].to_string()),
            2 => {
                for _ in 0..1 + rng.below(3) {
                    out.push(LINES[rng.below(LINES.len())].to_string());
                }
                out.push(l.clone());
            }
            _ => out.push(l.clone()),
        }
    }
    if rng.below(4) == 0 {
        out.push(LINES[rng.below(LINES.len())].to_string());
    }
    out
}

/// Checks what every result must hold: the rows show each pane's lines once and in order,
/// rows outside blocks pair equal lines, and blocks hold exactly the lines of their rows.
fn check_rows(r: &DiffResult, panes: &[&[String]], seed: u64) {
    for (p, lines) in panes.iter().enumerate() {
        let shown: Vec<usize> = r.rows.iter().filter_map(|row| row.line(p)).collect();
        assert_eq!(shown, (0..lines.len()).collect::<Vec<_>>(), "seed {seed}");
    }
    for row in r.rows.iter().filter(|row| row.block().is_none()) {
        let first = &panes[0][row.line(0).unwrap()];
        for (p, lines) in panes.iter().enumerate() {
            assert_eq!(&lines[row.line(p).unwrap()], first, "seed {seed}");
        }
    }
    for (bi, b) in r.blocks.iter().enumerate() {
        let rows = &r.rows[b.first_row..b.first_row + b.rows];
        assert!(
            rows.iter().all(|row| row.block() == Some(bi)),
            "seed {seed}"
        );
        for p in 0..panes.len() {
            let lines: Vec<usize> = rows.iter().filter_map(|row| row.line(p)).collect();
            assert_eq!(
                lines,
                b.ranges[p].clone().collect::<Vec<_>>(),
                "seed {seed}"
            );
            // Also for empty ranges, which mark where the other side's lines go.
            let before = r.rows[..b.first_row]
                .iter()
                .filter(|row| row.line(p).is_some())
                .count();
            assert_eq!(
                b.ranges[p].start, before,
                "seed {seed}: block {bi} misplaced"
            );
        }
    }
}

fn unified(a: &[String], b: &[String], r: &DiffResult, context: usize) -> String {
    let input = PatchInput {
        name_a: "a/f.txt",
        name_b: "b/f.txt",
        date_a: "",
        date_b: "",
        a,
        b,
        final_newline_a: true,
        final_newline_b: true,
    };
    make_patch(&input, r, PatchStyle::Unified, context)
}

#[test]
fn random_two_way_diffs_are_whole_and_their_patches_rebuild_the_right_side() {
    let algorithms = [
        DiffAlgorithm::Myers,
        DiffAlgorithm::Patience,
        DiffAlgorithm::Lcs,
    ];
    for seed in seeds(400) {
        let mut rng = Rng::new(seed);
        let a = random_lines(&mut rng, 40);
        let b = mutate(&mut rng, &a);
        let opts = DiffOptions {
            algorithm: algorithms[rng.below(3)],
            detect_moved: rng.below(2) == 0,
            ..DiffOptions::default()
        };
        let r = compute(&[&a, &b], &opts);
        check_rows(&r, &[&a, &b], seed);
        assert_eq!(r.is_identical(), a == b, "seed {seed}");
        let stats = r.stats();
        assert_eq!(
            stats.identical + stats.changed + stats.removed,
            r.rows.iter().filter(|row| row.line(0).is_some()).count(),
            "seed {seed}"
        );
        if a == b {
            continue;
        }
        let patch = unified(&a, &b, &r, rng.below(5));
        let files = parse_unified(&patch).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{patch}"));
        assert_eq!(files.len(), 1, "seed {seed}");
        let done = apply(&a, &files[0]);
        assert!(done.failed.is_empty(), "seed {seed}\n{patch}");
        assert_eq!(done.lines, b, "seed {seed}\n{patch}");
    }
}

#[test]
fn random_three_way_merges_take_every_change_of_one_side() {
    for seed in seeds(300) {
        let mut rng = Rng::new(seed);
        let base = random_lines(&mut rng, 30);
        let changed = mutate(&mut rng, &base);
        assert_eq!(
            merge3(&base, &base, &base).as_ref(),
            Some(&base),
            "seed {seed}"
        );
        assert_eq!(
            merge3(&changed, &base, &base).as_ref(),
            Some(&changed),
            "seed {seed}"
        );
        assert_eq!(
            merge3(&base, &base, &changed).as_ref(),
            Some(&changed),
            "seed {seed}"
        );
        assert_eq!(
            merge3(&changed, &base, &changed).as_ref(),
            Some(&changed),
            "seed {seed}"
        );

        // Changes on both sides apart from each other, with lines found nowhere else between.
        let (top, bottom) = (random_lines(&mut rng, 12), random_lines(&mut rng, 12));
        let sep: Vec<String> = (0..3).map(|i| format!("== separator {i} ==")).collect();
        let join = |a: &[String], b: &[String]| [a, &sep, b].concat();
        let (new_top, new_bottom) = (mutate(&mut rng, &top), mutate(&mut rng, &bottom));
        let left = join(&new_top, &bottom);
        let base = join(&top, &bottom);
        let right = join(&top, &new_bottom);
        assert_eq!(
            merge3(&left, &base, &right),
            Some(join(&new_top, &new_bottom)),
            "seed {seed}"
        );

        let other = mutate(&mut rng, &base);
        let r = compute(&[&left, &base, &other], &DiffOptions::default());
        check_rows(&r, &[&left, &base, &other], seed);
        for b in r.blocks.iter().filter(|b| b.kind == BlockKind::LeftOnly) {
            assert_eq!(
                base[b.ranges[1].clone()],
                other[b.ranges[2].clone()],
                "seed {seed}"
            );
        }
    }
}

#[test]
fn inline_differences_fall_on_character_boundaries() {
    let words = [
        "naïve", "café", "漢字", "x", " ", "  ", "ß", "🙂", "a_b", "1.5", "\t",
    ];
    for seed in seeds(300) {
        let mut rng = Rng::new(seed);
        let mut line = |n: usize| -> String {
            (0..rng.below(n + 1))
                .map(|_| words[rng.below(words.len())])
                .collect()
        };
        let (a, b) = (line(12), line(12));
        for opts in [
            DiffOptions::default(),
            DiffOptions {
                inline: super::diff::InlineMode::Char,
                ignore_case: true,
                whitespace: WhitespaceMode::IgnoreAll,
                ..DiffOptions::default()
            },
        ] {
            let (ra, rb) = inline_diff(&a, &b, &opts);
            for (s, ranges) in [(&a, &ra), (&b, &rb)] {
                for r in ranges {
                    assert!(
                        s.is_char_boundary(r.start) && s.is_char_boundary(r.end),
                        "seed {seed}: {r:?} in {s:?}"
                    );
                }
            }
            if a == b {
                assert!(ra.is_empty() && rb.is_empty(), "seed {seed}");
            }
        }
    }
}

#[test]
fn repetitive_and_large_inputs_stay_correct() {
    let mut rng = Rng::new(2024);
    let a: Vec<String> = (0..20_000)
        .map(|i| {
            if i % 3 == 0 {
                "}".into()
            } else {
                format!("line {}", i % 50)
            }
        })
        .collect();
    let b = mutate(&mut rng, &a);
    let r = compute(&[&a, &b], &DiffOptions::default());
    check_rows(&r, &[&a, &b], 2024);
    let patch = unified(&a, &b, &r, 3);
    let done = apply(&a, &parse_unified(&patch).unwrap()[0]);
    assert!(done.failed.is_empty());
    assert_eq!(done.lines, b);

    let same = vec!["x".to_string(); 1000];
    let r = compute(&[&same, &same[..999]], &DiffOptions::default());
    assert_eq!((r.blocks.len(), r.stats().removed), (1, 1));

    let lines: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
    let mut inserted = lines.clone();
    inserted.splice(500..500, (0..500).map(|i| format!("new {i}")));
    let r = compute(&[&lines, &inserted], &DiffOptions::default());
    assert_eq!((r.blocks.len(), r.stats().added), (1, 500));
    let r = compute(&[&inserted, &lines], &DiffOptions::default());
    assert_eq!((r.blocks.len(), r.stats().removed), (1, 500));
}

#[test]
fn empty_and_identical_inputs() {
    let none: Vec<String> = Vec::new();
    let one = vec!["a".to_string()];
    assert!(compute(&[&none, &none], &DiffOptions::default()).is_identical());
    assert!(compute(&[&one, &one], &DiffOptions::default()).is_identical());
    let r = compute(&[&none, &one], &DiffOptions::default());
    assert_eq!((r.blocks.len(), r.stats().added), (1, 1));
    let r = compute(&[&none, &none, &none], &DiffOptions::default());
    assert!(r.is_identical() && r.rows.is_empty());
    assert_eq!(merge3(&none, &none, &one), Some(one.clone()));
    assert_eq!(merge3(&one, &one, &none), Some(none.clone()));
    // A patch that creates a file, and one that empties it.
    let r = compute(&[&none, &one], &DiffOptions::default());
    let created = parse_unified(&unified(&none, &one, &r, 3)).unwrap();
    assert_eq!(apply(&none, &created[0]).lines, one);
    let r = compute(&[&one, &none], &DiffOptions::default());
    let emptied = parse_unified(&unified(&one, &none, &r, 3)).unwrap();
    assert_eq!(apply(&one, &emptied[0]).lines, none);
}

#[test]
fn line_endings_final_newlines_and_encodings() {
    let (lines, eol, mixed, final_newline) = text::split_lines("a\r\nb\nc\r\nd");
    assert_eq!(lines, ["a", "b", "c", "d"]);
    assert_eq!(
        (eol, mixed, final_newline),
        (Some(text::Eol::CrLf), true, false)
    );
    let (_, _, mixed, final_newline) = text::split_lines("a\nb\n");
    assert!(!mixed && final_newline);
    // The same lines with other line endings compare equal; the lines hold no terminators.
    let unix = text::split_lines("x\ny\n").0;
    let dos = text::split_lines("x\r\ny\r\n").0;
    assert!(compute(&[&unix, &dos], &DiffOptions::default()).is_identical());

    // Text that is not UTF-8 is still read, as the legacy encoding it most likely is.
    let (s, enc, _) = text::decode(b"caf\xe9 cr\xe8me br\xfbl\xe9e", None);
    assert_eq!(s, "café crème brûlée");
    assert_ne!(enc, text::TextEncoding::Utf8);
    let (s, enc, bom) = text::decode(&[0xFF, 0xFE, b'h', 0, b'i', 0], None);
    assert_eq!(
        (s.as_str(), enc, bom),
        ("hi", text::TextEncoding::Utf16Le, true)
    );
    // Bytes that fit no encoding at all do not stop the compare.
    let (s, _, _) = text::decode(&[0xC3, 0x28, 0xA0, 0xA1], Some(text::TextEncoding::Utf8));
    assert!(s.contains('\u{FFFD}'));
    let doc = text::doc_from_bytes(None, "première\nzweite".as_bytes(), None);
    assert_eq!(doc.lines, ["première", "zweite"]);
    assert!(!doc.final_newline);
}

fn v(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

#[test]
fn compare_options_alone_and_together() {
    let rs = super::comments::for_extension("rs");
    let left = v(&["a  b", "ab", "ABC", "", "x; // note", "v = 1.0", "id 12"]);
    let right = v(&["a b", "a b", "abc", "x;", "v = 1.001", "id 13"]);
    let identical = |o: &DiffOptions| compute_with(&[&left, &right], o, rs).is_identical();
    let every = DiffOptions {
        whitespace: WhitespaceMode::IgnoreAll,
        ignore_case: true,
        ignore_blank_lines: true,
        ignore_comments: true,
        numeric_tolerance_enabled: true,
        numeric_tolerance: 0.01,
        ignore_numbers: true,
        ..DiffOptions::default()
    };
    assert!(identical(&every));
    assert!(!identical(&DiffOptions::default()));
    // Without any one of them, its difference shows again.
    let without: [(&str, DiffOptions); 5] = [
        (
            "whitespace",
            DiffOptions {
                whitespace: WhitespaceMode::Compare,
                ..every.clone()
            },
        ),
        (
            "case",
            DiffOptions {
                ignore_case: false,
                ..every.clone()
            },
        ),
        (
            "blank lines",
            DiffOptions {
                ignore_blank_lines: false,
                ..every.clone()
            },
        ),
        (
            "comments",
            DiffOptions {
                ignore_comments: false,
                ..every.clone()
            },
        ),
        (
            "numbers",
            DiffOptions {
                numeric_tolerance_enabled: false,
                ignore_numbers: false,
                ..every.clone()
            },
        ),
    ];
    for (name, o) in without {
        assert!(!identical(&o), "{name}");
    }
    // Ignoring changes of whitespace still sees whitespace added between words.
    let o = DiffOptions {
        whitespace: WhitespaceMode::IgnoreChange,
        ..DiffOptions::default()
    };
    assert!(compute(&[&v(&["a  b"]), &v(&["a b"])], &o).is_identical());
    assert!(!compute(&[&v(&["ab"]), &v(&["a b"])], &o).is_identical());
}

#[test]
fn moved_code_and_word_differences_in_source() {
    let left = v(&[
        "fn helper() -> u32 {",
        "    42",
        "}",
        "",
        "fn main() {",
        "    let total = helper() + 1;",
        "    println!(\"{total}\");",
        "}",
    ]);
    let right = v(&[
        "fn main() {",
        "    let total = helper() + 2;",
        "    println!(\"{total}\");",
        "}",
        "",
        "fn helper() -> u32 {",
        "    42",
        "}",
    ]);
    let r = compute(&[&left, &right], &DiffOptions::default());
    let moved = |p: usize, lines: &[String]| {
        let mut t: Vec<String> = r.moved[p].iter().map(|&i| lines[i].clone()).collect();
        t.sort_unstable();
        t
    };
    assert!(!r.moved[0].is_empty(), "{:?}", r.moved);
    assert_eq!(moved(0, &left), moved(1, &right));
    let o = DiffOptions::default();
    let (a, b) = inline_diff(&left[5], &right[1], &o);
    assert_eq!(
        (&left[5][a[0].clone()], &right[1][b[0].clone()]),
        ("1", "2")
    );
    let (a, b) = inline_diff("let count = 1;", "let total = 1;", &o);
    let word = Some(4..9);
    assert_eq!((a.len(), b.len()), (1, 1));
    assert_eq!(
        (a.first().cloned(), b.first().cloned()),
        (word.clone(), word)
    );
}

#[test]
fn known_merges_of_close_and_overlapping_edits() {
    let base = v(&["1", "2", "3", "4", "5"]);
    // Next to each other with no unchanged line between: a conflict, as in diff3.
    assert_eq!(
        merge3(
            &v(&["1", "L", "3", "4", "5"]),
            &base,
            &v(&["1", "2", "R", "4", "5"])
        ),
        None
    );
    // One unchanged line between them is enough.
    assert_eq!(
        merge3(
            &v(&["1", "L", "3", "4", "5"]),
            &base,
            &v(&["1", "2", "3", "R", "5"])
        ),
        Some(v(&["1", "L", "3", "R", "5"]))
    );
    // The same insertion on both sides is taken once; different ones at one place conflict.
    let both = v(&["1", "2", "new", "3", "4", "5"]);
    assert_eq!(merge3(&both, &base, &both), Some(both.clone()));
    assert_eq!(
        merge3(&both, &base, &v(&["1", "2", "other", "3", "4", "5"])),
        None
    );
    // Deleting a line the other side changed conflicts; deleting it on both sides does not.
    let deleted = v(&["1", "3", "4", "5"]);
    assert_eq!(
        merge3(&deleted, &base, &v(&["1", "two", "3", "4", "5"])),
        None
    );
    assert_eq!(merge3(&deleted, &base, &deleted), Some(deleted.clone()));
    // Changes at both ends of the file.
    assert_eq!(
        merge3(
            &v(&["0", "1", "2", "3", "4", "5"]),
            &base,
            &v(&["1", "2", "3", "4", "5", "6"])
        ),
        Some(v(&["0", "1", "2", "3", "4", "5", "6"]))
    );
}

#[test]
fn malformed_patches_are_refused() {
    let bad = [
        ("", "no unified diff hunks"),
        ("just text\n", "no unified diff hunks"),
        ("@@ -1 +1 @@\n-a\n+b\n", "hunk without a file header"),
        (
            "--- a\n+++ b\n@@ -x +1 @@\n-a\n+b\n",
            "malformed hunk header",
        ),
        ("--- a\n+++ b\n@@ -1,2 +1,2\n a\n", "malformed hunk header"),
        (
            "--- a\n+++ b\n@@ -1,2 +1,2 @@\n a\n",
            "ends in the middle of a hunk",
        ),
        (
            "--- a\n+++ b\n@@ -1 +1 @@\n-a\n-b\n+c\n",
            "longer than its header",
        ),
        ("--- a\n+++ b\n@@ -1,2 +1,2 @@\n a\n*b\n", "unexpected line"),
    ];
    for (patch, why) in bad {
        let err = parse_unified(patch).expect_err(patch);
        assert!(err.contains(why), "{patch:?}: {err}");
    }
}

#[test]
fn patches_meet_files_changed_since() {
    let original = v(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
    let mut target = original.clone();
    target[1] = "B".into();
    target[8] = "I".into();
    let r = compute(&[&original, &target], &DiffOptions::default());
    let patch = parse_unified(&unified(&original, &target, &r, 1)).unwrap();
    assert_eq!(patch[0].hunks.len(), 2);

    // Lines added above: the hunks are found further down.
    let mut moved = v(&["new 1", "new 2"]);
    moved.extend(original.iter().cloned());
    let done = apply(&moved, &patch[0]);
    assert!(done.failed.is_empty());
    assert_eq!(&done.lines[2..], &target[..]);

    // Context changed under the second hunk only: the first applies, the second is reported.
    let mut edited = original.clone();
    edited[7] = "changed".into();
    let done = apply(&edited, &patch[0]);
    assert_eq!(done.failed, [2]);
    assert_eq!(done.lines[1], "B");
    assert_eq!(done.lines[8], "i");

    // Trailing whitespace added since does not stop a hunk.
    let spaced: Vec<String> = original.iter().map(|l| format!("{l}  ")).collect();
    assert!(apply(&spaced, &patch[0]).failed.is_empty());

    // Applied twice, the changes are not made again.
    let again = apply(&target, &patch[0]);
    assert_eq!(again.failed, [1, 2]);
    assert_eq!(again.lines, target);
}

#[test]
fn hunks_follow_the_offset_found_for_the_one_before() {
    // Two lines more at the top than the patch was made against.
    let file = v(&[
        "x", "y", "one", "two", "three", "four", "five", "six", "seven", "eight",
    ]);
    let text = "--- a\n+++ b\n@@ -2 +2 @@\n-two\n+TWO\n@@ -8,0 +9 @@\n+nine\n";
    let done = apply(&file, &parse_unified(text).unwrap()[0]);
    assert!(done.failed.is_empty());
    assert_eq!(
        done.lines,
        v(&["x", "y", "one", "TWO", "three", "four", "five", "six", "seven", "eight", "nine"])
    );
}

#[test]
fn patches_without_context_insert_after_the_stated_line() {
    let base = v(&["1", "2", "3"]);
    let cases = [
        ("@@ -3,0 +4 @@\n+new\n", v(&["1", "2", "3", "new"])),
        ("@@ -1,0 +2 @@\n+new\n", v(&["1", "new", "2", "3"])),
        ("@@ -0,0 +1 @@\n+new\n", v(&["new", "1", "2", "3"])),
        ("@@ -1,0 +1 @@\n+new\n", v(&["new", "1", "2", "3"])),
        (
            "@@ -1 +0,0 @@\n-1\n@@ -2,0 +2 @@\n+x\n",
            v(&["2", "x", "3"]),
        ),
    ];
    for (hunks, want) in cases {
        let text = format!("--- a\n+++ b\n{hunks}");
        let done = apply(&base, &parse_unified(&text).unwrap()[0]);
        assert!(done.failed.is_empty(), "{hunks}");
        assert_eq!(done.lines, want, "{hunks}");
    }
    // The second hunk of `@@ -1 +0,0 @@ -1`, alone: its new start counts the line removed.
    let text = "--- a\n+++ b\n@@ -2,0 +2 @@\n+x\n";
    let done = apply(&base, &parse_unified(text).unwrap()[0]);
    assert_eq!(done.lines, v(&["1", "2", "x", "3"]));
}
