//! Reading documents, spreadsheets and executables as text, so that they can be compared
//! without installing a converter. Everything here is read-only: a file is never written back,
//! and the text is a reading of the file, not a copy of it, so the compare it feeds is marked
//! read-only too.
//!
//! What is read here needs no external program. An [`Unpacker`](super::unpacker::Unpacker) the
//! user configured is used in preference, so a better converter can always be put in front.

mod pdf;

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;

/// The most text one file is read as, as for the external converters.
const MAX_OUTPUT: usize = 16 << 20;

/// The most bytes read from one part of a document; a part larger than this is left out rather
/// than unpacked, so that a small file cannot ask for a large amount of memory.
const MAX_PART: u64 = 64 << 20;

/// What a file is read as.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Word (`.docx`) or OpenDocument text (`.odt`): the words of the document, with the cells
    /// of its tables separated by tabs.
    Document,
    /// Rich Text Format (`.rtf`).
    Rtf,
    /// A spreadsheet (`.xlsx`, `.ods`): every sheet as comma separated records, which the table
    /// compare then shows as a table.
    Spreadsheet,
    /// An ELF executable, shared object, object file or core: its header and section table.
    Elf,
    /// A PDF file: the text of its pages.
    Pdf,
}

impl Kind {
    /// What the file at `path` is read as, from its name, or for a file without an extension
    /// from the four bytes it starts with. `None` for anything compared as it is.
    pub fn of(path: &Path) -> Option<Kind> {
        let extension = super::text::extension(path);
        match extension.as_str() {
            "docx" | "odt" => Some(Kind::Document),
            "rtf" => Some(Kind::Rtf),
            "pdf" => Some(Kind::Pdf),
            "xlsx" | "xlsm" | "ods" => Some(Kind::Spreadsheet),
            "so" | "o" | "ko" | "elf" | "axf" => Some(Kind::Elf),
            // A program on Unix usually has no extension of its own, so those are the only
            // files whose first bytes are read; everything else is told by its name alone.
            "" => elf_magic(path).then_some(Kind::Elf),
            _ => None,
        }
    }

    /// The name of the kind, for the description of a converted file.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Document => "label.extracted_document",
            Kind::Rtf => "label.extracted_document",
            Kind::Spreadsheet => "label.extracted_sheet",
            Kind::Elf => "label.extracted_elf",
            Kind::Pdf => "label.extracted_document",
        }
    }

    /// Reads the file at `path` as text.
    pub fn read(self, path: &Path) -> Result<String> {
        let text = match self {
            Kind::Document => document(path),
            Kind::Rtf => rtf(path),
            Kind::Spreadsheet => spreadsheet(path),
            Kind::Elf => elf(path),
            Kind::Pdf => pdf::text(&std::fs::read(path)?).map(tidy),
        }
        .with_context(|| {
            crate::i18n::trf(
                "msg.cannot_extract",
                &[("file", path.display().to_string())],
            )
        })?;
        Ok(cut(text))
    }
}

/// Whether the file starts with the four bytes every ELF file does.
fn elf_magic(path: &Path) -> bool {
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == *b"\x7fELF")
}

/// Keeps the text within [`MAX_OUTPUT`], saying so at the end when it does not fit.
fn cut(mut text: String) -> String {
    if text.len() <= MAX_OUTPUT {
        return text;
    }
    let mut end = MAX_OUTPUT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('\n');
    text.push_str(crate::i18n::tr("msg.extract_truncated"));
    text.push('\n');
    text
}

// ---------- the parts of a zipped document ----------

/// The bytes of the part `name` of the zipped document `path`.
fn part(path: &Path, name: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let entry = archive
        .by_name(name)
        .with_context(|| format!("{name} is missing"))?;
    if entry.size() > MAX_PART {
        bail!("{name} is too large to read");
    }
    let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
    entry.take(MAX_PART).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The same, for a part that a document need not have.
fn optional_part(path: &Path, name: &str) -> Option<Vec<u8>> {
    part(path, name).ok()
}

/// Namespaces, so that a document is read by what its elements mean rather than by the prefix
/// it happens to use for them.
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const ODF_TEXT: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const ODF_TABLE: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";

/// An element's namespace and local name, so that a document is read by what its elements mean
/// rather than by the prefix it happens to use for them.
fn named(
    reader: &quick_xml::NsReader<&[u8]>,
    name: quick_xml::name::QName<'_>,
) -> (Option<String>, String) {
    let (ns, local) = reader.resolver().resolve_element(name);
    let ns = match ns {
        quick_xml::name::ResolveResult::Bound(n) => Some(n.as_ref().to_owned()),
        _ => None,
    };
    (ns, local.as_ref().to_owned())
}

/// The value of the attribute `name` of `e`, whatever namespace it is in.
fn attribute(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let key = a.key.as_ref().to_owned();
        let local = key.rsplit(':').next().unwrap_or(&key);
        (local == name).then(|| a.value.as_ref().to_owned())
    })
}

