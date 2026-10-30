//! Line based 2-way and 3-way diff engine with WinMerge style ignore options.

use super::comments::{strip as strip_comments, CommentSyntax, Stripped};
use regex::Regex;
use serde::{Deserialize, Serialize};
use similar::algorithms::{self, Capture, Compact, DiffHook, Replace};
use similar::{Algorithm, DiffOp, DiffTag};
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
    /// Runtime-only converters from trusted application settings; never loaded from reviews.
    #[serde(skip)]
    pub unpackers: Vec<super::unpacker::Unpacker>,
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
    /// Changes that are only in comments do not count, for languages whose comments are known.
    pub ignore_comments: bool,
    /// JSON and XML files are reformatted before comparing.
    pub prettify: bool,
    /// Lines that differ only in numbers at most `numeric_tolerance` apart do not count.
    pub numeric_tolerance_enabled: bool,
    pub numeric_tolerance: f64,
    /// Lines from one matching a start regex to the next one matching its end regex do not
    /// count as differences.
    pub block_filters_enabled: bool,
    pub block_filters: Vec<(String, String)>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            unpackers: Vec::new(),
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
            ignore_comments: false,
            prettify: false,
            numeric_tolerance_enabled: false,
            numeric_tolerance: 0.001,
            block_filters_enabled: false,
            block_filters: Vec::new(),
        }
    }
}

/// Applies the ignore options to produce comparison keys.
pub struct Normalizer {
    opts: DiffOptions,
    line_res: Vec<Regex>,
    subs: Vec<(Regex, String)>,
    blocks: Vec<(Regex, Regex)>,
    tolerance: Option<f64>,
}

fn number_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?").unwrap())
}

