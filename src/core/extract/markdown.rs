//! Word documents, workbooks and tagged PDF files as Markdown blocks, for the formatted compare.
//!
//! A Word document is read as headings, paragraphs with
//! their bold, slanted and struck-through words, list items, links, images and tables. A table
//! keeps one cell for every column of every row: a cell that a merged cell covers is written as
//! [`COVERED_LEFT`] or [`COVERED_ABOVE`], so the cells of two tables still line up column by
//! column and the view can draw the merged cells across the cells they cover.
//!
//! Like the text reading, this is the document as it reads with its changes accepted, without
//! its headers, footers, notes and comments.
//!
//! A workbook is read as a heading and a table for each sheet, merged cells written the same
//! way, its numbers as their number formats show them, and the pictures placed on it.
//!
//! A tagged PDF file is read from the structure it is tagged with, which names its headings,
//! paragraphs, lists, tables and figures and the cells a table cell spans.

use super::{
    check, column_of, optional_part, part, part_name, pdf, read_whole, reference, relationships,
    shared_strings, MARKUP_COMPATIBILITY, MAX_OUTPUT, SHEET_NS, W,
};
use crate::core::web_markdown::{
    destination, escape_cell, escape_line_start, escapes, lay_out, Block, SpanCell, COVERED_ABOVE,
    COVERED_LEFT,
};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// How deep elements are read; what is nested deeper is left out, so that a document cannot
/// make the reading recurse without end.
const MAX_DEPTH: usize = 200;

/// How many elements are read between looks at whether the read has been cancelled.
const CHECK_EVERY: usize = 4096;

/// The most styles a paragraph's style is followed through to the styles it is based on.
const MAX_BASED_ON: usize = 16;

/// The blocks of the Word document at `path`.
pub fn docx_blocks(path: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<Block>> {
    let body = parse(&part(path, "word/document.xml")?, cancel)?;
    let styles = match optional_part(path, "word/styles.xml")? {
        Some(xml) => Styles::read(&parse(&xml, cancel)?),
        None => Styles::default(),
    };
    let numbering = match optional_part(path, "word/numbering.xml")? {
        Some(xml) => Numbering::read(&parse(&xml, cancel)?),
        None => Numbering::default(),
    };
    let links = optional_part(path, "word/_rels/document.xml.rels")?
        .map(|xml| relationships(&xml).into_iter().collect())
        .unwrap_or_default();
    let mut reader = Reader {
        path,
        cancel,
        styles,
        numbering,
        links,
        counters: HashMap::new(),
        images: HashMap::new(),
        described: None,
        blocks: Vec::new(),
        size: 0,
    };
    if let Some(body) = body.child(W, "body") {
        reader.blocks_of(body, 0)?;
    }
    Ok(reader.blocks)
}

// ---------- the element tree ----------

/// An element of a part, with the namespace of its name and its attributes by local name.
#[derive(Default)]
struct Element {
    ns: String,
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    fn child(&self, ns: &str, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.is(ns, name))
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The `w:val` of the child `name`, if it has that child.
    fn val(&self, name: &str) -> Option<&str> {
        self.child(W, name).map(|c| c.attr("val").unwrap_or(""))
    }

    /// Whether the on-off property `name` (such as `w:b`) is there and not turned off.
    fn on(&self, name: &str) -> bool {
        self.val(name)
            .is_some_and(|v| !matches!(v, "0" | "false" | "off" | "none"))
    }

    fn number(&self, name: &str) -> Option<usize> {
        self.val(name).and_then(|v| v.parse().ok())
    }

    /// Every descendant, depth first, as deep as the tree was read.
    fn descendants(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        let mut stack: Vec<&Element> = self.elements().collect();
        stack.reverse();
        while let Some(e) = stack.pop() {
            out.push(e);
            let at = stack.len();
            stack.extend(e.elements());
            stack[at..].reverse();
        }
        out
    }
}

/// Reads a part into an element tree. What another reader shows in place of content it cannot
/// show (`mc:Fallback`) is left out, as the text reading leaves it out.
fn parse(xml: &[u8], cancel: Option<&AtomicBool>) -> Result<Element> {
    use quick_xml::events::Event;
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    let mut stack = vec![Element::default()];
    // Elements left out, counted so that their ends are told from the ends of others.
    let mut skipping = 0usize;
    let mut buf = Vec::new();
    for read in 0.. {
        if read % CHECK_EVERY == 0 {
            check(cancel)?;
        }
        buf.clear();
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(e) => bail!("{e}"),
        };
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                if skipping > 0 {
                    skipping += usize::from(!empty);
                    continue;
                }
                let (ns, name) = reader.resolver().resolve_element(e.name());
                let ns = match ns {
                    quick_xml::name::ResolveResult::Bound(n) => n.as_ref().to_owned(),
                    _ => String::new(),
                };
                let name = name.as_ref().to_owned();
                if (ns == MARKUP_COMPATIBILITY && name == "Fallback") || stack.len() > MAX_DEPTH {
                    skipping = usize::from(!empty);
                    continue;
                }
                let attrs = e
                    .attributes()
                    .flatten()
                    .map(|a| {
                        let key = a.key.local_name();
                        let value = a
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_or_else(|_| a.value.to_string(), |v| v.into_owned());
                        (key.as_ref().to_owned(), value)
                    })
                    .collect();
                let element = Element {
                    ns,
                    name,
                    attrs,
                    children: Vec::new(),
                };
                if empty {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(Node::Element(element));
                    }
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                if skipping > 0 {
                    skipping -= 1;
                    continue;
                }
                if stack.len() > 1 {
                    let done = stack.pop().unwrap_or_default();
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(Node::Element(done));
                    }
                }
            }
            Event::Text(ref e) if skipping == 0 => push_text(&mut stack, &e.xml10_content()),
            Event::CData(ref e) if skipping == 0 => push_text(&mut stack, e.as_ref()),
            Event::GeneralRef(ref e) if skipping == 0 => push_text(&mut stack, &reference(e)),
            Event::Eof => break,
            _ => {}
        }
    }
    // A part cut short still gives what was read of it.
    while stack.len() > 1 {
        let done = stack.pop().unwrap_or_default();
        if let Some(parent) = stack.last_mut() {
            parent.children.push(Node::Element(done));
        }
    }
    // The part's top element, such as `w:document`.
    let root = stack.pop().unwrap_or_default();
    Ok(root
        .children
        .into_iter()
        .find_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
        .unwrap_or_default())
}

fn push_text(stack: &mut [Element], text: &str) {
    if let Some(e) = stack.last_mut() {
        match e.children.last_mut() {
            Some(Node::Text(t)) => t.push_str(text),
            _ => e.children.push(Node::Text(text.to_string())),
        }
    }
}

/// The text an element holds directly.
fn own_text(e: &Element) -> String {
    e.children
        .iter()
        .filter_map(|n| match n {
            Node::Text(t) => Some(t.as_str()),
            Node::Element(_) => None,
        })
        .collect()
}

// ---------- styles and numbering ----------

/// What the paragraph styles of `word/styles.xml` say about headings and lists.
#[derive(Default)]
struct Styles {
    by_id: HashMap<String, Style>,
}

#[derive(Default)]
struct Style {
    based_on: Option<String>,
    /// The heading level its name or outline level gives, from 1.
    level: Option<usize>,
    /// The list it puts its paragraphs in, with the list level.
    numbering: Option<(String, usize)>,
}

impl Styles {
    fn read(root: &Element) -> Styles {
        let mut by_id = HashMap::new();
        for s in root.elements().filter(|e| e.is(W, "style")) {
            if s.attr("type").is_some_and(|t| t != "paragraph") {
                continue;
            }
            let Some(id) = s.attr("styleId") else {
                continue;
            };
            let name = s.val("name").unwrap_or("").to_ascii_lowercase();
            let level = if name == "title" {
                Some(1)
            } else if let Some(n) = name.strip_prefix("heading ") {
                n.trim().parse().ok().filter(|n| (1..=6).contains(n))
            } else {
                s.child(W, "pPr").and_then(outline_level)
            };
            let numbering = s.child(W, "pPr").and_then(list_of);
            by_id.insert(
                id.to_string(),
                Style {
                    based_on: s.val("basedOn").map(str::to_string),
                    level,
                    numbering,
                },
            );
        }
        Styles { by_id }
    }

    /// The first thing `get` finds in style `id` or the styles it is based on.
    fn find<T>(&self, id: &str, get: impl Fn(&Style) -> Option<T>) -> Option<T> {
        let mut id = id;
        for _ in 0..MAX_BASED_ON {
            let style = self.by_id.get(id)?;
            if let Some(found) = get(style) {
                return Some(found);
            }
            id = style.based_on.as_deref()?;
        }
        None
    }
}

/// The heading level an outline level in paragraph properties gives, from 1.
fn outline_level(ppr: &Element) -> Option<usize> {
    ppr.number("outlineLvl").filter(|&l| l < 6).map(|l| l + 1)
}

