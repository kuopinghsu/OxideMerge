use super::{span, Node};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use yaml_rust::parser::{Event, Parser};
use yaml_rust::scanner::{TScalarStyle, TokenType};

/// Larger YAML input is left to the text view.
pub const MAX_BYTES: usize = 4 << 20;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 100_000;

enum Frame {
    Sequence(Node),
    /// A mapping and the key whose value comes next.
    Mapping(Node, Option<String>),
}

/// One YAML document in the shape of JSON: mappings are objects with `["key"]` members and
/// sequences are arrays. Plain scalars follow the YAML 1.2 core schema, so `yes` and `on` are
/// strings. Aliases, several documents, complex keys and tags other than the core ones are
/// refused rather than guessed at. The parser is driven event by event, so nesting is bounded
/// here and not by the stack.
pub fn parse(source: &str) -> Result<Node> {
    if source.len() > MAX_BYTES {
        bail!("YAML input exceeds 4 MiB");
    }
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
                if let Some(Frame::Mapping(map, key @ None)) = stack.last_mut() {
                    if map.children.contains_key(&member(&value)?) {
                        bail!("Duplicate key: {value}");
                    }
                    *key = Some(value);
                    continue;
                }
                scalar(source, at, value, style, tag)?
            }
            Event::SequenceStart(_) | Event::MappingStart(_) => {
                if matches!(stack.last(), Some(Frame::Mapping(_, None))) {
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
                    Frame::Mapping(node, None)
                } else {
                    Frame::Sequence(node)
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let mut node = match stack.pop() {
                    Some(Frame::Sequence(node)) | Some(Frame::Mapping(node, None)) => node,
                    _ => bail!("YAML mapping ends without a value"),
                };
                let mut end = source[..at].trim_end().len().max(node.span.bytes.start);
                if matches!(source.as_bytes().get(at), Some(b']' | b'}')) {
                    end = at + 1;
                }
                node.span.bytes.end = end;
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
            Some(Frame::Mapping(map, key)) => {
                let key = key.take().expect("a key comes before its value");
                map.children.insert(member(&key)?, node);
            }
        }
    }
    root.ok_or_else(|| anyhow!("The YAML file holds no document"))
}

fn member(key: &str) -> Result<String> {
    Ok(format!("[{}]", serde_json::to_string(key)?))
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
    let mut plain = style == TScalarStyle::Plain;
    match &tag {
        None => {}
        Some(TokenType::Tag(handle, suffix)) => match (handle.as_str(), suffix.as_str()) {
            ("!", "") | ("!!", "str") => plain = false,
            ("!!", "int" | "float" | "bool" | "null") => plain = true,
            (handle, suffix) => bail!("YAML tag {handle}{suffix} is not supported"),
        },
        Some(_) => bail!("Unexpected YAML tag"),
    }
    let (kind, value) = if plain {
        resolve(&value)
    } else {
        ("string", serde_json::to_string(&value)?)
    };
    Ok(Node {
        kind: kind.into(),
        value: Some(value),
        children: BTreeMap::new(),
        span: span(source, at, end),
    })
}

fn resolve(value: &str) -> (&'static str, String) {
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
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
    let integer = if digits(unsigned, 10) {
        value.parse::<i128>().ok()
    } else if let Some(hex) = value.strip_prefix("0x").filter(|h| digits(h, 16)) {
        i128::from_str_radix(hex, 16).ok()
    } else if let Some(oct) = value.strip_prefix("0o").filter(|o| digits(o, 8)) {
        i128::from_str_radix(oct, 8).ok()
    } else {
        None
    };
    if let Some(n) = integer {
        return ("number", n.to_string());
    }
    let float = unsigned.chars().any(|c| c.is_ascii_digit())
        && unsigned
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'))
        && unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.');
    match value.parse::<f64>() {
        Ok(f) if float && f.is_finite() => ("number", format!("{f:?}")),
        _ => ("string", serde_json::to_string(value).unwrap_or_default()),
    }
}
