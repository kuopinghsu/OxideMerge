//! Opt-in read-only structured comparison. Text ignore rules are deliberately not applied.
use super::analysis;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Range;
pub mod json;
pub mod xml;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Json,
    Xml,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    TypeChanged,
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
    pub kind: ChangeKind,
    pub before: Option<String>,
    pub after: Option<String>,
    pub spans: [Option<SourceSpan>; 2],
    pub confidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticReport {
    pub version: u32,
    pub format: Format,
    pub changes: Vec<Change>,
    pub errors: Vec<String>,
    pub policies: Vec<String>,
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
pub fn parse(source: &str, format: Format) -> Result<Node> {
    if source.len() as u64 > analysis::MAX_FILE {
        bail!("Structured input exceeds 16 MiB");
    }
    let mut root = match format {
        Format::Json => json::parse(source)?,
        Format::Xml => xml::parse(source)?,
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
fn compare_node(
    path: &str,
    a: Option<&Node>,
    b: Option<&Node>,
    out: &mut Vec<Change>,
    budget: &mut usize,
) -> Result<()> {
    if path.len() > 4096 {
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
        let cost = path.len()
            + a.map_or(0, |node| node.span.bytes.len())
            + b.map_or(0, |node| node.span.bytes.len())
            + 256;
        if cost > *budget {
            bail!("Semantic changes exceed the 32 MiB comparison budget; use Text view");
        }
        *budget -= cost;
        let before = a.map(describe);
        let after = b.map(describe);
        out.push(Change {
            id: analysis::identity(&(path, a.map(node_fingerprint), b.map(node_fingerprint))),
            path: path.into(),
            kind,
            before,
            after,
            spans: [a.map(|n| n.span.clone()), b.map(|n| n.span.clone())],
            confidence: "parser_exact".into(),
        });
        return Ok(());
    }
    if let (Some(a), Some(b)) = (a, b) {
        for key in a
            .children
            .keys()
            .chain(b.children.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            compare_node(
                &format!("{path}{key}"),
                a.children.get(key),
                b.children.get(key),
                out,
                budget,
            )?;
        }
    }
    Ok(())
}
pub fn compare(a: &str, b: &str, format: Format) -> SemanticReport {
    let (a_source, b_source) = (a, b);
    let mut report = SemanticReport { version: 1, format, changes: Vec::new(), errors: Vec::new(), policies: vec!["Read-only; text comparison rules are not semantic path rules and are not applied.".into(), "JSON objects unordered; arrays ordered; duplicate keys rejected. XML attributes unordered; child/mixed-content order and text whitespace significant; expanded namespace names; comments included. No external entities/DTDs.".into()] };
    let parsed = [parse(a, format), parse(b, format)];
    for (side, p) in parsed.iter().enumerate() {
        if let Err(e) = p {
            report
                .errors
                .push(format!("Side {}: {e:#}; use Text view", side + 1));
        }
    }
    if let [Ok(a), Ok(b)] = &parsed {
        let mut budget = analysis::MAX_REPORT;
        if let Err(error) = compare_node("$", Some(a), Some(b), &mut report.changes, &mut budget) {
            report.changes.clear();
            report.errors.push(error.to_string());
            return report;
        }
        for change in &mut report.changes {
            if let Some(span) = &change.spans[0] {
                if let Some(value) = a_source.get(span.bytes.clone()) {
                    change.before = Some(value.to_owned());
                }
            }
            if let Some(span) = &change.spans[1] {
                if let Some(value) = b_source.get(span.bytes.clone()) {
                    change.after = Some(value.to_owned());
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
