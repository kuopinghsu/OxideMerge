//! PDF reports of text and folder compares, for printing and for attaching to change requests.
//!
//! A PDF report holds what the HTML report holds — the compared files, the rules, the counts and
//! every difference with the lines around it, changed words marked and line numbers — laid out
//! on pages with a running header and page numbers. The PDF is written directly, with no browser:
//! the fonts the program uses are embedded (only the glyphs used), so Chinese, Japanese and
//! Korean text prints as it shows. Colors are only one of the cues: each changed line also has a
//! sign in front of it (`-` `+` `~` `!`), changed words are underlined and lines missing on one
//! side are hatched, so a report printed in black and white still says what changed.

use super::diff::{DiffOptions, DiffResult};
use super::folder::DirItem;
use super::html_report::{
    file_diffs, hunks_with, ignored_blocks, kind, line_range, row_cells, status_class, FileDiffs,
    HUNK_MAX_ROWS, LINE_CHARS,
};
use super::report::{fmt_size, fmt_time, side_name, status_text};
use crate::i18n::{tr, trf};
use krilla::action::LinkAction;
use krilla::annotation::{LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::destination::XyzDestination;
use krilla::geom::{PathBuilder, Point, Rect, Size, Transform};
use krilla::image::Image;
use krilla::metadata::Metadata;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule, Stroke};
use krilla::surface::Surface;
use krilla::text::{Font, TextDirection};
use krilla::Document;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

/// The bytes of a font file, shared.
pub type Blob = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// One face of a font file.
#[derive(Clone)]
pub struct FontFile {
    pub data: Blob,
    pub index: u32,
}

/// The fonts of a report, each list in the order its fonts are tried for a character: `mono` for
/// the compared lines, `sans` for everything else.
#[derive(Clone, Default)]
pub struct Fonts {
    pub mono: Vec<FontFile>,
    pub sans: Vec<FontFile>,
}

/// Makes a PDF report; it may run on another thread.
pub type PdfJob = Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>;

/// A part added at the end of a report, as the HTML report adds it: what matching by key
/// found, review notes, or the review summary.
#[derive(Clone, Debug, Default)]
pub struct Section {
    pub title: String,
    pub lines: Vec<String>,
    /// Lines in the code font, kept as they are (for JSON).
    pub mono: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageSize {
    #[default]
    A4,
    Letter,
}

/// How the lines of a two-way compare are laid out; three-way compares are always side by side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    SideBySide,
    /// One column: the removed lines of each change above the added lines.
    Unified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfOptions {
    pub page: PageSize,
    /// Pages are landscape unless this is set.
    pub portrait: bool,
    pub layout: Layout,
    /// Unchanged lines shown around each difference; `None` shows the whole file.
    pub context: Option<usize>,
    /// Blue and orange instead of green and red, as the color-blind setting of the program.
    pub color_blind: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        PdfOptions {
            page: PageSize::A4,
            portrait: false,
            layout: Layout::SideBySide,
            context: Some(3),
            color_blind: false,
        }
    }
}

/// Rows of compared lines in one report, all files together; the differences after them are only
/// counted. A page holds about 50 rows, so this is some 2,000 pages.
const REPORT_ROWS: usize = 100_000;
const MARGIN: f32 = 36.0;
/// Where the footer's "OxideMerge" links to.
pub const HOME_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// The app icon, decoded once and shrunk to a size that keeps every page
/// small: krilla embeds it once and every page refers to it.
fn icon_image() -> Option<&'static Image> {
    static ICON: std::sync::OnceLock<Option<Image>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let img = super::html_report::icon_rgba(48)?;
        let (w, h) = img.dimensions();
        Some(Image::from_rgba8(img.into_raw(), w, h))
    })
    .as_ref()
}
const CODE_SIZE: f32 = 7.5;
const CODE_LEAD: f32 = 9.6;
const TEXT_SIZE: f32 = 9.0;
const TITLE_SIZE: f32 = 16.0;
const SMALL_SIZE: f32 = 7.5;
/// Spaces a tab stands for.
const TAB: usize = 4;

type Color = (u8, u8, u8);
const BLACK: Color = (0x1d, 0x23, 0x30);
const DIM: Color = (0x66, 0x70, 0x85);
/// The report's accent, for the footer link.
const LINK: Color = (0x5b, 0x6c, 0xff);
const RULE: Color = (0xd0, 0xd4, 0xdb);
const SURFACE: Color = (0xf1, 0xf3, 0xf6);

/// The colors of the kinds of lines: a background, the background of the changed words, and the
/// color of the sign in front.
struct Palette {
    add: (Color, Color, Color),
    del: (Color, Color, Color),
    chg: (Color, Color, Color),
    conf: (Color, Color, Color),
    ign: (Color, Color, Color),
}

impl Palette {
    fn new(color_blind: bool) -> Palette {
        let (add, del) = if color_blind {
            (
                ((0xe3, 0xf0, 0xfa), (0xa8, 0xd0, 0xee), (0x00, 0x72, 0xb2)),
                ((0xfd, 0xee, 0xdd), (0xf5, 0xc4, 0x8a), (0xd5, 0x5e, 0x00)),
            )
        } else {
            (
                ((0xe7, 0xf7, 0xed), (0xa9, 0xe5, 0xbf), (0x16, 0xa3, 0x4a)),
                ((0xfd, 0xec, 0xee), (0xf5, 0xb8, 0xc1), (0xdc, 0x26, 0x26)),
            )
        };
        Palette {
            add,
            del,
            chg: ((0xff, 0xf5, 0xdc), (0xf6, 0xd9, 0x8a), (0xb4, 0x6a, 0x00)),
            conf: ((0xff, 0xe0, 0xe0), (0xf5, 0xa0, 0xa0), (0xb9, 0x1c, 0x1c)),
            ign: ((0xee, 0xf0, 0xfb), (0xd8, 0xdc, 0xf5), (0x5b, 0x6c, 0xff)),
        }
    }

