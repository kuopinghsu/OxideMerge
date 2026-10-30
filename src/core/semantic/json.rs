use super::{span, Node};
use anyhow::{bail, Result};
use serde::de::{MapAccess, Visitor};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;

struct Members<'a>(Vec<(String, &'a RawValue)>);
impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Members<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON object without duplicate keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut members = Vec::new();
                let mut keys = std::collections::HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if members.len() >= 100_000 {
                        return Err(serde::de::Error::custom(
                            "JSON object member limit exceeded",
                        ));
                    }
                    if !keys.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!("Duplicate key: {key}")));
                    }
                    members.push((key, value));
                }
                Ok(Members(members))
            }
        }
        d.deserialize_map(V)
    }
}
pub fn parse(source: &str) -> Result<Node> {
    let raw: &RawValue = serde_json::from_str(source)?;
    let mut budget = 100_000;
    node(source, raw.get(), 0, &mut budget)
}
fn node(source: &str, raw: &str, depth: usize, budget: &mut usize) -> Result<Node> {
    if depth > 64 || *budget == 0 {
        bail!("JSON nesting/node limit exceeded");
    }
    *budget -= 1;
    let start = raw.as_ptr() as usize - source.as_ptr() as usize;
    let mut n = Node {
        kind: String::new(),
        value: None,
        children: BTreeMap::new(),
        span: span(source, start, start + raw.len()),
    };
    match raw.as_bytes().first() {
        Some(b'{') => {
            n.kind = "object".into();
            let Members(members) = serde_json::from_str(raw)?;
            for (key, value) in members {
                n.children.insert(
                    format!("[{}]", serde_json::to_string(&key)?),
                    node(source, value.get(), depth + 1, budget)?,
                );
            }
        }
        Some(b'[') => {
            n.kind = "array".into();
            let members: Vec<&RawValue> = serde_json::from_str(raw)?;
            for (i, value) in members.iter().enumerate() {
                n.children.insert(
                    format!("[{i}]"),
                    node(source, value.get(), depth + 1, budget)?,
                );
            }
        }
        _ => {
            let value: serde_json::Value = serde_json::from_str(raw)?;
            n.kind = match value {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::String(_) => "string",
                _ => unreachable!(),
            }
            .into();
            n.value = Some(value.to_string());
        }
    }
    Ok(n)
}
