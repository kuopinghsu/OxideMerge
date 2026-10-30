use super::{span, Node};
use anyhow::{bail, Context, Result};
use quick_xml::{events::Event, name::ResolveResult, NsReader};
use std::collections::BTreeMap;

fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}
fn name_start(c: char) -> bool {
    matches!(c, '_' | 'A'..='Z' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}
fn check_name(name: &str) -> Result<()> {
    if name.len() > 1024 {
        bail!("XML name exceeds the semantic adapter limit; use Text view");
    }
    let parts: Vec<_> = name.split(':').collect();
    if parts.len() > 2 || parts.iter().any(|part| {
        let mut chars = part.chars();
        !chars.next().is_some_and(name_start) || chars.any(|c| !(name_start(c) || matches!(c, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')))
    }) { bail!("Invalid XML qualified name"); }
    Ok(())
}
fn name(resolved: ResolveResult<'_>, local: &str) -> Result<String> {
    let uri = match resolved {
        ResolveResult::Bound(ns) => {
            if ns.as_ref().len() + local.len() > 1024 {
                bail!("Expanded XML name exceeds 1 KiB; use Text view");
            }
            let raw = ns
                .as_ref()
                .replace("\r\n", " ")
                .replace(['\r', '\n', '\t'], " ");
            quick_xml::escape::unescape(&raw)?.into_owned()
        }
        ResolveResult::Unbound => String::new(),
        ResolveResult::Unknown(_) => bail!("Undeclared XML namespace prefix"),
    };
    Ok(format!("{{{uri}}}{local}"))
}
fn scalar(kind: &str, value: String, source: &str, start: usize, end: usize) -> Node {
    Node {
        kind: kind.into(),
        value: Some(value),
        children: BTreeMap::new(),
        span: span(source, start, end),
    }
}
// Attributes live in the same map but do not consume a child-node position.
// Track that position while parsing instead of rescanning every preceding sibling.
struct Frame {
    node: Node,
    next_child: usize,
}
impl Frame {
    fn push(&mut self, node: Node) {
        self.node
            .children
            .insert(format!("/node()[{}]", self.next_child), node);
        self.next_child += 1;
    }
}

pub fn parse(source: &str) -> Result<Node> {
    if !source.chars().all(xml_char) {
        bail!("Invalid XML 1.0 character");
    }
    let mut reader = NsReader::from_str(source);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    let mut stack: Vec<Frame> = Vec::new();
    let mut root = None;
    let mut document = Node {
        kind: "document".into(),
        value: None,
        children: BTreeMap::new(),
        span: span(source, 0, source.len()),
    };
    let mut nodes = 0;
    let mut declaration = false;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event()?;
        let end = reader.buffer_position() as usize;
        nodes += 1;
        if nodes > 100_000 || stack.len() > 64 {
            bail!("XML nesting/node limit exceeded");
        }
        match event {
            Event::Decl(d) => {
                if declaration
                    || start > 3
                    || root.is_some()
                    || !stack.is_empty()
                    || d.version()?.as_ref() != "1.0"
                {
                    bail!("Invalid/unsupported XML declaration; only XML 1.0 is supported");
                }
                declaration = true;
            }
            Event::DocType(_) => bail!("XML DTD/entity declarations are unsupported"),
            Event::Start(ref e) | Event::Empty(ref e) => {
                let qname = e.name();
                check_name(qname.as_ref())?;
                let (ns, local) = reader.resolver().resolve_element(qname);
                let mut node = scalar("element", name(ns, local.as_ref())?, source, start, end);
                let mut attributes = std::collections::HashSet::new();
                for attr in e.attributes().with_checks(false) {
                    nodes += 1;
                    if nodes > 100_000 {
                        bail!("XML node/attribute limit exceeded");
                    }
                    let attr = attr?;
                    if !attributes.insert(attr.key.as_ref().to_owned()) {
                        bail!("Duplicate XML attribute");
                    }
                    let raw = attr.key.as_ref();
                    check_name(raw)?;
                    let val = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                        .into_owned();
                    if !val.chars().all(xml_char) {
                        bail!("Invalid XML attribute character");
                    }
                    if raw == "xmlns" || raw.starts_with("xmlns:") {
                        if (raw != "xmlns:xml" && val == "http://www.w3.org/XML/1998/namespace")
                            || raw == "xmlns:xmlns"
                            || val == "http://www.w3.org/2000/xmlns/"
                        {
                            bail!("Reserved XML namespace binding");
                        }
                        continue;
                    }
                    let (ns, local) = reader.resolver().resolve_attribute(attr.key);
                    let key = format!("/@{}", name(ns, local.as_ref())?);
                    if node
                        .children
                        .insert(key, scalar("attribute", val, source, start, end))
                        .is_some()
                    {
                        bail!("Duplicate expanded XML attribute");
                    }
                }
                if matches!(event, Event::Empty(_)) {
                    attach(node, &mut stack, &mut root)?;
                } else {
                    stack.push(Frame {
                        node,
                        next_child: 0,
                    });
                }
            }
            Event::End(_) => {
                let mut node = stack.pop().context("Unbalanced XML")?.node;
                node.span.bytes.end = end;
                attach(node, &mut stack, &mut root)?;
            }
            Event::Text(e) => text(&mut stack, &e.xml10_content(), source, start, end)?,
            Event::CData(e) => {
                if stack.is_empty() {
                    bail!("CDATA outside XML root");
                }
                text(&mut stack, &e.xml10_content(), source, start, end)?;
            }
            Event::GeneralRef(e) => {
                if stack.is_empty() {
                    bail!("Character reference outside XML root");
                }
                let escaped = format!("&{};", e.as_ref());
                let value = quick_xml::escape::unescape(&escaped)?;
                text(&mut stack, &value, source, start, end)?;
            }
            Event::Comment(e) => misc(
                scalar(
                    "comment",
                    e.xml10_content().into_owned(),
                    source,
                    start,
                    end,
                ),
                &mut stack,
                &mut document,
                root.is_some(),
            ),
            Event::PI(e) => {
                check_name(e.target())?;
                if e.target().eq_ignore_ascii_case("xml") {
                    bail!("Reserved XML processing-instruction target");
                }
                misc(
                    scalar("pi", e.as_ref().into(), source, start, end),
                    &mut stack,
                    &mut document,
                    root.is_some(),
                );
            }
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        bail!("Unclosed XML element");
    }
    document
        .children
        .insert("/root".into(), root.context("XML has no root element")?);
    Ok(document)
}
fn misc(node: Node, stack: &mut [Frame], document: &mut Node, after: bool) {
    if let Some(parent) = stack.last_mut() {
        parent.push(node);
    } else {
        let i = document.children.len();
        document.children.insert(
            format!("/{}[{i}]", if after { "after" } else { "before" }),
            node,
        );
    }
}
fn attach(node: Node, stack: &mut [Frame], root: &mut Option<Node>) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        parent.push(node);
    } else if root.replace(node).is_some() {
        bail!("XML has multiple roots");
    }
    Ok(())
}
fn text(stack: &mut [Frame], value: &str, source: &str, start: usize, end: usize) -> Result<()> {
    if !value.chars().all(xml_char) {
        bail!("Invalid XML text character");
    }
    let Some(parent) = stack.last_mut() else {
        if value
            .chars()
            .any(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
        {
            bail!("Text outside XML root");
        }
        return Ok(());
    };
    let count = parent.next_child;
    let previous = format!("/node()[{}]", count.saturating_sub(1));
    if let Some(last) = parent
        .node
        .children
        .get_mut(&previous)
        .filter(|n| n.kind == "text")
    {
        last.value.as_mut().unwrap().push_str(value);
        last.span.bytes.end = end;
    } else {
        parent.push(scalar("text", value.into(), source, start, end));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_positions_exclude_attributes_and_coalesce_text_events() {
        let doc = parse("<r a='1'>a&amp;b<![CDATA[c]]><!--note--><?pi d?><item/>d</r>").unwrap();
        let root = &doc.children["/root"];
        assert_eq!(root.children.len(), 6);
        assert_eq!(root.children["/node()[0]"].value.as_deref(), Some("a&bc"));
        assert_eq!(root.children["/node()[1]"].kind, "comment");
        assert_eq!(root.children["/node()[2]"].kind, "pi");
        assert_eq!(root.children["/node()[3]"].kind, "element");
        assert_eq!(root.children["/node()[4]"].value.as_deref(), Some("d"));
    }

    #[test]
    fn wide_documents_within_the_node_limit_do_not_scan_all_previous_siblings() {
        let source = format!("<r a='1'>{}</r>", "<item/>".repeat(40_000));
        let start = std::time::Instant::now();
        let doc = parse(&source).unwrap();
        assert_eq!(doc.children["/root"].children.len(), 40_001);
        assert_eq!(
            doc.children["/root"].children["/node()[39999]"].kind,
            "element"
        );
        // Generous margin for CI load; the previous quadratic implementation takes minutes.
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }
}
