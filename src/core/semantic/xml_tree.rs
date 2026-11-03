//! The Tree view of two XML documents: one logical tree whose rows pair a node of the first
//! document with its counterpart in the second, or with nothing where a node was added or
//! removed. Read-only: building it parses with [`super::xml`] and writes nothing.
//!
//! Pairing works sibling group by sibling group below paired parents, so a node never moves
//! to another parent. In a group, siblings are paired, in this order:
//!
//! 1. by a key rule the user chose (`book@id`), when every element it covers on both sides has
//!    a unique, complete key; with a missing or duplicate key the rule is not used for that
//!    group, a warning says so, and the elements it covers are marked ambiguous;
//! 2. as identical subtrees, keeping their order (a longest common subsequence);
//! 3. as identical subtrees out of order, when such a subtree occurs once on each side — the
//!    only moves found without a key;
//! 4. by kind and name between the pairs found so far, again keeping order. A group where
//!    more such siblings are on one side than on the other may be read another way, so those
//!    pairs are marked ambiguous.
//!
//! Comparison follows the semantic view: element and attribute names are expanded names,
//! attributes are unordered, child order and text whitespace are significant, CDATA is text,
//! and comments and processing instructions are compared. Its counts are of the aligned rows,
//! which can be fewer than the semantic view's path-by-path changes: an inserted sibling is
//! one addition here, where comparing by position also changes every later sibling.
use super::{Format, Node};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XKind {
    Element,
    Text,
    Comment,
    Pi,
}

#[derive(Clone, Debug)]
pub struct XAttr {
    /// `{uri}local`, which attributes are compared by.
    pub name: String,
    /// The name as written, with its prefix.
    pub display: String,
    pub value: String,
    pub bytes: Range<usize>,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug)]
pub struct XNode {
    pub kind: XKind,
    /// `{uri}local` of an element; empty for other nodes.
    pub name: String,
    /// The element name as written, with its prefix.
    pub display: String,
    /// Text, comment or processing-instruction content; empty for elements.
    pub value: String,
    /// In source order.
    pub attrs: Vec<XAttr>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    pub bytes: Range<usize>,
    /// 1-based, as in the Text view.
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    /// Equal for subtrees that compare equal.
    pub print: u128,
}

impl XNode {
    pub fn local(&self) -> &str {
        local(&self.name)
    }
    fn blank(&self) -> bool {
        self.kind == XKind::Text
            && self
                .value
                .chars()
                .all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
    }
}

fn local(expanded: &str) -> &str {
    expanded.rsplit_once('}').map_or(expanded, |(_, l)| l)
}

/// One XML document as nodes with ids, which hold only while it stays loaded.
#[derive(Clone, Debug, Default)]
pub struct XmlDoc {
    pub nodes: Vec<XNode>,
    /// Comments and processing instructions before the root, the root, and those after it.
    pub top: Vec<usize>,
}

impl XmlDoc {
    /// Parses `source` with the semantic XML parser and its limits, and keeps what the Tree
    /// view shows: names as written, attribute order and where each node is.
    pub fn parse(source: &str) -> Result<Self> {
        let parsed = super::parse(source, Format::Xml)?;
        let starts: Vec<usize> = std::iter::once(0)
            .chain(
                crate::core::text::lines(source)
                    .skip(1)
                    .map(|(line, _)| line.as_ptr() as usize - source.as_ptr() as usize),
            )
            .collect();
        let mut doc = XmlDoc::default();
        let mut top: Vec<(u8, usize, &Node)> = parsed
            .children
            .iter()
            .filter_map(|(key, node)| {
                if key == "/root" {
                    Some((1, 0, node))
                } else if let Some(i) = index(key, "/before[") {
                    Some((0, i, node))
                } else {
                    index(key, "/after[").map(|i| (2, i, node))
                }
            })
            .collect();
        top.sort_by_key(|&(group, i, _)| (group, i));
        for (_, _, node) in top {
            let id = doc.add(node, None, source, &starts);
            doc.top.push(id);
        }
        for id in (0..doc.nodes.len()).rev() {
            doc.nodes[id].print = doc.fingerprint(id);
        }
        Ok(doc)
    }

    fn add(&mut self, node: &Node, parent: Option<usize>, source: &str, starts: &[usize]) -> usize {
        let at = |pos: usize| {
            let line = starts.partition_point(|&s| s <= pos);
            let from = starts.get(line.wrapping_sub(1)).copied().unwrap_or(0);
            let col = source.get(from..pos).map_or(1, |s| s.chars().count() + 1);
            (line.max(1), col)
        };
        let bytes = node.span.bytes.clone();
        let (line, col) = at(bytes.start);
        let end_line = at(bytes.end.saturating_sub(1).max(bytes.start)).0;
        let kind = match node.kind.as_str() {
            "element" => XKind::Element,
            "comment" => XKind::Comment,
            "pi" => XKind::Pi,
            _ => XKind::Text,
        };
        let value = node.value.clone().unwrap_or_default();
        let (name, display, value) = if kind == XKind::Element {
            let written: String = source
                .get(bytes.start + 1..)
                .unwrap_or("")
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '/' | '>'))
                .collect();
            let display = if written.is_empty() {
                local(&value).to_owned()
            } else {
                written
            };
            (value, display, String::new())
        } else {
            (String::new(), String::new(), value)
        };
        let id = self.nodes.len();
        self.nodes.push(XNode {
            kind,
            name,
            display,
            value,
            attrs: Vec::new(),
            children: Vec::new(),
            parent,
            bytes: bytes.clone(),
            line,
            col,
            end_line,
            print: 0,
        });
        let mut attrs = Vec::new();
        let mut children = Vec::new();
        for (key, child) in &node.children {
            if let Some(attr) = key.strip_prefix("/@") {
                let span = child.span.bytes.clone();
                // An attribute whose place could not be read has the span of its start tag.
                let display = source
                    .get(span.clone())
                    .filter(|_| span.start != bytes.start)
                    .and_then(|s| s.split_once('='))
                    .map(|(name, _)| name.trim().to_owned())
                    .unwrap_or_else(|| local(attr).to_owned());
                let (line, col) = at(span.start);
                attrs.push(XAttr {
                    name: attr.to_owned(),
                    display,
                    value: child.value.clone().unwrap_or_default(),
                    bytes: span,
                    line,
                    col,
                });
            } else if let Some(i) = index(key, "/node()[") {
                children.push((i, child));
            }
        }
        attrs.sort_by_key(|a| a.bytes.start);
        children.sort_by_key(|&(i, _)| i);
        self.nodes[id].attrs = attrs;
        let ids = children
            .into_iter()
            .map(|(_, child)| self.add(child, Some(id), source, starts))
            .collect();
        self.nodes[id].children = ids;
        id
    }

    fn fingerprint(&self, id: usize) -> u128 {
        let node = &self.nodes[id];
        let half = |seed: u8| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            seed.hash(&mut h);
            node.kind.hash(&mut h);
            node.name.hash(&mut h);
            node.value.hash(&mut h);
            let mut attrs: Vec<_> = node.attrs.iter().map(|a| (&a.name, &a.value)).collect();
            attrs.sort();
            attrs.hash(&mut h);
            for &child in &node.children {
                self.nodes[child].print.hash(&mut h);
            }
            h.finish() as u128
        };
        half(0) << 64 | half(1)
    }

    /// `/catalog/book[2]/@id`: names as written, with a position where a parent has several
    /// children of the same name.
    pub fn path(&self, id: usize, attr: Option<usize>) -> String {
        let mut steps = Vec::new();
        let mut at = Some(id);
        while let Some(i) = at {
            let node = &self.nodes[i];
            let siblings = match node.parent {
                Some(p) => &self.nodes[p].children,
                None => &self.top,
            };
            let same = |&&s: &&usize| {
                let other = &self.nodes[s];
                other.kind == node.kind && other.name == node.name
            };
            let test = match node.kind {
                XKind::Element => node.display.clone(),
                XKind::Text => "text()".into(),
                XKind::Comment => "comment()".into(),
                XKind::Pi => "processing-instruction()".into(),
            };
            let count = siblings.iter().filter(same).count();
            steps.push(if count > 1 {
                let n = siblings
                    .iter()
                    .take_while(|&&s| s != i)
                    .filter(same)
                    .count()
                    + 1;
                format!("{test}[{n}]")
            } else {
                test
            });
            at = node.parent;
        }
        steps.reverse();
        let mut path = format!("/{}", steps.join("/"));
        if let Some(a) = attr {
            path.push_str(&format!("/@{}", self.nodes[id].attrs[a].display));
        }
        path
    }

    /// [`Self::path`] of every node, in one pass: a node's siblings are counted once for all
    /// of them rather than once for each.
    pub fn paths(&self) -> Vec<String> {
        let mut out = vec![String::new(); self.nodes.len()];
        let name = |parent: &str, list: &[usize], out: &mut Vec<String>| {
            let key = |id: usize| (self.nodes[id].kind, self.nodes[id].name.as_str());
            let mut total: HashMap<(XKind, &str), usize> = HashMap::new();
            for &id in list {
                *total.entry(key(id)).or_default() += 1;
            }
            let mut seen: HashMap<(XKind, &str), usize> = HashMap::new();
            for &id in list {
                let node = &self.nodes[id];
                let test = match node.kind {
                    XKind::Element => node.display.as_str(),
                    XKind::Text => "text()",
                    XKind::Comment => "comment()",
                    XKind::Pi => "processing-instruction()",
                };
                let n = seen.entry(key(id)).or_default();
                *n += 1;
                out[id] = if total[&key(id)] > 1 {
                    format!("{parent}/{test}[{n}]")
                } else {
                    format!("{parent}/{test}")
                };
            }
        };
        name("", &self.top, &mut out);
        // ids are given parents first, so a parent's path is known before its children's
        for id in 0..self.nodes.len() {
            if !self.nodes[id].children.is_empty() {
                let parent = std::mem::take(&mut out[id]);
                name(&parent, &self.nodes[id].children, &mut out);
                out[id] = parent;
            }
        }
        out
    }
}

