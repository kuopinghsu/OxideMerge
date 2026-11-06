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
use krilla::text::{Font, GlyphId, KrillaGlyph};
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

/// Drawn lines of compared text in one report, all files together, a wrapped line counting
/// once for each line it takes; the differences after them are only counted. A page holds
/// about 50, so this is some 400 pages.
const REPORT_LINES: usize = 20_000;
/// Unchanged rows kept above a change when its difference is cut: the rows above them are
/// left out first, so the change itself is always drawn.
const LEAD_ROWS: usize = 100;

#[cfg(test)]
thread_local! {
    /// The lines of compared text the reports of a test may draw.
    static LINES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn report_lines() -> usize {
    #[cfg(test)]
    if let Some(n) = LINES.with(|l| l.get()) {
        return n;
    }
    REPORT_LINES
}
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
        // Color-blind: blue and orange, changes in purple and conflicts in yellow, as the
        // program's own color-blind colors.
        let (add, del, chg, conf) = if color_blind {
            (
                ((0xe3, 0xf0, 0xfa), (0xa8, 0xd0, 0xee), (0x00, 0x72, 0xb2)),
                ((0xfd, 0xee, 0xdd), (0xf5, 0xc4, 0x8a), (0xd5, 0x5e, 0x00)),
                ((0xeb, 0xdd, 0xf9), (0xd3, 0xc0, 0xf0), (0x7b, 0x5b, 0xc4)),
                ((0xfa, 0xf5, 0xc4), (0xf0, 0xe4, 0x42), (0xa8, 0x92, 0x00)),
            )
        } else {
            (
                ((0xe7, 0xf7, 0xed), (0xa9, 0xe5, 0xbf), (0x16, 0xa3, 0x4a)),
                ((0xfd, 0xec, 0xee), (0xf5, 0xb8, 0xc1), (0xdc, 0x26, 0x26)),
                ((0xff, 0xf5, 0xdc), (0xf6, 0xd9, 0x8a), (0xb4, 0x6a, 0x00)),
                ((0xff, 0xe0, 0xe0), (0xf5, 0xa0, 0xa0), (0xb9, 0x1c, 0x1c)),
            )
        };
        Palette {
            add,
            del,
            chg,
            conf,
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

/// The font drawing a character, its advance in ems, the character drawn and its glyph in
/// that font.
type Glyph = (usize, f32, char, u16);

/// The fonts of a report, parsed, with the width of each character found once.
struct Faces<'a> {
    parsed: [Vec<ttf_parser::Face<'a>>; 2],
    fonts: [Vec<Font>; 2],
    /// For each family and character: the font drawing it, its advance in ems, the character
    /// drawn (`?` for one no font has) and its glyph.
    cache: RefCell<HashMap<(Family, char), Glyph>>,
    /// The characters no font has, drawn as `?`.
    missing: RefCell<std::collections::BTreeSet<char>>,
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
            return Err(tr("report.err_no_font").to_string());
        }
        Ok(Faces {
            parsed: [mono, sans],
            fonts: [mono_fonts, sans_fonts],
            cache: RefCell::new(HashMap::new()),
            missing: RefCell::default(),
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
                Some((i, adv, c, g.0))
            })
        };
        let found = find(c).or_else(|| {
            self.missing.borrow_mut().insert(c);
            find('?')
        });
        let found = found.unwrap_or((0, 0.5, '?', 0));
        self.cache.borrow_mut().insert((family, c), found);
        found
    }

    fn width(&self, family: Family, size: f32, text: &str) -> f32 {
        let chars: Vec<char> = text.chars().collect();
        self.forms(family, &chars)
            .into_iter()
            .map(|c| self.advance(family, c))
            .sum::<f32>()
            * size
    }

    /// How far `form`, a character of [`Faces::forms`], moves the pen at size 1: not at all for
    /// an alef drawn as part of a ligature.
    fn advance(&self, family: Family, form: Option<char>) -> f32 {
        form.map_or(0.0, |c| self.glyph(family, c).1)
    }

    /// `chars` as they are drawn, in reading order: Arabic letters in their joined forms
    /// where a font has them, so they are measured as drawn (see [`arabic_forms`]).
    fn forms(&self, family: Family, chars: &[char]) -> Vec<Option<char>> {
        if needs_bidi(chars) {
            arabic_forms(chars, |c| self.has(family, c))
        } else {
            chars.iter().copied().map(Some).collect()
        }
    }

    /// Whether a font of `family` draws `c`.
    fn has(&self, family: Family, c: char) -> bool {
        self.parsed[family as usize]
            .iter()
            .any(|f| f.glyph_index(c).is_some())
    }

    /// `chars` in the order they are drawn from left to right, Arabic letters in their joined
    /// forms where a font has them, each with the characters of `chars` it stands for: one, or
    /// a lam through the alef of their ligature.
    fn shown(&self, family: Family, chars: &[char]) -> Vec<(Range<usize>, char)> {
        if !needs_bidi(chars) {
            return chars
                .iter()
                .enumerate()
                .map(|(k, &c)| (k..k + 1, c))
                .collect();
        }
        let forms = arabic_forms(chars, |c| self.has(family, c));
        visual_order(chars)
            .into_iter()
            .filter_map(|(k, mirrored)| {
                let c = forms[k]?;
                Some((
                    ligature_span(&forms, k),
                    if mirrored { mirror(c) } else { c },
                ))
            })
            .collect()
    }
}

// ---------- right-to-left text ----------