    /// Background, word background, sign color and sign of a cell of `class` (see
    /// [`row_cells`]), or `None` for an unchanged line.
    fn of(&self, class: &str) -> Option<(Color, Color, Color, &'static str)> {
        let ((bg, mark, sign), s) = match class {
            "add" => (self.add, "+"),
            "del" => (self.del, "-"),
            "chg" => (self.chg, "~"),
            "conf" => (self.conf, "!"),
            "ign" => (self.ign, "="),
            _ => return None,
        };
        Some((bg, mark, sign, s))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Family {
    Mono = 0,
    Sans = 1,
}

/// The font drawing a character, its advance in ems and the character drawn.
type Glyph = (usize, f32, char);

/// The fonts of a report, parsed, with the width of each character found once.
struct Faces<'a> {
    parsed: [Vec<ttf_parser::Face<'a>>; 2],
    fonts: [Vec<Font>; 2],
    /// For each family and character: the font drawing it, its advance in ems and the character
    /// drawn, `?` for one no font has.
    cache: RefCell<HashMap<(Family, char), Glyph>>,
}

impl<'a> Faces<'a> {
    fn new(fonts: &'a Fonts) -> Result<Faces<'a>, String> {
        let load = |files: &'a [FontFile]| {
            let mut parsed = Vec::new();
            let mut made = Vec::new();
            for f in files {
                let bytes: &'a [u8] = (*f.data).as_ref();
                let Ok(face) = ttf_parser::Face::parse(bytes, f.index) else {
                    continue;
                };
                let Some(font) = Font::new(f.data.clone().into(), f.index) else {
                    continue;
                };
                parsed.push(face);
                made.push(font);
            }
            (parsed, made)
        };
        let (mono, mono_fonts) = load(&fonts.mono);
        let (sans, sans_fonts) = load(&fonts.sans);
        if mono.is_empty() || sans.is_empty() {
            return Err("no font could be read for the PDF report".into());
        }
        Ok(Faces {
            parsed: [mono, sans],
            fonts: [mono_fonts, sans_fonts],
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// The font that draws `c` in `family` and its advance in ems; a character no font has is
    /// drawn as `?`.
    fn glyph(&self, family: Family, c: char) -> Glyph {
        if let Some(&found) = self.cache.borrow().get(&(family, c)) {
            return found;
        }
        let faces = &self.parsed[family as usize];
        let find = |c: char| {
            faces.iter().enumerate().find_map(|(i, f)| {
                let g = f.glyph_index(c)?;
                let adv = f.glyph_hor_advance(g)? as f32 / f.units_per_em() as f32;
                Some((i, adv, c))
            })
        };
        let found = find(c).or_else(|| find('?')).unwrap_or((0, 0.5, '?'));
        self.cache.borrow_mut().insert((family, c), found);
        found
    }

    fn width(&self, family: Family, size: f32, text: &str) -> f32 {
        text.chars().map(|c| self.glyph(family, c).1).sum::<f32>() * size
    }
}

/// A character of a compared line and whether it is part of a change.
#[derive(Clone, Copy)]
struct Cell {
    c: char,
    marked: bool,
}

/// The characters of `line` as they are drawn: tabs as spaces to the next stop, control
/// characters as their pictures, and at most [`LINE_CHARS`] characters, with `…` after a cut.
fn cells(line: &str, marks: &[Range<usize>]) -> Vec<Cell> {
    let mut out = Vec::new();
    let mut m = 0;
    for (count, (i, c)) in line.char_indices().enumerate() {
        if count == LINE_CHARS {
            let marked = marks.iter().any(|r| r.end > i);
            out.push(Cell { c: '…', marked });
            break;
        }
        while m < marks.len() && marks[m].end <= i {
            m += 1;
        }
        let marked = m < marks.len() && marks[m].start <= i;
        match c {
            '\t' => {
                let n = TAB - out.len() % TAB;
                out.extend(std::iter::repeat_n(Cell { c: ' ', marked }, n));
            }
            c if (c as u32) < 0x20 => out.push(Cell {
                c: char::from_u32(0x2400 + c as u32).unwrap_or('?'),
                marked,
            }),
            '\u{7f}' => out.push(Cell {
                c: '\u{2421}',
                marked,
            }),
            c => out.push(Cell { c, marked }),
        }
    }
    out
}

enum Op {
    Fill(Rect, Color),
    Hatch(Rect),
    Line(f32, f32, f32, f32, f32, Color),
    Text {
        x: f32,
        y: f32,
        size: f32,
        family: Family,
        text: String,
        color: Color,
    },
}

/// A bookmark: its title, page, height on the page, and nesting level (0 or 1).
struct Mark {
    title: String,
    page: usize,
    y: f32,
    level: u8,
}

/// Lays the report out on pages, then draws them.
struct Writer<'a> {
    faces: Faces<'a>,
    w: f32,
    h: f32,
    pages: Vec<Vec<Op>>,
    y: f32,
    marks: Vec<Mark>,
    palette: Palette,
    /// The title in the header of each page.
    title: String,
    /// The bookmark level of a difference: 0 in a text report, 1 under its file in a folder
    /// report.
    diff_level: u8,
}

impl<'a> Writer<'a> {
    fn new(fonts: &'a Fonts, opts: &PdfOptions, title: &str) -> Result<Writer<'a>, String> {
        let (short, long) = match opts.page {
            PageSize::A4 => (595.28, 841.89),
            PageSize::Letter => (612.0, 792.0),
        };
        let (w, h) = if opts.portrait {
            (short, long)
        } else {
            (long, short)
        };
        let mut wr = Writer {
            faces: Faces::new(fonts)?,
            w,
            h,
            pages: Vec::new(),
            y: 0.0,
            marks: Vec::new(),
            palette: Palette::new(opts.color_blind),
            title: title.to_owned(),
            diff_level: 0,
        };
        wr.new_page();
        Ok(wr)
    }

    fn left(&self) -> f32 {
        MARGIN
    }
    fn right(&self) -> f32 {
        self.w - MARGIN
    }
    fn content_w(&self) -> f32 {
        self.w - 2.0 * MARGIN
    }
    fn top(&self) -> f32 {
        MARGIN + 16.0
    }
    fn bottom(&self) -> f32 {
        self.h - MARGIN - 12.0
    }

    fn new_page(&mut self) {
        self.pages.push(Vec::new());
        self.y = self.top();
    }

    /// Starts a new page unless `height` still fits on this one.
    fn need(&mut self, height: f32) {
        if self.y + height > self.bottom() && self.y > self.top() {
            self.new_page();
        }
    }

    fn op(&mut self, op: Op) {
        self.pages.last_mut().expect("a page").push(op);
    }

    fn page(&self) -> usize {
        self.pages.len() - 1
    }

    fn mark(&mut self, title: String, level: u8) {
        self.marks.push(Mark {
            title,
            page: self.page(),
            y: self.y,
            level,
        });
    }

    fn text(&mut self, x: f32, y: f32, size: f32, family: Family, text: &str, color: Color) {
        if !text.is_empty() {
            self.op(Op::Text {
                x,
                y,
                size,
                family,
                text: text.to_owned(),
                color,
            });
        }
    }

    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            self.op(Op::Fill(r, color));
        }
    }

    /// Breaks `text` into lines no wider than `width`, at spaces where it can.
    fn wrap(&self, family: Family, size: f32, text: &str, width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut line = String::new();
            let mut w = 0.0;
            let mut last_space: Option<usize> = None;
            for c in para.chars() {
                let cw = self.faces.glyph(family, c).1 * size;
                if w + cw > width && !line.is_empty() {
                    match last_space.filter(|&s| s > 0) {
                        Some(s) => {
                            let rest = line.split_off(s);
                            lines.push(line.trim_end().to_owned());
                            line = rest.trim_start().to_owned();
                        }
                        None => lines.push(std::mem::take(&mut line)),
                    }
                    w = self.faces.width(family, size, &line);
                    last_space = None;
                }
                if c == ' ' {
                    last_space = Some(line.len());
                }
                line.push(c);
                w += cw;
            }
            lines.push(line);
        }
        lines
    }

    /// A paragraph of `text`, wrapped to the page.
    fn para(&mut self, size: f32, family: Family, text: &str, color: Color) {
        let lead = size * 1.35;
        for line in self.wrap(family, size, text, self.content_w()) {
            self.need(lead);
            let (x, y) = (self.left(), self.y + size);
            self.text(x, y, size, family, &line, color);
            self.y += lead;
        }
    }

    fn gap(&mut self, h: f32) {
        self.y += h;
    }

    fn heading(&mut self, text: &str) {
        self.need(TITLE_SIZE * 2.5);
        let y = self.y + TITLE_SIZE;
        self.text(self.left(), y, TITLE_SIZE, Family::Sans, text, BLACK);
        self.y += TITLE_SIZE * 1.5;
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.para(SMALL_SIZE, Family::Sans, &now, DIM);
        self.gap(4.0);
    }

    /// The compared sides, one per line.
    fn sides(&mut self, names: &[String]) {
        let n = names.len();
        for (p, name) in names.iter().enumerate() {
            self.para(
                TEXT_SIZE,
                Family::Sans,
                &format!("{}: {name}", side_name(p, n)),
                BLACK,
            );
        }
    }

    fn rules(&mut self, rules: &str) {
        if !rules.is_empty() {
            let text = format!(
                "{}: {}",
                tr("report.rules"),
                super::rules::display_name(rules)
            );
            self.para(TEXT_SIZE, Family::Sans, &text, DIM);
        }
    }

    /// The counts of the report on one line, as `label value` pairs.
    fn counts(&mut self, cards: &[(&str, String)]) {
        let text = cards
            .iter()
            .map(|(label, v)| format!("{label}: {v}"))
            .collect::<Vec<_>>()
            .join("   ·   ");
        self.gap(2.0);
        self.para(TEXT_SIZE, Family::Sans, &text, BLACK);
        self.gap(6.0);
    }

    /// The bar above a difference: its kind, where it is on each side, and its number.
    fn hunk_bar(&mut self, result: &DiffResult, b: usize, n: usize, (i, total): (usize, usize)) {
        let h = TEXT_SIZE * 1.9;
        self.need(h + 3.0 * CODE_LEAD);
        let (class, label) = kind(result, b);
        let blk = &result.blocks[b];
        let pos = (0..n)
            .map(|p| format!("{} {}", side_name(p, n), line_range(&blk.ranges[p])))
            .collect::<Vec<_>>()
            .join("    ");
        self.mark(format!("{}. {label}  {pos}", i + 1), self.diff_level);
        let (x, y, w) = (self.left(), self.y, self.content_w());
        self.fill(x, y, w, h, SURFACE);
        let sign_color = self.palette.of(class).map_or(DIM, |c| c.2);
        let base = y + h * 0.68;
        self.text(x + 5.0, base, TEXT_SIZE, Family::Sans, label, sign_color);
        let lw = self.faces.width(Family::Sans, TEXT_SIZE, label);
        self.text(x + 14.0 + lw, base, TEXT_SIZE, Family::Sans, &pos, DIM);
        let num = trf(
            "report.diff_n",
            &[("i", (i + 1).to_string()), ("n", total.to_string())],
        );
        let nw = self.faces.width(Family::Sans, SMALL_SIZE, &num);
        self.text(x + w - nw - 5.0, base, SMALL_SIZE, Family::Sans, &num, DIM);
        self.y += h;
    }

    /// One drawn line of compared text in a column starting at `x`: its background, sign, line
    /// number and characters, with the changed ones marked.
    #[allow(clippy::too_many_arguments)]
    fn code_line(
        &mut self,
        x: f32,
        w: f32,
        num_w: f32,
        nums: &[Option<usize>],
        class: &str,
        chars: &[Cell],
        first: bool,
    ) {
        let y = self.y;
        let colors = self.palette.of(class);
        match (class, colors) {
            ("gap", _) => {
                if let Some(r) = Rect::from_xywh(x, y, w, CODE_LEAD) {
                    self.op(Op::Hatch(r));
                }
            }
            (_, Some((bg, ..))) => self.fill(x, y, w, CODE_LEAD, bg),
            _ => {}
        }
        let base = y + CODE_LEAD * 0.78;
        let mut tx = x + 2.0;
        for num in nums {
            if let (true, Some(l)) = (first, num) {
                let s = (l + 1).to_string();
                let sw = self.faces.width(Family::Mono, CODE_SIZE, &s);
                self.text(
                    tx + num_w - sw - 3.0,
                    base,
                    CODE_SIZE,
                    Family::Mono,
                    &s,
                    DIM,
                );
            }
            tx += num_w;
        }
        if let (true, Some((_, _, sign_color, sign))) = (first, colors) {
            self.text(tx, base, CODE_SIZE, Family::Mono, sign, sign_color);
        }
        tx += self.faces.width(Family::Mono, CODE_SIZE, "+ ");
        // The changed words: a darker background and a line under them.
        let mark = colors.map(|c| c.1);
        let mut run_start: Option<f32> = None;
        let mut cx = tx;
        let mut text = String::new();
        let mut ends = Vec::new();
        for (k, cell) in chars.iter().enumerate() {
            let (_, adv, c) = self.faces.glyph(Family::Mono, cell.c);
            let cw = adv * CODE_SIZE;
            match (cell.marked, run_start) {
                (true, None) => run_start = Some(cx),
                (false, Some(s)) => {
                    ends.push((s, cx));
                    run_start = None;
                }
                _ => {}
            }
            text.push(c);
            cx += cw;
            if k + 1 == chars.len() {
                if let Some(s) = run_start.take() {
                    ends.push((s, cx));
                }
            }
        }
        if let Some(mark) = mark {
            for &(s, e) in &ends {
                self.fill(s, y + 0.6, e - s, CODE_LEAD - 1.2, mark);
                self.op(Op::Line(
                    s,
                    base + 1.3,
                    e,
                    base + 1.3,
                    0.6,
                    (0x30, 0x35, 0x3f),
                ));
            }
        }
        self.text(tx, base, CODE_SIZE, Family::Mono, &text, BLACK);
    }

    /// Splits `chars` into lines that fit `width`.
    fn wrap_cells(&self, chars: &[Cell], width: f32) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let (mut start, mut w) = (0, 0.0);
        for (i, cell) in chars.iter().enumerate() {
            let cw = self.faces.glyph(Family::Mono, cell.c).1 * CODE_SIZE;
            if w + cw > width && i > start {
                out.push(start..i);
                start = i;
                w = 0.0;
            }
            w += cw;
        }
        out.push(start..chars.len());
        out
    }

    /// The width of the line numbers of `panes`.
    fn num_width(&self, panes: &[&[String]]) -> f32 {
        let digits = panes
            .iter()
            .map(|p| p.len().max(1).to_string().len())
            .max()
            .unwrap_or(1)
            .max(3);
        self.faces
            .width(Family::Mono, CODE_SIZE, &"0".repeat(digits + 1))
    }

    /// The rows of one difference side by side, a column for each side.
    fn rows_side_by_side(&mut self, doc: &Doc, rows: Range<usize>) {
        let n = doc.panes.len();
        let col_w = self.content_w() / n as f32;
        let num_w = self.num_width(doc.panes);
        let sign_w = self.faces.width(Family::Mono, CODE_SIZE, "+ ");
        let text_w = col_w - num_w - sign_w - 6.0;
        for r in rows {
            let row = &doc.result.rows[r];
            let cellv = row_cells(doc.result, doc.panes, doc.opts, r);
            let laid: Vec<(Vec<Cell>, Vec<Range<usize>>)> = (0..n)
                .map(|p| {
                    let chars = match row.line(p) {
                        Some(l) => cells(&doc.panes[p][l], &cellv[p].1),
                        None => Vec::new(),
                    };
                    let wrapped = self.wrap_cells(&chars, text_w);
                    (chars, wrapped)
                })
                .collect();
            let lines = laid.iter().map(|(_, w)| w.len()).max().unwrap_or(1);
            for k in 0..lines {
                self.need(CODE_LEAD);
                for (p, (chars, wrapped)) in laid.iter().enumerate() {
                    let x = self.left() + p as f32 * col_w;
                    let part = wrapped.get(k).map_or(&[][..], |r| &chars[r.clone()]);
                    let line = row.line(p);
                    let class = cellv[p].0;
                    self.code_line(x, col_w, num_w, &[line], class, part, k == 0);
                }
                if n > 1 {
                    for p in 1..n {
                        let x = self.left() + p as f32 * col_w;
                        self.op(Op::Line(x, self.y, x, self.y + CODE_LEAD, 0.5, RULE));
                    }
                }
                self.y += CODE_LEAD;
            }
            self.notes(doc, r);
        }
    }

    /// The rows of one difference of two sides in one column: unchanged lines once, and the
    /// lines of each change with the left side's above the right side's.
    fn rows_unified(&mut self, doc: &Doc, rows: Range<usize>) {
        let num_w = self.num_width(doc.panes);
        let sign_w = self.faces.width(Family::Mono, CODE_SIZE, "+ ");
        let w = self.content_w();
        let text_w = w - 2.0 * num_w - sign_w - 6.0;
        let mut r = rows.start;
        while r < rows.end {
            let cellv = row_cells(doc.result, doc.panes, doc.opts, r);
            if cellv.iter().all(|c| c.0 == "ctx") {
                let row = &doc.result.rows[r];
                let p = if row.line(0).is_some() { 0 } else { 1 };
                let chars = row
                    .line(p)
                    .map_or_else(Vec::new, |l| cells(&doc.panes[p][l], &[]));
                self.unified_line(&chars, text_w, num_w, [row.line(0), row.line(1)], "ctx");
                self.notes(doc, r);
                r += 1;
                continue;
            }
            // A run of changed rows of one block.
            let block = doc.result.rows[r].block();
            let mut end = r;
            let mut all = Vec::new();
            while end < rows.end {
                let c = row_cells(doc.result, doc.panes, doc.opts, end);
                if doc.result.rows[end].block() != block || c.iter().all(|c| c.0 == "ctx") {
                    break;
                }
                all.push(c);
                end += 1;
            }
            for p in 0..2 {
                for (k, row_r) in (r..end).enumerate() {
                    let row = &doc.result.rows[row_r];
                    let Some(l) = row.line(p) else { continue };
                    let (class, marks) = &all[k][p];
                    let chars = cells(&doc.panes[p][l], marks);
                    let mut nums = [None, None];
                    nums[p] = Some(l);
                    self.unified_line(&chars, text_w, num_w, nums, class);
                }
            }
            for row_r in r..end {
                self.notes(doc, row_r);
            }
            r = end;
        }
    }

    fn unified_line(
        &mut self,
        chars: &[Cell],
        text_w: f32,
        num_w: f32,
        nums: [Option<usize>; 2],
        class: &str,
    ) {
        let w = self.content_w();
        for (k, part) in self.wrap_cells(chars, text_w).into_iter().enumerate() {
            self.need(CODE_LEAD);
            let x = self.left();
            self.code_line(x, w, num_w, &nums, class, &chars[part], k == 0);
            self.y += CODE_LEAD;
        }
    }

    /// The bookmark comments on the lines of row `r`.
    fn notes(&mut self, doc: &Doc, r: usize) {
        let n = doc.panes.len();
        for (p, marks) in doc.notes.iter().enumerate().take(n) {
            let Some(l) = doc.result.rows[r].line(p) else {
                continue;
            };
            let Some(text) = marks.get(&l).filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            let line = format!("{} {}: {text}", side_name(p, n), l + 1);
            for part in self.wrap(Family::Sans, SMALL_SIZE, &line, self.content_w() - 16.0) {
                self.need(CODE_LEAD);
                let (x, y) = (self.left(), self.y);
                self.fill(x, y, self.content_w(), CODE_LEAD, (0xfb, 0xf6, 0xe4));
                self.text(
                    x + 12.0,
                    y + CODE_LEAD * 0.78,
                    SMALL_SIZE,
                    Family::Sans,
                    &part,
                    BLACK,
                );
                self.y += CODE_LEAD;
            }
        }
    }

    /// Every difference of one compared text, within `budget` rows. Returns the rows drawn and
    /// the number of differences.
    fn diffs(&mut self, doc: &Doc, opts: &PdfOptions, budget: usize) -> (usize, usize) {
        let list = match opts.context {
            Some(c) => hunks_with(doc.result, c),
            None => whole_file(doc.result),
        };
        let total = list.len();
        let unified = opts.layout == Layout::Unified && doc.panes.len() == 2;
        let mut used = 0;
        let mut shown = 0;
        for (k, (b, rows)) in list.into_iter().enumerate() {
            let cap = if opts.context.is_some() {
                HUNK_MAX_ROWS
            } else {
                usize::MAX
            };
            let drawn = rows.start..rows.end.min(rows.start.saturating_add(cap));
            if used + drawn.len() > budget && shown > 0 {
                break;
            }
            used += drawn.len();
            shown += 1;
            self.hunk_bar(doc.result, b, doc.panes.len(), (k, total));
            let hidden = rows.len() - drawn.len();
            if unified {
                self.rows_unified(doc, drawn);
            } else {
                self.rows_side_by_side(doc, drawn);
            }
            if hidden > 0 {
                let more = trf("report.more_lines", &[("n", hidden.to_string())]);
                self.para(SMALL_SIZE, Family::Sans, &more, DIM);
            }
            self.gap(8.0);
        }
        if shown < total {
            let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
            self.para(TEXT_SIZE, Family::Sans, &more, DIM);
        }
        (used, total)
    }

    /// The parts added at the end, each with a bookmark.
    fn sections(&mut self, sections: &[Section]) {
        for sec in sections {
            self.gap(10.0);
            self.need(TEXT_SIZE * 5.0);
            self.mark(sec.title.clone(), 0);
            self.para(TEXT_SIZE + 2.0, Family::Sans, &sec.title, BLACK);
            self.gap(2.0);
            for line in &sec.lines {
                if sec.mono {
                    self.para(SMALL_SIZE, Family::Mono, line, BLACK);
                } else {
                    self.para(TEXT_SIZE, Family::Sans, &format!("•  {line}"), BLACK);
                }
            }
        }
    }

    /// Draws the pages laid out, with a header and page numbers on each, and the bookmarks.
    fn finish(self) -> Result<Vec<u8>, String> {
        let mut document = Document::new();
        let count = self.pages.len();
        for (i, ops) in self.pages.iter().enumerate() {
            let settings = PageSettings::from_wh(self.w, self.h).ok_or("bad page size")?;
            let mut page = document.start_page_with(settings);
            let mut s = page.surface();
            for op in ops {
                self.draw(&mut s, op);
            }
            // Header and footer.
            // The app icon sits just left of the title, inside the margin.
            let icon = SMALL_SIZE + 3.0;
            if let (Some(img), Some(size)) = (icon_image(), Size::from_wh(icon, icon)) {
                s.push_transform(&Transform::from_translate(MARGIN, MARGIN - icon + 2.0));
                s.draw_image(img.clone(), size);
                s.pop();
            }
            let header = Op::Text {
                x: MARGIN + icon + 3.0,
                y: MARGIN,
                size: SMALL_SIZE,
                family: Family::Sans,
                text: self.title.clone(),
                color: DIM,
            };
            self.draw(&mut s, &header);
            self.draw(
                &mut s,
                &Op::Line(
                    MARGIN,
                    MARGIN + 4.0,
                    self.w - MARGIN,
                    MARGIN + 4.0,
                    0.5,
                    RULE,
                ),
            );
            let num = trf(
                "report.page_n",
                &[("i", (i + 1).to_string()), ("n", count.to_string())],
            );
            let nw = self.faces.width(Family::Sans, SMALL_SIZE, &num);
            let footer = Op::Text {
                x: self.w - MARGIN - nw,
                y: self.h - MARGIN + 4.0,
                size: SMALL_SIZE,
                family: Family::Sans,
                text: num,
                color: DIM,
            };
            self.draw(&mut s, &footer);
            let brand = Op::Text {
                x: MARGIN,
                y: self.h - MARGIN + 4.0,
                size: SMALL_SIZE,
                family: Family::Sans,
                text: "OxideMerge".into(),
                color: LINK,
            };
            self.draw(&mut s, &brand);
            s.finish();
            let bw = self.faces.width(Family::Sans, SMALL_SIZE, "OxideMerge");
            let at = self.h - MARGIN + 4.0 - SMALL_SIZE;
            if let Some(rect) = Rect::from_xywh(MARGIN, at, bw, SMALL_SIZE + 2.0) {
                page.add_annotation(
                    LinkAnnotation::new(
                        rect,
                        Target::Action(LinkAction::new(HOME_URL.into()).into()),
                    )
                    .into(),
                );
            }
            page.finish();
        }
        let mut outline = Outline::new();
        let mut parent: Option<OutlineNode> = None;
        for m in &self.marks {
            let node = OutlineNode::new(
                m.title.clone(),
                XyzDestination::new(m.page, Point::from_xy(0.0, m.y)),
            );
            if m.level == 0 {
                if let Some(p) = parent.take() {
                    outline.push_child(p);
                }
                parent = Some(node);
            } else {
                match parent.as_mut() {
                    Some(p) => p.push_child(node),
                    None => outline.push_child(node),
                }
            }
        }
        if let Some(p) = parent {
            outline.push_child(p);
        }
        document.set_outline(outline);
        document.set_metadata(
            Metadata::new()
                .title(self.title.clone())
                .creator(format!("OxideMerge {}", env!("CARGO_PKG_VERSION"))),
        );
        document.finish().map_err(|e| format!("{e:?}"))
    }

    fn draw(&self, s: &mut Surface, op: &Op) {
        let color = |(r, g, b): Color| rgb::Color::new(r, g, b);
        match op {
            Op::Fill(rect, c) => {
                let mut pb = PathBuilder::new();
                pb.push_rect(*rect);
                if let Some(path) = pb.finish() {
                    s.set_stroke(None);
                    s.set_fill(Some(Fill {
                        paint: color(*c).into(),
                        ..Default::default()
                    }));
                    s.draw_path(&path);
                }
            }
            Op::Hatch(rect) => {
                let mut pb = PathBuilder::new();
                pb.push_rect(*rect);
                let Some(clip) = pb.finish() else { return };
                s.set_stroke(None);
                s.set_fill(Some(Fill {
                    paint: color((0xf4, 0xf5, 0xf7)).into(),
                    ..Default::default()
                }));
                s.draw_path(&clip);
                s.push_clip_path(&clip, &FillRule::NonZero);
                let mut lines = PathBuilder::new();
                let h = rect.height();
                let mut x = rect.left() - h;
                while x < rect.right() {
                    lines.move_to(x, rect.bottom());
                    lines.line_to(x + h, rect.top());
                    x += 3.5;
                }
                if let Some(p) = lines.finish() {
                    s.set_fill(None);
                    s.set_stroke(Some(Stroke {
                        paint: color((0xb8, 0xbd, 0xc6)).into(),
                        width: 0.4,
                        ..Default::default()
                    }));
                    s.draw_path(&p);
                }
                s.pop();
            }
            Op::Line(x1, y1, x2, y2, width, c) => {
                let mut pb = PathBuilder::new();
                pb.move_to(*x1, *y1);
                pb.line_to(*x2, *y2);
                if let Some(p) = pb.finish() {
                    s.set_fill(None);
                    s.set_stroke(Some(Stroke {
                        paint: color(*c).into(),
                        width: *width,
                        ..Default::default()
                    }));
                    s.draw_path(&p);
                }
            }
            Op::Text {
                x,
                y,
                size,
                family,
                text,
                color: c,
            } => {
                s.set_stroke(None);
                s.set_fill(Some(Fill {
                    paint: color(*c).into(),
                    ..Default::default()
                }));
                // Runs of the characters each font draws, placed one after another.
                let fonts = &self.faces.fonts[*family as usize];
                let mut cx = *x;
                let mut run = String::new();
                let mut run_font = usize::MAX;
                let mut run_x = cx;
                for ch in text.chars() {
                    let (f, adv, ch) = self.faces.glyph(*family, ch);
                    if f != run_font && !run.is_empty() {
                        s.draw_text(
                            Point::from_xy(run_x, *y),
                            fonts[run_font].clone(),
                            *size,
                            &run,
                            false,
                            TextDirection::LeftToRight,
                        );
                        run.clear();
                    }
                    if run.is_empty() {
                        run_font = f;
                        run_x = cx;
                    }
                    run.push(ch);
                    cx += adv * size;
                }
                if !run.is_empty() && run.trim() != "" {
                    s.draw_text(
                        Point::from_xy(run_x, *y),
                        fonts[run_font].clone(),
                        *size,
                        &run,
                        false,
                        TextDirection::LeftToRight,
                    );
                }
            }
        }
    }
}

/// The differences of a file and the rows of each when the whole file is shown: each runs from
/// its own first row to the next one's, the first from the top of the file.
fn whole_file(result: &DiffResult) -> Vec<(usize, Range<usize>)> {
    let listed: Vec<usize> = (0..result.blocks.len())
        .filter(|&b| !result.blocks[b].ignored)
        .collect();
    listed
        .iter()
        .enumerate()
        .map(|(k, &b)| {
            let from = if k == 0 {
                0
            } else {
                result.blocks[b].first_row
            };
            let to = listed
                .get(k + 1)
                .map_or(result.rows.len(), |&j| result.blocks[j].first_row);
            (b, from..to)
        })
        .collect()
}

/// A compared text: the lines of each side, their differences, the options they were found
/// with, and bookmark comments.
struct Doc<'a> {
    panes: &'a [&'a [String]],
    result: &'a DiffResult,
    opts: &'a DiffOptions,
    notes: &'a [BTreeMap<usize, String>],
}