/// The numbers in `s`, leaving out digits that are part of a word or a dotted version such as
/// `v1.2` or `1.2.3`.
fn numbers(s: &str) -> impl Iterator<Item = regex::Match<'_>> {
    number_re().find_iter(s).filter(move |m| {
        let before = s[..m.start()].chars().next_back();
        let after = s[m.end()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && !(after == Some('.') && s[m.end() + 1..].starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// Whether two numbers are at most `tol` apart. Whole numbers are compared exactly, as
/// floating point cannot tell long ones apart.
fn numbers_close(a: &str, b: &str, tol: f64) -> bool {
    let whole = |s: &str| !s.contains(['.', 'e', 'E']);
    if whole(a) && whole(b) {
        let int = |s: &str| s.trim_start_matches('+').parse::<i128>();
        return match (int(a), int(b)) {
            (Ok(u), Ok(v)) => u.abs_diff(v) as f64 <= tol * (1.0 + 1e-9),
            _ => {
                let digits = |s: &str| {
                    let neg = s.starts_with('-');
                    let d = s.trim_start_matches(['+', '-']).trim_start_matches('0');
                    (neg && !d.is_empty(), d.to_string())
                };
                digits(a) == digits(b)
            }
        };
    }
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(u), Ok(v)) => (u - v).abs() <= tol * (1.0 + 1e-9),
        _ => false,
    }
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
        let blocks = if opts.block_filters_enabled {
            opts.block_filters
                .iter()
                .filter(|(s, e)| !s.is_empty() && !e.is_empty())
                .filter_map(|(s, e)| Some((Regex::new(s).ok()?, Regex::new(e).ok()?)))
                .collect()
        } else {
            Vec::new()
        };
        let tolerance = (opts.numeric_tolerance_enabled && opts.numeric_tolerance >= 0.0)
            .then_some(opts.numeric_tolerance);
        Normalizer {
            opts: opts.clone(),
            line_res,
            subs,
            blocks,
            tolerance,
        }
    }

    /// Whether equality needs a full diff: block filters and the numeric tolerance cannot be
    /// decided one line at a time.
    pub fn needs_diff(&self) -> bool {
        !self.blocks.is_empty() || self.tolerance.is_some()
    }

    /// The lines inside block filters, start and end lines included. The end may be on the
    /// start line, after the start. A block without an end line is not a block.
    pub fn block_mask(&self, lines: &[String]) -> Option<Vec<bool>> {
        if self.blocks.is_empty() {
            return None;
        }
        let mut mask = vec![false; lines.len()];
        let mut open: Option<(usize, &Regex)> = None;
        for (i, line) in lines.iter().enumerate() {
            match open {
                Some((start, end)) if end.is_match(line) => {
                    mask[start..=i].fill(true);
                    open = None;
                }
                Some(_) => {}
                None => {
                    let start = self
                        .blocks
                        .iter()
                        .find_map(|(s, e)| s.find(line).map(|m| (m.end(), e)));
                    if let Some((after, end)) = start {
                        if end.is_match(&line[after..]) {
                            mask[i] = true;
                        } else {
                            open = Some((i, end));
                        }
                    }
                }
            }
        }
        Some(mask)
    }

    /// Whether the lines are the same apart from numbers within the numeric tolerance.
    pub fn close(&self, a: &str, b: &str) -> bool {
        let (ka, kb) = (self.key(a), self.key(b));
        if ka == kb {
            return true;
        }
        let Some(tol) = self.tolerance else {
            return false;
        };
        let (mut na, mut nb) = (numbers(&ka), numbers(&kb));
        let (mut pa, mut pb) = (0, 0);
        loop {
            match (na.next(), nb.next()) {
                (None, None) => return ka[pa..] == kb[pb..],
                (Some(x), Some(y)) => {
                    if ka[pa..x.start()] != kb[pb..y.start()]
                        || !numbers_close(x.as_str(), y.as_str(), tol)
                    {
                        return false;
                    }
                    (pa, pb) = (x.end(), y.end());
                }
                _ => return false,
            }
        }
    }

    fn borrows_keys(&self) -> bool {
        self.subs.is_empty()
            && !self.opts.ignore_numbers
            && self.opts.whitespace == WhitespaceMode::Compare
            && !self.opts.ignore_case
    }

    pub fn key<'a>(&self, line: &'a str) -> Cow<'a, str> {
        if self.borrows_keys() {
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
    /// Three-way: independently mergeable changes to text or different format properties.
    Compatible,
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
    // A missing index is u32::MAX. Avoid four 16-byte Option<usize> values
    // per display row: a 30-million-line file otherwise needs nearly 2 GB here.
    packed: [u32; 4],
}

impl Row {
    const NONE: u32 = u32::MAX;
    fn pack(value: Option<usize>) -> u32 {
        value.map_or(Self::NONE, |value| {
            assert!(
                value < Self::NONE as usize,
                "text comparison exceeds 4 billion lines or blocks"
            );
            value as u32
        })
    }
    fn unpack(value: u32) -> Option<usize> {
        (value != Self::NONE).then_some(value as usize)
    }
    pub fn new(lines: [Option<usize>; 3], block: Option<usize>) -> Self {
        Self {
            packed: [
                Self::pack(lines[0]),
                Self::pack(lines[1]),
                Self::pack(lines[2]),
                Self::pack(block),
            ],
        }
    }
    pub fn line(&self, pane: usize) -> Option<usize> {
        Self::unpack(self.packed[pane])
    }
    pub fn lines(&self) -> [Option<usize>; 3] {
        std::array::from_fn(|p| self.line(p))
    }
    pub fn block(&self) -> Option<usize> {
        Self::unpack(self.packed[3])
    }
    pub fn set_block(&mut self, block: Option<usize>) {
        self.packed[3] = Self::pack(block);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub identical: usize,
    pub changed: usize,
    pub added: usize,
    pub removed: usize,
    pub ignored: usize,
    pub differences: usize,
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

    /// Line counts of a 2-way result: lines only on the right are added, lines only on the
    /// left removed.
    pub fn stats(&self) -> Stats {
        let mut s = Stats {
            differences: self.significant_blocks(),
            ..Default::default()
        };
        for row in &self.rows {
            let (a, b) = (row.line(0).is_some(), row.line(1).is_some());
            match row.block().map(|b| self.blocks[b].ignored) {
                None => s.identical += 1,
                Some(true) => s.ignored += 1,
                Some(false) if a && b => s.changed += 1,
                Some(false) if a => s.removed += 1,
                Some(false) => s.added += 1,
            }
        }
        s
    }

    /// Row index displaying `line` of `pane` (or the nearest row).
    pub fn row_of_line(&self, pane: usize, line: usize) -> usize {
        let mut best = 0;
        for (i, r) in self.rows.iter().enumerate() {
            if let Some(l) = r.line(pane) {
                if l >= line {
                    return i;
                }
                best = i;
            }
        }
        best
    }
}

/// Adds document format differences to a line diff without inventing lines in the documents.
/// Final-newline changes are content changes; `ignore_eol` only ignores terminator styles.
pub fn include_doc_formats(
    result: &mut DiffResult,
    docs: &[&super::text::TextDoc],
    opts: &DiffOptions,
) {
    fn kind<T: PartialEq>(values: &[T]) -> Option<BlockKind> {
        if values.len() == 2 {
            return (values[0] != values[1]).then_some(BlockKind::Diff);
        }
        match (
            values[0] == values[1],
            values[2] == values[1],
            values[0] == values[2],
        ) {
            (true, true, _) => None,
            (false, true, _) => Some(BlockKind::LeftOnly),
            (true, false, _) => Some(BlockKind::RightOnly),
            (false, false, true) => Some(BlockKind::BothSame),
            _ => Some(BlockKind::Conflict),
        }
    }
    fn combine(a: Option<BlockKind>, b: Option<BlockKind>) -> Option<BlockKind> {
        match (a, b) {
            (None, k) | (k, None) => k,
            (Some(BlockKind::Conflict), _) | (_, Some(BlockKind::Conflict)) => {
                Some(BlockKind::Conflict)
            }
            (Some(a), Some(b)) if a == b => Some(a),
            _ => Some(BlockKind::Compatible),
        }
    }
    let codepage = if opts.ignore_codepage {
        None
    } else {
        combine(
            kind(&docs.iter().map(|d| d.encoding).collect::<Vec<_>>()),
            kind(&docs.iter().map(|d| d.bom).collect::<Vec<_>>()),
        )
    };
    // The common case has no format differences anywhere. Avoid a per-row label buffer
    // and millions of tiny allocations when opening very large, terminated UTF-8 files.
    if codepage.is_none()
        && docs.iter().all(|d| d.final_newline)
        && (opts.ignore_eol
            || opts.prettify
            || docs.iter().all(|d| !d.mixed_eol && d.eol == docs[0].eol))
    {
        return;
    }
    let metadata = |row: &Row| {
        let mut changed = codepage;
        if row.lines()[..docs.len()].iter().all(Option::is_some) {
            let mut ends = [None; 3];
            for (p, doc) in docs.iter().enumerate() {
                ends[p] = doc.line_eol(row.line(p).unwrap());
            }
            let ends = &ends[..docs.len()];
            // A retained line can become the unterminated final line after a later deletion.
            let present = [
                ends[0].is_some(),
                ends[1].is_some(),
                ends.get(2).is_some_and(Option::is_some),
            ];
            changed = combine(changed, kind(&present[..docs.len()]));
            if !opts.ignore_eol && !opts.prettify {
                // Missing terminators are handled above independently of their style.
                if ends.iter().all(Option::is_some) {
                    changed = combine(changed, kind(ends));
                }
            }
        }
        changed
    };
    // Empty files can still have different encodings or BOMs.
    if result.rows.is_empty()
        && (1..docs.len()).any(|p| {
            !opts.ignore_codepage
                && (docs[0].encoding != docs[p].encoding || docs[0].bom != docs[p].bom)
        })
    {
        result.rows.push(Row::new([None; 3], None));
    }
    let mut labels = Vec::with_capacity(result.rows.len());
    let mut changed = false;
    for row in &result.rows {
        let meta = metadata(row);
        let old = row
            .block()
            .map(|b| (result.blocks[b].kind, result.blocks[b].ignored, Some(b)));
        let label = match (old, meta) {
            (old, None) => old,
            (Some((kind, false, source)), Some(m)) => {
                Some((combine(Some(kind), Some(m)).unwrap(), false, source))
            }
            (_, Some(m)) => Some((m, false, None)),
        };
        changed |= label != old;
        labels.push(label);
    }
    if !changed {
        return;
    }
    result.blocks.clear();
    let mut positions = [0usize; 3];
    let mut previous = None;
    for (i, (row, label)) in result.rows.iter_mut().zip(labels).enumerate() {
        let start = positions;
        for (p, line) in row.lines().iter().enumerate() {
            if line.is_some() {
                positions[p] += 1;
            }
        }
        row.set_block(label.map(|(kind, ignored, _)| {
            if previous != Some(label) || result.blocks.is_empty() {
                result.blocks.push(Block {
                    ranges: std::array::from_fn(|p| start[p]..positions[p]),
                    kind,
                    ignored,
                    first_row: i,
                    rows: 1,
                });
            } else {
                let last = result.blocks.last_mut().unwrap();
                last.rows += 1;
                for (p, r) in last.ranges.iter_mut().enumerate() {
                    r.end = positions[p];
                }
            }
            result.blocks.len() - 1
        }));
        previous = Some(label);
    }
}

fn algorithm(a: DiffAlgorithm) -> Algorithm {
    match a {
        DiffAlgorithm::Myers => Algorithm::Myers,
        DiffAlgorithm::Patience => Algorithm::Patience,
        DiffAlgorithm::Lcs => Algorithm::Lcs,
    }
}

struct Interner<'a> {
    map: HashMap<Cow<'a, str>, u32>,
}

impl<'a> Interner<'a> {
    fn ids(&mut self, norm: &Normalizer, lines: &'a [String]) -> Vec<u32> {
        lines
            .iter()
            .map(|l| {
                let k = norm.key(l);
                let next = self.map.len() as u32;
                *self.map.entry(k).or_insert(next)
            })
            .collect()
    }
}

fn line_ops(
    a: &[u32],
    b: &[u32],
    alg: DiffAlgorithm,
) -> Vec<(DiffTag, Range<usize>, Range<usize>)> {
    // Past the deadline Myers settles for a coarser diff instead of freezing the window.
    let deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
    // LCS needs a table of a.len() * b.len() entries, which the deadline does not bound.
    let alg = if alg == DiffAlgorithm::Lcs && (a.len() as u64) * (b.len() as u64) > 25_000_000 {
        DiffAlgorithm::Myers
    } else {
        alg
    };
    // Myers takes time proportional to the size times the number of differences, so large
    // files are first split at lines that occur once on each side (patience diff).
    let anchored = alg == DiffAlgorithm::Patience
        || (alg == DiffAlgorithm::Myers && a.len() + b.len() > ANCHOR_MIN_LINES);
    if !anchored {
        return ops_between(a, b, (0, 0), alg, deadline);
    }
    let mut out = Vec::new();
    let (mut pa, mut pb) = (0, 0);
    for (ia, ib) in unique_anchors(a, b) {
        let (ia, ib) = (ia as usize, ib as usize);
        out.extend(ops_between(
            &a[pa..ia],
            &b[pb..ib],
            (pa, pb),
            DiffAlgorithm::Myers,
            deadline,
        ));
        // Adjacent anchors are a single equal run, not millions of tiny operations.
        if let Some((DiffTag::Equal, a, b)) = out
            .last_mut()
            .filter(|(_, a, b)| a.end == ia && b.end == ib)
        {
            a.end += 1;
            b.end += 1;
        } else {
            out.push((DiffTag::Equal, ia..ia + 1, ib..ib + 1));
        }
        (pa, pb) = (ia + 1, ib + 1);
    }
    out.extend(ops_between(
        &a[pa..],
        &b[pb..],
        (pa, pb),
        DiffAlgorithm::Myers,
        deadline,
    ));
    out
}

const ANCHOR_MIN_LINES: usize = 10_000;

/// Lines that occur exactly once in `a` and once in `b`, paired, keeping the longest run of
/// pairs that is in order on both sides.
fn unique_anchors(a: &[u32], b: &[u32]) -> Vec<(u32, u32)> {
    // Interned IDs are dense. Store only unique positions in a compact array instead
    // of a second per-line hash table. Sparse external/segmented IDs use the fallback.
    const MISSING: u32 = u32::MAX;
    const REPEATED: u32 = u32::MAX - 1;
    let max_id = a.iter().chain(b).copied().max().map_or(0, |id| id as usize);
    let mut pairs: Vec<(u32, u32)> = if max_id <= a.len().saturating_add(b.len()).saturating_mul(2)
        && a.len().max(b.len()) < REPEATED as usize
    {
        let mut positions = vec![(MISSING, MISSING); max_id + 1];
        for (side, ids) in [a, b].into_iter().enumerate() {
            for (i, &id) in ids.iter().enumerate() {
                let entry = &mut positions[id as usize];
                let pos = if side == 0 {
                    &mut entry.0
                } else {
                    &mut entry.1
                };
                *pos = if *pos == MISSING { i as u32 } else { REPEATED };
            }
        }
        a.iter()
            .enumerate()
            .filter_map(|(i, &id)| {
                let (left, right) = positions[id as usize];
                (left == i as u32 && right < REPEATED).then_some((i as u32, right))
            })
            .collect()
    } else {
        let mut seen: HashMap<u32, (usize, usize, usize, usize)> = HashMap::new();
        for (i, &id) in a.iter().enumerate() {
            let e = seen.entry(id).or_default();
            e.0 += 1;
            e.2 = i;
        }
        for (i, &id) in b.iter().enumerate() {
            let e = seen.entry(id).or_default();
            e.1 += 1;
            e.3 = i;
        }
        a.iter()
            .enumerate()
            .filter_map(|(i, id)| {
                let &(ca, cb, ia, ib) = &seen[id];
                (ca == 1 && cb == 1 && ia == i).then_some((ia as u32, ib as u32))
            })
            .collect()
    };
    // Source-like large files usually keep all unique anchors in order already.
    if pairs.windows(2).all(|p| p[0].1 < p[1].1) {
        return pairs;
    }
    // longest increasing subsequence of the b indexes, by patience sorting
    let mut tails: Vec<u32> = Vec::new();
    let mut prev = vec![u32::MAX; pairs.len()];
    for (k, &(_, y)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t as usize].1 < y);
        prev[k] = pos.checked_sub(1).map_or(u32::MAX, |p| tails[p]);
        if pos == tails.len() {
            tails.push(k as u32);
        } else {
            tails[pos] = k as u32;
        }
    }
    // Reconstruct into the unused suffix of the existing pair buffer. Walking
    // backwards only writes at/after the current index, so predecessor pairs survive.
    let count = tails.len();
    let mut write = pairs.len();
    let mut at = tails.last().copied();
    while let Some(index) = at {
        let index = index as usize;
        write -= 1;
        pairs[write] = pairs[index];
        at = (prev[index] != u32::MAX).then_some(prev[index]);
    }
    pairs.copy_within(write.., 0);
    pairs.truncate(count);
    pairs
}