/// The list and list level of paragraph properties.
fn list_of(ppr: &Element) -> Option<(String, usize)> {
    let num = ppr.child(W, "numPr")?;
    let id = num.val("numId")?.to_string();
    // Number 0 takes a paragraph out of the list its style puts it in.
    (id != "0").then(|| (id, num.number("ilvl").unwrap_or(0).min(8)))
}

/// The lists of `word/numbering.xml`: for each list, whether each level is numbered and where
/// it starts.
#[derive(Default)]
struct Numbering {
    lists: HashMap<String, Vec<(bool, u64)>>,
}

impl Numbering {
    fn read(root: &Element) -> Numbering {
        let mut abstracts: HashMap<&str, Vec<(bool, u64)>> = HashMap::new();
        for a in root.elements().filter(|e| e.is(W, "abstractNum")) {
            let mut levels = vec![(false, 1); 9];
            for l in a.elements().filter(|e| e.is(W, "lvl")) {
                let Some(k) = l.attr("ilvl").and_then(|v| v.parse::<usize>().ok()) else {
                    continue;
                };
                if let Some(level) = levels.get_mut(k) {
                    let format = l.val("numFmt").unwrap_or("bullet");
                    let start = l.val("start").and_then(|v| v.parse().ok()).unwrap_or(1);
                    *level = (!matches!(format, "bullet" | "none" | ""), start);
                }
            }
            if let Some(id) = a.attr("abstractNumId") {
                abstracts.insert(id, levels);
            }
        }
        let mut lists = HashMap::new();
        for n in root.elements().filter(|e| e.is(W, "num")) {
            if let (Some(id), Some(levels)) = (
                n.attr("numId"),
                n.val("abstractNumId").and_then(|a| abstracts.get(a)),
            ) {
                lists.insert(id.to_string(), levels.clone());
            }
        }
        Numbering { lists }
    }
}

// ---------- the document ----------

/// How a run of text is shown.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Format {
    bold: bool,
    italic: bool,
    strike: bool,
}

/// A piece of a paragraph: text in a format, or Markdown written as it is.
enum Piece {
    Text(Format, String),
    Markdown(String),
    Break,
}

struct Reader<'a> {
    path: &'a Path,
    cancel: Option<&'a AtomicBool>,
    styles: Styles,
    numbering: Numbering,
    /// What each relationship of the document leads to.
    links: HashMap<String, String>,
    /// The number each list level got last, by list.
    counters: HashMap<String, Vec<u64>>,
    /// How each image is shown, by relationship and description.
    images: HashMap<(String, Option<String>), String>,
    /// The description of the drawing being read.
    described: Option<String>,
    blocks: Vec<Block>,
    /// The length of the blocks so far, kept within what a text reading would read.
    size: usize,
}

