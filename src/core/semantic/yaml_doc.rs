use super::{span, Node};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use yaml_rust::parser::{Event, Parser};
use yaml_rust::scanner::{Scanner, TScalarStyle, Token, TokenType};

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 100_000;

enum Frame {
    Sequence(Node),
    /// A mapping, the key whose value comes next, and each key's name as written.
    Mapping(Node, Option<String>, BTreeMap<String, String>),
}

/// Refuses tags on mappings and sequences other than `!!map` and `!!seq`, which the parser
/// does not report.
fn check_collection_tags(source: &str) -> Result<()> {
    // The tag, and whether it is the value of a mapping key: only there does a `-` start a
    // sequence rather than the next item after an empty node.
    let mut tag: Option<(String, String, bool)> = None;
    let mut after_value = false;
    for Token(_, token) in Scanner::new(source.chars()) {
        let value = matches!(token, TokenType::Value);
        let properties = matches!(token, TokenType::Tag(..) | TokenType::Anchor(_));
        match token {
            TokenType::Tag(handle, suffix) => tag = Some((handle, suffix, after_value)),
            TokenType::Anchor(_) => {}
            TokenType::BlockEntry if tag.as_ref().is_some_and(|&(_, _, keyed)| !keyed) => {
                tag = None
            }
            TokenType::BlockSequenceStart
            | TokenType::BlockMappingStart
            | TokenType::FlowSequenceStart
            | TokenType::FlowMappingStart
            | TokenType::BlockEntry => {
                if let Some((handle, suffix, _)) = tag.take() {
                    if !matches!(
                        (handle.as_str(), suffix.as_str()),
                        ("!!", "map" | "seq") | ("", "!")
                    ) {
                        bail!("YAML tag {handle}{suffix} is not supported");
                    }
                }
            }
            _ => tag = None,
        }
        after_value = value || after_value && properties;
    }
    Ok(())
}

