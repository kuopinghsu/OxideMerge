//! Reading documents, spreadsheets and executables as text, so that they can be compared
//! without installing a converter. Everything here is read-only: a file is never written back,
//! and the text is a reading of the file, not a copy of it, so the compare it feeds is marked
//! read-only too.
//!
//! What is read here needs no external program. An [`Unpacker`](super::unpacker::Unpacker) the
//! user configured is used in preference, so a better converter can always be put in front.

mod markdown;
mod notebook;
pub(crate) mod pdf;

pub use markdown::{docx_blocks, pdf_blocks, xlsx_blocks};
pub use pdf::NotTagged;

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The most text one file is read as, as for the external converters.
const MAX_OUTPUT: usize = 16 << 20;

/// The most bytes read from one part of a document; a part larger than this is left out rather
/// than unpacked, so that a small file cannot ask for a large amount of memory.
const MAX_PART: u64 = 64 << 20;

/// The largest PDF or RTF file read, since those are read whole.
const MAX_FILE: u64 = 256 << 20;

/// The most sections of an ELF file listed.
const MAX_SECTIONS: usize = 4096;
/// The most symbols listed of one symbol table of an ELF file.
const MAX_SYMBOLS: usize = 100_000;

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
    /// A Jupyter notebook (`.ipynb`): each cell's own text under a line naming its kind,
    /// without its outputs (execution counts, captured results and embedded images), which
    /// are rebuilt by running the notebook rather than written by hand and would otherwise
    /// swamp the diff of what the notebook's author actually wrote.
    Notebook,
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
            "ipynb" => Some(Kind::Notebook),
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
            Kind::Notebook => "label.extracted_notebook",
        }
    }

    /// Whether two files of this kind are the same when their text is. The text of an ELF file
    /// is only its header and section table, so two different programs can read the same.
    pub fn compares_by_text(self) -> bool {
        self != Kind::Elf
    }

    /// Reads the file at `path` as text, giving up once `cancel` is set.
    #[cfg(test)]
    pub fn read(self, path: &Path, cancel: Option<&AtomicBool>) -> Result<String> {
        self.read_with_merges(path, cancel).map(|(text, _)| text)
    }

    /// Reads the extracted text and the table coordinates of any worksheet merges.
    pub fn read_with_merges(
        self,
        path: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<(String, TableMerges)> {
        let mut pages = None;
        let mut merges = TableMerges::default();
        let text = match self {
            Kind::Document => document(path, cancel),
            Kind::Rtf => rtf(path),
            Kind::Spreadsheet => match super::text::extension(path).as_str() {
                "ods" => part(path, "content.xml").and_then(|xml| ods(&xml, cancel, false)),
                _ => xlsx(path, cancel, false).map(|(text, ranges)| {
                    merges = ranges;
                    text
                }),
            },
            Kind::Elf => elf(path),
            Kind::Pdf => pdf_text(path, cancel).map(|(text, starts)| {
                pages = Some(starts);
                text
            }),
            Kind::Notebook => notebook::read(path, cancel),
        }
        .with_context(|| {
            crate::i18n::trf(
                "msg.cannot_extract",
                &[("file", path.display().to_string())],
            )
        })?;
        let text = cut(text);
        if let Some(starts) = pages {
            remember_pages(path, (starts, super::text::lines(&text).count()));
        }
        Ok((text, merges))
    }
}

/// Whether the file starts with the four bytes every ELF file does.
fn elf_magic(path: &Path) -> bool {
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == *b"\x7fELF")
}

/// The bytes of the file at `path`, refused when there are more than [`MAX_FILE`].
fn read_whole(path: &Path) -> Result<Vec<u8>> {
    let too_large = || anyhow::anyhow!("the file is larger than {} MiB", MAX_FILE >> 20);
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > MAX_FILE {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // The file may grow while it is read.
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Up to `size` bytes of `file` from `offset`, as many of them as the file has, and never more
/// than [`MAX_PART`].
fn piece(file: &mut std::fs::File, offset: u64, size: u64, len: u64) -> Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    if offset >= len {
        return Ok(Vec::new());
    }
    let size = size.min(len - offset).min(MAX_PART);
    file.seek(SeekFrom::Start(offset))?;
    let mut out = Vec::with_capacity(size as usize);
    file.by_ref().take(size).read_to_end(&mut out)?;
    Ok(out)
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
    optional_part(path, name)?.with_context(|| format!("{name} is missing"))
}

/// The same, for a part that a document need not have: `None` only when it has not. One that
/// is there but cannot be read is an error, since reading on without it would show the
/// document as something it is not, such as a sheet whose words are all missing.
fn optional_part(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{name} cannot be read")),
    };
    let too_large = || anyhow::anyhow!("{name} is larger than {} MiB", MAX_PART >> 20);
    if entry.size() > MAX_PART {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
    // The size an archive gives for an entry need not be the size it unpacks to.
    entry.take(MAX_PART + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PART {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

/// Namespaces, so that a document is read by what its elements mean rather than by the prefix
/// it happens to use for them.
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const ODF_TEXT: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const ODF_TABLE: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const MARKUP_COMPATIBILITY: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

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

/// The value of the attribute `name` of `e`, whatever namespace it is in, with its references
/// such as `&amp;` read.
fn attribute(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let key = a.key.as_ref().to_owned();
        let local = key.rsplit(':').next().unwrap_or(&key);
        (local == name).then(|| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_or_else(|_| a.value.to_string(), |v| v.into_owned())
        })
    })
}

/// The characters a reference in a document's text such as `&amp;` or `&#65;` stands for. One
/// the document defines for itself is kept as it is written.
fn reference(e: &quick_xml::events::BytesRef<'_>) -> String {
    match e.resolve_char_ref() {
        Ok(Some(c)) => c.to_string(),
        _ => quick_xml::escape::resolve_xml_entity(e)
            .map_or_else(|| format!("&{};", &**e), str::to_string),
    }
}

/// Fails once `cancel` is set, so that a long read stops when it is no longer wanted.
fn check(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        bail!("cancelled");
    }
    Ok(())
}

// ---------- Word and OpenDocument text ----------

fn document(path: &Path, cancel: Option<&AtomicBool>) -> Result<String> {
    match super::text::extension(path).as_str() {
        "odt" => odf_text(&part(path, "content.xml")?, cancel),
        _ => docx(&part(path, "word/document.xml")?, cancel),
    }
}

/// How many elements are read between looks at whether the read has been cancelled.
const CHECK_EVERY: usize = 4096;

/// The text of a `word/document.xml`: one line a paragraph, a table row to a line with its
/// cells separated by tabs.
fn docx(xml: &[u8], cancel: Option<&AtomicBool>) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut cells = 0usize;
    // Inside a run, inside a paragraph's properties (whose tab stops are not tabs), and inside
    // the text of a run; deleted text and field codes are in elements of their own.
    let (mut runs, mut properties, mut words) = (0usize, 0usize, 0usize);
    let mut buf = Vec::new();
    let mut skipped = Vec::new();
    for read in 0.. {
        if read % CHECK_EVERY == 0 {
            check(cancel)?;
        }
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                // What a reader that cannot show the choice before it shows instead, such as a
                // text box drawn the old way, holding the same words a second time; and text
                // moved elsewhere, which is read where it was moved to.
                let fallback = ns.as_deref() == Some(MARKUP_COMPATIBILITY) && local == "Fallback";
                if fallback || (ns.as_deref() == Some(W) && local == "moveFrom") {
                    let end = e.to_end().into_owned();
                    skipped.clear();
                    reader.read_to_end_into(end.name(), &mut skipped)?;
                    continue;
                }
                if ns.as_deref() == Some(W) {
                    match local.as_str() {
                        "tc" => cells += 1,
                        "r" => runs += 1,
                        "pPr" => properties += 1,
                        "t" => words += 1,
                        "tab" if runs > 0 && properties == 0 => out.push('\t'),
                        "br" | "cr" if runs > 0 => out.push('\n'),
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
                    "r" => runs = runs.saturating_sub(1),
                    "pPr" => properties = properties.saturating_sub(1),
                    "t" => words = words.saturating_sub(1),
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
            Ok(quick_xml::events::Event::Text(e)) if words > 0 => {
                out.push_str(&e.xml10_content());
            }
            Ok(quick_xml::events::Event::GeneralRef(e)) if words > 0 => {
                out.push_str(&reference(&e));
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
fn odf_text(xml: &[u8], cancel: Option<&AtomicBool>) -> Result<String> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut cells = 0usize;
    let mut buf = Vec::new();
    for read in 0.. {
        if read % CHECK_EVERY == 0 {
            check(cancel)?;
        }
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
            Ok(quick_xml::events::Event::GeneralRef(e)) => out.push_str(&reference(&e)),
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
    tidy_marked(text, &[]).0
}

/// [`tidy`], with the line of the tidy text that each of `marks`, byte offsets into `text` in
/// order, is at: the line it is on, or the next line kept when its own is left out.
fn tidy_marked(text: String, marks: &[usize]) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut at = Vec::with_capacity(marks.len());
    let (mut lines, mut blanks, mut offset) = (0usize, 0, 0);
    for line in text.split_inclusive('\n') {
        offset += line.len();
        while at.len() < marks.len() && marks[at.len()] < offset {
            at.push(lines);
        }
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
        lines += 1;
    }
    at.resize(marks.len(), lines);
    let lead = out.len() - out.trim_start_matches('\n').len();
    if lead > 0 {
        out.drain(..lead);
        at.iter_mut().for_each(|a| *a = a.saturating_sub(lead));
    }
    (out, at)
}

/// The text of the PDF file at `path` as [`Kind::read`] reads it, with the line each of its
/// pages starts on.
fn pdf_text(path: &Path, cancel: Option<&AtomicBool>) -> Result<(String, Vec<usize>)> {
    let (text, starts) = pdf::Document::read(&read_whole(path)?, cancel)?.text_pages()?;
    Ok(tidy_marked(text, &starts))
}

/// The line each page of a PDF file starts on in the text [`Kind::read`] reads from it, with the
/// number of lines of that text.
type Pages = (Vec<usize>, usize);

/// A file as it was when read: its path, size and the time it was written.
type Stamp = (std::path::PathBuf, u64, Option<std::time::SystemTime>);

/// The pages of the PDF files read lately, the latest last, so that a report naming the pages
/// of a difference finds them without reading a file it compared again.
static PAGES: std::sync::Mutex<Vec<(Stamp, Pages)>> = std::sync::Mutex::new(Vec::new());

/// How many files [`PAGES`] keeps the pages of.
const PAGES_KEPT: usize = 256;

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((path.to_path_buf(), meta.len(), meta.modified().ok()))
}

fn remember_pages(path: &Path, pages: Pages) {
    let Some(stamp) = stamp(path) else { return };
    let mut kept = PAGES.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|(s, _)| s.0 != stamp.0);
    if kept.len() >= PAGES_KEPT {
        kept.remove(0);
    }
    kept.push((stamp, pages));
}

/// The pages of the PDF file at `path`: as found when [`Kind::read`] read it, while the file is
/// unchanged, or read again.
pub fn pdf_pages(path: &Path, cancel: Option<&AtomicBool>) -> Result<Pages> {
    if let Some(stamp) = stamp(path) {
        let kept = PAGES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, pages)) = kept.iter().find(|(s, _)| *s == stamp) {
            return Ok(pages.clone());
        }
    }
    let (text, starts) = pdf_text(path, cancel)?;
    let pages = (starts, super::text::lines(&cut(text)).count());
    remember_pages(path, pages.clone());
    Ok(pages)
}