fn index(key: &str, prefix: &str) -> Option<usize> {
    key.strip_prefix(prefix)?.strip_suffix(']')?.parse().ok()
}

/// What one side of a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Node(usize),
    /// Attribute `.1` of element `.0`.
    Attr(usize, usize),
}

impl Side {
    pub fn node(self) -> usize {
        match self {
            Side::Node(n) | Side::Attr(n, _) => n,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// The node itself compares equal; what is below it may not.
    Same,
    Added,
    Removed,
    /// A text, comment, processing instruction or attribute whose value differs.
    Modified,
}

/// How the two sides of a row were paired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
    /// Identical subtrees, or attributes of the same expanded name.
    Exact,
    /// By the key rule.
    Key,
    /// By kind and name between other pairs.
    Name,
    /// Only on one side.
    Unpaired,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub sides: [Option<Side>; 2],
    pub depth: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub kind: RowKind,
    pub pairing: Pairing,
    /// Another pairing is as likely: to be checked by hand.
    pub ambiguous: bool,
    /// Moved among its siblings: 1-based child positions on each side.
    pub moved: Option<[usize; 2]>,
    /// Below an added or removed row, or whitespace beside one that is part of it, so not a
    /// change of its own.
    pub within: bool,
    /// Whitespace-only text that is not a change, which the view leaves out.
    pub blank: bool,
    /// Changes in this row and below it.
    pub changes: usize,
}

impl Row {
    pub fn is_change(&self) -> bool {
        !self.within && (self.kind != RowKind::Same || self.moved.is_some())
    }
}

/// A key rule: elements named `element` (a local name, a name as written, or a path ending
/// in it such as `catalog/book`) are paired by the values of `attrs`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyRule {
    pub element: String,
    pub attrs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TreeOptions {
    pub keys: Vec<KeyRule>,
}

impl TreeOptions {
    /// Reads rules written `book@id; device@name+slot`.
    pub fn parse(text: &str) -> Result<Self> {
        let mut keys = Vec::new();
        for rule in text.split(';').map(str::trim).filter(|r| !r.is_empty()) {
            let Some((element, attrs)) = rule.split_once('@') else {
                bail!("Key rule {rule:?} needs element@attribute");
            };
            let attrs: Vec<String> = attrs
                .split('+')
                .map(|a| a.trim().trim_start_matches('@').to_owned())
                .collect();
            let element = element.trim().trim_matches('/').to_owned();
            if element.is_empty() || attrs.iter().any(|a| a.is_empty()) {
                bail!("Key rule {rule:?} needs element@attribute");
            }
            keys.push(KeyRule { element, attrs });
        }
        Ok(Self { keys })
    }
}

/// Why a key rule was not used for some sibling groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyWarning {
    pub rule: String,
    /// Elements whose key another sibling has too.
    pub duplicates: usize,
    /// Elements without every attribute of the key.
    pub missing: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
    pub ambiguous: usize,
}

/// The aligned tree of two documents.
#[derive(Clone, Debug)]
pub struct TreeDiff {
    pub docs: [XmlDoc; 2],
    pub rows: Vec<Row>,
    pub top: Vec<usize>,
    /// Change rows, in tree order.
    pub targets: Vec<usize>,
    pub counts: Counts,
    pub warnings: Vec<KeyWarning>,
    /// Elements the key rules paired.
    pub keyed: usize,
    /// Different for every comparison made, so a view can tell a new one from the last.
    pub serial: u64,
}

impl TreeDiff {
    pub fn compare(a: &str, b: &str, options: &TreeOptions, cancel: &AtomicBool) -> Result<Self> {
        let docs = [XmlDoc::parse(a)?, XmlDoc::parse(b)?];
        Self::of(docs, options, cancel)
    }

    pub fn of(docs: [XmlDoc; 2], options: &TreeOptions, cancel: &AtomicBool) -> Result<Self> {
        let mut builder = Builder {
            docs: [&docs[0], &docs[1]],
            options,
            rows: Vec::new(),
            warnings: BTreeMap::new(),
            keyed: 0,
            cancel,
        };
        let top = builder.group(&docs[0].top, &docs[1].top, None, 0, false)?;
        let Builder {
            rows,
            warnings,
            keyed,
            ..
        } = builder;
        let mut diff = TreeDiff {
            docs,
            rows,
            top,
            targets: Vec::new(),
            counts: Counts::default(),
            warnings: warnings
                .into_iter()
                .map(|(rule, (duplicates, missing))| KeyWarning {
                    rule,
                    duplicates,
                    missing,
                })
                .collect(),
            keyed,
            serial: {
                static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                SERIAL.fetch_add(1, Ordering::Relaxed)
            },
        };
        let mut order = Vec::with_capacity(diff.rows.len());
        let mut stack: Vec<usize> = diff.top.iter().rev().copied().collect();
        while let Some(r) = stack.pop() {
            order.push(r);
            stack.extend(diff.rows[r].children.iter().rev());
        }
        for &r in order.iter().rev() {
            let row = &diff.rows[r];
            let below: usize = row.children.iter().map(|&c| diff.rows[c].changes).sum();
            diff.rows[r].changes = below + row.is_change() as usize;
        }
        for &r in &order {
            let row = &diff.rows[r];
            if !row.is_change() {
                continue;
            }
            diff.targets.push(r);
            let c = &mut diff.counts;
            match row.kind {
                RowKind::Added => c.added += 1,
                RowKind::Removed => c.removed += 1,
                RowKind::Modified => c.modified += 1,
                RowKind::Same => {}
            }
            c.moved += row.moved.is_some() as usize;
            c.ambiguous += row.ambiguous as usize;
        }
        Ok(diff)
    }

    pub fn identical(&self) -> bool {
        self.targets.is_empty()
    }

