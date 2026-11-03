//! Opt-in structured comparison of two files, or of three in [`three_way`]. Text ignore rules
//! are deliberately not applied. Comparing changes nothing; [`write`] copies a value from one
//! file into the other.
use super::analysis;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;
pub mod json;
pub mod three_way;
pub mod toml_doc;
pub mod write;
pub mod xml;
pub mod xml_tree;
pub mod yaml_doc;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Json,
    Xml,
    Toml,
    Yaml,
}
impl Format {
    /// The format of a file with this extension.
    pub fn of_extension(extension: &str) -> Option<Format> {
        match extension.to_ascii_lowercase().as_str() {
            "json" => Some(Format::Json),
            "xml" => Some(Format::Xml),
            "toml" => Some(Format::Toml),
            "yaml" | "yml" => Some(Format::Yaml),
            _ => None,
        }
    }
    /// Whether values have keys and arrays as in JSON, so an array matching key applies.
    pub fn keyed(self) -> bool {
        self != Format::Xml
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::Xml => "XML",
            Format::Toml => "TOML",
            Format::Yaml => "YAML",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    TypeChanged,
    Moved,
}
/// How array elements are paired between the two sides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    /// Pairs object elements of arrays by the value of this member instead of by position,
    /// when every element has a unique scalar value for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_key: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSpan {
    pub bytes: Range<usize>,
    pub line: usize,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: String,
    pub value: Option<String>,
    pub children: BTreeMap<String, Node>,
    pub span: SourceSpan,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub path: String,
    /// Where a moved element was, on the first side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub kind: ChangeKind,
    pub before: Option<String>,
    pub after: Option<String>,
    pub spans: [Option<SourceSpan>; 2],
    /// How the compared nodes were paired: `parser_exact` (by path alone), `array_key`,
    /// `reordered` (an exact reordering of the array) or `position` (arrays of different
    /// lengths, compared by position).
    pub confidence: String,
    /// Another reading of the same change is as likely: an element that may as well have
    /// stayed while others moved, an element that has duplicates, or a pairing by position
    /// that an insertion may have shifted. To be checked by hand.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ambiguous: bool,
    /// The path of the node on each side, or on a side without it, where it would go.
    #[serde(skip)]
    pub at: [String; 2],
    /// The structure of the node on each side, which equal values share however written.
    #[serde(skip)]
    pub prints: [Option<String>; 2],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticReport {
    pub version: u32,
    pub format: Format,
    pub changes: Vec<Change>,
    pub errors: Vec<String>,
    pub policies: Vec<String>,
    #[serde(default)]
    pub options: Options,
    /// Where the comparison fell back to a weaker pairing than asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}
impl SemanticReport {
    pub fn identical(&self) -> bool {
        self.errors.is_empty() && self.changes.is_empty()
    }
}
pub fn span(_source: &str, start: usize, end: usize) -> SourceSpan {
    SourceSpan {
        bytes: start..end,
        line: 0,
    }
}
/// Larger TOML and YAML input is left to the text view.
pub const MAX_CONFIG_BYTES: usize = 4 << 20;
/// Trailing or leading zeros a number's text writes out; past this it has an exponent, so the
/// text is never much longer than what the file wrote.
const MAX_WRITTEN_ZEROS: i64 = 20;
/// The text a TOML or YAML float written `raw` compares by: its exact decimal value, so `1`,
/// `1.0` and `1e0` are equal, `1e23` equals its 24-digit spelling, `-0.0` is `0`, `1e400` is
/// `10e+399` and `1e-400` is not `0`.
fn float_literal(raw: &str) -> String {
    let lower = raw.replace('_', "").to_ascii_lowercase();
    let (negative, unsigned) = match lower.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, lower.strip_prefix('+').unwrap_or(&lower)),
    };
    let exact = decimal_parts(unsigned)
        .and_then(|(digits, exponent)| decimal_text(negative, &digits, exponent));
    if let Some(text) = exact {
        return text;
    }
    match lower.parse::<f64>() {
        Ok(f) if f.is_nan() => "nan".into(),
        Ok(f) if f.is_infinite() && lower.contains("inf") => {
            if f > 0.0 { "inf" } else { "-inf" }.into()
        }
        // an exponent too large for 64 bits
        _ => lower.trim_start_matches('+').to_owned(),
    }
}
/// The digits and power of ten of the decimal float `unsigned`, such as `1.5e3`.
fn decimal_parts(unsigned: &str) -> Option<(String, i64)> {
    let (mantissa, exponent) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
    let exponent: i64 = exponent.parse().ok()?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((
        format!("{whole}{fraction}"),
        exponent.checked_sub(fraction.len() as i64)?,
    ))
}
/// The text the number `digits` × 10^`exponent` compares by, the same for every way of writing
/// it: without leading or trailing zeros beyond [`MAX_WRITTEN_ZEROS`], else with an exponent.
fn decimal_text(negative: bool, digits: &str, exponent: i64) -> Option<String> {
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0".into());
    }
    let significant = digits.trim_end_matches('0');
    let exponent = exponent.checked_add((digits.len() - significant.len()) as i64)?;
    let places = significant.len() as i64;
    let text = if (0..=MAX_WRITTEN_ZEROS).contains(&exponent) {
        format!("{significant}{}", "0".repeat(exponent as usize))
    } else if exponent < 0 && exponent >= -places {
        let (whole, fraction) = significant.split_at((places + exponent) as usize);
        format!("{}.{fraction}", if whole.is_empty() { "0" } else { whole })
    } else if exponent < 0 && exponent >= -(places + MAX_WRITTEN_ZEROS) {
        let zeros = "0".repeat((-exponent - places) as usize);
        format!("0.{zeros}{significant}")
    } else {
        format!("{significant}e{exponent}")
    };
    Some(if negative { format!("-{text}") } else { text })
}
pub fn parse(source: &str, format: Format) -> Result<Node> {
    if source.len() as u64 > analysis::MAX_FILE {
        bail!("Structured input exceeds 16 MiB");
    }
    let mut root = match format {
        Format::Json => json::parse(source)?,
        Format::Xml => xml::parse(source)?,
        Format::Toml => toml_doc::parse(source)?,
        Format::Yaml => yaml_doc::parse(source)?,
    };
    let lines: Vec<_> = std::iter::once(0)
        .chain(
            super::text::lines(source)
                .skip(1)
                .map(|(line, _)| line.as_ptr() as usize - source.as_ptr() as usize),
        )
        .collect();
    fn locate(node: &mut Node, lines: &[usize]) {
        node.span.line = lines.partition_point(|&start| start <= node.span.bytes.start);
        for child in node.children.values_mut() {
            locate(child, lines);
        }
    }
    locate(&mut root, &lines);
    Ok(root)
}
fn describe(node: &Node) -> String {
    node.value
        .clone()
        .unwrap_or_else(|| format!("{} ({} children)", node.kind, node.children.len()))
}
fn node_fingerprint(node: &Node) -> String {
    let children: Vec<_> = node
        .children
        .iter()
        .map(|(key, value)| (key, node_fingerprint(value)))
        .collect();
    analysis::identity(&(&node.kind, &node.value, children))
}
/// How the nodes being compared were paired, passed down to the changes below them.
#[derive(Clone, Copy)]
struct Pairing {
    confidence: &'static str,
    ambiguous: bool,
}
const BY_PATH: Pairing = Pairing {
    confidence: "parser_exact",
    ambiguous: false,
};
/// Notes kept at most in a report.
const MAX_NOTES: usize = 20;
/// A comparison in progress: the changes found so far and what is left of the report budget.
struct Walk<'a> {
    options: &'a Options,
    changes: Vec<Change>,
    notes: Vec<String>,
    budget: usize,
}
impl Walk<'_> {
    /// Records a change between `a` at `at[0]` and `b` at `at[1]`, charged against the report
    /// budget. Its path is where it is on the second side, or else where it was on the first.
    fn push(
        &mut self,
        at: [&str; 2],
        from: Option<String>,
        kind: ChangeKind,
        [a, b]: [Option<&Node>; 2],
        pairing: Pairing,
    ) -> Result<()> {
        let path = if b.is_some() { at[1] } else { at[0] };
        let cost = at[0].len()
            + at[1].len()
            + a.map_or(0, |node| node.span.bytes.len())
            + b.map_or(0, |node| node.span.bytes.len())
            + 384;
        if cost > self.budget {
            bail!("Semantic changes exceed the 32 MiB comparison budget; use Text view");
        }
        self.budget -= cost;
        let prints = [a.map(node_fingerprint), b.map(node_fingerprint)];
        self.changes.push(Change {
            id: analysis::identity(&(path, &from, &prints[0], &prints[1])),
            path: path.into(),
            from,
            kind,
            before: a.map(describe),
            after: b.map(describe),
            spans: [a.map(|n| n.span.clone()), b.map(|n| n.span.clone())],
            confidence: pairing.confidence.into(),
            ambiguous: pairing.ambiguous,
            at: at.map(str::to_owned),
            prints,
        });
        Ok(())
    }
    fn node(
        &mut self,
        at: [&str; 2],
        a: Option<&Node>,
        b: Option<&Node>,
        mut pairing: Pairing,
    ) -> Result<()> {
        if at.iter().any(|path| path.len() > 4096) {
            bail!("Semantic path exceeds 4 KiB; use Text view");
        }
        let kind = match (a, b) {
            (Some(a), Some(b)) if a.kind != b.kind => Some(ChangeKind::TypeChanged),
            (Some(a), Some(b)) if a.value != b.value => Some(ChangeKind::Modified),
            (None, Some(_)) => Some(ChangeKind::Added),
            (Some(_), None) => Some(ChangeKind::Removed),
            _ => None,
        };
        if let Some(kind) = kind {
            return self.push(at, None, kind, [a, b], pairing);
        }
        let (Some(a), Some(b)) = (a, b) else {
            return Ok(());
        };
        if a.kind == "array" {
            let (xs, ys) = (elements(a), elements(b));
            match array_pairs(&xs, &ys, self.options) {
                Pairs::Keyed(pairs) => {
                    pairing.confidence = "array_key";
                    return self.array(at, &xs, &ys, &pairs, pairing);
                }
                Pairs::Reordered(pairs) => {
                    pairing.confidence = "reordered";
                    return self.array(at, &xs, &ys, &pairs, pairing);
                }
                Pairs::Position => {
                    let records = || xs.iter().chain(&ys).any(|n| n.kind == "object");
                    if let Some(key) = &self.options.array_key {
                        if !xs.is_empty() && !ys.is_empty() && records() {
                            self.note(format!(
                                "Array matching key {key:?} not used at {}: not every \
                                 element has a unique scalar value for it, so elements are \
                                 compared by position.",
                                at[1]
                            ));
                        }
                    }
                    if xs.len() != ys.len() {
                        pairing = Pairing {
                            confidence: "position",
                            ambiguous: true,
                        };
                    }
                }
            }
        }
        for key in a
            .children
            .keys()
            .chain(b.children.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            self.node(
                [&format!("{}{key}", at[0]), &format!("{}{key}", at[1])],
                a.children.get(key),
                b.children.get(key),
                pairing,
            )?;
        }
        Ok(())
    }
    fn note(&mut self, note: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(note);
        }
    }
    /// Compares arrays whose elements are paired: an element out of the order of the others is
    /// moved, unpaired elements are added or removed, and paired elements are compared member
    /// by member. An unpaired element would go on the other side after the counterpart of the
    /// nearest paired element before it.
    fn array(
        &mut self,
        [path_a, path_b]: [&str; 2],
        xs: &[&Node],
        ys: &[&Node],
        pairs: &[(usize, usize)],
        pairing: Pairing,
    ) -> Result<()> {
        let mut pairs = pairs.to_vec();
        pairs.sort();
        let order: Vec<_> = pairs.iter().map(|&(_, j)| j).collect();
        let stay = increasing(&order);
        let could_stay = in_some_increasing(&order);
        let mut seen = BTreeMap::new();
        if pairing.confidence == "reordered" {
            for x in xs {
                *seen.entry(node_fingerprint(x)).or_insert(0usize) += 1;
            }
        }
        let mut by_new: Vec<_> = pairs
            .iter()
            .enumerate()
            .map(|(k, &(i, j))| (j, i, stay.contains(&k), could_stay.contains(&k)))
            .collect();
        by_new.sort();
        let paired_old: std::collections::BTreeSet<_> = pairs.iter().map(|&(i, _)| i).collect();
        let paired_new: std::collections::BTreeSet<_> = pairs.iter().map(|&(_, j)| j).collect();
        let mut next = by_new.iter().peekable();
        // one past the last first-side element paired so far
        let mut after = 0;
        for (j, y) in ys.iter().enumerate() {
            let here = format!("{path_b}[{j}]");
            if !paired_new.contains(&j) {
                self.node(
                    [&format!("{path_a}[{after}]"), &here],
                    None,
                    Some(y),
                    pairing,
                )?;
                continue;
            }
            let &(_, i, stays, could_stay) = next.next().expect("paired element");
            after = after.max(i + 1);
            let there = format!("{path_a}[{i}]");
            if !stays {
                let duplicated = seen.get(&node_fingerprint(xs[i])).is_some_and(|&n| n > 1);
                let moved = Pairing {
                    ambiguous: pairing.ambiguous || could_stay || duplicated,
                    ..pairing
                };
                self.push(
                    [&there, &here],
                    Some(there.clone()),
                    ChangeKind::Moved,
                    [Some(xs[i]), Some(y)],
                    moved,
                )?;
            }
            self.node([&there, &here], Some(xs[i]), Some(y), pairing)?;
        }
        let mut pairs = pairs.iter().peekable();
        let mut after = 0;
        for (i, x) in xs.iter().enumerate() {
            while let Some(&(_, j)) = pairs.next_if(|&&(paired, _)| paired < i) {
                after = after.max(j + 1);
            }
            if !paired_old.contains(&i) {
                let at = [format!("{path_a}[{i}]"), format!("{path_b}[{after}]")];
                self.node([&at[0], &at[1]], Some(x), None, pairing)?;
            }
        }
        Ok(())
    }
}
/// The elements of an array node, in order.
fn elements(node: &Node) -> Vec<&Node> {
    let mut items: Vec<_> = node
        .children
        .iter()
        .filter_map(|(key, child)| {
            let index = key
                .strip_prefix('[')?
                .strip_suffix(']')?
                .parse::<usize>()
                .ok()?;
            Some((index, child))
        })
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, child)| child).collect()
}
/// How the elements of two arrays are paired, as (first side, second side) indexes.
enum Pairs {
    /// By the value of the array matching key.
    Keyed(Vec<(usize, usize)>),
    /// As an exact reordering.
    Reordered(Vec<(usize, usize)>),
    /// By position.
    Position,
}
fn array_pairs(xs: &[&Node], ys: &[&Node], options: &Options) -> Pairs {
    if let Some(key) = &options.array_key {
        let Ok(member) = serde_json::to_string(key).map(|k| format!("[{k}]")) else {
            return Pairs::Position;
        };
        let keyed = |items: &[&Node]| -> Option<BTreeMap<String, usize>> {
            let mut out = BTreeMap::new();
            for (i, item) in items.iter().enumerate() {
                let value = item
                    .children
                    .get(&member)
                    .filter(|v| v.children.is_empty())?
                    .value
                    .clone()?;
                if out.insert(value, i).is_some() {
                    return None;
                }
            }
            Some(out)
        };
        if let (Some(ka), Some(kb)) = (keyed(xs), keyed(ys)) {
            if !xs.is_empty() && !ys.is_empty() {
                return Pairs::Keyed(
                    ka.iter()
                        .filter_map(|(value, &i)| kb.get(value).map(|&j| (i, j)))
                        .collect(),
                );
            }
        }
    }
    if xs.len() != ys.len() || xs.len() < 2 {
        return Pairs::Position;
    }
    let prints = |items: &[&Node]| {
        items
            .iter()
            .map(|n| node_fingerprint(n))
            .collect::<Vec<_>>()
    };
    let (pa, pb) = (prints(xs), prints(ys));
    if pa == pb {
        return Pairs::Position;
    }
    let mut sorted = (pa.clone(), pb.clone());
    sorted.0.sort();
    sorted.1.sort();
    if sorted.0 != sorted.1 {
        return Pairs::Position;
    }
    let mut free: BTreeMap<&str, std::collections::VecDeque<usize>> = BTreeMap::new();
    for (j, print) in pb.iter().enumerate() {
        free.entry(print).or_default().push_back(j);
    }
    let pairs: Option<Vec<_>> = pa
        .iter()
        .enumerate()
        .map(|(i, print)| Some((i, free.get_mut(print.as_str())?.pop_front()?)))
        .collect();
    pairs.map_or(Pairs::Position, Pairs::Reordered)
}
/// For each index of `seq`, the length of the longest strictly increasing subsequence ending
/// there.
fn increasing_lengths(seq: &[usize]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new();
    seq.iter()
        .map(|&value| {
            let at = tails.partition_point(|&t| t < value);
            if at == tails.len() {
                tails.push(value);
            } else {
                tails[at] = value;
            }
            at + 1
        })
        .collect()
}
/// The indexes of `seq`, of distinct values, in at least one longest strictly increasing
/// subsequence.
pub(crate) fn in_some_increasing(seq: &[usize]) -> std::collections::BTreeSet<usize> {
    let ending = increasing_lengths(seq);
    let reversed: Vec<usize> = seq.iter().rev().map(|&v| usize::MAX - v).collect();
    let starting: Vec<usize> = increasing_lengths(&reversed).into_iter().rev().collect();
    let longest = ending.iter().copied().max().unwrap_or(0);
    (0..seq.len())
        .filter(|&i| ending[i] + starting[i] - 1 == longest)
        .collect()
}
/// The indexes of `seq` in one longest strictly increasing subsequence.
pub(crate) fn increasing(seq: &[usize]) -> std::collections::BTreeSet<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; seq.len()];
    for (i, &value) in seq.iter().enumerate() {
        let at = tails.partition_point(|&t| seq[t] < value);
        if at > 0 {
            prev[i] = tails[at - 1];
        }
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut out = std::collections::BTreeSet::new();
    let mut at = tails.last().copied();
    while let Some(i) = at {
        out.insert(i);
        at = (prev[i] != usize::MAX).then(|| prev[i]);
    }
    out
}
/// The steps of a semantic path below `$`: `["key"]` and `[0]` for JSON, `/name[1]` and
/// `/@{uri}name` for XML.
pub fn segments(path: &str) -> Vec<&str> {
    let rest = path.strip_prefix('$').unwrap_or(path);
    let bytes = rest.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = start + 1;
        match bytes[start] {
            b'[' if bytes.get(start + 1) == Some(&b'"') => {
                end += 1;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                end = (end + 2).min(bytes.len());
            }
            b'[' => {
                while end < bytes.len() && bytes[end - 1] != b']' {
                    end += 1;
                }
            }
            _ => {
                let mut braces = 0usize;
                while end < bytes.len() {
                    match bytes[end] {
                        b'{' => braces += 1,
                        b'}' => braces = braces.saturating_sub(1),
                        b'/' if braces == 0 => break,
                        b'[' if braces == 0 && rest[end..].starts_with("[\"") => break,
                        _ => {}
                    }
                    end += 1;
                }
            }
        }
        let end = end.min(bytes.len());
        match rest.get(start..end) {
            Some(segment) => out.push(segment),
            None => {
                out.push(&rest[start..]);
                break;
            }
        }
        start = end;
    }
    out
}
/// Changes grouped by path: a node per step, with steps that hold nothing else joined up.
#[derive(Debug, Default)]
pub struct PathTree<'a> {
    pub label: String,
    pub changes: Vec<&'a Change>,
    pub children: Vec<PathTree<'a>>,
    /// Changes in this node and below it.
    pub count: usize,
}
impl<'a> PathTree<'a> {
    pub fn of(changes: impl IntoIterator<Item = &'a Change>) -> Self {
        let mut root = PathTree {
            label: "$".into(),
            ..Default::default()
        };
        for change in changes {
            let mut node = &mut root;
            node.count += 1;
            for segment in segments(&change.path) {
                let at = match node.children.iter().position(|c| c.label == segment) {
                    Some(at) => at,
                    None => {
                        node.children.push(PathTree {
                            label: segment.into(),
                            ..Default::default()
                        });
                        node.children.len() - 1
                    }
                };
                node = &mut node.children[at];
                node.count += 1;
            }
            node.changes.push(change);
        }
        fn join(node: &mut PathTree) {
            for child in &mut node.children {
                while child.changes.is_empty() && child.children.len() == 1 {
                    let only = child.children.pop().expect("one child");
                    child.label.push_str(&only.label);
                    child.changes = only.changes;
                    child.children = only.children;
                }
                join(child);
            }
        }
        join(&mut root);
        root
    }
}
pub fn compare(a: &str, b: &str, format: Format) -> SemanticReport {
    compare_with(a, b, format, &Options::default())
}
pub fn compare_with(a: &str, b: &str, format: Format, options: &Options) -> SemanticReport {
    let (a_source, b_source) = (a, b);
    let mut report = SemanticReport {
        version: 1,
        format,
        changes: Vec::new(),
        errors: Vec::new(),
        policies: vec![
            "Read-only; text comparison rules are not semantic path rules and are not applied.".into(),
            "JSON objects unordered; duplicate keys rejected. Arrays ordered: elements are compared by position, an exact reordering is reported as moves, and with an array matching key, object elements are paired by that member's unique scalar value. XML attributes unordered; child/mixed-content order and text whitespace significant; expanded namespace names; comments included. No external entities/DTDs.".into(),
            "TOML and YAML compare as JSON-shaped values: tables and mappings are unordered objects, numbers compare by value (1 equals 1.0), date-times with an offset by the moment they name, and comments and formatting are ignored. YAML follows the YAML 1.2 core schema (yes/no/on/off are strings) and its keys compare by value; aliases, several documents, complex keys and tags other than the core ones are refused. Input is limited to 4 MiB, 64 levels and 100,000 nodes.".into(),
        ],
        options: options.clone(),
        notes: Vec::new(),
    };
    let parsed = [parse(a, format), parse(b, format)];
    for (side, p) in parsed.iter().enumerate() {
        if let Err(e) = p {
            report
                .errors
                .push(format!("Side {}: {e:#}; use Text view", side + 1));
        }
    }
    if let [Ok(a), Ok(b)] = &parsed {
        let mut walk = Walk {
            options,
            changes: Vec::new(),
            notes: Vec::new(),
            budget: analysis::MAX_REPORT,
        };
        if let Err(error) = walk.node(["$", "$"], Some(a), Some(b), BY_PATH) {
            report.errors.push(error.to_string());
            return report;
        }
        report.changes = walk.changes;
        report.notes = walk.notes;
        for change in &mut report.changes {
            for (side, source) in [a_source, b_source].into_iter().enumerate() {
                let value = change.spans[side]
                    .as_ref()
                    .filter(|span| !span.bytes.is_empty())
                    .and_then(|span| source.get(span.bytes.clone()));
                if let Some(value) = value {
                    *if side == 0 {
                        &mut change.before
                    } else {
                        &mut change.after
                    } = Some(value.to_owned());
                }
            }
        }
    }
    report
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn added_containers_keep_values_and_structural_ids_ignore_object_order() {
        let a = compare("{}", r#"{"added":{"a":1,"b":2}}"#, Format::Json);
        let b = compare("{}", r#"{"added":{"b":2,"a":1}}"#, Format::Json);
        assert_eq!(a.changes[0].after.as_deref(), Some(r#"{"a":1,"b":2}"#));
        assert_eq!(a.changes[0].id, b.changes[0].id);
        let path = format!("{{\"{}\":1}}", "x".repeat(4097));
        assert!(!compare("{}", &path, Format::Json).errors.is_empty());
        let xml = format!("<x:r xmlns:x='{}'/>", "u".repeat(1025));
        assert!(parse(&xml, Format::Xml).is_err());
        let deep = format!("{}0{}", "[".repeat(70), "]".repeat(70));
        assert!(parse(&deep, Format::Json).is_err());
    }

    #[test]
    fn xml_rejects_invalid_names_characters_and_preserves_document_comments() {
        assert!(!compare("<!--old--><r/>", "<!--new--><r/>", Format::Xml).identical());
        assert!(compare(
            "<a:r xmlns:a='urn:a&amp;b'/>",
            "<b:r xmlns:b='urn:a&#38;b'/>",
            Format::Xml
        )
        .identical());
        assert!(!compare("<r/><!--old-->", "<r/><!--new-->", Format::Xml).identical());
        for source in [
            "<1bad/>",
            "<r>&#0;</r>",
            "<r a='&#0;'/>",
            "<r><?xml version='1.0'?></r>",
            "<r/><?XML data?>",
            "<![CDATA[ ]]><r/>",
            "<r><!--bad--comment--></r>",
        ] {
            assert!(parse(source, Format::Xml).is_err(), "{source}");
        }
    }

    #[test]
    fn json_reordering_types_duplicates_and_ordered_arrays() {
        assert!(compare(r#"{"b":2,"a":1}"#, r#"{"a":1,"b":2}"#, Format::Json).identical());
        let r = compare(r#"{"timeout":30}"#, r#"{"timeout":60}"#, Format::Json);
        assert_eq!(r.changes.len(), 1);
        assert_eq!(r.changes[0].path, "$[\"timeout\"]");
        assert_eq!(r.changes[0].spans[0].as_ref().unwrap().bytes, 11..13);
        let cr = compare("{\r\"timeout\":30\r}", "{\r\"timeout\":60\r}", Format::Json);
        assert_eq!(cr.changes[0].spans[0].as_ref().unwrap().line, 2);

        assert!(!compare("[1,2]", "[2,1]", Format::Json).identical());
        assert_eq!(
            compare("null", "\"null\"", Format::Json).changes[0].kind,
            ChangeKind::TypeChanged
        );
        assert!(!compare(r#"{"a":1,"a":2}"#, "{}", Format::Json)
            .errors
            .is_empty());
        assert!(!compare("{}", r#"{"a":null}"#, Format::Json).identical());
    }
    #[test]
    fn array_elements_move_by_matching_key_or_exact_reordering() {
        let kinds = |r: &SemanticReport| {
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            r.changes
                .iter()
                .map(|c| (c.path.clone(), c.from.clone(), c.kind.clone()))
                .collect::<Vec<_>>()
        };
        let moved = |path: &str, from: &str| (path.into(), Some(from.into()), ChangeKind::Moved);
        assert_eq!(
            kinds(&compare("[1,2,3]", "[3,1,2]", Format::Json)),
            [moved("$[0]", "$[2]")]
        );
        assert_eq!(
            kinds(&compare("[1,1,2]", "[2,1,1]", Format::Json)),
            [moved("$[0]", "$[2]")]
        );
        let a = r#"{"items":[{"id":1,"v":"a"},{"id":2,"v":"b"}]}"#;
        let b = r#"{"items":[{"id":3,"v":"c"},{"id":2,"v":"B"},{"id":1,"v":"a"}]}"#;
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let report = compare_with(a, b, Format::Json, &keyed);
        assert_eq!(
            kinds(&report),
            [
                (r#"$["items"][0]"#.into(), None, ChangeKind::Added),
                (r#"$["items"][1]["v"]"#.into(), None, ChangeKind::Modified),
                moved(r#"$["items"][2]"#, r#"$["items"][0]"#),
            ]
        );
        assert_eq!(report.options, keyed);
        assert_eq!(
            report.changes[2].before.as_deref(),
            Some(r#"{"id":1,"v":"a"}"#)
        );
        assert!(kinds(&compare(a, b, Format::Json))
            .iter()
            .all(|(_, from, _)| from.is_none()));
        for b in [
            r#"{"items":[{"id":2},{"id":2}]}"#,
            r#"{"items":[{"id":[2]},{"id":1}]}"#,
            r#"{"items":[{"id":2},{"name":1}]}"#,
        ] {
            assert!(
                kinds(&compare_with(a, b, Format::Json, &keyed))
                    .iter()
                    .all(|(_, _, kind)| kind != &ChangeKind::Moved),
                "{b}"
            );
        }
        assert!(compare_with("[1,2]", "[1,2]", Format::Json, &keyed).identical());
    }

    #[test]
    fn pairings_say_how_sure_they_are() {
        let flags = |r: &SemanticReport| {
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            r.changes
                .iter()
                .map(|c| (c.path.clone(), c.confidence.clone(), c.ambiguous))
                .collect::<Vec<_>>()
        };
        let flag =
            |path: &str, confidence: &str, ambiguous| (path.into(), confidence.into(), ambiguous);
        assert_eq!(
            flags(&compare("[1,2,3]", "[3,1,2]", Format::Json)),
            [flag("$[0]", "reordered", false)]
        );
        // either element may be the one that moved
        assert_eq!(
            flags(&compare("[1,2]", "[2,1]", Format::Json)),
            [flag("$[1]", "reordered", true)]
        );
        // the moved 1 may be either of the two
        assert_eq!(
            flags(&compare("[1,2,3,1]", "[1,1,2,3]", Format::Json)),
            [flag("$[1]", "reordered", true)]
        );
        assert_eq!(
            flags(&compare("[1,2]", "[1,5]", Format::Json)),
            [flag("$[1]", "parser_exact", false)]
        );
        let shifted = flags(&compare("[1,2,3]", "[0,1,2,3]", Format::Json));
        assert_eq!(shifted.len(), 4);
        assert!(shifted.iter().all(|(_, c, a)| c == "position" && *a));

        let keyed = Options {
            array_key: Some("id".into()),
        };
        let r = compare_with(
            r#"[{"id":1,"v":1},{"id":2},{"id":3}]"#,
            r#"[{"id":3},{"id":1,"v":2},{"id":2}]"#,
            Format::Json,
            &keyed,
        );
        assert_eq!(
            flags(&r),
            [
                flag("$[0]", "array_key", false),
                flag(r#"$[1]["v"]"#, "array_key", false),
            ]
        );
        assert!(r.notes.is_empty());
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("ambiguous") && !json.contains("notes"),
            "{json}"
        );

        // records the key cannot pair are noted; plain values are not records
        let r = compare_with(
            r#"{"items":[{"id":1},{"id":1}]}"#,
            r#"{"items":[{"id":1}]}"#,
            Format::Json,
            &keyed,
        );
        assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
        assert!(r.notes[0].contains(r#""id""#) && r.notes[0].contains(r#"$["items"]"#));
        assert!(r.changes.iter().all(|c| c.ambiguous));
        assert!(compare_with("[1,2]", "[1,3]", Format::Json, &keyed)
            .notes
            .is_empty());
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""ambiguous":true"#) && json.contains(r#""notes""#));
    }

    #[test]
    fn some_longest_increasing_run_holds_each_index_that_could_stay() {
        let set = |v: &[usize]| v.iter().copied().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(in_some_increasing(&[1, 2, 0]), set(&[0, 1]));
        assert_eq!(in_some_increasing(&[1, 0]), set(&[0, 1]));
        assert_eq!(in_some_increasing(&[0, 2, 3, 1]), set(&[0, 1, 2]));
        assert_eq!(in_some_increasing(&[2, 0, 3, 1]), set(&[0, 1, 2, 3]));
        assert!(in_some_increasing(&[]).is_empty());
    }

    /// xorshift64*: the same documents on every run and platform.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
    }
    #[derive(Clone, Debug)]
    enum Value {
        Null,
        Bool(bool),
        Number(String),
        Text(String),
        List(Vec<Value>),
        Map(Vec<(String, Value)>),
    }
    fn text(rng: &mut Rng) -> String {
        const CHARS: [&str; 14] = [
            "a", "b", "id", " ", "\"", "\\", "é", "漢", "😀", "\n", "\t", "/", "\u{1}", ".",
        ];
        (0..rng.below(5)).map(|_| *rng.pick(&CHARS)).collect()
    }
    fn number(rng: &mut Rng) -> String {
        let n = rng.below(2001) as i64 - 1000;
        if rng.below(4) == 0 {
            format!("{}", n as f64 / 4.0)
        } else {
            n.to_string()
        }
    }
    fn value(rng: &mut Rng, depth: usize) -> Value {
        let pick = match depth {
            0 => 4 + rng.below(3),
            1..4 => rng.below(7),
            _ => rng.below(4),
        };
        match pick {
            0 => Value::Null,
            1 => Value::Bool(rng.below(2) == 0),
            2 => Value::Number(number(rng)),
            3 => Value::Text(text(rng)),
            4 => Value::List((0..rng.below(5)).map(|_| value(rng, depth + 1)).collect()),
            _ => {
                let mut map: Vec<(String, Value)> = Vec::new();
                for _ in 0..rng.below(5) {
                    let key = if rng.below(3) == 0 {
                        "id".into()
                    } else {
                        text(rng)
                    };
                    if map.iter().all(|(k, _)| *k != key) {
                        map.push((key, value(rng, depth + 1)));
                    }
                }
                Value::Map(map)
            }
        }
    }
    /// JSON text of `v`, with object members in a random order and random spacing.
    fn emit(v: &Value, rng: &mut Rng, out: &mut String) {
        const SPACE: [&str; 5] = ["", "", " ", "\n  ", "\r\n\t"];
        let space = |rng: &mut Rng, out: &mut String| out.push_str(rng.pick(&SPACE));
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::Number(n) => out.push_str(n),
            Value::Text(s) => out.push_str(&serde_json::to_string(s).unwrap()),
            Value::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    space(rng, out);
                    emit(item, rng, out);
                }
                space(rng, out);
                out.push(']');
            }
            Value::Map(members) => {
                let mut order: Vec<usize> = (0..members.len()).collect();
                for i in (1..order.len()).rev() {
                    order.swap(i, rng.below(i + 1));
                }
                out.push('{');
                for (n, &i) in order.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    space(rng, out);
                    out.push_str(&serde_json::to_string(&members[i].0).unwrap());
                    out.push(':');
                    space(rng, out);
                    emit(&members[i].1, rng, out);
                }
                space(rng, out);
                out.push('}');
            }
        }
    }
    fn json(v: &Value, rng: &mut Rng) -> String {
        let mut out = String::new();
        emit(v, rng, &mut out);
        out
    }
    /// The semantic paths of the scalars in `v`.
    fn leaves(v: &Value, path: String, out: &mut Vec<String>) {
        match v {
            Value::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    leaves(item, format!("{path}[{i}]"), out);
                }
            }
            Value::Map(members) => {
                for (key, item) in members {
                    let key = serde_json::to_string(key).unwrap();
                    leaves(item, format!("{path}[{key}]"), out);
                }
            }
            _ => out.push(path),
        }
    }
    /// Changes the scalar at `path`; whether it keeps its type.
    fn edit(v: &mut Value, path: &str, at: &str, rng: &mut Rng) -> bool {
        match v {
            Value::List(items) => items.iter_mut().enumerate().any(|(i, item)| {
                let here = format!("{at}[{i}]");
                path.starts_with(&here) && edit(item, path, &here, rng)
            }),
            Value::Map(members) => members.iter_mut().any(|(key, item)| {
                let here = format!("{at}[{}]", serde_json::to_string(key).unwrap());
                path.starts_with(&here) && edit(item, path, &here, rng)
            }),
            _ if path != at => false,
            Value::Null => {
                *v = Value::Number(number(rng));
                true
            }
            Value::Bool(b) => {
                *b = !*b;
                true
            }
            Value::Number(n) => {
                *n = format!("{}", n.parse::<f64>().unwrap() + 1.0);
                true
            }
            Value::Text(s) => {
                s.push('x');
                true
            }
        }
    }
    /// Shuffles the first list of `v` with at least two elements; whether there was one.
    fn shuffle_a_list(v: &mut Value, rng: &mut Rng) -> bool {
        match v {
            Value::List(items) if items.len() > 1 => {
                for i in (1..items.len()).rev() {
                    items.swap(i, rng.below(i + 1));
                }
                true
            }
            Value::List(items) => items.iter_mut().any(|item| shuffle_a_list(item, rng)),
            Value::Map(members) => members
                .iter_mut()
                .any(|(_, item)| shuffle_a_list(item, rng)),
            _ => false,
        }
    }
    /// `source` with a few characters deleted, doubled or replaced by JSON punctuation.
    fn damage(source: &str, rng: &mut Rng) -> String {
        const BITS: [&str; 10] = ["{", "}", "[", "]", "\"", ",", ":", "\\", "0", "\u{0}"];
        let mut chars: Vec<String> = source.chars().map(String::from).collect();
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(chars.len() + 1);
            match rng.below(3) {
                0 if at < chars.len() => {
                    chars.remove(at);
                }
                1 if at < chars.len() => {
                    let c = chars[at].clone();
                    chars.insert(at, c);
                }
                _ => chars.insert(at, (*rng.pick(&BITS)).into()),
            }
        }
        chars.concat()
    }

    #[test]
    fn fixed_seed_fuzzing_keeps_the_invariants() {
        let mut rng = Rng(0x00DD_BA11_5EED);
        let keyed = Options {
            array_key: Some("id".into()),
        };
        // how much of the comparison the documents reach
        let (mut nested, mut edited_leaves, mut moves) = (0, 0, 0);
        for round in 0..400 {
            let doc = value(&mut rng, 0);
            let (a, shuffled) = (json(&doc, &mut rng), json(&doc, &mut rng));
            let same = compare(&a, &shuffled, Format::Json);
            assert!(same.identical(), "{round}: {a} | {shuffled}: {same:?}");
            // JSON is YAML 1.2 too
            let yaml = compare(&a, &shuffled, Format::Yaml);
            assert!(yaml.identical(), "{round}: {a} | {shuffled}: {yaml:?}");

            let mut paths = Vec::new();
            leaves(&doc, "$".into(), &mut paths);
            nested += usize::from(paths.len() > 3);
            if !paths.is_empty() {
                edited_leaves += 1;
                let path = rng.pick(&paths).clone();
                let mut edited = doc.clone();
                let same_type = !matches!(
                    crate::core::semantic::tests::find(&doc, &path),
                    Some(Value::Null)
                );
                assert!(edit(&mut edited, &path, "$", &mut rng), "{round}: {path}");
                let b = json(&edited, &mut rng);
                let r = compare(&a, &b, Format::Json);
                assert_eq!(r.changes.len(), 1, "{round}: {a} | {b}: {:?}", r.changes);
                let change = &r.changes[0];
                assert_eq!(change.path, path, "{round}: {a} | {b}");
                let kind = if same_type {
                    ChangeKind::Modified
                } else {
                    ChangeKind::TypeChanged
                };
                assert_eq!(change.kind, kind, "{round}: {a} | {b}");
                assert_eq!(change.confidence, "parser_exact");
                assert!(!change.ambiguous);
            }

            let mut reordered = doc.clone();
            if shuffle_a_list(&mut reordered, &mut rng) {
                let b = json(&reordered, &mut rng);
                let r = compare(&a, &b, Format::Json);
                moves += r.changes.len();
                assert!(
                    r.changes
                        .iter()
                        .all(|c| c.kind == ChangeKind::Moved && c.confidence == "reordered"),
                    "{round}: {a} | {b}: {:?}",
                    r.changes
                );
            }

            let other = json(&value(&mut rng, 0), &mut rng);
            for options in [Options::default(), keyed.clone()] {
                let there = compare_with(&a, &other, Format::Json, &options);
                let back = compare_with(&other, &a, Format::Json, &options);
                assert!(there.errors.is_empty() && back.errors.is_empty());
                assert_eq!(
                    there.changes.len(),
                    back.changes.len(),
                    "{round}: {a} | {other}"
                );
                for change in &there.changes {
                    let rest = change.path.strip_prefix('$').unwrap();
                    assert_eq!(segments(&change.path).concat(), rest, "{round}");
                    for (side, span) in change.spans.iter().enumerate() {
                        let source = [&a, &other][side];
                        if let Some(span) = span {
                            assert!(source.get(span.bytes.clone()).is_some(), "{round}");
                        }
                    }
                }
            }

            let damaged = damage(&a, &mut rng);
            let r = compare(&a, &damaged, Format::Json);
            if serde_json::from_str::<serde_json::Value>(&damaged).is_err() {
                assert!(!r.errors.is_empty(), "{round}: {damaged:?} was accepted");
            }
            for format in [Format::Xml, Format::Toml, Format::Yaml] {
                let _ = compare(&damaged, &a, format);
            }
        }
        assert!(
            nested > 80 && edited_leaves > 300,
            "{nested} {edited_leaves}"
        );
        assert!(moves > 100, "{moves} moves");
        let xml = "<?xml version='1.0'?><r xmlns:p='urn:p' a='1'><p:x b=\"2\">t&amp;<y/></p:x><!--c--><![CDATA[d]]></r>";
        let toml =
            "title = \"x\"\n[server]\nport = 8080\nhosts = [\"a\", \"b\"]\n[[items]]\nid = 1\n";
        let yaml = "server:\n  port: 8080\n  hosts: [a, b]\nitems:\n  - id: 1\n    v: \"x\"\n";
        for _ in 0..300 {
            for (source, format) in [
                (xml, Format::Xml),
                (toml, Format::Toml),
                (yaml, Format::Yaml),
            ] {
                let damaged = damage(source, &mut rng);
                let r = compare(source, &damaged, format);
                assert!(r.errors.is_empty() || !r.identical());
            }
        }
    }

    #[test]
    fn fixed_seed_fuzzing_copies_every_change_until_the_sides_agree() {
        let mut rng = Rng(0x0C0F_FEE5);
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let (mut copies, mut yaml_copies, mut yaml_refused) = (0, 0, 0);
        for round in 0..150 {
            let doc = value(&mut rng, 0);
            let mut other = doc.clone();
            let mut paths = Vec::new();
            leaves(&doc, "$".into(), &mut paths);
            for _ in 0..3 {
                if let Some(path) = paths.get(rng.below(paths.len().max(1))) {
                    edit(&mut other, path, "$", &mut rng);
                }
            }
            shuffle_a_list(&mut other, &mut rng);
            let fresh = value(&mut rng, 0);
            for b in [json(&other, &mut rng), json(&fresh, &mut rng)] {
                for options in [Options::default(), keyed.clone()] {
                    for from in [0, 1] {
                        let mut docs = [json(&doc, &mut rng), b.clone()];
                        let mut steps = 0;
                        loop {
                            let r = compare_with(&docs[0], &docs[1], Format::Json, &options);
                            assert!(r.errors.is_empty(), "{round}: {:?}", r.errors);
                            let Some(change) =
                                r.changes.iter().find(|c| c.kind != ChangeKind::Moved)
                            else {
                                break;
                            };
                            let copied = write::copy_change(
                                change,
                                [&docs[0], &docs[1]],
                                Format::Json,
                                from,
                            )
                            .unwrap_or_else(|e| panic!("{round}: {e:?} {change:?}\n{docs:?}"));
                            docs[1 - from] = copied;
                            copies += 1;
                            steps += 1;
                            assert!(steps < 200, "{round}: {docs:?}");
                        }
                        // JSON is YAML too, where an escaped string has no span to copy
                        let mut docs = [json(&doc, &mut rng), b.clone()];
                        for _ in 0..200 {
                            let r = compare_with(&docs[0], &docs[1], Format::Yaml, &options);
                            let Some(change) =
                                r.changes.iter().find(|c| c.kind != ChangeKind::Moved)
                            else {
                                break;
                            };
                            match write::copy_change(
                                change,
                                [&docs[0], &docs[1]],
                                Format::Yaml,
                                from,
                            ) {
                                Ok(text) => {
                                    docs[1 - from] = text;
                                    yaml_copies += 1;
                                }
                                Err(write::Refusal::NoEdit) => {
                                    yaml_refused += 1;
                                    break;
                                }
                                Err(e) => panic!("{round}: {e:?} {change:?}\n{docs:?}"),
                            }
                        }
                    }
                }
            }
        }
        assert!(copies > 1000, "{copies} copies");
        assert!(
            yaml_copies > 10 * yaml_refused,
            "{yaml_copies} copies, {yaml_refused} refused"
        );
    }

    /// The scalar of `v` at the semantic `path`.
    fn find<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
        let mut at = v;
        for segment in segments(path) {
            let inner = segment.strip_prefix('[')?.strip_suffix(']')?;
            at = match at {
                Value::List(items) => items.get(inner.parse::<usize>().ok()?)?,
                Value::Map(members) => {
                    let key: String = serde_json::from_str(inner).ok()?;
                    &members.iter().find(|(k, _)| *k == key)?.1
                }
                _ => return None,
            };
        }
        Some(at)
    }

    /// A YAML block mapping nested `depth` levels deep.
    fn nested_block(depth: usize) -> String {
        (0..depth)
            .map(|i| format!("{}k{i}:\n", "  ".repeat(i)))
            .collect::<String>()
            + &format!("{}v: 1\n", "  ".repeat(depth))
    }

    #[test]
    fn toml_and_yaml_compare_values_and_ignore_formatting() {
        let toml = "title = \"x\"\n[server]\nport = 8_080\nhosts = [\"a\", \"b\"]\n";
        assert!(compare(
            toml,
            "# settings\ntitle = 'x'\n\n[server]\nhosts = [ \"a\", \"b\" ]\nport = 8080\n",
            Format::Toml
        )
        .identical());
        let r = compare(
            toml,
            "title = \"x\"\n[server]\nport = 0x1F90\nhosts = [\"b\", \"a\"]\n",
            Format::Toml,
        );
        assert_eq!(r.changes.len(), 1, "{:?}", r.changes);
        assert_eq!(r.changes[0].kind, ChangeKind::Moved);
        assert_eq!(r.changes[0].path, r#"$["server"]["hosts"][1]"#);
        let r = compare(toml, &toml.replace("8_080", "9090"), Format::Toml);
        assert_eq!(r.changes[0].path, r#"$["server"]["port"]"#);
        assert_eq!(r.changes[0].before.as_deref(), Some("8_080"));
        assert_eq!(r.changes[0].spans[1].as_ref().unwrap().line, 3);
        for bad in ["a = 1\na = 2\n", "a = \n", "[a]\nb = 1\n[a]\n"] {
            let r = compare(bad, "a = 1\n", Format::Toml);
            assert!(!r.errors.is_empty(), "{bad}");
            assert!(r.errors[0].contains("TOML line"), "{:?}", r.errors);
        }

        let yaml = "server:\n  port: 8080\n  hosts: [a, b]\nflag: yes\n";
        assert!(compare(
            yaml,
            "flag: \"yes\"\nserver:\n  hosts:\n    - a\n    - b\n  port: 0x1F90 # same\n",
            Format::Yaml
        )
        .identical());
        let r = compare(yaml, &yaml.replace("8080", "9090"), Format::Yaml);
        assert_eq!(r.changes.len(), 1);
        assert_eq!(r.changes[0].path, r#"$["server"]["port"]"#);
        assert_eq!(r.changes[0].before.as_deref(), Some("8080"));
        assert_eq!(r.changes[0].spans[0].as_ref().unwrap().line, 2);
        let r = compare("名前: 値\nx: 1\n", "名前: 値\nx: 2\n", Format::Yaml);
        assert_eq!(r.changes[0].before.as_deref(), Some("1"));
        assert_eq!(r.changes[0].spans[1].as_ref().unwrap().line, 2);
        assert_eq!(
            compare("a: 1\n", "a: '1'\n", Format::Yaml).changes[0].kind,
            ChangeKind::TypeChanged
        );
        assert!(compare("a: !!str 1\n", "a: '1'\n", Format::Yaml).identical());
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let r = compare_with(
            "- id: 1\n  v: a\n- id: 2\n",
            "- id: 2\n- id: 1\n  v: b\n",
            Format::Yaml,
            &keyed,
        );
        assert!(r.changes.iter().any(|c| c.kind == ChangeKind::Moved));
        let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
        let big = format!("a: \"{}\"\n", "x".repeat(MAX_CONFIG_BYTES));
        let block = nested_block(80);
        for (bad, reason) in [
            ("a: &x 1\nb: *x\n", "aliases"),
            ("a: 1\n---\nb: 2\n", "documents"),
            ("? [a]\n: 1\n", "Complex"),
            ("a: !custom 1\n", "tag"),
            ("a: 1\na: 2\n", "Duplicate"),
            (deep.as_str(), "limit exceeded"),
            (block.as_str(), "nesting"),
            (big.as_str(), "4 MiB"),
        ] {
            let r = compare(bad, "a: 1\n", Format::Yaml);
            assert!(
                r.errors.iter().any(|e| e.contains(reason)),
                "{reason}: {:?}",
                r.errors
            );
        }
        assert_eq!(Format::of_extension("YML"), Some(Format::Yaml));
        assert!(!Format::Xml.keyed());
    }

    #[test]
    fn toml_and_yaml_numbers_and_dates_compare_by_value() {
        for (a, b) in [
            ("a = 1\n", "a = 1.0\n"),
            ("a = 0.0\n", "a = -0.0\n"),
            ("a = inf\n", "a = +inf\n"),
            ("a = nan\n", "a = -nan\n"),
            ("a = 1e3\n", "a = 1000\n"),
            ("a = 1e400\n", "a = 10e+399\n"),
            ("a = 1.5e1\n", "a = 15\n"),
            (
                "t = 1979-05-27T07:32:00Z\n",
                "t = 1979-05-27T07:32:00.000+00:00\n",
            ),
            (
                "t = 1979-05-27T07:32:00Z\n",
                "t = 1979-05-27T09:32:00+02:00\n",
            ),
            (
                "t = 1979-05-27T00:10:00+01:00\n",
                "t = 1979-05-26T23:10:00Z\n",
            ),
            ("t = 07:32:00\n", "t = 07:32:00.0\n"),
        ] {
            let r = compare(a, b, Format::Toml);
            assert!(r.identical(), "{a} {b} {:?} {:?}", r.changes, r.errors);
        }
        assert!(!compare("t = 07:32:00\n", "t = 07:32:01\n", Format::Toml).identical());
        assert!(!compare("a = 1e400\n", "a = 2e400\n", Format::Toml).identical());
        let r = compare("a = 9223372036854775808\n", "a = 1\n", Format::Toml);
        assert!(r.errors[0].contains("out of range"), "{:?}", r.errors);
        for (a, b) in [
            ("a: 1\n", "a: 1.0\n"),
            ("a: -0.0\n", "a: 0\n"),
            ("a: 1e3\n", "a: 1000\n"),
            ("a: !!float 1\n", "a: 1.0\n"),
            ("a: 1e23\n", "a: 100000000000000000000000\n"),
            ("a: 1e38\n", "a: 100000000000000000000000000000000000000\n"),
            ("a: 1e400\n", "a: 1e+400\n"),
            ("a: -0.5e1\n", "a: -5\n"),
            ("a: 0.10\n", "a: 0.1\n"),
            ("a: 1.5e-3\n", "a: 0.0015\n"),
            ("a: 1e-30\n", "a: 0.000000000000000000000000000001\n"),
            ("0x10: a\n", "16: a\n"),
            ("~: a\n", "null: a\n"),
            ("a: ! 1\n", "a: '1'\n"),
        ] {
            let r = compare(a, b, Format::Yaml);
            assert!(r.identical(), "{a} {b} {:?} {:?}", r.changes, r.errors);
        }
        let big = "a: 170141183460469231731687303715884105728\n";
        assert!(!compare(big, &big.replace("28\n", "29\n"), Format::Yaml).identical());
        assert_eq!(
            compare(big, "a: '1'\n", Format::Yaml).changes[0].kind,
            ChangeKind::TypeChanged
        );
        assert_eq!(
            compare("a: 1e400\n", "a: '1e400'\n", Format::Yaml).changes[0].kind,
            ChangeKind::TypeChanged
        );
        assert!(!compare("a: 1e400\n", "a: 2e400\n", Format::Yaml).identical());
        assert!(!compare("a: 0.1\n", "a: 0.2\n", Format::Yaml).identical());
        let long = format!("a: 1{}\n", "0".repeat(1000));
        assert!(compare("a: 1e1000\n", &long, Format::Yaml).identical());
        for (a, b) in [
            ("a: 1e-400\n", "a: 0\n"),
            ("a: 1e-400\n", "a: 2e-400\n"),
            ("a: 1.00000000000000001\n", "a: 1\n"),
            ("a: 1.00000000000000001\n", "a: 1.00000000000000002\n"),
        ] {
            assert!(!compare(a, b, Format::Yaml).identical(), "{a} {b}");
        }
        // Exponents at the ends of 64 bits neither overflow nor write out their zeros.
        for huge in [
            "a: 1e9223372036854775807\n",
            "a: 10e9223372036854775807\n",
            "a: 1e-9223372036854775808\n",
            "1e9223372036854775807: x\n",
        ] {
            let r = compare(huge, "a: 1\n", Format::Yaml);
            assert!(
                r.errors.is_empty() && !r.identical(),
                "{huge} {:?}",
                r.errors
            );
            let toml = huge.replace(": ", " = ");
            let r = compare(&toml, "a = 1\n", Format::Toml);
            assert!(!r.identical(), "{toml}");
        }
        assert_eq!(float_literal("1e400"), "1e400");
        assert_eq!(float_literal("-1.50e2"), "-150");
        assert_eq!(float_literal("0.001"), "0.001");
        assert_eq!(float_literal("1e-25"), "1e-25");
        let r = compare("0x10: a\n\"16\": b\n", "a: 1\n", Format::Yaml);
        assert!(
            r.errors.iter().any(|e| e.contains("0x10 and \"16\"")),
            "{:?}",
            r.errors
        );
        assert!(compare("", "# only a comment\n", Format::Yaml).identical());
        assert!(compare("", "a: 1\n", Format::Yaml).errors.is_empty());
    }

    #[test]
    fn yaml_tags_other_than_the_core_ones_are_refused_everywhere() {
        for bad in [
            "a: !custom {k: v}\n",
            "a: !!set {x, y}\n",
            "a: !!omap [a]\n",
            "a: !custom\n  k: v\n",
            "a: !custom\n- 1\n",
            "!binary k: v\n",
            "a: !!int abc\n",
            "a: !!int 1.5\n",
            "a: !!bool yes\n",
            "a: !!int\n",
            "1: a\n01: b\n",
            "a: !!float 0x10\n",
            "a: !!float 0o7\n",
            "- !custom\n- 1\n",
        ] {
            let r = compare(bad, "a: 1\n", Format::Yaml);
            assert!(!r.errors.is_empty(), "{bad}");
        }
        for good in [
            "a: !!map {k: v}\n",
            "a: !!seq [1]\n",
            "a: !!str\n",
            "a: !!null\n",
            "- !!null\n- 1\n",
            "- !!str\n- x\n",
            "a: !!seq\n- 1\n",
            "a: &x !!seq\n- 1\n",
            "a:\n  - !!null\n  - b: !!str\n    c: 1\n",
        ] {
            let r = compare(good, "a: 1\n", Format::Yaml);
            assert!(r.errors.is_empty(), "{good}: {:?}", r.errors);
        }
    }

    #[test]
    fn changed_tables_and_mappings_show_their_members() {
        let r = compare(
            "[a]\nx = 1\n",
            "[a]\nx = 1\n\n[b]\ny = 2\nz = 3\n",
            Format::Toml,
        );
        assert_eq!(r.changes[0].after.as_deref(), Some("[b]\ny = 2\nz = 3"));
        let r = compare("x = 1\n", "x = 1\na.b = 2\n", Format::Toml);
        assert_eq!(r.changes[0].after.as_deref(), Some("a.b = 2"));
        let r = compare(
            "[[w]]\nn = 1\n",
            "[[w]]\nn = 1\n[[w]]\nn = 2\nm = 3\n",
            Format::Toml,
        );
        assert_eq!(r.changes[0].after.as_deref(), Some("[[w]]\nn = 2\nm = 3"));
        let r = compare("a: [x]\n", "a: [x, k: 1, y]\n", Format::Yaml);
        assert!(
            r.changes.iter().any(|c| c.after.as_deref() == Some("k: 1")),
            "{:?}",
            r.changes
        );
        let r = compare(
            "a:\n  x: 1\nb: 2\n",
            "a:\n  x: 1\n  y: 2\n# about b\nb: 2\n",
            Format::Yaml,
        );
        assert_eq!(r.changes[0].after.as_deref(), Some("2"));
        let r = compare(
            "b: 2\n",
            "a:\n  x: 1\n  y: 2\n# about b\nb: 2\n",
            Format::Yaml,
        );
        assert_eq!(r.changes[0].after.as_deref(), Some("x: 1\n  y: 2"));
    }

    #[test]
    fn toml_and_yaml_samples_differ_only_in_their_settings() {
        let changes = |a: &str, b: &str, format, key: Option<&str>| {
            let options = Options {
                array_key: key.map(str::to_owned),
            };
            let r = compare_with(a, b, format, &options);
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            r.changes
                .into_iter()
                .map(|c| (format!("{:?}", c.kind), c.path, c.confidence))
                .collect::<Vec<_>>()
        };
        let toml = [
            include_str!("../../samples/left/config/service.toml"),
            include_str!("../../samples/right/config/service.toml"),
        ];
        let by_position = changes(toml[0], toml[1], Format::Toml, None);
        assert_eq!(by_position.len(), 7, "{by_position:?}");
        assert!(by_position
            .iter()
            .filter(|(_, path, _)| path.starts_with(r#"$["workers"]"#))
            .all(|(_, _, confidence)| *confidence == "position"));
        let kinds = |changes: Vec<(String, String, String)>| {
            changes
                .into_iter()
                .map(|(kind, path, _)| format!("{kind} {path}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(changes(toml[0], toml[1], Format::Toml, Some("id"))),
            [
                r#"Added $["features"]["audit"]"#,
                r#"Modified $["server"]["timeout_s"]"#,
                r#"Modified $["workers"][0]["threads"]"#,
                r#"Moved $["workers"][1]"#,
                r#"Added $["workers"][2]"#,
            ]
        );
        let yaml = [
            include_str!("../../samples/left/config/deploy.yaml"),
            include_str!("../../samples/right/config/deploy.yaml"),
        ];
        let expected = [
            r#"Modified $["env"]["LOG_LEVEL"]"#,
            r#"Moved $["ports"][1]"#,
            r#"Modified $["replicas"]"#,
        ];
        assert_eq!(
            kinds(changes(yaml[0], yaml[1], Format::Yaml, None)),
            expected
        );
        assert_eq!(
            kinds(changes(yaml[0], yaml[1], Format::Yaml, Some("name"))),
            expected
        );
    }

    #[test]
    fn paths_split_into_json_and_xml_segments() {
        assert_eq!(
            segments(r#"$["a"]["b]\"c"][3]"#),
            [r#"["a"]"#, r#"["b]\"c"]"#, "[3]"]
        );
        assert_eq!(
            segments("$/node()[1]/@{urn:a/b}x"),
            ["/node()[1]", "/@{urn:a/b}x"]
        );
        assert!(segments("$").is_empty());
        assert_eq!(segments("$[\"unterminated"), ["[\"unterminated"]);
        let report = compare(
            r#"{"a":{"b":{"c":1,"d":2}},"e":1}"#,
            r#"{"a":{"b":{"c":10,"d":20}},"e":2}"#,
            Format::Json,
        );
        let tree = PathTree::of(&report.changes);
        assert_eq!(tree.count, 3);
        let labels: Vec<_> = tree
            .children
            .iter()
            .map(|c| (c.label.as_str(), c.count, c.changes.len()))
            .collect();
        assert_eq!(labels, [(r#"["a"]["b"]"#, 2, 0), (r#"["e"]"#, 1, 1)]);
        assert_eq!(tree.children[0].children[1].label, r#"["d"]"#);
    }

    #[test]
    fn xml_namespaces_attributes_mixed_content_and_hostile_entities() {
        assert!(compare("<r a='1' b='2'/>", "<r b='2' a='1'></r>", Format::Xml).identical());
        assert!(compare(
            "<p:r xmlns:p='urn:a'/>",
            "<q:r xmlns:q='urn:a'/>",
            Format::Xml
        )
        .identical());
        assert!(!compare("<r>a<b/>c</r>", "<r>ac<b/></r>", Format::Xml).identical());
        assert!(!compare("<r><!--old--></r>", "<r><!--new--></r>", Format::Xml).identical());
        for xml in [
            "<!DOCTYPE r [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><r>&x;</r>",
            "<r>&unknown;</r>",
            "<a><b></a>",
            "<a/><b/>",
        ] {
            assert!(
                !compare(xml, "<r/>", Format::Xml).errors.is_empty(),
                "{xml}"
            );
        }
    }
}
