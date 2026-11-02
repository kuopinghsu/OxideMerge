//! Matching the records of two tables by key columns instead of by position.
//!
//! Records whose key is unique on both sides are paired, whatever their order. Records with a
//! duplicate, empty or missing key are never paired by guess: they are paired by position among
//! themselves only to be shown side by side, and are marked ambiguous so that nothing copies
//! them automatically. Values are normalized for comparing only; the tables are not changed.

use super::table::Table;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tables with more records than this are compared by position only.
pub const MAX_RECORDS: usize = 2_000_000;

/// A key column, by its header name or, without a header row, by its position (0-based).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyColumn {
    Name(String),
    Index(usize),
}

/// How the columns of the two sides are paired for comparing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnPolicy {
    /// The same position on both sides.
    #[default]
    Position,
    /// The same header name, wherever the column is.
    Name,
}

/// The order the matched rows are shown in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowOrder {
    /// The order of the left side; records only on the right follow the right record before
    /// them.
    #[default]
    Left,
    /// The order of the right side; records only on the left follow the left record before them.
    Right,
    /// By key value, numbers by value; records without a key last, in left order.
    Key,
}

/// How records are matched; matching by key is on when `keys` is not empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableMatch {
    /// The first record of each side holds the column names.
    pub header: bool,
    pub keys: Vec<KeyColumn>,
    pub columns: ColumnPolicy,
    /// Records in another order are not differences (they are still counted).
    pub ignore_order: bool,
    /// Numbers at most this far apart are equal; 0 is off.
    pub abs_tolerance: f64,
    /// Numbers apart by at most this fraction of the larger one are equal; 0 is off.
    pub rel_tolerance: f64,
    pub order: RowOrder,
}

impl TableMatch {
    pub fn by_key(&self) -> bool {
        !self.keys.is_empty()
    }
}

/// What a matched row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Identical,
    /// Both sides have the record, and some compared cells differ.
    Modified,
    /// Only the right side has the record.
    Added,
    /// Only the left side has the record.
    Removed,
    /// The key is duplicate, empty or missing: the records are not matched.
    Ambiguous,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchedRow {
    /// Record index on the left and on the right side.
    pub records: [Option<usize>; 2],
    pub state: RowState,
    /// The record is in another order relative to the other matched records.
    pub moved: bool,
    /// The column names of a header row.
    pub header: bool,
    /// Columns, on each side, whose value differs from the paired column.
    pub changed: [Vec<usize>; 2],
}

impl MatchedRow {
    /// The row counts as a difference: a change, a record of one side, an ambiguous key, or
    /// unless the order is ignored, a move.
    pub fn differs(&self, ignore_order: bool) -> bool {
        self.state != RowState::Identical || (self.moved && !ignore_order)
    }
}

/// Why a key or column could not be used as asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// A key column is not on a side (0 left, 1 right).
    MissingKey {
        side: usize,
        column: String,
    },
    /// A key column name is on a side more than once.
    DuplicateKeyColumn {
        side: usize,
        column: String,
    },
    /// Matching by name needs a header row.
    NoHeader,
    TooLarge,
    /// The user stopped the matching.
    Cancelled,
    /// The matching stopped on an internal error.
    Failed,
}

/// Something the user should know about a match that was still made.
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// A header name is on a side more than once; such columns are paired in their order.
    DuplicateHeader { side: usize, column: String },
    /// Columns that only one side has, so they are not compared.
    UnpairedColumns { side: usize, columns: Vec<String> },
    /// Records whose key is on that side more than once.
    DuplicateKeys { side: usize, records: usize },
    /// Records whose key cells are empty or missing.
    EmptyKeys { side: usize, records: usize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub identical: usize,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
    pub ambiguous: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableDiff {
    /// In the order of [`TableMatch::order`], the header row first.
    pub rows: Vec<MatchedRow>,
    /// Compared columns: the left and right column of each pair.
    pub columns: Vec<[Option<usize>; 2]>,
    pub counts: Counts,
    pub warnings: Vec<Warning>,
    pub ignore_order: bool,
}

impl TableDiff {
    /// The column of side `to` paired with column `column` of side `from`.
    pub fn paired_column(&self, from: usize, column: usize, to: usize) -> Option<usize> {
        self.columns
            .iter()
            .find(|c| c[from] == Some(column))
            .and_then(|c| c[to])
    }

    #[cfg(test)]
    pub fn differences(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.differs(self.ignore_order))
            .count()
    }
}

/// How cells are compared: `key` normalizes a key cell; `eq` tells whether two values are
/// the same under the compare options.
pub struct Compare<'a> {
    pub key: &'a dyn Fn(&str) -> String,
    pub eq: &'a dyn Fn(&str, &str) -> bool,
}

fn header_names(table: &Table) -> Vec<String> {
    table
        .records
        .first()
        .map(|r| r.cells.iter().map(|c| c.trim().to_string()).collect())
        .unwrap_or_default()
}

fn column_label(names: &[String], column: usize) -> String {
    match names.get(column) {
        Some(name) if !name.is_empty() => name.clone(),
        _ => format!("#{}", column + 1),
    }
}