impl Reader<'_> {
    fn emit(&mut self, depth: usize, text: String) {
        if text.trim().is_empty() || self.size > MAX_OUTPUT {
            return;
        }
        self.size += text.len();
        self.blocks.push(Block { depth, text });
    }

    /// The paragraphs and tables of `e`, such as the body, a content control or a text box.
    fn blocks_of(&mut self, e: &Element, depth: usize) -> Result<()> {
        check(self.cancel)?;
        if depth > MAX_DEPTH {
            return Ok(());
        }
        for child in e.elements() {
            match (child.ns.as_str(), child.name.as_str()) {
                (W, "p") => self.paragraph(child)?,
                (W, "tbl") => {
                    let text = self.table(child);
                    self.emit(0, text);
                }
                (W, "sectPr" | "del" | "moveFrom") => {}
                _ => self.blocks_of(child, depth + 1)?,
            }
        }
        Ok(())
    }

    fn paragraph(&mut self, p: &Element) -> Result<()> {
        let ppr = p.child(W, "pPr");
        let style = ppr.and_then(|ppr| ppr.val("pStyle")).unwrap_or("Normal");
        let level = ppr
            .and_then(outline_level)
            .or_else(|| self.styles.find(style, |s| s.level));
        let list = ppr
            .and_then(list_of)
            .or_else(|| self.styles.find(style, |s| s.numbering.clone()));
        let mut pieces = Vec::new();
        let mut boxes = Vec::new();
        self.inline(p, Format::default(), &mut pieces, &mut boxes, 0);
        let breaks = level.is_none() && list.is_none();
        let text = markdown(&pieces, breaks);
        if let Some(level) = level {
            if !text.is_empty() {
                self.emit(0, format!("{} {}", "#".repeat(level), text));
            }
        } else if let Some((id, ilvl)) = list.filter(|_| !text.is_empty()) {
            let marker = self.list_marker(&id, ilvl);
            self.emit(ilvl, format!("{marker} {text}"));
        } else {
            let text: Vec<String> = text.split('\n').map(escape_line_start).collect();
            self.emit(0, text.join("\n"));
        }
        // A text box's paragraphs follow the paragraph it is drawn in.
        for b in boxes {
            self.blocks_of(b, 1)?;
        }
        Ok(())
    }

    /// `- ` for a bulleted list item, its number for a numbered one, counting on from the
    /// item before it in the same list and level.
    fn list_marker(&mut self, id: &str, ilvl: usize) -> String {
        let levels = self.numbering.lists.get(id);
        let (numbered, start) = levels
            .and_then(|l| l.get(ilvl).copied())
            .unwrap_or((false, 1));
        let counters = self
            .counters
            .entry(id.to_string())
            .or_insert_with(|| vec![0; 9]);
        // A deeper level starts again under each item above it.
        for (k, c) in counters.iter_mut().enumerate().skip(ilvl + 1) {
            *c = levels
                .and_then(|l| l.get(k))
                .map_or(0, |&(_, s)| s.saturating_sub(1));
        }
        if counters[ilvl] == 0 {
            counters[ilvl] = start.saturating_sub(1);
        }
        counters[ilvl] += 1;
        if numbered {
            format!("{}.", counters[ilvl])
        } else {
            "-".to_string()
        }
    }

    /// The pieces of text within `e`: runs and what holds runs, such as links, inserted text
    /// and fields. Deleted text and field codes are left out. Text boxes are put in `boxes`.
    fn inline<'e>(
        &mut self,
        e: &'e Element,
        format: Format,
        out: &mut Vec<Piece>,
        boxes: &mut Vec<&'e Element>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        for child in e.elements() {
            if child.ns != W {
                if child.name == "docPr" {
                    // The description of the drawing whose picture follows.
                    self.described = child
                        .attr("descr")
                        .filter(|d| !d.trim().is_empty())
                        .or_else(|| child.attr("title"))
                        .map(str::to_string);
                    continue;
                }
                if child.name == "blip" || child.name == "imagedata" {
                    if let Some(image) = self.image(child) {
                        out.push(Piece::Markdown(image));
                    }
                    continue;
                }
                self.inline(child, format, out, boxes, depth + 1);
                continue;
            }
            match child.name.as_str() {
                "r" => {
                    let mut f = format;
                    if let Some(rpr) = child.child(W, "rPr") {
                        f.bold |= rpr.on("b");
                        f.italic |= rpr.on("i");
                        f.strike |= rpr.on("strike") || rpr.on("dstrike");
                    }
                    self.run(child, f, out, boxes, depth + 1);
                }
                "hyperlink" => {
                    let mut inner = Vec::new();
                    self.inline(child, format, &mut inner, boxes, depth + 1);
                    let text = markdown(&inner, false);
                    let url = child
                        .attr("id")
                        .and_then(|id| self.links.get(id))
                        .filter(|url| url.contains(':'));
                    match url {
                        Some(url) if !text.is_empty() => {
                            out.push(Piece::Markdown(format!("[{text}]({})", destination(url))))
                        }
                        _ => out.push(Piece::Markdown(text)),
                    }
                }
                "pPr" | "rPr" | "del" | "moveFrom" | "instrText" | "delText"
                | "commentReference" | "footnoteReference" | "endnoteReference" => {}
                "txbxContent" => boxes.push(child),
                _ => self.inline(child, format, out, boxes, depth + 1),
            }
        }
    }

    fn run<'e>(
        &mut self,
        r: &'e Element,
        format: Format,
        out: &mut Vec<Piece>,
        boxes: &mut Vec<&'e Element>,
        depth: usize,
    ) {
        for child in r.elements() {
            if child.ns != W {
                self.inline(child, format, out, boxes, depth + 1);
                continue;
            }
            match child.name.as_str() {
                "t" => out.push(Piece::Text(format, own_text(child))),
                "tab" | "ptab" => out.push(Piece::Text(format, " ".into())),
                "br" | "cr" => out.push(Piece::Break),
                "noBreakHyphen" => out.push(Piece::Text(format, "-".into())),
                "sym" => {
                    let c = child
                        .attr("char")
                        .and_then(|c| u32::from_str_radix(c, 16).ok())
                        .and_then(char::from_u32);
                    if let Some(c) = c {
                        out.push(Piece::Text(format, c.to_string()));
                    }
                }
                "drawing" | "pict" | "object" => self.inline(child, format, out, boxes, depth + 1),
                _ => {}
            }
        }
    }

    /// An image as its description and a short fingerprint of its picture, so that a changed
    /// picture shows as a change even where its description is the same. The description is
    /// on the drawing the picture reference `blip` is in, or, drawn the older way, on the
    /// image data itself.
    fn image(&mut self, blip: &Element) -> Option<String> {
        let described = self
            .described
            .take()
            .or_else(|| blip.attr("title").map(str::to_string));
        let id = blip.attr("embed").or_else(|| blip.attr("id"))?.to_string();
        let key = (id.clone(), described.clone());
        if let Some(found) = self.images.get(&key) {
            return Some(found.clone());
        }
        let target = self.links.get(&id)?.clone();
        let name = target.rsplit('/').next().unwrap_or(&target).to_string();
        let print = if target.contains("://") {
            None
        } else {
            part_name("word", &target)
                .and_then(|p| optional_part(self.path, &p).ok().flatten())
                .map(|bytes| fingerprint(&bytes))
        };
        let label = described.filter(|d| !d.trim().is_empty());
        let shown = image_markdown(label.as_deref(), print.as_deref().or(Some(&name)));
        self.images.insert(key, shown.clone());
        Some(shown)
    }

    /// A table as a Markdown pipe table with a cell for every column of every row; see the
    /// module notes for how merged cells are written. Text in a cell is on one line.
    fn table(&mut self, tbl: &Element) -> String {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for tr in rows_of(tbl) {
            let mut row = Vec::new();
            let trpr = tr.child(W, "trPr");
            for _ in 0..trpr
                .and_then(|p| p.number("gridBefore"))
                .unwrap_or(0)
                .min(64)
            {
                row.push(String::new());
            }
            for tc in cells_of(tr) {
                let tcpr = tc.child(W, "tcPr");
                let span = tcpr
                    .and_then(|p| p.number("gridSpan"))
                    .unwrap_or(1)
                    .clamp(1, 64);
                let continues = |name: &str| {
                    tcpr.and_then(|p| p.child(W, name))
                        .is_some_and(|m| m.attr("val").is_none_or(|v| v == "continue"))
                };
                let text = if continues("vMerge") && !rows.is_empty() {
                    COVERED_ABOVE.to_string()
                } else if continues("hMerge") && !row.is_empty() {
                    COVERED_LEFT.to_string()
                } else {
                    escape_cell(&self.cell_text(tc))
                };
                row.push(text);
                for _ in 1..span {
                    row.push(COVERED_LEFT.to_string());
                }
            }
            for _ in 0..trpr
                .and_then(|p| p.number("gridAfter"))
                .unwrap_or(0)
                .min(64)
            {
                row.push(String::new());
            }
            rows.push(row);
        }
        pipe_table(&rows)
    }

    /// The text of a cell on one line: its paragraphs, list items and tables one after another.
    fn cell_text(&mut self, tc: &Element) -> String {
        let mut parts = Vec::new();
        // Every paragraph under the cell, those of tables and text boxes in it included; the
        // paragraph a text box is drawn in leaves the box's words to its own paragraphs.
        for e in tc.descendants() {
            if e.is(W, "p") {
                let mut pieces = Vec::new();
                let mut boxes = Vec::new();
                self.inline(e, Format::default(), &mut pieces, &mut boxes, 0);
                let text = markdown(&pieces, false);
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        parts.join(" ")
    }
}

/// The rows of a table, also those inside content controls.
fn rows_of(tbl: &Element) -> Vec<&Element> {
    let mut out = Vec::new();
    for e in tbl.elements() {
        if e.is(W, "tr") {
            out.push(e);
        } else if e.is(W, "sdt") || e.is(W, "customXml") {
            out.extend(e.descendants().into_iter().filter(|d| d.is(W, "tr")));
        }
    }
    out
}

/// The cells of a row, also those inside content controls.
fn cells_of(tr: &Element) -> Vec<&Element> {
    let mut out = Vec::new();
    for e in tr.elements() {
        if e.is(W, "tc") {
            out.push(e);
        } else if e.is(W, "sdt") || e.is(W, "customXml") {
            for d in e.elements() {
                if d.is(W, "sdtContent") {
                    out.extend(d.elements().filter(|c| c.is(W, "tc")));
                } else if d.is(W, "tc") {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// The first 8 hexadecimal digits of the SHA-256 of `bytes`.
pub(super) fn fingerprint(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// How an image is shown: slanted, with its description and what tells its picture apart,
/// such as its fingerprint. A Word document's image gives its file name only when its picture
/// cannot be read, since Word renames the pictures of a document whenever it saves it.
fn image_markdown(description: Option<&str>, print: Option<&str>) -> String {
    let mut text = String::from("Image");
    if let Some(d) = description {
        text.push_str(": ");
        text.push_str(&d.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    if let Some(print) = print {
        text.push_str(" (");
        text.push_str(print);
        text.push(')');
    }
    format!("*{}*", escape(&text))
}

/// Text with the characters Markdown would read as formatting escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if escapes(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The Markdown of a paragraph's pieces: neighbouring text in the same format joined, so that
/// its marks are written once, with the spaces at its ends outside them. Line breaks are
/// written as Markdown line breaks when `breaks` is set, otherwise as spaces.
fn markdown(pieces: &[Piece], breaks: bool) -> String {
    let mut out = String::new();
    let mut run: Option<(Format, String)> = None;
    let flush = |out: &mut String, run: &mut Option<(Format, String)>| {
        if let Some((f, text)) = run.take() {
            out.push_str(&formatted(f, &text));
        }
    };
    for piece in pieces {
        match piece {
            Piece::Text(f, t) => match &mut run {
                Some((rf, text)) if rf == f => text.push_str(t),
                _ => {
                    flush(&mut out, &mut run);
                    run = Some((*f, t.clone()));
                }
            },
            Piece::Markdown(m) => {
                flush(&mut out, &mut run);
                out.push_str(m);
            }
            Piece::Break => {
                flush(&mut out, &mut run);
                if breaks {
                    let end = out.trim_end_matches(' ').len();
                    out.truncate(end);
                    if !out.is_empty() && !out.ends_with("\\\n") {
                        out.push_str("\\\n");
                    }
                } else {
                    out.push(' ');
                }
            }
        }
    }
    flush(&mut out, &mut run);
    let mut text = out
        .split('\n')
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    while text.ends_with("\\") || text.ends_with('\n') {
        text.pop();
    }
    text
}

/// `text` in format `f`, with its marks inside the spaces at its ends.
fn formatted(f: Format, text: &str) -> String {
    let core = text.trim();
    if core.is_empty() {
        return text.to_string();
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    let mut marks = String::new();
    if f.strike {
        marks.push_str("~~");
    }
    if f.bold {
        marks.push_str("**");
    }
    if f.italic {
        marks.push('*');
    }
    let closing: String = marks.chars().rev().collect();
    format!("{lead}{marks}{}{closing}{trail}", escape(core))
}

/// Rows of Markdown cells as a pipe table, every row as wide as the widest, the first row the
/// header.
fn pipe_table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let mut s = String::from("|");
        for c in cells {
            s.push(' ');
            s.push_str(c);
            s.push_str(" |");
        }
        s
    };
    let mut out = Vec::with_capacity(rows.len() + 1);
    for (i, r) in rows.iter().enumerate() {
        out.push(line(
            &mut (0..cols).map(|k| r.get(k).map_or("", String::as_str)),
        ));
        if i == 0 {
            out.push(line(&mut (0..cols).map(|_| "---")));
        }
    }
    out.join("\n")
}

// ---------- workbooks ----------

const DRAWING: &str = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

/// The most rows and columns of a sheet shown as a table; a sheet larger than this is cut off.
const MAX_SHEET_ROWS: usize = 100_000;
const MAX_SHEET_COLUMNS: usize = 1_024;

/// The blocks of the workbook at `path`: for each sheet a heading, its cells as a table, with
/// merged cells written as markers and numbers shown in their number formats, and the pictures
/// placed on it, each with the cell it is placed at.
pub fn xlsx_blocks(path: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<Block>> {
    let workbook = parse(&part(path, "xl/workbook.xml")?, cancel)?;
    let date1904 = workbook
        .child(SHEET_NS, "workbookPr")
        .and_then(|p| p.attr("date1904"))
        .is_some_and(|v| v == "1" || v == "true");
    let targets: HashMap<String, String> =
        relationships(&optional_part(path, "xl/_rels/workbook.xml.rels")?.unwrap_or_default())
            .into_iter()
            .collect();
    let shared = match optional_part(path, "xl/sharedStrings.xml")? {
        Some(xml) => shared_strings(&xml)?,
        None => Vec::new(),
    };
    let formats = match optional_part(path, "xl/styles.xml")? {
        Some(xml) => NumberFormats::read(&parse(&xml, cancel)?),
        None => NumberFormats::default(),
    };
    let book = Book {
        path,
        shared,
        formats,
        date1904,
    };
    let mut blocks = Vec::new();
    let mut size = 0;
    let sheets = workbook
        .child(SHEET_NS, "sheets")
        .map(|s| s.elements().filter(|e| e.is(SHEET_NS, "sheet")).collect())
        .unwrap_or_else(Vec::new);
    if sheets.is_empty() {
        bail!("the workbook lists no sheets");
    }
    for sheet in sheets {
        check(cancel)?;
        let name = sheet.attr("name").unwrap_or("");
        let Some(target) = sheet
            .attr("id")
            .and_then(|id| targets.get(id))
            .and_then(|t| part_name("xl", t))
        else {
            continue;
        };
        let Some(xml) = optional_part(path, &target)? else {
            continue;
        };
        let heading = crate::i18n::trf("label.sheet", &[("name", name.to_string())]);
        let mut add = |text: String| {
            size += text.len();
            blocks.push(Block { depth: 0, text });
        };
        add(format!("## {}", escape(heading.trim())));
        let root = parse(&xml, cancel)?;
        let table = book.table(&root);
        if !table.is_empty() {
            add(table);
        }
        for picture in book.pictures(&root, &target) {
            add(picture);
        }
        if size > MAX_OUTPUT {
            break;
        }
    }
    Ok(blocks)
}

/// What the cells of every sheet of a workbook are read with.
struct Book<'a> {
    path: &'a Path,
    shared: Vec<String>,
    formats: NumberFormats,
    date1904: bool,
}

/// A cell reference such as `B12` as a row and column, counting from zero.
fn cell_at(reference: &str) -> Option<(usize, usize)> {
    let col = column_of(reference)?;
    let digits = reference.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let row: usize = digits.trim_start_matches('$').parse().ok()?;
    Some((row.checked_sub(1)?, col))
}

/// A cell's reference, such as `B12`, from its row and column counting from zero.
fn cell_name(row: usize, col: usize) -> String {
    let mut letters = String::new();
    let mut n = col + 1;
    while n > 0 {
        let r = (n - 1) % 26;
        letters.insert(0, (b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    format!("{letters}{}", row + 1)
}

impl Book<'_> {
    /// A sheet's cells as a pipe table: every row from the first to the last that holds a value
    /// or a merged cell, and every column likewise.
    fn table(&self, root: &Element) -> String {
        let mut cells: Vec<(usize, usize, String)> = Vec::new();
        if let Some(data) = root.child(SHEET_NS, "sheetData") {
            let mut next_row = 0usize;
            for row in data.elements().filter(|e| e.is(SHEET_NS, "row")) {
                let r = row
                    .attr("r")
                    .and_then(|r| r.trim().parse::<usize>().ok())
                    .and_then(|r| r.checked_sub(1))
                    .unwrap_or(next_row);
                next_row = r + 1;
                if r >= MAX_SHEET_ROWS {
                    break;
                }
                let mut next_col = 0usize;
                for c in row.elements().filter(|e| e.is(SHEET_NS, "c")) {
                    let k = c.attr("r").and_then(cell_at).map_or(next_col, |(_, k)| k);
                    next_col = k + 1;
                    if k >= MAX_SHEET_COLUMNS {
                        continue;
                    }
                    let text = self.value(c);
                    if !text.is_empty() {
                        cells.push((r, k, text));
                    }
                }
            }
        }
        let merges: Vec<((usize, usize), (usize, usize))> = root
            .child(SHEET_NS, "mergeCells")
            .map(|m| {
                m.elements()
                    .filter(|e| e.is(SHEET_NS, "mergeCell"))
                    .filter_map(|e| {
                        let (a, b) = e.attr("ref")?.split_once(':')?;
                        let (a, b) = (cell_at(a)?, cell_at(b)?);
                        let top = (a.0.min(b.0), a.1.min(b.1));
                        let end = (
                            a.0.max(b.0).min(MAX_SHEET_ROWS - 1),
                            a.1.max(b.1).min(MAX_SHEET_COLUMNS - 1),
                        );
                        (top.0 <= end.0 && top.1 <= end.1).then_some((top, end))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows = cells
            .iter()
            .map(|c| c.0)
            .chain(merges.iter().map(|m| m.1 .0))
            .max();
        let cols = cells
            .iter()
            .map(|c| c.1)
            .chain(merges.iter().map(|m| m.1 .1))
            .max();
        let (Some(rows), Some(cols)) = (rows, cols) else {
            return String::new();
        };
        let mut grid = vec![vec![String::new(); cols + 1]; rows + 1];
        for (r, k, text) in cells {
            grid[r][k] = escape_cell(&escape(&text).replace('\n', " "));
        }
        for ((r0, c0), (r1, c1)) in merges {
            for (r, row) in grid.iter_mut().enumerate().take(r1 + 1).skip(r0) {
                for (k, cell) in row.iter_mut().enumerate().take(c1 + 1).skip(c0) {
                    if (r, k) == (r0, c0) {
                        continue;
                    }
                    *cell = if k == c0 { COVERED_ABOVE } else { COVERED_LEFT }.to_string();
                }
            }
        }
        pipe_table(&collapse_blank_rows(grid))
    }

    /// What a cell shows: its value, a shared or inline string, a truth value, an error, or a
    /// number in the cell's number format. A formula shows the result it was saved with.
    fn value(&self, c: &Element) -> String {
        let v = c.child(SHEET_NS, "v").map(own_text).unwrap_or_default();
        match c.attr("t").unwrap_or("n") {
            "s" => v
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|i| self.shared.get(i).cloned())
                .unwrap_or_default(),
            "inlineStr" => c
                .child(SHEET_NS, "is")
                .map(|is| {
                    is.descendants()
                        .into_iter()
                        .filter(|e| e.is(SHEET_NS, "t"))
                        .filter(|t| !is_phonetic(is, t))
                        .map(own_text)
                        .collect()
                })
                .unwrap_or_default(),
            "b" => if v.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
            "n" => {
                let style = c.attr("s").and_then(|s| s.parse().ok()).unwrap_or(0);
                self.formats.show(&v, style, self.date1904)
            }
            _ => v,
        }
    }

    /// The pictures placed on the sheet `root` of the part `target`, each as its description,
    /// the cell its top left corner is in and a fingerprint of the picture.
    fn pictures(&self, root: &Element, target: &str) -> Vec<String> {
        let Some(id) = root.child(SHEET_NS, "drawing").and_then(|d| d.attr("id")) else {
            return Vec::new();
        };
        let (folder, file) = target.rsplit_once('/').unwrap_or(("", target));
        let rels = |folder: &str, file: &str| -> HashMap<String, String> {
            optional_part(self.path, &format!("{folder}/_rels/{file}.rels"))
                .ok()
                .flatten()
                .map(|xml| relationships(&xml).into_iter().collect())
                .unwrap_or_default()
        };
        let Some(drawing) = rels(folder, file)
            .get(id)
            .and_then(|t| part_name(folder, t))
        else {
            return Vec::new();
        };
        let Some(xml) = optional_part(self.path, &drawing).ok().flatten() else {
            return Vec::new();
        };
        let Ok(tree) = parse(&xml, None) else {
            return Vec::new();
        };
        let (dfolder, dfile) = drawing.rsplit_once('/').unwrap_or(("", &drawing));
        let media = rels(dfolder, dfile);
        let mut out = Vec::new();
        for anchor in tree.elements() {
            if !matches!(
                anchor.name.as_str(),
                "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor"
            ) || anchor.ns != DRAWING
            {
                continue;
            }
            let at = anchor.child(DRAWING, "from").and_then(|f| {
                let n = |name: &str| {
                    f.child(DRAWING, name)
                        .and_then(|e| own_text(e).trim().parse::<usize>().ok())
                };
                Some(cell_name(n("row")?, n("col")?))
            });
            for pic in anchor
                .descendants()
                .into_iter()
                .filter(|e| e.is(DRAWING, "pic"))
            {
                let described = pic
                    .descendants()
                    .into_iter()
                    .find(|e| e.is(DRAWING, "cNvPr"))
                    .and_then(|p| {
                        p.attr("descr")
                            .filter(|d| !d.trim().is_empty())
                            .or_else(|| p.attr("name"))
                    })
                    .map(str::to_string);
                let print = pic
                    .descendants()
                    .into_iter()
                    .find(|e| e.is(DRAWINGML, "blip"))
                    .and_then(|b| b.attr("embed"))
                    .and_then(|id| media.get(id))
                    .and_then(|t| part_name(dfolder, t))
                    .and_then(|p| optional_part(self.path, &p).ok().flatten())
                    .map(|bytes| fingerprint(&bytes));
                let mut text = String::from("Image");
                if let Some(d) = described {
                    text.push_str(": ");
                    text.push_str(&d.split_whitespace().collect::<Vec<_>>().join(" "));
                }
                if let Some(at) = &at {
                    text.push_str(" at ");
                    text.push_str(at);
                }
                if let Some(p) = print {
                    text.push_str(&format!(" ({p})"));
                }
                out.push(format!("*{}*", escape(&text)));
            }
        }
        out
    }
}

/// The fewest blank rows in a row that are shown as one row saying how many there are.
const BLANK_RUN: usize = 3;

/// `grid` with each run of [`BLANK_RUN`] or more rows without a value or a merged cell shown as
/// one row saying how many rows it stands for, so that a sheet with values far apart is not
/// shown as a long empty table, while a value that moved a row still shows as a change.
fn collapse_blank_rows(grid: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::with_capacity(grid.len());
    let mut blank: Vec<Vec<String>> = Vec::new();
    let flush = |out: &mut Vec<Vec<String>>, blank: &mut Vec<Vec<String>>| {
        if blank.len() >= BLANK_RUN {
            let mut row = vec![String::new(); blank[0].len()];
            let count = crate::i18n::trf("label.empty_rows", &[("count", blank.len().to_string())]);
            row[0] = format!("*{}*", escape(&format!("\u{22ef} {count}")));
            out.push(row);
            blank.clear();
        } else {
            out.append(blank);
        }
    };
    for row in grid {
        // The first row stays, since it is the table's header.
        if !out.is_empty() && row.iter().all(String::is_empty) {
            blank.push(row);
            continue;
        }
        flush(&mut out, &mut blank);
        out.push(row);
    }
    flush(&mut out, &mut blank);
    out
}

/// Whether the text `t` of an inline string `is` is the reading of its words rather than words.
fn is_phonetic(is: &Element, t: &Element) -> bool {
    is.elements()
        .filter(|e| e.is(SHEET_NS, "rPh"))
        .any(|r| r.descendants().into_iter().any(|d| std::ptr::eq(d, t)))
}

/// The number formats of `xl/styles.xml`, by cell style.
#[derive(Default)]
struct NumberFormats {
    /// The number format of each cell style.
    styles: Vec<u32>,
    /// The formats the workbook defines itself, by number.
    codes: HashMap<u32, String>,
}

impl NumberFormats {
    fn read(root: &Element) -> NumberFormats {
        let codes = root
            .child(SHEET_NS, "numFmts")
            .map(|n| {
                n.elements()
                    .filter(|e| e.is(SHEET_NS, "numFmt"))
                    .filter_map(|e| {
                        Some((
                            e.attr("numFmtId")?.parse().ok()?,
                            e.attr("formatCode")?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let styles = root
            .child(SHEET_NS, "cellXfs")
            .map(|x| {
                x.elements()
                    .filter(|e| e.is(SHEET_NS, "xf"))
                    .map(|e| e.attr("numFmtId").and_then(|n| n.parse().ok()).unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default();
        NumberFormats { styles, codes }
    }

    /// The number `value` as a cell of style `style` shows it.
    fn show(&self, value: &str, style: usize, date1904: bool) -> String {
        let id = self.styles.get(style).copied().unwrap_or(0);
        let code = self
            .codes
            .get(&id)
            .map(String::as_str)
            .or_else(|| builtin_format(id));
        match (code, value.trim().parse::<f64>()) {
            (Some(code), Ok(n)) if n.is_finite() => {
                number_format(n, code, date1904).unwrap_or_else(|| value.trim().to_string())
            }
            _ => value.trim().to_string(),
        }
    }
}

/// The formats a workbook can use without defining them.
fn builtin_format(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 | 48 => "0.00E+00",
        14 => "yyyy-mm-dd",
        15..=17 => "d-mmm-yy",
        18 | 20 => "h:mm",
        19 | 21 | 45..=47 => "h:mm:ss",
        22 => "yyyy-mm-dd h:mm",
        37 | 38 => "#,##0;(#,##0)",
        39 | 40 => "#,##0.00;(#,##0.00)",
        49 => "@",
        _ => return None,
    })
}

/// `n` shown in the number format `code`: dates and times in ISO form whatever order the code
/// puts their parts in, so that they read the same in every locale, and numbers with their
/// decimals, thousands separators, percent sign and the text around them. `None` for a format
/// that shows the number as it is.
fn number_format(n: f64, code: &str, date1904: bool) -> Option<String> {
    let sections: Vec<&str> = split_sections(code);
    let (section, n) = match sections.get(1) {
        Some(negative) if n < 0.0 => (*negative, -n),
        _ => (sections.first().copied().unwrap_or(""), n),
    };
    // What the code writes as it is, and the code without it.
    let mut literal_before = String::new();
    let mut literal_after = String::new();
    let mut pattern = String::new();
    let mut chars = section.chars().peekable();
    while let Some(c) = chars.next() {
        let literal = match c {
            '"' => {
                let mut s = String::new();
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                    s.push(q);
                }
                Some(s)
            }
            '\\' => chars.next().map(String::from),
            // Room as wide as the next character, and filling with it.
            '_' | '*' => {
                chars.next();
                None
            }
            '[' => {
                let mut s = String::new();
                for q in chars.by_ref() {
                    if q == ']' {
                        break;
                    }
                    s.push(q);
                }
                // An elapsed time, unlike a colour or locale, is part of the pattern.
                if matches!(
                    s.to_ascii_lowercase().as_str(),
                    "h" | "hh" | "m" | "mm" | "s" | "ss"
                ) {
                    pattern.push_str(&s);
                }
                None
            }
            // The sign of an exponent is part of the pattern.
            '-' | '+' if pattern.ends_with(['E', 'e']) => {
                pattern.push(c);
                None
            }
            '$' | '-' | '+' | '(' | ')' | ' ' | ':' | '/' => Some(c.to_string()),
            _ => {
                pattern.push(c);
                None
            }
        };
        if let Some(l) = literal {
            if pattern
                .chars()
                .any(|p| p.is_ascii_alphanumeric() || p == '#')
            {
                literal_after.push_str(&l);
            } else {
                literal_before.push_str(&l);
            }
        }
    }
    let lower = pattern.to_ascii_lowercase();
    if lower.trim().is_empty() || lower.contains("general") || lower.contains('@') {
        return None;
    }
    if lower.contains(['y', 'd', 'h', 's']) || (lower.contains('m') && !lower.contains(['0', '#']))
    {
        return date_time(n, &lower, date1904);
    }
    let (body, percent) = match pattern.find('%') {
        Some(_) => (pattern.replace('%', ""), true),
        None => (pattern.clone(), false),
    };
    let value = if percent { n * 100.0 } else { n };
    let shown = if let Some(e) = body.to_ascii_uppercase().find('E') {
        let decimals = body[..e].split_once('.').map_or(0, |(_, d)| {
            d.chars().filter(|c| matches!(c, '0' | '#')).count()
        });
        let s = format!("{value:.decimals$E}");
        let (mantissa, exponent) = s.split_once('E').unwrap_or((&s, "0"));
        let exponent: i32 = exponent.parse().unwrap_or(0);
        format!(
            "{mantissa}E{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        )
    } else {
        let decimals = body.split_once('.').map_or(0, |(_, d)| {
            d.chars().filter(|c| matches!(c, '0' | '#' | '?')).count()
        });
        let grouped = body.split('.').next().unwrap_or("").contains(',');
        // Halves are rounded away from zero, as a spreadsheet rounds them.
        let scale = 10f64.powi(decimals.min(15) as i32);
        let s = format!("{:.decimals$}", (value * scale).round() / scale);
        if grouped {
            group_thousands(&s)
        } else {
            s
        }
    };
    let mut out = literal_before;
    out.push_str(&shown);
    if percent {
        out.push('%');
    }
    out.push_str(&literal_after);
    Some(out.trim().to_string())
}

/// The sections of a number format, split at the `;` outside quotes.
fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in code.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&code[start..]);
    out
}

/// `12345.6` as `12,345.6`.
fn group_thousands(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map_or(("", s), |r| ("-", r));
    let (int, frac) = rest
        .split_once('.')
        .map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// The date and time serial number `n` stands for, in ISO form: the date when the format shows
/// one, the time when it shows one.
fn date_time(n: f64, pattern: &str, date1904: bool) -> Option<String> {
    if !(0.0..2_958_466.0).contains(&n) {
        return None;
    }
    let days = n.floor() as i64;
    let seconds = ((n - n.floor()) * 86_400.0).round() as i64;
    let (days, seconds) = if seconds >= 86_400 {
        (days + 1, seconds - 86_400)
    } else {
        (days, seconds)
    };
    // Serial 1 is 1900-01-01, and 60 the 29 February 1900 that never was.
    let base = if date1904 {
        chrono::NaiveDate::from_ymd_opt(1904, 1, 1)?
    } else if days < 60 {
        chrono::NaiveDate::from_ymd_opt(1899, 12, 31)?
    } else {
        chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?
    };
    let date = base.checked_add_signed(chrono::Duration::days(days))?;
    let has_date =
        pattern.contains(['y', 'd']) || (pattern.contains('m') && !pattern.contains(['h', 's']));
    let has_time = pattern.contains(['h', 's']);
    let mut out = String::new();
    if has_date {
        out.push_str(&date.format("%Y-%m-%d").to_string());
    }
    if has_time {
        if !out.is_empty() {
            out.push(' ');
        }
        let (h, m, s) = (
            seconds / 3600 + if has_date { 0 } else { days * 24 },
            seconds / 60 % 60,
            seconds % 60,
        );
        out.push_str(&format!("{h:02}:{m:02}"));
        if pattern.contains('s') {
            out.push_str(&format!(":{s:02}"));
        }
    }
    Some(out)
}

// ---------- tagged PDF files ----------

/// The blocks of the tagged PDF file at `path`, read from the structure it is tagged with:
/// its headings, paragraphs, list items, tables with their merged cells, and figures with
/// their descriptions. Text drawn in a bold or slanted font keeps that format. A file that is
/// not tagged is refused.
pub fn pdf_blocks(path: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<Block>> {
    let bytes = read_whole(path)?;
    let tree = pdf::structure(&bytes, cancel)?;
    let mut writer = Tagged {
        cancel,
        blocks: Vec::new(),
        size: 0,
        sections: 0,
    };
    writer.block(&tree, 0, 0)?;
    if writer.blocks.is_empty() {
        bail!("the PDF file is tagged, but its structure holds no text");
    }
    Ok(writer.blocks)
}

/// What an element of a tagged file is read as.
#[derive(PartialEq, Eq)]
enum Role {
    /// Holds other blocks, such as the document or a section.
    Group,
    Heading(usize),
    List,
    Table,
    Figure,
    /// Text inside a block, such as a span, a link or a word in bold.
    Inline,
    /// A block of text.
    Paragraph,
    /// A type this does not know, which the file's role map does not say the standard type
    /// of: a group when it holds blocks, otherwise a paragraph.
    Unknown,
    /// Left out, as an artifact is.
    Hidden,
}

fn role_of(role: &str) -> Role {
    match role {
        "Document" | "DocumentFragment" | "Part" | "Art" | "Sect" | "Div" | "BlockQuote"
        | "NonStruct" | "Private" | "TOC" | "Index" | "Aside" | "Form" | "THead" | "TBody"
        | "TFoot" => Role::Group,
        "Title" => Role::Heading(1),
        // A heading whose level is how many sections it is in.
        "H" => Role::Heading(0),
        "H1" | "H2" | "H3" | "H4" | "H5" | "H6" => {
            Role::Heading((role.as_bytes()[1] - b'0') as usize)
        }
        "L" => Role::List,
        "Table" => Role::Table,
        "Figure" => Role::Figure,
        "Span" | "Link" | "Quote" | "Reference" | "Em" | "Strong" | "Lbl" | "Annot" | "Ruby"
        | "RB" | "RT" | "RP" | "Warichu" | "WT" | "WP" | "Sub" => Role::Inline,
        "Artifact" => Role::Hidden,
        "P" | "Caption" | "Note" | "Code" | "Formula" | "TOCI" | "BibEntry" | "FENote" | "LI"
        | "LBody" | "TR" | "TH" | "TD" => Role::Paragraph,
        _ => Role::Unknown,
    }
}

/// Whether `e` holds blocks, such as paragraphs, headings or tables, rather than only text.
fn holds_blocks(e: &pdf::Element) -> bool {
    e.kids.iter().any(|k| match k {
        pdf::Kid::Element(k) => !matches!(role_of(&k.role), Role::Inline | Role::Hidden),
        pdf::Kid::Text(_) => false,
    })
}

/// What an element is read as where blocks are written: a type this does not know is a group
/// when it holds blocks.
fn block_role_of(e: &pdf::Element) -> Role {
    match role_of(&e.role) {
        Role::Unknown if holds_blocks(e) => Role::Group,
        Role::Unknown => Role::Paragraph,
        role => role,
    }
}

/// Writing a tagged file's structure as blocks.
struct Tagged<'a> {
    cancel: Option<&'a AtomicBool>,
    blocks: Vec<Block>,
    size: usize,
    /// How many sections the element being written is in.
    sections: usize,
}

/// The pieces of text of a block being read, with the lists found in it.
#[derive(Default)]
struct Gathered<'e> {
    pieces: Vec<Piece>,
    lists: Vec<&'e pdf::Element>,
    /// Where the last text was drawn, so that a run on another line, or far along the same
    /// one, is written after a space.
    last: Option<Place>,
}

/// Where a run of text was drawn: its page, line, where it ends and how large it is.
#[derive(Clone, Copy)]
struct Place {
    page: usize,
    y: f64,
    end: f64,
    size: f64,
}

impl Place {
    fn of(run: &pdf::Run) -> Place {
        Place {
            page: run.page,
            y: run.y,
            end: run.end,
            size: run.size,
        }
    }

    /// Whether `next`, drawn after this, is set apart from it by a space.
    fn apart_from(&self, next: &pdf::Run) -> bool {
        self.page != next.page
            || (self.y - next.y).abs() >= 1.0
            || pdf::gap_is_space(next.x - self.end, self.size.max(next.size))
    }
}

impl<'e> Gathered<'e> {
    fn text(&mut self, format: Format, runs: &[pdf::Run]) {
        for run in runs {
            if self.last.is_some_and(|last| last.apart_from(run)) {
                self.pieces.push(Piece::Text(format, " ".to_string()));
            }
            let f = Format {
                bold: format.bold || run.bold,
                italic: format.italic || run.italic,
                strike: false,
            };
            self.pieces.push(Piece::Text(f, run.text.clone()));
            self.last = Some(Place::of(run));
        }
    }

    /// Adds the text of `kids`, in `format`.
    fn kids(&mut self, kids: &'e [pdf::Kid], format: Format, depth: usize) {
        for kid in kids {
            match kid {
                pdf::Kid::Text(runs) => self.text(format, runs),
                pdf::Kid::Element(e) => self.element(e, format, depth + 1),
            }
        }
    }

    /// Adds the text of `e` and what is inside it.
    fn element(&mut self, e: &'e pdf::Element, format: Format, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let role = role_of(&e.role);
        if role == Role::Hidden {
            return;
        }
        if role == Role::List {
            self.lists.push(e);
            return;
        }
        if role == Role::Figure {
            self.pieces.push(Piece::Text(format, " ".to_string()));
            self.pieces.push(Piece::Markdown(figure(e)));
            self.pieces.push(Piece::Text(format, " ".to_string()));
            return;
        }
        // Blocks inside the text, such as the paragraphs of a cell, are set apart by spaces.
        let apart = role != Role::Inline;
        if apart {
            self.pieces.push(Piece::Text(format, " ".to_string()));
        }
        let format = Format {
            bold: format.bold || e.role == "Strong",
            italic: format.italic || e.role == "Em",
            strike: false,
        };
        if let Some(actual) = &e.actual {
            self.pieces.push(Piece::Text(format, actual.clone()));
        } else if let Some(url) = e.link.as_deref().filter(|u| u.contains(':')) {
            let mut inner = Gathered {
                last: self.last,
                ..Default::default()
            };
            inner.kids(&e.kids, Format::default(), depth);
            self.lists.extend(inner.lists);
            self.last = inner.last;
            let text = markdown(&inner.pieces, false);
            if text.is_empty() {
                self.pieces.push(Piece::Text(format, url.to_string()));
            } else {
                self.pieces
                    .push(Piece::Markdown(format!("[{text}]({})", destination(url))));
            }
        } else {
            self.kids(&e.kids, format, depth);
        }
        if apart {
            self.pieces.push(Piece::Text(format, " ".to_string()));
        }
    }
}

/// A figure: slanted, with its description and the fingerprint of the picture drawn for it.
fn figure(e: &pdf::Element) -> String {
    let mut prints = Vec::new();
    images_of(e, &mut prints, 0);
    let prints = Some(prints.join(", ")).filter(|p| !p.is_empty());
    image_markdown(e.alt.as_deref().or(e.actual.as_deref()), prints.as_deref())
}

fn images_of(e: &pdf::Element, out: &mut Vec<String>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for print in &e.images {
        if !out.contains(print) {
            out.push(print.clone());
        }
    }
    for kid in &e.kids {
        if let pdf::Kid::Element(k) = kid {
            images_of(k, out, depth + 1);
        }
    }
}

impl Tagged<'_> {
    fn emit(&mut self, depth: usize, text: String) {
        if text.trim().is_empty() || self.size > MAX_OUTPUT {
            return;
        }
        self.size += text.len();
        self.blocks.push(Block { depth, text });
    }

    /// Writes `e` as blocks; `list` is how many list items it is in.
    fn block(&mut self, e: &pdf::Element, list: usize, depth: usize) -> Result<()> {
        check(self.cancel)?;
        if depth > MAX_DEPTH {
            return Ok(());
        }
        match block_role_of(e) {
            Role::Hidden => {}
            Role::Group => {
                let section = e.role == "Sect";
                self.sections += usize::from(section);
                // Text directly in a group, and its inline elements, are read as paragraphs,
                // as many neighbours as there are together.
                let mut start = 0;
                for (i, kid) in e.kids.iter().enumerate() {
                    let block = match kid {
                        pdf::Kid::Element(k) => role_of(&k.role) != Role::Inline,
                        pdf::Kid::Text(_) => false,
                    };
                    if block {
                        self.paragraph(&e.kids[start..i], list, depth)?;
                        if let pdf::Kid::Element(k) = kid {
                            self.block(k, list, depth + 1)?;
                        }
                        start = i + 1;
                    }
                }
                self.paragraph(&e.kids[start..], list, depth)?;
                self.sections -= usize::from(section);
            }
            Role::Heading(level) => {
                let level = if level == 0 {
                    self.sections.clamp(1, 6)
                } else {
                    level
                };
                let mut gathered = Gathered::default();
                gathered.element(e, Format::default(), depth);
                let text = markdown(&plain_if_whole(gathered.pieces), false);
                if !text.is_empty() {
                    self.emit(0, format!("{} {}", "#".repeat(level), text));
                }
            }
            Role::List => self.list(e, list, depth)?,
            Role::Table => self.table(e, depth),
            Role::Figure => self.emit(0, figure(e)),
            Role::Inline | Role::Paragraph | Role::Unknown => {
                let mut gathered = Gathered::default();
                gathered.element(e, Format::default(), depth);
                self.write(gathered, list, depth)?;
            }
        }
        Ok(())
    }

    /// Writes `kids` as a paragraph, then the lists inside them.
    fn paragraph(&mut self, kids: &[pdf::Kid], list: usize, depth: usize) -> Result<()> {
        let mut gathered = Gathered::default();
        gathered.kids(kids, Format::default(), depth);
        self.write(gathered, list, depth)
    }

    /// Writes what was gathered as a paragraph, then the lists found in it.
    fn write(&mut self, gathered: Gathered, list: usize, depth: usize) -> Result<()> {
        let text = markdown(&plain_if_whole(gathered.pieces), false);
        self.emit(0, escape_line_start(&text));
        for l in gathered.lists {
            self.list(l, list, depth + 1)?;
        }
        Ok(())
    }

    /// Writes the items of the list `l`, `depth` lists deep.
    fn list(&mut self, l: &pdf::Element, level: usize, depth: usize) -> Result<()> {
        check(self.cancel)?;
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let mut number = 0;
        for kid in &l.kids {
            let item = match kid {
                pdf::Kid::Element(e) if e.role == "L" => {
                    self.list(e, level + 1, depth + 1)?;
                    continue;
                }
                pdf::Kid::Element(e) => e,
                pdf::Kid::Text(runs) => {
                    let mut gathered = Gathered::default();
                    gathered.text(Format::default(), runs);
                    let text = markdown(&gathered.pieces, false);
                    self.emit(level, format!("- {text}"));
                    continue;
                }
            };
            number += 1;
            // The label, such as a bullet or a number, and the body, read apart.
            let mut label = Gathered::default();
            let mut body = Gathered::default();
            for k in &item.kids {
                match k {
                    pdf::Kid::Element(e) if e.role == "Lbl" => {
                        label.element(e, Format::default(), depth)
                    }
                    pdf::Kid::Element(e) => body.element(e, Format::default(), depth),
                    pdf::Kid::Text(runs) => body.text(Format::default(), runs),
                }
            }
            let label = markdown(&label.pieces, false);
            let text = markdown(&plain_if_whole(body.pieces), false);
            let marker = list_label(&label, number);
            if !text.is_empty() {
                self.emit(level, format!("{marker} {text}"));
            }
            for nested in body.lists {
                self.list(nested, level + 1, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Writes the table `t` as a pipe table, a cell for every column of every row, the cells
    /// a cell spans written as markers, then its caption.
    fn table(&mut self, t: &pdf::Element, depth: usize) {
        let mut rows = Vec::new();
        let mut captions = Vec::new();
        rows_and_captions(t, &mut rows, &mut captions, depth);
        let mut cells = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut line = Vec::with_capacity(row.len());
            for cell in row {
                let mut gathered = Gathered::default();
                gathered.kids(&cell.kids, Format::default(), depth);
                // A list in a cell is read as the words of its items.
                for _ in 0..MAX_DEPTH {
                    let Some(l) = gathered.lists.pop() else { break };
                    gathered.kids(&l.kids, Format::default(), depth + 1);
                }
                let text = escape_cell(&markdown(&gathered.pieces, false).replace("\\\n", " "));
                line.push(SpanCell {
                    text,
                    rows: cell.rows,
                    cols: cell.cols,
                });
            }
            cells.push(line);
        }
        let text = pipe_table(&lay_out(cells));
        self.emit(0, text);
        for caption in captions {
            let mut gathered = Gathered::default();
            gathered.element(caption, Format::default(), depth);
            let text = markdown(&plain_if_whole(gathered.pieces), false);
            self.emit(0, escape_line_start(&text));
        }
    }
}

/// The rows of a table, each its cells, and its captions; rows are found in the table's head,
/// body and foot as well.
fn rows_and_captions<'e>(
    t: &'e pdf::Element,
    rows: &mut Vec<Vec<&'e pdf::Element>>,
    captions: &mut Vec<&'e pdf::Element>,
    depth: usize,
) {
    if depth > MAX_DEPTH || rows.len() >= MAX_SHEET_ROWS {
        return;
    }
    for kid in &t.kids {
        let pdf::Kid::Element(e) = kid else { continue };
        match e.role.as_str() {
            "TR" => rows.push(
                e.kids
                    .iter()
                    .filter_map(|k| match k {
                        pdf::Kid::Element(c) if role_of(&c.role) != Role::Hidden => Some(c),
                        _ => None,
                    })
                    .collect(),
            ),
            "Caption" => captions.push(e),
            _ => rows_and_captions(e, rows, captions, depth + 1),
        }
    }
}

/// The pieces of a block with their format dropped when all of its words are in the same one:
/// a heading's bold, or a caption's slant, is how its style looks, as with a Word document's
/// styles, not words set apart from the rest.
fn plain_if_whole(mut pieces: Vec<Piece>) -> Vec<Piece> {
    let mut formats = pieces.iter().filter_map(|p| match p {
        Piece::Text(f, t) if !t.trim().is_empty() => Some(*f),
        _ => None,
    });
    let Some(first) = formats.next() else {
        return pieces;
    };
    if first != Format::default() && formats.all(|f| f == first) {
        for p in &mut pieces {
            if let Piece::Text(f, _) = p {
                *f = Format::default();
            }
        }
    }
    pieces
}

/// `- ` for a bulleted item, its number for a numbered one; a label that is not a number
/// counts as a bullet.
fn list_label(label: &str, number: usize) -> String {
    let label = label.trim().replace('\\', "");
    let digits = label.trim_end_matches(['.', ')']);
    if !digits.is_empty()
        && digits.len() < label.len()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{}.", digits.parse::<u64>().unwrap_or(number as u64));
    }
    "-".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

    fn docx(body: &str, parts: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(".docx").tempfile().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", options).unwrap();
        write!(
            zip,
            r#"<w:document {NS}><w:body>{body}</w:body></w:document>"#
        )
        .unwrap();
        for (name, bytes) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    fn blocks(body: &str, parts: &[(&str, &[u8])]) -> Vec<(usize, String)> {
        let file = docx(body, parts);
        docx_blocks(file.path(), None)
            .unwrap()
            .into_iter()
            .map(|b| (b.depth, b.text))
            .collect()
    }

    fn p(text: &str) -> String {
        format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
    }

    fn cell(props: &str, text: &str) -> String {
        format!("<w:tc><w:tcPr>{props}</w:tcPr>{}</w:tc>", p(text))
    }

    #[test]
    fn headings_formatting_and_breaks() {
        let styles =
            br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:style w:type="paragraph" w:styleId="H"><w:name w:val="heading 2"/></w:style>
            <w:style w:type="paragraph" w:styleId="Mine"><w:basedOn w:val="H"/></w:style>
            </w:styles>"#;
        let body = concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Mine"/></w:pPr><w:r><w:t>Plan</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">Some </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">bold </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>words</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i w:val="0"/></w:rPr><w:t xml:space="preserve">, a * star</w:t></w:r>"#,
            r#"<w:del><w:r><w:delText>gone</w:delText></w:r></w:del>"#,
            r#"<w:r><w:br/><w:t>second line</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>1. Not a list</w:t></w:r></w:p>"#,
            r#"<w:p/>"#,
        );
        assert_eq!(
            blocks(body, &[("word/styles.xml", styles)]),
            [
                (0, "## Plan".to_string()),
                (
                    0,
                    "Some **bold words**, a \\* star\\\nsecond line".to_string()
                ),
                (0, "1\\. Not a list".to_string()),
            ]
        );
    }

    #[test]
    fn list_items_are_numbered_and_nested() {
        let numbering = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="decimal"/></w:lvl>
            <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
            <w:num w:numId="7"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
        let item = |level: usize, text: &str| {
            format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="7"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
            )
        };
        let body = [item(0, "One"), item(1, "Sub"), item(0, "Two")].concat();
        assert_eq!(
            blocks(&body, &[("word/numbering.xml", numbering)]),
            [
                (0, "3. One".to_string()),
                (1, "- Sub".to_string()),
                (0, "4. Two".to_string()),
            ]
        );
    }

    #[test]
    fn merged_cells_are_written_as_markers() {
        let rows = [
            [
                cell(r#"<w:vMerge w:val="restart"/>"#, "Area"),
                cell(r#"<w:gridSpan w:val="2"/>"#, "Revenue"),
            ]
            .concat(),
            [cell("<w:vMerge/>", ""), cell("", "Q2"), cell("", "^")].concat(),
            [
                r#"<w:trPr><w:gridBefore w:val="1"/></w:trPr>"#.to_string(),
                cell(r#"<w:gridSpan w:val="2"/>"#, "a | b"),
            ]
            .concat(),
        ];
        let body = format!(
            "<w:tbl>{}</w:tbl>",
            rows.map(|r| format!("<w:tr>{r}</w:tr>")).concat()
        );
        let got = blocks(&body, &[]);
        assert_eq!(
            got,
            [(
                0,
                "| Area | Revenue | < |\n| --- | --- | --- |\n| ^ | Q2 | \\^ |\n|  | a \\| b | < |"
                    .to_string()
            )]
        );
        let cells = crate::core::web_markdown::table_cells(&got[0].1).unwrap();
        use crate::core::web_markdown::{spans, Slot};
        let slots = spans(&cells);
        assert_eq!(slots[0][0], Slot::Anchor { cols: 1, rows: 2 });
        assert_eq!(slots[0][1], Slot::Anchor { cols: 2, rows: 1 });
        assert_eq!(slots[1][0], Slot::Covered { row: 0, col: 0 });
        assert_eq!(slots[1][2], Slot::Anchor { cols: 1, rows: 1 });
        assert_eq!(slots[2][2], Slot::Covered { row: 2, col: 1 });
    }

    #[test]
    fn images_links_and_text_boxes() {
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rId1" Type="image" Target="media/chart.png"/>
            <Relationship Id="rId2" Type="hyperlink" Target="https://example.com/a b" TargetMode="External"/>
            </Relationships>"#;
        let body = concat!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Chart" descr="Sales chart"/>"#,
            r#"<a:graphic><a:graphicData><a:blip r:embed="rId1"/></a:graphicData></a:graphic>"#,
            r#"</wp:inline></w:drawing></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="rId2"><w:r><w:t>the site</w:t></w:r></w:hyperlink></w:p>"#,
            r#"<w:p><w:r><w:t>Boxed:</w:t></w:r><w:r><w:drawing><w:txbxContent><w:p><w:r><w:t>inside</w:t></w:r></w:p></w:txbxContent></w:drawing></w:r></w:p>"#,
        );
        let got = blocks(
            body,
            &[
                ("word/_rels/document.xml.rels", rels),
                ("word/media/chart.png", b"picture"),
            ],
        );
        let print = fingerprint(b"picture");
        assert_eq!(
            got,
            [
                (0, format!("*Image: Sales chart ({print})*")),
                (0, "See [the site](<https://example.com/a b>)".to_string()),
                (0, "Boxed:".to_string()),
                (0, "inside".to_string()),
            ]
        );
    }

    #[test]
    fn number_formats_show_dates_money_and_percentages() {
        let f = |n: f64, code: &str| number_format(n, code, false);
        assert_eq!(f(46295.0, "yyyy-mm-dd").as_deref(), Some("2026-09-30"));
        assert_eq!(f(46295.0, "d-mmm-yy").as_deref(), Some("2026-09-30"));
        assert_eq!(
            f(46295.75, "m/d/yyyy h:mm").as_deref(),
            Some("2026-09-30 18:00")
        );
        assert_eq!(f(0.5, "h:mm:ss").as_deref(), Some("12:00:00"));
        assert_eq!(f(1.5, "[h]:mm").as_deref(), Some("36:00"));
        assert_eq!(f(1234567.891, "#,##0.00").as_deref(), Some("1,234,567.89"));
        assert_eq!(f(741700.0, "\"$\"#,##0").as_deref(), Some("$741,700"));
        assert_eq!(f(-5.0, "#,##0;(#,##0)").as_deref(), Some("(5)"));
        assert_eq!(f(0.824111, "0.0%").as_deref(), Some("82.4%"));
        assert_eq!(f(12.0, "0 \"pcs\"").as_deref(), Some("12 pcs"));
        assert_eq!(f(12345.0, "0.00E+00").as_deref(), Some("1.23E+04"));
        assert_eq!(f(3.0, "[Red]0"), Some("3".to_string()));
        assert_eq!(f(3.0, "General"), None);
        assert_eq!(f(3.0, "@"), None);
        // 1904 dates count from 1904, and the leap day 1900 never had is passed over.
        assert_eq!(
            number_format(0.0, "yyyy-mm-dd", true).as_deref(),
            Some("1904-01-01")
        );
        assert_eq!(f(59.0, "yyyy-mm-dd").as_deref(), Some("1900-02-28"));
        assert_eq!(f(61.0, "yyyy-mm-dd").as_deref(), Some("1900-03-01"));
    }

    fn xlsx(sheet: &str, extra: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(".xlsx").tempfile().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let options = zip::write::SimpleFileOptions::default();
        let ns = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let parts: Vec<(String, Vec<u8>)> = vec![
            (
                "xl/workbook.xml".into(),
                format!(r#"<workbook {ns}><sheets><sheet name="One" sheetId="1" r:id="rId1"/><sheet name="Two" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#).into_bytes(),
            ),
            (
                "xl/_rels/workbook.xml.rels".into(),
                br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="worksheet" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "xl/styles.xml".into(),
                format!(r#"<styleSheet {ns}><numFmts><numFmt numFmtId="164" formatCode="yyyy-mm-dd"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="3"/></cellXfs></styleSheet>"#).into_bytes(),
            ),
            (
                "xl/sharedStrings.xml".into(),
                format!(r#"<sst {ns}><si><t>Title</t></si><si><r><t>a|b</t></r><r><t>*c*</t></r></si></sst>"#).into_bytes(),
            ),
            ("xl/worksheets/sheet1.xml".into(), format!(r#"<worksheet {ns}>{sheet}</worksheet>"#).into_bytes()),
            ("xl/worksheets/sheet2.xml".into(), format!(r#"<worksheet {ns}><sheetData><row r="1"><c r="A1"><v>7</v></c></row></sheetData></worksheet>"#).into_bytes()),
        ];
        for (name, bytes) in parts {
            zip.start_file(name, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn sheets_are_tables_with_merged_cells_and_formatted_values() {
        let sheet = concat!(
            r#"<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"/><c r="C1"/></row>"#,
            r#"<row r="2"><c r="A2" s="1"><v>46295</v></c><c r="B2" s="2"><v>1234.5</v></c><c r="C2" t="b"><v>1</v></c></row>"#,
            r#"<row r="3"><c r="A3" t="s"><v>1</v></c><c r="B3" t="e"><f>1/0</f><v>#DIV/0!</v></c>"#,
            r#"<c r="C3" t="inlineStr"><is><t>^</t></is></c></row>"#,
            r#"<row r="4"><c r="B4" t="str"><f>"x"</f><v>On track</v></c></row>"#,
            r#"<row r="9"><c r="A9"><v>9</v></c></row></sheetData>"#,
            r#"<mergeCells><mergeCell ref="A1:C1"/><mergeCell ref="B4:C5"/></mergeCells>"#,
        );
        let file = xlsx(sheet, &[]);
        let blocks: Vec<String> = xlsx_blocks(file.path(), None)
            .unwrap()
            .into_iter()
            .map(|b| b.text)
            .collect();
        let heading = |name: &str| {
            format!(
                "## {}",
                crate::i18n::trf("label.sheet", &[("name", name.to_string())]).trim()
            )
        };
        let blank = crate::i18n::trf("label.empty_rows", &[("count", "3".to_string())]);
        assert_eq!(blocks[0], heading("One"));
        assert_eq!(
            blocks[1],
            [
                "| Title | < | < |",
                "| --- | --- | --- |",
                "| 2026-09-30 | 1,235 | TRUE |",
                "| a\\|b\\*c\\* | #DIV/0! | \\^ |",
                "|  | On track | < |",
                "|  | ^ | < |",
                &format!("| *\u{22ef} {blank}* |  |  |"),
                "| 9 |  |  |",
            ]
            .join("\n")
        );
        // A hidden sheet is read too.
        assert_eq!(blocks[2], heading("Two"));
        assert_eq!(blocks[3], "| 7 |\n| --- |");
        let cells = crate::core::web_markdown::table_cells(&blocks[1]).unwrap();
        use crate::core::web_markdown::{spans, Slot};
        let slots = spans(&cells);
        assert_eq!(slots[0][0], Slot::Anchor { cols: 3, rows: 1 });
        assert_eq!(slots[3][1], Slot::Anchor { cols: 2, rows: 2 });
        assert_eq!(slots[4][2], Slot::Covered { row: 3, col: 1 });
    }

    #[test]
    fn pictures_are_listed_with_their_cells() {
        let sheet = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><drawing r:id="rId1"/>"#;
        let drawing = br#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
            <xdr:twoCellAnchor><xdr:from><xdr:col>7</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
            <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Picture 1" descr="A photo"/></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor>
            </xdr:wsDr>"#;
        let file = xlsx(
            sheet,
            &[
                (
                    "xl/worksheets/_rels/sheet1.xml.rels",
                    br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
                ),
                ("xl/drawings/drawing1.xml", drawing),
                (
                    "xl/drawings/_rels/drawing1.xml.rels",
                    br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="image" Target="../media/image1.png"/></Relationships>"#,
                ),
                ("xl/media/image1.png", b"pixels"),
            ],
        );
        let blocks = xlsx_blocks(file.path(), None).unwrap();
        assert_eq!(
            blocks[2].text,
            format!("*Image: A photo at H3 ({})*", fingerprint(b"pixels"))
        );
    }
}