/// The diff of `a` and `b`, with indexes shifted by `offset`.
fn ops_between(
    a: &[u32],
    b: &[u32],
    offset: (usize, usize),
    alg: DiffAlgorithm,
    deadline: Option<std::time::Instant>,
) -> Vec<(DiffTag, Range<usize>, Range<usize>)> {
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    let mut found = Capture::new();
    let Ok(()) = algorithms::diff_deadline(
        algorithm(alg),
        &mut found,
        a,
        0..a.len(),
        b,
        0..b.len(),
        deadline,
    );
    let ops = found.into_ops();
    // Compacting slides changes to cleaner boundaries, but takes time quadratic in the number
    // of changes, so long runs of differences skip it.
    let ops = if ops.len() <= MAX_COMPACT_OPS {
        compact(&ops, a, b)
    } else {
        ops
    };
    // The ops cover both sides in order; their positions are rebuilt from the lengths, as
    // compacting can leave a stale position on the empty side of a moved insertion or deletion.
    let (mut pa, mut pb) = offset;
    ops.iter()
        .map(|op| {
            let (tag, ra, rb) = op.as_tag_tuple();
            let o = pa..pa + ra.len();
            let n = pb..pb + rb.len();
            (pa, pb) = (o.end, n.end);
            (tag, o, n)
        })
        .collect()
}

const MAX_COMPACT_OPS: usize = 4000;

fn compact(ops: &[DiffOp], a: &[u32], b: &[u32]) -> Vec<DiffOp> {
    let mut d = Compact::new(Replace::new(Capture::new()), a, b);
    for op in ops {
        let Ok(()) = match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => d.equal(old_index, new_index, len),
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => d.delete(old_index, old_len, new_index),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => d.insert(old_index, new_index, new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => d.replace(old_index, old_len, new_index, new_len),
        };
    }
    let Ok(()) = d.finish();
    d.into_inner().into_inner().into_ops()
}

