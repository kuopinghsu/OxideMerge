//! Line based 2-way and 3-way diff engine with WinMerge style ignore options.

use regex::Regex;
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum WhitespaceMode {
    Compare,
    IgnoreChange,
    IgnoreAll,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum DiffAlgorithm {
    Myers,
    Patience,
    Lcs,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum InlineMode {
    Off,
    Word,
    Char,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct DiffOptions {
    pub whitespace: WhitespaceMode,
    pub ignore_case: bool,
    pub ignore_blank_lines: bool,
    pub ignore_eol: bool,
    pub ignore_numbers: bool,
    pub ignore_codepage: bool,
    pub algorithm: DiffAlgorithm,
    pub detect_moved: bool,
    pub inline: InlineMode,
    pub line_filters_enabled: bool,
    pub line_filters: Vec<String>,
    pub substitutions_enabled: bool,
    pub substitutions: Vec<(String, String)>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            whitespace: WhitespaceMode::Compare,
            ignore_case: false,
            ignore_blank_lines: false,
            ignore_eol: true,
            ignore_numbers: false,
            ignore_codepage: true,
            algorithm: DiffAlgorithm::Myers,
            detect_moved: true,
            inline: InlineMode::Word,
            line_filters_enabled: false,
            line_filters: Vec::new(),
            substitutions_enabled: false,
            substitutions: Vec::new(),
        }
    }
}

/// Applies the ignore options to produce comparison keys.
pub struct Normalizer {
    opts: DiffOptions,
    line_res: Vec<Regex>,
    subs: Vec<(Regex, String)>,
}

impl Normalizer {
    pub fn new(opts: &DiffOptions) -> Self {
        let line_res = if opts.line_filters_enabled {
            opts.line_filters
                .iter()
                .filter(|s| !s.trim().is_empty())
                .filter_map(|s| Regex::new(s).ok())
                .collect()
        } else {
            Vec::new()
        };
        let subs = if opts.substitutions_enabled {
            opts.substitutions
                .iter()
                .filter(|(s, _)| !s.is_empty())
                .filter_map(|(s, r)| Regex::new(s).ok().map(|re| (re, r.clone())))
                .collect()
        } else {
            Vec::new()
        };
        Normalizer {
            opts: opts.clone(),
            line_res,
            subs,
        }
    }

    pub fn key<'a>(&self, line: &'a str) -> Cow<'a, str> {
        let o = &self.opts;
        if self.subs.is_empty()
            && !o.ignore_numbers
            && o.whitespace == WhitespaceMode::Compare
            && !o.ignore_case
        {
            return Cow::Borrowed(line);
        }
        let mut s: String = line.to_string();
        for (re, rep) in &self.subs {
            s = re.replace_all(&s, rep.as_str()).into_owned();
        }
        if self.opts.ignore_numbers {
            s.retain(|c| !c.is_ascii_digit());
        }
        match self.opts.whitespace {
            WhitespaceMode::Compare => {}
            WhitespaceMode::IgnoreAll => s.retain(|c| !c.is_whitespace()),
            WhitespaceMode::IgnoreChange => {
                let mut out = String::with_capacity(s.len());
                let mut in_ws = false;
                for c in s.chars() {
                    if c.is_whitespace() {
                        in_ws = true;
                    } else {
                        if in_ws {
                            out.push(' ');
                        }
                        in_ws = false;
                        out.push(c);
                    }
                }
                s = out;
            }
        }
        if self.opts.ignore_case {
            s = s.to_lowercase();
        }
        Cow::Owned(s)
    }

    /// Lines that do not count as differences on their own (blank lines / line filters).
    pub fn is_ignorable(&self, line: &str) -> bool {
        if self.opts.ignore_blank_lines && line.trim().is_empty() {
            return true;
        }
        self.line_res.iter().any(|re| re.is_match(line))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    /// Two-way difference.
    Diff,
    /// Three-way: only the left side changed relative to the base.
    LeftOnly,
    /// Three-way: only the right side changed relative to the base.
    RightOnly,
    /// Three-way: both sides made the same change (only the middle differs).
    BothSame,
    /// Three-way: conflicting changes.
    Conflict,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub ranges: [Range<usize>; 3],
    pub kind: BlockKind,
    pub ignored: bool,
    pub first_row: usize,
    pub rows: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub line: [Option<usize>; 3],
    pub block: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct DiffResult {
    pub panes: usize,
    pub rows: Vec<Row>,
    pub blocks: Vec<Block>,
    pub moved: [HashSet<usize>; 3],
}

impl DiffResult {
    pub fn significant_blocks(&self) -> usize {
        self.blocks.iter().filter(|b| !b.ignored).count()
    }

    pub fn conflicts(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| !b.ignored && b.kind == BlockKind::Conflict)
            .count()
    }

    pub fn is_identical(&self) -> bool {
        self.significant_blocks() == 0
    }

    /// Row index displaying `line` of `pane` (or the nearest row).
    pub fn row_of_line(&self, pane: usize, line: usize) -> usize {
        let mut best = 0;
        for (i, r) in self.rows.iter().enumerate() {
            if let Some(l) = r.line[pane] {
                if l >= line {
                    return i;
                }
                best = i;
            }
        }
        best
    }
}