/// The position of key column `key` on a side.
fn key_index(names: &[String], key: &KeyColumn, side: usize) -> Result<usize, Problem> {
    match key {
        KeyColumn::Index(i) => Ok(*i),
        KeyColumn::Name(name) => {
            let mut found = names.iter().enumerate().filter(|(_, n)| *n == name.trim());
            match (found.next(), found.next()) {
                (Some((i, _)), None) => Ok(i),
                (None, _) => Err(Problem::MissingKey {
                    side,
                    column: name.clone(),
                }),
                (Some(_), Some(_)) => Err(Problem::DuplicateKeyColumn {
                    side,
                    column: name.clone(),
                }),
            }
        }
    }
}

fn width(table: &Table) -> usize {
    table
        .records
        .iter()
        .map(|r| r.cells.len())
        .max()
        .unwrap_or(0)
}

/// Pairs the columns of the two sides.
fn pair_columns(
    tables: [&Table; 2],
    names: &[Vec<String>; 2],
    policy: ColumnPolicy,
    warnings: &mut Vec<Warning>,
) -> Vec<[Option<usize>; 2]> {
    let widths = [width(tables[0]), width(tables[1])];
    if policy == ColumnPolicy::Position {
        let pairs: Vec<[Option<usize>; 2]> = (0..widths[0].max(widths[1]))
            .map(|c| [(c < widths[0]).then_some(c), (c < widths[1]).then_some(c)])
            .collect();
        unpaired(&pairs, names, warnings);
        return pairs;
    }
    for (side, side_names) in names.iter().enumerate() {
        let mut seen = HashMap::new();
        for name in side_names.iter().filter(|n| !n.is_empty()) {
            *seen.entry(name.as_str()).or_insert(0) += 1;
        }
        let mut dups: Vec<&str> = seen
            .into_iter()
            .filter(|&(_, n)| n > 1)
            .map(|(k, _)| k)
            .collect();
        dups.sort_unstable();
        for column in dups {
            warnings.push(Warning::DuplicateHeader {
                side,
                column: column.to_string(),
            });
        }
    }
    // The n-th column of a name on the right, for each name.
    let mut right: HashMap<(&str, usize), usize> = HashMap::new();
    let mut occurrence: HashMap<&str, usize> = HashMap::new();
    for (j, name) in names[1].iter().enumerate().take(widths[1]) {
        if name.is_empty() {
            continue;
        }
        let k = occurrence.entry(name.as_str()).or_insert(0);
        right.insert((name.as_str(), *k), j);
        *k += 1;
    }
    let mut used = vec![false; widths[1]];
    let mut pairs = Vec::new();
    occurrence.clear();
    for i in 0..widths[0] {
        let name = names[0].get(i).map(String::as_str).unwrap_or("");
        let j = (!name.is_empty())
            .then(|| {
                let k = occurrence.entry(name).or_insert(0);
                let j = right.get(&(name, *k)).copied();
                *k += 1;
                j
            })
            .flatten();
        if let Some(j) = j {
            used[j] = true;
        }
        pairs.push([Some(i), j]);
    }
    for (j, used) in used.iter().enumerate() {
        if !used {
            pairs.push([None, Some(j)]);
        }
    }
    unpaired(&pairs, names, warnings);
    pairs
}

/// Warns of the columns of one side that have none on the other, which are not compared.
fn unpaired(pairs: &[[Option<usize>; 2]], names: &[Vec<String>; 2], warnings: &mut Vec<Warning>) {
    for side in 0..2 {
        let columns: Vec<String> = pairs
            .iter()
            .filter(|p| p[1 - side].is_none())
            .filter_map(|p| p[side])
            .map(|c| column_label(&names[side], c))
            .collect();
        if !columns.is_empty() {
            warnings.push(Warning::UnpairedColumns { side, columns });
        }
    }
}

/// Whether two cells are equal: by the compare options, else as numbers within a tolerance.
fn same_value(a: &str, b: &str, opts: &TableMatch, cmp: &Compare) -> bool {
    if (cmp.eq)(a, b) {
        return true;
    }
    if opts.abs_tolerance <= 0.0 && opts.rel_tolerance <= 0.0 {
        return false;
    }
    let number = |s: &str| {
        let t = s.trim();
        // Only plain decimal numbers: "inf", "NaN" and the like are text.
        let plain = !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
            && t.bytes().any(|b| b.is_ascii_digit());
        plain
            .then(|| t.parse::<f64>().ok())
            .flatten()
            .filter(|x| x.is_finite())
    };
    let (Some(x), Some(y)) = (number(a), number(b)) else {
        return false;
    };
    let d = (x - y).abs();
    (opts.abs_tolerance > 0.0 && d <= opts.abs_tolerance)
        || (opts.rel_tolerance > 0.0 && d <= opts.rel_tolerance * x.abs().max(y.abs()))
}

/// Indices into `seq` of a longest increasing subsequence of it.
fn longest_increasing(seq: &[usize]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; seq.len()];
    for (i, &v) in seq.iter().enumerate() {
        let pos = tails.partition_point(|&t| seq[t] < v);
        if pos > 0 {
            prev[i] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(i);
        cur = (prev[i] != usize::MAX).then(|| prev[i]);
    }
    out.reverse();
    out
}

/// Matches the records of `left` and `right` by the key columns of `opts`.
#[cfg(test)]
pub fn compare(
    left: &Table,
    right: &Table,
    opts: &TableMatch,
    cmp: &Compare,
) -> Result<TableDiff, Problem> {
    compare_with_stop(left, right, opts, cmp, &|_| false)
}