/// Compute a diff between 2 or 3 documents. For three panes the middle one is the base.
pub fn compute(panes: &[&[String]], opts: &DiffOptions) -> DiffResult {
    compute_with(panes, opts, None)
}

/// Like `compute`; with `ignore_comments` set, `comments` is the syntax of the documents.
pub fn compute_with(
    panes: &[&[String]],
    opts: &DiffOptions,
    comments: Option<&CommentSyntax>,
) -> DiffResult {
    compute_synced(panes, opts, comments, &[])
}

/// A line of each pane (only the first `panes` entries count) that the compare puts on one row:
/// the differences are worked out separately before and after it, as WinMerge's
/// synchronization points do when the compare pairs the wrong lines.
pub type SyncPoint = [usize; 3];

/// The synchronization points that can be kept, in order: each within its panes' lines and
/// after the previous one in every pane. Others are left out.
pub fn valid_sync_points(points: &[SyncPoint], lens: &[usize]) -> Vec<SyncPoint> {
    let mut sorted = points.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out: Vec<SyncPoint> = Vec::new();
    for s in sorted {
        let inside = lens.iter().enumerate().all(|(p, &n)| s[p] < n);
        let after = out
            .last()
            .is_none_or(|last| (0..lens.len()).all(|p| s[p] > last[p]));
        if inside && after {
            out.push(s);
        }
    }
    out
}

/// Like `compute_with`, with the lines of each synchronization point on one row (see
/// [`SyncPoint`]; ones that can't be kept are left out, see [`valid_sync_points`]).
pub fn compute_synced(
    panes: &[&[String]],
    opts: &DiffOptions,
    comments: Option<&CommentSyntax>,
    sync: &[SyncPoint],
) -> DiffResult {
    let norm = Normalizer::new(opts);
    let stripped: Vec<Vec<Stripped>> = match comments.filter(|_| opts.ignore_comments) {
        Some(syn) => panes.iter().map(|p| strip_comments(p, syn)).collect(),
        None => Vec::new(),
    };
    let code: Vec<Vec<String>> = stripped
        .iter()
        .map(|p| p.iter().map(|s| s.code.clone()).collect())
        .collect();
    let keyed: Vec<&[String]> = if code.is_empty() {
        panes.to_vec()
    } else {
        code.iter().map(Vec::as_slice).collect()
    };
    let skip = panes
        .iter()
        .enumerate()
        .map(|(p, lines)| {
            let comment_only = stripped.get(p).map(|s| s.iter().map(|s| s.comment_only));
            match (norm.block_mask(lines), comment_only) {
                (Some(mask), Some(c)) => mask.iter().zip(c).map(|(&m, c)| m || c).collect(),
                (Some(mask), None) => mask,
                (None, Some(c)) => c.collect(),
                (None, None) => Vec::new(),
            }
        })
        .collect();
    let ignorable = Ignorable {
        norm: &norm,
        panes: &keyed,
        skip,
    };
    // The ordinary case needs only borrowed str keys, without Cow's discriminant
    // in every hash bucket. Normalizing rules retain their owned-key behavior.
    let ids: Vec<Vec<u32>> = if norm.borrows_keys() {
        // Estimate an initial dictionary from a small sample. Starting at zero
        // repeatedly allocates/frees huge tables whose pages can stay resident.
        // Discount the estimate so repeated/generated files do not reserve by line count.
        let first = keyed.first().copied().unwrap_or(&[]);
        let sample = first.len().min(4096);
        let distinct: HashSet<&str> = first.iter().take(sample).map(String::as_str).collect();
        let initial = first.len().saturating_mul(distinct.len()) / sample.max(1) / 2;
        let mut dictionary = HashMap::<&str, u32>::with_capacity(initial);
        keyed
            .iter()
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| {
                        let next = dictionary.len() as u32;
                        *dictionary.entry(line.as_str()).or_insert(next)
                    })
                    .collect()
            })
            .collect()
    } else {
        let mut interner = Interner {
            map: HashMap::new(),
        };
        keyed.iter().map(|p| interner.ids(&norm, p)).collect()
    };
    // Both dictionaries leave scope before allocating anchors or display rows.
    #[cfg(test)]
    super::bench::profile("interned", || {
        format!("unique_max={}", ids.iter().flatten().max().unwrap_or(&0))
    });

    let lens: Vec<usize> = panes.iter().map(|p| p.len()).collect();
    let sync = valid_sync_points(sync, &lens);
    let mut result = if sync.is_empty() {
        diff_segment(&ids, &ignorable, opts)
    } else {
        // Each synchronization point's lines make a row of their own; between them, the
        // panes are compared as if they held only those lines.
        let n = panes.len();
        let mut result = DiffResult {
            panes: n,
            ..Default::default()
        };
        let mut start = [0usize; 3];
        // Rows where a difference before a point's row could run on into it, or one in it
        // into the next part: such neighbors are one difference.
        let mut seams = Vec::new();
        for point in sync.iter().map(Some).chain([None]) {
            let end: Vec<usize> = (0..n).map(|p| point.map_or(lens[p], |s| s[p])).collect();
            let part = segment(&ids, &ignorable, &start[..n], &end, opts);
            append(&mut result, part, &start);
            if let Some(s) = point {
                seams.push(result.rows.len());
                seams.push(result.rows.len() + 1);
                let ranges: Vec<Range<usize>> = (0..n).map(|p| s[p]..s[p] + 1).collect();
                let same = (1..n).all(|p| ids[p][s[p]] == ids[0][s[0]]);
                if same {
                    result.rows.push(Row::new(
                        [Some(s[0]), Some(s[1]), (n == 3).then_some(s[2])],
                        None,
                    ));
                } else {
                    let kind = if n == 3 {
                        let r = [ranges[0].clone(), ranges[1].clone(), ranges[2].clone()];
                        kind3(&ids, &r).unwrap_or(BlockKind::Conflict)
                    } else {
                        BlockKind::Diff
                    };
                    let ignored = ignorable.all(&ranges);
                    let mut r = [0..0, 0..0, 0..0];
                    r[..n].clone_from_slice(&ranges);
                    push_block(&mut result, r, kind, ignored);
                }
                for p in 0..n {
                    start[p] = s[p] + 1;
                }
            }
        }
        join_at_seams(&mut result, &seams, &ids);
        result
    };
    if opts.detect_moved {
        detect_moved(&mut result, &ids);
    }
    result
}

fn diff_segment<T: AsRef<[u32]>>(
    ids: &[T],
    ignorable: &Ignorable,
    opts: &DiffOptions,
) -> DiffResult {
    if ids.len() == 3 {
        diff3(ids, ignorable, opts)
    } else {
        diff2(ids[0].as_ref(), ids[1].as_ref(), ignorable, opts)
    }
}