fn algorithm(a: DiffAlgorithm) -> Algorithm {
    match a {
        DiffAlgorithm::Myers => Algorithm::Myers,
        DiffAlgorithm::Patience => Algorithm::Patience,
        DiffAlgorithm::Lcs => Algorithm::Lcs,
    }
}

struct Interner {
    map: HashMap<String, u32>,
}

impl Interner {
    fn ids(&mut self, norm: &Normalizer, lines: &[String]) -> Vec<u32> {
        lines
            .iter()
            .map(|l| {
                let k = norm.key(l);
                if let Some(&id) = self.map.get(k.as_ref()) {
                    return id;
                }
                let id = self.map.len() as u32;
                self.map.insert(k.into_owned(), id);
                id
            })
            .collect()
    }
}

fn line_ops(
    a: &[u32],
    b: &[u32],
    alg: DiffAlgorithm,
) -> Vec<(DiffTag, Range<usize>, Range<usize>)> {
    let deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    // LCS needs a table of a.len() * b.len() entries, which the deadline does not bound.
    let alg = if alg == DiffAlgorithm::Lcs && (a.len() as u64) * (b.len() as u64) > 25_000_000 {
        DiffAlgorithm::Myers
    } else {
        alg
    };
    similar::capture_diff_slices_deadline(algorithm(alg), a, b, deadline)
        .iter()
        .map(|op| op.as_tag_tuple())
        .collect()
}

/// Compute a diff between 2 or 3 documents. For three panes the middle one is the base.
pub fn compute(panes: &[&[String]], opts: &DiffOptions) -> DiffResult {
    let norm = Normalizer::new(opts);
    let mut interner = Interner {
        map: HashMap::new(),
    };
    let ids: Vec<Vec<u32>> = panes.iter().map(|p| interner.ids(&norm, p)).collect();
    let mut result = if panes.len() == 3 {
        diff3(&ids, panes, &norm, opts)
    } else {
        diff2(&ids[0], &ids[1], panes, &norm, opts)
    };
    if opts.detect_moved {
        detect_moved(&mut result, &ids);
    }
    result
}

fn all_ignorable(norm: &Normalizer, panes: &[&[String]], ranges: &[Range<usize>]) -> bool {
    panes
        .iter()
        .zip(ranges)
        .all(|(p, r)| p[r.clone()].iter().all(|l| norm.is_ignorable(l)))
}

fn push_block(result: &mut DiffResult, ranges: [Range<usize>; 3], kind: BlockKind, ignored: bool) {
    let n = ranges.iter().map(|r| r.len()).max().unwrap_or(0);
    if n == 0 {
        return;
    }
    let idx = result.blocks.len();
    let first_row = result.rows.len();
    for i in 0..n {
        let mut line = [None; 3];
        for p in 0..3 {
            if i < ranges[p].len() {
                line[p] = Some(ranges[p].start + i);
            }
        }
        result.rows.push(Row {
            line,
            block: Some(idx),
        });
    }
    result.blocks.push(Block {
        ranges,
        kind,
        ignored,
        first_row,
        rows: n,
    });
}

fn diff2(
    a: &[u32],
    b: &[u32],
    panes: &[&[String]],
    norm: &Normalizer,
    opts: &DiffOptions,
) -> DiffResult {
    let mut result = DiffResult {
        panes: 2,
        ..Default::default()
    };
    let mut pending: Option<(Range<usize>, Range<usize>)> = None;
    let flush = |result: &mut DiffResult, pending: &mut Option<(Range<usize>, Range<usize>)>| {
        if let Some((ra, rb)) = pending.take() {
            let ignored = all_ignorable(norm, panes, &[ra.clone(), rb.clone()]);
            push_block(result, [ra, rb, 0..0], BlockKind::Diff, ignored);
        }
    };
    for (tag, o, n) in line_ops(a, b, opts.algorithm) {
        if tag == DiffTag::Equal {
            flush(&mut result, &mut pending);
            for k in 0..o.len() {
                result.rows.push(Row {
                    line: [Some(o.start + k), Some(n.start + k), None],
                    block: None,
                });
            }
        } else {
            pending = Some(match pending {
                Some((pa, pb)) => (pa.start..o.end.max(pa.end), pb.start..n.end.max(pb.end)),
                None => (o, n),
            });
        }
    }
    flush(&mut result, &mut pending);
    result
}