/// Whether `c` is written right to left: Hebrew, Arabic, Syriac, Thaana and N'Ko letters and
/// marks and their presentation forms, but not the digits and number separators among them.
fn is_rtl(c: char) -> bool {
    matches!(c, '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFC}')
        && !is_number(c)
        && !matches!(c, '\u{060C}' | '\u{066A}'..='\u{066C}')
}

/// Whether a line holds text read right to left: letters, or Arabic numbers.
fn needs_bidi(chars: &[char]) -> bool {
    chars.iter().any(|&c| is_rtl(c) || is_arabic_number(c))
}

/// Whether `c` is an Arabic letter, after which digits read as Arabic numbers: separators and
/// signs like `-` and `%` then stay apart from them.
fn is_arabic(c: char) -> bool {
    matches!(c, '\u{0600}'..='\u{07BF}' | '\u{0860}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFC}')
        && is_rtl(c)
}

fn is_number(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '\u{0660}'..='\u{0669}' | '\u{06F0}'..='\u{06F9}')
}

/// An Arabic-Indic digit: always an Arabic number, which reads right to left among others
/// like it and keeps `%`, `+` and `-` apart, wherever it stands.
fn is_arabic_number(c: char) -> bool {
    matches!(c, '\u{0660}'..='\u{0669}')
}

fn is_ltr(c: char) -> bool {
    c.is_alphabetic() && !is_rtl(c)
}

/// A separator kept inside a number when it stands alone between two digits.
fn is_number_separator(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ':' | '/' | '\u{060C}' | '\u{066B}' | '\u{066C}'
    )
}

/// A sign kept with the digits it stands next to, unless they follow Arabic letters: `%`,
/// `#`, `°`, currency signs and the like.
fn is_number_terminator(c: char) -> bool {
    matches!(
        c,
        '#' | '$'
            | '%'
            | '°'
            | '±'
            | '¢'
            | '£'
            | '¤'
            | '¥'
            | '‰'
            | '‱'
            | '′'
            | '″'
            | '‴'
            | '\u{066A}'
    ) || ('\u{20A0}'..='\u{20CF}').contains(&c)
}

/// Which characters of `chars` are drawn as part of a number, left to right even inside
/// right-to-left text: digits, a separator standing alone between two of the same kind and,
/// unless the digits are Arabic numbers (Arabic-Indic digits, or any after Arabic letters), a
/// `+` or `-` between two of them and the signs like `%` next to them (the Unicode
/// bidirectional algorithm's rules W2, W4 and W5).
fn number_parts(chars: &[char]) -> Vec<bool> {
    let mut num: Vec<bool> = chars.iter().map(|&c| is_number(c)).collect();
    let mut arabic = vec![false; chars.len()];
    let mut last = false;
    for (k, &c) in chars.iter().enumerate() {
        if is_rtl(c) || is_ltr(c) {
            last = is_arabic(c);
        }
        arabic[k] = last;
    }
    let digit = |k: usize| is_number(chars[k]);
    let an = |k: usize| arabic[k] || is_arabic_number(chars[k]);
    for k in 1..chars.len().saturating_sub(1) {
        let c = chars[k];
        let european = !an(k - 1) && !an(k + 1) && matches!(c, '+' | '-');
        let separator = is_number_separator(c) && an(k - 1) == an(k + 1);
        if (separator || european) && digit(k - 1) && digit(k + 1) {
            num[k] = true;
        }
    }
    for k in 0..chars.len() {
        if !digit(k) || an(k) {
            continue;
        }
        let mut b = k;
        while b > 0 && is_number_terminator(chars[b - 1]) {
            b -= 1;
            num[b] = true;
        }
        let mut a = k + 1;
        while a < chars.len() && is_number_terminator(chars[a]) {
            num[a] = true;
            a += 1;
        }
    }
    num
}

fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        _ => c,
    }
}

/// For each character of `chars`, the bracket that pairs with it, if any: the first of
/// `( [ {` that is closed later on the line, each closing bracket matching the nearest one
/// still open of its kind.
fn bracket_pairs(chars: &[char]) -> Vec<Option<usize>> {
    let mut pairs = vec![None; chars.len()];
    let mut open: Vec<usize> = Vec::new();
    for (k, &c) in chars.iter().enumerate() {
        match c {
            '(' | '[' | '{' => {
                if open.len() == 63 {
                    break;
                }
                open.push(k);
            }
            ')' | ']' | '}' => {
                if let Some(at) = open.iter().rposition(|&o| mirror(chars[o]) == c) {
                    let o = open[at];
                    open.truncate(at);
                    pairs[o] = Some(k);
                    pairs[k] = Some(o);
                }
            }
            _ => {}
        }
    }
    pairs
}

/// For each character, whether it reads right to left (`Some(true)`), left to right
/// (`Some(false)`) or as the text around it (`None`). Letters read their own way, Arabic
/// numbers right to left, other numbers the way of the letter before them (rule W7). Paired
/// brackets read as rule N0 gives it: left to right when the pair holds a left-to-right letter
/// (or a number after one); right to left when it holds right-to-left text or a number after
/// it, and the text before the pair is right to left too; left to right when only the inside
/// is; and as the text around them when the pair holds neither.
fn directions(chars: &[char], pairs: &[Option<usize>], num: &[bool]) -> Vec<Option<bool>> {
    // Whether each character with a direction reads right to left: letters, Arabic numbers,
    // and other numbers taking that of the letter before them (rule W7).
    let mut strong = vec![None; chars.len()];
    let mut last = false;
    for (k, &c) in chars.iter().enumerate() {
        if is_rtl(c) || is_ltr(c) {
            last = is_rtl(c);
            strong[k] = Some(last);
        } else if is_arabic_number(c) {
            strong[k] = Some(true);
        } else if num[k] {
            strong[k] = Some(last);
        }
    }
    for o in 0..chars.len() {
        let Some(close) = pairs[o].filter(|&p| p > o) else {
            continue;
        };
        let inside = &strong[o + 1..close];
        let d = if inside.contains(&Some(false)) {
            false
        } else if inside.contains(&Some(true)) {
            strong[..o].iter().rev().find_map(|&s| s).unwrap_or(false)
        } else {
            continue;
        };
        // A pair resolved counts as the text before the pairs after it.
        strong[o] = Some(d);
        strong[close] = Some(d);
    }
    strong
}

