//! Copying a value of one structured file into the other. Only the text where the value is
//! written changes, so the rest of the file keeps its layout and comments. An edit is kept only
//! when the whole file it gives reads back as exactly the structure expected; otherwise the
//! copy is refused and nothing changes.
use super::{node_fingerprint, parse, segments, Change, ChangeKind, Format, Node};

/// Why a value was not copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A move is a change of the order of the whole array.
    Moved,
    /// A file does not read, or no longer holds the value where the comparison found it.
    Stale,
    /// No edit of the text gives the expected structure.
    NoEdit,
}

/// The text of the second side with the node of `change` made as it is on side `from`: replaced,
/// added or removed.
pub fn copy_change(
    change: &Change,
    sources: [&str; 2],
    format: Format,
    from: usize,
) -> Result<String, Refusal> {
    if change.kind == ChangeKind::Moved {
        return Err(Refusal::Moved);
    }
    let to = 1 - from;
    let held = held(&change.kind);
    copy_value(
        (sources[from], &change.at[from]),
        (sources[to], &change.at[to]),
        format,
        [held[from], held[to]],
    )
}

/// Which sides of a comparison hold the node of a change of `kind`: a side without it names
/// where it would go, which may be the path of another node.
pub(super) fn held(kind: &ChangeKind) -> [bool; 2] {
    match kind {
        ChangeKind::Added => [false, true],
        ChangeKind::Removed => [true, false],
        _ => [true, true],
    }
}

/// `to.0` with its node at path `to.1` made like the node of `from.0` at `from.1`: replaced by
/// it, added where it would go, or removed when `from` has none. `held` tells whether each side
/// holds the node at its path; an element added before another at the same position shifts it.
pub fn copy_value(
    from: (&str, &str),
    to: (&str, &str),
    format: Format,
    held: [bool; 2],
) -> Result<String, Refusal> {
    let (src_text, dst_text) = (from.0, to.0);
    let src = parse(src_text, format).map_err(|_| Refusal::Stale)?;
    let dst = parse(dst_text, format).map_err(|_| Refusal::Stale)?;
    let steps = segments(to.1);
    if format!("${}", steps.concat()) != to.1 {
        return Err(Refusal::Stale);
    }
    let value = match held[0] {
        true => Some(find(&src, from.1).ok_or(Refusal::Stale)?),
        false => None,
    };
    let old = match held[1] {
        true => Some(find_steps(&dst, &steps).ok_or(Refusal::Stale)?),
        false => None,
    };
    let Some((expected, key)) = expect(&dst, &steps, value, held[1]) else {
        return Err(Refusal::Stale);
    };
    let target = node_fingerprint(&expected);
    let parent = find_steps(&dst, &steps[..steps.len().saturating_sub(1)]);
    let edit = Edit {
        src: src_text,
        dst: dst_text,
        format,
    };
    let candidates = match (value, old) {
        (Some(value), Some(old)) => edit.replace(value, old),
        (Some(value), None) => match parent {
            Some(parent) => edit.insert(value, parent, &key),
            None => Vec::new(),
        },
        (None, Some(old)) => edit.remove(old, &key),
        (None, None) => return Err(Refusal::Stale),
    };
    candidates
        .into_iter()
        .find(|text| parse(text, format).is_ok_and(|n| node_fingerprint(&n) == target))
        .ok_or(Refusal::NoEdit)
}

pub(super) fn find<'a>(root: &'a Node, path: &str) -> Option<&'a Node> {
    let steps = segments(path);
    if format!("${}", steps.concat()) != path {
        return None;
    }
    find_steps(root, &steps)
}

fn find_steps<'a>(root: &'a Node, steps: &[&str]) -> Option<&'a Node> {
    steps
        .iter()
        .try_fold(root, |node, step| node.children.get(*step))
}

/// A member key that is a position: `[3]` in an array, `/node()[3]` among XML children.
pub(super) fn position(key: &str) -> Option<(&str, usize)> {
    let open = key.rfind('[')?;
    let prefix = &key[..open];
    let index = key[open + 1..].strip_suffix(']')?.parse().ok()?;
    matches!(prefix, "" | "/node()").then_some((prefix, index))
}

fn positions(node: &Node, prefix: &str) -> usize {
    node.children
        .keys()
        .filter(|k| position(k).is_some_and(|(p, _)| p == prefix))
        .count()
}

