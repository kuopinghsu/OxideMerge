//! Reformats JSON and XML so that layout differences (indentation, line breaks, key order)
//! do not show up as changes.

use std::fmt::Write;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Json,
    Xml,
}

const JSON_EXTS: &[&str] = &["json", "geojson", "jsonl", "har", "ipynb", "webmanifest"];
const XML_EXTS: &[&str] = &[
    "xml", "xsd", "xsl", "xslt", "svg", "xaml", "csproj", "vbproj", "vcxproj", "props", "targets",
    "resx", "nuspec", "wsdl", "pom", "plist", "config", "manifest", "rss", "atom", "gpx", "kml",
];

/// The format of a file, from its extension, or else from how its text starts.
pub fn detect(path: Option<&Path>, text: &str) -> Option<Format> {
    if let Some(ext) = path.map(super::text::extension) {
        if JSON_EXTS.contains(&ext.as_str()) {
            return Some(Format::Json);
        }
        if XML_EXTS.contains(&ext.as_str()) {
            return Some(Format::Xml);
        }
    }
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("<?xml") {
        Some(Format::Xml)
    } else if (head.starts_with('{') || head.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        Some(Format::Json)
    } else {
        None
    }
}

/// `text` reformatted, or `None` when it is not valid in that format.
pub fn format(text: &str, format: Format) -> Option<String> {
    match format {
        Format::Json => json(text),
        Format::Xml => xml(text),
    }
}

/// JSON with two-space indentation and object keys in alphabetical order. A file of one value
/// per line (JSON Lines) is formatted value by value.
fn json(text: &str) -> Option<String> {
    let mut out = String::new();
    let values: Vec<serde_json::Value> = serde_json::Deserializer::from_str(text)
        .into_iter()
        .collect::<Result<_, _>>()
        .ok()?;
    if values.is_empty() {
        return None;
    }
    for v in &values {
        write_json(&mut out, v, 0);
        out.push('\n');
    }
    Some(out)
}

fn write_json(out: &mut String, v: &serde_json::Value, depth: usize) {
    use serde_json::Value;
    let pad = |out: &mut String, d: usize| {
        for _ in 0..d {
            out.push_str("  ");
        }
    };
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                pad(out, depth + 1);
                write_json(out, item, depth + 1);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            pad(out, depth);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push_str("{\n");
            for (i, k) in keys.iter().enumerate() {
                pad(out, depth + 1);
                let _ = write!(out, "{}: ", Value::String((*k).clone()));
                write_json(out, &map[*k], depth + 1);
                out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
            }
            pad(out, depth);
            out.push('}');
        }
        other => {
            let _ = write!(out, "{other}");
        }
    }
}

/// XML with one element per line, indented by two spaces. Text between tags is trimmed;
/// comments, CDATA and processing instructions are kept.
fn xml(text: &str) -> Option<String> {
    use quick_xml::events::{BytesText, Event};
    let mut reader = quick_xml::Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut writer = quick_xml::Writer::new_with_indent(Vec::new(), b' ', 2);
    let mut elements = 0;
    // Text arrives in pieces split at entity references; it is trimmed as a whole, in its
    // escaped form, and dropped when it is only the whitespace between tags.
    let mut run = String::new();
    loop {
        let event = reader.read_event().ok()?;
        match &event {
            Event::Text(t) => {
                run.push_str(t);
                continue;
            }
            Event::GeneralRef(r) => {
                run.push('&');
                run.push_str(r);
                run.push(';');
                continue;
            }
            _ => {}
        }
        let piece = run.trim();
        if !piece.is_empty() {
            writer
                .write_event(Event::Text(BytesText::from_escaped(piece)))
                .ok()?;
        }
        run.clear();
        match event {
            Event::Eof => break,
            e => {
                if matches!(e, Event::Start(_) | Event::Empty(_)) {
                    elements += 1;
                }
                writer.write_event(e).ok()?;
            }
        }
    }
    if elements == 0 {
        return None;
    }
    let mut s = String::from_utf8(writer.into_inner()).ok()?;
    s.push('\n');
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_json_and_xml_the_same_way_whatever_their_layout() {
        let a = r#"{"b": [1, 2], "a": {"y": null, "x": "s"}, "e": {}}"#;
        let b = "{\n    \"a\": {\"x\": \"s\", \"y\": null},\n \"e\": {},\n\t\"b\": [1,\n 2]\n}";
        let fa = format(a, Format::Json).unwrap();
        assert_eq!(fa, format(b, Format::Json).unwrap());
        assert_eq!(
            fa,
            "{\n  \"a\": {\n    \"x\": \"s\",\n    \"y\": null\n  },\n  \"b\": [\n    1,\n    2\n  ],\n  \"e\": {}\n}\n"
        );
        assert!(format("{oops", Format::Json).is_none());
        assert_eq!(
            format("{\"a\":1}\n{\"b\":2}\n", Format::Json).unwrap(),
            "{\n  \"a\": 1\n}\n{\n  \"b\": 2\n}\n"
        );

        let x = "<?xml version=\"1.0\"?><root><a k=\"v\">text</a><!-- c --><b/></root>";
        let fx = format(x, Format::Xml).unwrap();
        assert_eq!(
            fx,
            "<?xml version=\"1.0\"?>\n<root>\n  <a k=\"v\">text</a>\n  <!-- c -->\n  <b/>\n</root>\n"
        );
        let y =
            "<?xml version=\"1.0\"?>\n<root>\n<a k=\"v\">\n  text\n</a>\n<!-- c -->\n<b/></root>";
        assert_eq!(format(y, Format::Xml).unwrap(), fx);
        assert!(format("<a><b></a>", Format::Xml).is_none());
        assert_eq!(
            format("<a>\n  x &amp; y &lt;z&gt;\n</a>", Format::Xml).unwrap(),
            "<a>x &amp; y &lt;z&gt;</a>\n"
        );

        assert_eq!(detect(Some(Path::new("x.JSON")), ""), Some(Format::Json));
        assert_eq!(
            detect(None, "  <?xml version='1.0'?><a/>"),
            Some(Format::Xml)
        );
        assert_eq!(detect(None, "[1, 2]"), Some(Format::Json));
        assert_eq!(detect(Some(Path::new("x.txt")), "{ not json"), None);
    }
}
