//! Jupyter notebooks (`.ipynb`), read as the text of their cells.
//!
//! The file is parsed as it is read rather than into a tree of JSON values first: what a cell
//! carries beyond its own source (outputs, embedded images, metadata) is skipped unread, so a
//! notebook full of plots takes no more memory than its code does.

use super::{check, read_whole, MAX_OUTPUT};
use anyhow::{bail, Context, Result};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The most cells read of one notebook; the rest are skipped unread, so that a crafted file
/// cannot ask for unbounded memory one tiny cell at a time.
pub(super) const MAX_CELLS: usize = 20_000;

/// The cells of the Jupyter notebook at `path`, each under a line naming its kind. Cells are
/// not numbered, so a cell added near the top doesn't change the label of every one below it.
/// Both the current format (nbformat 4) and the older one keeping cells in `worksheets`
/// (nbformat 3) are read; a nbformat 3 heading cell reads as a Markdown heading.
pub(super) fn read(path: &Path, cancel: Option<&AtomicBool>) -> Result<String> {
    let bytes = read_whole(path)?;
    let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let mut text = Cells::default();
    let mut de = serde_json::Deserializer::from_slice(json);
    let parsed = Document {
        text: &mut text,
        cancel,
    }
    .deserialize(&mut de)
    .and_then(|()| de.end());
    check(cancel)?;
    parsed.context("not a valid notebook (invalid JSON)")?;
    if !text.found {
        bail!("not a valid notebook (no cells)");
    }
    let mut out = text.out;
    if text.count > MAX_CELLS {
        out.push('\n');
        out.push_str(&crate::i18n::trf(
            "msg.notebook_truncated",
            &[
                ("max", MAX_CELLS.to_string()),
                ("n", text.count.to_string()),
            ],
        ));
        out.push('\n');
    }
    Ok(out)
}

/// The line put above a cell's text, set apart from it so it is not mistaken for a line of
/// the cell.
pub(super) fn header(key: &'static str) -> String {
    format!("── {} ──", crate::i18n::tr(key))
}

/// The text read so far.
#[derive(Default)]
struct Cells {
    out: String,
    /// Every cell seen, those past [`MAX_CELLS`] included.
    count: usize,
    /// Whether the notebook had a list of cells at all.
    found: bool,
}

impl Cells {
    fn push(&mut self, cell: Cell) {
        self.count += 1;
        if self.count > MAX_CELLS || self.out.len() > MAX_OUTPUT {
            return;
        }
        let mut prefix = String::new();
        let key = match cell.cell_type.as_deref() {
            Some("markdown") => "label.notebook_markdown_cell",
            Some("heading") => {
                let level = cell.level.as_ref().and_then(|l| l.as_u64()).unwrap_or(1);
                prefix = format!("{} ", "#".repeat(level.clamp(1, 6) as usize));
                "label.notebook_markdown_cell"
            }
            Some("code") | None => "label.notebook_code_cell",
            Some(_) => "label.notebook_raw_cell",
        };
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out.push_str(&header(key));
        self.out.push('\n');
        let source = prefix + &cell.source.or(cell.input).unwrap_or_default().0;
        self.out.push_str(&source);
        if !source.ends_with('\n') {
            self.out.push('\n');
        }
    }
}

/// What is read of one cell; everything else it has is skipped.
#[derive(Deserialize)]
struct Cell {
    #[serde(default)]
    cell_type: Option<String>,
    #[serde(default)]
    source: Option<Source>,
    /// A nbformat 3 code cell's source.
    #[serde(default)]
    input: Option<Source>,
    /// A nbformat 3 heading cell's level.
    #[serde(default)]
    level: Option<serde_json::Value>,
}

/// A cell's source: either a single string, or (how Jupyter itself writes a notebook, one
/// entry per line kept) a list of strings to join as they are, since each one but the last
/// already ends with its own line break. Anything else reads as no text.
#[derive(Default)]
struct Source(String);

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Source;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or a list of strings")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Source, E> {
                Ok(Source(s.to_owned()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Source, A::Error> {
                let mut s = String::new();
                while let Some(line) = seq.next_element::<serde_json::Value>()? {
                    if let (Some(line), true) = (line.as_str(), s.len() <= MAX_OUTPUT) {
                        s.push_str(line);
                    }
                }
                Ok(Source(s))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Source, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Source::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Source, E> {
                Ok(Source::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Source, E> {
                Ok(Source::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Source, E> {
                Ok(Source::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Source, E> {
                Ok(Source::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Source, E> {
                Ok(Source::default())
            }
        }
        d.deserialize_any(V)
    }
}

/// The notebook, or one of the worksheets of a nbformat 3 one: its `cells` are read, and the
/// cells of its `worksheets`.
struct Document<'a> {
    text: &'a mut Cells,
    cancel: Option<&'a AtomicBool>,
}

impl<'de> DeserializeSeed<'de> for Document<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Document<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a notebook")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Document { text, cancel } = self;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "cells" => map.next_value_seed(CellList {
                    text: &mut *text,
                    cancel,
                })?,
                "worksheets" => map.next_value_seed(Worksheets {
                    text: &mut *text,
                    cancel,
                })?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// The `worksheets` of a nbformat 3 notebook.
struct Worksheets<'a> {
    text: &'a mut Cells,
    cancel: Option<&'a AtomicBool>,
}

impl<'de> DeserializeSeed<'de> for Worksheets<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Worksheets<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of worksheets")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let Worksheets { text, cancel } = self;
        while seq
            .next_element_seed(Document {
                text: &mut *text,
                cancel,
            })?
            .is_some()
        {}
        Ok(())
    }
}

/// A list of cells, each read as it comes and those past [`MAX_CELLS`] skipped unread.
struct CellList<'a> {
    text: &'a mut Cells,
    cancel: Option<&'a AtomicBool>,
}

impl<'de> DeserializeSeed<'de> for CellList<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for CellList<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of cells")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let CellList { text, cancel } = self;
        text.found = true;
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(de::Error::custom("cancelled"));
            }
            if text.count >= MAX_CELLS {
                match seq.next_element::<IgnoredAny>()? {
                    Some(_) => text.count += 1,
                    None => break,
                }
            } else {
                match seq.next_element::<Cell>()? {
                    Some(cell) => text.push(cell),
                    None => break,
                }
            }
        }
        Ok(())
    }
}