/// `dst` with `value` at `steps`, or without the node there when `value` is `None`, and the key
/// the node has (or had) in its parent: a position past the end is the end. `held` tells
/// whether `dst` holds the node there; when not, a position there is where it goes in.
fn expect(dst: &Node, steps: &[&str], value: Option<&Node>, held: bool) -> Option<(Node, String)> {
    let Some((&key, above)) = steps.split_last() else {
        return Some((value?.clone(), String::new()));
    };
    let mut root = dst.clone();
    let parent = above
        .iter()
        .try_fold(&mut root, |node, step| node.children.get_mut(*step))?;
    let had = parent.children.contains_key(key);
    if had != held && (held || position(key).is_none()) {
        return None;
    }
    let key = match (position(key), value, held) {
        (Some((prefix, index)), Some(_), false) => {
            format!("{prefix}[{}]", index.min(positions(parent, prefix)))
        }
        _ => key.to_owned(),
    };
    match (value, held) {
        (Some(value), true) => {
            parent.children.insert(key.clone(), value.clone());
        }
        (Some(value), false) => {
            if let Some((prefix, index)) = position(&key) {
                for i in (index..positions(parent, prefix)).rev() {
                    let moved = parent.children.remove(&format!("{prefix}[{i}]"))?;
                    parent
                        .children
                        .insert(format!("{prefix}[{}]", i + 1), moved);
                }
            }
            parent.children.insert(key.clone(), value.clone());
        }
        (None, true) => {
            parent.children.remove(&key);
            if let Some((prefix, index)) = position(&key) {
                for i in index + 1..=positions(parent, prefix) {
                    let moved = parent.children.remove(&format!("{prefix}[{i}]"))?;
                    parent
                        .children
                        .insert(format!("{prefix}[{}]", i - 1), moved);
                }
            }
        }
        (None, false) => return None,
    }
    Some((root, key))
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// The last byte before `at` that is not white space.
fn prev_word(text: &str, at: usize) -> Option<usize> {
    text.as_bytes()[..at].iter().rposition(|&b| !is_space(b))
}

/// The first byte from `at` that is not white space, or the end.
fn next_word(text: &str, at: usize) -> usize {
    at + text.as_bytes()[at..]
        .iter()
        .position(|&b| !is_space(b))
        .unwrap_or(text.len() - at)
}

fn line_start(text: &str, at: usize) -> usize {
    text.as_bytes()[..at]
        .iter()
        .rposition(|&b| b == b'\n' || b == b'\r')
        .map_or(0, |i| i + 1)
}

/// Just past the line break ending the line of `at`, or the end.
fn line_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes[at..].iter().position(|&b| b == b'\n' || b == b'\r') {
        Some(i) if bytes[at + i] == b'\r' && bytes.get(at + i + 1) == Some(&b'\n') => at + i + 2,
        Some(i) => at + i + 1,
        None => text.len(),
    }
}

