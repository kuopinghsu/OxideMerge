use super::{span, Node};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use toml::de::{DeTable, DeValue};

/// Values in the shape of JSON: tables are objects, arrays arrays, and member names `["key"]`,
/// so semantic paths, array keys and impact rules work the same. Integers and floats are
/// compared by value, so `1_000` equals `1000` and `0x10` equals `16`.
pub fn parse(source: &str) -> Result<Node> {
    let root = DeTable::parse(source).map_err(|e| match e.span() {
        Some(at) => {
            let before = &source.as_bytes()[..at.start.min(source.len())];
            let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
            anyhow::anyhow!("TOML line {line}: {}", e.message())
        }
        None => anyhow::anyhow!("TOML: {}", e.message()),
    })?;
    let range = root.span();
    let mut budget = 100_000;
    table(source, root.get_ref(), range, 0, &mut budget)
}

fn table(
    source: &str,
    members: &DeTable,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    let mut n = start(source, range, depth, budget)?;
    n.kind = "object".into();
    for (key, value) in members {
        n.children.insert(
            format!("[{}]", serde_json::to_string(key.get_ref().as_ref())?),
            node(source, value.get_ref(), value.span(), depth + 1, budget)?,
        );
    }
    Ok(n)
}

fn start(
    source: &str,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    if depth > 64 || *budget == 0 {
        bail!("TOML nesting/node limit exceeded");
    }
    *budget -= 1;
    let end = range.end.min(source.len());
    Ok(Node {
        kind: String::new(),
        value: None,
        children: BTreeMap::new(),
        span: span(source, range.start.min(end), end),
    })
}

fn node(
    source: &str,
    value: &DeValue,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    if let DeValue::Table(members) = value {
        return table(source, members, range, depth, budget);
    }
    let mut n = start(source, range, depth, budget)?;
    let (kind, value) = match value {
        DeValue::String(s) => ("string", Some(serde_json::to_string(s.as_ref())?)),
        DeValue::Integer(i) => {
            let digits = i.as_str().replace('_', "");
            let parsed = i128::from_str_radix(&digits, i.radix())
                .map_or_else(|_| i.to_string(), |v| v.to_string());
            ("number", Some(parsed))
        }
        DeValue::Float(f) => {
            let raw = f.as_str().replace('_', "");
            let value = match raw.parse::<f64>() {
                Ok(v) if v.is_finite() => format!("{v:?}"),
                _ => raw,
            };
            ("number", Some(value))
        }
        DeValue::Boolean(b) => ("bool", Some(b.to_string())),
        DeValue::Datetime(d) => ("datetime", Some(d.to_string())),
        DeValue::Array(items) => {
            n.kind = "array".into();
            for (i, item) in items.iter().enumerate() {
                n.children.insert(
                    format!("[{i}]"),
                    node(source, item.get_ref(), item.span(), depth + 1, budget)?,
                );
            }
            return Ok(n);
        }
        DeValue::Table(_) => unreachable!("tables are handled above"),
    };
    n.kind = kind.into();
    n.value = value;
    Ok(n)
}
