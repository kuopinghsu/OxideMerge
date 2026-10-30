use super::{span, Node};
use anyhow::{bail, Context, Result};
use quick_xml::{events::Event, name::ResolveResult, NsReader};
use std::collections::BTreeMap;
fn name(resolved: ResolveResult<'_>, local: &str) -> Result<String> {
    let uri = match resolved {
        ResolveResult::Bound(ns) => ns.as_ref().to_owned(),
        ResolveResult::Unbound => String::new(),
        ResolveResult::Unknown(_) => bail!("Undeclared XML namespace prefix"),
    };
    Ok(format!("{{{uri}}}{}", local))
}
pub fn parse(source: &str) -> Result<Node> {
    let mut reader = NsReader::from_str(source);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut nodes = 0;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event()?;
        let end = reader.buffer_position() as usize;
        nodes += 1;
        if nodes > 100_000 || stack.len() > 64 {
            bail!("XML nesting/node limit exceeded");
        }
        match event {
            Event::Decl(d) if d.version()?.as_ref() != "1.0" => bail!("Only XML 1.0 is supported"),
            Event::DocType(_) => bail!("XML DTD/entity declarations are unsupported"),
            Event::Start(ref e) | Event::Empty(ref e) => {
                let qname = e.name();
                let (ns, local) = reader.resolver().resolve_element(qname);
                let mut n = Node {
                    kind: "element".into(),
                    value: Some(name(ns, local.as_ref())?),
                    children: BTreeMap::new(),
                    span: span(source, start, end),
                };
                for attr in e.attributes() {
                    let attr = attr?;
                    let raw = attr.key.as_ref();
                    if raw == "xmlns" || raw.starts_with("xmlns:") {
                        continue;
                    }
                    let (ns, local) = reader.resolver().resolve_attribute(attr.key);
                    let key = format!("/@{}", name(ns, local.as_ref())?);
                    let val = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                        .into_owned();
                    if n.children
                        .insert(
                            key,
                            Node {
                                kind: "attribute".into(),
                                value: Some(val),
                                children: BTreeMap::new(),
                                span: span(source, start, end),
                            },
                        )
                        .is_some()
                    {
                        bail!("Duplicate expanded XML attribute");
                    }
                }
                if matches!(event, Event::Empty(_)) {
                    attach(n, &mut stack, &mut root)?;
                } else {
                    stack.push(n);
                }
            }
            Event::End(_) => {
                let mut n = stack.pop().context("Unbalanced XML")?;
                n.span.bytes.end = end;
                attach(n, &mut stack, &mut root)?;
            }
            Event::Text(e) => {
                let value = e.xml10_content().into_owned();
                text(&mut stack, &value, source, start, end)?;
            }
            Event::CData(e) => {
                text(&mut stack, &e.xml10_content(), source, start, end)?;
            }
            Event::GeneralRef(e) => {
                let escaped = format!("&{};", e.as_ref());
                let value = quick_xml::escape::unescape(&escaped)?;
                text(&mut stack, &value, source, start, end)?;
            }
            Event::Comment(e) => {
                if let Some(parent) = stack.last_mut() {
                    let i = parent
                        .children
                        .keys()
                        .filter(|k| !k.starts_with("/@"))
                        .count();
                    parent.children.insert(
                        format!("/comment()[{i}]"),
                        Node {
                            kind: "comment".into(),
                            value: Some(e.xml10_content().into_owned()),
                            children: BTreeMap::new(),
                            span: span(source, start, end),
                        },
                    );
                }
            }
            Event::PI(e) => {
                if let Some(parent) = stack.last_mut() {
                    let i = parent
                        .children
                        .keys()
                        .filter(|k| !k.starts_with("/@"))
                        .count();
                    parent.children.insert(
                        format!("/processing-instruction()[{i}]"),
                        Node {
                            kind: "pi".into(),
                            value: Some(e.as_ref().into()),
                            children: BTreeMap::new(),
                            span: span(source, start, end),
                        },
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        bail!("Unclosed XML element");
    }
    root.context("XML has no root element")
}
fn attach(n: Node, stack: &mut [Node], root: &mut Option<Node>) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        let i = parent
            .children
            .keys()
            .filter(|k| !k.starts_with("/@"))
            .count();
        parent.children.insert(format!("/node()[{i}]"), n);
    } else if root.replace(n).is_some() {
        bail!("XML has multiple roots");
    }
    Ok(())
}
fn text(stack: &mut [Node], value: &str, source: &str, start: usize, end: usize) -> Result<()> {
    let Some(parent) = stack.last_mut() else {
        if !value.trim().is_empty() {
            bail!("Text outside XML root");
        }
        return Ok(());
    };
    let count = parent
        .children
        .keys()
        .filter(|k| !k.starts_with("/@"))
        .count();
    let previous = format!("/node()[{}]", count.saturating_sub(1));
    if let Some(last) = parent
        .children
        .get_mut(&previous)
        .filter(|n| n.kind == "text")
    {
        last.value.as_mut().unwrap().push_str(value);
        last.span.bytes.end = end;
    } else {
        parent.children.insert(
            format!("/node()[{count}]"),
            Node {
                kind: "text".into(),
                value: Some(value.into()),
                children: BTreeMap::new(),
                span: span(source, start, end),
            },
        );
    }
    Ok(())
}