/// The PDF report of a two- or three-way text compare, with the same content as its HTML report.
#[allow(clippy::too_many_arguments)]
pub fn text_pdf(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
    sections: &[Section],
    fonts: &Fonts,
    pdf: &PdfOptions,
) -> Result<Vec<u8>, String> {
    let n = panes.len();
    let title = tr("report.text_title");
    let mut w = Writer::new(fonts, pdf, title)?;
    w.heading(title);
    w.sides(titles);
    w.rules(rules);
    let list = hunks_with(result, 0);
    let s = result.stats();
    let mut cards = vec![(tr("report.differences"), list.len().to_string())];
    if n == 2 {
        cards.push((tr("report.added"), s.added.to_string()));
        cards.push((tr("report.removed"), s.removed.to_string()));
        cards.push((tr("report.changed"), s.changed.to_string()));
    } else {
        cards.push((tr("report.kind_conflict"), result.conflicts().to_string()));
    }
    let ignored = ignored_blocks(result);
    if ignored > 0 {
        cards.push((tr("report.ignored"), ignored.to_string()));
    }
    w.counts(&cards);
    let comments: Vec<String> = notes
        .iter()
        .enumerate()
        .take(n)
        .flat_map(|(p, m)| {
            m.iter()
                .filter(|(_, t)| !t.trim().is_empty())
                .map(move |(l, t)| format!("{} {}: {t}", side_name(p, n), l + 1))
        })
        .collect();
    if !comments.is_empty() {
        w.para(TEXT_SIZE, Family::Sans, tr("report.comments"), BLACK);
        for c in comments {
            w.para(SMALL_SIZE, Family::Sans, &format!("•  {c}"), BLACK);
        }
        w.gap(6.0);
    }
    if list.is_empty() {
        w.para(TEXT_SIZE, Family::Sans, tr("report.no_diffs"), BLACK);
    } else {
        let doc = Doc {
            panes,
            result,
            opts,
            notes,
        };
        w.diffs(&doc, pdf, REPORT_ROWS);
    }
    w.sections(sections);
    w.finish()
}