// ---------- Word and OpenDocument text ----------

fn document(path: &Path) -> Result<String> {
    match super::text::extension(path).as_str() {
        "odt" => odf_text(&part(path, "content.xml")?),
        _ => docx(&part(path, "word/document.xml")?),
    }
}

/// The text of a `word/document.xml`: one line a paragraph, a table row to a line with its
/// cells separated by tabs.
fn docx(xml: &[u8]) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut cells = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() == Some(W) {
                    match local.as_str() {
                        "tc" => cells += 1,
                        "tab" => out.push('\t'),
                        "br" | "cr" => out.push('\n'),
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() != Some(W) {
                    continue;
                }
                match local.as_str() {
                    // A paragraph inside a cell is part of the cell, not a line of its own.
                    "p" if cells == 0 => out.push('\n'),
                    "p" => out.push(' '),
                    "tc" => {
                        cells = cells.saturating_sub(1);
                        while out.ends_with(' ') {
                            out.pop();
                        }
                        out.push('\t');
                    }
                    "tr" => {
                        while out.ends_with('\t') {
                            out.pop();
                        }
                        out.push('\n');
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                out.push_str(&e.xml10_content());
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    Ok(tidy(out))
}

/// The text of an OpenDocument `content.xml`: one line a paragraph or heading, a table row to a
/// line with its cells separated by tabs.
fn odf_text(xml: &[u8]) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut cells = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                match (ns.as_deref(), local.as_str()) {
                    (Some(ODF_TABLE), "table-cell") => cells += 1,
                    (Some(ODF_TEXT), "tab") => out.push('\t'),
                    (Some(ODF_TEXT), "line-break") => out.push('\n'),
                    (Some(ODF_TEXT), "s") => {
                        let n: usize = attribute(e, "c")
                            .and_then(|c| c.parse().ok())
                            .unwrap_or(1)
                            .min(1000);
                        out.push_str(&" ".repeat(n));
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                match (ns.as_deref(), local.as_str()) {
                    (Some(ODF_TEXT), "p" | "h") if cells == 0 => out.push('\n'),
                    (Some(ODF_TEXT), "p" | "h") => out.push(' '),
                    (Some(ODF_TABLE), "table-cell") => {
                        cells = cells.saturating_sub(1);
                        while out.ends_with(' ') {
                            out.pop();
                        }
                        out.push('\t');
                    }
                    (Some(ODF_TABLE), "table-row") => {
                        while out.ends_with('\t') {
                            out.pop();
                        }
                        out.push('\n');
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => out.push_str(&e.xml10_content()),
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    Ok(tidy(out))
}

/// Trailing spaces off each line, no more than one blank line in a row, one newline at the end.
fn tidy(text: String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    while out.starts_with('\n') {
        out.remove(0);
    }
    out
}

// ---------- spreadsheets ----------

fn spreadsheet(path: &Path) -> Result<String> {
    match super::text::extension(path).as_str() {
        "ods" => ods(&part(path, "content.xml")?),
        _ => xlsx(path),
    }
}

/// A record of cells as one comma separated line, quoted as a CSV reader expects.
fn record(cells: &[String]) -> String {
    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if cell.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push('\n');
    out
}

/// Cells with nothing after them on the line are left out, so that a sheet is as wide as its
/// contents rather than as wide as the file happens to record.
fn trim_record(mut cells: Vec<String>) -> Vec<String> {
    while cells.last().is_some_and(String::is_empty) {
        cells.pop();
    }
    cells
}

/// The column a cell reference such as `BC12` is in, counting from zero.
fn column_of(reference: &str) -> Option<usize> {
    let letters: String = reference
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n = 0usize;
    for c in letters.chars() {
        n = n * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    Some(n - 1)
}

fn xlsx(path: &Path) -> Result<String> {
    let shared = match optional_part(path, "xl/sharedStrings.xml") {
        Some(xml) => shared_strings(&xml)?,
        None => Vec::new(),
    };
    let targets =
        relationships(&optional_part(path, "xl/_rels/workbook.xml.rels").unwrap_or_default());
    let sheets = workbook_sheets(&part(path, "xl/workbook.xml")?)?;
    let mut out = String::new();
    let many = sheets.len() > 1;
    for (name, id) in &sheets {
        let target = id
            .as_ref()
            .and_then(|id| targets.iter().find(|(r, _)| r == id))
            .map(|(_, t)| t.clone());
        let Some(xml) =
            target.and_then(|t| optional_part(path, &format!("xl/{}", t.trim_start_matches('/'))))
        else {
            continue;
        };
        if many {
            out.push_str(&format!(
                "# {}\n",
                crate::i18n::trf("label.sheet", &[("name", name.clone())])
            ));
        }
        out.push_str(&sheet(&xml, &shared)?);
        if many {
            out.push('\n');
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    if sheets.is_empty() {
        bail!("the workbook lists no sheets");
    }
    Ok(out)
}

/// The sheets of `xl/workbook.xml`: each name with the relationship that leads to its part.
fn workbook_sheets(xml: &[u8]) -> Result<Vec<(String, Option<String>)>> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().expand_empty_elements = true;
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e))
            | Ok(quick_xml::events::Event::Empty(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() == Some(SHEET_NS) && local == "sheet" {
                    let name = attribute(e, "name").unwrap_or_default();
                    out.push((name, attribute(e, "id")));
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
    }
    Ok(out)
}

/// The relationship identifiers of a `.rels` part with what each leads to.
fn relationships(xml: &[u8]) -> Vec<(String, String)> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().expand_empty_elements = true;
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e))
            | Ok(quick_xml::events::Event::Empty(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() == Some(RELS) && local == "Relationship" {
                    if let (Some(id), Some(target)) = (attribute(e, "Id"), attribute(e, "Target")) {
                        out.push((id, target));
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

/// The strings of `xl/sharedStrings.xml`, which cells refer to by number.
fn shared_strings(xml: &[u8]) -> Result<Vec<String>> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut in_text = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() == Some(SHEET_NS) {
                    match local.as_str() {
                        "si" => current = Some(String::new()),
                        "t" => in_text = true,
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() == Some(SHEET_NS) {
                    match local.as_str() {
                        "si" => out.push(current.take().unwrap_or_default()),
                        "t" => in_text = false,
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_text {
                    if let Some(s) = &mut current {
                        s.push_str(&e.xml10_content());
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
    }
    Ok(out)
}

/// One worksheet as comma separated records.
fn sheet(xml: &[u8], shared: &[String]) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut cells: Vec<String> = Vec::new();
    let mut column = 0usize;
    let mut kind = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e))
            | Ok(quick_xml::events::Event::Empty(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() != Some(SHEET_NS) {
                    continue;
                }
                match local.as_str() {
                    "row" => {
                        cells.clear();
                        column = 0;
                    }
                    "c" => {
                        column = attribute(e, "r")
                            .as_deref()
                            .and_then(column_of)
                            .unwrap_or(cells.len());
                        kind = attribute(e, "t").unwrap_or_default();
                        value.clear();
                    }
                    "v" | "t" => {
                        value.clear();
                        in_value = true;
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_value {
                    value.push_str(&e.xml10_content());
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() != Some(SHEET_NS) {
                    continue;
                }
                match local.as_str() {
                    "v" | "t" => in_value = false,
                    "c" => {
                        let text = match kind.as_str() {
                            "s" => value
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i).cloned())
                                .unwrap_or_default(),
                            "b" => match value.trim() {
                                "1" => "TRUE".to_string(),
                                _ => "FALSE".to_string(),
                            },
                            _ => value.clone(),
                        };
                        while cells.len() < column {
                            cells.push(String::new());
                        }
                        if cells.len() == column {
                            cells.push(text);
                        } else {
                            cells[column] = text;
                        }
                        value.clear();
                    }
                    "row" => out.push_str(&record(&trim_record(std::mem::take(&mut cells)))),
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    Ok(out)
}

/// An OpenDocument spreadsheet `content.xml` as comma separated records.
fn ods(xml: &[u8]) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut tables = 0usize;
    let mut names: Vec<String> = Vec::new();
    let mut sheets: Vec<String> = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut repeat = 1usize;
    let mut in_cell = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e))
            | Ok(quick_xml::events::Event::Empty(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                match (ns.as_deref(), local.as_str()) {
                    (Some(ODF_TABLE), "table") => {
                        tables += 1;
                        names.push(attribute(e, "name").unwrap_or_default());
                        sheets.push(String::new());
                    }
                    (Some(ODF_TABLE), "table-row") => cells.clear(),
                    (Some(ODF_TABLE), "table-cell" | "covered-table-cell") => {
                        cell.clear();
                        in_cell = true;
                        // A run of equal cells is recorded once; a long trailing run of empty
                        // ones is how a sheet records its unused width, so it is capped.
                        repeat = attribute(e, "number-columns-repeated")
                            .and_then(|r| r.parse().ok())
                            .unwrap_or(1)
                            .min(1024);
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_cell {
                    cell.push_str(&e.xml10_content());
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                match (ns.as_deref(), local.as_str()) {
                    (Some(ODF_TEXT), "p") if in_cell && !cell.ends_with(' ') => cell.push(' '),
                    (Some(ODF_TABLE), "table-cell" | "covered-table-cell") => {
                        in_cell = false;
                        let text = cell.trim().to_string();
                        for _ in 0..repeat {
                            cells.push(text.clone());
                        }
                        repeat = 1;
                    }
                    (Some(ODF_TABLE), "table-row") => {
                        let line = record(&trim_record(std::mem::take(&mut cells)));
                        if let Some(s) = sheets.last_mut() {
                            s.push_str(&line);
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if sheets.iter().map(String::len).sum::<usize>() > MAX_OUTPUT {
            break;
        }
    }
    for (i, body) in sheets.iter().enumerate() {
        // Sheets a spreadsheet keeps but never filled in say nothing.
        if body.trim().is_empty() {
            continue;
        }
        if tables > 1 {
            out.push_str(&format!(
                "# {}\n",
                crate::i18n::trf("label.sheet", &[("name", names[i].clone())])
            ));
        }
        out.push_str(body);
        if tables > 1 {
            out.push('\n');
        }
    }
    Ok(out)
}

// ---------- Rich Text Format ----------

/// The text of an RTF file: its control words read, its tables to tab separated lines, and the
/// parts that describe fonts, colors and pictures rather than text left out.
fn rtf(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(tidy(rtf_text(&bytes)))
}

/// Groups whose contents describe the document rather than being part of it.
const RTF_SKIP: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "listtable",
    "listoverridetable",
    "revtbl",
    "rsidtbl",
    "generator",
    "info",
    "pict",
    "object",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "xmlnstbl",
    "filetbl",
];

fn rtf_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    // For each open group: how deep it is, and whether its contents are left out.
    let mut skipping: Vec<bool> = vec![false];
    // How many characters follow a \uN that repeat it for readers that cannot show it.
    let mut skip_after_unicode = 1usize;
    let mut pending = 0usize;
    let mut i = 0usize;
    while i < bytes.len() && out.len() <= MAX_OUTPUT {
        let b = bytes[i];
        let hidden = *skipping.last().unwrap_or(&false);
        match b {
            b'{' => {
                skipping.push(hidden);
                i += 1;
            }
            b'}' => {
                if skipping.len() > 1 {
                    skipping.pop();
                }
                i += 1;
            }
            b'\\' => {
                let (word, argument, next) = rtf_control(bytes, i + 1);
                i = next;
                match word.as_str() {
                    // \* marks a group whose contents a reader that does not know it leaves out.
                    "*" => {
                        if let Some(last) = skipping.last_mut() {
                            *last = true;
                        }
                    }
                    "" => {
                        // An escaped character: \\ \{ \} and the like.
                        if !hidden {
                            if let Some(c) = argument.and_then(|n| char::from_u32(n as u32)) {
                                out.push(c);
                            }
                        }
                    }
                    "u" => {
                        if let Some(n) = argument {
                            if !hidden {
                                let code = if n < 0 { (n + 65536) as u32 } else { n as u32 };
                                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            }
                            pending = skip_after_unicode;
                        }
                    }
                    "uc" => skip_after_unicode = argument.unwrap_or(1).clamp(0, 32) as usize,
                    "par" | "line" | "row" | "sect" if !hidden => out.push('\n'),
                    "tab" | "cell" if !hidden => out.push('\t'),
                    "'" => {
                        if pending > 0 {
                            pending -= 1;
                        } else if !hidden {
                            if let Some(n) = argument {
                                let byte = [n as u8];
                                let (text, _, _) = encoding_rs::WINDOWS_1252.decode(&byte);
                                out.push_str(&text);
                            }
                        }
                    }
                    w if RTF_SKIP.contains(&w) => {
                        if let Some(last) = skipping.last_mut() {
                            *last = true;
                        }
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if pending > 0 {
                    pending -= 1;
                } else if !hidden {
                    let (text, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes[i..i + 1]);
                    out.push_str(&text);
                }
                i += 1;
            }
        }
    }
    out
}

/// The control word at `at` (just after its backslash): its name, its number, and where the
/// text after it starts. A name of `''` is a hexadecimal byte and an empty name an escaped
/// character, both with the character in the number.
fn rtf_control(bytes: &[u8], at: usize) -> (String, Option<i32>, usize) {
    let mut i = at;
    if i >= bytes.len() {
        return (String::new(), None, i);
    }
    if bytes[i] == b'\'' {
        let hex = bytes.get(i + 1..i + 3).unwrap_or_default();
        let value = std::str::from_utf8(hex)
            .ok()
            .and_then(|h| i32::from_str_radix(h, 16).ok());
        return ("'".to_string(), value, (i + 3).min(bytes.len()));
    }
    if !bytes[i].is_ascii_alphabetic() {
        // An escaped character, or \* before a destination.
        let c = bytes[i];
        let word = if c == b'*' { "*" } else { "" };
        let argument = match c {
            b'\\' | b'{' | b'}' => Some(c as i32),
            b'~' => Some(' ' as i32),
            b'-' | b'_' => None,
            _ => None,
        };
        return (word.to_string(), argument, i + 1);
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let word = String::from_utf8_lossy(&bytes[start..i]).into_owned();
    let negative = bytes.get(i) == Some(&b'-');
    if negative {
        i += 1;
    }
    let digits = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let argument = (i > digits)
        .then(|| {
            std::str::from_utf8(&bytes[digits..i])
                .ok()?
                .parse::<i32>()
                .ok()
        })
        .flatten()
        .map(|n| if negative { -n } else { n });
    // One space after a control word belongs to it, not to the text.
    if bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    (word, argument, i)
}

// ---------- ELF ----------

/// The header and section table of an ELF file, as lines. Addresses and sizes are written in
/// full so that two builds of the same program line up line by line.
fn elf(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 64 || bytes[..4] != *b"\x7fELF" {
        bail!("not an ELF file");
    }
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        other => bail!("unknown ELF class {other}"),
    };
    let little = match bytes[5] {
        1 => true,
        2 => false,
        other => bail!("unknown ELF byte order {other}"),
    };
    let at = |offset: usize, size: usize| -> u64 {
        let Some(slice) = bytes.get(offset..offset + size) else {
            return 0;
        };
        let mut value = 0u64;
        for (i, b) in slice.iter().enumerate() {
            let shift = if little { i } else { size - 1 - i } * 8;
            value |= (*b as u64) << shift;
        }
        value
    };
    let half = |o: usize| at(o, 2);
    let word = |o: usize| at(o, 4);
    let addr = |o: usize| if wide { at(o, 8) } else { at(o, 4) };

    let kind = match half(16) {
        0 => "none".to_string(),
        1 => "relocatable".to_string(),
        2 => "executable".to_string(),
        3 => "shared object".to_string(),
        4 => "core".to_string(),
        other => format!("type {other}"),
    };
    let machine = machine_name(half(18));
    let (entry_o, phoff_o, shoff_o) = if wide { (24, 32, 40) } else { (24, 28, 32) };
    let entry = addr(entry_o);
    let phoff = addr(phoff_o);
    let shoff = addr(shoff_o);
    let (ehsize_o, phentsize_o, phnum_o, shentsize_o, shnum_o, shstrndx_o) = if wide {
        (52, 54, 56, 58, 60, 62)
    } else {
        (40, 42, 44, 46, 48, 50)
    };
    let mut out = String::new();
    out.push_str(&format!(
        "ELF {} {} {}\n",
        if wide { "64-bit" } else { "32-bit" },
        if little {
            "little-endian"
        } else {
            "big-endian"
        },
        kind
    ));
    out.push_str(&format!("Machine: {machine}\n"));
    out.push_str(&format!(
        "ABI: {} version {}\n",
        abi_name(bytes[7]),
        bytes[8]
    ));
    out.push_str(&format!("Version: {}\n", word(20)));
    out.push_str(&format!("Entry point: 0x{entry:016x}\n"));
    out.push_str(&format!(
        "Flags: 0x{:08x}\n",
        word(if wide { 48 } else { 36 })
    ));
    out.push_str(&format!("Header size: {}\n", half(ehsize_o)));
    out.push_str(&format!(
        "Program headers: {} of {} bytes at 0x{phoff:x}\n",
        half(phnum_o),
        half(phentsize_o)
    ));
    let count = half(shnum_o) as usize;
    let size = half(shentsize_o) as usize;
    out.push_str(&format!(
        "Section headers: {count} of {size} bytes at 0x{shoff:x}\n"
    ));

    // The names of the sections are in the section the header points at.
    let names_index = half(shstrndx_o) as usize;
    let names = (names_index < count && size >= 16)
        .then(|| {
            let head = (shoff as usize).checked_add(names_index.checked_mul(size)?)?;
            let (offset_o, size_o) = if wide { (24, 32) } else { (16, 20) };
            let start = addr(head + offset_o) as usize;
            let length = addr(head + size_o) as usize;
            bytes
                .get(start..start.saturating_add(length))
                .map(<[u8]>::to_vec)
        })
        .flatten()
        .unwrap_or_default();
    let name_at = |offset: usize| -> String {
        let rest = names.get(offset..).unwrap_or_default();
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    };

    out.push_str("\nSections:\n");
    for i in 0..count.min(4096) {
        let Some(head) = (shoff as usize).checked_add(i * size) else {
            break;
        };
        if head.saturating_add(size) > bytes.len() {
            break;
        }
        let (type_o, flags_o, addr_o, offset_o, size_o, link_o, info_o, align_o, entsize_o) =
            if wide {
                (4, 8, 16, 24, 32, 40, 44, 48, 56)
            } else {
                (4, 8, 12, 16, 20, 24, 28, 32, 36)
            };
        out.push_str(&format!(
            "[{i:3}] {:<20} {:<14} addr 0x{:016x} off 0x{:08x} size {:>10} align {:>5} entsize {:>4} link {} info {} flags {}\n",
            name_at(word(head) as usize),
            section_type(word(head + type_o)),
            addr(head + addr_o),
            addr(head + offset_o),
            addr(head + size_o),
            addr(head + align_o),
            addr(head + entsize_o),
            word(head + link_o),
            word(head + info_o),
            section_flags(addr(head + flags_o)),
        ));
    }
    Ok(out)
}

fn machine_name(value: u64) -> String {
    match value {
        0 => "none".into(),
        3 => "x86".into(),
        8 => "MIPS".into(),
        20 => "PowerPC".into(),
        21 => "PowerPC 64-bit".into(),
        22 => "S/390".into(),
        40 => "ARM".into(),
        42 => "SuperH".into(),
        43 => "SPARC v9".into(),
        50 => "IA-64".into(),
        62 => "x86-64".into(),
        183 => "AArch64".into(),
        243 => "RISC-V".into(),
        other => format!("machine {other}"),
    }
}

fn abi_name(value: u8) -> String {
    match value {
        0 => "System V".into(),
        1 => "HP-UX".into(),
        2 => "NetBSD".into(),
        3 => "Linux".into(),
        6 => "Solaris".into(),
        9 => "FreeBSD".into(),
        12 => "OpenBSD".into(),
        other => format!("ABI {other}"),
    }
}

fn section_type(value: u64) -> String {
    match value {
        0 => "NULL".into(),
        1 => "PROGBITS".into(),
        2 => "SYMTAB".into(),
        3 => "STRTAB".into(),
        4 => "RELA".into(),
        5 => "HASH".into(),
        6 => "DYNAMIC".into(),
        7 => "NOTE".into(),
        8 => "NOBITS".into(),
        9 => "REL".into(),
        11 => "DYNSYM".into(),
        14 => "INIT_ARRAY".into(),
        15 => "FINI_ARRAY".into(),
        16 => "PREINIT_ARRAY".into(),
        17 => "GROUP".into(),
        18 => "SYMTAB_SHNDX".into(),
        0x6ffffff6 => "GNU_HASH".into(),
        0x6ffffffe => "GNU_VERNEED".into(),
        0x6fffffff => "GNU_VERSYM".into(),
        other => format!("0x{other:x}"),
    }
}

/// The flags of a section as the letters `readelf` uses.
fn section_flags(value: u64) -> String {
    let mut out = String::new();
    for (bit, letter) in [
        (0x1, 'W'),
        (0x2, 'A'),
        (0x4, 'X'),
        (0x10, 'M'),
        (0x20, 'S'),
        (0x40, 'I'),
        (0x80, 'L'),
        (0x200, 'G'),
        (0x400, 'T'),
    ] {
        if value & bit != 0 {
            out.push(letter);
        }
    }
    if out.is_empty() {
        out.push('-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zipped document built from its parts, as Word and OpenDocument write them.
    fn zipped(dir: &Path, name: &str, parts: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (entry, body) in parts {
            zip.start_file(*entry, options).unwrap();
            std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    const DOCX: &str = concat!(
        r#"<?xml version="1.0"?><w:document xmlns:w=""#,
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        r#""><w:body>"#,
        r#"<w:p><w:r><w:t>First line</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Second</w:t><w:tab/><w:t>after a tab</w:t></w:r></w:p>"#,
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a1</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:p><w:r><w:t>b1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        r#"</w:body></w:document>"#
    );

    #[test]
    fn a_word_document_is_read_as_its_paragraphs_and_table_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = zipped(dir.path(), "a.docx", &[("word/document.xml", DOCX)]);
        assert_eq!(Kind::of(&path), Some(Kind::Document));
        let text = Kind::Document.read(&path).unwrap();
        assert_eq!(text, "First line\nSecond\tafter a tab\na1\tb1\n");
    }

    #[test]
    fn an_opendocument_text_is_read_as_its_paragraphs_and_table_rows() {
        let dir = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"<?xml version="1.0"?><office:document-content "#,
            r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
            r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
            r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">"#,
            r#"<text:h>Title</text:h><text:p>A line<text:tab/>and more</text:p>"#,
            r#"<table:table><table:table-row>"#,
            r#"<table:table-cell><text:p>a1</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>b1</text:p></table:table-cell>"#,
            r#"</table:table-row></table:table>"#,
            r#"</office:document-content>"#
        );
        let path = zipped(dir.path(), "a.odt", &[("content.xml", content)]);
        assert_eq!(Kind::of(&path), Some(Kind::Document));
        let text = Kind::Document.read(&path).unwrap();
        assert_eq!(text, "Title\nA line\tand more\na1\tb1\n");
    }

    /// The prefix a document uses for a namespace is its own choice, so reading must go by the
    /// namespace itself.
    #[test]
    fn a_document_that_names_the_word_namespace_differently_reads_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let odd = DOCX
            .replace("<w:", "<x:")
            .replace("</w:", "</x:")
            .replace("xmlns:w=", "xmlns:x=");
        let path = zipped(dir.path(), "b.docx", &[("word/document.xml", &odd)]);
        assert_eq!(Kind::Document.read(&path).unwrap(), DOCX_TEXT);
    }

    const DOCX_TEXT: &str = "First line\nSecond\tafter a tab\na1\tb1\n";

    fn workbook(sheets: &[(&str, &str)], shared: &[&str]) -> Vec<(String, String)> {
        let mut parts = Vec::new();
        let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rels = "http://schemas.openxmlformats.org/package/2006/relationships";
        let list: String = sheets
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                format!(
                    r#"<sheet name="{name}" sheetId="{n}" r:id="rId{n}"/>"#,
                    n = i + 1
                )
            })
            .collect();
        parts.push((
            "xl/workbook.xml".to_string(),
            format!(
                r#"<workbook xmlns="{ns}" xmlns:r="{rels}"><sheets>{list}</sheets></workbook>"#
            ),
        ));
        let links: String = (1..=sheets.len())
            .map(|n| format!(r#"<Relationship Id="rId{n}" Target="worksheets/sheet{n}.xml"/>"#))
            .collect();
        parts.push((
            "xl/_rels/workbook.xml.rels".to_string(),
            format!(r#"<Relationships xmlns="{rels}">{links}</Relationships>"#),
        ));
        if !shared.is_empty() {
            let items: String = shared
                .iter()
                .map(|s| format!("<si><t>{s}</t></si>"))
                .collect();
            parts.push((
                "xl/sharedStrings.xml".to_string(),
                format!(r#"<sst xmlns="{ns}">{items}</sst>"#),
            ));
        }
        for (i, (_, body)) in sheets.iter().enumerate() {
            parts.push((
                format!("xl/worksheets/sheet{}.xml", i + 1),
                format!(r#"<worksheet xmlns="{ns}"><sheetData>{body}</sheetData></worksheet>"#),
            ));
        }
        parts
    }

    #[test]
    fn a_workbook_is_read_as_comma_separated_records() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>"#,
            r#"<row r="2"><c r="A2"><v>12.5</v></c><c r="C2" t="s"><v>2</v></c></row>"#,
            r#"<row r="3"><c r="A3" t="b"><v>1</v></c></row>"#
        );
        let parts = workbook(&[("Only", rows)], &["name", "note", "a, comma"]);
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "a.xlsx", &borrowed);
        assert_eq!(Kind::of(&path), Some(Kind::Spreadsheet));
        let text = Kind::Spreadsheet.read(&path).unwrap();
        // The gap at B2 is kept so that the columns line up, and a comma in a value is quoted.
        assert_eq!(text, "name,note\n12.5,,\"a, comma\"\nTRUE\n");
        // Read as comma separated records, so the table compare shows it as a table.
        assert_eq!(super::super::table::delimiter_for(&path), Some(','));
    }

    #[test]
    fn a_workbook_of_several_sheets_names_each_one() {
        let dir = tempfile::tempdir().unwrap();
        let parts = workbook(
            &[
                ("First", r#"<row r="1"><c r="A1"><v>1</v></c></row>"#),
                ("Second", r#"<row r="1"><c r="A1"><v>2</v></c></row>"#),
            ],
            &[],
        );
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "two.xlsx", &borrowed);
        let text = Kind::Spreadsheet.read(&path).unwrap();
        assert!(text.contains("First"), "{text}");
        assert!(text.contains("Second"), "{text}");
        assert!(text.contains("\n1\n"), "{text}");
    }

    #[test]
    fn an_opendocument_spreadsheet_repeats_the_cells_it_records_once() {
        let dir = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"<?xml version="1.0"?><office:document-content "#,
            r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
            r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
            r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">"#,
            r#"<table:table table:name="S"><table:table-row>"#,
            r#"<table:table-cell><text:p>x</text:p></table:table-cell>"#,
            r#"<table:table-cell table:number-columns-repeated="3"><text:p>y</text:p></table:table-cell>"#,
            r#"<table:table-cell table:number-columns-repeated="900"/>"#,
            r#"</table:table-row></table:table>"#,
            r#"</office:document-content>"#
        );
        let path = zipped(dir.path(), "a.ods", &[("content.xml", content)]);
        assert_eq!(Kind::of(&path), Some(Kind::Spreadsheet));
        // The long run of empty cells that records the sheet's width is not part of the row.
        assert_eq!(Kind::Spreadsheet.read(&path).unwrap(), "x,y,y,y\n");
    }

    /// A cell with nothing in it is written as one element, and every cell after it is in a
    /// column further along: losing it would move a whole row left.
    #[test]
    fn an_empty_cell_of_an_opendocument_spreadsheet_keeps_its_column() {
        let dir = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"<?xml version="1.0"?><office:document-content "#,
            r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
            r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
            r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">"#,
            r#"<table:table table:name="S"><table:table-row>"#,
            r#"<table:table-cell><text:p>a</text:p></table:table-cell>"#,
            r#"<table:table-cell/>"#,
            r#"<table:table-cell table:number-columns-repeated="2"/>"#,
            r#"<table:table-cell><text:p>c</text:p></table:table-cell>"#,
            r#"</table:table-row></table:table>"#,
            r#"</office:document-content>"#
        );
        let path = zipped(dir.path(), "gap.ods", &[("content.xml", content)]);
        assert_eq!(Kind::Spreadsheet.read(&path).unwrap(), "a,,,,c\n");
    }

    /// The same for a worksheet, where a cell may also be written as one element.
    #[test]
    fn an_empty_cell_of_a_worksheet_keeps_its_column() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"/>"#,
            r#"<c r="C1"><v>3</v></c></row>"#
        );
        let parts = workbook(&[("Only", rows)], &[]);
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "gap.xlsx", &borrowed);
        assert_eq!(Kind::Spreadsheet.read(&path).unwrap(), "1,,3\n");
    }

    /// An ELF file may say its sections are anywhere at all, including past its own end.
    #[test]
    fn an_elf_file_that_points_outside_itself_is_refused_rather_than_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wild");
        let mut bytes = elf_file();
        bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        // Read, and says what it can, rather than ending the program.
        let text = Kind::Elf.read(&path).unwrap();
        assert!(text.starts_with("ELF 64-bit"), "{text}");
    }

    #[test]
    fn rich_text_is_read_without_its_font_and_colour_tables() {
        let rtf = concat!(
            "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Times;}}{\\colortbl;\\red0\\green0\\blue0;}\n",
            "{\\*\\generator Something 1.0;}\n",
            "Hello \\b bold\\b0  world\\par\n",
            "Second\\tab line\\par\n",
            "caf\\'e9 and \\u8212?dash\\par\n",
            "}"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rtf");
        std::fs::write(&path, rtf).unwrap();
        assert_eq!(Kind::of(&path), Some(Kind::Rtf));
        let text = Kind::Rtf.read(&path).unwrap();
        assert_eq!(text, "Hello bold world\nSecond\tline\ncafé and —dash\n");
    }

    /// A header of a 64-bit little-endian program with two sections.
    fn elf_file() -> Vec<u8> {
        let names = b"\0.text\0.shstrtab\0";
        let header = 64usize;
        let section_size = 64usize;
        let sections = 3usize;
        let shoff = header;
        let names_at = shoff + sections * section_size;
        let mut bytes = vec![0u8; names_at + names.len()];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2; // 64-bit
        bytes[5] = 1; // little-endian
        bytes[6] = 1; // version
        bytes[7] = 3; // Linux
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes()); // executable
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x401000u64.to_le_bytes()); // entry
        bytes[40..48].copy_from_slice(&(shoff as u64).to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes()); // header size
        bytes[58..60].copy_from_slice(&(section_size as u16).to_le_bytes());
        bytes[60..62].copy_from_slice(&(sections as u16).to_le_bytes());
        bytes[62..64].copy_from_slice(&2u16.to_le_bytes()); // names are section 2
        let mut put =
            |index: usize, name: u32, kind: u32, flags: u64, addr: u64, off: u64, size: u64| {
                let at = shoff + index * section_size;
                bytes[at..at + 4].copy_from_slice(&name.to_le_bytes());
                bytes[at + 4..at + 8].copy_from_slice(&kind.to_le_bytes());
                bytes[at + 8..at + 16].copy_from_slice(&flags.to_le_bytes());
                bytes[at + 16..at + 24].copy_from_slice(&addr.to_le_bytes());
                bytes[at + 24..at + 32].copy_from_slice(&off.to_le_bytes());
                bytes[at + 32..at + 40].copy_from_slice(&size.to_le_bytes());
            };
        put(1, 1, 1, 0x6, 0x401000, 0x1000, 0x20); // .text, PROGBITS, A|X
        put(2, 7, 3, 0, 0, names_at as u64, names.len() as u64); // .shstrtab
        bytes[names_at..names_at + names.len()].copy_from_slice(names);
        bytes
    }

    #[test]
    fn an_elf_program_is_read_as_its_header_and_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("program");
        std::fs::write(&path, elf_file()).unwrap();
        // A program usually has no extension, so it is told by the bytes it starts with.
        assert_eq!(Kind::of(&path), Some(Kind::Elf));
        let text = Kind::Elf.read(&path).unwrap();
        assert!(
            text.starts_with("ELF 64-bit little-endian executable\n"),
            "{text}"
        );
        assert!(text.contains("Machine: x86-64\n"), "{text}");
        assert!(text.contains("ABI: Linux version 0\n"), "{text}");
        assert!(text.contains("Entry point: 0x0000000000401000\n"), "{text}");
        assert!(text.contains(".text"), "{text}");
        assert!(text.contains("PROGBITS"), "{text}");
        assert!(text.contains("flags AX"), "{text}");
        assert!(text.contains(".shstrtab"), "{text}");
    }

    #[test]
    fn a_file_that_is_not_what_its_name_says_is_reported_rather_than_shown_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.docx");
        std::fs::write(&path, b"not a zip at all").unwrap();
        let e = Kind::Document.read(&path).unwrap_err();
        assert!(format!("{e:#}").contains("broken.docx"), "{e:#}");
        // A plain file keeps its own name and is compared as it is.
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, b"hello").unwrap();
        assert_eq!(Kind::of(&plain), None);
    }
}