/// The order in which the characters of a left-to-right line are drawn, each with whether it
/// is mirrored: a simplified Unicode bidirectional algorithm. A run from a right-to-left
/// character or Arabic number to the last right-to-left character or number before the next
/// left-to-right one is reversed, its brackets mirrored, and the numbers in it still read
/// left to right. Characters read in the direction [`directions`] gives them.
fn visual_order(chars: &[char]) -> Vec<(usize, bool)> {
    let mut order: Vec<(usize, bool)> = (0..chars.len()).map(|k| (k, false)).collect();
    let num = number_parts(chars);
    let pairs = bracket_pairs(chars);
    let dir = directions(chars, &pairs, &num);
    let mut i = 0;
    while i < chars.len() {
        if dir[i] != Some(true) {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        let mut k = end;
        while k < chars.len() && dir[k] != Some(false) {
            if dir[k] == Some(true) {
                end = k + 1;
            }
            k += 1;
        }
        order[i..end].reverse();
        let mut j = i;
        while j < end {
            let at = order[j].0;
            if !num[at] {
                let c = chars[at];
                order[j].1 = c != mirror(c);
                j += 1;
                continue;
            }
            // Reversed, a number runs from its end back; it goes back in reading order.
            let mut e = j + 1;
            while e < end && num[order[e].0] && order[e].0 + 1 == order[e - 1].0 {
                e += 1;
            }
            order[j..e].reverse();
            j = e;
        }
        i = end;
    }
    order
}

/// How an Arabic letter joins: `None` for one that is not in the Arabic presentation forms,
/// else whether it joins the letter after it too, with its isolated presentation form; the
/// final, initial and medial forms follow that one.
fn arabic_joining(c: char) -> Option<(bool, u32)> {
    // Letters Persian and Urdu add, with their forms in the U+FB50 block: peh, tcheh, jeh,
    // keheh, gaf and farsi yeh.
    match c {
        '\u{067E}' => return Some((true, 0xFB56)),
        '\u{0686}' => return Some((true, 0xFB7A)),
        '\u{0698}' => return Some((false, 0xFB8A)),
        '\u{06A9}' => return Some((true, 0xFB8E)),
        '\u{06AF}' => return Some((true, 0xFB92)),
        '\u{06CC}' => return Some((true, 0xFBFC)),
        _ => {}
    }
    // From U+0621: U for hamza (no joining), R for a letter joining only the one before it,
    // D for one joining both sides, and x for one not in the presentation forms.
    const TYPES: &[u8] = b"URRRRDRDRDDDDDRRRRDDDDDDDDxxxxxxDDDDDDDRRD";
    let i = (c as u32).checked_sub(0x0621)? as usize;
    let mut form = 0xFE80;
    for (k, &t) in TYPES.iter().enumerate() {
        let forms = match t {
            b'U' => 1,
            b'R' => 2,
            b'D' => 4,
            _ => 0,
        };
        if k == i {
            return (forms > 1).then_some((t == b'D', form));
        }
        form += forms;
    }
    None
}

/// An Arabic mark, passed over when letters join.
fn transparent(c: char) -> bool {
    matches!(
        c,
        '\u{0610}'..='\u{061A}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{06D6}'..='\u{06DC}'
            | '\u{06DF}'..='\u{06E4}'
            | '\u{06E7}'
            | '\u{06E8}'
            | '\u{06EA}'..='\u{06ED}'
    )
}

/// The characters the form at `k` of [`arabic_forms`] stands for: `k` alone, or a lam through
/// the alef drawn with it, the first `None` past the marks after it.
fn ligature_span(forms: &[Option<char>], k: usize) -> Range<usize> {
    if !matches!(forms[k], Some('\u{FEF5}'..='\u{FEFC}')) {
        return k..k + 1;
    }
    let mut n = k + 1;
    while let Some(Some(c)) = forms.get(n) {
        if !transparent(*c) {
            return k..k + 1;
        }
        n += 1;
    }
    if n < forms.len() {
        k..n + 1
    } else {
        k..k + 1
    }
}

/// `chars` with each Arabic letter in its isolated, final, initial or medial presentation
/// form, as it joins its neighbors, where `has` the form; marks between letters are passed
/// over. A lam and the alef after it become the two letters' ligature, where `has` it: the
/// lam's place holds it, and the alef's is `None`.
fn arabic_forms(chars: &[char], has: impl Fn(char) -> bool) -> Vec<Option<char>> {
    // Tatweel and the zero width joiner join both sides.
    let causes = |c: char| matches!(c, '\u{0640}' | '\u{200D}');
    let joins_after = |c: char| causes(c) || arabic_joining(c).is_some_and(|j| j.0);
    let joins_before = |c: char| causes(c) || arabic_joining(c).is_some();
    let mut out = chars.to_vec();
    for (k, &c) in chars.iter().enumerate() {
        let Some((dual, isolated)) = arabic_joining(c) else {
            continue;
        };
        let before = chars[..k].iter().rev().find(|&&p| !transparent(p));
        let after = chars[k + 1..].iter().find(|&&n| !transparent(n));
        let prev = before.is_some_and(|&p| joins_after(p));
        let next = dual && after.is_some_and(|&n| joins_before(n));
        let offset = match (prev, next) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        };
        if let Some(form) = char::from_u32(isolated + offset).filter(|&f| has(f)) {
            out[k] = form;
        }
    }
    let mut out: Vec<Option<char>> = out.into_iter().map(Some).collect();
    for (k, &c) in chars.iter().enumerate() {
        if c != '\u{0644}' {
            continue;
        }
        let Some(a) = (k + 1..chars.len()).find(|&n| !transparent(chars[n])) else {
            continue;
        };
        let isolated = match chars[a] {
            '\u{0622}' => 0xFEF5,
            '\u{0623}' => 0xFEF7,
            '\u{0625}' => 0xFEF9,
            '\u{0627}' => 0xFEFB,
            _ => continue,
        };
        let before = chars[..k].iter().rev().find(|&&p| !transparent(p));
        let prev = before.is_some_and(|&p| joins_after(p));
        if let Some(form) = char::from_u32(isolated + u32::from(prev)).filter(|&f| has(f)) {
            out[k] = Some(form);
            out[a] = None;
        }
    }
    out
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
    /// Lines of compared text still to draw; see [`REPORT_LINES`].
    lines_left: usize,
    /// Whether the differences are drawn in one column.
    unified: bool,
    /// The page the column names of the difference being drawn were last drawn on.
    labeled: usize,
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
            lines_left: report_lines(),
            unified: false,
            labeled: 0,
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
        let text = self.shown_text(family, text);
        self.put_text(x, y, size, family, text, color);
    }

    /// `text` in the order its characters are drawn.
    fn shown_text(&self, family: Family, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        self.faces
            .shown(family, &chars)
            .into_iter()
            .map(|(_, c)| c)
            .collect()
    }

    /// `text`, already in the order it is drawn.
    fn put_text(&mut self, x: f32, y: f32, size: f32, family: Family, text: String, color: Color) {
        if !text.is_empty() {
            self.op(Op::Text {
                x,
                y,
                size,
                family,
                text,
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
            let chars: Vec<char> = para.chars().collect();
            let forms = self.faces.forms(family, &chars);
            for (&c, &form) in chars.iter().zip(&forms) {
                let cw = self.faces.advance(family, form) * size;
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

    /// `text`, its start cut with `…` when it is wider than `width`.
    fn fit(&self, family: Family, size: f32, text: &str, width: f32) -> String {
        if self.faces.width(family, size, text) <= width {
            return text.to_owned();
        }
        let room = width - self.faces.width(family, size, "…");
        let chars: Vec<char> = text.chars().collect();
        let forms = self.faces.forms(family, &chars);
        let mut w = 0.0;
        let mut tail: Vec<char> = Vec::new();
        let mut part_of_ligature = false;
        for (&c, &form) in chars.iter().zip(&forms).rev() {
            w += self.faces.advance(family, form) * size;
            if w > room {
                // An alef without the lam it was drawn with would be drawn on its own, wider.
                if part_of_ligature {
                    tail.pop();
                }
                break;
            }
            tail.push(c);
            part_of_ligature = form.is_none();
        }
        std::iter::once('…').chain(tail.into_iter().rev()).collect()
    }

    /// Makes room for a line of compared text; on a new page the column names come first.
    fn code_need(&mut self, doc: &Doc) {
        self.need(CODE_LEAD);
        if self.page() != self.labeled {
            self.columns(doc, true);
        }
    }

    /// The names of the sides over their columns, and on a page a difference continues on,
    /// the name of its file.
    fn columns(&mut self, doc: &Doc, continued: bool) {
        let n = doc.panes.len();
        let labels = !self.unified && n > 1;
        let name = if continued { doc.name } else { "" };
        if labels || !name.is_empty() {
            self.need(CODE_LEAD);
            let y = self.y + CODE_LEAD * 0.78;
            if labels {
                let col_w = self.content_w() / n as f32;
                for p in 0..n {
                    let x = self.left() + p as f32 * col_w + 2.0;
                    self.text(x, y, SMALL_SIZE, Family::Sans, side_name(p, n), DIM);
                }
            }
            if !name.is_empty() {
                let name = self.fit(Family::Sans, SMALL_SIZE, name, self.content_w() / 2.0);
                let w = self.faces.width(Family::Sans, SMALL_SIZE, &name);
                self.text(
                    self.right() - w - 2.0,
                    y,
                    SMALL_SIZE,
                    Family::Sans,
                    &name,
                    DIM,
                );
            }
            self.y += CODE_LEAD;
        }
        self.labeled = self.page();
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
        let logical: Vec<char> = chars.iter().map(|cell| cell.c).collect();
        let chars: Vec<Cell> = self
            .faces
            .shown(Family::Mono, &logical)
            .into_iter()
            .map(|(span, c)| Cell {
                c,
                marked: chars[span].iter().any(|cell| cell.marked),
            })
            .collect();
        // The changed words: a darker background and a line under them.
        let mark = colors.map(|c| c.1);
        let mut run_start: Option<f32> = None;
        let mut cx = tx;
        let mut text = String::new();
        let mut ends = Vec::new();
        for (k, cell) in chars.iter().enumerate() {
            let (_, adv, c, _) = self.faces.glyph(Family::Mono, cell.c);
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
        self.put_text(tx, base, CODE_SIZE, Family::Mono, text, BLACK);
    }

    /// Splits `chars` into lines that fit `width`.
    fn wrap_cells(&self, chars: &[Cell], width: f32) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let (mut start, mut w) = (0, 0.0);
        let plain: Vec<char> = chars.iter().map(|cell| cell.c).collect();
        let forms = self.faces.forms(Family::Mono, &plain);
        for (i, &form) in forms.iter().enumerate() {
            let cw = self.faces.advance(Family::Mono, form) * CODE_SIZE;
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

    /// The rows of one difference side by side, a column for each side, until the lines left
    /// run out. Returns the number of rows drawn.
    fn rows_side_by_side(&mut self, doc: &Doc, rows: Range<usize>) -> usize {
        let n = doc.panes.len();
        let col_w = self.content_w() / n as f32;
        let num_w = self.num_width(doc.panes);
        let sign_w = self.faces.width(Family::Mono, CODE_SIZE, "+ ");
        let text_w = col_w - num_w - sign_w - 6.0;
        for r in rows.clone() {
            if self.lines_left == 0 {
                return r - rows.start;
            }
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
                self.code_need(doc);
                self.lines_left = self.lines_left.saturating_sub(1);
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
        rows.len()
    }

    /// The rows of one difference of two sides in one column: unchanged lines once, and the
    /// lines of each change with the left side's above the right side's, until the lines left
    /// run out. Returns the number of rows drawn.
    fn rows_unified(&mut self, doc: &Doc, rows: Range<usize>) -> usize {
        let num_w = self.num_width(doc.panes);
        let sign_w = self.faces.width(Family::Mono, CODE_SIZE, "+ ");
        let w = self.content_w();
        let text_w = w - 2.0 * num_w - sign_w - 6.0;
        let mut r = rows.start;
        while r < rows.end {
            if self.lines_left == 0 {
                return r - rows.start;
            }
            let cellv = row_cells(doc.result, doc.panes, doc.opts, r);
            if cellv.iter().all(|c| c.0 == "ctx") {
                let row = &doc.result.rows[r];
                let p = if row.line(0).is_some() { 0 } else { 1 };
                let chars = row
                    .line(p)
                    .map_or_else(Vec::new, |l| cells(&doc.panes[p][l], &[]));
                let nums = [row.line(0), row.line(1)];
                self.unified_line(doc, &chars, text_w, num_w, nums, "ctx");
                self.notes(doc, r);
                r += 1;
                continue;
            }
            // A run of changed rows of one block, each drawing up to two lines.
            let block = doc.result.rows[r].block();
            let mut end = r;
            let mut all = Vec::new();
            while end < rows.end && (end - r) * 2 < self.lines_left.max(2) {
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
                    self.unified_line(doc, &chars, text_w, num_w, nums, class);
                }
            }
            for row_r in r..end {
                self.notes(doc, row_r);
            }
            r = end;
        }
        rows.len()
    }

    fn unified_line(
        &mut self,
        doc: &Doc,
        chars: &[Cell],
        text_w: f32,
        num_w: f32,
        nums: [Option<usize>; 2],
        class: &str,
    ) {
        let w = self.content_w();
        for (k, part) in self.wrap_cells(chars, text_w).into_iter().enumerate() {
            self.code_need(doc);
            self.lines_left = self.lines_left.saturating_sub(1);
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

    /// Every difference of one compared text, while lines are left to draw: each is cut to
    /// [`HUNK_MAX_ROWS`] rows (unless the whole file is shown) and to the lines left, with the
    /// rows above its change left out first.
    fn diffs(&mut self, doc: &Doc, opts: &PdfOptions) {
        let list = match opts.context {
            Some(c) => hunks_with(doc.result, c),
            None => whole_file(doc.result),
        };
        let total = list.len();
        self.unified = opts.layout == Layout::Unified && doc.panes.len() == 2;
        let mut shown = 0;
        for (k, (b, rows)) in list.into_iter().enumerate() {
            if self.lines_left == 0 {
                break;
            }
            shown += 1;
            let cap = match opts.context {
                Some(_) => HUNK_MAX_ROWS.min(self.lines_left),
                None => self.lines_left,
            };
            let start = if rows.len() > cap {
                let change = doc.result.blocks[b].first_row.clamp(rows.start, rows.end);
                change - (change - rows.start).min(LEAD_ROWS.min(cap / 4))
            } else {
                rows.start
            };
            self.hunk_bar(doc.result, b, doc.panes.len(), (k, total));
            let more = |n: usize| trf("report.more_lines", &[("n", n.to_string())]);
            if start > rows.start {
                self.para(SMALL_SIZE, Family::Sans, &more(start - rows.start), DIM);
            }
            self.columns(doc, false);
            let drawn = start..rows.end.min(start + cap);
            let done = if self.unified {
                self.rows_unified(doc, drawn)
            } else {
                self.rows_side_by_side(doc, drawn)
            };
            let hidden = rows.end - (start + done);
            if hidden > 0 {
                self.para(SMALL_SIZE, Family::Sans, &more(hidden), DIM);
            }
            self.gap(8.0);
        }
        if shown < total {
            let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
            self.para(TEXT_SIZE, Family::Sans, &more, DIM);
        }
    }

    /// A note naming the characters no font has, which are drawn as `?`; also written to
    /// standard error.
    fn missing_note(&mut self) {
        let missing = self.faces.missing.borrow().clone();
        if missing.is_empty() {
            return;
        }
        let mut chars: Vec<String> = missing
            .iter()
            .take(20)
            .map(|c| format!("U+{:04X}", *c as u32))
            .collect();
        if missing.len() > 20 {
            chars.push("…".into());
        }
        let note = trf(
            "report.missing_glyphs",
            &[("n", missing.len().to_string()), ("chars", chars.join(" "))],
        );
        eprintln!("{note}");
        self.gap(8.0);
        self.para(TEXT_SIZE, Family::Sans, &note, DIM);
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
            let settings = PageSettings::from_wh(self.w, self.h)
                .ok_or_else(|| tr("report.err_page_size").to_string())?;
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
                text: self.shown_text(Family::Sans, &self.title),
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
                // Runs of the characters each font draws, placed one after another. The
                // glyphs are placed as they are, not shaped: shaping would turn `!=` or `->`
                // into ligatures in some code fonts and move glyphs off their marks.
                let fonts = &self.faces.fonts[*family as usize];
                let mut cx = *x;
                let mut run = String::new();
                let mut glyphs: Vec<KrillaGlyph> = Vec::new();
                let mut run_font = usize::MAX;
                let mut run_x = cx;
                let flush = |s: &mut Surface,
                             run: &mut String,
                             glyphs: &mut Vec<KrillaGlyph>,
                             f: usize,
                             at: f32| {
                    if !run.trim().is_empty() {
                        s.draw_glyphs(
                            Point::from_xy(at, *y),
                            glyphs.as_slice(),
                            fonts[f].clone(),
                            run,
                            *size,
                            false,
                        );
                    }
                    run.clear();
                    glyphs.clear();
                };
                for ch in text.chars() {
                    let (f, adv, ch, id) = self.faces.glyph(*family, ch);
                    if f != run_font && !run.is_empty() {
                        flush(s, &mut run, &mut glyphs, run_font, run_x);
                    }
                    if run.is_empty() {
                        run_font = f;
                        run_x = cx;
                    }
                    let start = run.len();
                    run.push(ch);
                    glyphs.push(KrillaGlyph {
                        glyph_id: GlyphId::new(id.into()),
                        text_range: start..run.len(),
                        x_advance: adv,
                        x_offset: 0.0,
                        y_offset: 0.0,
                        y_advance: 0.0,
                        location: None,
                    });
                    cx += adv * size;
                }
                if !run.is_empty() {
                    flush(s, &mut run, &mut glyphs, run_font, run_x);
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
    /// The file in a folder report, empty in a text report.
    name: &'a str,
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
            name: "",
            panes,
            result,
            opts,
            notes,
        };
        w.diffs(&doc, pdf);
    }
    w.sections(sections);
    w.missing_note();
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
        w.missing_note();
        return w.finish();
    };
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
                    name: &rel,
                    panes: &refs,
                    result: &result,
                    opts: dopts,
                    notes: &[],
                };
                if w.lines_left == 0 {
                    let total = hunks_with(&result, 0).len();
                    let more = trf("report.more_diffs", &[("n", total.to_string())]);
                    w.para(TEXT_SIZE, Family::Sans, &more, DIM);
                    continue;
                }
                w.diffs(&doc, pdf);
            }
            FileDiffs::Note(note) => w.para(TEXT_SIZE, Family::Sans, &note, DIM),
            FileDiffs::None => {}
        }
        w.gap(6.0);
    }
    w.sections(sections);
    w.missing_note();
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
    let widths: Vec<f32> = (0..header.len())
        .map(|i| if i < 3 { widths[i] } else { side_w })
        .collect();
    table_row(w, &header, &widths, None, None);
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
        table_row(w, &cells, &widths, Some(color), Some(&header));
    }
    if listed.is_empty() {
        w.para(TEXT_SIZE, Family::Sans, tr("report.no_match"), DIM);
    }
}

/// One row of the file table, each cell wrapped to its column and cut with `…` where it would
/// not fit on a page. `header` is the table's header row, drawn again first when this row
/// starts a new page; `None` for the header row itself.
fn table_row(
    w: &mut Writer,
    cells: &[String],
    widths: &[f32],
    color: Option<Color>,
    header: Option<&[String]>,
) {
    let max_lines = (((w.bottom() - w.top()) / CODE_LEAD) as usize)
        .saturating_sub(4)
        .max(1);
    let wrapped: Vec<Vec<String>> = cells
        .iter()
        .zip(widths)
        .map(|(c, &cw)| {
            let mut lines = w.wrap(Family::Sans, SMALL_SIZE, c, cw - 6.0);
            if lines.len() > max_lines {
                lines.truncate(max_lines);
                lines[max_lines - 1].push('…');
            }
            lines
        })
        .collect();
    let lines = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let h = lines as f32 * CODE_LEAD + 3.0;
    let page = w.page();
    w.need(h);
    if let (true, Some(head)) = (w.page() != page, header) {
        table_row(w, head, widths, None, None);
    }
    let (x0, y) = (w.left(), w.y);
    if header.is_none() {
        w.fill(x0, y, w.content_w(), h, SURFACE);
    }
    if let Some(c) = color {
        w.fill(x0 + 2.0, y + 3.5, 4.0, 4.0, c);
    }
    let mut x = x0 + 10.0;
    for (lines, cw) in wrapped.iter().zip(widths) {
        for (k, l) in lines.iter().enumerate() {
            let base = y + 1.5 + CODE_LEAD * (k as f32 + 0.78);
            w.text(x, base, SMALL_SIZE, Family::Sans, l, BLACK);
        }
        x += cw;
    }
    w.y += h;
    w.op(Op::Line(x0, w.y, w.right(), w.y, 0.4, RULE));
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

    fn pages(pdf: &[u8]) -> usize {
        String::from_utf8_lossy(pdf).matches("/Type/Page/").count()
    }

    #[test]
    fn a_whole_file_or_wide_context_report_is_cut_but_still_shows_the_change() {
        LINES.with(|l| l.set(Some(300)));
        let a: Vec<String> = (0..6000).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        b[5000] = "the change".into();
        for context in [None, Some(500)] {
            let opts = PdfOptions {
                context,
                ..Default::default()
            };
            let pdf = report(&a, &b, &opts);
            let text = read(&pdf);
            assert!(text.contains("the change"), "{context:?}");
            assert!(text.contains("line 4999") && !text.contains("line 4500"));
            assert!(pages(&pdf) <= 10, "{context:?}: {} pages", pages(&pdf));
            let skipped = trf("report.more_lines", &[("n", "".into())]);
            let word = skipped.split_whitespace().nth(1).unwrap().to_owned();
            assert!(text.contains(&word), "the rows left out are noted: {text}");
        }
        LINES.with(|l| l.set(None));
    }

    #[test]
    fn wrapped_lines_count_against_the_lines_a_report_draws() {
        LINES.with(|l| l.set(Some(200)));
        let a: Vec<String> = (0..300)
            .map(|i| format!("{i} {}", "x ".repeat(900)))
            .collect();
        let b: Vec<String> = (0..300)
            .map(|i| format!("{i} {}", "y ".repeat(900)))
            .collect();
        let pdf = report(&a, &b, &PdfOptions::default());
        assert!(pages(&pdf) <= 8, "{} pages", pages(&pdf));
        LINES.with(|l| l.set(None));
    }

    #[test]
    fn characters_no_font_has_are_noted_and_code_is_not_made_into_ligatures() {
        let built_in = egui::FontDefinitions::default();
        let hack = built_in.font_data.get("Hack").unwrap();
        let only_hack = Fonts {
            mono: vec![FontFile {
                data: hack.font.clone(),
                index: hack.index,
            }],
            sans: vec![FontFile {
                data: hack.font.clone(),
                index: hack.index,
            }],
        };
        let panes: Vec<Vec<String>> = vec![v(&["a != b -> c", "x"]), v(&["a != b -> c", "測"])];
        let refs: Vec<&[String]> = panes.iter().map(Vec::as_slice).collect();
        let opts = DiffOptions::default();
        let result = compute(&refs, &opts);
        let titles = v(&["l", "r"]);
        let pdf = text_pdf(
            &titles,
            &refs,
            &result,
            &opts,
            &[],
            "",
            &[],
            &only_hack,
            &PdfOptions {
                context: None,
                ..Default::default()
            },
        )
        .unwrap();
        let text = read(&pdf);
        assert!(text.contains("U+6E2C"), "{text}");
        assert!(text.contains("!=") && text.contains("->"), "{text}");
    }

    #[test]
    fn right_to_left_text_is_drawn_in_reading_order_with_numbers_and_brackets() {
        let shown = |s: &str| -> String {
            let chars: Vec<char> = s.chars().collect();
            visual_order(&chars)
                .into_iter()
                .map(|(k, m)| if m { mirror(chars[k]) } else { chars[k] })
                .collect()
        };
        assert_eq!(shown("abc שלום 123 def"), "abc 123 םולש def");
        assert_eq!(shown("x = \"אב 1,000.5\";"), "x = \"1,000.5 בא\";");
        assert_eq!(shown("(שלום)"), "(םולש)");
        assert_eq!(shown("א(ב)ג"), "ג(ב)א");
        assert_eq!(shown("\"שלום (1)\";"), "\"(1) םולש\";");
        assert_eq!(shown("א () b"), "א () b");
        assert_eq!(shown("plain (text) 12"), "plain (text) 12");
        // Dates, phone numbers, percentages and prices stay whole after Hebrew...
        assert_eq!(shown("תאריך 2024-01-15"), "2024-01-15 ךיראת");
        assert_eq!(shown("טל 050-1234567"), "050-1234567 לט");
        assert_eq!(shown("הנחה 50%"), "50% החנה");
        assert_eq!(shown("מחיר $5"), "$5 ריחמ");
        // ...but after Arabic letters, digits are Arabic numbers the signs keep apart.
        assert_eq!(shown("تاريخ 2024-01-15"), "15-01-2024 خيرات");
        assert_eq!(shown("ع ٣٫١٤"), "٣٫١٤ ع");
        // A bracket pair holding English reads left to right, around reversed Hebrew.
        assert_eq!(shown("שלום (עולם foo)"), "םולש (םלוע foo)");
        assert_eq!(shown("א [ב (ג) ד] e"), "[ד (ג) ב] א e");
        // A pair holding only Hebrew, after English or nothing, reads left to right.
        assert_eq!(shown("(ב) ג"), "(ב) ג");
        assert_eq!(shown("x (אב) יד"), "x (בא) די");
        assert_eq!(shown("א (1)"), "(1) א");
        // Arabic-Indic digits are Arabic numbers wherever they stand: two after English read
        // right to left, and `%` keeps apart from them.
        assert_eq!(shown("a ١٢ ٣٤"), "a ٣٤ ١٢");
        assert_eq!(shown("ב 12%"), "12% ב");
        assert_eq!(shown("ב ١٢%"), "١٢ ב%");
        // A number after English reads left to right, even after an Arabic number.
        assert_eq!(
            shown("\"ar\": [\"٠\", 0, \"صفر\"]"),
            "\"ar\": [\"٠\", 0, \"رفص\"]"
        );
        assert_eq!(shown("a ١٢ 34"), "a ١٢ 34");
        // As drawn, too, on a line with no right-to-left letter.
        let drawn_fonts = fonts();
        let faces = Faces::new(&drawn_fonts).unwrap();
        let drawn = |s: &str| -> String {
            let chars: Vec<char> = s.chars().collect();
            faces
                .shown(Family::Mono, &chars)
                .into_iter()
                .map(|(_, c)| c)
                .collect()
        };
        assert_eq!(drawn("a ١٢ ٣٤"), "a ٣٤ ١٢");
        assert_eq!(drawn("x \u{FFFF} y"), "x \u{FFFF} y");
        // A name cut to fit keeps a lam with its alef, so it is no wider than it was cut to.
        let writer = Writer::new(&drawn_fonts, &PdfOptions::default(), "").unwrap();
        let name = "سلام سلام سلام.txt";
        for room in (10..120).step_by(3) {
            let room = room as f32;
            let cut = writer.fit(Family::Sans, 9.0, name, room);
            assert!(
                writer.faces.width(Family::Sans, 9.0, &cut) <= room + 0.01,
                "{cut}"
            );
        }

        let forms = |s: &str| -> Vec<Option<char>> {
            arabic_forms(&s.chars().collect::<Vec<_>>(), |_| true)
        };
        let joined = |s: &str| -> Vec<char> { forms(s).into_iter().flatten().collect() };
        // Persian letters join, and so does a letter before a zero width joiner.
        assert_eq!(joined("یک"), ['\u{FBFE}', '\u{FB8F}']);
        assert_eq!(joined("ب\u{200D}")[0], '\u{FE91}');
        // Quranic signs that are not marks don't let the letters around them join.
        assert_eq!(joined("ب\u{06DE}ب")[0], '\u{FE8F}');

        assert_eq!(
            forms("سلام"),
            [Some('\u{FEB3}'), Some('\u{FEFC}'), None, Some('\u{FEE1}')],
            "initial, lam-alef final, isolated"
        );
        assert_eq!(
            forms("لا لأ"),
            [Some('\u{FEFB}'), None, Some(' '), Some('\u{FEF7}'), None]
        );
        // A mark between lam and alef is drawn after their ligature.
        assert_eq!(forms("لَا"), [Some('\u{FEFB}'), Some('\u{064E}'), None]);
        assert_eq!(ligature_span(&forms("لَا x"), 0), 0..3);
        assert_eq!(ligature_span(&forms("لَا x"), 1), 1..2);
        assert_eq!(joined("سل"), ['\u{FEB3}', '\u{FEDE}'], "initial and final");
        let salam: Vec<char> = "سلام".chars().collect();
        assert_eq!(
            arabic_forms(&salam, |_| false),
            salam.iter().copied().map(Some).collect::<Vec<_>>(),
            "kept without the forms"
        );
        assert_eq!(
            joined("بَب")[..2],
            ['\u{FE91}', '\u{064E}'],
            "a mark is passed over"
        );

        let panes: Vec<Vec<String>> =
            vec![v(&["שלום עולם (1)", "سلام"]), v(&["שלום עולם (2)", "سلام x"])];
        let refs: Vec<&[String]> = panes.iter().map(Vec::as_slice).collect();
        let opts = DiffOptions::default();
        let result = compute(&refs, &opts);
        let titles = v(&["ימין", "שמאל"]);
        let pdf = text_pdf(
            &titles,
            &refs,
            &result,
            &opts,
            &[],
            "",
            &[],
            &fonts(),
            &PdfOptions::default(),
        )
        .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn the_file_table_header_is_repeated_on_each_page() {
        use crate::core::folder::CmpStatus;
        let items: Vec<DirItem> = (0..400)
            .map(|i| DirItem {
                rel_dir: String::new(),
                name: format!("file{i}.txt"),
                is_dir: false,
                exists: [true, true, false],
                size: [Some(1), Some(2), None],
                mtime: [None; 3],
                status: CmpStatus::Different,
                diff_side: None,
                binary: false,
                error: None,
                depth: 0,
                parent: None,
                children: Vec::new(),
            })
            .collect();
        let refs: Vec<&DirItem> = items.iter().collect();
        let roots = vec![PathBuf::from("a"), PathBuf::from("b")];
        let pdf = folder_pdf(
            &refs,
            &roots,
            &roots,
            None,
            "",
            &[],
            &fonts(),
            &Default::default(),
        )
        .unwrap();
        let text = read(&pdf);
        let header = tr("col.result");
        assert!(pages(&pdf) > 2);
        assert!(text.matches(header).count() >= pages(&pdf) - 1, "{text}");
    }

    #[test]
    fn a_three_way_report_has_every_side() {
        let base = v(&["one", "two", "three"]);
        let ours = v(&["one", "deux", "three"]);
        let theirs = v(&["one", "zwei", "three"]);
        let panes: Vec<&[String]> = vec![&base, &ours, &theirs];
        let opts = DiffOptions::default();
        let result = compute(&panes, &opts);
        let titles = v(&["base.txt", "ours.txt", "theirs.txt"]);
        let pdf = text_pdf(
            &titles,
            &panes,
            &result,
            &opts,
            &[],
            "",
            &[],
            &fonts(),
            &Default::default(),
        )
        .unwrap();
        let text = read(&pdf);
        for want in ["base.txt", "theirs.txt", "deux", "zwei"] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
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