/// The PDF report of a folder compare: the files listed with their status, sizes and dates, and
/// when `diffs` is given the differences of every changed text file, read from `roots`, as the
/// HTML report with `--report-diffs`. `display` names the compared folders.
#[allow(clippy::too_many_arguments)]
pub fn folder_pdf(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    diffs: Option<&DiffOptions>,
    rules: &str,
    sections: &[Section],
    fonts: &Fonts,
    pdf: &PdfOptions,
) -> Result<Vec<u8>, String> {
    let n = display.len();
    let mut listed: Vec<&DirItem> = items
        .iter()
        .copied()
        .filter(|it| !it.is_dir || it.status == super::folder::CmpStatus::Unique)
        .collect();
    if diffs.is_some() {
        listed.sort_by_key(|it| status_class(it.status) == "identical");
    }
    let count = |st: &str| {
        listed
            .iter()
            .filter(|it| status_class(it.status) == st)
            .count()
    };
    let title = tr("report.title");
    let mut w = Writer::new(fonts, pdf, title)?;
    w.heading(title);
    let names: Vec<String> = display.iter().map(|d| d.display().to_string()).collect();
    w.sides(&names);
    w.rules(rules);
    let mut cards = vec![
        (tr("report.files"), listed.len().to_string()),
        (tr("report.identical"), count("identical").to_string()),
        (tr("report.different"), count("different").to_string()),
        (tr("report.unique"), count("unique").to_string()),
    ];
    if count("error") > 0 {
        cards.push((tr("report.errors"), count("error").to_string()));
    }
    if count("other") > 0 {
        cards.push((tr("report.not_compared"), count("other").to_string()));
    }
    w.counts(&cards);
    w.mark(tr("report.files").into(), 0);
    file_table(&mut w, &listed, n);
    w.diff_level = 1;

    let Some(dopts) = diffs else {
        w.sections(sections);
        return w.finish();
    };
    let mut budget = REPORT_ROWS;
    let mut first = true;
    for it in &listed {
        let found = file_diffs(it, roots, dopts);
        if matches!(found, FileDiffs::None) {
            continue;
        }
        if first {
            w.new_page();
            first = false;
        }
        let rel = it.rel_path();
        w.need(TEXT_SIZE * 6.0);
        w.mark(rel.clone(), 0);
        w.gap(4.0);
        w.para(TEXT_SIZE + 2.0, Family::Sans, &rel, BLACK);
        w.para(SMALL_SIZE, Family::Sans, &status_text(it, n), DIM);
        w.gap(3.0);
        match found {
            FileDiffs::Diffs(docs, result) => {
                let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
                let doc = Doc {
                    panes: &refs,
                    result: &result,
                    opts: dopts,
                    notes: &[],
                };
                if budget == 0 {
                    let total = hunks_with(&result, 0).len();
                    let more = trf("report.more_diffs", &[("n", total.to_string())]);
                    w.para(TEXT_SIZE, Family::Sans, &more, DIM);
                    continue;
                }
                let (used, _) = w.diffs(&doc, pdf, budget);
                budget = budget.saturating_sub(used);
            }
            FileDiffs::Note(note) => w.para(TEXT_SIZE, Family::Sans, &note, DIM),
            FileDiffs::None => {}
        }
        w.gap(6.0);
    }
    w.sections(sections);
    w.finish()
}