/// One YAML document in the shape of JSON: mappings are objects with `["key"]` members and
/// sequences are arrays. Plain scalars follow the YAML 1.2 core schema, so `yes` and `on` are
/// strings, and keys are compared by value, so `0x10` and `16` are one key (and a mapping with
/// both `0x10` and `"16"` is refused). Aliases, several
/// documents, complex keys and tags other than the core ones are refused rather than guessed
/// at. The parser is driven event by event, so nesting is bounded here and not by the stack.
pub fn parse(source: &str) -> Result<Node> {
    if source.len() > super::MAX_CONFIG_BYTES {
        bail!("YAML input exceeds 4 MiB");
    }
    check_collection_tags(source)?;
    let offsets: Option<Vec<usize>> = (!source.is_ascii()).then(|| {
        source
            .char_indices()
            .map(|(at, _)| at)
            .chain([source.len()])
            .collect()
    });
    let byte = |index: usize| match &offsets {
        Some(o) => o.get(index).copied().unwrap_or(source.len()),
        None => index.min(source.len()),
    };
    let mut parser = Parser::new(source.chars());
    let mut stack: Vec<Frame> = Vec::new();
    let mut root = None;
    let mut documents = 0;
    let mut nodes = 0usize;
    loop {
        let (event, mark) = parser.next().map_err(|e| anyhow!("YAML: {e}"))?;
        let at = byte(mark.index());
        let node = match event {
            Event::StreamEnd => break,
            Event::StreamStart | Event::Nothing | Event::DocumentEnd => continue,
            Event::DocumentStart => {
                documents += 1;
                if documents > 1 {
                    bail!("Several YAML documents in one file are not supported");
                }
                continue;
            }
            Event::Alias(_) => bail!("YAML aliases are not supported"),
            Event::Scalar(value, style, _, tag) => {
                if let Some(Frame::Mapping(map, key @ None, written)) = stack.last_mut() {
                    // The parser places a block mapping after its first key.
                    map.span.bytes.start = map.span.bytes.start.min(at);
                    let shown = match style {
                        TScalarStyle::Plain => value.clone(),
                        _ => serde_json::to_string(&value)?,
                    };
                    let name = key_text(value, style, tag)?;
                    match written.insert(name.clone(), shown.clone()) {
                        Some(other) if other != shown => {
                            bail!("YAML keys {other} and {shown} are not supported in one mapping, as both read as {name}")
                        }
                        Some(_) => bail!("Duplicate key: {name}"),
                        None => {}
                    }
                    *key = Some(name);
                    continue;
                }
                scalar(source, at, value, style, tag)?
            }
            Event::SequenceStart(_) | Event::MappingStart(_) => {
                if matches!(stack.last(), Some(Frame::Mapping(_, None, _))) {
                    bail!("Complex YAML keys are not supported");
                }
                if stack.len() >= MAX_DEPTH {
                    bail!("YAML nesting limit exceeded");
                }
                let mapping = matches!(event, Event::MappingStart(_));
                let node = Node {
                    kind: if mapping { "object" } else { "array" }.into(),
                    value: None,
                    children: BTreeMap::new(),
                    span: span(source, at, at),
                };
                stack.push(if mapping {
                    Frame::Mapping(node, None, BTreeMap::new())
                } else {
                    Frame::Sequence(node)
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let mut node = match stack.pop() {
                    Some(Frame::Sequence(node)) | Some(Frame::Mapping(node, None, _)) => node,
                    _ => bail!("YAML mapping ends without a value"),
                };
                let start = node.span.bytes.start;
                let flow = matches!(source.as_bytes().get(start), Some(b'[' | b'{'));
                // The end mark is where the parser looked ahead to, past comments and the
                // separators of an enclosing collection; a block ends at its last member.
                node.span.bytes.end =
                    if flow && matches!(source.as_bytes().get(at), Some(b']' | b'}')) {
                        at + 1
                    } else {
                        node.children
                            .values()
                            .map(|c| c.span.bytes.end)
                            .max()
                            .unwrap_or_else(|| source[..at].trim_end().len())
                            .max(start)
                    };
                node
            }
        };
        nodes += 1;
        if nodes > MAX_NODES {
            bail!("YAML node limit exceeded");
        }
        match stack.last_mut() {
            None if root.is_none() => root = Some(node),
            None => bail!("Several YAML documents in one file are not supported"),
            Some(Frame::Sequence(list)) => {
                let index = list.children.len();
                list.children.insert(format!("[{index}]"), node);
            }
            Some(Frame::Mapping(map, key, _)) => {
                let key = key.take().expect("a key comes before its value");
                map.children.insert(member(&key)?, node);
            }
        }
    }
    // An empty file, or one of only comments, is a null document.
    Ok(root.unwrap_or_else(|| Node {
        kind: "null".into(),
        value: Some("null".into()),
        children: BTreeMap::new(),
        span: span(source, 0, 0),
    }))
}

fn member(key: &str) -> Result<String> {
    Ok(format!("[{}]", serde_json::to_string(key)?))
}

/// The name a mapping key compares by: a plain number, boolean or null by its value.
fn key_text(value: String, style: TScalarStyle, tag: Option<TokenType>) -> Result<String> {
    let plain = match tag_of(&tag)? {
        Some(Tag::Str) => false,
        Some(_) => bail!("Only string tags are supported on YAML keys"),
        None => style == TScalarStyle::Plain,
    };
    Ok(match resolve(&value) {
        ("string", _) => value,
        (_, resolved) if plain => resolved,
        _ => value,
    })
}

/// The core tags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Str,
    Int,
    Float,
    Bool,
    Null,
}

fn tag_of(tag: &Option<TokenType>) -> Result<Option<Tag>> {
    Ok(match tag {
        None => None,
        Some(TokenType::Tag(handle, suffix)) => Some(match (handle.as_str(), suffix.as_str()) {
            ("", "!") | ("!!", "str") => Tag::Str,
            ("!!", "int") => Tag::Int,
            ("!!", "float") => Tag::Float,
            ("!!", "bool") => Tag::Bool,
            ("!!", "null") => Tag::Null,
            (handle, suffix) => bail!("YAML tag {handle}{suffix} is not supported"),
        }),
        Some(_) => bail!("Unexpected YAML tag"),
    })
}

/// A scalar typed by its tag, or for a plain scalar by the YAML 1.2 core schema.
fn scalar(
    source: &str,
    at: usize,
    value: String,
    style: TScalarStyle,
    tag: Option<TokenType>,
) -> Result<Node> {
    let rest = &source[at..];
    let end = match style {
        TScalarStyle::Plain if rest.starts_with(&value) => at + value.len(),
        TScalarStyle::SingleQuoted | TScalarStyle::DoubleQuoted
            if rest.get(1..).is_some_and(|r| r.starts_with(&value))
                && rest.as_bytes().get(value.len() + 1) == rest.as_bytes().first() =>
        {
            at + value.len() + 2
        }
        _ => at,
    };
    let (kind, value) = match tag_of(&tag)? {
        Some(Tag::Str) => ("string", serde_json::to_string(&value)?),
        Some(tag) => {
            let (kind, resolved) = resolve(&value);
            let fits = match tag {
                Tag::Int => kind == "number" && integer(&value).is_some(),
                Tag::Float => {
                    kind == "number" && !value.starts_with("0x") && !value.starts_with("0o")
                }
                Tag::Bool => kind == "bool",
                Tag::Null => kind == "null",
                Tag::Str => true,
            };
            if !fits {
                bail!("YAML value {value:?} does not read as its tag");
            }
            (kind, resolved)
        }
        None if style == TScalarStyle::Plain => resolve(&value),
        None => ("string", serde_json::to_string(&value)?),
    };
    Ok(Node {
        kind: kind.into(),
        value: Some(value),
        children: BTreeMap::new(),
        span: span(source, at, end),
    })
}

/// The text of `value` as a core schema integer, if it is one: in decimal like any number (see
/// `decimal_text`), or as written when too large to read in another base.
fn integer(value: &str) -> Option<String> {
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    let based = |prefix: &str, radix: u32| {
        let rest = value.strip_prefix(prefix).filter(|r| digits(r, radix))?;
        Some(i128::from_str_radix(rest, radix).map_or_else(
            |_| {
                format!(
                    "{prefix}{}",
                    rest.trim_start_matches('0').to_ascii_lowercase()
                )
            },
            |n| super::decimal_text(false, &n.to_string(), 0).unwrap_or_default(),
        ))
    };
    if digits(unsigned, 10) {
        super::decimal_text(value.starts_with('-'), unsigned, 0)
    } else {
        based("0x", 16).or_else(|| based("0o", 8))
    }
}

fn resolve(value: &str) -> (&'static str, String) {
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    match value {
        "" | "~" | "null" | "Null" | "NULL" => return ("null", "null".into()),
        "true" | "True" | "TRUE" => return ("bool", "true".into()),
        "false" | "False" | "FALSE" => return ("bool", "false".into()),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return ("number", "inf".into()),
        "-.inf" | "-.Inf" | "-.INF" => return ("number", "-inf".into()),
        ".nan" | ".NaN" | ".NAN" => return ("number", "nan".into()),
        _ => {}
    }
    if let Some(n) = integer(value) {
        return ("number", n);
    }
    let float = unsigned.chars().any(|c| c.is_ascii_digit())
        && unsigned
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'))
        && unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.');
    if float && value.parse::<f64>().is_ok() {
        ("number", super::float_literal(value))
    } else {
        ("string", serde_json::to_string(value).unwrap_or_default())
    }
}