fn matches(a: &[u32], b: &[u32], alg: DiffAlgorithm) -> Vec<Option<usize>> {
    let mut m = vec![None; a.len()];
    for (tag, o, n) in line_ops(a, b, alg) {
        if tag == DiffTag::Equal {
            for k in 0..o.len() {
                m[o.start + k] = Some(n.start + k);
            }
        }
    }
    m
}

fn diff3(
    ids: &[Vec<u32>],
    panes: &[&[String]],
    norm: &Normalizer,
    opts: &DiffOptions,
) -> DiffResult {
    let (left, base, right) = (&ids[0], &ids[1], &ids[2]);
    let ml = matches(base, left, opts.algorithm);
    let mr = matches(base, right, opts.algorithm);
    let mut result = DiffResult {
        panes: 3,
        ..Default::default()
    };
    let (mut i, mut l, mut r) = (0usize, 0usize, 0usize);
    let (nb, nl, nr) = (base.len(), left.len(), right.len());
    while i < nb || l < nl || r < nr {
        if i < nb && ml[i] == Some(l) && mr[i] == Some(r) {
            result.rows.push(Row {
                line: [Some(l), Some(i), Some(r)],
                block: None,
            });
            i += 1;
            l += 1;
            r += 1;
            continue;
        }
        let mut j = i;
        while j < nb && !(ml[j].is_some() && mr[j].is_some()) {
            j += 1;
        }
        let (le, re) = if j < nb {
            (ml[j].unwrap(), mr[j].unwrap())
        } else {
            (nl, nr)
        };
        let (rl, rb, rr) = (l..le, i..j, r..re);
        let lc = left[rl.clone()] != base[rb.clone()];
        let rc = right[rr.clone()] != base[rb.clone()];
        let kind = match (lc, rc) {
            (true, false) => Some(BlockKind::LeftOnly),
            (false, true) => Some(BlockKind::RightOnly),
            (true, true) => {
                if left[rl.clone()] == right[rr.clone()] {
                    Some(BlockKind::BothSame)
                } else {
                    Some(BlockKind::Conflict)
                }
            }
            (false, false) => None,
        };
        match kind {
            Some(kind) => {
                let ranges = [rl.clone(), rb.clone(), rr.clone()];
                let ignored = all_ignorable(norm, panes, &ranges);
                push_block(&mut result, ranges, kind, ignored);
            }
            None => {
                for k in 0..rb.len() {
                    result.rows.push(Row {
                        line: [Some(l + k), Some(i + k), Some(r + k)],
                        block: None,
                    });
                }
            }
        }
        i = j;
        l = le;
        r = re;
    }
    result
}

fn detect_moved(result: &mut DiffResult, ids: &[Vec<u32>]) {
    let n = result.panes;
    // For each pane, the sorted, deduplicated blocks each line id appears in.
    let mut per_pane: Vec<HashMap<u32, Vec<usize>>> = vec![HashMap::new(); n];
    for (bi, b) in result.blocks.iter().enumerate() {
        for p in 0..n {
            for line in b.ranges[p].clone() {
                let blocks = per_pane[p].entry(ids[p][line]).or_default();
                if blocks.last() != Some(&bi) {
                    blocks.push(bi);
                }
            }
        }
    }
    let mut moved: [HashSet<usize>; 3] = Default::default();
    for (bi, b) in result.blocks.iter().enumerate() {
        if b.ignored {
            continue;
        }
        for p in 0..n {
            for line in b.ranges[p].clone() {
                let id = ids[p][line];
                // Moved: the line is in another block of another pane, but not in this block there.
                let found = (0..n).filter(|&q| q != p).any(|q| {
                    per_pane[q]
                        .get(&id)
                        .is_some_and(|blocks| blocks.binary_search(&bi).is_err())
                });
                if found {
                    moved[p].insert(line);
                }
            }
        }
    }
    result.moved = moved;
}

/// Character/word level differences between two lines. Returns byte ranges for each side.
pub fn inline_diff(a: &str, b: &str, opts: &DiffOptions) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if opts.inline == InlineMode::Off {
        return (Vec::new(), Vec::new());
    }
    if a.len() > 4000 || b.len() > 4000 {
        return (
            std::iter::once(0..a.len()).collect(),
            std::iter::once(0..b.len()).collect(),
        );
    }
    let ta = tokenize(a, opts);
    let tb = tokenize(b, opts);
    let ka: Vec<&str> = ta.iter().map(|t| t.1.as_str()).collect();
    let kb: Vec<&str> = tb.iter().map(|t| t.1.as_str()).collect();
    let ops = similar::capture_diff_slices(Algorithm::Myers, &ka, &kb);
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    for op in ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        for t in &ta[o] {
            push_merge(&mut ra, t.0.clone());
        }
        for t in &tb[n] {
            push_merge(&mut rb, t.0.clone());
        }
    }
    (ra, rb)
}