    /// The rows in tree order.
    pub fn preorder(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.rows.len());
        let mut stack: Vec<usize> = self.top.iter().rev().copied().collect();
        while let Some(r) = stack.pop() {
            out.push(r);
            stack.extend(self.rows[r].children.iter().rev());
        }
        out
    }

    pub fn node(&self, side: usize, row: usize) -> Option<&XNode> {
        self.rows[row].sides[side].map(|s| &self.docs[side].nodes[s.node()])
    }

    #[cfg(test)]
    pub fn attr(&self, side: usize, row: usize) -> Option<&XAttr> {
        match self.rows[row].sides[side]? {
            Side::Attr(n, a) => Some(&self.docs[side].nodes[n].attrs[a]),
            Side::Node(_) => None,
        }
    }

    pub fn path(&self, side: usize, row: usize) -> Option<String> {
        self.rows[row].sides[side].map(|s| match s {
            Side::Node(n) => self.docs[side].path(n, None),
            Side::Attr(n, a) => self.docs[side].path(n, Some(a)),
        })
    }

    /// The first row, in tree order, at one of `paths` on its side, as [`Self::path`] gives.
    pub fn row_at_path(&self, paths: &[Option<String>; 2]) -> Option<usize> {
        let all = [0, 1].map(|s| paths[s].as_ref().map(|_| self.docs[s].paths()));
        self.preorder().into_iter().find(|&r| {
            (0..2).any(|s| {
                let (Some(want), Some(all), Some(side)) =
                    (&paths[s], &all[s], self.rows[r].sides[s])
                else {
                    return false;
                };
                match side {
                    Side::Node(n) => all[n] == *want,
                    Side::Attr(n, a) => {
                        want.strip_prefix(all[n].as_str())
                            .and_then(|rest| rest.strip_prefix("/@"))
                            == Some(self.docs[s].nodes[n].attrs[a].display.as_str())
                    }
                }
            })
        })
    }

    /// Where side `side` of `row` is written: byte range, line and column.
    pub fn location(&self, side: usize, row: usize) -> Option<(Range<usize>, usize, usize)> {
        Some(match self.rows[row].sides[side]? {
            Side::Node(n) => {
                let node = &self.docs[side].nodes[n];
                (node.bytes.clone(), node.line, node.col)
            }
            Side::Attr(n, a) => {
                let attr = &self.docs[side].nodes[n].attrs[a];
                (attr.bytes.clone(), attr.line, attr.col)
            }
        })
    }

    /// The value one side of `row` shows: an attribute value, text or comment.
    pub fn value(&self, side: usize, row: usize) -> Option<&str> {
        Some(match self.rows[row].sides[side]? {
            Side::Attr(n, a) => &self.docs[side].nodes[n].attrs[a].value,
            Side::Node(n) => &self.docs[side].nodes[n].value,
        })
    }

    /// The row furthest down the tree whose node on `side` covers 1-based `line`.
    pub fn row_at_line(&self, side: usize, line: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut rows: Vec<usize> = self.top.clone();
        while let Some(r) = rows.pop() {
            let row = &self.rows[r];
            let covers = match row.sides[side] {
                Some(Side::Node(n)) => {
                    let node = &self.docs[side].nodes[n];
                    node.line <= line && line <= node.end_line
                }
                Some(Side::Attr(n, a)) => self.docs[side].nodes[n].attrs[a].line == line,
                None => false,
            };
            if covers && !row.blank {
                if best.is_none_or(|b| self.rows[b].depth <= row.depth) {
                    best = Some(r);
                }
                rows.extend(&row.children);
            }
        }
        best
    }

    /// Rows matching `query`, in tree order: an XPath subset when it starts with `/`, else
    /// text found in names, attribute values, text and comments, ignoring case.
    pub fn search(&self, query: &str) -> Result<Vec<usize>, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        if query.starts_with('/') {
            let path = xpath::parse(query)?;
            let found = [0, 1].map(|side| xpath::select(&self.docs[side], &path));
            return Ok(self
                .preorder()
                .into_iter()
                .filter(|&r| {
                    let row = &self.rows[r];
                    (0..2).any(|s| row.sides[s].is_some_and(|x| found[s].contains(&x)))
                })
                .collect());
        }
        let needle = query.to_lowercase();
        let has = |s: &str| s.to_lowercase().contains(&needle);
        Ok(self
            .preorder()
            .into_iter()
            .filter(|&r| {
                let row = &self.rows[r];
                !row.blank
                    && (0..2).any(|side| match row.sides[side] {
                        Some(Side::Attr(n, a)) => {
                            let attr = &self.docs[side].nodes[n].attrs[a];
                            has(&attr.display) || has(&attr.value)
                        }
                        Some(Side::Node(n)) => {
                            let node = &self.docs[side].nodes[n];
                            has(&node.display) || has(&node.value)
                        }
                        None => false,
                    })
            })
            .collect())
    }

    /// Elements that occur more than once among the child rows of `row`, for the Collection
    /// view: their expanded name, their name as written and their number.
    pub fn collections(&self, row: usize) -> Vec<(String, String, usize)> {
        let mut counts: Vec<(String, String, usize)> = Vec::new();
        for &c in &self.rows[row].children {
            let Some((name, display)) = self.element_name(c) else {
                continue;
            };
            match counts.iter_mut().find(|(n, _, _)| *n == name) {
                Some(entry) => entry.2 += 1,
                None => counts.push((name, display, 1)),
            }
        }
        counts.retain(|(_, _, n)| *n > 1);
        counts
    }

    /// The expanded name and the name as written of the element of `row`.
    fn element_name(&self, row: usize) -> Option<(String, String)> {
        let r = &self.rows[row];
        let side = r.sides.iter().position(Option::is_some)?;
        match r.sides[side]? {
            Side::Node(n) => {
                let node = &self.docs[side].nodes[n];
                (node.kind == XKind::Element).then(|| (node.name.clone(), node.display.clone()))
            }
            Side::Attr(..) => None,
        }
    }

    /// The child rows of `row` whose expanded name is `name` as a table: a column per
    /// attribute (`@id`), one per child element that holds only text and occurs once in an
    /// element (`title`), and one for the text of elements that hold only text (`text()`).
    pub fn collection(&self, row: usize, name: &str) -> Collection {
        #[derive(PartialEq)]
        enum Column {
            Attr(String),
            Child(String),
            /// Attribute `.1` of child element `.0`.
            ChildAttr(String, String),
            Text,
        }
        let items: Vec<usize> = self.rows[row]
            .children
            .iter()
            .copied()
            .filter(|&c| self.element_name(c).is_some_and(|(n, _)| n == name))
            .collect();
        let mut columns: Vec<(Column, String)> = Vec::new();
        let mut nested = false;
        let mut text = false;
        let mut add = |column: Column, label: String| {
            if !columns.iter().any(|(c, _)| *c == column) {
                columns.push((column, label));
            }
        };
        for &item in &items {
            for side in 0..2 {
                let Some(node) = self.node(side, item) else {
                    continue;
                };
                let doc = &self.docs[side];
                for attr in &node.attrs {
                    add(
                        Column::Attr(attr.name.clone()),
                        format!("@{}", attr.display),
                    );
                }
                let elements: Vec<&XNode> = node
                    .children
                    .iter()
                    .map(|&c| &doc.nodes[c])
                    .filter(|c| c.kind == XKind::Element)
                    .collect();
                for child in &elements {
                    let simple = child
                        .children
                        .iter()
                        .all(|&g| doc.nodes[g].kind == XKind::Text)
                        && elements.iter().filter(|e| e.name == child.name).count() == 1;
                    if simple {
                        add(Column::Child(child.name.clone()), child.display.clone());
                        for attr in &child.attrs {
                            add(
                                Column::ChildAttr(child.name.clone(), attr.name.clone()),
                                format!("{}/@{}", child.display, attr.display),
                            );
                        }
                    } else {
                        nested = true;
                    }
                }
                let own_text = node
                    .children
                    .iter()
                    .any(|&c| doc.nodes[c].kind == XKind::Text && !doc.nodes[c].blank());
                if own_text && !elements.is_empty() {
                    nested = true;
                }
                text |= own_text && elements.is_empty();
            }
        }
        if text {
            add(Column::Text, "text()".into());
        }
        let text_of = |side: usize, node: &XNode| -> String {
            node.children
                .iter()
                .map(|&c| self.docs[side].nodes[c].value.as_str())
                .collect()
        };
        let rows = items
            .iter()
            .map(|&item| {
                let cells = columns
                    .iter()
                    .map(|(column, _)| {
                        [0, 1].map(|side| {
                            let node = self.node(side, item)?;
                            let doc = &self.docs[side];
                            let child = |n: &str| {
                                node.children
                                    .iter()
                                    .map(|&c| &doc.nodes[c])
                                    .find(|c| c.kind == XKind::Element && c.name == n)
                            };
                            match column {
                                Column::Attr(n) => node
                                    .attrs
                                    .iter()
                                    .find(|a| &a.name == n)
                                    .map(|a| a.value.clone()),
                                Column::Child(n) => child(n).map(|c| text_of(side, c)),
                                Column::ChildAttr(n, a) => child(n)?
                                    .attrs
                                    .iter()
                                    .find(|x| &x.name == a)
                                    .map(|x| x.value.clone()),
                                Column::Text => Some(text_of(side, node)),
                            }
                        })
                    })
                    .collect();
                CollectionRow { row: item, cells }
            })
            .collect();
        Collection {
            columns: columns.into_iter().map(|(_, label)| label).collect(),
            rows,
            nested,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Collection {
    pub columns: Vec<String>,
    pub rows: Vec<CollectionRow>,
    /// The elements hold elements with content of their own, or mixed content, which the
    /// table does not show.
    pub nested: bool,
}

#[derive(Clone, Debug)]
pub struct CollectionRow {
    pub row: usize,
    /// Per column, the value on each side.
    pub cells: Vec<[Option<String>; 2]>,
}

/// A pairing of siblings by the aligner.
#[derive(Clone, Copy, Debug)]
struct Pair {
    sides: [Option<usize>; 2],
    pairing: Pairing,
    ambiguous: bool,
    moved: bool,
    /// Whitespace beside an added or removed element, which is part of it.
    attached: bool,
}

struct Builder<'a> {
    docs: [&'a XmlDoc; 2],
    options: &'a TreeOptions,
    rows: Vec<Row>,
    /// Per rule: duplicate and missing keys.
    warnings: BTreeMap<String, (usize, usize)>,
    keyed: usize,
    cancel: &'a AtomicBool,
}

impl Builder<'_> {
    fn node(&self, side: usize, id: usize) -> &XNode {
        &self.docs[side].nodes[id]
    }

    fn push(&mut self, row: Row) -> Result<usize> {
        if self.rows.len().is_multiple_of(4096) && self.cancel.load(Ordering::Relaxed) {
            bail!("Analysis cancelled");
        }
        self.rows.push(row);
        Ok(self.rows.len() - 1)
    }

    /// Rows for the sibling groups `xs` and `ys` under row `parent`.
    fn group(
        &mut self,
        xs: &[usize],
        ys: &[usize],
        parent: Option<usize>,
        depth: usize,
        within: bool,
    ) -> Result<Vec<usize>> {
        let pairs = self.align(xs, ys);
        let places: [HashMap<usize, usize>; 2] =
            [xs, ys].map(|list| list.iter().enumerate().map(|(i, &id)| (id, i)).collect());
        let mut out = Vec::with_capacity(pairs.len());
        for pair in pairs {
            // whitespace kept beside reordered elements is not a move of its own
            let blank = pair.sides[0].is_some_and(|x| self.node(0, x).blank());
            let moved = (pair.moved && !blank)
                .then(|| [0, 1].map(|s| pair.sides[s].map_or(0, |id| places[s][&id] + 1)));
            out.push(self.pair(pair, moved, parent, depth, within || pair.attached)?);
        }
        Ok(out)
    }

    fn pair(
        &mut self,
        pair: Pair,
        moved: Option<[usize; 2]>,
        parent: Option<usize>,
        depth: usize,
        within: bool,
    ) -> Result<usize> {
        let [a, b] = pair.sides;
        let kind = match (a, b) {
            (Some(_), None) => RowKind::Removed,
            (None, Some(_)) => RowKind::Added,
            (Some(a), Some(b)) => {
                let (x, y) = (self.node(0, a), self.node(1, b));
                if x.kind != XKind::Element && x.value != y.value {
                    RowKind::Modified
                } else {
                    RowKind::Same
                }
            }
            (None, None) => unreachable!("a row has a side"),
        };
        let one = a.or(b).map(|id| self.node(a.map_or(1, |_| 0), id));
        let blank = one.is_some_and(XNode::blank) && (kind == RowKind::Same || within);
        let row = self.push(Row {
            sides: [a.map(Side::Node), b.map(Side::Node)],
            depth,
            parent,
            children: Vec::new(),
            kind,
            pairing: pair.pairing,
            ambiguous: pair.ambiguous && !within,
            moved,
            within,
            blank,
            changes: 0,
        })?;
        let below = within || kind != RowKind::Same;
        let mut children = self.attributes(a, b, row, depth + 1, below)?;
        let empty = Vec::new();
        let kids = |side: usize, id: Option<usize>| {
            id.map_or(&empty, |id| &self.docs[side].nodes[id].children)
                .clone()
        };
        let (xs, ys) = (kids(0, a), kids(1, b));
        children.extend(self.group(&xs, &ys, Some(row), depth + 1, below)?);
        self.rows[row].children = children;
        Ok(row)
    }

    /// Attribute rows of elements `a` and `b`, by expanded name in the order of `b`, then those
    /// only `a` has.
    fn attributes(
        &mut self,
        a: Option<usize>,
        b: Option<usize>,
        parent: usize,
        depth: usize,
        within: bool,
    ) -> Result<Vec<usize>> {
        let attrs = |side: usize, id: Option<usize>| -> Vec<(usize, String, String)> {
            id.map(|id| {
                self.docs[side].nodes[id]
                    .attrs
                    .iter()
                    .enumerate()
                    .map(|(i, at)| (i, at.name.clone(), at.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
        };
        let (xa, ya) = (attrs(0, a), attrs(1, b));
        let mut rows = Vec::new();
        let mut entries = Vec::new();
        for (j, name, value) in &ya {
            let left = xa.iter().find(|(_, n, _)| n == name);
            let kind = match left {
                None => RowKind::Added,
                Some((_, _, v)) if v != value => RowKind::Modified,
                Some(_) => RowKind::Same,
            };
            entries.push(([left.map(|l| l.0), Some(*j)], kind));
        }
        for (i, name, _) in &xa {
            if !ya.iter().any(|(_, n, _)| n == name) {
                entries.push(([Some(*i), None], RowKind::Removed));
            }
        }
        for ([i, j], kind) in entries {
            let sides = [
                i.zip(a).map(|(i, a)| Side::Attr(a, i)),
                j.zip(b).map(|(j, b)| Side::Attr(b, j)),
            ];
            rows.push(self.push(Row {
                sides,
                depth,
                parent: Some(parent),
                children: Vec::new(),
                kind,
                pairing: if sides.iter().all(Option::is_some) {
                    Pairing::Exact
                } else {
                    Pairing::Unpaired
                },
                ambiguous: false,
                moved: None,
                within,
                blank: false,
                changes: 0,
            })?);
        }
        Ok(rows)
    }

    /// The key rule covering element `id` on `side`.
    fn rule(&self, side: usize, id: usize) -> Option<usize> {
        let node = self.node(side, id);
        if node.kind != XKind::Element {
            return None;
        }
        self.options.keys.iter().position(|rule| {
            if !rule.element.contains('/') {
                return rule.element == node.local() || rule.element == node.display;
            }
            let mut at = Some(id);
            rule.element.rsplit('/').all(|step| {
                let Some(n) = at else {
                    return false;
                };
                let node = self.node(side, n);
                at = node.parent;
                step == "*" || step == node.local() || step == node.display
            })
        })
    }

    /// The key of element `id` under `rule`, led by its expanded name: a rule matches local
    /// names, but elements of other namespaces are never paired by it.
    fn key(&self, side: usize, id: usize, rule: &KeyRule) -> Option<Vec<String>> {
        let node = self.node(side, id);
        std::iter::once(Some(node.name.clone()))
            .chain(rule.attrs.iter().map(|want| {
                node.attrs
                    .iter()
                    .find(|a| local(&a.name) == want || &a.display == want)
                    .map(|a| a.value.clone())
            }))
            .collect()
    }

    fn align(&mut self, xs: &[usize], ys: &[usize]) -> Vec<Pair> {
        // Siblings settled by a key rule: paired, or known to have no counterpart.
        let mut settled: [HashSet<usize>; 2] = Default::default();
        // Siblings a key rule should have paired but could not.
        let mut doubtful: [HashSet<usize>; 2] = Default::default();
        let mut pairs: Vec<Pair> = Vec::new();
        for (r, rule) in self.options.keys.iter().enumerate() {
            let covered = [(0, xs), (1, ys)].map(|(side, list)| {
                list.iter()
                    .copied()
                    .filter(|&id| self.rule(side, id) == Some(r))
                    .collect::<Vec<_>>()
            });
            if covered.iter().all(Vec::is_empty) {
                continue;
            }
            let mut keys: [HashMap<Vec<String>, usize>; 2] = Default::default();
            let (mut duplicates, mut missing) = (0, 0);
            for side in 0..2 {
                let mut seen: HashMap<Vec<String>, usize> = HashMap::new();
                for &id in &covered[side] {
                    match self.key(side, id, rule) {
                        None => missing += 1,
                        Some(key) => {
                            *seen.entry(key.clone()).or_default() += 1;
                            keys[side].insert(key, id);
                        }
                    }
                }
                duplicates += seen.values().filter(|&&n| n > 1).sum::<usize>();
            }
            if duplicates + missing > 0 {
                let label = format!("{}@{}", rule.element, rule.attrs.join("+"));
                let entry = self.warnings.entry(label).or_default();
                entry.0 += duplicates;
                entry.1 += missing;
                for side in 0..2 {
                    doubtful[side].extend(&covered[side]);
                }
                continue;
            }
            for &id in &covered[0] {
                settled[0].insert(id);
            }
            for &id in &covered[1] {
                settled[1].insert(id);
            }
            for &x in &covered[0] {
                let key = self.key(0, x, rule).expect("complete key");
                if let Some(&y) = keys[1].get(&key) {
                    self.keyed += 1;
                    pairs.push(Pair {
                        sides: [Some(x), Some(y)],
                        pairing: Pairing::Key,
                        ambiguous: false,
                        moved: false,
                        attached: false,
                    });
                }
            }
        }
        let position = |list: &[usize]| -> HashMap<usize, usize> {
            list.iter().enumerate().map(|(i, &id)| (id, i)).collect()
        };
        let (pos_x, pos_y) = (position(xs), position(ys));
        // Keyed pairs out of the order of the others moved.
        pairs.sort_by_key(|p| pos_x[&p.sides[0].unwrap()]);
        let order: Vec<usize> = pairs.iter().map(|p| pos_y[&p.sides[1].unwrap()]).collect();
        let stay = super::increasing(&order);
        let could_stay = super::in_some_increasing(&order);
        for (k, pair) in pairs.iter_mut().enumerate() {
            if !stay.contains(&k) {
                pair.moved = true;
                pair.ambiguous = could_stay.contains(&k);
            }
        }
        // Between keyed anchors, the other siblings are paired as identical subtrees.
        let mut anchors: Vec<(usize, usize)> = pairs
            .iter()
            .filter(|p| !p.moved)
            .map(|p| (pos_x[&p.sides[0].unwrap()], pos_y[&p.sides[1].unwrap()]))
            .collect();
        let free = |settled: &[HashSet<usize>; 2],
                    list: &[usize],
                    side: usize,
                    from: usize,
                    to: usize|
         -> Vec<usize> {
            (from..to)
                .filter(|&i| !settled[side].contains(&list[i]))
                .collect()
        };
        let bounds = |anchors: &[(usize, usize)]| {
            let mut out = Vec::with_capacity(anchors.len() + 1);
            let mut from = (0, 0);
            for &(i, j) in anchors {
                out.push((from, (i, j)));
                from = (i + 1, j + 1);
            }
            out.push((from, (xs.len(), ys.len())));
            out
        };
        let mut exact = Vec::new();
        for ((fi, fj), (ti, tj)) in bounds(&anchors) {
            let gx = free(&settled, xs, 0, fi, ti);
            let gy = free(&settled, ys, 1, fj, tj);
            let px: Vec<u128> = gx.iter().map(|&i| self.node(0, xs[i]).print).collect();
            let py: Vec<u128> = gy.iter().map(|&j| self.node(1, ys[j]).print).collect();
            for (i, j) in common(&px, &py) {
                exact.push((gx[i], gy[j]));
            }
        }
        for &(i, j) in &exact {
            settled[0].insert(xs[i]);
            settled[1].insert(ys[j]);
            pairs.push(Pair {
                sides: [Some(xs[i]), Some(ys[j])],
                pairing: Pairing::Exact,
                ambiguous: false,
                moved: false,
                attached: false,
            });
        }
        anchors.extend(exact);
        anchors.sort();
        // A subtree found once on each side, out of order, moved.
        let mut once: [HashMap<u128, Option<usize>>; 2] = Default::default();
        for (side, list) in [xs, ys].into_iter().enumerate() {
            for &id in list {
                let node = self.node(side, id);
                if node.kind == XKind::Text {
                    continue;
                }
                once[side]
                    .entry(node.print)
                    .and_modify(|e| *e = None)
                    .or_insert(Some(id));
            }
        }
        for (print, x) in &once[0] {
            let (Some(x), Some(Some(y))) = (x, once[1].get(print)) else {
                continue;
            };
            if settled[0].contains(x)
                || settled[1].contains(y)
                || doubtful[0].contains(x)
                || doubtful[1].contains(y)
            {
                continue;
            }
            settled[0].insert(*x);
            settled[1].insert(*y);
            pairs.push(Pair {
                sides: [Some(*x), Some(*y)],
                pairing: Pairing::Exact,
                ambiguous: false,
                moved: true,
                attached: false,
            });
        }
        // What is left is paired by kind and name between the anchors.
        for ((fi, fj), (ti, tj)) in bounds(&anchors) {
            let gx = free(&settled, xs, 0, fi, ti);
            let gy = free(&settled, ys, 1, fj, tj);
            let sig = |side: usize, id: usize| {
                let node = self.node(side, id);
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (node.kind, &node.name).hash(&mut h);
                h.finish() as u128
            };
            let sx: Vec<u128> = gx.iter().map(|&i| sig(0, xs[i])).collect();
            let sy: Vec<u128> = gy.iter().map(|&j| sig(1, ys[j])).collect();
            let matched = common(&sx, &sy);
            let mut paired = [HashSet::new(), HashSet::new()];
            for &(i, j) in &matched {
                paired[0].insert(i);
                paired[1].insert(j);
            }
            // Siblings of a signature left unpaired could have been paired instead.
            let unpaired: HashSet<u128> = (0..sx.len())
                .filter(|i| !paired[0].contains(i))
                .map(|i| sx[i])
                .chain(
                    (0..sy.len())
                        .filter(|j| !paired[1].contains(j))
                        .map(|j| sy[j]),
                )
                .collect();
            let matched_sigs: HashSet<u128> = matched.iter().map(|&(i, _)| sx[i]).collect();
            for (i, &x) in gx.iter().enumerate() {
                if !paired[0].contains(&i) && matched_sigs.contains(&sx[i]) {
                    doubtful[0].insert(xs[x]);
                }
            }
            for (j, &y) in gy.iter().enumerate() {
                if !paired[1].contains(&j) && matched_sigs.contains(&sy[j]) {
                    doubtful[1].insert(ys[y]);
                }
            }
            for (i, j) in matched {
                let (x, y) = (xs[gx[i]], ys[gy[j]]);
                settled[0].insert(x);
                settled[1].insert(y);
                pairs.push(Pair {
                    sides: [Some(x), Some(y)],
                    pairing: Pairing::Name,
                    ambiguous: unpaired.contains(&sx[i])
                        || doubtful[0].contains(&x)
                        || doubtful[1].contains(&y),
                    moved: false,
                    attached: false,
                });
            }
        }
        // Whitespace beside an element left without a counterpart, such as the indentation of
        // an inserted line, is part of that element when no other whitespace is left over.
        let in_pair: [HashSet<usize>; 2] =
            [0, 1].map(|side| pairs.iter().filter_map(|p| p.sides[side]).collect());
        let beside: [HashSet<usize>; 2] = [0, 1].map(|side| {
            let list = [xs, ys][side];
            let alone = |id: usize| !in_pair[side].contains(&id) && !settled[side].contains(&id);
            (0..list.len())
                .filter(|&i| alone(list[i]) && self.node(side, list[i]).blank())
                .filter(|&i| {
                    [i.wrapping_sub(1), i + 1].iter().any(|&n| {
                        list.get(n).is_some_and(|&id| {
                            !in_pair[side].contains(&id) && !self.node(side, id).blank()
                        })
                    })
                })
                .map(|i| list[i])
                .collect()
        });
        // Whitespace-only text left over, mostly indentation between reordered elements, pairs
        // with the same text anywhere in the group: first what is beside no such element.
        for round in 0..2 {
            let skip = |side: usize, id: usize| round == 0 && beside[side].contains(&id);
            let mut blanks: HashMap<&str, Vec<usize>> = HashMap::new();
            for &y in ys.iter().rev() {
                let node = self.node(1, y);
                if node.blank() && !settled[1].contains(&y) && !skip(1, y) {
                    blanks.entry(node.value.as_str()).or_default().push(y);
                }
            }
            for &x in xs {
                let node = self.node(0, x);
                if !node.blank() || settled[0].contains(&x) || skip(0, x) {
                    continue;
                }
                if let Some(y) = blanks.get_mut(node.value.as_str()).and_then(Vec::pop) {
                    settled[0].insert(x);
                    settled[1].insert(y);
                    pairs.push(Pair {
                        sides: [Some(x), Some(y)],
                        pairing: Pairing::Exact,
                        ambiguous: false,
                        moved: true,
                        attached: false,
                    });
                }
            }
        }
        // Siblings with an identical sibling that was paired: either could be the one added.
        let paired_prints: [HashSet<u128>; 2] = [0, 1].map(|side| {
            pairs
                .iter()
                .filter_map(|p| p.sides[side])
                .map(|id| self.node(side, id).print)
                .collect()
        });
        let in_pair: [HashSet<usize>; 2] =
            [0, 1].map(|side| pairs.iter().filter_map(|p| p.sides[side]).collect());
        for (side, list) in [xs, ys].into_iter().enumerate() {
            for &id in list {
                if in_pair[side].contains(&id) {
                    continue;
                }
                let node = self.node(side, id);
                let mut sides = [None, None];
                sides[side] = Some(id);
                pairs.push(Pair {
                    sides,
                    pairing: Pairing::Unpaired,
                    ambiguous: doubtful[side].contains(&id)
                        || (node.kind != XKind::Text && paired_prints[side].contains(&node.print)),
                    moved: false,
                    attached: beside[side].contains(&id),
                });
            }
        }
        order_pairs(pairs, &pos_x, &pos_y)
    }
}

/// Pairs in the order rows show them: by the second side, with what only the first side has
/// after the pair before it there, and before what only the second side has in its place.
fn order_pairs(
    pairs: Vec<Pair>,
    pos_x: &HashMap<usize, usize>,
    pos_y: &HashMap<usize, usize>,
) -> Vec<Pair> {
    let mut left_only: Vec<Pair> = Vec::new();
    let mut right: Vec<Pair> = Vec::new();
    for pair in pairs {
        if pair.sides[1].is_none() {
            left_only.push(pair);
        } else {
            right.push(pair);
        }
    }
    left_only.sort_by_key(|p| pos_x[&p.sides[0].unwrap()]);
    right.sort_by_key(|p| pos_y[&p.sides[1].unwrap()]);
    // For each place on the second side, the first-side position of the next pair that stays.
    let mut next_stay = vec![usize::MAX; right.len() + 1];
    for k in (0..right.len()).rev() {
        let p = &right[k];
        next_stay[k] = match p.sides[0] {
            Some(x) if !p.moved => pos_x[&x],
            _ => next_stay[k + 1],
        };
    }
    let mut out = Vec::with_capacity(left_only.len() + right.len());
    let mut pending = left_only.into_iter().peekable();
    for (k, pair) in right.into_iter().enumerate() {
        if !pair.moved {
            let limit = next_stay[k];
            while let Some(p) = pending.next_if(|p| pos_x[&p.sides[0].unwrap()] < limit) {
                out.push(p);
            }
        }
        out.push(pair);
    }
    out.extend(pending);
    out
}

/// Index pairs of a longest common subsequence of `a` and `b`.
fn common(a: &[u128], b: &[u128]) -> Vec<(usize, usize)> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let ops =
        similar::capture_diff_slices_deadline(similar::Algorithm::Myers, a, b, Some(deadline));
    let mut out = Vec::new();
    for op in ops {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            out.extend((0..len).map(|k| (old_index + k, new_index + k)));
        }
    }
    out
}

/// A small XPath subset for searching the tree: `/` and `//` steps of element names (`*` for
/// any), `@name`, `text()` and `comment()`, with `[@a]`, `[@a='v']` and `[n]` predicates.
mod xpath {
    use super::{local, Side, XKind, XmlDoc};
    use std::collections::HashSet;

    #[derive(Debug)]
    enum Test {
        Element(String),
        Attr(String),
        Text,
        Comment,
    }
    #[derive(Debug)]
    enum Pred {
        Has(String),
        Equals(String, String),
        Nth(usize),
    }
    #[derive(Debug)]
    pub struct Step {
        descendant: bool,
        test: Test,
        preds: Vec<Pred>,
    }

    pub fn parse(query: &str) -> Result<Vec<Step>, String> {
        let unsupported = || format!("Unsupported XPath: {query}");
        let mut steps = Vec::new();
        let mut rest = query;
        while !rest.is_empty() {
            let descendant = if let Some(r) = rest.strip_prefix("//") {
                rest = r;
                true
            } else if let Some(r) = rest.strip_prefix('/') {
                rest = r;
                false
            } else {
                return Err(unsupported());
            };
            let end = step_end(rest);
            let (step, after) = rest.split_at(end);
            rest = after;
            let (head, mut preds_text) = step.split_once('[').map_or((step, ""), |(h, p)| (h, p));
            let test = match head {
                "text()" => Test::Text,
                "comment()" => Test::Comment,
                h if h.starts_with('@') && name_ok(&h[1..]) => Test::Attr(h[1..].into()),
                h if name_ok(h) => Test::Element(h.into()),
                _ => return Err(unsupported()),
            };
            let mut preds = Vec::new();
            while !preds_text.is_empty() {
                let Some(close) = closing(preds_text) else {
                    return Err(unsupported());
                };
                let (pred, after) = (&preds_text[..close], &preds_text[close + 1..]);
                preds.push(predicate(pred.trim()).ok_or_else(unsupported)?);
                preds_text = after.strip_prefix('[').unwrap_or(after);
                if !after.is_empty() && !after.starts_with('[') {
                    return Err(unsupported());
                }
            }
            if matches!(test, Test::Attr(_)) && !rest.is_empty() {
                return Err(unsupported());
            }
            steps.push(Step {
                descendant,
                test,
                preds,
            });
            if steps.len() > 32 {
                return Err(unsupported());
            }
        }
        if steps.is_empty() {
            return Err(unsupported());
        }
        Ok(steps)
    }

    /// Where the `]` closing a condition is, outside quotes.
    fn closing(text: &str) -> Option<usize> {
        let mut quote = None;
        for (i, c) in text.char_indices() {
            match (quote, c) {
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, ']') => return Some(i),
                _ => {}
            }
        }
        None
    }

    fn step_end(rest: &str) -> usize {
        let mut quote = None;
        for (i, c) in rest.char_indices() {
            match (quote, c) {
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '/') => return i,
                _ => {}
            }
        }
        rest.len()
    }

    fn name_ok(name: &str) -> bool {
        name == "*"
            || !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    }

    fn predicate(text: &str) -> Option<Pred> {
        if let Ok(n) = text.parse::<usize>() {
            return (n > 0).then_some(Pred::Nth(n));
        }
        let attr = text.strip_prefix('@')?;
        match attr.split_once('=') {
            None => name_ok(attr).then(|| Pred::Has(attr.into())),
            Some((name, value)) => {
                let (name, value) = (name.trim(), value.trim());
                let quoted = value.len() >= 2
                    && (value.starts_with('\'') && value.ends_with('\'')
                        || value.starts_with('"') && value.ends_with('"'));
                (name_ok(name) && quoted)
                    .then(|| Pred::Equals(name.into(), value[1..value.len() - 1].into()))
            }
        }
    }

    fn named(want: &str, expanded: &str, display: &str) -> bool {
        want == "*" || want == local(expanded) || want == display
    }

    /// What `path` selects in `doc`.
    pub fn select(doc: &XmlDoc, path: &[Step]) -> HashSet<Side> {
        // `None` is the document node.
        let mut context: Vec<Option<usize>> = vec![None];
        let mut out = HashSet::new();
        for (k, step) in path.iter().enumerate() {
            let mut next: Vec<Option<usize>> = Vec::new();
            let mut seen = HashSet::new();
            // Each parent is looked at once a step, so `//a//b` stays linear in the document.
            let mut done: HashSet<Option<usize>> = HashSet::new();
            for &ctx in &context {
                if !done.insert(ctx) {
                    continue;
                }
                let mut parents = vec![ctx];
                if step.descendant {
                    let mut stack: Vec<usize> = children(doc, ctx).to_vec();
                    while let Some(n) = stack.pop() {
                        if doc.nodes[n].kind == XKind::Element && done.insert(Some(n)) {
                            parents.push(Some(n));
                            stack.extend(&doc.nodes[n].children);
                        }
                    }
                }
                for parent in parents {
                    if let Test::Attr(want) = &step.test {
                        if let Some(p) = parent {
                            for (a, attr) in doc.nodes[p].attrs.iter().enumerate() {
                                if named(want, &attr.name, &attr.display) {
                                    out.insert(Side::Attr(p, a));
                                }
                            }
                        }
                        continue;
                    }
                    let mut matches: Vec<usize> = children(doc, parent)
                        .iter()
                        .copied()
                        .filter(|&c| {
                            let n = &doc.nodes[c];
                            match &step.test {
                                Test::Element(w) => {
                                    n.kind == XKind::Element && named(w, &n.name, &n.display)
                                }
                                Test::Text => n.kind == XKind::Text,
                                Test::Comment => n.kind == XKind::Comment,
                                Test::Attr(_) => false,
                            }
                        })
                        .collect();
                    for pred in &step.preds {
                        matches = match pred {
                            Pred::Nth(n) => matches.get(n - 1).copied().into_iter().collect(),
                            Pred::Has(a) => matches
                                .into_iter()
                                .filter(|&c| {
                                    doc.nodes[c]
                                        .attrs
                                        .iter()
                                        .any(|x| named(a, &x.name, &x.display))
                                })
                                .collect(),
                            Pred::Equals(a, v) => matches
                                .into_iter()
                                .filter(|&c| {
                                    doc.nodes[c]
                                        .attrs
                                        .iter()
                                        .any(|x| named(a, &x.name, &x.display) && &x.value == v)
                                })
                                .collect(),
                        };
                    }
                    for m in matches {
                        if seen.insert(m) {
                            next.push(Some(m));
                        }
                    }
                }
            }
            if k + 1 == path.len() {
                out.extend(next.iter().flatten().map(|&n| Side::Node(n)));
            }
            context = next;
        }
        out
    }

    fn children(doc: &XmlDoc, node: Option<usize>) -> &[usize] {
        match node {
            Some(n) => &doc.nodes[n].children,
            None => &doc.top,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(a: &str, b: &str) -> TreeDiff {
        diff_with(a, b, "")
    }
    fn diff_with(a: &str, b: &str, keys: &str) -> TreeDiff {
        TreeDiff::compare(
            a,
            b,
            &TreeOptions::parse(keys).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
    /// A row as `kind name/value | kind name/value` for both sides.
    fn show(d: &TreeDiff, r: usize) -> String {
        let side = |s: usize| match d.rows[r].sides[s] {
            None => "-".to_owned(),
            Some(Side::Attr(..)) => {
                let a = d.attr(s, r).unwrap();
                format!("@{}={}", a.display, a.value)
            }
            Some(Side::Node(n)) => {
                let node = &d.docs[s].nodes[n];
                match node.kind {
                    XKind::Element => format!("<{}>", node.display),
                    _ => format!("{:?}", node.value),
                }
            }
        };
        format!("{} | {}", side(0), side(1))
    }
    fn changes(d: &TreeDiff) -> Vec<String> {
        d.targets
            .iter()
            .map(|&r| {
                let row = &d.rows[r];
                format!(
                    "{:?}{}{} {}",
                    row.kind,
                    if row.moved.is_some() { " moved" } else { "" },
                    if row.ambiguous { " ?" } else { "" },
                    show(d, r)
                )
            })
            .collect()
    }

    #[test]
    fn identical_documents_have_no_changes_and_agree_with_the_semantic_view() {
        let cases = [
            ("<r a='1' b='2'><x/>t</r>", "<r b='2' a='1'><x></x>t</r>"),
            (
                "<p:r xmlns:p='urn:a'><p:x/></p:r>",
                "<q:r xmlns:q='urn:a'><q:x/></q:r>",
            ),
            ("<r><![CDATA[a<b]]></r>", "<r>a&lt;b</r>"),
            ("<r>\n  <x/>\n</r>", "<r>\n    <x/>\n</r>"),
            ("<r><!--a--></r>", "<r><!--b--></r>"),
            ("<r><?pi x?></r>", "<r><?pi y?></r>"),
            ("<r a=''/>", "<r/>"),
            ("<r><x/></r>", "<r><x>  </x></r>"),
        ];
        for (a, b) in cases {
            let tree = diff(a, b);
            let semantic = super::super::compare(a, b, Format::Xml);
            assert!(semantic.errors.is_empty(), "{a} {b}");
            assert_eq!(tree.identical(), semantic.identical(), "{a} vs {b}");
        }
    }

    #[test]
    fn inserting_a_first_sibling_adds_one_row_and_changes_nothing_else() {
        let d = diff(
            "<r><item id='1'>a</item><item id='2'>b</item></r>",
            "<r><item id='0'>z</item><item id='1'>a</item><item id='2'>b</item></r>",
        );
        assert_eq!(changes(&d), ["Added - | <item>"]);
        let semantic = super::super::compare(
            "<r><item id='1'>a</item><item id='2'>b</item></r>",
            "<r><item id='0'>z</item><item id='1'>a</item><item id='2'>b</item></r>",
            Format::Xml,
        );
        assert!(semantic.changes.len() > 1, "by position every item shifts");
    }

    #[test]
    fn a_changed_attribute_is_the_change_and_its_parents_count_it() {
        let d = diff(
            "<r><cfg><opt name='a' value='1'/></cfg></r>",
            "<r><cfg><opt value='2' name='a'/></cfg></r>",
        );
        assert_eq!(changes(&d), ["Modified @value=1 | @value=2"]);
        let root = d.top[0];
        assert_eq!(d.rows[root].kind, RowKind::Same);
        assert_eq!(d.rows[root].changes, 1);
        let t = d.targets[0];
        assert_eq!(d.path(1, t).unwrap(), "/r/cfg/opt/@value");
        assert_eq!(d.location(1, t).unwrap().1, 1);
    }

    #[test]
    fn an_added_subtree_is_one_change_and_its_inside_is_not_counted() {
        let d = diff("<r/>", "<r><a x='1'><b>t</b></a></r>");
        assert_eq!(changes(&d), ["Added - | <a>"]);
        let a = d.targets[0];
        assert!(d.rows[a].children.iter().all(|&c| d.rows[c].within));
        assert_eq!(d.counts.added, 1);
    }

    #[test]
    fn modified_text_pairs_by_name_between_unchanged_siblings() {
        let d = diff(
            "<r><a>1</a><b>2</b><c>3</c></r>",
            "<r><a>1</a><b>20</b><c>3</c></r>",
        );
        assert_eq!(changes(&d), ["Modified \"2\" | \"20\""]);
        let b = d.rows[d.targets[0]].parent.unwrap();
        assert_eq!(d.rows[b].pairing, Pairing::Name);
        assert!(!d.rows[b].ambiguous);
    }

    #[test]
    fn repeated_siblings_of_unequal_number_are_marked_ambiguous() {
        let d = diff("<r><i>1</i><i>2</i></r>", "<r><i>3</i></r>");
        let rows = changes(&d);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Removed ?")), "{rows:?}");
        let paired = d.rows[d.top[0]].children[0];
        assert!(d.rows[paired].ambiguous);
    }

    #[test]
    fn identical_repeated_subtrees_are_not_claimed_as_a_certain_addition() {
        let d = diff("<r><i/></r>", "<r><i/><i/></r>");
        assert_eq!(changes(&d), ["Added ? - | <i>"]);
    }

    #[test]
    fn unique_subtrees_out_of_order_are_moves() {
        let d = diff(
            "<r><a>1</a><b>2</b><c>3</c></r>",
            "<r><c>3</c><a>1</a><b>2</b></r>",
        );
        assert_eq!(changes(&d), ["Same moved <c> | <c>"]);
        assert_eq!(d.rows[d.targets[0]].moved, Some([3, 1]));
    }

    #[test]
    fn a_key_rule_pairs_reordered_books_with_one_change_and_one_move() {
        let a =
            "<lib><book id='1'><title>A</title></book><book id='2'><title>B</title></book></lib>";
        let b =
            "<lib><book id='2'><title>B</title></book><book id='1'><title>A2</title></book></lib>";
        let d = diff_with(a, b, "book@id");
        let rows = changes(&d);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().any(|r| r.contains(" moved")), "{rows:?}");
        assert!(
            rows.contains(&"Modified \"A\" | \"A2\"".to_owned()),
            "{rows:?}"
        );
        assert_eq!(d.keyed, 2);
        assert!(d.warnings.is_empty());
        // Without the rule, the unchanged book is kept in place and the edited one is removed
        // and added again: no move is claimed.
        let plain = changes(&diff(a, b));
        assert!(plain.iter().all(|r| !r.contains(" moved")), "{plain:?}");
        assert_eq!(plain.len(), 2);
    }

    #[test]
    fn duplicate_or_missing_keys_warn_and_pair_conservatively() {
        let d = diff_with(
            "<lib><book id='1'>a</book><book id='1'>b</book><book>c</book></lib>",
            "<lib><book id='1'>a</book><book id='1'>x</book></lib>",
            "book@id",
        );
        assert_eq!(
            d.warnings,
            [KeyWarning {
                rule: "book@id".into(),
                duplicates: 4,
                missing: 1
            }]
        );
        assert_eq!(d.keyed, 0);
        assert!(d.targets.iter().all(|&t| {
            let r = &d.rows[t];
            r.ambiguous || d.rows[r.parent.unwrap()].ambiguous
        }));
    }

    #[test]
    fn keys_compose_and_follow_paths() {
        let opts = TreeOptions::parse(" lib/book @ id + lang ; dev@name").unwrap();
        assert_eq!(
            opts.keys[0],
            KeyRule {
                element: "lib/book".into(),
                attrs: vec!["id".into(), "lang".into()]
            }
        );
        assert!(TreeOptions::parse("book").is_err());
        let d = diff_with(
            "<lib><book id='1' lang='en'/><book id='1' lang='fr'/></lib>",
            "<lib><book id='1' lang='fr'/><book id='1' lang='en'/></lib>",
            "lib/book@id+lang",
        );
        assert_eq!(d.keyed, 2);
        assert_eq!(d.counts.moved, 1);
        assert!(
            d.rows[d.targets[0]].ambiguous,
            "either of two swapped may have moved"
        );
    }

    #[test]
    fn mixed_content_comments_and_instructions_keep_their_order() {
        let d = diff(
            "<p>Hello <b>big</b> world<!--c--><?go now?></p>",
            "<p>Hello <b>big</b> there<!--c--><?go later?></p>",
        );
        assert_eq!(
            changes(&d),
            [
                "Modified \" world\" | \" there\"",
                "Modified \"go now\" | \"go later\""
            ]
        );
    }

    #[test]
    fn spans_lines_and_names_as_written_are_kept() {
        let src = "<?xml version='1.0'?>\n<!--top-->\n<n:r xmlns:n='urn:x'\n  n:a='1'\n  b=\"2\">\n  <n:c/>\n</n:r>\n";
        let doc = XmlDoc::parse(src).unwrap();
        assert_eq!(doc.top.len(), 2);
        let root = &doc.nodes[doc.top[1]];
        assert_eq!(root.display, "n:r");
        assert_eq!(root.name, "{urn:x}r");
        assert_eq!((root.line, root.col, root.end_line), (3, 1, 7));
        let names: Vec<_> = root
            .attrs
            .iter()
            .map(|a| (a.display.as_str(), a.line, a.col))
            .collect();
        assert_eq!(names, [("n:a", 4, 3), ("b", 5, 3)]);
        assert_eq!(&src[root.attrs[1].bytes.clone()], "b=\"2\"");
        assert_eq!(doc.path(root.children[1], None), "/n:r/n:c");
        let empty = XmlDoc::parse("<r>\r\n<x/></r>").unwrap();
        assert_eq!(empty.nodes[2].line, 2);
    }

    #[test]
    fn unsafe_or_invalid_xml_is_refused() {
        for bad in [
            "<!DOCTYPE r [<!ENTITY e SYSTEM 'http://x'>]><r>&e;</r>",
            "<r a='1' a='2'/>",
            "<r xmlns:p='u' xmlns:q='u' p:a='1' q:a='2'/>",
            "<r>",
        ] {
            assert!(XmlDoc::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn search_finds_text_and_an_xpath_subset() {
        let d = diff(
            "<lib><book id='1'><title>Rust</title></book><book id='2'/></lib>",
            "<lib><book id='1'><title>Rust 2</title></book><book id='3'/></lib>",
        );
        let found = d.search("rust").unwrap();
        assert!(!found.is_empty());
        let hits = d.search("//book[@id='3']").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(show(&d, hits[0]), "<book> | <book>");
        assert_eq!(d.search("/lib/book[1]/title").unwrap().len(), 1);
        assert_eq!(d.search("//book/@id").unwrap().len(), 2);
        assert!(d.search("/lib/book[last()]").is_err());
        assert!(d.search("//a|//b").is_err());
    }

    #[test]
    fn collections_tabulate_repeated_elements() {
        let d = diff_with(
            "<r><d name='a' v='1'/><d name='b' v='2'/><x/></r>",
            "<r><d name='b' v='2'/><d name='a' v='3'/><x/></r>",
            "d@name",
        );
        let root = d.top[0];
        assert_eq!(d.collections(root), [("{}d".to_owned(), "d".to_owned(), 2)]);
        let table = d.collection(root, "{}d");
        assert_eq!(table.columns, ["@name", "@v"]);
        assert!(!table.nested);
        let changed: Vec<_> = table
            .rows
            .iter()
            .filter(|r| r.cells.iter().any(|c| c[0] != c[1]))
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].cells[1], [Some("1".into()), Some("3".into())]);
    }

    #[test]
    fn a_hundred_nested_changes_are_each_a_target_once() {
        let make = |v: usize| {
            let mut s = String::from("<r>");
            for i in 0..100 {
                s.push_str(&format!("<g n='{i}'><h><v>{}</v></h></g>", i * v));
            }
            s.push_str("</r>");
            s
        };
        let d = diff(&make(1), &make(2));
        assert_eq!(d.targets.len(), 99, "i = 0 is equal on both sides");
        let unique: HashSet<_> = d.targets.iter().collect();
        assert_eq!(unique.len(), d.targets.len());
        assert_eq!(d.rows[d.top[0]].changes, 99);
    }

    #[test]
    fn large_documents_align_quickly_and_cancel() {
        let make = |skip: usize| {
            let mut s = String::from("<r>");
            for i in 0..20_000 {
                if i != skip {
                    s.push_str(&format!("<i k='{i}'>{i}</i>"));
                }
            }
            s.push_str("</r>");
            s
        };
        let start = std::time::Instant::now();
        let d = diff(&make(usize::MAX), &make(10_000));
        assert_eq!(changes(&d), ["Removed <i> | -"]);
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        let cancelled = TreeDiff::compare(
            &make(1),
            &make(2),
            &TreeOptions::default(),
            &AtomicBool::new(true),
        );
        assert!(cancelled.is_err());
    }

    #[test]
    fn the_catalog_sample_reads_as_one_insertion_one_move_and_two_edits_with_a_key() {
        let a = include_str!("../../../samples/xml/catalog-left.xml");
        let b = include_str!("../../../samples/xml/catalog-right.xml");
        let d = diff_with(a, b, "book@id");
        assert_eq!(
            changes(&d),
            [
                "Modified \" Book catalog, spring edition \" | \" Book catalog, summer edition \"",
                "Added - | <c:book>",
                "Same moved <cat:book> | <c:book>",
                "Modified \"45.00\" | \"42.00\"",
                "Modified \"XML in Depth\" | \"XML in Depth, 2nd edition\"",
            ]
        );
        assert_eq!(d.keyed, 3);
        assert_eq!(d.counts.ambiguous, 0);
    }

    #[test]
    fn indentation_beside_keyed_reordered_elements_is_not_a_change() {
        let a = "<l>\n  <b id='1'/>\n  <b id='2'/>\n  <b id='3'/>\n</l>";
        let b = "<l>\n  <b id='3'/>\n  <b id='1'/>\n  <b id='2'/>\n</l>";
        let d = diff_with(a, b, "b@id");
        assert_eq!(changes(&d), ["Same moved <b> | <b>"]);
        // indentation that did change is still shown
        let d = diff_with(a, &b.replace("\n  <b id='1'", "\n    <b id='1'"), "b@id");
        assert!(
            changes(&d).iter().any(|c| c.contains("\"\\n    \"")),
            "{:?}",
            changes(&d)
        );
    }

    #[test]
    fn paths_of_every_node_agree_with_one_path_and_find_rows_in_wide_documents() {
        let src = "<!--a--><r xmlns:p='urn:p'><p:i n='1'/>t<p:i/><!--c--><j/><?x y?></r><!--b-->";
        let doc = XmlDoc::parse(src).unwrap();
        let all = doc.paths();
        for (id, path) in all.iter().enumerate() {
            assert_eq!(*path, doc.path(id, None));
        }
        let make = |v: usize| {
            let items: String = (0..12_000)
                .map(|i| format!("\n  <item id='{i}'><name>{}</name></item>", i * v))
                .collect();
            format!("<catalog>{items}\n</catalog>")
        };
        let d = diff(&make(1), &make(2));
        let last = *d.targets.last().unwrap();
        let attr = d.rows[d.rows[d.rows[last].parent.unwrap()].parent.unwrap()].children[0];
        let start = std::time::Instant::now();
        for row in [last, attr] {
            let paths = [d.path(0, row), d.path(1, row)];
            assert_eq!(d.row_at_path(&paths), Some(row));
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(d.row_at_path(&[Some("/nowhere".into()), None]), None);
    }

    #[test]
    fn a_key_rule_never_pairs_elements_of_other_namespaces() {
        let a = "<l><a:book xmlns:a='urn:1' id='1'/></l>";
        let b = "<l><b:book xmlns:b='urn:2' id='1'/></l>";
        let d = diff_with(a, b, "book@id");
        let semantic = super::super::compare(a, b, Format::Xml);
        assert!(!semantic.identical());
        assert!(!d.identical());
        assert_eq!(changes(&d), ["Removed <a:book> | -", "Added - | <b:book>"]);
        assert_eq!(d.keyed, 0);
    }

    #[test]
    fn the_indentation_of_an_inserted_or_removed_element_is_part_of_it() {
        let a = "<r>\n  <a/>\n</r>";
        let b = "<r>\n  <z/>\n  <a/>\n</r>";
        let d = diff(a, b);
        assert_eq!(changes(&d), ["Added - | <z>"]);
        assert_eq!(d.counts.added, 1);
        let d = diff(b, a);
        assert_eq!(changes(&d), ["Removed <z> | -"]);
        // whitespace that changed on its own still is a change
        let d = diff(a, "<r>\n    <a/>\n</r>");
        assert_eq!(changes(&d), ["Modified \"\\n  \" | \"\\n    \""]);
    }

    #[test]
    fn xpath_conditions_may_quote_brackets() {
        let d = diff("<r><b a='x]y'/><b a='z'/></r>", "<r><b a='x]y'/></r>");
        assert_eq!(d.search("//b[@a='x]y']").unwrap().len(), 1);
        assert_eq!(d.search("//b[@a=\"x]y\"][1]").unwrap().len(), 1);
        assert!(d.search("//b[@a='x]").is_err());
    }

    #[test]
    fn nested_descendant_searches_stay_linear() {
        let deep = format!("{}{}", "<a>".repeat(60), "</a>".repeat(60));
        let wide = format!("<r>{}</r>", "<a><b/></a>".repeat(15_000));
        for doc in [deep, wide] {
            let d = diff(&doc, &doc);
            let start = std::time::Instant::now();
            let found = d.search("//a//a//a//b").unwrap();
            let _ = d.search("//*//*//*").unwrap();
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            let _ = found;
        }
    }

    #[test]
    fn collections_show_simple_child_elements_as_columns() {
        let a = include_str!("../../../samples/xml/catalog-left.xml");
        let b = include_str!("../../../samples/xml/catalog-right.xml");
        let d = diff_with(a, b, "book@id");
        let root = d.top[1];
        let groups = d.collections(root);
        assert_eq!(
            groups[0].2, 4,
            "prefixes differ but the expanded name is the same"
        );
        let table = d.collection(root, &groups[0].0);
        assert_eq!(
            table.columns,
            ["@id", "@lang", "c:title", "c:price", "c:price/@currency"]
        );
        assert!(!table.nested);
        let b103 = table
            .rows
            .iter()
            .find(|r| r.cells[0][1].as_deref() == Some("b103"))
            .unwrap();
        assert_eq!(b103.cells[3], [Some("45.00".into()), Some("42.00".into())]);
        let mixed = diff("<r><p>a<b/></p><p>c</p></r>", "<r><p>a<b/></p><p>d</p></r>");
        assert!(mixed.collection(mixed.top[0], "{}p").nested);
    }
}