/// The compare of lines `start[p]..end[p]` of each pane, numbered from those starts.
fn segment(
    ids: &[Vec<u32>],
    ignorable: &Ignorable,
    start: &[usize],
    end: &[usize],
    opts: &DiffOptions,
) -> DiffResult {
    let part_ids: Vec<&[u32]> = (0..ids.len()).map(|p| &ids[p][start[p]..end[p]]).collect();
    let part_panes: Vec<&[String]> = (0..ids.len())
        .map(|p| &ignorable.panes[p][start[p]..end[p]])
        .collect();
    let part = Ignorable {
        norm: ignorable.norm,
        panes: &part_panes,
        skip: (0..ids.len())
            .map(|p| match ignorable.skip.get(p) {
                Some(s) if !s.is_empty() => s[start[p]..end[p]].to_vec(),
                _ => Vec::new(),
            })
            .collect(),
    };
    diff_segment(&part_ids, &part, opts)
}

/// Adds `part`, whose lines are numbered from `start`, after the rows of `result`.
fn append(result: &mut DiffResult, part: DiffResult, start: &[usize; 3]) {
    let (rows, blocks) = (result.rows.len(), result.blocks.len());
    for mut row in part.rows {
        row = Row::new(
            std::array::from_fn(|p| row.line(p).map(|l| l + start[p])),
            row.block().map(|b| b + blocks),
        );
        result.rows.push(row);
    }
    for mut b in part.blocks {
        for (r, s) in b.ranges.iter_mut().zip(start) {
            *r = r.start + s..r.end + s;
        }
        b.first_row += rows;
        result.blocks.push(b);
    }
}

/// Makes one difference of each two that meet at one of the rows `seams` (a difference ending
/// on the row before and one starting on it), when both are ignored or neither is.
fn join_at_seams(result: &mut DiffResult, seams: &[usize], ids: &[Vec<u32>]) {
    let mut join = vec![false; result.blocks.len()];
    for &r in seams {
        if r == 0 || r >= result.rows.len() {
            continue;
        }
        if let (Some(a), Some(b)) = (result.rows[r - 1].block(), result.rows[r].block()) {
            if a != b && result.blocks[a].ignored == result.blocks[b].ignored {
                join[b] = true;
            }
        }
    }
    if !join.contains(&true) {
        return;
    }
    let mut to = vec![0; join.len()];
    let mut blocks: Vec<Block> = Vec::with_capacity(join.len());
    for (i, b) in std::mem::take(&mut result.blocks).into_iter().enumerate() {
        match blocks.last_mut() {
            Some(last) if join[i] => {
                for (r, next) in last.ranges.iter_mut().zip(&b.ranges) {
                    r.end = next.end;
                }
                last.rows += b.rows;
                if result.panes == 3 {
                    last.kind = kind3(ids, &last.ranges).unwrap_or(BlockKind::Conflict);
                }
            }
            _ => blocks.push(b),
        }
        to[i] = blocks.len() - 1;
    }
    result.blocks = blocks;
    for row in &mut result.rows {
        row.set_block(row.block().map(|b| to[b]));
    }
}

/// Decides which lines do not count as differences on their own.
struct Ignorable<'a> {
    norm: &'a Normalizer,
    panes: &'a [&'a [String]],
    /// Per pane, lines that hold only comments or are inside a block filter (empty when
    /// neither option applies).
    skip: Vec<Vec<bool>>,
}

impl Ignorable<'_> {
    fn all(&self, ranges: &[Range<usize>]) -> bool {
        self.panes
            .iter()
            .zip(ranges)
            .enumerate()
            .all(|(p, (lines, r))| {
                r.clone().all(|i| {
                    self.skip.get(p).is_some_and(|c| c.get(i) == Some(&true))
                        || self.norm.is_ignorable(&lines[i])
                })
            })
    }

    /// With a numeric tolerance, splits a changed block whose sides have the same number of
    /// lines into runs of rows that do (`true`) and do not differ only in close numbers. Each
    /// row is compared with the left side, or with the base in a 3-way compare.
    fn split_close(&self, ranges: &[Range<usize>]) -> Vec<(Vec<Range<usize>>, bool)> {
        let whole = vec![(ranges.to_vec(), false)];
        let n = ranges[0].len();
        if self.norm.tolerance.is_none() || n == 0 || ranges.iter().any(|r| r.len() != n) {
            return whole;
        }
        let base = if ranges.len() == 3 { 1 } else { 0 };
        let close = |k: usize| {
            let b = &self.panes[base][ranges[base].start + k];
            (0..ranges.len())
                .filter(|&p| p != base)
                .all(|p| self.norm.close(&self.panes[p][ranges[p].start + k], b))
        };
        let flags: Vec<bool> = (0..n).map(close).collect();
        if !flags.iter().any(|&c| c) {
            return whole;
        }
        let mut out = Vec::new();
        let mut start = 0;
        for k in 1..=n {
            if k == n || flags[k] != flags[start] {
                let sub = ranges
                    .iter()
                    .map(|r| r.start + start..r.start + k)
                    .collect();
                out.push((sub, flags[start]));
                start = k;
            }
        }
        out
    }
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
        result.rows.push(Row::new(line, Some(idx)));
    }
    result.blocks.push(Block {
        ranges,
        kind,
        ignored,
        first_row,
        rows: n,
    });
}