/// Records handled between two calls of the `stop` callback.
const STEP: usize = 4096;

/// [`compare`], calling `stop` with the number of records handled so far every few thousand
/// records; it returns [`Problem::Cancelled`] once `stop` returns true.
pub fn compare_with_stop(
    left: &Table,
    right: &Table,
    opts: &TableMatch,
    cmp: &Compare,
    stop: &dyn Fn(usize) -> bool,
) -> Result<TableDiff, Problem> {
    let tables = [left, right];
    if tables.iter().any(|t| t.records.len() > MAX_RECORDS) {
        return Err(Problem::TooLarge);
    }
    if !opts.header
        && (opts.columns == ColumnPolicy::Name
            || opts.keys.iter().any(|k| matches!(k, KeyColumn::Name(_))))
    {
        return Err(Problem::NoHeader);
    }
    let names = if opts.header {
        [header_names(left), header_names(right)]
    } else {
        [Vec::new(), Vec::new()]
    };
    let mut keys = [Vec::new(), Vec::new()];
    for (side, k) in keys.iter_mut().enumerate() {
        // An empty side has no records to key, nor a header to find the columns in.
        if tables[side].records.is_empty() {
            continue;
        }
        for key in &opts.keys {
            k.push(key_index(&names[side], key, side)?);
        }
    }
    let mut warnings = Vec::new();
    let columns = pair_columns(tables, &names, opts.columns, &mut warnings);
    let first = usize::from(opts.header);

    // Records handled, for `stop`: keys of both sides, then the left, then the right records.
    let mut done = 0;
    let mut step = |n: usize| {
        done += n;
        if stop(done) {
            Err(Problem::Cancelled)
        } else {
            Ok(())
        }
    };
    // Normalized keys; `None` when a key cell is missing or all of them are empty.
    let mut record_keys: [Vec<Option<Vec<String>>>; 2] = [Vec::new(), Vec::new()];
    for (side, out) in record_keys.iter_mut().enumerate() {
        let records = tables[side].records.get(first..).unwrap_or_default();
        out.reserve(records.len());
        for chunk in records.chunks(STEP) {
            out.extend(chunk.iter().map(|r| {
                let key: Option<Vec<String>> = keys[side]
                    .iter()
                    .map(|&c| r.cells.get(c).map(|v| (cmp.key)(v)))
                    .collect();
                key.filter(|k| k.iter().any(|v| !v.trim().is_empty()))
            }));
            step(chunk.len())?;
        }
    }
    let mut index: [HashMap<&[String], Vec<usize>>; 2] = [HashMap::new(), HashMap::new()];
    for side in 0..2 {
        for (i, key) in record_keys[side].iter().enumerate() {
            if i % STEP == STEP - 1 {
                step(0)?;
            }
            if let Some(key) = key {
                index[side]
                    .entry(key.as_slice())
                    .or_default()
                    .push(i + first);
            }
        }
    }
    let count = |side: usize, key: &[String]| index[side].get(key).map_or(0, Vec::len);

    let changed = |l: usize, r: usize| -> [Vec<usize>; 2] {
        let (a, b) = (&left.records[l].cells, &right.records[r].cells);
        let mut out = [Vec::new(), Vec::new()];
        for pair in &columns {
            let (va, vb) = (
                pair[0].and_then(|c| a.get(c)),
                pair[1].and_then(|c| b.get(c)),
            );
            let same = match (va, vb) {
                (Some(x), Some(y)) => same_value(x, y, opts, cmp),
                (None, None) => true,
                // A column that only one side has is not compared.
                _ => pair[0].is_none() || pair[1].is_none(),
            };
            if !same {
                out[0].extend(pair[0].filter(|&c| c < a.len()));
                out[1].extend(pair[1].filter(|&c| c < b.len()));
            }
        }
        out
    };

    let mut rows: Vec<MatchedRow> = Vec::new();
    let row = |records: [Option<usize>; 2], state: RowState| MatchedRow {
        records,
        state,
        moved: false,
        header: false,
        changed: [Vec::new(), Vec::new()],
    };
    if opts.header {
        let has = [!left.records.is_empty(), !right.records.is_empty()];
        let (state, ch) = match has {
            [true, true] => {
                let ch = changed(0, 0);
                let same = ch[0].is_empty() && ch[1].is_empty();
                let state = if same {
                    RowState::Identical
                } else {
                    RowState::Modified
                };
                (state, ch)
            }
            [true, false] => (RowState::Removed, Default::default()),
            [false, true] => (RowState::Added, Default::default()),
            [false, false] => (RowState::Identical, Default::default()),
        };
        if has[0] || has[1] {
            rows.push(MatchedRow {
                header: true,
                changed: ch,
                ..row([has[0].then_some(0), has[1].then_some(0)], state)
            });
        }
    }

    // Ambiguous records of the right side, in order, grouped like those of the left so the
    // n-th of a group is shown beside the n-th of the same group.
    let mut right_groups: HashMap<Option<&[String]>, std::collections::VecDeque<usize>> =
        HashMap::new();
    for (i, key) in record_keys[1].iter().enumerate() {
        let k = key.as_deref();
        if k.is_none_or(|k| count(0, k) > 1 || count(1, k) > 1) {
            right_groups.entry(k).or_default().push_back(i + first);
        }
    }
    let mut placed_right = vec![false; right.records.len()];
    if opts.header && !right.records.is_empty() {
        placed_right[0] = true;
    }
    let mut matched: Vec<usize> = Vec::new();
    for (i, key) in record_keys[0].iter().enumerate() {
        if i % STEP == STEP - 1 {
            step(STEP)?;
        }
        let l = i + first;
        let k = key.as_deref();
        match k {
            Some(k) if count(0, k) == 1 && count(1, k) == 1 => {
                let r = index[1][k][0];
                placed_right[r] = true;
                let ch = changed(l, r);
                let state = if ch[0].is_empty() && ch[1].is_empty() {
                    RowState::Identical
                } else {
                    RowState::Modified
                };
                matched.push(rows.len());
                rows.push(MatchedRow {
                    changed: ch,
                    ..row([Some(l), Some(r)], state)
                });
            }
            Some(k) if count(0, k) == 1 && count(1, k) == 0 => {
                rows.push(row([Some(l), None], RowState::Removed));
            }
            _ => {
                let r = right_groups.get_mut(&k).and_then(|g| g.pop_front());
                let ch = r.map(|r| changed(l, r)).unwrap_or_default();
                if let Some(r) = r {
                    placed_right[r] = true;
                }
                rows.push(MatchedRow {
                    changed: ch,
                    ..row([Some(l), r], RowState::Ambiguous)
                });
            }
        }
    }

    // Records only on the right go after the row of the right record before them.
    let mut after: Vec<Vec<MatchedRow>> = vec![Vec::new(); rows.len() + 1];
    let mut row_of_right = vec![None; right.records.len()];
    for (i, r) in rows.iter().enumerate() {
        if let Some(rr) = r.records[1] {
            row_of_right[rr] = Some(i);
        }
    }
    let mut last: Option<usize> = None;
    let header_slots = first.min(right.records.len());
    for (r, key) in std::iter::repeat_n(&None, header_slots)
        .chain(record_keys[1].iter())
        .enumerate()
    {
        if r % STEP == STEP - 1 {
            step(STEP)?;
        }
        if placed_right[r] {
            last = row_of_right[r].or(last);
            continue;
        }
        let k = key.as_deref();
        let state = match k {
            Some(k) if count(0, k) == 0 && count(1, k) == 1 => RowState::Added,
            _ => RowState::Ambiguous,
        };
        after[last.map_or(0, |i| i + 1)].push(row([None, Some(r)], state));
    }

    // Matched records out of their order: those off a longest run in the same order.
    let order: Vec<usize> = matched
        .iter()
        .map(|&i| rows[i].records[1].unwrap())
        .collect();
    let mut moved = vec![true; matched.len()];
    for i in longest_increasing(&order) {
        moved[i] = false;
    }
    for (&i, m) in matched.iter().zip(moved) {
        rows[i].moved = m;
    }

    let mut out = Vec::with_capacity(rows.len());
    let mut after = after.into_iter();
    out.extend(after.next().unwrap_or_default());
    for (r, extra) in rows.into_iter().zip(after) {
        out.push(r);
        out.extend(extra);
    }
    let key_of = |r: &MatchedRow| {
        (0..2).find_map(|side| {
            let i = r.records[side]?.checked_sub(first)?;
            record_keys[side].get(i)?.as_deref()
        })
    };
    reorder(&mut out, opts.order, key_of);
    let mut counts = Counts::default();
    let mut dup = [0usize; 2];
    let mut empty = [0usize; 2];
    for (side, keys) in record_keys.iter().enumerate() {
        for k in keys {
            match k {
                None => empty[side] += 1,
                Some(k) if count(side, k) > 1 => dup[side] += 1,
                _ => {}
            }
        }
    }
    for r in out.iter().filter(|r| !r.header) {
        match r.state {
            RowState::Identical => counts.identical += 1,
            RowState::Modified => counts.modified += 1,
            RowState::Added => counts.added += 1,
            RowState::Removed => counts.removed += 1,
            RowState::Ambiguous => counts.ambiguous += 1,
        }
        if r.moved {
            counts.moved += 1;
        }
    }
    for side in 0..2 {
        if dup[side] > 0 {
            warnings.push(Warning::DuplicateKeys {
                side,
                records: dup[side],
            });
        }
        if empty[side] > 0 {
            warnings.push(Warning::EmptyKeys {
                side,
                records: empty[side],
            });
        }
    }
    Ok(TableDiff {
        rows: out,
        columns,
        counts,
        warnings,
        ignore_order: opts.ignore_order,
    })
}