/// The table of the files of a folder report.
fn file_table(w: &mut Writer, listed: &[&DirItem], n: usize) {
    let total = w.content_w() - 10.0;
    let side_w = total * 0.40 / n as f32;
    let widths = [total * 0.26, total * 0.18, total * 0.16];
    let mut header: Vec<String> = vec![
        tr("col.name").into(),
        tr("col.folder").into(),
        tr("col.result").into(),
    ];
    for s in 0..n {
        header.push(side_name(s, n).to_string());
    }
    let col_w = |i: usize| if i < 3 { widths[i] } else { side_w };
    let row = |w: &mut Writer, cells: &[String], color: Option<Color>, bold_bg: bool| {
        let wrapped: Vec<Vec<String>> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| w.wrap(Family::Sans, SMALL_SIZE, c, col_w(i) - 6.0))
            .collect();
        let lines = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        let h = lines as f32 * CODE_LEAD + 3.0;
        w.need(h);
        let (x0, y) = (w.left(), w.y);
        if bold_bg {
            w.fill(x0, y, w.content_w(), h, SURFACE);
        }
        if let Some(c) = color {
            w.fill(x0 + 2.0, y + 3.5, 4.0, 4.0, c);
        }
        let mut x = x0 + 10.0;
        for (i, lines) in wrapped.iter().enumerate() {
            for (k, l) in lines.iter().enumerate() {
                let base = y + 1.5 + CODE_LEAD * (k as f32 + 0.78);
                w.text(x, base, SMALL_SIZE, Family::Sans, l, BLACK);
            }
            x += col_w(i);
        }
        w.y += h;
        w.op(Op::Line(x0, w.y, w.right(), w.y, 0.4, RULE));
    };
    row(w, &header, None, true);
    for it in listed {
        let mut cells = vec![it.name.clone(), it.rel_dir.clone(), status_text(it, n)];
        for s in 0..n {
            let size = match it.size[s] {
                Some(_) => fmt_size(it.size[s]),
                None => "—".to_owned(),
            };
            let time = fmt_time(it.mtime[s]);
            cells.push(if it.is_dir || time.is_empty() {
                size
            } else {
                format!("{size}\n{time}")
            });
        }
        let color = match status_class(it.status) {
            "identical" => (0x16, 0xa3, 0x4a),
            "different" => (0xd9, 0x77, 0x06),
            "unique" => (0x7c, 0x3a, 0xed),
            "error" => (0xdc, 0x26, 0x26),
            _ => DIM,
        };
        row(w, &cells, Some(color), false);
    }
    if listed.is_empty() {
        w.para(TEXT_SIZE, Family::Sans, tr("report.no_match"), DIM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::compute;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    fn fonts() -> Fonts {
        crate::ui::fonts::document_fonts("en")
    }

    /// The text of a PDF as the program's own reader finds it.
    fn read(pdf: &[u8]) -> String {
        assert!(pdf.starts_with(b"%PDF-"));
        crate::core::extract::pdf::text(pdf, None).expect("the report reads back")
    }

    fn report(a: &[String], b: &[String], pdf: &PdfOptions) -> Vec<u8> {
        let panes: Vec<&[String]> = vec![a, b];
        let opts = DiffOptions::default();
        let result = compute(&panes, &opts);
        let titles = v(&["left.txt", "right.txt"]);
        text_pdf(&titles, &panes, &result, &opts, &[], "", &[], &fonts(), pdf).unwrap()
    }

    #[test]
    fn a_text_report_reads_back_with_its_lines_and_page_numbers() {
        let a = v(&["alpha", "beta one", "gamma"]);
        let b = v(&["alpha", "beta two", "gamma", "delta"]);
        let text = read(&report(&a, &b, &PdfOptions::default()));
        for want in ["left.txt", "right.txt", "beta", "one", "two", "delta"] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
        assert!(text.contains(tr("report.text_title")));
        let page = trf("report.page_n", &[("i", "1".into()), ("n", "1".into())]);
        let first_word = page.split_whitespace().next().unwrap().to_owned();
        assert!(text.contains(&first_word), "{page}: {text}");
    }

    #[test]
    fn every_page_carries_the_icon_and_a_link_home() {
        let a: Vec<String> = (0..400).map(|i| format!("line {i}")).collect();
        let b: Vec<String> = (0..400).map(|i| format!("row {i}")).collect();
        let pdf = report(&a, &b, &PdfOptions::default());
        let raw = String::from_utf8_lossy(&pdf);
        let pages = raw.matches("/Type/Page/").count();
        assert!(pages > 1, "a long report spans pages: {pages}");
        // One colour image plus its alpha mask, shared by every page.
        assert_eq!(raw.matches("/Subtype/Image").count(), 2);
        let link = format!("/S/URI/URI({HOME_URL})");
        assert_eq!(raw.matches(&link).count(), pages, "each page links home");
        assert_eq!(HOME_URL, "https://github.com/kuopinghsu/OxideMerge");
    }

    #[test]
    fn chinese_japanese_and_korean_text_is_embedded_when_a_font_has_it() {
        let f = fonts();
        let has = |c: char| {
            f.mono.iter().any(|ff| {
                ttf_parser::Face::parse((*ff.data).as_ref(), ff.index)
                    .is_ok_and(|face| face.glyph_index(c).is_some())
            })
        };
        let a = v(&["// 註解", "x"]);
        let b = v(&["// 註解：中文測試", "y テスト 한국어"]);
        let text = read(&report(&a, &b, &PdfOptions::default()));
        for (c, word) in [('測', "中文測試"), ('テ', "テスト"), ('한', "한국어")] {
            if has(c) {
                assert!(text.contains(word), "{word} missing in {text}");
            }
        }
    }

    #[test]
    fn the_whole_file_shows_lines_far_from_any_difference() {
        let a: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        b[39] = "changed".into();
        let near = read(&report(&a, &b, &PdfOptions::default()));
        assert!(!near.contains("line 5"), "{near}");
        let whole = PdfOptions {
            context: None,
            ..Default::default()
        };
        let all = read(&report(&a, &b, &whole));
        assert!(all.contains("line 5") && all.contains("changed"), "{all}");
    }

    #[test]
    fn a_unified_portrait_letter_report_has_both_sides_of_a_change() {
        let opts = PdfOptions {
            page: PageSize::Letter,
            portrait: true,
            layout: Layout::Unified,
            color_blind: true,
            ..Default::default()
        };
        let pdf = report(&v(&["a", "old"]), &v(&["a", "new"]), &opts);
        let text = read(&pdf);
        assert!(text.contains("old") && text.contains("new"), "{text}");
        let raw = String::from_utf8_lossy(&pdf);
        assert!(raw.contains("612") && raw.contains("792"), "a Letter page");
    }

    #[test]
    fn a_folder_report_lists_the_files_and_shows_their_differences_and_sections() {
        use crate::core::filter::FileFilter;
        use crate::core::folder::{FolderOptions, Progress, Scanner};
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("changed.txt"), "one\ntwo\n").unwrap();
        std::fs::write(b.path().join("changed.txt"), "one\nzwei\n").unwrap();
        std::fs::write(a.path().join("same.txt"), "same\n").unwrap();
        std::fs::write(b.path().join("same.txt"), "same\n").unwrap();
        std::fs::write(b.path().join("fresh.txt"), "new file\n").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: &opts,
            progress: &Progress::default(),
        }
        .run();
        let refs: Vec<&DirItem> = items.iter().collect();
        let sections = [Section {
            title: "Notes".into(),
            lines: v(&["changed.txt: checked"]),
            mono: false,
        }];
        let list = folder_pdf(
            &refs,
            &roots,
            &roots,
            None,
            "",
            &[],
            &fonts(),
            &Default::default(),
        );
        let listed = read(&list.unwrap());
        assert!(listed.contains("changed.txt") && listed.contains("fresh.txt"));
        assert!(!listed.contains("zwei"), "no differences without asking");
        let full = folder_pdf(
            &refs,
            &roots,
            &roots,
            Some(&opts),
            "",
            &sections,
            &fonts(),
            &Default::default(),
        );
        let text = read(&full.unwrap());
        for want in ["zwei", "new file", "checked", "same.txt"] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
    }

    #[test]
    fn a_ten_thousand_line_compare_stays_within_budget() {
        let a: Vec<String> = (0..10_000)
            .map(|i| format!("row {i} of the file"))
            .collect();
        let b: Vec<String> = a
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i % 10 == 0 {
                    format!("{l} changed")
                } else {
                    l.clone()
                }
            })
            .collect();
        let start = std::time::Instant::now();
        let pdf = report(&a, &b, &PdfOptions::default());
        let took = start.elapsed();
        assert!(took.as_secs() < 30, "took {took:?}");
        assert!(pdf.len() < 40 << 20, "{} bytes", pdf.len());
        assert!(read(&pdf).contains("row 9990 of the file changed"));
    }

    #[test]
    fn cells_expand_tabs_and_keep_marks() {
        let line = "a\tbc\u{1}";
        let marks: Vec<Range<usize>> = std::iter::once(2..4).collect();
        let c = cells(line, &marks);
        let s: String = c.iter().map(|c| c.c).collect();
        assert_eq!(s, "a   bc\u{2401}");
        let marked: Vec<bool> = c.iter().map(|c| c.marked).collect();
        assert_eq!(
            marked,
            [false, false, false, false, true, true, false],
            "the marks follow the characters, not the bytes"
        );
        let long = "x".repeat(LINE_CHARS + 10);
        assert_eq!(cells(&long, &[]).len(), LINE_CHARS + 1);
    }
}