fn diff2(a: &[u32], b: &[u32], ignorable: &Ignorable, opts: &DiffOptions) -> DiffResult {
    let mut result = DiffResult {
        panes: 2,
        ..Default::default()
    };
    let ops = line_ops(a, b, opts.algorithm);
    // Insertions can make the alignment longer than either input. Reserve from
    // the operations, avoiding a nearly 2x row-buffer growth just above that boundary.
    let row_bound: usize = ops.iter().map(|(_, a, b)| a.len().max(b.len())).sum();
    result.rows.reserve(row_bound);
    let mut pending: Option<(Range<usize>, Range<usize>)> = None;
    let flush = |result: &mut DiffResult, pending: &mut Option<(Range<usize>, Range<usize>)>| {
        if let Some((ra, rb)) = pending.take() {
            for (r, close) in ignorable.split_close(&[ra, rb]) {
                let ignored = close || ignorable.all(&r);
                let [ra, rb] = [r[0].clone(), r[1].clone()];
                push_block(result, [ra, rb, 0..0], BlockKind::Diff, ignored);
            }
        }
    };
    for (tag, o, n) in ops {
        if tag == DiffTag::Equal {
            flush(&mut result, &mut pending);
            for k in 0..o.len() {
                result
                    .rows
                    .push(Row::new([Some(o.start + k), Some(n.start + k), None], None));
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

fn diff3<T: AsRef<[u32]>>(ids: &[T], ignorable: &Ignorable, opts: &DiffOptions) -> DiffResult {
    let (left, base, right) = (ids[0].as_ref(), ids[1].as_ref(), ids[2].as_ref());
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
            result
                .rows
                .push(Row::new([Some(l), Some(i), Some(r)], None));
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
        let ranges = [l..le, i..j, r..re];
        if kind3(ids, &ranges).is_some() {
            for (sub, close) in ignorable.split_close(&ranges) {
                let sub = [sub[0].clone(), sub[1].clone(), sub[2].clone()];
                match kind3(ids, &sub) {
                    Some(kind) => {
                        let ignored = close || ignorable.all(&sub);
                        push_block(&mut result, sub, kind, ignored);
                    }
                    None => push_equal3(&mut result, &sub),
                }
            }
        } else {
            push_equal3(&mut result, &ranges);
        }
        i = j;
        l = le;
        r = re;
    }
    result
}

/// The 3-way merge of `left` and `right` with their common ancestor `base`, taking every
/// change made on one side (or the same on both), or `None` when the sides conflict.
pub fn merge3(left: &[String], base: &[String], right: &[String]) -> Option<Vec<String>> {
    let opts = DiffOptions {
        ignore_eol: false,
        detect_moved: false,
        ..Default::default()
    };
    let d = compute(&[left, base, right], &opts);
    let mut out = Vec::with_capacity(base.len());
    let mut row = 0;
    while row < d.rows.len() {
        let r = d.rows[row];
        let Some(b) = r.block() else {
            out.extend(r.line(1).map(|l| base[l].clone()));
            row += 1;
            continue;
        };
        let block = &d.blocks[b];
        let (src, range) = match block.kind {
            BlockKind::LeftOnly | BlockKind::BothSame => (left, &block.ranges[0]),
            BlockKind::RightOnly => (right, &block.ranges[2]),
            BlockKind::Conflict | BlockKind::Diff | BlockKind::Compatible => return None,
        };
        out.extend_from_slice(&src[range.clone()]);
        row = block.first_row + block.rows;
    }
    Some(out)
}

/// How the left and right lines of a 3-way block changed relative to the base, or `None`
/// when they did not.
fn kind3<T: AsRef<[u32]>>(ids: &[T], [rl, rb, rr]: &[Range<usize>; 3]) -> Option<BlockKind> {
    let (left, base, right) = (
        &ids[0].as_ref()[rl.clone()],
        &ids[1].as_ref()[rb.clone()],
        &ids[2].as_ref()[rr.clone()],
    );
    match (left != base, right != base) {
        (true, false) => Some(BlockKind::LeftOnly),
        (false, true) => Some(BlockKind::RightOnly),
        (true, true) if left == right => Some(BlockKind::BothSame),
        (true, true) => Some(BlockKind::Conflict),
        (false, false) => None,
    }
}

fn push_equal3(result: &mut DiffResult, [rl, rb, rr]: &[Range<usize>; 3]) {
    for k in 0..rb.len() {
        result.rows.push(Row::new(
            [Some(rl.start + k), Some(rb.start + k), Some(rr.start + k)],
            None,
        ));
    }
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
    // Long lines of many tokens could take long; past the deadline the result is coarser.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, &ka, &kb, Some(deadline));
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

    /// Checks that `ops` cover both sides in order and pair only equal lines.
    fn assert_valid(ops: &[(DiffTag, Range<usize>, Range<usize>)], a: &[u32], b: &[u32]) {
        let (mut i, mut j) = (0, 0);
        for (tag, ra, rb) in ops {
            assert_eq!((ra.start, rb.start), (i, j));
            if *tag == DiffTag::Equal {
                assert_eq!(a[ra.clone()], b[rb.clone()]);
            }
            (i, j) = (ra.end, rb.end);
        }
        assert_eq!((i, j), (a.len(), b.len()));
    }

    #[test]
    fn packed_rows_keep_indices_and_missing_sides_in_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<Row>(), 16);
        let mut row = Row::new([Some(0), None, Some(u32::MAX as usize - 1)], Some(42));
        assert_eq!(row.lines(), [Some(0), None, Some(u32::MAX as usize - 1)]);
        assert_eq!(row.block(), Some(42));
        row.set_block(None);
        assert_eq!(row.block(), None);
    }

    #[test]
    fn interning_borrows_unchanged_keys_and_owns_transformed_keys() {
        let lines = vec!["UPPER".to_string(), "second".into(), "UPPER".into()];
        let mut interner = Interner {
            map: HashMap::new(),
        };
        assert_eq!(
            interner.ids(&Normalizer::new(&DiffOptions::default()), &lines),
            [0, 1, 0]
        );
        assert!(interner
            .map
            .keys()
            .all(|key| matches!(key, Cow::Borrowed(_))));
        let mut interner = Interner {
            map: HashMap::new(),
        };
        let options = DiffOptions {
            ignore_case: true,
            ..Default::default()
        };
        assert_eq!(interner.ids(&Normalizer::new(&options), &lines), [0, 1, 0]);
        assert!(interner.map.keys().all(|key| matches!(key, Cow::Owned(_))));
    }

    #[test]
    fn dense_and_sparse_unique_anchors_match_the_reference_selection() {
        let reference = |a: &[u32], b: &[u32]| {
            let mut entries: HashMap<u32, (usize, usize, usize, usize)> = HashMap::new();
            for (i, &id) in a.iter().enumerate() {
                let e = entries.entry(id).or_default();
                e.0 += 1;
                e.2 = i;
            }
            for (i, &id) in b.iter().enumerate() {
                let e = entries.entry(id).or_default();
                e.1 += 1;
                e.3 = i;
            }
            let mut pairs: Vec<_> = entries
                .values()
                .filter(|e| e.0 == 1 && e.1 == 1)
                .map(|e| (e.2, e.3))
                .collect();
            pairs.sort_unstable();
            let mut tails = Vec::<usize>::new();
            let mut prev = vec![None; pairs.len()];
            for (k, &(_, y)) in pairs.iter().enumerate() {
                let pos = tails.partition_point(|&t| pairs[t].1 < y);
                prev[k] = pos.checked_sub(1).map(|p| tails[p]);
                if pos == tails.len() {
                    tails.push(k);
                } else {
                    tails[pos] = k;
                }
            }
            let mut out = Vec::new();
            let mut at = tails.last().copied();
            while let Some(i) = at {
                out.push(pairs[i]);
                at = prev[i];
            }
            out.reverse();
            out
        };
        let mut seed = 7u32;
        for _ in 0..200 {
            let mut make = || {
                (0..60)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        (seed >> 16) % 100
                    })
                    .collect::<Vec<_>>()
            };
            let (a, b) = (make(), make());
            for scale in [1, 10_000_000] {
                let a: Vec<_> = a.iter().map(|&n| n * scale).collect();
                let b: Vec<_> = b.iter().map(|&n| n * scale).collect();
                assert_eq!(
                    unique_anchors(&a, &b),
                    reference(&a, &b)
                        .into_iter()
                        .map(|(a, b)| (a as u32, b as u32))
                        .collect::<Vec<_>>()
                );
            }
        }
        let equal: Vec<_> = (0..50_000).collect();
        let ops = line_ops(&equal, &equal, DiffAlgorithm::Myers);
        assert_valid(&ops, &equal, &equal);
        assert_eq!(
            ops.len(),
            1,
            "adjacent equal anchors should be one operation"
        );
    }

    #[test]
    fn document_format_differences_respect_options_and_three_way_kinds() {
        use crate::core::text::{TextDoc, TextEncoding};
        let a = TextDoc::from_text(None, "one\r\ntwo\r\n");
        let b = TextDoc::from_text(None, "one\ntwo\n");
        let compare = |docs: &[&TextDoc], opts: &DiffOptions| {
            let lines: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
            let mut r = compute(&lines, opts);
            include_doc_formats(&mut r, docs, opts);
            r
        };
        assert!(compare(&[&a, &b], &DiffOptions::default()).is_identical());
        let strict = DiffOptions {
            ignore_eol: false,
            ignore_codepage: false,
            ..Default::default()
        };
        let r = compare(&[&a, &b], &strict);
        assert_eq!(r.significant_blocks(), 1);
        assert_eq!(r.blocks[0].ranges[..2], [0..2, 0..2]);
        let r = compare(&[&a, &b, &b], &strict);
        assert_eq!(r.blocks[0].kind, BlockKind::LeftOnly);
        let r = compare(&[&a, &b, &a], &strict);
        assert_eq!(r.blocks[0].kind, BlockKind::BothSame);
        let mut encoded = b.clone();
        encoded.encoding = TextEncoding::Utf16Le;
        encoded.bom = true;
        assert!(!compare(&[&b, &encoded], &strict).is_identical());
        assert!(compare(&[&b, &encoded], &DiffOptions::default()).is_identical());
        let unterminated = TextDoc::from_text(None, "one\ntwo");
        let r = compare(&[&b, &unterminated], &DiffOptions::default());
        assert_eq!(r.blocks[0].ranges[..2], [1..2, 1..2]);
        let mixed = TextDoc::from_text(None, "one\r\ntwo\n");
        assert!(!compare(&[&b, &mixed], &strict).is_identical());
        assert!(compare(&[&mixed, &mixed], &strict).is_identical());
        let empty = TextDoc::empty(None);
        let mut bom = empty.clone();
        bom.bom = true;
        assert!(!compare(&[&empty, &bom], &strict).is_identical());
    }

    #[test]
    fn large_diffs_are_split_at_unique_lines() {
        let mut seed = 7u32;
        let mut rand = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            seed >> 16
        };
        let a: Vec<u32> = (0..30_000)
            .map(|i| if i % 10 == 0 { 1 } else { i })
            .collect();
        let mut b = Vec::new();
        for &x in &a {
            match rand() % 20 {
                0 => {}
                1 => b.push(1_000_000 + rand()),
                2 => b.extend([x, 2_000_000 + rand()]),
                _ => b.push(x),
            }
        }
        let anchored = line_ops(&a, &b, DiffAlgorithm::Myers);
        assert_valid(&anchored, &a, &b);
        let small = (&a[..3000], &b[..3000]);
        let plain = line_ops(small.0, small.1, DiffAlgorithm::Myers);
        assert_valid(&plain, small.0, small.1);
        let patience = line_ops(small.0, small.1, DiffAlgorithm::Patience);
        assert_valid(&patience, small.0, small.1);
        let equal = |ops: &[(DiffTag, Range<usize>, Range<usize>)]| {
            ops.iter()
                .filter(|o| o.0 == DiffTag::Equal)
                .map(|o| o.1.len())
                .sum::<usize>()
        };
        // Anchoring may give up a few matches, but not many.
        assert!(equal(&patience) * 100 >= equal(&plain) * 99);
        assert_eq!(
            unique_anchors(&[1, 2, 3, 4], &[4, 2, 3, 1]),
            [(1, 1), (2, 2)]
        );
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    /// Every line of every pane appears once, in order, and blocks match their rows.
    fn assert_consistent(d: &DiffResult, lens: &[usize]) {
        for (p, &n) in lens.iter().enumerate() {
            let seen: Vec<usize> = d.rows.iter().filter_map(|r| r.line(p)).collect();
            assert_eq!(seen, (0..n).collect::<Vec<_>>(), "pane {p}");
        }
        for (i, b) in d.blocks.iter().enumerate() {
            for r in &d.rows[b.first_row..b.first_row + b.rows] {
                assert_eq!(r.block(), Some(i));
            }
        }
    }

    fn row_with<'a>(d: &'a DiffResult, lines: &[usize]) -> Option<&'a Row> {
        d.rows
            .iter()
            .find(|r| lines.iter().enumerate().all(|(p, &l)| r.line(p) == Some(l)))
    }

    #[test]
    fn sync_points_put_their_lines_on_one_row() {
        let a = v(&[
            "intro",
            "fn one() {",
            "  body",
            "}",
            "fn two() {",
            "  more",
            "}",
        ]);
        let b = v(&[
            "fn two() {",
            "  more",
            "}",
            "intro",
            "fn one() {",
            "  body",
            "}",
        ]);
        let opts = DiffOptions::default();
        // Unsynchronized, "intro" (0) pairs with right line 3.
        let plain = compute(&[&a, &b], &opts);
        assert!(row_with(&plain, &[0, 3]).is_some());
        // Anchored at the two "fn two" lines, they share a row, and so does what follows.
        let d = compute_synced(&[&a, &b], &opts, None, &[[4, 0, 0]]);
        assert_consistent(&d, &[7, 3 + 4]);
        let anchor = row_with(&d, &[4, 0]).expect("the sync point is one row");
        assert_eq!(anchor.block(), None, "equal lines");
        assert!(row_with(&d, &[5, 1]).is_some() && row_with(&d, &[6, 2]).is_some());
        // Lines that differ still share the row, as a change.
        let d = compute_synced(&[&a, &b], &opts, None, &[[0, 1, 0]]);
        assert_consistent(&d, &[7, 7]);
        let r = row_with(&d, &[0, 1]).unwrap();
        assert!(r.block().is_some_and(|b| !d.blocks[b].ignored));
    }

    #[test]
    fn sync_points_that_differ_join_the_differences_next_to_them() {
        let opts = DiffOptions::default();
        let a = v(&["same", "X", "anchor L", "P", "tail"]);
        let b = v(&["same", "Y", "anchor R", "Q", "tail"]);
        let d = compute_synced(&[&a, &b], &opts, None, &[[2, 2, 0]]);
        assert_consistent(&d, &[5, 5]);
        assert_eq!(d.blocks.len(), 1, "{:?}", d.blocks);
        assert_eq!(d.blocks[0].ranges[..2], [1..4, 1..4]);
        // Equal sync lines keep the differences around them apart.
        let b = v(&["same", "Y", "anchor L", "Q", "tail"]);
        let d = compute_synced(&[&a, &b], &opts, None, &[[2, 2, 0]]);
        assert_consistent(&d, &[5, 5]);
        assert_eq!(d.blocks.len(), 2);
        // 3-way: the joined difference is classified as a whole.
        let base = v(&["x", "anchor", "y"]);
        let left = v(&["x", "anchor", "y"]);
        let right = v(&["X", "ANCHOR", "y"]);
        let d = compute_synced(&[&left, &base, &right], &opts, None, &[[1, 1, 1]]);
        assert_consistent(&d, &[3, 3, 3]);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].kind, BlockKind::RightOnly);
    }

    #[test]
    fn sync_points_that_cross_or_fall_outside_are_left_out() {
        let lens = [5, 5];
        assert_eq!(
            valid_sync_points(&[[3, 1, 0], [1, 3, 0], [2, 2, 0], [9, 0, 0]], &lens),
            vec![[1, 3, 0]],
            "sorted by the first pane: [1,3] is kept, then [2,2] and [3,1] go back in pane 2"
        );
        assert_eq!(
            valid_sync_points(&[[1, 1, 0], [1, 1, 0], [2, 4, 0]], &lens),
            vec![[1, 1, 0], [2, 4, 0]]
        );
        let a = v(&["a", "b"]);
        let d = compute_synced(&[&a, &a], &DiffOptions::default(), None, &[[7, 7, 0]]);
        assert!(d.is_identical());
        assert_consistent(&d, &[2, 2]);
    }

    #[test]
    fn three_way_sync_points_align_all_panes() {
        let base = v(&["x", "shared", "y"]);
        let left = v(&["shared", "x", "y"]);
        let right = v(&["x", "y", "shared"]);
        let opts = DiffOptions::default();
        let d = compute_synced(&[&left, &base, &right], &opts, None, &[[0, 1, 2]]);
        assert_consistent(&d, &[3, 3, 3]);
        let r = row_with(&d, &[0, 1, 2]).expect("one row");
        assert_eq!(r.block(), None);
        // A sync point whose lines differ is classified like any 3-way change, together with
        // the change before it: "x" left out on the left, "y" and "other" added on the right.
        let right2 = v(&["x", "y", "other"]);
        let d = compute_synced(&[&left, &base, &right2], &opts, None, &[[0, 1, 2]]);
        assert_consistent(&d, &[3, 3, 3]);
        let r = row_with(&d, &[0, 1, 2]).unwrap();
        assert_eq!(d.blocks[r.block().unwrap()].kind, BlockKind::Conflict);
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
    fn comment_changes_ignored() {
        let syn = crate::core::comments::for_extension("rs").unwrap();
        let a = v(&["let x = 1; // one", "/* old", "   note */", "let y = 2;"]);
        let b = v(&["let x = 1;", "// new note", "let y = 2;"]);
        let o = DiffOptions {
            ignore_comments: true,
            ..Default::default()
        };
        assert!(compute_with(&[&a, &b], &o, Some(syn)).is_identical());
        assert!(!compute_with(&[&a, &b], &DiffOptions::default(), Some(syn)).is_identical());
        let c = v(&["let x = 2; // one", "/* old", "   note */", "let y = 2;"]);
        assert_eq!(
            compute_with(&[&a, &c], &o, Some(syn)).significant_blocks(),
            1
        );
    }

    #[test]
    fn numbers_within_tolerance_and_filtered_blocks_ignored() {
        let o = DiffOptions {
            numeric_tolerance_enabled: true,
            numeric_tolerance: 0.01,
            ..Default::default()
        };
        let a = v(&["x = 1.000, y = -2", "name a", "z = 3e2", "w = 5"]);
        let b = v(&["x = 1.004, y = -2.00", "name b", "z = 300.001", "w = 6"]);
        let r = compute(&[&a, &b], &o);
        let sig: Vec<_> = r.blocks.iter().filter(|b| !b.ignored).collect();
        assert_eq!(sig.len(), 2, "{:?}", r.blocks);
        assert_eq!(sig[0].ranges[0], 1..2);
        assert_eq!(sig[1].ranges[0], 3..4);
        assert_eq!(compute(&[&a, &b], &DiffOptions::default()).blocks.len(), 1);
        assert_eq!(
            r.stats(),
            Stats {
                changed: 2,
                ignored: 2,
                differences: 2,
                ..Default::default()
            }
        );

        let exact = Normalizer::new(&DiffOptions {
            numeric_tolerance: 0.0,
            ..o.clone()
        });
        assert!(!exact.close("id=9007199254740993", "id=9007199254740992"));
        assert!(!exact.close("n=123456789012345678901234567890123456789012", "n=1"));
        assert!(exact.close("n=+007", "n=7"));
        let n = Normalizer::new(&o);
        assert!(!n.close("v1.2", "v1.3"));
        assert!(!n.close("1.2.3", "1.2.4"));
        assert!(!n.close("x1", "x2") && n.close("x 1", "x 1.001"));
        assert!(n.close("-5", "-5.005"));

        let base = v(&["t = 10.0", "keep"]);
        let left = v(&["t = 10.001", "keep"]);
        let right = v(&["t = 10.0", "changed"]);
        let r = compute(&[&left, &base, &right], &o);
        assert_eq!(r.significant_blocks(), 1);
        assert_eq!(
            r.blocks.iter().find(|b| !b.ignored).unwrap().kind,
            BlockKind::RightOnly
        );

        let o = DiffOptions {
            block_filters_enabled: true,
            block_filters: vec![("^// BEGIN GENERATED".into(), "^// END GENERATED".into())],
            ..Default::default()
        };
        let a = v(&["a", "// BEGIN GENERATED", "1", "// END GENERATED", "b"]);
        let b = v(&["a", "// BEGIN GENERATED", "2", "3", "// END GENERATED", "b"]);
        assert!(compute(&[&a, &b], &o).is_identical());
        let o = DiffOptions {
            block_filters: vec![(r"/\*".into(), r"\*/".into())],
            ..o
        };
        let a = v(&["/* note */", "code", "same", "/* one", "two */", "end"]);
        let b = v(&["/* other */", "CODE", "same", "/* three", "four */", "end"]);
        let r = compute(&[&a, &b], &o);
        assert_eq!(r.significant_blocks(), 1, "{:?}", r.blocks);
        assert!(r
            .blocks
            .iter()
            .any(|b| !b.ignored && b.ranges[0].contains(&1)));
        assert!(r.blocks.iter().any(|b| b.ignored && b.ranges[0] == (3..5)));
        let o = DiffOptions {
            block_filters: vec![("^// BEGIN GENERATED".into(), "^// END GENERATED".into())],
            ..o
        };
        let unterminated = v(&["a", "// BEGIN GENERATED", "2", "b"]);
        let c = v(&["a", "// BEGIN GENERATED", "3", "b"]);
        assert!(!compute(&[&unterminated, &c], &o).is_identical());
    }

    #[test]
    fn merges_three_way_without_conflicts() {
        let base = v(&["a", "b", "c", "d"]);
        let left = v(&["a", "B", "c", "d"]);
        let right = v(&["a", "b", "c", "d", "e"]);
        assert_eq!(
            merge3(&left, &base, &right).unwrap(),
            v(&["a", "B", "c", "d", "e"])
        );
        let other = v(&["a", "X", "c", "d"]);
        assert!(merge3(&left, &base, &other).is_none());
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