/// `content`, a report of a 2-way compare as `format`, with what matching by key found added
/// after its differences, which stay those of the text: `<table-match>` in XML, and `lines`
/// (a heading, then the settings, counts and warnings in words) in HTML and slideshows. Other
/// formats come back unchanged.
pub fn annotate_report(
    mut content: String,
    format: super::report::ReportFormat,
    opts: &TableMatch,
    found: (&Counts, &[Warning]),
    lines: &[String],
) -> String {
    use super::report::ReportFormat;
    use std::fmt::Write;
    let esc = super::xml_report::esc;
    match format {
        ReportFormat::Xml => {
            let columns = match opts.columns {
                ColumnPolicy::Position => "position",
                ColumnPolicy::Name => "name",
            };
            let order = match opts.order {
                RowOrder::Left => "left",
                RowOrder::Right => "right",
                RowOrder::Key => "key",
            };
            let mut x = format!(
                "  <table-match version=\"1\" header=\"{}\" columns=\"{}\" order=\"{}\" ignore-order=\"{}\" abs-tolerance=\"{}\" rel-tolerance=\"{}\">\n",
                opts.header, columns, order, opts.ignore_order,
                opts.abs_tolerance,
                opts.rel_tolerance,
            );
            for key in &opts.keys {
                let _ = match key {
                    KeyColumn::Name(n) => writeln!(x, "    <key name=\"{}\"/>", esc(n)),
                    KeyColumn::Index(i) => writeln!(x, "    <key column=\"{}\"/>", i + 1),
                };
            }
            let (c, warnings) = found;
            let _ = writeln!(
                x,
                "    <counts identical=\"{}\" added=\"{}\" removed=\"{}\" modified=\"{}\" moved=\"{}\" ambiguous=\"{}\"/>",
                c.identical, c.added, c.removed, c.modified, c.moved, c.ambiguous
            );
            for w in warnings {
                let _ = match w {
                    Warning::DuplicateHeader { side, column } => writeln!(
                        x,
                        "    <warning kind=\"duplicate_header\" side=\"{side}\" column=\"{}\"/>",
                        esc(column)
                    ),
                    Warning::UnpairedColumns { side, columns } => writeln!(
                        x,
                        "    <warning kind=\"unpaired_columns\" side=\"{side}\">{}</warning>",
                        columns
                            .iter()
                            .map(|c| format!("<column>{}</column>", esc(c)))
                            .collect::<String>()
                    ),
                    Warning::DuplicateKeys { side, records } => writeln!(
                        x,
                        "    <warning kind=\"duplicate_keys\" side=\"{side}\" records=\"{records}\"/>"
                    ),
                    Warning::EmptyKeys { side, records } => writeln!(
                        x,
                        "    <warning kind=\"empty_keys\" side=\"{side}\" records=\"{records}\"/>"
                    ),
                };
            }
            x.push_str("  </table-match>\n");
            let at = content.rfind("</comparison>").unwrap_or(content.len());
            content.insert_str(at, &x);
        }
        ReportFormat::Html | ReportFormat::Slideshow => {
            let mut h = String::from("<section class=\"table-match\">");
            let mut lines = lines.iter();
            if let Some(title) = lines.next() {
                let _ = write!(h, "<h3>{}</h3>", super::report::esc(title));
            }
            h.push_str("<ul>");
            for line in lines {
                let _ = write!(h, "<li>{}</li>", super::report::esc(line));
            }
            h.push_str("</ul></section>\n");
            let at = content
                .rfind("<p class=\"foot\">")
                .or_else(|| content.rfind("<section class=\"end\">"))
                .or_else(|| content.rfind("</main>"))
                .or_else(|| content.rfind("</body>"))
                .unwrap_or(content.len());
            content.insert_str(at, &h);
        }
        ReportFormat::Csv | ReportFormat::Text => {}
    }
    content
}