/// The spaces and tabs a line starts with.
fn indent(text: &str, line: usize) -> &str {
    let rest = &text[line..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// Where the member whose value starts at `value` starts: at its key, written before a `:` or
/// `=`, when it has one.
fn member_start(text: &str, value: usize) -> usize {
    let bytes = text.as_bytes();
    let Some(separator) = prev_word(text, value).filter(|&i| matches!(bytes[i], b':' | b'='))
    else {
        return value;
    };
    let Some(end) = prev_word(text, separator) else {
        return value;
    };
    match bytes[end] {
        quote @ (b'"' | b'\'') => {
            let mut at = end;
            while let Some(open) = bytes[..at].iter().rposition(|&b| b == quote) {
                let escapes = bytes[..open]
                    .iter()
                    .rev()
                    .take_while(|&&b| b == b'\\')
                    .count();
                if quote == b'\'' || escapes % 2 == 0 {
                    return open;
                }
                at = open;
            }
            value
        }
        _ => bytes[..=end]
            .iter()
            .rposition(|&b| is_space(b) || matches!(b, b'{' | b'[' | b','))
            .map_or(0, |i| i + 1),
    }
}

/// `text` with every line after the first that starts with `from` starting with `to` instead.
fn reindent(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        match line.strip_prefix(from).filter(|_| i > 0) {
            Some(rest) => {
                out.push_str(to);
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// What a line holds before column `at`, as spaces: where a value starting there is aligned.
fn column(text: &str, at: usize) -> String {
    " ".repeat(text[line_start(text, at)..at].chars().count())
}

struct Edit<'a> {
    src: &'a str,
    dst: &'a str,
    format: Format,
}

impl Edit<'_> {
    /// The line break the second side uses.
    fn eol(&self) -> &'static str {
        if self.dst.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// `text` from the first side with the second side's line breaks.
    fn local(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n");
        match self.eol() {
            "\n" => text,
            eol => text.replace('\n', eol),
        }
    }

    fn splice(&self, range: std::ops::Range<usize>, text: &str) -> String {
        let mut out = String::with_capacity(self.dst.len() + text.len());
        out.push_str(&self.dst[..range.start]);
        out.push_str(text);
        out.push_str(&self.dst[range.end..]);
        out
    }

    /// `value`'s text, aligned for a place on the second side where a value starts at `at`:
    /// continuation lines follow the indent of the line or the column of the value.
    fn aligned(&self, value: &Node, at: usize) -> Vec<String> {
        let span = &value.span.bytes;
        let text = self.local(&self.src[span.clone()]);
        let mut out = Vec::new();
        if text.contains('\n') {
            let line = line_start(self.src, span.start);
            for (from, to) in [
                (
                    indent(self.src, line).to_owned(),
                    indent(self.dst, line_start(self.dst, at)).to_owned(),
                ),
                (column(self.src, span.start), column(self.dst, at)),
            ] {
                if from != to {
                    out.push(reindent(&text, &from, &to));
                }
            }
        }
        out.push(text);
        out
    }

    fn replace(&self, value: &Node, old: &Node) -> Vec<String> {
        let at = old.span.bytes.clone();
        if at.is_empty() {
            return Vec::new();
        }
        let mut texts = Vec::new();
        if !value.span.bytes.is_empty() {
            texts.extend(self.aligned(value, at.start));
        }
        if value.children.is_empty() && self.format != Format::Xml {
            texts.extend(value.value.clone());
        }
        texts
            .into_iter()
            .map(|text| self.splice(at.clone(), &text))
            .collect()
    }

    fn remove(&self, old: &Node, key: &str) -> Vec<String> {
        let span = old.span.bytes.clone();
        if span.is_empty() {
            return Vec::new();
        }
        let dst = self.dst;
        let bytes = dst.as_bytes();
        let start = if key.starts_with("[\"") {
            member_start(dst, span.start)
        } else {
            span.start
        };
        let mut ranges = Vec::new();
        // the lines the member is on
        ranges.push(line_start(dst, start)..line_end(dst, span.end));
        // with the comma after or before it
        let after = next_word(dst, span.end);
        let before = prev_word(dst, start);
        if bytes.get(after) == Some(&b',') {
            ranges.push(start..next_word(dst, after + 1));
        } else if let Some(before) = before.filter(|&i| bytes[i] == b',') {
            ranges.push(before..span.end);
        } else if let Some(open) = before.filter(|&i| matches!(bytes[i], b'{' | b'[')) {
            if matches!(bytes.get(after), Some(b'}' | b']')) {
                ranges.push(open + 1..after);
            }
        }
        // an attribute with the space before it
        let spaced = bytes[..start]
            .iter()
            .rposition(|&b| !is_space(b))
            .map_or(0, |i| i + 1);
        ranges.push(spaced..span.end);
        ranges.push(start..span.end);
        ranges
            .into_iter()
            .map(|range| self.splice(range, ""))
            .collect()
    }

    fn insert(&self, value: &Node, parent: &Node, key: &str) -> Vec<String> {
        let span = value.span.bytes.clone();
        if span.is_empty() {
            return Vec::new();
        }
        let (src, dst) = (self.src, self.dst);
        let keyed = key.starts_with("[\"");
        let start = if keyed {
            member_start(src, span.start)
        } else {
            span.start
        };
        let member = self.local(&src[start..span.end]);
        let member_at = |node: &Node| {
            if keyed {
                member_start(dst, node.span.bytes.start)
            } else {
                node.span.bytes.start
            }
        };
        let mut texts = Vec::new();
        if key.starts_with("/@") {
            if let Some(end) = start_tag_end(dst, parent.span.bytes.start) {
                texts.push(self.splice(end..end, &format!(" {member}")));
            }
            return texts;
        }
        // siblings in order; where a position is, before it, else after the last
        let mut siblings: Vec<&Node> = match position(key) {
            Some((prefix, _)) => {
                let mut items: Vec<_> = parent
                    .children
                    .iter()
                    .filter_map(|(k, n)| match position(k) {
                        Some((p, i)) if p == prefix => Some((i, n)),
                        _ => None,
                    })
                    .collect();
                items.sort_by_key(|(i, _)| *i);
                items.into_iter().map(|(_, n)| n).collect()
            }
            None => {
                let mut items: Vec<_> = parent.children.values().collect();
                items.sort_by_key(|n| n.span.bytes.start);
                items
            }
        };
        siblings.retain(|n| !n.span.bytes.is_empty());
        let before = match position(key) {
            Some((_, index)) => siblings.get(index).copied(),
            None => None,
        };
        let eol = self.eol();
        let lines = {
            let first = line_start(src, start);
            let mut block = self.local(&src[first..line_end(src, span.end)]);
            if !block.ends_with('\n') {
                block.push_str(eol);
            }
            (block, indent(src, first).to_owned())
        };
        match (before, siblings.last()) {
            (Some(next), _) => {
                let at = member_at(next);
                if self.format == Format::Xml {
                    texts.push(self.splice(at..at, &member));
                }
                let space = &dst[prev_word(dst, at).map_or(0, |i| i + 1)..at];
                texts.push(self.splice(at..at, &format!("{member},{space}")));
                let line = line_start(dst, at);
                let ind = indent(dst, line);
                let block = reindent(&lines.0, &lines.1, ind);
                let block = format!("{ind}{}", block.trim_start_matches([' ', '\t']));
                texts.push(self.splice(line..line, &block));
            }
            (None, Some(last)) => {
                let at = member_at(last);
                let space = &dst[prev_word(dst, at).map_or(0, |i| i + 1)..at];
                let end = last.span.bytes.end;
                if self.format == Format::Xml {
                    texts.push(self.splice(end..end, &member));
                }
                texts.push(self.splice(end..end, &format!(",{space}{member}")));
                let ind = indent(dst, line_start(dst, at));
                let block = format!(
                    "{ind}{}",
                    reindent(&lines.0, &lines.1, ind).trim_start_matches([' ', '\t'])
                );
                let after = line_end(dst, end);
                let lead = if after == dst.len() && !dst.ends_with(['\n', '\r']) {
                    eol
                } else {
                    ""
                };
                texts.push(self.splice(after..after, &format!("{lead}{block}")));
                if position(key).is_none() {
                    // before the first member, for a table whose last member is a table
                    let first = member_at(siblings[0]);
                    let line = line_start(dst, first);
                    let ind = indent(dst, line);
                    let block = format!(
                        "{ind}{}",
                        reindent(&lines.0, &lines.1, ind).trim_start_matches([' ', '\t'])
                    );
                    texts.push(self.splice(line..line, &block));
                }
            }
            (None, None) => {
                let range = parent.span.bytes.clone();
                let inner = dst.get(range.clone()).unwrap_or("");
                if self.format == Format::Xml {
                    texts.extend(xml_fill(dst, range, &member));
                } else if matches!(inner.as_bytes().first(), Some(b'{' | b'['))
                    && inner
                        .get(1..inner.len().saturating_sub(1))
                        .is_some_and(|s| s.trim().is_empty())
                {
                    texts.push(self.splice(range.start + 1..range.end - 1, &member));
                }
            }
        }
        texts
    }
}

/// Where the start tag beginning at `open` ends: before its `>`, or its `/>` when empty.
fn start_tag_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return Some(if bytes[i - 1] == b'/' { i - 1 } else { i }),
            _ => {}
        }
    }
    None
}