fn push_merge(v: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.is_empty() {
        return;
    }
    if let Some(last) = v.last_mut() {
        if last.end == r.start {
            last.end = r.end;
            return;
        }
    }
    v.push(r);
}

fn tokenize(s: &str, opts: &DiffOptions) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    let norm = |t: &str| -> String {
        if opts.ignore_case {
            t.to_lowercase()
        } else {
            t.to_string()
        }
    };
    while i < chars.len() {
        let (start, c) = chars[i];
        let mut j = i + 1;
        if opts.inline == InlineMode::Word {
            if c.is_alphanumeric() || c == '_' {
                while j < chars.len() && (chars[j].1.is_alphanumeric() || chars[j].1 == '_') {
                    j += 1;
                }
            } else if c.is_whitespace() {
                while j < chars.len() && chars[j].1.is_whitespace() {
                    j += 1;
                }
            }
        }
        let end = if j < chars.len() { chars[j].0 } else { s.len() };
        let text = &s[start..end];
        if c.is_whitespace() {
            match opts.whitespace {
                WhitespaceMode::Compare => out.push((start..end, norm(text))),
                WhitespaceMode::IgnoreChange => out.push((start..end, " ".into())),
                WhitespaceMode::IgnoreAll => {}
            }
        } else {
            out.push((start..end, norm(text)));
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn two_way_basic() {
        let a = v(&["a", "b", "c"]);
        let b = v(&["a", "x", "c", "d"]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        assert_eq!(r.blocks.len(), 2);
        assert_eq!(r.blocks[0].ranges[0], 1..2);
        assert_eq!(r.blocks[0].ranges[1], 1..2);
        assert_eq!(r.blocks[1].ranges[1], 3..4);
        assert_eq!(r.rows.len(), 4);
    }

    #[test]
    fn ignore_whitespace_and_case() {
        let a = v(&["Hello  World"]);
        let b = v(&["hello world"]);
        let mut o = DiffOptions::default();
        assert!(!compute(&[&a, &b], &o).is_identical());
        o.whitespace = WhitespaceMode::IgnoreChange;
        o.ignore_case = true;
        assert!(compute(&[&a, &b], &o).is_identical());
    }

    #[test]
    fn blank_lines_ignored() {
        let a = v(&["a", "", "b"]);
        let b = v(&["a", "b"]);
        let o = DiffOptions {
            ignore_blank_lines: true,
            ..Default::default()
        };
        let r = compute(&[&a, &b], &o);
        assert_eq!(r.blocks.len(), 1);
        assert!(r.is_identical());
    }

    #[test]
    fn three_way_kinds() {
        let base = v(&["1", "2", "3", "4"]);
        let left = v(&["1", "L", "3", "4"]);
        let right = v(&["1", "2", "3", "R"]);
        let r = compute(&[&left, &base, &right], &DiffOptions::default());
        let kinds: Vec<_> = r.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![BlockKind::LeftOnly, BlockKind::RightOnly]);
        let right2 = v(&["1", "X", "3", "4"]);
        let r = compute(&[&left, &base, &right2], &DiffOptions::default());
        assert_eq!(r.blocks[0].kind, BlockKind::Conflict);
    }

    #[test]
    fn inline_word_diff() {
        let o = DiffOptions::default();
        let (a, b) = inline_diff("let x = 1;", "let y = 1;", &o);
        assert_eq!(a, vec![4..5]);
        assert_eq!(b, vec![4..5]);
    }

    #[test]
    fn detects_moved_lines() {
        let a = v(&["moved", "a", "b", "c", "x"]);
        let b = v(&["y", "a", "b", "c", "moved"]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        assert!(r.moved[0].contains(&0));
        assert!(r.moved[1].contains(&4));
        assert!(!r.moved[0].contains(&4));
        assert!(!r.moved[1].contains(&0));
    }

    #[test]
    fn moved_detection_handles_large_repetitive_blocks() {
        // One huge block full of identical lines used to take quadratic time.
        let n = 40_000;
        let ids: Vec<Vec<u32>> = (0..2u32)
            .map(|p| {
                (0..n as u32)
                    .map(|i| if i % 2 == 0 { 0 } else { 1 + p * n as u32 + i })
                    .collect()
            })
            .collect();
        let mut r = DiffResult {
            panes: 2,
            ..Default::default()
        };
        push_block(&mut r, [0..n, 0..n, 0..0], BlockKind::Diff, false);
        let start = std::time::Instant::now();
        detect_moved(&mut r, &ids);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(r.moved[0].is_empty() && r.moved[1].is_empty());
    }
}