// ---------- spreadsheets ----------

/// A rectangle in a worksheet, with zero-based, inclusive corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MergeRange {
    first: (usize, usize),
    last: (usize, usize),
}

impl MergeRange {
    fn parse(reference: &str) -> Option<Self> {
        let (a, b) = reference.split_once(':')?;
        let (a, b) = (cell_at(a)?, cell_at(b)?);
        let first = (a.0.min(b.0), a.1.min(b.1));
        let last = (a.0.max(b.0), a.1.max(b.1));
        (first != last).then_some(Self { first, last })
    }
}

/// A worksheet merge and the table record corresponding to row 1 of that worksheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableMerge {
    pub sheet_start: usize,
    pub first: (usize, usize),
    pub last: (usize, usize),
    visible_last: (usize, usize),
}

impl TableMerge {
    #[cfg(test)]
    pub fn new(sheet_start: usize, first: (usize, usize), last: (usize, usize)) -> Self {
        Self {
            sheet_start,
            first,
            last,
            visible_last: last,
        }
    }

    pub fn first_record(self) -> usize {
        self.sheet_start + self.first.0
    }

    fn last_record(self) -> usize {
        self.sheet_start + self.visible_last.0
    }
}

/// Merges ordered by their first record, with an index of the latest end in each subtree.
#[derive(Clone, Debug, Default)]
pub struct TableMerges {
    ranges: std::sync::Arc<[TableMerge]>,
    latest_end: std::sync::Arc<[usize]>,
    /// Blank records added to show the parts of merges beyond the last written row.
    padding: std::sync::Arc<[std::ops::Range<usize>]>,
}