/// Puts `rows`, which are in left order, in the order `order` asks for.
fn reorder<'k>(
    rows: &mut Vec<MatchedRow>,
    order: RowOrder,
    key_of: impl Fn(&MatchedRow) -> Option<&'k [String]>,
) {
    match order {
        RowOrder::Left => {}
        RowOrder::Right => {
            // A row without a right record goes after the row of the left record before it,
            // or first.
            let mut anchor = 0;
            let mut keyed: Vec<((usize, usize, usize), MatchedRow)> = std::mem::take(rows)
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    let at = match (r.header, r.records[1]) {
                        (true, _) => (0, 0, 0),
                        (false, Some(right)) => {
                            if r.records[0].is_some() {
                                anchor = right + 1;
                            }
                            (right + 1, 0, i)
                        }
                        (false, None) => (anchor, 1, i),
                    };
                    (at, r)
                })
                .collect();
            keyed.sort_by_key(|(at, _)| *at);
            rows.extend(keyed.into_iter().map(|(_, r)| r));
        }
        RowOrder::Key => {
            let start = usize::from(rows.first().is_some_and(|r| r.header));
            // A stable sort keeps rows with the same key, and those without one, in left order.
            rows[start..].sort_by(|a, b| match (key_of(a), key_of(b)) {
                (Some(x), Some(y)) => compare_keys(x, y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
    }
}

/// Key values part by part: numbers by value before text, text by its characters.
fn compare_keys(a: &[String], b: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let number = |s: &str| s.trim().parse::<f64>().ok().filter(|x| x.is_finite());
    for (x, y) in a.iter().zip(b) {
        let o = match (number(x), number(y)) {
            (Some(p), Some(q)) => p.total_cmp(&q),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => x.cmp(y),
        };
        if o.is_ne() {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> Table {
        Table::parse(s.into(), ',').unwrap()
    }

    fn exact() -> (impl Fn(&str) -> String, impl Fn(&str, &str) -> bool) {
        (|s: &str| s.to_string(), |a: &str, b: &str| a == b)
    }

    fn run(a: &str, b: &str, opts: &TableMatch) -> Result<TableDiff, Problem> {
        let (key, eq) = exact();
        compare(&table(a), &table(b), opts, &Compare { key: &key, eq: &eq })
    }

    fn by(keys: &[&str]) -> TableMatch {
        TableMatch {
            header: true,
            keys: keys
                .iter()
                .map(|k| KeyColumn::Name(k.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn states(d: &TableDiff) -> Vec<(Option<usize>, Option<usize>, RowState, bool)> {
        d.rows
            .iter()
            .map(|r| (r.records[0], r.records[1], r.state, r.moved))
            .collect()
    }

    #[test]
    fn the_sample_project_matches_its_orders_by_key() {
        let file = std::path::Path::new("samples/tables/orders.oxmproj");
        let project = super::super::project::Project::load(file).unwrap();
        let opts = project.table.clone().unwrap();
        assert_eq!(opts.order, RowOrder::Key);
        let [a, b] = [0, 1]
            .map(|i| table(&std::fs::read_to_string(&project.resolved_paths(file)[i]).unwrap()));
        let (key, eq) = exact();
        let d = compare(&a, &b, &opts, &Compare { key: &key, eq: &eq }).unwrap();
        let c = d.counts;
        assert_eq!(
            (c.identical, c.modified, c.added, c.removed, c.ambiguous),
            (3, 1, 1, 1, 2),
            "{c:?}"
        );
        assert!(matches!(
            d.warnings[..],
            [Warning::DuplicateKeys { side: 1, .. }]
        ));
    }

    #[test]
    fn reordered_unique_keys_have_no_cell_changes() {
        let d = run(
            "id,name\n1,a\n2,b\n3,c\n",
            "id,name\n3,c\n1,a\n2,b\n",
            &by(&["id"]),
        )
        .unwrap();
        assert_eq!(d.counts.modified, 0);
        assert_eq!(
            d.counts.moved, 1,
            "only the record off the longest run moved"
        );
        assert_eq!(
            states(&d),
            [
                (Some(0), Some(0), RowState::Identical, false),
                (Some(1), Some(2), RowState::Identical, false),
                (Some(2), Some(3), RowState::Identical, false),
                (Some(3), Some(1), RowState::Identical, true),
            ]
        );
        assert_eq!(d.differences(), 1);
        let ignored = run(
            "id,name\n1,a\n2,b\n3,c\n",
            "id,name\n3,c\n1,a\n2,b\n",
            &TableMatch {
                ignore_order: true,
                ..by(&["id"])
            },
        )
        .unwrap();
        assert_eq!(ignored.differences(), 0);
        assert_eq!(ignored.counts.moved, 1, "a move is still counted");
    }

    #[test]
    fn one_changed_cell_among_moved_records_is_reported_once() {
        let d = run(
            "id,name,qty\n1,a,5\n2,b,6\n3,c,7\n",
            "id,name,qty\n2,b,6\n3,c,8\n1,a,5\n",
            &by(&["id"]),
        )
        .unwrap();
        assert_eq!(d.counts.modified, 1);
        let row = d
            .rows
            .iter()
            .find(|r| r.state == RowState::Modified)
            .unwrap();
        assert_eq!(row.records, [Some(3), Some(2)]);
        assert_eq!(row.changed, [vec![2], vec![2]]);
    }

    #[test]
    fn added_removed_and_moved_and_modified_records() {
        let d = run(
            "id,v\n1,a\n2,b\n3,c\n",
            "id,v\n4,d\n3,x\n1,a\n",
            &by(&["id"]),
        )
        .unwrap();
        assert_eq!(
            d.counts,
            Counts {
                identical: 1,
                added: 1,
                removed: 1,
                modified: 1,
                moved: 1,
                ambiguous: 0
            }
        );
        // Added "4" goes first: no right record comes before it.
        assert_eq!(d.rows[1].records, [None, Some(1)]);
        assert_eq!(d.rows[1].state, RowState::Added);
        let removed = d
            .rows
            .iter()
            .find(|r| r.state == RowState::Removed)
            .unwrap();
        assert_eq!(removed.records, [Some(2), None]);
    }

    #[test]
    fn composite_keys_do_not_collide_and_take_unicode_and_spaces() {
        let d = run(
            "a,b,v\n\"x|y\",z,1\nx,\"y|z\",2\n名, 前 ,3\n",
            "a,b,v\nx,\"y|z\",2\n\"x|y\",z,1\n名, 前 ,3\n",
            &by(&["a", "b"]),
        )
        .unwrap();
        assert_eq!(d.counts.modified, 0);
        assert_eq!(d.counts.ambiguous, 0);
        assert_eq!(d.counts.identical, 3);
        // An empty part of a composite key is allowed while another part is set.
        let d = run("a,b\n1,\n,2\n", "a,b\n,2\n1,\n", &by(&["a", "b"])).unwrap();
        assert_eq!(d.counts.identical, 2);
    }

    #[test]
    fn duplicate_and_empty_keys_are_ambiguous_and_never_matched_by_guess() {
        let d = run(
            "id,v\n1,a\n1,b\n,c\n2,d\n",
            "id,v\n1,b\n1,a\n,c\n2,d\n",
            &by(&["id"]),
        )
        .unwrap();
        assert_eq!(d.counts.ambiguous, 3);
        assert_eq!(d.counts.identical, 1);
        assert!(
            d.rows
                .iter()
                .filter(|r| r.state == RowState::Ambiguous)
                .count()
                == 3
        );
        assert!(d.warnings.contains(&Warning::DuplicateKeys {
            side: 0,
            records: 2
        }));
        assert!(d.warnings.contains(&Warning::EmptyKeys {
            side: 1,
            records: 1
        }));
        // A key unique on one side but repeated on the other is not matched either.
        let d = run("id,v\n1,a\n", "id,v\n1,a\n1,b\n", &by(&["id"])).unwrap();
        assert_eq!(d.counts.ambiguous, 2);
        assert_eq!(d.counts.identical, 0);
        // A record too short for its key column is ambiguous.
        let d = run("id,v\n1,a\nx\n", "v,id\na,1\n", &by(&["v"])).unwrap();
        assert!(d.rows.iter().any(|r| r.state == RowState::Ambiguous));
    }

    #[test]
    fn columns_pair_by_name_or_position_with_warnings() {
        let opts = TableMatch {
            columns: ColumnPolicy::Name,
            ..by(&["id"])
        };
        let d = run("id,a,b\n1,x,y\n", "b,id,c,a\n y,1,z,x\n", &opts).unwrap();
        assert_eq!(d.counts.identical, 0, "the leading space in b differs");
        let row = &d.rows[1];
        assert_eq!(row.changed, [vec![2], vec![0]]);
        assert_eq!(d.paired_column(0, 1, 1), Some(3));
        assert!(d.warnings.contains(&Warning::UnpairedColumns {
            side: 1,
            columns: vec!["c".into()]
        }));
        let d = run("id,a,a\n1,x,y\n", "id,a,a\n1,x,y\n", &opts).unwrap();
        assert!(d.warnings.contains(&Warning::DuplicateHeader {
            side: 0,
            column: "a".into()
        }));
        assert_eq!(d.counts.identical, 1);

        assert_eq!(
            run("id,v\n", "key,v\n", &by(&["id"])),
            Err(Problem::MissingKey {
                side: 1,
                column: "id".into()
            })
        );
        assert_eq!(
            run("id,id\n", "id\n", &by(&["id"])),
            Err(Problem::DuplicateKeyColumn {
                side: 0,
                column: "id".into()
            })
        );
        let no_header = TableMatch {
            keys: vec![KeyColumn::Index(1)],
            ..Default::default()
        };
        let d = run("a,1\nb,2\n", "b,2\na,1\n", &no_header).unwrap();
        assert_eq!(d.counts.identical, 2);
        assert!(d.rows.iter().all(|r| !r.header));
        assert_eq!(
            run(
                "a\n",
                "a\n",
                &TableMatch {
                    header: false,
                    ..by(&["a"])
                }
            ),
            Err(Problem::NoHeader)
        );
    }

    #[test]
    fn numeric_tolerances_are_opt_in_and_only_for_plain_numbers() {
        let a = "id,v\n1,10.0\n2,100\n3,NaN\n4,abc\n5,-0\n6,1e3\n";
        let b = "id,v\n1,10.05\n2,101\n3,NaN \n4,abd\n5,0\n6,1000.4\n";
        let d = run(a, b, &by(&["id"])).unwrap();
        assert_eq!(d.counts.modified, 6);
        let abs = TableMatch {
            abs_tolerance: 0.1,
            ..by(&["id"])
        };
        let d = run(a, b, &abs).unwrap();
        // 10.0/10.05 and -0/0 are within 0.1; 1e3/1000.4 and 100/101 are not.
        assert_eq!(d.counts.modified, 4);
        let rel = TableMatch {
            rel_tolerance: 0.01,
            ..by(&["id"])
        };
        let d = run(a, b, &rel).unwrap();
        // 1%: 10.0/10.05, 100/101, -0/0 and 1e3/1000.4; NaN and text stay different.
        assert_eq!(d.counts.modified, 2);
    }

    #[test]
    fn multiline_quoted_cells_and_line_endings_and_trailing_delimiters() {
        let d = run(
            "id,note,\r\n1,\"a\r\nb\",\r\n2,\"x,y\",\r\n",
            "id,note,\n2,\"x,y\",\n1,\"a\r\nb\",\n",
            &by(&["id"]),
        )
        .unwrap();
        assert_eq!(d.counts.modified, 0);
        assert_eq!(d.counts.moved, 1);
    }

    #[test]
    fn results_do_not_depend_on_hashing_and_scale_linearly() {
        let n = 10_000;
        let mut a = String::from("id,v\n");
        let mut b = String::from("id,v\n");
        for i in 0..n {
            a.push_str(&format!("{i},{}\n", i * 7));
        }
        for i in (0..n).rev() {
            b.push_str(&format!("{i},{}\n", i * 7));
        }
        let first = run(&a, &b, &by(&["id"])).unwrap();
        let again = run(&a, &b, &by(&["id"])).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.counts.modified, 0);
        assert_eq!(first.counts.identical, n);
        assert_eq!(first.counts.moved, n - 1);
    }

    #[test]
    fn an_empty_side_shows_the_other_header_and_records() {
        let d = run("", "id,v\n1,a\n", &by(&["id"])).unwrap();
        assert_eq!(
            states(&d),
            [
                (None, Some(0), RowState::Added, false),
                (None, Some(1), RowState::Added, false),
            ]
        );
        assert!(d.rows[0].header);
        let d = run("id,v\n1,a\n", "", &by(&["id"])).unwrap();
        assert_eq!(
            states(&d),
            [
                (Some(0), None, RowState::Removed, false),
                (Some(1), None, RowState::Removed, false),
            ]
        );
        assert!(run("", "", &by(&["id"])).unwrap().rows.is_empty());
    }

    #[test]
    fn columns_only_one_side_has_by_position_are_reported() {
        let d = run("id,v\n1,a\n", "id,v,w\n1,a,zzz\n", &by(&["id"])).unwrap();
        assert!(d.warnings.contains(&Warning::UnpairedColumns {
            side: 1,
            columns: vec!["w".into()]
        }));
    }

    #[test]
    fn settings_round_trip_and_default_is_by_position() {
        let opts = TableMatch {
            header: true,
            keys: vec![KeyColumn::Name("id".into()), KeyColumn::Index(2)],
            columns: ColumnPolicy::Name,
            ignore_order: true,
            abs_tolerance: 0.5,
            rel_tolerance: 0.0,
            order: RowOrder::Key,
        };
        let json = serde_json::to_string(&opts).unwrap();
        assert_eq!(serde_json::from_str::<TableMatch>(&json).unwrap(), opts);
        let old = serde_json::from_str::<TableMatch>("{}").unwrap();
        assert!(!old.by_key());
        assert_eq!(old.order, RowOrder::Left);
    }

    #[test]
    fn rows_show_in_left_right_or_key_order() {
        // 10 and 2 swapped, 7 only on the left, 5 only on the right, two records without a key.
        let (a, b) = ("id,v\n10,a\n2,b\n7,c\n,x\n", "id,v\n2,b\n5,e\n10,a\n,y\n");
        let ids = |order: RowOrder| {
            let d = run(
                a,
                b,
                &TableMatch {
                    order,
                    ..by(&["id"])
                },
            )
            .unwrap();
            assert!(d.rows[0].header);
            d.rows[1..]
                .iter()
                .map(|r| (r.records[0], r.records[1]))
                .collect::<Vec<_>>()
        };
        let left = ids(RowOrder::Left);
        assert_eq!(
            left,
            [
                (Some(1), Some(3)),
                (Some(2), Some(1)),
                (None, Some(2)),
                (Some(3), None),
                (Some(4), Some(4)),
            ]
        );
        // 7 follows 2, the left record before it, as 5 follows 2 in left order.
        assert_eq!(
            ids(RowOrder::Right),
            [
                (Some(2), Some(1)),
                (Some(3), None),
                (None, Some(2)),
                (Some(1), Some(3)),
                (Some(4), Some(4)),
            ]
        );
        // 2 < 5 < 7 < 10 by value, not as text; the records without a key last.
        assert_eq!(
            ids(RowOrder::Key),
            [
                (Some(2), Some(1)),
                (None, Some(2)),
                (Some(3), None),
                (Some(1), Some(3)),
                (Some(4), Some(4)),
            ]
        );
        // The same rows and counts whatever the order.
        let counts = |order| {
            run(
                a,
                b,
                &TableMatch {
                    order,
                    ..by(&["id"])
                },
            )
            .unwrap()
            .counts
        };
        assert_eq!(counts(RowOrder::Key), counts(RowOrder::Left));
        assert_eq!(counts(RowOrder::Right), counts(RowOrder::Left));
    }

    #[test]
    fn key_order_puts_numbers_before_text_so_any_keys_sort() {
        let keys = ["10", "9", "1a", "b", "-1", "1e1", "a"].map(|k| vec![k.to_string()]);
        let mut sorted = keys.to_vec();
        sorted.sort_by(|x, y| compare_keys(x, y));
        let flat: Vec<&str> = sorted.iter().map(|k| k[0].as_str()).collect();
        assert_eq!(flat, ["-1", "9", "10", "1e1", "1a", "a", "b"]);
    }

    #[test]
    fn matching_stops_when_asked_and_reports_progress() {
        let rows: String = (0..20_000).map(|i| format!("{i},v\n")).collect();
        let t = table(&format!("id,v\n{rows}"));
        let (key, eq) = exact();
        let cmp = Compare { key: &key, eq: &eq };
        let seen = std::cell::Cell::new(0);
        let stop = |done: usize| {
            seen.set(done);
            done >= 3 * STEP
        };
        let result = compare_with_stop(&t, &t, &by(&["id"]), &cmp, &stop);
        assert_eq!(result, Err(Problem::Cancelled));
        assert_eq!(seen.get(), 3 * STEP);
        let all = std::cell::Cell::new(0);
        let watch = |done: usize| {
            all.set(done);
            false
        };
        let d = compare_with_stop(&t, &t, &by(&["id"]), &cmp, &watch).unwrap();
        assert_eq!(d.counts.identical, 20_000);
        assert!(all.get() >= 40_000, "{}", all.get());
    }

    #[test]
    fn a_slideshow_shows_the_match_before_its_end_slide() {
        let show = "<main><section class=\"title\">t</section><section>d</section><section class=\"end\">e</section></main>";
        let out = annotate_report(
            show.into(),
            crate::core::report::ReportFormat::Slideshow,
            &by(&["id"]),
            (&Counts::default(), &[]),
            &["Matched by key".into()],
        );
        let at = |s: &str| out.find(s).unwrap();
        assert!(at("table-match") > at("<section>d") && at("table-match") < at("class=\"end\""));
    }
}