/// The XML element at `range` holding no child nodes, with `child` as its content.
fn xml_fill(text: &str, range: std::ops::Range<usize>, child: &str) -> Option<String> {
    let end = start_tag_end(text, range.start)?;
    let mut out = String::with_capacity(text.len() + child.len() * 2);
    if text.as_bytes()[end] == b'/' {
        let name: String = text[range.start + 1..]
            .chars()
            .take_while(|c| !c.is_whitespace() && !matches!(c, '/' | '>'))
            .collect();
        out.push_str(&text[..end]);
        out.push('>');
        out.push_str(child);
        out.push_str(&format!("</{name}>"));
        out.push_str(&text[end + 2..]);
    } else {
        out.push_str(&text[..=end]);
        out.push_str(child);
        out.push_str(&text[end + 1..]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::{compare, compare_with, Options};
    use super::*;

    /// Copies every change of `a` → `b` from `from` into the other side, one by one, comparing
    /// again after each; the sides end up equal.
    fn converge(a: &str, b: &str, format: Format, options: &Options, from: usize) -> String {
        let mut docs = [a.to_owned(), b.to_owned()];
        for _ in 0..50 {
            let report = compare_with(&docs[0], &docs[1], format, options);
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            let Some(change) = report.changes.iter().find(|c| c.kind != ChangeKind::Moved) else {
                return docs[1 - from].clone();
            };
            let text = copy_change(change, [&docs[0], &docs[1]], format, from)
                .unwrap_or_else(|e| panic!("{e:?}: {change:?}\n{}\n{}", docs[0], docs[1]));
            docs[1 - from] = text;
        }
        panic!("no end: {docs:?}");
    }

    #[test]
    fn values_copy_both_ways_keeping_the_rest_of_the_file() {
        let a = "{\n  \"name\": \"svc\",\n  \"timeout\": 30,\n  \"tags\": [\"a\", \"b\"],\n  \"old\": true\n}\n";
        let b = "{\n  \"name\": \"svc\",\n  \"timeout\": 60,\n  \"tags\": [\"a\", \"c\", \"b\"],\n  \"new\": {\"x\": 1}\n}\n";
        let r = compare(a, b, Format::Json);
        let timeout = r
            .changes
            .iter()
            .find(|c| c.path == r#"$["timeout"]"#)
            .unwrap();
        assert_eq!(
            copy_change(timeout, [a, b], Format::Json, 1).unwrap(),
            a.replace("30", "60")
        );
        let left = converge(a, b, Format::Json, &Options::default(), 1);
        assert!(compare(&left, b, Format::Json).identical(), "{left}");
        assert!(left.contains("\"name\": \"svc\",\n"), "{left}");
        let right = converge(a, b, Format::Json, &Options::default(), 0);
        assert!(compare(a, &right, Format::Json).identical(), "{right}");

        let removed = compare("{\"a\": 1, \"b\": 2}", "{\"b\": 2}", Format::Json);
        assert_eq!(
            copy_change(
                &removed.changes[0],
                ["{\"a\": 1, \"b\": 2}", "{\"b\": 2}"],
                Format::Json,
                1
            )
            .unwrap(),
            "{\"b\": 2}"
        );
        let only = compare("{\"a\": 1}", "{}", Format::Json);
        assert_eq!(
            copy_change(&only.changes[0], ["{\"a\": 1}", "{}"], Format::Json, 0).unwrap(),
            "{\"a\": 1}"
        );
        let moved = compare("[1,2,3]", "[3,1,2]", Format::Json);
        assert_eq!(
            copy_change(&moved.changes[0], ["[1,2,3]", "[3,1,2]"], Format::Json, 0),
            Err(Refusal::Moved)
        );
        // a file edited since the comparison
        assert_eq!(
            copy_change(timeout, [a, "{"], Format::Json, 0),
            Err(Refusal::Stale)
        );
    }

    #[test]
    fn yaml_toml_and_xml_values_copy_in_place() {
        let yaml = [
            "# service\nserver:\n  port: 8080 # http\n  hosts:\n    - a\n    - b\nflag: yes\n",
            "# service\nserver:\n  port: 9090 # http\n  hosts:\n    - a\n    - c\n  tls: true\nflag: no\n",
        ];
        let left = converge(yaml[0], yaml[1], Format::Yaml, &Options::default(), 1);
        assert!(compare(&left, yaml[1], Format::Yaml).identical(), "{left}");
        assert!(
            left.starts_with("# service\n") && left.contains("# http"),
            "{left}"
        );
        let right = converge(yaml[0], yaml[1], Format::Yaml, &Options::default(), 0);
        assert!(
            compare(yaml[0], &right, Format::Yaml).identical(),
            "{right}"
        );

        let toml = [
            "# settings\ntitle = \"x\"\n\n[server]\nport = 8080\nhosts = [\"a\", \"b\"]\n",
            "# settings\ntitle = \"y\"\n\n[server]\nport = 9090\nhosts = [\"a\"]\ntls = true\n",
        ];
        for from in [0, 1] {
            let out = converge(toml[0], toml[1], Format::Toml, &Options::default(), from);
            assert!(compare(&out, toml[from], Format::Toml).identical(), "{out}");
            assert!(out.starts_with("# settings\n"), "{out}");
        }

        let xml = [
            "<r a='1' b='2'>\n  <!-- keep -->\n  <x>old</x>\n</r>\n",
            "<r a='3' c='4'>\n  <!-- keep -->\n  <x>new</x>\n</r>\n",
        ];
        for from in [0, 1] {
            let out = converge(xml[0], xml[1], Format::Xml, &Options::default(), from);
            assert!(compare(&out, xml[from], Format::Xml).identical(), "{out}");
            assert!(out.contains("<!-- keep -->"), "{out}");
        }
        let r = compare(xml[0], xml[1], Format::Xml);
        let attribute = r
            .changes
            .iter()
            .find(|c| c.path.ends_with("/@{}a"))
            .unwrap();
        assert_eq!(attribute.before.as_deref(), Some("a='1'"));
    }

    #[test]
    fn keyed_array_elements_copy_to_where_they_belong() {
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let a = "[\n  {\"id\": 2, \"v\": \"b\"},\n  {\"id\": 1, \"v\": \"a\"}\n]\n";
        let b = "[\n  {\"id\": 3, \"v\": \"c\"},\n  {\"id\": 1, \"v\": \"a\"},\n  {\"id\": 2, \"v\": \"B\"}\n]\n";
        let r = compare_with(a, b, Format::Json, &keyed);
        let modified = r
            .changes
            .iter()
            .find(|c| c.kind == ChangeKind::Modified)
            .unwrap();
        // the element is first on the left and third on the right
        assert_eq!(modified.path, r#"$[2]["v"]"#);
        assert_eq!(modified.at[0], r#"$[0]["v"]"#);
        let out = converge(a, b, Format::Json, &keyed, 1);
        let back = compare_with(&out, b, Format::Json, &keyed);
        assert!(
            back.changes.iter().all(|c| c.kind == ChangeKind::Moved),
            "{out}: {:?}",
            back.changes
        );
        // nested in an element that moved, a move names where it was on the first side
        let r = compare_with(
            r#"[{"id":1,"t":[1,2,3]},{"id":2}]"#,
            r#"[{"id":2},{"id":1,"t":[3,1,2]}]"#,
            Format::Json,
            &keyed,
        );
        let inner = r
            .changes
            .iter()
            .find(|c| c.path.contains("[\"t\"]"))
            .unwrap();
        assert_eq!(inner.path, r#"$[1]["t"][0]"#);
        assert_eq!(inner.from.as_deref(), Some(r#"$[0]["t"][2]"#));
    }

    /// The text of copying the one change of `kind` in `a` → `b` from side `from`, read back.
    fn copy_one(a: &str, b: &str, options: &Options, kind: ChangeKind, from: usize) -> String {
        let r = compare_with(a, b, Format::Json, options);
        let change = r
            .changes
            .iter()
            .find(|c| c.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?}: {:?}", r.changes));
        let text = copy_change(change, [a, b], Format::Json, from).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v.to_string()
    }

    #[test]
    fn an_element_only_one_side_has_goes_in_or_out_without_touching_its_neighbours() {
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let a = r#"[{"id":2,"v":"b"},{"id":1,"v":"a"}]"#;
        let b = r#"[{"id":3,"v":"c"},{"id":1,"v":"a"},{"id":2,"v":"B"}]"#;
        assert_eq!(
            copy_one(a, b, &keyed, ChangeKind::Added, 1),
            r#"[{"id":3,"v":"c"},{"id":2,"v":"b"},{"id":1,"v":"a"}]"#
        );
        assert_eq!(
            copy_one(a, b, &keyed, ChangeKind::Added, 0),
            r#"[{"id":1,"v":"a"},{"id":2,"v":"B"}]"#
        );
        let (a, b) = (b, a);
        assert_eq!(
            copy_one(a, b, &keyed, ChangeKind::Removed, 0),
            r#"[{"id":3,"v":"c"},{"id":2,"v":"b"},{"id":1,"v":"a"}]"#
        );
        assert_eq!(
            copy_one(a, b, &keyed, ChangeKind::Removed, 1),
            r#"[{"id":1,"v":"a"},{"id":2,"v":"B"}]"#
        );
        let plain = Options::default();
        let (short, long) = (r#"{"l":[1,2,3]}"#, r#"{"l":[1,2,3,4]}"#);
        assert_eq!(
            copy_one(short, long, &plain, ChangeKind::Added, 1),
            r#"{"l":[1,2,3,4]}"#
        );
        assert_eq!(
            copy_one(long, short, &plain, ChangeKind::Removed, 1),
            r#"{"l":[1,2,3]}"#
        );
    }
}