impl TableMerges {
    pub fn new(mut ranges: Vec<TableMerge>, padding: Vec<std::ops::Range<usize>>) -> Self {
        ranges.sort_unstable_by_key(|m| (m.first_record(), m.first.1));
        let leaves = ranges.len().next_power_of_two();
        let mut latest_end = vec![0; 2 * leaves];
        for (i, merge) in ranges.iter().enumerate() {
            latest_end[leaves + i] = merge.last_record();
        }
        for i in (1..leaves).rev() {
            latest_end[i] = latest_end[2 * i].max(latest_end[2 * i + 1]);
        }
        Self {
            ranges: ranges.into(),
            latest_end: latest_end.into(),
            padding: padding.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn width(&self, sheet_start: Option<usize>) -> usize {
        let ranges = match sheet_start {
            Some(start) => {
                let first = self.ranges.partition_point(|m| m.sheet_start < start);
                let count = self.ranges[first..].partition_point(|m| m.sheet_start == start);
                &self.ranges[first..first + count]
            }
            None => &self.ranges,
        };
        ranges
            .iter()
            .map(|m| m.visible_last.1 + 1)
            .max()
            .unwrap_or(0)
    }

    /// The merge covering a cell or spanning the records on either side of an aligned gap.
    pub fn covering(&self, (first, last): (usize, usize), column: usize) -> Option<TableMerge> {
        self.find_spanning(first, last, |m| (m.first.1..=m.last.1).contains(&column))
    }

    pub fn covers_row(&self, record: usize) -> bool {
        self.find_spanning(record, record, |_| true).is_some()
    }

    /// A row covered from above or added to reach the end of a merged range.
    pub fn extends_to(&self, record: usize) -> bool {
        self.is_padding(record)
            || record
                .checked_sub(1)
                .is_some_and(|previous| self.find_spanning(previous, record, |_| true).is_some())
    }

    pub fn is_padding(&self, record: usize) -> bool {
        let padding = self.padding.partition_point(|range| range.end <= record);
        self.padding
            .get(padding)
            .is_some_and(|r| r.contains(&record))
    }

    pub fn padding(&self) -> &[std::ops::Range<usize>] {
        &self.padding
    }

    fn find_spanning(
        &self,
        first: usize,
        last: usize,
        mut matches: impl FnMut(TableMerge) -> bool,
    ) -> Option<TableMerge> {
        let stop = self.ranges.partition_point(|m| m.first_record() <= first);
        self.find_in_subtree(1, 0..self.latest_end.len() / 2, (stop, last), &mut matches)
    }

    fn find_in_subtree(
        &self,
        node: usize,
        records: std::ops::Range<usize>,
        (stop, last): (usize, usize),
        matches: &mut impl FnMut(TableMerge) -> bool,
    ) -> Option<TableMerge> {
        if records.start >= stop || self.latest_end[node] < last {
            return None;
        }
        if records.len() == 1 {
            return self
                .ranges
                .get(records.start)
                .copied()
                .filter(|&m| matches(m));
        }
        let mid = records.start + records.len() / 2;
        self.find_in_subtree(2 * node, records.start..mid, (stop, last), matches)
            .or_else(|| self.find_in_subtree(2 * node + 1, mid..records.end, (stop, last), matches))
    }
}

/// A spreadsheet's records read the other way from [`Kind::read`], line for line as it: the
/// numbers of an `.xlsx` workbook as their number formats show them, where its text has them
/// as stored, or the values of an `.ods` one as stored, where its text has them as shown.
pub struct OtherCells {
    pub text: String,
    /// Whether `text` has the values as shown rather than as stored.
    pub shown: bool,
    pub sheet_merges: TableMerges,
}

/// The records of the spreadsheet at `path` read the other way, as [`OtherCells`] says.
pub fn other_cells(path: &Path, cancel: Option<&AtomicBool>) -> Result<OtherCells> {
    Ok(match super::text::extension(path).as_str() {
        "ods" => OtherCells {
            text: cut(ods(&part(path, "content.xml")?, cancel, true)?),
            shown: false,
            sheet_merges: TableMerges::default(),
        },
        _ => {
            let (text, sheet_merges) = xlsx(path, cancel, true)?;
            OtherCells {
                text: cut(text),
                shown: true,
                sheet_merges,
            }
        }
    })
}

/// The first line of a sheet's records when a workbook has several, so that a reader can tell
/// a workbook of several sheets, which is not one table, from a workbook of one.
pub fn is_sheet_heading(line: &str) -> bool {
    let heading = crate::i18n::trf("label.sheet", &[("name", String::new())]);
    line.strip_prefix("# ")
        .is_some_and(|rest| rest.starts_with(heading.trim_end()))
}

/// The heading of the sheet `name`.
fn sheet_heading(name: &str) -> String {
    format!(
        "# {}\n",
        crate::i18n::trf("label.sheet", &[("name", name.to_string())])
    )
}

/// The most columns a row is read as; as many as the widest spreadsheet programs allow.
const MAX_COLUMNS: usize = 16_384;
const MAX_XLSX_ROWS: usize = 1_048_576;
// Merge geometry alone can describe an entire worksheet with one XML element.
const MAX_MERGE_PADDING_ROWS: usize = 1_024;
const MAX_MERGE_PADDING_COLUMNS: usize = 128;

/// How many more copies of `line` fit when `used` bytes are already read, at least one.
fn copies_that_fit(line: &str, used: usize, wanted: usize) -> usize {
    let room = MAX_OUTPUT.saturating_sub(used) / line.len().max(1);
    wanted.min(room.max(1))
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

/// A reference such as `B12` as a zero-based row and column.
fn cell_at(reference: &str) -> Option<(usize, usize)> {
    let reference = reference.trim_start_matches('$');
    let col = column_of(reference)?;
    let digits = reference.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let row: usize = digits.trim_start_matches('$').parse().ok()?;
    Some((row.checked_sub(1)?, col))
}

/// The sheets of the workbook at `path` as comma separated records, with numbers as stored, or
/// as their number formats show them when `shown` is set.
fn xlsx(path: &Path, cancel: Option<&AtomicBool>, shown: bool) -> Result<(String, TableMerges)> {
    let formats = if shown {
        Some(markdown::number_formats(path, cancel)?)
    } else {
        None
    };
    let formats = formats.as_ref().map(|(f, date1904)| (f, *date1904));
    let shared = match optional_part(path, "xl/sharedStrings.xml")? {
        Some(xml) => shared_strings(&xml)?,
        None => Vec::new(),
    };
    let targets =
        relationships(&optional_part(path, "xl/_rels/workbook.xml.rels")?.unwrap_or_default());
    let sheets = workbook_sheets(&part(path, "xl/workbook.xml")?)?;
    let mut out = String::new();
    let mut merges = Vec::new();
    let mut padding = Vec::new();
    let mut padding_left = MAX_MERGE_PADDING_ROWS;
    let mut record_count = 0;
    let many = sheets.len() > 1;
    let mut found = false;
    for (name, id) in &sheets {
        let target = id
            .as_ref()
            .and_then(|id| targets.iter().find(|(r, _)| r == id))
            .and_then(|(_, t)| part_name("xl", t));
        let xml = match target {
            Some(t) => optional_part(path, &t)?,
            None => None,
        };
        let Some(xml) = xml else {
            continue;
        };
        found = true;
        if many {
            out.push_str(&sheet_heading(name));
            record_count += 1;
        }
        let sheet = sheet(&xml, &shared, formats, out.len(), padding_left, cancel)?;
        merges.extend(sheet.merges.into_iter().map(|range| TableMerge {
            sheet_start: record_count,
            first: range.first,
            last: range.last,
            visible_last: (
                range.last.0.min(sheet.records - 1),
                range.last.1.min(sheet.width - 1),
            ),
        }));
        if sheet.written < sheet.records {
            padding.push(record_count + sheet.written..record_count + sheet.records);
            padding_left -= sheet.records - sheet.written;
        }
        record_count += sheet.records;
        out.push_str(&sheet.text);
        if many {
            out.push('\n');
            record_count += 1;
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    if sheets.is_empty() {
        bail!("the workbook lists no sheets");
    }
    if !found {
        bail!("none of the sheets the workbook lists is in the file");
    }
    Ok((out, TableMerges::new(merges, padding)))
}

/// The part a relationship of a part in `folder` leads to: a target that starts with `/` is
/// named from the top of the package, any other from `folder`. `None` for one that leads out of
/// the package.
fn part_name(folder: &str, target: &str) -> Option<String> {
    let mut names: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        folder.split('/').filter(|s| !s.is_empty()).collect()
    };
    for name in target.split('/') {
        match name {
            "" | "." => {}
            ".." => {
                names.pop()?;
            }
            other => names.push(other),
        }
    }
    (!names.is_empty()).then(|| names.join("/"))
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
    // The reading of East Asian words written over them, which is not part of the value.
    let mut phonetic = 0usize;
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
                        "rPh" => phonetic += 1,
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
                        "rPh" => phonetic = phonetic.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_text && phonetic == 0 {
                    if let Some(s) = &mut current {
                        s.push_str(&e.xml10_content());
                    }
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(e)) => {
                if in_text && phonetic == 0 {
                    if let Some(s) = &mut current {
                        s.push_str(&reference(&e));
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

struct Worksheet {
    text: String,
    records: usize,
    written: usize,
    width: usize,
    merges: Vec<MergeRange>,
}

/// One worksheet as comma separated records, when `used` bytes of the workbook are already read:
/// its numbers as stored, or in the number formats `formats` gives with whether the workbook
/// counts dates from 1904.
fn sheet(
    xml: &[u8],
    shared: &[String],
    formats: Option<(&markdown::NumberFormats, bool)>,
    used: usize,
    padding_left: usize,
    cancel: Option<&AtomicBool>,
) -> Result<Worksheet> {
    let source = String::from_utf8_lossy(xml).into_owned();
    let mut reader = quick_xml::NsReader::from_str(&source);
    reader.config_mut().trim_text(false);
    // `<x/>` is read as a start and an end, so that an element with nothing in it —
    // an empty cell, most of all — is not missed by code that waits for its end.
    reader.config_mut().expand_empty_elements = true;
    let mut out = String::new();
    let mut records = 0;
    let mut width = 0;
    let mut merges = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut column = 0usize;
    // The number of the row the next one is when the sheet leaves none out, from one.
    let mut next_row = 1usize;
    let mut kind = String::new();
    let mut style = 0usize;
    let mut value = String::new();
    let mut in_value = false;
    let mut phonetic = 0usize;
    let mut buf = Vec::new();
    for read in 0.. {
        if read % CHECK_EVERY == 0 {
            check(cancel)?;
        }
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
                        // Rows with nothing in them are not written, so the rows on either side
                        // of them are kept apart by as many empty records.
                        let number = attribute(e, "r").and_then(|r| r.trim().parse::<usize>().ok());
                        if let Some(number) = number.filter(|&n| n > next_row) {
                            let gap = copies_that_fit("\n", used + out.len(), number - next_row);
                            out.push_str(&"\n".repeat(gap));
                            records += gap;
                        }
                        next_row = number
                            .map_or(next_row, |n| n.max(next_row))
                            .saturating_add(1);
                    }
                    "c" => {
                        column = attribute(e, "r")
                            .as_deref()
                            .and_then(column_of)
                            .unwrap_or(cells.len());
                        kind = attribute(e, "t").unwrap_or_default();
                        style = attribute(e, "s").and_then(|s| s.parse().ok()).unwrap_or(0);
                        value.clear();
                    }
                    "v" => {
                        value.clear();
                        in_value = true;
                    }
                    // The words of an inline string may be in several runs, each with its own.
                    "t" => in_value = true,
                    "rPh" => phonetic += 1,
                    "mergeCell" => {
                        if let Some(range) = attribute(e, "ref")
                            .as_deref()
                            .and_then(MergeRange::parse)
                            .filter(|m| m.last.0 < MAX_XLSX_ROWS && m.last.1 < MAX_COLUMNS)
                        {
                            merges.push(range);
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_value && phonetic == 0 {
                    value.push_str(&e.xml10_content());
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(e)) => {
                if in_value && phonetic == 0 {
                    value.push_str(&reference(&e));
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                if ns.as_deref() != Some(SHEET_NS) {
                    continue;
                }
                match local.as_str() {
                    "v" | "t" => in_value = false,
                    "rPh" => phonetic = phonetic.saturating_sub(1),
                    "c" if column >= MAX_COLUMNS => value.clear(),
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
                            "" | "n" => match formats {
                                Some((f, date1904)) => f.show(&value, style, date1904),
                                None => value.clone(),
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
                    "row" => {
                        let cells = trim_record(std::mem::take(&mut cells));
                        width = width.max(cells.len());
                        out.push_str(&record(&cells));
                        records += 1;
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if used + out.len() > MAX_OUTPUT {
            break;
        }
    }
    // XLSX can omit empty rows at the end of a merged region from sheetData.
    let written = records;
    let width = (width + MAX_MERGE_PADDING_COLUMNS).min(MAX_COLUMNS);
    let padding = MAX_OUTPUT
        .saturating_sub(used + out.len())
        .min(padding_left);
    merges.retain(|m| m.first.0 < written + padding && m.first.1 < width);
    if let Some(last) = merges.iter().map(|m| m.last.0).max() {
        records += (last + 1).saturating_sub(written).min(padding);
    }
    Ok(Worksheet {
        text: out,
        records,
        written,
        width,
        merges,
    })
}

/// An OpenDocument spreadsheet `content.xml` as comma separated records: its cells as shown, or
/// the values of those that store one apart from what they show when `stored` is set.
fn ods(xml: &[u8], cancel: Option<&AtomicBool>, stored: bool) -> Result<String> {
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
    // The bytes of the row so far, and of every sheet so far.
    let (mut row_bytes, mut used) = (0usize, 0usize);
    let mut cell = String::new();
    let mut value: Option<String> = None;
    let mut repeat = 1usize;
    let mut rows_repeat = 1usize;
    // Empty rows not yet written: only those with a row after them in the same sheet are, so
    // that the unused rows a sheet records at its end are not part of it.
    let mut blank_rows = 0usize;
    let mut in_cell = false;
    let mut buf = Vec::new();
    let repeated = |e: &quick_xml::events::BytesStart<'_>, name: &str| -> usize {
        attribute(e, name)
            .and_then(|r| r.trim().parse().ok())
            .unwrap_or(1usize)
            .max(1)
    };
    for read in 0.. {
        if read % CHECK_EVERY == 0 {
            check(cancel)?;
        }
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
                        blank_rows = 0;
                    }
                    (Some(ODF_TABLE), "table-row") => {
                        cells.clear();
                        row_bytes = 0;
                        rows_repeat = repeated(e, "number-rows-repeated");
                    }
                    (Some(ODF_TABLE), "table-cell" | "covered-table-cell") => {
                        cell.clear();
                        value = stored
                            .then(|| {
                                ["value", "date-value", "time-value", "boolean-value"]
                                    .iter()
                                    .find_map(|name| attribute(e, name))
                            })
                            .flatten();
                        in_cell = true;
                        // A run of equal cells is recorded once; a long trailing run of empty
                        // ones is how a sheet records its unused width.
                        repeat = repeated(e, "number-columns-repeated");
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(e)) => {
                if in_cell {
                    cell.push_str(&e.xml10_content());
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(e)) => {
                if in_cell {
                    cell.push_str(&reference(&e));
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let (ns, local) = named(&reader, e.name());
                match (ns.as_deref(), local.as_str()) {
                    (Some(ODF_TEXT), "p") if in_cell && !cell.ends_with(' ') => cell.push(' '),
                    (Some(ODF_TABLE), "table-cell" | "covered-table-cell") => {
                        in_cell = false;
                        let text = match value.take() {
                            Some(v) => v.trim().to_string(),
                            None => cell.trim().to_string(),
                        };
                        let mut copies = repeat.min(MAX_COLUMNS.saturating_sub(cells.len()));
                        if !text.is_empty() {
                            let field = text.len() + 1;
                            copies =
                                copies.min(MAX_OUTPUT.saturating_sub(used + row_bytes) / field + 1);
                            row_bytes += copies * field;
                        }
                        cells.extend(std::iter::repeat_n(text, copies));
                        repeat = 1;
                    }
                    (Some(ODF_TABLE), "table-row") => {
                        let line = record(&trim_record(std::mem::take(&mut cells)));
                        let Some(s) = sheets.last_mut() else { continue };
                        if line == "\n" {
                            blank_rows = blank_rows.saturating_add(rows_repeat);
                        } else {
                            let gap = copies_that_fit("\n", used, blank_rows);
                            s.push_str(&"\n".repeat(gap));
                            used += gap;
                            blank_rows = 0;
                            let copies = copies_that_fit(&line, used, rows_repeat);
                            s.push_str(&line.repeat(copies));
                            used += copies * line.len();
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("{e}"),
        }
        if used > MAX_OUTPUT {
            break;
        }
    }
    for (i, body) in sheets.iter().enumerate() {
        // Sheets a spreadsheet keeps but never filled in say nothing.
        if body.trim().is_empty() {
            continue;
        }
        if tables > 1 {
            out.push_str(&sheet_heading(&names[i]));
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
    let bytes = read_whole(path)?;
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

/// The header, section table and symbols of an ELF file, as lines. Addresses and sizes are
/// written in full so that two builds of the same program line up line by line, and the symbols
/// of each symbol table are sorted by name, so that a symbol that moved is one changed line.
fn elf(path: &Path) -> Result<String> {
    // A program or a core file can be very large, so only its headers, the table of its
    // sections, the program's interpreter and its symbol and string tables are read, each at
    // most `MAX_PART`.
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let header = piece(&mut file, 0, 64, len)?;
    if header.len() < 64 || header[..4] != *b"\x7fELF" {
        bail!("not an ELF file");
    }
    let wide = match header[4] {
        1 => false,
        2 => true,
        other => bail!("unknown ELF class {other}"),
    };
    let little = match header[5] {
        1 => true,
        2 => false,
        other => bail!("unknown ELF byte order {other}"),
    };
    let (abi, abi_version) = (header[7], header[8]);
    let number = |bytes: &[u8]| -> u64 {
        let mut value = 0u64;
        for (i, b) in bytes.iter().enumerate() {
            let shift = if little { i } else { bytes.len() - 1 - i } * 8;
            value |= (*b as u64) << shift;
        }
        value
    };
    let (shoff_o, shentsize_o, shnum_o) = if wide { (40, 58, 60) } else { (32, 46, 48) };
    let shoff = number(&header[shoff_o..shoff_o + if wide { 8 } else { 4 }]);
    let size = number(&header[shentsize_o..shentsize_o + 2]) as usize;
    let count = number(&header[shnum_o..shnum_o + 2]) as usize;
    let table = piece(
        &mut file,
        shoff,
        (count.min(MAX_SECTIONS) * size) as u64,
        len,
    )?;
    let pieces = [(0u64, header), (shoff, table)];
    let at = |offset: usize, size: usize| -> u64 {
        let found = pieces.iter().find_map(|(start, bytes)| {
            let from = (offset as u64).checked_sub(*start)? as usize;
            bytes.get(from..from.checked_add(size)?)
        });
        found.map_or(0, number)
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
    let (entry_o, phoff_o) = if wide { (24, 32) } else { (24, 28) };
    let entry = addr(entry_o);
    let phoff = addr(phoff_o);
    let (ehsize_o, phentsize_o, phnum_o, shstrndx_o) = if wide {
        (52, 54, 56, 62)
    } else {
        (40, 42, 44, 50)
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
    out.push_str(&format!("ABI: {} version {}\n", abi_name(abi), abi_version));
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
    out.push_str(&format!(
        "Section headers: {count} of {size} bytes at 0x{shoff:x}\n"
    ));

    // The program headers: the segments the program is loaded as.
    let (ph_count, ph_size) = (half(phnum_o) as usize, half(phentsize_o) as usize);
    let ph_wanted = if wide { 56 } else { 32 };
    if ph_count > 0 && ph_size >= ph_wanted {
        let headers = piece(
            &mut file,
            phoff,
            (ph_count.min(MAX_SECTIONS) * ph_size) as u64,
            len,
        )?;
        out.push_str("\nProgram headers:\n");
        for (i, h) in headers.chunks_exact(ph_size).enumerate() {
            let read = |o: usize, n: usize| number(&h[o..o + n]);
            let (kind, flags, offset, vaddr, paddr, filesz, memsz, align) = if wide {
                (
                    read(0, 4),
                    read(4, 4),
                    read(8, 8),
                    read(16, 8),
                    read(24, 8),
                    read(32, 8),
                    read(40, 8),
                    read(48, 8),
                )
            } else {
                (
                    read(0, 4),
                    read(24, 4),
                    read(4, 4),
                    read(8, 4),
                    read(12, 4),
                    read(16, 4),
                    read(20, 4),
                    read(28, 4),
                )
            };
            out.push_str(&format!(
                "[{i:3}] {:<14} off 0x{offset:08x} vaddr 0x{vaddr:016x} paddr 0x{paddr:016x} filesz {filesz:>10} memsz {memsz:>10} align {align:>6} flags {}\n",
                segment_type(kind),
                segment_flags(flags),
            ));
            // The program that loads a dynamically linked one.
            if kind == 3 {
                let path = piece(&mut file, offset, filesz.min(4096), len)?;
                out.push_str(&format!("      interpreter {}\n", c_string(&path, 0)));
            }
        }
    }

    // The names of the sections are in the section the header points at.
    let names_index = half(shstrndx_o) as usize;
    let names_at = (names_index < count && size >= 16)
        .then(|| {
            let head = (shoff as usize).checked_add(names_index.checked_mul(size)?)?;
            let (offset_o, size_o) = if wide { (24, 32) } else { (16, 20) };
            let start = addr(head.checked_add(offset_o)?);
            let length = addr(head.checked_add(size_o)?);
            (start.checked_add(length)? <= len).then_some((start, length))
        })
        .flatten();
    let names = match names_at {
        Some((start, length)) => piece(&mut file, start, length, len)?,
        None => Vec::new(),
    };
    let name_at = |offset: usize| c_string(&names, offset);

    out.push_str("\nSections:\n");
    let mut sections = Vec::new();
    for i in 0..count.min(MAX_SECTIONS) {
        let Some(head) = (shoff as usize).checked_add(i * size) else {
            break;
        };
        // The table was read as far as the file goes.
        if pieces[1].1.len() < (i + 1) * size {
            break;
        }
        let (type_o, flags_o, addr_o, offset_o, size_o, link_o, info_o, align_o, entsize_o) =
            if wide {
                (4, 8, 16, 24, 32, 40, 44, 48, 56)
            } else {
                (4, 8, 12, 16, 20, 24, 28, 32, 36)
            };
        let section = Section {
            name: name_at(word(head) as usize),
            kind: word(head + type_o),
            offset: addr(head + offset_o),
            size: addr(head + size_o),
            link: word(head + link_o) as usize,
        };
        out.push_str(&format!(
            "[{i:3}] {:<20} {:<14} addr 0x{:016x} off 0x{:08x} size {:>10} align {:>5} entsize {:>4} link {} info {} flags {}\n",
            section.name,
            section_type(section.kind),
            addr(head + addr_o),
            section.offset,
            section.size,
            addr(head + align_o),
            addr(head + entsize_o),
            section.link,
            word(head + info_o),
            section_flags(addr(head + flags_o)),
        ));
        sections.push(section);
    }

    // The symbols of each symbol table, the full one and the one for dynamic linking.
    let entry_size = if wide { 24 } else { 16 };
    // All the symbol tables together are read up to this much, so that a file of many tables
    // cannot take long to read.
    let mut budget = 4 * MAX_PART;
    for (table_index, table) in sections.iter().enumerate() {
        if !matches!(table.kind, 2 | 11) {
            continue;
        }
        // The text is cut to `MAX_OUTPUT` in the end; nothing past it is worth making.
        if out.len() > MAX_OUTPUT || budget == 0 {
            break;
        }
        let entries = piece(&mut file, table.offset, table.size.min(budget), len)?;
        budget -= entries.len() as u64;
        // The section indexes that do not fit the symbols' 16 bits, in a table of their own.
        let wide_indexes = match sections
            .iter()
            .find(|s| s.kind == 18 && s.link == table_index)
        {
            Some(s) => piece(&mut file, s.offset, s.size, len)?,
            None => Vec::new(),
        };
        let strings = match sections.get(table.link).filter(|s| s.kind == 3) {
            Some(s) => piece(&mut file, s.offset, s.size, len)?,
            None => Vec::new(),
        };
        let mut symbols = Vec::new();
        let (mut mapping, mut unlisted) = (0usize, 0usize);
        // The first entry of a table is always empty.
        for (n, entry) in entries.chunks_exact(entry_size).enumerate().skip(1) {
            let read = |o: usize, n: usize| number(&entry[o..o + n]);
            let (value, size, info, other, index) = if wide {
                (read(8, 8), read(16, 8), entry[4], entry[5], read(6, 2))
            } else {
                (read(4, 4), read(8, 4), entry[12], entry[13], read(14, 2))
            };
            let name = c_string(&strings, read(0, 4) as usize);
            // A section's own symbol has no name; the section table already lists it.
            if name.is_empty() {
                continue;
            }
            // The marks ARM and RISC-V assemblers put where code or data starts (`$x`, `$d`,
            // `$t.n`, `$xrv32i2p1…`) say nothing of the program and change with every build.
            if info == 0 && name.starts_with('$') {
                mapping += 1;
                continue;
            }
            if symbols.len() == MAX_SYMBOLS {
                unlisted += 1;
                continue;
            }
            let named = |i: u64| {
                sections
                    .get(i as usize)
                    .map_or_else(|| format!("[{i}]"), |s| s.name.clone())
            };
            let section = match index {
                0 => "UND".to_string(),
                0xfff1 => "ABS".to_string(),
                0xfff2 => "COMMON".to_string(),
                // The index is in the table of wide indexes, at the symbol's place.
                0xffff => wide_indexes
                    .get(n * 4..n * 4 + 4)
                    .map_or_else(|| "XINDEX".to_string(), |i| named(number(i))),
                i @ 0xff00.. => format!("reserved 0x{i:x}"),
                i => named(i),
            };
            symbols.push((name, value, size, info, other, section));
        }
        symbols.sort();
        out.push_str(&format!("\nSymbols in {}: {}\n", table.name, symbols.len()));
        for (name, value, size, info, other, section) in &symbols {
            if out.len() > MAX_OUTPUT {
                break;
            }
            out.push_str(&format!(
                "{name:<32} addr 0x{value:016x} size {size:>8} {:<7} {:<6} {:<9} {section}\n",
                symbol_type(info & 0xf),
                symbol_binding(info >> 4),
                symbol_visibility(other & 0x3),
            ));
        }
        if mapping > 0 {
            out.push_str(&format!("({mapping} mapping symbols left out)\n"));
        }
        if unlisted > 0 {
            out.push_str(&format!("... {unlisted} more symbols not listed\n"));
        }
    }
    Ok(out)
}

/// What the symbols of an ELF file need of a section.
struct Section {
    name: String,
    kind: u64,
    offset: u64,
    size: u64,
    link: usize,
}

/// The text from `offset` in a table of strings, to the zero byte that ends it.
fn c_string(table: &[u8], offset: usize) -> String {
    let rest = table.get(offset..).unwrap_or_default();
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

fn segment_type(value: u64) -> String {
    match value {
        0 => "NULL".into(),
        1 => "LOAD".into(),
        2 => "DYNAMIC".into(),
        3 => "INTERP".into(),
        4 => "NOTE".into(),
        5 => "SHLIB".into(),
        6 => "PHDR".into(),
        7 => "TLS".into(),
        0x6474e550 => "GNU_EH_FRAME".into(),
        0x6474e551 => "GNU_STACK".into(),
        0x6474e552 => "GNU_RELRO".into(),
        0x6474e553 => "GNU_PROPERTY".into(),
        0x70000001 => "ARM_EXIDX".into(),
        0x70000003 => "RISCV_ATTRIBUTES".into(),
        other => format!("0x{other:x}"),
    }
}

/// The flags of a segment as the letters `readelf` uses: read, write, execute.
fn segment_flags(value: u64) -> String {
    [(4, 'R'), (2, 'W'), (1, 'E')]
        .iter()
        .map(|&(bit, letter)| if value & bit != 0 { letter } else { '-' })
        .collect()
}

fn symbol_type(value: u8) -> String {
    match value {
        0 => "NOTYPE".into(),
        1 => "OBJECT".into(),
        2 => "FUNC".into(),
        3 => "SECTION".into(),
        4 => "FILE".into(),
        5 => "COMMON".into(),
        6 => "TLS".into(),
        10 => "IFUNC".into(),
        other => format!("type{other}"),
    }
}

fn symbol_binding(value: u8) -> String {
    match value {
        0 => "LOCAL".into(),
        1 => "GLOBAL".into(),
        2 => "WEAK".into(),
        10 => "UNIQUE".into(),
        other => format!("bind{other}"),
    }
}

fn symbol_visibility(value: u8) -> &'static str {
    match value {
        1 => "INTERNAL",
        2 => "HIDDEN",
        3 => "PROTECTED",
        _ => "DEFAULT",
    }
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
    use crate::i18n::trf;

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
        let text = Kind::Document.read(&path, None).unwrap();
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
        let text = Kind::Document.read(&path, None).unwrap();
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
        assert_eq!(Kind::Document.read(&path, None).unwrap(), DOCX_TEXT);
    }

    const DOCX_TEXT: &str = "First line\nSecond\tafter a tab\na1\tb1\n";

    /// A Word document whose body is `body`.
    fn docx_with(dir: &Path, body: &str) -> std::path::PathBuf {
        let xml = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="{W}" xmlns:mc="{MARKUP_COMPATIBILITY}"><w:body>{body}</w:body></w:document>"#
        );
        zipped(dir, "doc.docx", &[("word/document.xml", &xml)])
    }

    /// Characters a document cannot write as they are, such as `&` and `<`, are written as
    /// references, which are read as the characters they stand for.
    #[test]
    fn the_references_in_a_document_are_read_as_their_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = docx_with(
            dir.path(),
            "<w:p><w:r><w:t>AT&amp;T &lt;x&gt; &#65;&#x42; &quot;q&apos;</w:t></w:r></w:p>",
        );
        assert_eq!(
            Kind::Document.read(&path, None).unwrap(),
            "AT&T <x> AB \"q'\n"
        );
        let content = concat!(
            r#"<?xml version="1.0"?><office:document-content "#,
            r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
            r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">"#,
            r#"<text:p>a &amp; b &#8212; c</text:p></office:document-content>"#
        );
        let path = zipped(dir.path(), "a.odt", &[("content.xml", content)]);
        assert_eq!(Kind::Document.read(&path, None).unwrap(), "a & b — c\n");
    }

    /// Only the words of a document's runs are its text: not its tab stops, not the words it
    /// shows as deleted, not the codes of its fields, and not the second copy of a text box
    /// kept for programs that cannot show the first.
    #[test]
    fn a_word_document_is_read_as_the_words_it_shows() {
        let dir = tempfile::tempdir().unwrap();
        let body = concat!(
            r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr>"#,
            r#"<w:r><w:t>kept</w:t></w:r>"#,
            r#"<w:del><w:r><w:delText>deleted</w:delText></w:r></w:del>"#,
            r#"<w:r><w:instrText> PAGE </w:instrText></w:r>"#,
            r#"<w:r><w:tab/><w:t>after</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps">"#,
            r#"<w:t>box</w:t></mc:Choice><mc:Fallback><w:t>box</w:t></mc:Fallback>"#,
            r#"</mc:AlternateContent></w:r></w:p>"#
        );
        let path = docx_with(dir.path(), body);
        assert_eq!(
            Kind::Document.read(&path, None).unwrap(),
            "kept\tafter\nbox\n"
        );
    }

    /// The words of an inline string may be split over several runs, and a shared string may
    /// carry the reading of its words, which is not part of the value.
    #[test]
    fn a_workbook_reads_every_run_of_a_string_and_not_its_reading() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<row r="1"><c r="A1" t="inlineStr"><is><r><t>ab</t></r><r><t>cd</t></r></is></c>"#,
            r#"<c r="B1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>"#
        );
        let parts = workbook(
            &[("Only", rows)],
            &[
                r#"漢字</t><rPh sb="0" eb="2"><t>かんじ</t></rPh><t>"#,
                "AT&amp;T",
            ],
        );
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "runs.xlsx", &borrowed);
        assert_eq!(
            Kind::Spreadsheet.read(&path, None).unwrap(),
            "abcd,漢字,AT&T\n"
        );
    }

    /// Rows with nothing in them are not written, but the rows around them stay apart.
    #[test]
    fn a_workbook_keeps_the_empty_rows_between_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
            r#"<row r="4"><c r="A4"><v>4</v></c></row>"#,
            r#"<row r="1048576"><c r="A1048576"><v>last</v></c></row>"#
        );
        let parts = workbook(&[("Only", rows)], &[]);
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "gaps.xlsx", &borrowed);
        let text = Kind::Spreadsheet.read(&path, None).unwrap();
        assert!(text.starts_with("1\n\n\n4\n"), "{:?}", &text[..20]);
        assert_eq!(text.lines().count(), 1_048_576);
        assert!(text.ends_with("\nlast\n"));
    }

    /// An OpenDocument spreadsheet `content.xml` with the rows `rows` in one sheet.
    fn ods_with(dir: &Path, rows: &str) -> std::path::PathBuf {
        let content = format!(
            "{}{rows}{}",
            concat!(
                r#"<?xml version="1.0"?><office:document-content "#,
                r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
                r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
                r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">"#,
                r#"<table:table table:name="S">"#
            ),
            "</table:table></office:document-content>"
        );
        zipped(dir, "rows.ods", &[("content.xml", &content)])
    }

    /// A run of equal rows is recorded once, as the unused rows at the end of a sheet are.
    #[test]
    fn an_opendocument_spreadsheet_repeats_the_rows_it_records_once() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<table:table-row table:number-rows-repeated="2">"#,
            r#"<table:table-cell><text:p>same</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row table:number-rows-repeated="2"><table:table-cell/></table:table-row>"#,
            r#"<table:table-row><table:table-cell><text:p>a &amp; b</text:p></table:table-cell>"#,
            r#"</table:table-row>"#,
            r#"<table:table-row table:number-rows-repeated="1048000">"#,
            r#"<table:table-cell table:number-columns-repeated="1024"/></table:table-row>"#
        );
        let path = ods_with(dir.path(), rows);
        assert_eq!(
            Kind::Spreadsheet.read(&path, None).unwrap(),
            "same\nsame\n\n\na & b\n"
        );
    }

    #[test]
    fn an_opendocument_spreadsheet_read_the_other_way_has_its_stored_values() {
        let dir = tempfile::tempdir().unwrap();
        let rows = concat!(
            r#"<table:table-row><table:table-cell><text:p>share</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>due</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row><table:table-cell office:value-type="percentage" "#,
            r#"office:value="0.25"><text:p>25%</text:p></table:table-cell>"#,
            r#"<table:table-cell office:value-type="date" office:date-value="2026-09-30">"#,
            r#"<text:p>30/09/26</text:p></table:table-cell></table:table-row>"#
        );
        let path = ods_with(dir.path(), rows);
        assert_eq!(
            Kind::Spreadsheet.read(&path, None).unwrap(),
            "share,due\n25%,30/09/26\n"
        );
        let other = other_cells(&path, None).unwrap();
        assert!(!other.shown);
        assert_eq!(other.text, "share,due\n0.25,2026-09-30\n");
    }

    #[test]
    fn tidy_text_says_which_line_each_mark_is_on() {
        let text = "\n\na  \n\n\n\nb\nc\n".to_string();
        let marks = [0, 3, 8, 11, 13];
        let (tidy, at) = tidy_marked(text.clone(), &marks);
        assert_eq!(tidy, "a\n\nb\nc\n");
        assert_eq!(tidy, super::tidy(text));
        assert_eq!(at, [0, 0, 2, 3, 4]);
    }

    /// A small file can say a long value is repeated across every column of a million rows;
    /// the text it is read as stays within the limit all the same.
    #[test]
    fn a_long_value_repeated_everywhere_is_read_only_as_far_as_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let long = "x".repeat(4096);
        let rows = format!(
            r#"<table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="100000"><text:p>{long}</text:p></table:table-cell></table:table-row>"#
        );
        let path = ods_with(dir.path(), &rows);
        let text = Kind::Spreadsheet.read(&path, None).unwrap();
        assert!(text.len() <= MAX_OUTPUT + 1024, "{}", text.len());
        assert!(text.ends_with(&format!("{}\n", crate::i18n::tr("msg.extract_truncated"))));
    }

    /// A workbook of several sheets is compared as a table too: the table view shows each
    /// sheet, told apart by its heading, on a tab of its own.
    #[test]
    fn a_workbook_of_several_sheets_is_compared_as_a_table_of_sheets() {
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
        let opts = super::super::diff::DiffOptions::default();
        let doc = super::super::unpacker::load_doc(&path, &opts, None).unwrap();
        assert!(is_sheet_heading(&doc.lines[0]), "{:?}", doc.lines);
        assert_eq!(
            super::super::table::delimiter_of_doc(&doc, &opts),
            Some(',')
        );
    }

    #[test]
    fn a_cancelled_read_stops() {
        let dir = tempfile::tempdir().unwrap();
        let path = zipped(dir.path(), "a.docx", &[("word/document.xml", DOCX)]);
        let cancel = AtomicBool::new(true);
        assert!(Kind::Document.read(&path, Some(&cancel)).is_err());
    }

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
        let text = Kind::Spreadsheet.read(&path, None).unwrap();
        // The gap at B2 is kept so that the columns line up, and a comma in a value is quoted.
        assert_eq!(text, "name,note\n12.5,,\"a, comma\"\nTRUE\n");
        // Read as comma separated records, so the table compare shows it as a table.
        let opts = super::super::diff::DiffOptions::default();
        let doc = super::super::unpacker::load_doc(&path, &opts, None).unwrap();
        assert_eq!(
            super::super::table::delimiter_of_doc(&doc, &opts),
            Some(',')
        );
    }

    #[test]
    fn xlsm_merges_keep_sparse_rows_and_use_record_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let first = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>line
break</t></is></c></row>"#;
        let second = r#"<row r="2"><c r="B2"><v>1</v></c><c r="D2"><v>99</v></c></row>"#;
        let mut parts = workbook(&[("First", first), ("Second", second)], &[]);
        let sheet = &mut parts
            .iter_mut()
            .find(|(part, _)| part == "xl/worksheets/sheet2.xml")
            .unwrap()
            .1;
        *sheet = sheet.replace(
            "</worksheet>",
            r#"<mergeCells><mergeCell ref="B2:D4"/></mergeCells></worksheet>"#,
        );
        let borrowed: Vec<_> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "book.xlsm", &borrowed);
        let (text, merges) = Kind::Spreadsheet.read_with_merges(&path, None).unwrap();
        let table = super::super::table::Table::parse(text.clone(), ',').unwrap();
        assert_eq!(table.records[1].cells, ["line\nbreak"]);
        assert_eq!(table.records[5].cells, ["", "1", "", "99"]);
        assert_eq!(table.records.len(), 7);
        let table = table.with_spreadsheet_merges(&merges);
        assert_eq!(table.source, text);
        assert_eq!(table.records[5].cells, ["", "1"]);
        assert!(table.records[7].cells.is_empty());
        assert_eq!(
            merges.covering((5, 7), 3),
            Some(TableMerge::new(4, (1, 1), (3, 3)))
        );
    }

    #[test]
    fn a_long_merge_does_not_force_scanning_expired_ranges() {
        let rows = 20_000;
        let long = TableMerge::new(0, (0, 0), (rows - 1, 0));
        let short = |row| TableMerge::new(0, (row, 1), (row, 2));
        let merges = TableMerges::new(
            std::iter::once(long).chain((0..rows).map(short)).collect(),
            Vec::new(),
        );
        for row in 0..rows {
            assert_eq!(merges.covering((row, row), 2), Some(short(row)));
            assert_eq!(merges.covering((row, row), 3), None);
        }
        assert_eq!(merges.covering((0, rows - 1), 0), Some(long));
        assert_eq!(merges.covering((0, rows - 1), 2), None);
    }

    #[test]
    fn a_whole_sheet_merge_has_bounded_table_extents() {
        let dir = tempfile::tempdir().unwrap();
        let mut parts = workbook(
            &[("Only", r#"<row r="1"><c r="A1"><v>1</v></c></row>"#)],
            &[],
        );
        let xml = &mut parts.last_mut().unwrap().1;
        *xml = xml.replace(
            "</worksheet>",
            r#"<mergeCells><mergeCell ref="A1:XFD1048576"/></mergeCells></worksheet>"#,
        );
        let unreachable = xml.replace("A1:XFD1048576", "A1048575:A1048576");
        let distant = sheet(
            unreachable.as_bytes(),
            &[],
            None,
            0,
            MAX_MERGE_PADDING_ROWS,
            None,
        )
        .unwrap();
        assert_eq!(distant.records, distant.written);
        assert!(distant.merges.is_empty());

        let borrowed: Vec<_> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "whole-sheet.xlsx", &borrowed);
        let (text, merges) = Kind::Spreadsheet.read_with_merges(&path, None).unwrap();
        assert_eq!(text, "1\n");
        assert_eq!(
            merges.padding(),
            std::slice::from_ref(&(1..1 + MAX_MERGE_PADDING_ROWS))
        );
        assert_eq!(merges.width(None), 1 + MAX_MERGE_PADDING_COLUMNS);
        assert_eq!(
            merges.covering((0, 0), 0).unwrap().last,
            (MAX_XLSX_ROWS - 1, MAX_COLUMNS - 1)
        );
        let beyond = MAX_MERGE_PADDING_ROWS + 1;
        assert!(merges.covering((beyond, beyond), 0).is_none());
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
        let text = Kind::Spreadsheet.read(&path, None).unwrap();
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
        assert_eq!(Kind::Spreadsheet.read(&path, None).unwrap(), "x,y,y,y\n");
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
        assert_eq!(Kind::Spreadsheet.read(&path, None).unwrap(), "a,,,,c\n");
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
        assert_eq!(Kind::Spreadsheet.read(&path, None).unwrap(), "1,,3\n");
    }

    /// A relationship may name its part from the top of the package, as some programs other
    /// than Excel write them.
    #[test]
    fn a_workbook_whose_sheets_are_named_from_the_top_of_the_package_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut parts = workbook(
            &[("Only", r#"<row r="1"><c r="A1"><v>7</v></c></row>"#)],
            &[],
        );
        for (name, body) in &mut parts {
            if name == "xl/_rels/workbook.xml.rels" {
                *body = body.replace("Target=\"worksheets/", "Target=\"/xl/worksheets/");
            }
        }
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "absolute.xlsx", &borrowed);
        assert_eq!(Kind::Spreadsheet.read(&path, None).unwrap(), "7\n");
    }

    /// The words of a workbook's cells are kept in a part of their own. If that part is there
    /// but cannot be read, going on without it would show every one of those cells as empty.
    #[test]
    fn a_workbook_whose_words_cannot_be_read_is_reported_rather_than_shown_without_them() {
        let dir = tempfile::tempdir().unwrap();
        let rows = r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#;
        let parts = workbook(&[("Only", rows)], &["unmistakable"]);
        let path = dir.path().join("damaged.xlsx");
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (entry, body) in &parts {
            zip.start_file(entry.as_str(), stored).unwrap();
            std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
        }
        let mut bytes = zip.finish().unwrap().into_inner();
        // One letter of the stored words changed, so the part no longer matches its checksum.
        let at = bytes
            .windows(12)
            .position(|w| w == b"unmistakable")
            .unwrap();
        bytes[at] = b'U';
        std::fs::write(&path, bytes).unwrap();
        assert!(Kind::Spreadsheet.read(&path, None).is_err());
    }

    /// A workbook none of whose sheets can be found is reported, rather than compared as if
    /// it were empty.
    #[test]
    fn a_workbook_whose_sheets_are_all_missing_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let parts = workbook(&[("Only", "")], &[]);
        let borrowed: Vec<(&str, &str)> = parts
            .iter()
            .filter(|(a, _)| !a.starts_with("xl/worksheets/"))
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let path = zipped(dir.path(), "missing.xlsx", &borrowed);
        assert!(Kind::Spreadsheet.read(&path, None).is_err());
    }

    #[test]
    fn a_relationship_target_is_named_from_its_folder_or_from_the_top() {
        assert_eq!(
            part_name("xl", "worksheets/sheet1.xml").as_deref(),
            Some("xl/worksheets/sheet1.xml")
        );
        assert_eq!(
            part_name("xl", "/xl/worksheets/sheet1.xml").as_deref(),
            Some("xl/worksheets/sheet1.xml")
        );
        assert_eq!(
            part_name("xl", "./worksheets/../worksheets/sheet1.xml").as_deref(),
            Some("xl/worksheets/sheet1.xml")
        );
        assert_eq!(
            part_name("xl", "../docProps/app.xml").as_deref(),
            Some("docProps/app.xml")
        );
        assert_eq!(part_name("xl", "../../outside.xml"), None);
    }

    /// A PDF is read whole, so one far larger than any document is refused before it is read.
    #[test]
    fn a_pdf_too_large_to_read_whole_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.pdf");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        drop(file);
        let e = Kind::Pdf.read(&path, None).unwrap_err();
        assert!(format!("{e:#}").contains("larger than"), "{e:#}");
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
        let text = Kind::Elf.read(&path, None).unwrap();
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
        let text = Kind::Rtf.read(&path, None).unwrap();
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
        let text = Kind::Elf.read(&path, None).unwrap();
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

    /// A 64-bit little-endian program with one segment and a symbol table of the given
    /// symbols: (name, value, size, info, other, section index).
    fn elf_with_symbols(symbols: &[(&str, u64, u64, u8, u8, u16)]) -> Vec<u8> {
        let names = b"\0.text\0.shstrtab\0.symtab\0.strtab\0";
        let mut strings = vec![0u8];
        let mut offsets = Vec::new();
        for (name, ..) in symbols {
            offsets.push(strings.len() as u32);
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        let (phoff, shoff) = (64usize, 64 + 56);
        let names_at = shoff + 5 * 64;
        let symtab_at = names_at + names.len();
        let symtab_size = (symbols.len() + 1) * 24;
        let strtab_at = symtab_at + symtab_size;
        let mut bytes = vec![0u8; strtab_at + strings.len()];
        bytes[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x00");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        bytes[40..48].copy_from_slice(&(shoff as u64).to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&64u16.to_le_bytes());
        bytes[60..62].copy_from_slice(&5u16.to_le_bytes());
        bytes[62..64].copy_from_slice(&2u16.to_le_bytes());
        // One loadable segment, read and execute.
        let ph = &mut bytes[phoff..phoff + 56];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes());
        ph[4..8].copy_from_slice(&5u32.to_le_bytes());
        ph[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        ph[16..24].copy_from_slice(&0x401000u64.to_le_bytes());
        ph[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        ph[32..40].copy_from_slice(&0x20u64.to_le_bytes());
        ph[40..48].copy_from_slice(&0x30u64.to_le_bytes());
        ph[48..56].copy_from_slice(&0x1000u64.to_le_bytes());
        let mut put = |index: usize, name: u32, kind: u32, off: usize, size: usize, link: u32| {
            let at = shoff + index * 64;
            bytes[at..at + 4].copy_from_slice(&name.to_le_bytes());
            bytes[at + 4..at + 8].copy_from_slice(&kind.to_le_bytes());
            bytes[at + 24..at + 32].copy_from_slice(&(off as u64).to_le_bytes());
            bytes[at + 32..at + 40].copy_from_slice(&(size as u64).to_le_bytes());
            bytes[at + 40..at + 44].copy_from_slice(&link.to_le_bytes());
        };
        put(1, 1, 1, 0x1000, 0, 0); // .text
        put(2, 7, 3, names_at, names.len(), 0); // .shstrtab
        put(3, 17, 2, symtab_at, symtab_size, 4); // .symtab, names in section 4
        put(4, 25, 3, strtab_at, strings.len(), 0); // .strtab
        bytes[names_at..symtab_at].copy_from_slice(names);
        for (i, &(_, value, size, info, other, index)) in symbols.iter().enumerate() {
            let at = symtab_at + (i + 1) * 24;
            bytes[at..at + 4].copy_from_slice(&offsets[i].to_le_bytes());
            bytes[at + 4] = info;
            bytes[at + 5] = other;
            bytes[at + 6..at + 8].copy_from_slice(&index.to_le_bytes());
            bytes[at + 8..at + 16].copy_from_slice(&value.to_le_bytes());
            bytes[at + 16..at + 24].copy_from_slice(&size.to_le_bytes());
        }
        bytes[strtab_at..].copy_from_slice(&strings);
        bytes
    }

    #[test]
    fn segment_flags_are_always_three_letters() {
        assert_eq!(segment_flags(0), "---");
        assert_eq!(segment_flags(2), "-W-");
        assert_eq!(segment_flags(5), "R-E");
        assert_eq!(segment_flags(6), "RW-");
        assert_eq!(segment_flags(7), "RWE");
    }

    #[test]
    fn an_elf_program_lists_its_segments_and_its_symbols_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("program");
        let bytes = elf_with_symbols(&[
            ("main", 0x401010, 16, 0x12, 0, 1),        // FUNC GLOBAL in .text
            ("$x", 0x401000, 0, 0, 0, 1),              // a mapping symbol, left out
            ("counter", 0x402000, 4, 0x01, 2, 0xfff1), // OBJECT LOCAL HIDDEN, ABS
            ("abort", 0, 0, 0x22, 0, 0),               // FUNC WEAK, undefined
            ("tune", 8, 0, 0x10, 0, 0xff1f),           // a processor's own reserved section
        ]);
        std::fs::write(&path, bytes).unwrap();
        let text = Kind::Elf.read(&path, None).unwrap();
        assert!(
            text.contains(
                "[  0] LOAD           off 0x00001000 vaddr 0x0000000000401000 paddr 0x0000000000401000 filesz         32 memsz         48 align   4096 flags R-E\n"
            ),
            "{text}"
        );
        let symbols: Vec<&str> = text
            .split("Symbols in .symtab: 4\n")
            .nth(1)
            .expect(&text)
            .lines()
            .collect();
        let words = |line: &str| line.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            symbols.iter().map(|l| words(l)).collect::<Vec<_>>(),
            [
                "abort addr 0x0000000000000000 size 0 FUNC WEAK DEFAULT UND",
                "counter addr 0x0000000000402000 size 4 OBJECT LOCAL HIDDEN ABS",
                "main addr 0x0000000000401010 size 16 FUNC GLOBAL DEFAULT .text",
                "tune addr 0x0000000000000008 size 0 NOTYPE GLOBAL DEFAULT reserved 0xff1f",
                "(1 mapping symbols left out)",
            ]
        );
    }

    #[test]
    fn a_file_that_is_not_what_its_name_says_is_reported_rather_than_shown_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.docx");
        std::fs::write(&path, b"not a zip at all").unwrap();
        let e = Kind::Document.read(&path, None).unwrap_err();
        assert!(format!("{e:#}").contains("broken.docx"), "{e:#}");
        // A plain file keeps its own name and is compared as it is.
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, b"hello").unwrap();
        assert_eq!(Kind::of(&plain), None);
    }

    /// A notebook built from cells, each `(cell_type, source lines, outputs)`.
    fn notebook_file(
        dir: &Path,
        name: &str,
        cells: &[(&str, &[&str], &[&str])],
    ) -> std::path::PathBuf {
        let cells: Vec<_> = cells
            .iter()
            .map(|(kind, source, outputs)| {
                serde_json::json!({
                    "cell_type": kind,
                    "source": source,
                    "execution_count": 1,
                    "metadata": {},
                    "outputs": outputs
                        .iter()
                        .map(|t| serde_json::json!({
                            "output_type": "stream",
                            "name": "stdout",
                            "text": [t],
                        }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "cells": cells,
            "metadata": {"kernelspec": {"name": "python3"}},
            "nbformat": 4,
            "nbformat_minor": 5,
        });
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn a_notebook_is_read_as_its_cells_without_their_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = notebook_file(
            dir.path(),
            "report.ipynb",
            &[
                ("markdown", &["# Title\n", "Some text."], &[]),
                ("code", &["import pandas as pd\n", "print(1)"], &["1\n"]),
                ("code", &["print(2)"], &["2\n"]),
                ("raw", &["verbatim"], &[]),
            ],
        );
        assert_eq!(Kind::of(&path), Some(Kind::Notebook));
        let text = Kind::Notebook.read(&path, None).unwrap();
        let (md, code, raw) = (
            notebook::header("label.notebook_markdown_cell"),
            notebook::header("label.notebook_code_cell"),
            notebook::header("label.notebook_raw_cell"),
        );
        assert_eq!(
            text,
            format!(
                "{md}\n# Title\nSome text.\n\n{code}\nimport pandas as pd\nprint(1)\n\n{code}\nprint(2)\n\n{raw}\nverbatim\n"
            )
        );
        // Outputs never show up, however they changed: the two notebooks below only differ in
        // their outputs and execution order, so they must read as the same text.
        let same_code = notebook_file(
            dir.path(),
            "rerun.ipynb",
            &[
                ("markdown", &["# Title\n", "Some text."], &[]),
                (
                    "code",
                    &["import pandas as pd\n", "print(1)"],
                    &["only output changed\n"],
                ),
                ("code", &["print(2)"], &[]),
                ("raw", &["verbatim"], &[]),
            ],
        );
        assert_eq!(Kind::Notebook.read(&same_code, None).unwrap(), text);
    }

    #[test]
    fn a_notebook_with_a_single_string_source_reads_the_same_as_an_array_of_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single_string.ipynb");
        let doc = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "a\nb\n",
                "outputs": [],
            }],
            "nbformat": 4,
            "nbformat_minor": 5,
        });
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let text = Kind::Notebook.read(&path, None).unwrap();
        assert_eq!(
            text,
            format!("{}\na\nb\n", notebook::header("label.notebook_code_cell"))
        );
    }

    #[test]
    fn a_code_cell_added_at_the_top_changes_no_other_cell_label() {
        let dir = tempfile::tempdir().unwrap();
        let before = notebook_file(dir.path(), "a.ipynb", &[("code", &["x = 1"], &[])]);
        let after = notebook_file(
            dir.path(),
            "b.ipynb",
            &[("code", &["import os"], &[]), ("code", &["x = 1"], &[])],
        );
        let before = Kind::Notebook.read(&before, None).unwrap();
        let after = Kind::Notebook.read(&after, None).unwrap();
        assert!(after.ends_with(&before), "{after:?} vs {before:?}");
    }

    #[test]
    fn an_nbformat_3_notebook_reads_its_worksheets_and_headings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.ipynb");
        let doc = serde_json::json!({
            "nbformat": 3,
            "worksheets": [{
                "cells": [
                    {"cell_type": "heading", "level": 2, "source": ["Results"]},
                    {"cell_type": "code", "input": ["print(1)"], "outputs": [{"text": ["1"]}]},
                    {"cell_type": "markdown", "source": "Done."},
                ],
            }],
        });
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let (md, code) = (
            notebook::header("label.notebook_markdown_cell"),
            notebook::header("label.notebook_code_cell"),
        );
        assert_eq!(
            Kind::Notebook.read(&path, None).unwrap(),
            format!("{md}\n## Results\n\n{code}\nprint(1)\n\n{md}\nDone.\n")
        );
    }

    #[test]
    fn a_notebook_with_a_bom_or_an_unknown_cell_type_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom.ipynb");
        let json = br#"{"cells":[{"cell_type":"widget","source":"w"}],"nbformat":4}"#;
        std::fs::write(&path, [b"\xEF\xBB\xBF".as_slice(), json].concat()).unwrap();
        assert_eq!(
            Kind::Notebook.read(&path, None).unwrap(),
            format!("{}\nw\n", notebook::header("label.notebook_raw_cell"))
        );
    }

    #[test]
    fn cells_past_the_limit_are_counted_but_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.ipynb");
        let n = notebook::MAX_CELLS + 5;
        let cells = vec![r#"{"cell_type":"raw","source":"x","outputs":[{"data":"big"}]}"#; n];
        std::fs::write(&path, format!(r#"{{"cells":[{}]}}"#, cells.join(","))).unwrap();
        let text = Kind::Notebook.read(&path, None).unwrap();
        let raw = notebook::header("label.notebook_raw_cell");
        assert_eq!(text.matches(&raw).count(), notebook::MAX_CELLS);
        assert!(text.ends_with(&format!(
            "{}\n",
            trf(
                "msg.notebook_truncated",
                &[
                    ("max", notebook::MAX_CELLS.to_string()),
                    ("n", n.to_string())
                ]
            )
        )));
    }

    #[test]
    fn the_report_notebook_sample_differs_only_in_its_source_not_its_outputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let left = Kind::Notebook
            .read(&root.join("left/docs/report.ipynb"), None)
            .unwrap();
        let right = Kind::Notebook
            .read(&root.join("right/docs/report.ipynb"), None)
            .unwrap();
        assert_ne!(left, right);
        // The second code cell has the same source on each side, but a different execution
        // count and output, to show outputs never affect the comparison.
        let unchanged_cell = "df = pd.read_csv('sales.csv')\nprint(df.sum())";
        assert!(left.contains(unchanged_cell) && right.contains(unchanged_cell));
        assert!(!left.contains("1042") && !right.contains("1042"));
        // The other two cells do differ: a line added to the third, and the last paragraph.
        assert!(!left.contains("print(df.std())") && right.contains("print(df.std())"));
        assert!(!left.contains("stronger than Q4") && right.contains("stronger than Q4"));
    }

    #[test]
    fn a_notebook_that_is_not_valid_json_or_has_no_cells_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.ipynb");
        std::fs::write(&broken, b"not json").unwrap();
        assert!(Kind::Notebook.read(&broken, None).is_err());
        let no_cells = dir.path().join("no_cells.ipynb");
        std::fs::write(&no_cells, br#"{"nbformat": 4}"#).unwrap();
        assert!(Kind::Notebook.read(&no_cells, None).is_err());
        // Opened for a compare, either one is still shown as the text it is, and can be edited.
        let opts = crate::core::diff::DiffOptions::default();
        for path in [&broken, &no_cells] {
            let doc = crate::core::unpacker::load_doc(path, &opts, None).unwrap();
            assert!(!doc.compare_only && !doc.read_only, "{}", path.display());
            assert_eq!(doc.text(), std::fs::read_to_string(path).unwrap());
        }
    }
}
