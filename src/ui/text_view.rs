//! File compare / merge view for text files (2-way and 3-way).

use crate::config::{Config, TextLayout};
use crate::core::comments;
use crate::core::diff::{self, BlockKind, DiffOptions, DiffResult, InlineMode, SyncPoint};
use crate::core::edit;
use crate::core::patch::{self, PatchStyle};
use crate::core::prettify;
use crate::core::report::side_name;
use crate::core::rules::{self, RuleDraft, RuleKind, RuleSet};
use crate::core::saved::{SavedCompare, SavedPane};
use crate::core::table;
use crate::core::table_match::{self, KeyColumn, RowState, TableDiff, TableMatch};
use crate::core::text::{self, Eol, TextDoc, TextEncoding, COMMON_ENCODINGS};
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::icons;
use crate::ui::markdown_view::{MarkdownView, Prepared};
use crate::ui::syntax::{self, LineSpans};
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, Icon, Mark, Outcome};
use egui::text::{CCursor, LayoutJob};
use egui::{
    pos2, vec2, Align2, Color32, FontId, Galley, Key, Modifiers, Rect, RichText, Sense, Stroke,
    StrokeKind, TextFormat, Ui,
};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::SystemTime;

pub struct Pane {
    pub doc: TextDoc,
    hl: Option<Vec<LineSpans>>,
    hl_rx: Option<Receiver<Option<Vec<LineSpans>>>>,
    hl_dirty: bool,
    hl_dark: bool,
    /// Number of times this pane was saved; undo snapshots from before a save are unsaved changes.
    saves: u64,
    /// Modification time of the file when it was loaded or saved.
    disk_time: Option<SystemTime>,
    /// The document as loaded while a reformatted (JSON / XML) copy is shown instead.
    original: Option<TextDoc>,
    /// Hashes of the lines as loaded, to tell which lines were edited since.
    baseline: Vec<u64>,
    /// Lines inserted or changed since the document was loaded.
    edited: HashSet<usize>,
}

/// Modification time of a document's file. Documents with a description (unpacked or
/// generated text) do not mirror their file, so they are not watched.
const MAC: bool = cfg!(target_os = "macos");

/// Whether `m` moves or deletes by word: Option on macOS (`mac`), Ctrl on Windows and Linux,
/// with Shift to select.
fn word_key_on(mac: bool, m: Modifiers) -> bool {
    if mac {
        m.alt && !m.mac_cmd && !m.ctrl
    } else {
        m.ctrl && !m.alt
    }
}

fn disk_time(doc: &TextDoc) -> Option<SystemTime> {
    if doc.description.is_some() {
        return None;
    }
    std::fs::metadata(doc.path.as_ref()?).ok()?.modified().ok()
}

impl Pane {
    fn new(doc: TextDoc) -> Self {
        let disk_time = disk_time(&doc);
        let baseline = line_hashes(&doc.lines);
        Pane {
            doc,
            hl: None,
            hl_rx: None,
            hl_dirty: true,
            hl_dark: false,
            saves: 0,
            disk_time,
            original: None,
            baseline,
            edited: HashSet::new(),
        }
    }

    /// Takes the current lines as the unedited state.
    fn reset_baseline(&mut self) {
        self.baseline = line_hashes(&self.doc.lines);
        self.edited.clear();
    }

    /// Reads the file of the pane again, as it is on disk now.
    fn reload(&mut self) -> Result<(), String> {
        let Some(path) = self.doc.path.clone() else {
            return Ok(());
        };
        let mut d = text::load_doc(&path, Some(self.doc.encoding)).map_err(|e| e.to_string())?;
        d.read_only = self.doc.read_only;
        d.compare_only = self.doc.compare_only;
        if d.lines != self.doc.lines {
            self.hl_dirty = true;
        }
        if d.lines != self.doc.lines || format_of(&d) != format_of(&self.doc) {
            // Undo steps from before hold text that is no longer the file's.
            self.saves += 1;
        }
        self.doc = d;
        self.disk_time = disk_time(&self.doc);
        self.reset_baseline();
        Ok(())
    }
}

fn line_hashes(lines: &[String]) -> Vec<u64> {
    use std::hash::{Hash, Hasher};
    lines
        .iter()
        .map(|l| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            l.hash(&mut h);
            h.finish()
        })
        .collect()
}

/// Lines of `new` inserted or changed since the lines hashed in `old`.
fn edited_lines(old: &[u64], new: &[String]) -> HashSet<usize> {
    let new = line_hashes(new);
    if old == new {
        return HashSet::new();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    similar::capture_diff_deadline(
        similar::Algorithm::Myers,
        old,
        0..old.len(),
        &new,
        0..new.len(),
        Some(deadline),
    )
    .iter()
    .filter(|op| op.tag() != similar::DiffTag::Equal)
    .flat_map(|op| op.new_range())
    .collect()
}

/// Where each line of `old` ended up in `new`, and whether it is still there (deleted lines map
/// to where they were). With `edited`, lines replaced by as many others count as still there,
/// each as the line that took its place.
fn line_map(old: &[String], new: &[String], edited: bool) -> Vec<(usize, bool)> {
    let mut map = vec![(0, false); old.len()];
    // Bounded in time: past the deadline the map is coarser but still valid.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    for op in
        similar::capture_diff_slices_deadline(similar::Algorithm::Myers, old, new, Some(deadline))
    {
        let (tag, o, n) = op.as_tag_tuple();
        let kept = tag == similar::DiffTag::Equal
            || (edited && tag == similar::DiffTag::Replace && o.len() == n.len());
        for (k, i) in o.enumerate() {
            map[i] = if kept {
                (n.start + k, true)
            } else {
                (n.start, false)
            };
        }
    }
    map
}

/// A reformatted, read-only copy of `doc` when it is JSON or XML whose layout changes.
fn prettified(doc: &TextDoc) -> Option<TextDoc> {
    let text = doc.text();
    let format = prettify::detect(doc.path.as_deref(), &text)?;
    let pretty = prettify::format(&text, format)?;
    let mut d = TextDoc::from_text(doc.path.clone(), &pretty);
    if d.lines == doc.lines {
        return None;
    }
    d.encoding = doc.encoding;
    d.bom = doc.bom;
    d.syntax = doc.syntax.clone();
    d.read_only = true;
    d.compare_only = true;
    let name = doc
        .description
        .clone()
        .or_else(|| doc.path.as_ref().map(|p| p.display().to_string()))
        .unwrap_or_else(|| tr("untitled").to_string());
    d.description = Some(format!("{name} [{}]", tr("label.formatted")));
    Some(d)
}

/// A line being typed in its own editor in one pane (see [`TextView::begin_edit`]).
struct EditState {
    row: usize,
    pane: usize,
    text: String,
    focus: bool,
}

/// A column (rectangular) selection: the same display columns of the rows from `anchor` to
/// `head` in one pane. Each end is (row, column); equal columns make a caret on every row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColSel {
    pub pane: usize,
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

/// Where the view was, by source line, while the comparison is computed again: the line at the
/// top, the cursor row's line and the column selection's lines, so that an edit that adds or
/// removes rows above them leaves the same text in view rather than the same row numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct HeldView {
    /// (pane, line, how far into its row the view is scrolled, in display lines).
    top: Option<(usize, usize, f32)>,
    /// (pane, line) of the cursor row.
    cursor: Option<(usize, usize)>,
    /// (pane, anchor line, head line) of the column selection.
    col: Option<(usize, usize, usize)>,
}

/// The text caret of a pane: where typing goes, and the other end of its selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub pane: usize,
    pub pos: edit::Pos,
    pub anchor: edit::Pos,
    /// The display column that Up and Down keep to.
    want: Option<usize>,
}

impl Caret {
    fn range(&self) -> (edit::Pos, edit::Pos) {
        (self.pos.min(self.anchor), self.pos.max(self.anchor))
    }

    fn has_selection(&self) -> bool {
        self.pos != self.anchor
    }
}

/// A comparison slower than this (in milliseconds) waits while typing goes on in a line.
const SLOW_REDIFF_MS: f32 = 8.0;
/// How long typing pauses (in seconds) before a comparison it put off is done.
const TYPING_PAUSE: f64 = 0.3;

/// The caret with the text the input method is composing at it.
struct CaretState {
    at: Caret,
    preedit: String,
    /// The other carets of the same pane, added with Ctrl+click (⌘-click on macOS) or Ctrl+Alt
    /// with Up or Down; `at` is the primary one, the last added, which the view follows.
    others: Vec<Caret>,
    /// Set while one key is applied at each caret in turn: edits then only change the lines.
    batch: Option<Batch>,
}

/// What applying one key at several carets did so far.
#[derive(Clone, Copy, Debug)]
struct Batch {
    changed: bool,
    /// Every edit was typing within a line, so the step can join the one before it.
    typing: bool,
}

/// Sorts `carets` (the primary one marked) and merges those that overlap or meet at a bare
/// caret, keeping the primary mark on the merged one.
fn merge_carets(carets: &mut Vec<(Caret, bool)>) {
    carets.sort_by_key(|(c, _)| c.range());
    let mut out: Vec<(Caret, bool)> = Vec::with_capacity(carets.len());
    for (c, primary) in carets.drain(..) {
        if let Some((last, last_primary)) = out.last_mut() {
            let (a0, a1) = last.range();
            let (b0, b1) = c.range();
            let touch = b0 == a1 && (!c.has_selection() || !last.has_selection());
            if b0 < a1 || touch || (b0, b1) == (a0, a1) {
                let (lo, hi) = (a0.min(b0), a1.max(b1));
                // The merged caret keeps the direction of the primary one, or of the first.
                let forward = if primary && !*last_primary {
                    c.pos >= c.anchor
                } else {
                    last.pos >= last.anchor
                };
                let (anchor, pos) = if forward { (lo, hi) } else { (hi, lo) };
                last.anchor = anchor;
                last.pos = pos;
                last.want = if primary { c.want } else { last.want };
                *last_primary |= primary;
                continue;
            }
        }
        out.push((c, primary));
    }
    *carets = out;
}

/// Where position `p` of the text is after an edit before it changed the line count from
/// `old_len`: `tail` is what followed `p` on its line, which the edit did not touch.
fn shift_past_edit(after: &[String], old_len: usize, p: edit::Pos, tail: &str) -> edit::Pos {
    let line = p.0 as isize + after.len() as isize - old_len as isize;
    if let Some(t) = usize::try_from(line).ok().and_then(|l| after.get(l)) {
        if t.ends_with(tail) {
            return (line as usize, t.len() - tail.len());
        }
    }
    edit::clamp(after, (line.max(0) as usize, p.1))
}

/// What a key or input event asks of the caret, in the order they came.
enum CaretInput {
    Text(String),
    Paste(String),
    Copy,
    Cut,
    Key(Key, Modifiers),
}

impl ColSel {
    fn rows(&self) -> (usize, usize) {
        let (a, b) = (self.anchor.0, self.head.0);
        (a.min(b), a.max(b))
    }

    fn cols(&self) -> std::ops::Range<usize> {
        let (a, b) = (self.anchor.1, self.head.1);
        a.min(b)..a.max(b)
    }
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub focus: bool,
    pub show_replace: bool,
    pub replace: String,
}

/// The query, match case, whole word and regular expression settings of a search.
type FindKey = (String, bool, bool, bool);

impl FindState {
    fn key(&self) -> FindKey {
        (
            self.query.clone(),
            self.match_case,
            self.whole_word,
            self.regex,
        )
    }
}

/// The pattern of a find query: the text itself unless `regex`, case-insensitive unless
/// `match_case`, and only at word boundaries with `whole_word`.
pub fn find_regex(
    query: &str,
    match_case: bool,
    whole_word: bool,
    regex: bool,
) -> Result<Regex, regex::Error> {
    let body = if regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let body = if whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    regex::RegexBuilder::new(&body)
        .case_insensitive(!match_case)
        .build()
}

/// Replaces every match of `re` in `lines`; `$1`-style groups are expanded only for regex
/// searches. Returns how many matches were replaced.
pub fn replace_all(lines: &mut [String], re: &Regex, with: &str, expand: bool) -> usize {
    let mut n = 0;
    for line in lines.iter_mut() {
        let hits = re.find_iter(line).filter(|m| !m.is_empty()).count();
        if hits == 0 {
            continue;
        }
        n += hits;
        *line = if expand {
            re.replace_all(line, with).into_owned()
        } else {
            re.replace_all(line, regex::NoExpand(with)).into_owned()
        };
    }
    n
}

struct Snapshot {
    /// The lines of each pane. Below the top of its stack, a step keeps only the lines between
    /// the ones it shares with the step above, which `kept` counts.
    lines: Vec<Vec<String>>,
    /// For each pane, how many of the first and the last lines of the step above this step
    /// has too, and were left out of `lines`.
    kept: Vec<(usize, usize)>,
    modified: Vec<bool>,
    saves: Vec<u64>,
    /// The format of each pane before this step, where the step changed it.
    formats: Vec<Option<DocumentFormat>>,
    /// The synchronization points before this step, so that undo brings back the ones whose
    /// lines the step deleted.
    sync_points: Vec<SyncPoint>,
    /// The carets, the primary one first, when there were several: undo and redo put them back.
    carets: Vec<Caret>,
}

impl Snapshot {
    /// Leaves out of each pane the first and last lines `above`, all of whose lines are kept,
    /// has too.
    fn share_with(&mut self, above: &Snapshot) {
        for (i, lines) in self.lines.iter_mut().enumerate() {
            let Some(other) = above.lines.get(i) else {
                continue;
            };
            let head = lines.iter().zip(other).take_while(|(a, b)| a == b).count();
            let room = lines.len().min(other.len()) - head;
            let tail = lines
                .iter()
                .rev()
                .zip(other.iter().rev())
                .take(room)
                .take_while(|(a, b)| a == b)
                .count();
            let len = lines.len();
            lines.truncate(len - tail);
            lines.drain(..head);
            lines.shrink_to_fit();
            self.kept[i] = (head, tail);
        }
    }

    /// Puts back the lines [`share_with`](Self::share_with) left out, from `above`.
    fn unshare(&mut self, above: &Snapshot) {
        for (i, lines) in self.lines.iter_mut().enumerate() {
            let (head, tail) = std::mem::take(&mut self.kept[i]);
            if head + tail == 0 {
                continue;
            }
            let other = &above.lines[i];
            let mut whole = Vec::with_capacity(head + lines.len() + tail);
            whole.extend_from_slice(&other[..head]);
            whole.append(lines);
            whole.extend_from_slice(&other[other.len() - tail..]);
            *lines = whole;
        }
    }

    /// The lines this step holds.
    fn size(&self) -> usize {
        self.lines.iter().map(Vec::len).sum()
    }
}

/// Puts `s` on top of `steps`; the step below then keeps only the lines it does not share
/// with `s`.
fn push_step(steps: &mut Vec<Snapshot>, s: Snapshot) {
    if let Some(below) = steps.last_mut() {
        below.share_with(&s);
    }
    steps.push(s);
}

/// How many of the oldest undo steps go, given the lines each step holds, oldest first. Steps
/// below the top keep only the lines they changed; past 200 steps, or once the steps that stay
/// below the top hold more than five million lines, the oldest go, keeping at least ten.
fn undo_overflow(sizes: &[usize]) -> usize {
    let mut drop = sizes.len().saturating_sub(200);
    let below_top = sizes.len().saturating_sub(1);
    let mut held: usize = sizes[drop.min(below_top)..below_top].iter().sum();
    while held > 5_000_000 && sizes.len() - drop > 10 {
        held -= sizes[drop];
        drop += 1;
    }
    drop
}

/// Takes the top step off `steps`, and makes the one below it whole again.
fn pop_step(steps: &mut Vec<Snapshot>) -> Option<Snapshot> {
    let s = steps.pop()?;
    if let Some(below) = steps.last_mut() {
        below.unshare(&s);
    }
    Some(s)
}

/// Save-format state shared by merge operations and their undo snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DocumentFormat {
    encoding: TextEncoding,
    bom: bool,
    eol: Eol,
    final_newline: bool,
    mixed_eol: bool,
    original_eols: Vec<Option<Eol>>,
    preserve_eols: bool,
}

impl DocumentFormat {
    fn restore(self, doc: &mut TextDoc) {
        doc.encoding = self.encoding;
        doc.bom = self.bom;
        doc.eol = self.eol;
        doc.final_newline = self.final_newline;
        doc.mixed_eol = self.mixed_eol;
        doc.original_eols = self.original_eols;
        doc.preserve_eols = self.preserve_eols;
    }
}

fn format_of(doc: &TextDoc) -> DocumentFormat {
    DocumentFormat {
        encoding: doc.encoding,
        bom: doc.bom,
        eol: doc.eol,
        final_newline: doc.final_newline,
        mixed_eol: doc.mixed_eol,
        original_eols: doc.original_eols.clone(),
        preserve_eols: doc.preserve_eols,
    }
}

/// Key column `column`: by its header name when there is a header row and the name is
/// usable, else by position.
fn key_column(names: &[String], column: usize, header: bool) -> KeyColumn {
    match names.get(column) {
        Some(name) if header && !name.is_empty() => KeyColumn::Name(name.clone()),
        _ => KeyColumn::Index(column),
    }
}

fn key_label(key: &KeyColumn, names: &[String]) -> String {
    match key {
        KeyColumn::Name(name) => name.clone(),
        KeyColumn::Index(i) => match names.get(*i) {
            Some(name) if !name.is_empty() => format!("{} ({name})", i + 1),
            _ => format!("{} {}", tr("table.column"), i + 1),
        },
    }
}

/// A numeric tolerance as typed: "0.01" is absolute, "1%" relative, empty is none.
fn parse_tolerance(text: &str) -> Option<(f64, f64)> {
    let text = text.trim();
    if text.is_empty() {
        return Some((0.0, 0.0));
    }
    let (number, relative) = match text.strip_suffix('%') {
        Some(n) => (n.trim(), true),
        None => (text, false),
    };
    let value = number
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)?;
    Some(if relative {
        (0.0, value / 100.0)
    } else {
        (value, 0.0)
    })
}

fn tolerance_text(m: &TableMatch) -> String {
    // Rounded to 12 digits, so 7% is not shown as 7.000000000000001%.
    let short = |x: f64| format!("{x:.11e}").parse::<f64>().unwrap_or(x).to_string();
    if m.rel_tolerance > 0.0 {
        format!("{}%", short(m.rel_tolerance * 100.0))
    } else if m.abs_tolerance > 0.0 {
        short(m.abs_tolerance)
    } else {
        String::new()
    }
}

fn problem_text(problem: &table_match::Problem) -> String {
    use table_match::Problem;
    match problem {
        Problem::MissingKey { side, column } => trf(
            "table.err_missing_key",
            &[
                ("column", column.clone()),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
        Problem::DuplicateKeyColumn { side, column } => trf(
            "table.err_duplicate_key_column",
            &[
                ("column", column.clone()),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
        Problem::NoHeader => tr("table.err_no_header").to_string(),
        Problem::TooLarge => trf(
            "table.err_too_large",
            &[("count", table_match::MAX_RECORDS.to_string())],
        ),
        Problem::Cancelled => tr("table.err_cancelled").to_string(),
        Problem::Failed => tr("table.err_failed").to_string(),
    }
}

/// Records, both sides together, above which matching by key runs in the background.
const BACKGROUND_RECORDS: usize = 100_000;

/// Matches the records of `a` and `b` by the key columns of `m`, comparing cells with the
/// compare options; `stop` as in [`table_match::compare_with_stop`].
fn match_records(
    a: &table::Table,
    b: &table::Table,
    m: &TableMatch,
    opts: &DiffOptions,
    stop: &dyn Fn(usize) -> bool,
) -> Result<TableDiff, table_match::Problem> {
    let normalizer = diff::Normalizer::new(opts);
    let clean = |s: &str| {
        if opts.ignore_eol {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.to_string()
        }
    };
    let key = |s: &str| normalizer.key(&clean(s)).into_owned();
    let eq = |x: &str, y: &str| normalizer.close(&clean(x), &clean(y));
    let compare = table_match::Compare { key: &key, eq: &eq };
    table_match::compare_with_stop(a, b, m, &compare, stop)
}

fn warning_text(warning: &table_match::Warning) -> String {
    use table_match::Warning;
    match warning {
        Warning::DuplicateHeader { side, column } => trf(
            "table.warn_duplicate_header",
            &[
                ("column", column.clone()),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
        Warning::UnpairedColumns { side, columns } => trf(
            "table.warn_unpaired",
            &[
                ("columns", columns.join(", ")),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
        Warning::DuplicateKeys { side, records } => trf(
            "table.warn_duplicate_keys",
            &[
                ("count", records.to_string()),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
        Warning::EmptyKeys { side, records } => trf(
            "table.warn_empty_keys",
            &[
                ("count", records.to_string()),
                ("side", side_name(*side, 2).to_string()),
            ],
        ),
    }
}

/// The marker, description and edge colour of a row matched by key.
fn row_marker(m: &table_match::MatchedRow, c: &Colors) -> (String, String, Option<Color32>) {
    let (mark, key, fill) = match m.state {
        RowState::Identical => ("", None, None),
        RowState::Added => ("+", Some("table.state_added"), Some(c.change_bar)),
        RowState::Removed => ("−", Some("table.state_removed"), Some(c.change_bar)),
        RowState::Modified => ("~", Some("table.state_modified"), Some(c.change_bar)),
        RowState::Ambiguous => ("?", Some("table.state_ambiguous"), Some(c.conflict_bar)),
    };
    let mut tips: Vec<&str> = Vec::new();
    if m.header {
        tips.push(tr("table.state_header"));
    }
    tips.extend(key.map(tr));
    let mut mark = mark.to_string();
    if m.moved {
        mark.push('↕');
        tips.push(tr("table.state_moved"));
    }
    (mark, tips.join(" · "), fill.or(m.moved.then_some(c.moved)))
}

/// The source of `dst` with the changed cells of record `s` of `src` copied into record `t`,
/// or `None` when a changed cell has no counterpart on the other side.
fn copy_record_cells(
    d: &TableDiff,
    r: &table_match::MatchedRow,
    (src, s, from): (&table::Table, usize, usize),
    (dst, t, to): (&table::Table, usize, usize),
    delim: char,
) -> Option<String> {
    let (a, b) = (&src.records[s], &dst.records[t]);
    if r.changed[to].iter().any(|&c| {
        d.paired_column(to, c, from)
            .is_none_or(|f| f >= a.cells.len())
    }) {
        return None;
    }
    let cells = r.changed[from]
        .iter()
        .map(|&c| {
            Some((
                d.paired_column(from, c, to)
                    .filter(|&t| t < b.cells.len())?,
                a.cells[c].as_str(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    dst.replace_cells(t, &cells, delim).ok()
}

/// Makes the merge result pane (the middle of three, else the right one) save to `out`.
/// It counts as modified when `out` is another file, so saving always writes the result.
pub fn redirect_output(docs: &mut [TextDoc], out: &std::path::Path) {
    let target = if docs.len() == 3 { 1 } else { docs.len() - 1 };
    let d = &mut docs[target];
    if d.description.is_none() {
        d.description = Some(format!("{} → {}", d.title(), out.display()));
    }
    if d.path.as_deref() != Some(out) {
        d.modified = true;
    }
    d.path = Some(out.to_path_buf());
    d.read_only = d.compare_only;
}

/// Unchanged lines kept visible around each difference when folding.
const FOLD_CONTEXT: usize = 3;
/// Shorter unchanged runs are not worth folding.
const MIN_FOLD: usize = 4;
const SCROLL_ANIM_SECS: f64 = 0.18;

/// One line on screen: a wrapped segment of a diff row, or a fold of unchanged rows.
#[derive(Clone, Copy, PartialEq, Debug)]
enum VLine {
    Row(usize, u32),
    /// Rows `start..end` hidden behind a clickable bar.
    Fold(usize, usize),
}

/// Copy direction on a divider: from pane, to pane, icon, tooltip key.
type MergeArrow = (usize, usize, Icon, &'static str);

#[derive(Default)]
struct Layout {
    /// (wrap columns or 0, tab size, folding, layout generation, spacing) of these lines.
    key: Option<(usize, usize, bool, u64, TextLayout)>,
    lines: Vec<VLine>,
    /// First display line of every diff row (the fold line for hidden rows).
    row_line: Vec<usize>,
    panes: Vec<PaneLayout>,
}

/// Per-pane display lines and their mapping to shared comparison rows.
#[derive(Default)]
struct PaneLayout {
    /// Shared display boundary -> compact pane boundary, including the end sentinel.
    boundaries: Vec<usize>,
    /// Compact line -> shared display line, for pointer and keyboard navigation.
    shared: Vec<usize>,
}

impl PaneLayout {
    fn position(&self, shared: f32) -> f32 {
        let i = (shared.floor() as usize).min(self.boundaries.len().saturating_sub(1));
        let start = self.boundaries[i];
        let end = self.boundaries.get(i + 1).copied().unwrap_or(start);
        start as f32 + shared.fract() * (end - start) as f32
    }
}

/// A sampled cubic strip supports both an insertion's zero-height endpoint and
/// unequal block heights without relying on convex polygon triangulation.
fn paint_link(
    painter: &egui::Painter,
    left_x: f32,
    right_x: f32,
    left: (f32, f32),
    right: (f32, f32),
    fill: Color32,
    edge: Color32,
) {
    const STEPS: usize = 16;
    let mut mesh = egui::Mesh::default();
    let mut upper = Vec::with_capacity(STEPS + 1);
    let mut lower = Vec::with_capacity(STEPS + 1);
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        let ease = t * t * (3.0 - 2.0 * t);
        let x = egui::lerp(left_x..=right_x, t);
        let top = pos2(x, egui::lerp(left.0..=right.0, ease));
        let bottom = pos2(x, egui::lerp(left.1..=right.1, ease));
        upper.push(top);
        lower.push(bottom);
        mesh.colored_vertex(top, fill);
        mesh.colored_vertex(bottom, fill);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k, k - 1, k + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
    painter.add(egui::Shape::line(upper, Stroke::new(1.0_f32, edge)));
    painter.add(egui::Shape::line(lower, Stroke::new(1.0_f32, edge)));
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD)
}

/// Very long lines are cut before layout to keep painting fast.
fn clip_line(text: &str) -> &str {
    if text.len() <= 8000 {
        return text;
    }
    let mut end = 8000;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The first `max` characters of `text`, with an ellipsis when it is longer.
fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// A semantic report of two files, or of three with the middle one as the base.
enum Structured {
    Two(crate::core::semantic::SemanticReport),
    Three(crate::core::semantic::three_way::ThreeWayReport),
}

impl Structured {
    fn options(&self) -> &crate::core::semantic::Options {
        match self {
            Structured::Two(r) => &r.options,
            Structured::Three(r) => &r.options,
        }
    }
}

/// What a click in the semantic view asks for.
enum SemanticAction {
    /// Show this line of this pane in the text view.
    Jump(usize, usize),
    /// Copy the value of a change from this side to the other.
    Copy(Box<crate::core::semantic::Change>, usize),
    /// Take the value of an entry of a three-way report from the left (0) or right (1) pane
    /// into the middle one.
    Take(Box<crate::core::semantic::three_way::MergeEntry>, usize),
}

/// What a change does, and where it was moved from.
fn change_label(change: &crate::core::semantic::Change) -> String {
    use crate::core::semantic::ChangeKind;
    let kind = tr(match change.kind {
        ChangeKind::Added => "analysis.added",
        ChangeKind::Removed => "analysis.removed",
        ChangeKind::Modified => "analysis.modified",
        ChangeKind::TypeChanged => "analysis.type_changed",
        ChangeKind::Moved => "analysis.moved",
    });
    let label = match &change.from {
        Some(from) => format!(
            "{kind} · {}",
            trf("analysis.moved_from", &[("path", shorten(from, 240))])
        ),
        None => kind.to_owned(),
    };
    if change.ambiguous {
        format!("{label} · {}", tr("analysis.ambiguous"))
    } else {
        label
    }
}

/// The values of a change, how sure its pairing is, and links to where it is in panes `panes`
/// (of its first and second side).
fn change_details(
    ui: &mut Ui,
    change: &crate::core::semantic::Change,
    panes: [usize; 2],
    out: &mut Option<SemanticAction>,
) {
    ui.monospace(format!(
        "{} → {}",
        shorten(change.before.as_deref().unwrap_or("∅"), 512),
        shorten(change.after.as_deref().unwrap_or("∅"), 512),
    ));
    let paired = match change.confidence.as_str() {
        "array_key" => Some("analysis.paired_by_key"),
        "reordered" => Some("analysis.paired_by_reordering"),
        "position" => Some("analysis.paired_by_position"),
        _ => None,
    };
    if let Some(key) = paired {
        ui.small(tr(key));
    }
    if change.ambiguous {
        ui.colored_label(widgets::pal(ui).warning, tr("analysis.ambiguous_hint"));
    }
    ui.horizontal_wrapped(|ui| {
        for (side, span) in change.spans.iter().enumerate() {
            if let Some(span) = span {
                let pane = panes[side];
                let link = format!("{} {}:{}", tr("analysis.source"), pane + 1, span.line);
                if ui.button(link).clicked() {
                    *out = Some(SemanticAction::Jump(pane, span.line - 1));
                }
            }
        }
    });
}

/// The changes of a two-way semantic report under `node`, each a section with its values,
/// links to where it is in each file and, to a pane in `writable`, a button copying it there.
fn semantic_tree_ui(
    ui: &mut Ui,
    node: &crate::core::semantic::PathTree,
    depth: usize,
    writable: [bool; 2],
    out: &mut Option<SemanticAction>,
) {
    use crate::core::semantic::ChangeKind;
    for change in &node.changes {
        egui::CollapsingHeader::new(change_label(change))
            .id_salt(&change.id)
            .default_open(true)
            .show(ui, |ui| {
                change_details(ui, change, [0, 1], out);
                if change.kind == ChangeKind::Moved {
                    return;
                }
                ui.horizontal(|ui| {
                    for (to, key) in [(0, "analysis.copy_to_left"), (1, "analysis.copy_to_right")] {
                        if writable[to] && ui.button(tr(key)).clicked() {
                            *out = Some(SemanticAction::Copy(Box::new((*change).clone()), 1 - to));
                        }
                    }
                });
            });
    }
    for child in &node.children {
        egui::CollapsingHeader::new(format!("{} ({})", shorten(&child.label, 240), child.count))
            .id_salt(&child.label)
            .default_open(depth < 4 || child.count <= 20)
            .show(ui, |ui| {
                semantic_tree_ui(ui, child, depth + 1, writable, out)
            });
    }
}

/// The entries of a three-way semantic report: what each side changed at a path of the middle
/// one and, when it can be written, buttons taking a side's value into it.
fn three_way_ui(
    ui: &mut Ui,
    entries: &[&crate::core::semantic::three_way::MergeEntry],
    writable: bool,
    out: &mut Option<SemanticAction>,
) {
    use crate::core::semantic::three_way::MergeState;
    for entry in entries {
        let state = tr(match entry.state {
            MergeState::Left => "analysis.state_left",
            MergeState::Right => "analysis.state_right",
            MergeState::Both => "analysis.state_both",
            MergeState::Conflict => "analysis.state_conflict",
        });
        let header = egui::RichText::new(format!("{state} · {}", shorten(&entry.path, 240)));
        let header = if entry.state == MergeState::Conflict {
            header.color(widgets::pal(ui).danger)
        } else {
            header
        };
        egui::CollapsingHeader::new(header)
            .id_salt(("three_way", &entry.path))
            .default_open(true)
            .show(ui, |ui| {
                for (side, changes, pane, name) in [
                    (0, &entry.left, 0, "side.left"),
                    (1, &entry.right, 2, "side.right"),
                ] {
                    if changes.is_empty() {
                        continue;
                    }
                    ui.label(egui::RichText::new(tr(name)).strong());
                    for change in changes {
                        ui.indent(("change", side, &change.id), |ui| {
                            ui.label(format!(
                                "{} · {}",
                                change_label(change),
                                shorten(&change.path, 240)
                            ));
                            change_details(ui, change, [1, pane], out);
                        });
                    }
                }
                if !writable {
                    return;
                }
                ui.horizontal(|ui| {
                    for (side, changes, key) in [
                        (0, &entry.left, "analysis.use_left"),
                        (1, &entry.right, "analysis.use_right"),
                    ] {
                        let both = entry.state == MergeState::Both;
                        if !changes.is_empty()
                            && !(both && side == 1)
                            && ui.button(tr(key)).clicked()
                        {
                            *out = Some(SemanticAction::Take(Box::new((*entry).clone()), side));
                        }
                    }
                });
            });
    }
}

/// Display columns where each wrapped segment of `text` starts, breaking after
/// whitespace when possible. Wide characters take two columns.
fn wrap_starts(text: &str, tab: usize, cols: usize) -> Vec<usize> {
    let tab = tab.max(1);
    let mut starts = vec![0];
    let (mut col, mut tcol, mut seg, mut brk) = (0, 0, 0, 0);
    for ch in clip_line(text).chars() {
        let (w, n, space) = match ch {
            '\t' => (1, tab - tcol % tab, true),
            ' ' => (1, 1, true),
            c => (if is_wide(c) { 2 } else { 1 }, 1, false),
        };
        tcol += n;
        for _ in 0..n {
            if col + w > seg + cols {
                seg = if brk > seg { brk } else { col };
                starts.push(seg);
            }
            col += w;
            if space {
                brk = col;
            }
        }
    }
    starts
}

/// Each character of `text` that wrapping lays out, with its byte and the display columns it
/// covers, counted as [`wrap_starts`] counts them (a tab to its stop, a wide character as two).
fn wrap_columns(text: &str, tab: usize) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
    let tab = tab.max(1);
    let (mut col, mut tcol) = (0, 0);
    clip_line(text).char_indices().map(move |(i, ch)| {
        let (w, n) = match ch {
            '\t' => (1, tab - tcol % tab),
            c => (if is_wide(c) { 2 } else { 1 }, 1),
        };
        let start = col;
        col += w * n;
        tcol += n;
        (i, start..col)
    })
}

/// The display column (as [`wrap_starts`] counts) where byte `at` of `text` starts.
fn wrap_col(text: &str, tab: usize, at: usize) -> usize {
    let mut end = 0;
    for (i, cols) in wrap_columns(text, tab) {
        if i >= at {
            return cols.start;
        }
        end = cols.end;
    }
    end
}

/// The byte of `text` where the character covering display column `col` starts, or the end.
fn wrap_byte(text: &str, tab: usize, col: usize) -> usize {
    wrap_columns(text, tab)
        .find(|(_, cols)| col < cols.end)
        .map_or(text.len(), |(i, _)| i)
}

/// The wrapped display line of `starts` that display column `col` is on.
fn wrap_segment(starts: &[usize], col: usize) -> usize {
    starts.partition_point(|&s| s <= col).saturating_sub(1)
}

/// X offsets inside `galley` of the display columns in `starts`.
fn segment_x(galley: &Galley, starts: &[usize]) -> Vec<f32> {
    let mut out = Vec::with_capacity(starts.len());
    let mut next = starts.iter().peekable();
    let mut col = 0;
    for g in galley.rows.iter().flat_map(|r| &r.glyphs) {
        while next.next_if(|&&s| s <= col).is_some() {
            out.push(g.pos.x);
        }
        col += if is_wide(g.chr) { 2 } else { 1 };
    }
    out.extend(next.map(|_| galley.size().x));
    out
}

#[derive(Clone, Copy)]
enum MenuAction {
    Copy(usize, usize, usize),
    Edit(usize, usize),
    InsertBelow(usize, usize),
    DeleteLine(usize, usize),
    CopyText(usize, usize),
    /// Copy the selected lines from one pane to another.
    CopyLines(usize, usize),
    ToggleBookmark(usize, usize),
    EditNote(usize, usize),
    TakeBoth(usize, bool),
    AddRule(RuleKind, usize, usize),
    SyncPoint(usize, usize),
    ClearSyncPoints,
}

/// A bookmark comment being edited: pane, line, text, and whether the field needs focus.
type NoteEdit = (usize, usize, String, bool);

struct CellEdit {
    pane: usize,
    record: usize,
    column: usize,
    value: String,
    source: String,
    focus: bool,
}

#[derive(Default)]
struct TableState {
    docs: Vec<Result<Arc<table::Table>, String>>,
    diff: DiffResult,
    edit: Option<CellEdit>,
    column: usize,
    /// Rows matched by key (2-way, with key columns chosen), or why they could not be;
    /// `diff` then holds the same rows.
    keyed: Option<Box<Result<TableDiff, table_match::Problem>>>,
    /// Matching by key of large tables; `diff` is empty until it ends.
    job: Option<super::analysis::Task<Result<TableDiff, table_match::Problem>>>,
    /// The lines of the row selected when `job` started, to select it again.
    restore: Option<[Option<usize>; 3]>,
    /// Overrides [`BACKGROUND_RECORDS`].
    background_at: Option<usize>,
    /// The selected row of `diff`.
    selected: Option<usize>,
    /// Text of the numeric tolerance field.
    tolerance: String,
}

/// A line of the inline view.
#[derive(Clone, Copy)]
enum ULine {
    /// Row `row` as pane `pane` has it; `change` for a removed (left) or added (right) line.
    Line {
        row: usize,
        pane: usize,
        change: bool,
    },
    /// Unchanged rows `lo..hi`, folded away.
    Fold(usize, usize),
}

/// The panels of the side drawer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextPanel {
    Review,
    Verification,
}

/// What a file is shown as in the Formatted view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormattedKind {
    Document,
    Workbook,
    /// A PDF file, read from the structure it is tagged with.
    Pdf,
}

/// Whether `path` can be shown in the Formatted view, and as what.
fn formatted_kind(path: &std::path::Path) -> Option<FormattedKind> {
    match text::extension(path).as_str() {
        "docx" => Some(FormattedKind::Document),
        "xlsx" | "xlsm" => Some(FormattedKind::Workbook),
        "pdf" => Some(FormattedKind::Pdf),
        _ => None,
    }
}

/// A file's size and time of last change, when they can be read.
type FileStamp = Option<(u64, Option<SystemTime>)>;

/// What a Formatted view was made from: each file as it was on disk, and the compare options.
#[derive(Clone, PartialEq)]
struct FormattedKey {
    files: Vec<(std::path::PathBuf, FileStamp)>,
    opts: DiffOptions,
}

/// The Formatted view of Word documents.
#[derive(Default)]
struct FormattedSession {
    /// The view and what it was made from, or why it could not be made.
    view: Option<(FormattedKey, Result<MarkdownView, String>)>,
    /// The view being made, and what from.
    job: Option<(FormattedKey, super::analysis::Task<Prepared>)>,
}

impl FormattedKey {
    fn of(paths: &[std::path::PathBuf], opts: &DiffOptions) -> Self {
        FormattedKey {
            files: paths
                .iter()
                .map(|p| {
                    let meta = std::fs::metadata(p).ok();
                    (p.clone(), meta.map(|m| (m.len(), m.modified().ok())))
                })
                .collect(),
            opts: opts.clone(),
        }
    }
}

pub struct TextView {
    /// Whether the side drawer is open, and the panel it shows.
    pub drawer_open: bool,
    pub drawer_tab: TextPanel,
    pub review_decision: Option<Box<crate::core::review_state::Decision>>,
    pub review_dirty: bool,
    semantic_mode: bool,
    semantic_page: usize,
    semantic_cache: Option<(u64, Structured)>,
    semantic_job: Option<(u64, super::analysis::Task<Structured>)>,
    semantic_filter: Option<crate::core::semantic::ChangeKind>,
    semantic_state_filter: Option<crate::core::semantic::three_way::MergeState>,
    /// Why the last value was not copied.
    semantic_message: Option<String>,
    /// The array matching key of the semantic view; empty compares arrays by position.
    semantic_array_key: String,
    /// The Tree view of two XML files, which takes the place of the text panes.
    tree_mode: bool,
    /// Whether the Tree view was shown last frame, so difference commands step through it.
    tree_active: bool,
    /// The comparison and state of the Tree view.
    pub tree: Box<super::xml_tree_view::TreeSession>,
    /// The Formatted view of Word documents, which takes the place of the text panes.
    formatted_mode: bool,
    /// Whether the Formatted view was shown last frame, so difference commands step through it.
    formatted_active: bool,
    /// The Formatted view and the one being made.
    formatted: Box<FormattedSession>,
    merge_validation_generation: Option<u64>,
    merge_freshness: Option<(u64, bool)>,
    merge_preview_job: Option<(
        u64,
        super::analysis::Task<crate::core::merge_validation::MergePlan>,
    )>,
    merge_preview: Option<Box<crate::core::merge_validation::MergePlan>>,
    merge_validation: Option<Box<crate::core::merge_validation::MergeValidationReport>>,

    pub panes: Vec<Pane>,
    pub diff: DiffResult,
    opts: DiffOptions,
    frozen_options: Option<Box<DiffOptions>>,
    pub cur_block: Option<usize>,
    scroll: f32,
    hscroll: f32,
    pub active_pane: usize,
    pub cursor_row: Option<usize>,
    /// Other end of the selected rows; the cursor row is one end.
    sel_anchor: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    inline_cache: HashMap<usize, [Vec<Range<usize>>; 3]>,
    edit: Option<EditState>,
    /// The column selection made by Alt+dragging, if any (boxed: it is rare).
    pub col_sel: Option<Box<ColSel>>,
    /// The text caret, once a pane was clicked (boxed to keep the view small).
    caret: Option<Box<CaretState>>,
    /// Whether the last edit was typing in a line, which the next keystroke joins as one undo
    /// step.
    typing_group: bool,
    /// Whether to scroll sideways to the caret when it is next drawn.
    caret_reveal: bool,
    /// The view by source line from before the comparison was last computed again, until the
    /// rows and the layout are rebuilt.
    held: Option<Box<HeldView>>,
    pub find: FindState,
    /// The compiled find query, rebuilt when the query or its options change.
    /// The compiled find query (boxed: the view is kept small, and it is rare).
    find_re: Option<Box<(FindKey, Result<Regex, String>)>>,
    pub goto: Option<String>,
    pub table_mode: bool,
    pub table_delim: Option<char>,
    /// How the table view matches records: by position, or by key columns (2-way).
    pub table_match: TableMatch,
    col_widths: Vec<usize>,
    table: Box<TableState>,
    pub status: String,
    /// Message about an edit that changed many differences at once, for an undo toast.
    pub bulk_edit: Option<String>,
    scroll_to: Option<usize>,
    max_cols: usize,
    /// How long the last comparison took, in milliseconds.
    rediff_ms: f32,
    /// When typing put off comparing again: the time it is done by, unless something else
    /// needs the comparison first ([`Self::settle_diff`]).
    rediff_due: Option<f64>,
    /// The input time of the frame being drawn.
    now: f64,
    ctx_target: Option<(usize, usize)>,
    page_rows: usize,
    /// Modified panes whose file was changed by another program.
    disk_changed: Vec<usize>,
    layout: Layout,
    /// Bumped whenever rows or folds change so the layout is rebuilt.
    layout_gen: u64,
    text_layout: TextLayout,
    /// Folds (by first hidden row) the user opened.
    expanded: HashSet<usize>,
    pub collapse: bool,
    pub wrap: bool,
    /// Animated scroll: (from, to, start time).
    scroll_anim: Option<(f32, f32, f64)>,
    /// Set when difference navigation ran past the last (true) or first (false) difference,
    /// so the app can move on to the next or previous file of the folder compare.
    pub step_file: Option<bool>,
    /// Set whenever a pane was written to disk by [`Self::save_pane`], however that was
    /// triggered (the `Cmd::Save` command, but also the merge view's own "Save result"
    /// button, which calls it directly). The app checks this every frame to repack the
    /// archive this tab was opened from, if any, even when that bypassed `Cmd::Save`.
    pub saved: bool,
    /// Set when next (true) or previous conflict ran past the last or first open conflict.
    pub step_conflict: Option<bool>,
    /// Bookmarked lines of each pane, with their comments (empty when there is none).
    pub bookmarks: Vec<BTreeMap<usize, String>>,
    /// Lines the compare puts on one row (see [`SyncPoint`]), in order.
    pub sync_points: Vec<SyncPoint>,
    /// The line picked in each pane for a synchronization point being set.
    sync_pending: Vec<Option<usize>>,
    note_edit: Option<NoteEdit>,
    /// Off for a preview that must leave the keyboard to the view around it.
    pub keys_enabled: bool,
    /// Whether edited lines are marked, so they are only worked out when shown.
    edit_markers: bool,
    tab_size: usize,
    /// Compare rules file of this compare (a name or path, "" = none); `None` follows the
    /// options.
    pub rules: Option<String>,
    rule_set: Option<RuleSet>,
    /// A rule to add, picked from the context menu, until the app takes it.
    pub rule_request: Option<RuleDraft>,
    /// Whether the HTML report was asked for from the toolbar, until the app takes it.
    pub export_request: bool,
    /// Conflicts accepted as they are, by their content (see [`Self::block_key`]).
    resolved: HashSet<u64>,
    /// See [`Self::open_conflicts`]: the layout generation, number of resolved conflicts and
    /// of blocks it was worked out for, and whether each block is an open conflict.
    open_cache: std::cell::RefCell<((u64, usize, usize), Vec<bool>)>,
    /// 2-way: both files in one column instead of side by side.
    pub unified: bool,
    /// The lines of the inline view, with the layout generation, folding and row count they
    /// were worked out for.
    unified_lines: Option<((u64, bool, usize), Vec<ULine>)>,
    /// How far the inline view is scrolled and how tall it is, as last drawn.
    unified_shown: (f32, f32),
    /// Whether the inline view was the one last drawn.
    shown_unified: bool,
    id: egui::Id,
}

impl TextView {
    pub fn new(docs: Vec<TextDoc>, cfg: &Config) -> Self {
        let comparison = cfg.comparison_options();
        let table_delim = docs
            .iter()
            .find_map(|d| table::delimiter_of_doc(d, &comparison));
        let bookmarks = vec![BTreeMap::new(); docs.len()];
        let mut v = TextView {
            drawer_open: false,
            drawer_tab: TextPanel::Review,
            review_decision: None,
            review_dirty: false,
            semantic_mode: false,
            semantic_page: 0,
            semantic_cache: None,
            semantic_job: None,
            semantic_filter: None,
            semantic_state_filter: None,
            semantic_message: None,
            semantic_array_key: String::new(),
            tree_mode: false,
            tree_active: false,
            tree: Default::default(),
            formatted_mode: false,
            formatted_active: false,
            formatted: Default::default(),
            merge_validation_generation: None,
            merge_freshness: None,
            merge_preview_job: None,
            merge_preview: None,
            merge_validation: None,

            panes: docs.into_iter().map(Pane::new).collect(),
            diff: DiffResult::default(),
            opts: cfg.comparison_options(),
            frozen_options: None,
            cur_block: None,
            scroll: 0.0,
            hscroll: 0.0,
            active_pane: 0,
            cursor_row: None,
            sel_anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            inline_cache: HashMap::new(),
            edit: None,
            col_sel: None,
            caret: None,
            typing_group: false,
            caret_reveal: false,
            held: None,
            find: FindState::default(),
            find_re: None,
            goto: None,
            table_mode: table_delim.is_some(),
            table_delim,
            table_match: TableMatch::default(),
            col_widths: Vec::new(),
            table: Box::default(),
            status: String::new(),
            bulk_edit: None,
            scroll_to: None,
            max_cols: 0,
            rediff_ms: 0.0,
            rediff_due: None,
            now: 0.0,
            ctx_target: None,
            page_rows: 30,
            disk_changed: Vec::new(),
            layout: Layout::default(),
            layout_gen: 0,
            text_layout: cfg.text_layout,
            expanded: HashSet::new(),
            collapse: cfg.collapse_unchanged,
            wrap: cfg.word_wrap,
            scroll_anim: None,
            step_file: None,
            saved: false,
            step_conflict: None,
            sync_pending: vec![None; bookmarks.len()],
            bookmarks,
            sync_points: Vec::new(),
            note_edit: None,
            keys_enabled: true,
            edit_markers: cfg.show_edit_markers,
            tab_size: cfg.tab_size,
            rules: None,
            rule_set: None,
            rule_request: None,
            export_request: false,
            resolved: HashSet::new(),
            open_cache: Default::default(),
            unified: false,
            unified_lines: None,
            unified_shown: (0.0, 0.0),
            shown_unified: false,
            id: egui::Id::unique(("textview", std::time::Instant::now())),
        };
        v.apply_prettify();
        v.rediff();
        v.first_diff();
        v
    }

    pub fn n(&self) -> usize {
        self.panes.len()
    }

    pub fn title(&self) -> String {
        let names: Vec<String> = self
            .panes
            .iter()
            .map(|p| {
                let name = match (&p.doc.description, &p.doc.path) {
                    (Some(d), _) => d.clone(),
                    (None, Some(path)) => path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    (None, None) => tr("untitled").to_string(),
                };
                if p.doc.modified {
                    format!("{name}*")
                } else {
                    name
                }
            })
            .collect();
        names.join(" ↔ ")
    }

    /// Whether there are unsaved changes, counting a line still being typed.
    pub fn is_modified(&self) -> bool {
        self.panes.iter().any(|p| p.doc.modified)
            || self.table.edit.as_ref().is_some_and(|e| {
                self.table
                    .docs
                    .get(e.pane)
                    .and_then(|t| t.as_ref().ok())
                    .and_then(|t| t.records.get(e.record))
                    .and_then(|r| r.cells.get(e.column))
                    != Some(&e.value)
            })
            || self
                .edit
                .as_ref()
                .is_some_and(|e| self.edit_original(e) != e.text)
    }

    /// Local overlays/editors get Escape before the tab-level close shortcut.
    pub fn handles_escape(&self) -> bool {
        let editing = !self.shows_inline() && (self.col_sel.is_some() || self.caret.is_some());
        self.find.open
            || self.goto.is_some()
            || self.edit.is_some()
            || editing
            || self.note_edit.is_some()
            || self.table.edit.is_some()
    }

    /// Takes in a line still being typed, as if it had been left.
    pub fn commit_pending_edit(&mut self) {
        if let Some(edit) = self.table.edit.take() {
            if let Err(error) = self.apply_cell_edit(&edit) {
                self.status = error;
            }
        }
        if let Some(e) = self.edit.take() {
            self.commit_edit(e);
        }
    }

    /// Uses the compare rules file `rules` ("" = none, `None` = the one of the options).
    pub fn set_rules(&mut self, rules: Option<String>) {
        self.rules = rules;
        self.reload_rules();
    }

    /// Reads the compare rules file again.
    pub fn reload_rules(&mut self) {
        self.rule_set = self.rules.as_deref().and_then(rules::find);
    }

    /// The options to compare with: `base` with this compare's rules, when it has its own.
    pub fn options_for(&self, base: &DiffOptions) -> Option<DiffOptions> {
        if let Some(options) = &self.frozen_options {
            return Some((**options).clone());
        }
        self.rules
            .as_ref()
            .map(|_| rules::options_with(base, self.rule_set.as_ref()))
    }

    /// A rule of `kind` taken from the line at row `r` of pane `p`, or from the selected lines.
    fn rule_draft(&self, kind: RuleKind, r: usize, p: usize) -> RuleDraft {
        let line = |row: usize, pane: usize| {
            let l = self.diff.rows.get(row)?.line(pane)?;
            self.panes[pane].doc.lines.get(l).map(String::as_str)
        };
        let text = line(r, p)
            .or_else(|| (0..self.n()).find_map(|q| line(r, q)))
            .unwrap_or("");
        // The line on every side of a row, so a pattern built from it matches all of them.
        let sides = |row: usize| -> Option<String> {
            let mut all: Vec<&str> = line(row, p).into_iter().collect();
            all.extend(
                (0..self.n())
                    .filter(|&q| q != p)
                    .filter_map(|q| line(row, q)),
            );
            (!all.is_empty()).then(|| rules::lines_pattern(&all))
        };
        let (a, b) = match kind {
            RuleKind::IgnoreLine => (
                sides(r).unwrap_or_else(|| rules::line_pattern(text)),
                String::new(),
            ),
            RuleKind::Substitute => {
                let other = (0..self.n())
                    .filter(|&q| q != p)
                    .find_map(|q| line(r, q).filter(|o| *o != text))
                    .unwrap_or(text);
                let s = rules::substitution_for(text, other);
                (s.find, s.replace)
            }
            RuleKind::IgnoreBlock => {
                let (r0, r1) = self
                    .selection()
                    .filter(|&(a, b)| (a..=b).contains(&r))
                    .unwrap_or((r, r));
                let first = (r0..=r1).find_map(|row| line(row, p).and(sides(row)));
                let last = (r0..=r1).rev().find_map(|row| line(row, p).and(sides(row)));
                let end = match (r1 > r0, last) {
                    (true, Some(l)) => l,
                    _ => String::new(),
                };
                (first.unwrap_or_else(|| rules::line_pattern(text)), end)
            }
        };
        RuleDraft { kind, a, b }
    }

    pub fn freeze_options(&mut self, mut opts: DiffOptions) {
        opts.unpackers.clear();
        self.set_options(&opts);
        self.frozen_options = Some(Box::new(opts));
    }

    pub fn set_options(&mut self, opts: &DiffOptions) {
        if self.opts != *opts {
            self.invalidate_review();
            self.opts = opts.clone();
            self.apply_prettify();
            self.rediff();
        }
    }

    /// Shows JSON / XML panes reformatted while the option is on, and the loaded text otherwise.
    /// Panes with unsaved edits are left alone.
    fn apply_prettify(&mut self) {
        for (p, pane) in self.panes.iter_mut().enumerate() {
            if pane.doc.modified {
                continue;
            }
            let swap = if self.opts.prettify {
                match pane.original {
                    None => prettified(&pane.doc).map(|d| {
                        pane.original = Some(std::mem::replace(&mut pane.doc, d));
                    }),
                    Some(_) => None,
                }
            } else {
                pane.original.take().map(|o| pane.doc = o)
            };
            if swap.is_some() {
                pane.hl_dirty = true;
                pane.reset_baseline();
                self.bookmarks[p].clear();
                self.sync_pending[p] = None;
                self.sync_points.clear();
            }
        }
    }

    /// Comment syntax of the compared files, from the first pane whose language is known.
    fn comment_syntax(&self) -> Option<&'static comments::CommentSyntax> {
        self.panes.iter().find_map(|p| {
            p.doc
                .path
                .as_deref()
                .and_then(comments::for_path)
                .or_else(|| p.doc.syntax.as_deref().and_then(comments::for_extension))
        })
    }

    pub fn rediff(&mut self) {
        let started = std::time::Instant::now();
        self.rediff_due = None;
        self.rediff_inner();
        self.rediff_ms = started.elapsed().as_secs_f32() * 1000.0;
    }

    /// After typing within a line: compares again now when that is quick, else once typing
    /// pauses for [`TYPING_PAUSE`] or something else needs the comparison. The line count did not
    /// change, so until then the rows stay right and only the edited line's colors lag.
    fn rediff_soon(&mut self) {
        if self.rediff_ms < SLOW_REDIFF_MS {
            return self.rediff();
        }
        self.rediff_due = Some(self.now + TYPING_PAUSE);
        // The edited line is drawn from the text, so its layout must be made again.
        self.inline_cache.clear();
        self.layout_gen += 1;
    }

    /// Compares again now if typing put it off; every command that reads the comparison
    /// runs after this.
    pub fn settle_diff(&mut self) {
        if self.rediff_due.is_some() {
            self.rediff();
        }
    }

    /// The input of this frame: comparing again waits while it is only more typing, and is
    /// done before anything else (a click, a key that is not typing) or once typing pauses.
    fn settle_diff_for_input(&mut self, ctx: &egui::Context) {
        let (now, only_typing, typed) = ctx.input(|i| {
            let typing = |e: &egui::Event| match e {
                egui::Event::Text(_) | egui::Event::Ime(_) => true,
                egui::Event::PointerMoved(_) | egui::Event::ModifiersChanged(_) => true,
                egui::Event::Key { pressed: false, .. } => true,
                egui::Event::Key { key, modifiers, .. } => {
                    !modifiers.command
                        && !modifiers.ctrl
                        && (key.name().chars().count() == 1
                            || matches!(key, Key::Space | Key::Backspace | Key::Delete))
                }
                _ => false,
            };
            let typed = i.events.iter().any(|e| {
                matches!(e, egui::Event::Text(_) | egui::Event::Ime(_))
                    || matches!(e, egui::Event::Key { pressed: true, .. })
            });
            (i.time, i.events.iter().all(typing), typed)
        });
        self.now = now;
        let Some(due) = self.rediff_due else { return };
        if !only_typing || (!typed && now >= due) {
            self.rediff();
        } else {
            if typed {
                self.rediff_due = Some(now + TYPING_PAUSE);
            }
            let wait = self.rediff_due.map_or(0.0, |d| (d - now).max(0.0));
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }

    fn rediff_inner(&mut self) {
        self.hold_view();
        let syntax = self.comment_syntax();
        let refs: Vec<&[String]> = self.panes.iter().map(|p| p.doc.lines.as_slice()).collect();
        let lens: Vec<usize> = refs.iter().map(|l| l.len()).collect();
        self.sync_points = diff::valid_sync_points(&self.sync_points, &lens);
        // Do not hold the previous large alignment while allocating its replacement.
        self.diff = DiffResult::default();
        self.diff = diff::compute_synced(&refs, &self.opts, syntax, &self.sync_points);
        diff::include_doc_formats(
            &mut self.diff,
            &self.panes.iter().map(|p| &p.doc).collect::<Vec<_>>(),
            &self.opts,
        );
        for (p, marks) in self.bookmarks.iter_mut().enumerate() {
            let n = self.panes[p].doc.lines.len();
            marks.retain(|&l, _| l < n);
        }
        self.update_edited();
        self.recheck_review_hunks();
        self.inline_cache.clear();
        self.sel_anchor = None;
        self.restore_held_rows();
        let rows = self.diff.rows.len();
        let n = self.panes.len();
        if self
            .col()
            .is_some_and(|c| c.pane >= n || c.rows().1 >= rows)
        {
            self.col_sel = None;
        }
        if self.caret().is_some_and(|c| c.pane >= n) {
            self.drop_carets();
        }
        self.layout_gen += 1;
        if let Some(b) = self.cur_block {
            if b >= self.diff.blocks.len() {
                self.cur_block = None;
            }
        }
        self.max_cols = self
            .panes
            .iter()
            .flat_map(|p| p.doc.lines.iter())
            .map(|l| l.chars().count() + l.matches('\t').count() * 3)
            .max()
            .unwrap_or(0);
        if let Some(d) = self.table_delim {
            self.table.docs = self
                .panes
                .iter()
                .map(|p| table::Table::parse(table::source(&p.doc), d).map(Arc::new))
                .collect();
            // The records of the selected row, to select them again in the new rows; while a
            // match runs there are no rows, only the selection to make when it ends.
            let selected = match self.table.job {
                Some(_) => self.table.restore.take(),
                None => self
                    .table
                    .selected
                    .and_then(|r| self.table.diff.rows.get(r))
                    .map(|r| r.lines()),
            };
            self.table.keyed = None;
            self.table.job = None;
            if self.table.docs.iter().all(Result::is_ok) {
                if self.table_mode && self.table_match.by_key() && self.n() == 2 {
                    self.match_table_by_key(selected);
                }
                if self.table.job.is_none() && !matches!(self.table.keyed.as_deref(), Some(Ok(_))) {
                    self.table_by_position();
                }
            } else {
                self.table.diff = DiffResult::default();
            }
            self.select_table_lines(selected);
            let mut w: Vec<usize> = Vec::new();
            for p in &self.panes {
                for l in p.doc.lines.iter().take(20000) {
                    for (i, c) in table::split_row(l, d).iter().enumerate() {
                        if w.len() <= i {
                            w.push(3);
                        }
                        w[i] = w[i].max(c.chars().count().min(40));
                    }
                }
            }
            self.col_widths = w;
        }
    }

    /// The text changed: the file's review decision is stale. Its hunks' decisions are checked
    /// when the comparison is next computed ([`Self::recheck_review_hunks`]), so that only those
    /// whose hunk changed go stale.
    fn invalidate_review(&mut self) {
        if let Some(review) = &mut self.review_decision {
            review.stale = true;
            self.review_dirty = true;
        }
    }

    /// After the text of a reviewed file changed: a hunk decision goes stale when no hunk has its
    /// identity any more (its lines, the two lines around them or the options changed). One whose
    /// hunk is still there as it was stays as it is; a stale one never becomes current by itself.
    /// Past the size the review handles, every hunk decision goes stale.
    fn recheck_review_hunks(&mut self) {
        let Some(review) = self.review_decision.as_deref() else {
            return;
        };
        if !review.stale || review.hunks.values().all(|h| h.stale) {
            return;
        }
        let current: Option<std::collections::HashSet<String>> =
            (self.review_within_budget() && self.diff.blocks.len() <= 1000).then(|| {
                self.diff
                    .blocks
                    .iter()
                    .map(|b| self.review_hunk_id(b))
                    .collect()
            });
        let Some(review) = self.review_decision.as_deref_mut() else {
            return;
        };
        for (id, h) in &mut review.hunks {
            if !h.stale && current.as_ref().is_none_or(|ids| !ids.contains(id)) {
                h.stale = true;
                self.review_dirty = true;
            }
        }
    }
    fn mark_changed(&mut self, panes: &[usize]) {
        self.invalidate_review();
        // the rows and layout are still those of the text before the change
        self.hold_view();
        // Taken out for the remapping rather than copied: a pane can have millions of lines.
        let last = self.undo.pop();
        for &p in panes {
            self.panes[p].doc.modified = true;
            self.panes[p].hl_dirty = true;
            if let Some(before) = last.as_ref().map(|s| &s.lines[p]) {
                self.remap_bookmarks(p, before);
                self.remap_sync_points(p, before);
                self.remap_view(p, before);
            }
        }
        if let Some(s) = last {
            self.undo.push(s);
        }
        self.rediff();
    }

    /// Notes where the view is by source line, while the rows and the layout are still those of
    /// the text as it is shown; kept until the layout is rebuilt, so several changes before the
    /// next frame keep the first view.
    fn hold_view(&mut self) {
        if self.held.is_some() {
            return;
        }
        let rows = &self.diff.rows;
        let line_of = |r: usize, prefer: usize| {
            let row = rows.get(r)?;
            std::iter::once(prefer)
                .chain(0..self.n())
                .find_map(|p| row.line(p).map(|l| (p, l)))
        };
        let shown = self.layout.key.is_some_and(|k| k.3 == self.layout_gen)
            && self.layout.row_line.len() == rows.len();
        // The top keeps to a pane other than the active one when it can: that is the one an edit
        // usually changes.
        let other = (0..self.n()).find(|&p| p != self.active_pane).unwrap_or(0);
        // How far into its row the view is: the display lines of a wrapped line above the top,
        // and the part of a line scrolled past.
        let scroll = self.scroll.max(0.0);
        let top = shown
            .then(|| self.layout.lines.get(scroll as usize))
            .flatten()
            .and_then(|l| match *l {
                VLine::Row(r, _) | VLine::Fold(r, _) => {
                    let into = scroll - self.layout.row_line.get(r).map_or(0.0, |&v| v as f32);
                    line_of(r, other).map(|(p, l)| (p, l, into.max(0.0)))
                }
            });
        let cursor = self.cursor_row.and_then(|r| line_of(r, self.active_pane));
        let col = self.col().and_then(|c| {
            let at = |r: usize| rows.get(r).and_then(|row| row.line(c.pane));
            Some((c.pane, at(c.anchor.0)?, at(c.head.0)?))
        });
        self.held = Some(Box::new(HeldView { top, cursor, col }));
    }

    /// Moves the held view and the caret of pane `p` with its lines after they changed from
    /// `before`: a line that is still there keeps its place in it, and one that went lands on
    /// the nearest line after it.
    fn remap_view(&mut self, p: usize, before: &[String]) {
        let caret_here = self.caret().is_some_and(|c| c.pane == p);
        let held = self.held.as_deref().copied().unwrap_or_default();
        let in_pane = held.top.is_some_and(|t| t.0 == p)
            || held.cursor.is_some_and(|c| c.0 == p)
            || held.col.is_some_and(|c| c.0 == p);
        if !caret_here && !in_pane {
            return;
        }
        let after = &self.panes[p].doc.lines;
        let map = line_map(before, after, false);
        let last = after.len().saturating_sub(1);
        let line = |l: usize| map.get(l).map_or(last, |&(to, _)| to.min(last));
        let pos = |(l, b): edit::Pos| match map.get(l) {
            Some(&(to, true)) => (to, b),
            Some(&(to, false)) => (to.min(last), 0),
            None => (last, usize::MAX),
        };
        if let Some(h) = &mut self.held {
            if let Some(t) = h.top.as_mut().filter(|t| t.0 == p) {
                t.1 = line(t.1);
            }
            if let Some(c) = h.cursor.as_mut().filter(|c| c.0 == p) {
                c.1 = line(c.1);
            }
            if let Some(c) = h.col.as_mut().filter(|c| c.0 == p) {
                (c.1, c.2) = (line(c.1), line(c.2));
            }
        }
        if let Some(cs) = self.caret.as_deref_mut().filter(|cs| cs.at.pane == p) {
            for k in std::iter::once(&mut cs.at).chain(cs.others.iter_mut()) {
                k.pos = edit::clamp(after, pos(k.pos));
                k.anchor = edit::clamp(after, pos(k.anchor));
            }
        }
    }

    /// Points the cursor row and the column selection at the rows their held lines now have.
    fn restore_held_rows(&mut self) {
        let Some(h) = self.held.as_mut() else { return };
        let (cursor, col) = (h.cursor.take(), h.col.take());
        if self.diff.rows.is_empty() {
            return;
        }
        let row = |p: usize, l: usize| self.diff.row_of_line(p, l);
        if let Some((p, l)) = cursor {
            self.cursor_row = Some(row(p, l));
        }
        if let (Some((p, a, b)), Some(s)) = (col, self.col_sel.as_deref_mut()) {
            if s.pane == p {
                let (ra, rb) = (self.diff.row_of_line(p, a), self.diff.row_of_line(p, b));
                s.anchor.0 = ra;
                s.head.0 = rb;
            }
        }
        if let Some(c) = self.caret.as_deref_mut() {
            let lines = &self.panes[c.at.pane].doc.lines;
            for k in std::iter::once(&mut c.at).chain(c.others.iter_mut()) {
                k.pos = edit::clamp(lines, k.pos);
                k.anchor = edit::clamp(lines, k.anchor);
            }
        }
    }

    /// Moves the bookmarks of pane `p` along with its lines after they changed from `before`.
    fn remap_bookmarks(&mut self, p: usize, before: &[String]) {
        let editing = self.note_edit.as_ref().is_some_and(|n| n.0 == p);
        if self.bookmarks[p].is_empty() && !editing {
            return;
        }
        let map = line_map(before, &self.panes[p].doc.lines, false);
        if editing {
            match self.note_edit.as_ref().and_then(|n| map.get(n.1).copied()) {
                Some((to, _)) => {
                    if let Some(n) = &mut self.note_edit {
                        n.1 = to;
                    }
                }
                None => self.note_edit = None,
            }
        }
        // Bookmarks of deleted lines join the line they land on, after its own comment.
        let notes = std::mem::take(&mut self.bookmarks[p]);
        let mut moved: BTreeMap<usize, String> = BTreeMap::new();
        for kept_pass in [true, false] {
            for (&l, note) in &notes {
                let Some(&(to, kept)) = map.get(l) else {
                    continue;
                };
                if kept != kept_pass {
                    continue;
                }
                let joined = moved.entry(to).or_default();
                if !note.is_empty() {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(note);
                }
            }
        }
        self.bookmarks[p] = moved;
    }

    /// Moves the synchronization points of pane `p` along with its lines after they changed
    /// from `before`, also when the line itself was edited; a point whose line was deleted is
    /// dropped (undo brings it back).
    fn remap_sync_points(&mut self, p: usize, before: &[String]) {
        if self.sync_points.is_empty() && self.sync_pending[p].is_none() {
            return;
        }
        let map = line_map(before, &self.panes[p].doc.lines, true);
        let moved = |l: usize| map.get(l).filter(|(_, kept)| *kept).map(|&(to, _)| to);
        self.sync_points.retain_mut(|s| match moved(s[p]) {
            Some(to) => {
                s[p] = to;
                true
            }
            None => false,
        });
        self.sync_pending[p] = self.sync_pending[p].and_then(moved);
    }

    // ---------- synchronization points ----------

    /// Picks the active pane's line on the cursor row for a synchronization point (see
    /// [`Self::set_sync_point_at`]).
    pub fn set_sync_point(&mut self) {
        if let Some(r) = self.cursor_row {
            self.set_sync_point_at(r, self.active_pane);
        }
    }

    /// Picks the line of `pane` on `row` for a synchronization point. Once a line is picked in
    /// every pane, they are put on one row and the compare is worked out again. Picking a line
    /// that is part of a synchronization point removes that point.
    pub fn set_sync_point_at(&mut self, row: usize, pane: usize) {
        let Some(line) = self.diff.rows.get(row).and_then(|r| r.line(pane)) else {
            self.status = tr("sync.no_line").to_string();
            return;
        };
        if let Some(i) = self.sync_points.iter().position(|s| s[pane] == line) {
            self.sync_points.remove(i);
            self.sync_pending.iter_mut().for_each(|p| *p = None);
            self.status = tr("sync.removed").to_string();
            self.rediff();
            self.layout_gen += 1;
            return;
        }
        if self.sync_pending[pane] == Some(line) {
            // The line picked again: it is no longer picked.
            self.sync_pending[pane] = None;
            self.status = tr("sync.unpicked").to_string();
            self.layout_gen += 1;
            return;
        }
        self.sync_pending[pane] = Some(line);
        let n = self.n();
        if let Some(lines) = (0..n)
            .map(|p| self.sync_pending[p])
            .collect::<Option<Vec<_>>>()
        {
            let mut point = [0; 3];
            point[..n].copy_from_slice(&lines);
            self.sync_pending.iter_mut().for_each(|p| *p = None);
            // Points stay in order: one that would cross an existing point is refused.
            let crosses = self.sync_points.iter().any(|e| {
                let before = (0..n).all(|p| point[p] < e[p]);
                let after = (0..n).all(|p| point[p] > e[p]);
                !before && !after
            });
            if crosses {
                self.status = tr("sync.crossing").to_string();
            } else {
                self.sync_points.push(point);
                self.sync_points.sort_unstable();
                self.rediff();
                if let Some(r) = self
                    .diff
                    .rows
                    .iter()
                    .position(|r| r.line(0) == Some(point[0]))
                {
                    self.cursor_row = Some(r);
                    self.scroll_to = Some(r);
                }
                self.status = trf("sync.added", &[("n", self.sync_points.len().to_string())]);
            }
        } else {
            let missing: Vec<&str> = (0..n)
                .filter(|&p| self.sync_pending[p].is_none())
                .map(|p| side_name(p, n))
                .collect();
            self.status = trf(
                "sync.pick",
                &[
                    ("line", (line + 1).to_string()),
                    ("side", side_name(pane, n).to_string()),
                    ("others", missing.join(" / ")),
                ],
            );
        }
        self.layout_gen += 1;
    }

    pub fn has_sync_points(&self) -> bool {
        !self.sync_points.is_empty() || self.sync_pending.iter().any(Option::is_some)
    }

    /// Removes every synchronization point, and any line picked for one.
    pub fn clear_sync_points(&mut self) {
        self.sync_pending.iter_mut().for_each(|p| *p = None);
        if !self.sync_points.is_empty() {
            self.sync_points.clear();
            self.rediff();
        }
        self.status = tr("sync.cleared").to_string();
        self.layout_gen += 1;
    }

    // ---------- bookmarks ----------

    /// The pane and line a bookmark toggled on `row` refers to: the active pane's line, or the
    /// first line on that row.
    fn bookmark_target(&self, row: usize, pane: usize) -> Option<(usize, usize)> {
        let r = self.diff.rows.get(row)?;
        r.line(pane)
            .map(|l| (pane, l))
            .or_else(|| (0..self.n()).find_map(|p| r.line(p).map(|l| (p, l))))
    }

    pub fn toggle_bookmark(&mut self) {
        if let Some(r) = self.cursor_row {
            self.toggle_bookmark_at(r, self.active_pane);
        }
    }

    fn toggle_bookmark_at(&mut self, row: usize, pane: usize) {
        if let Some((p, l)) = self.bookmark_target(row, pane) {
            if self.bookmarks[p].remove(&l).is_none() {
                self.bookmarks[p].insert(l, String::new());
            }
        }
    }

    pub fn has_bookmarks(&self) -> bool {
        self.bookmarks.iter().any(|b| !b.is_empty())
    }

    pub fn clear_bookmarks(&mut self) {
        self.bookmarks.iter_mut().for_each(BTreeMap::clear);
    }

    /// Diff rows that hold a bookmark, sorted.
    fn bookmark_rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .bookmarks
            .iter()
            .enumerate()
            .flat_map(|(p, ls)| ls.keys().map(move |&l| (p, l)))
            .map(|(p, l)| self.diff.row_of_line(p, l))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    fn row_bookmarked(&self, r: usize, p: usize) -> bool {
        self.diff.rows[r]
            .line(p)
            .is_some_and(|l| self.bookmarks[p].contains_key(&l))
    }

    /// The comment of a bookmark on row `r`, in the pane that has one.
    fn row_note(&self, r: usize) -> Option<&str> {
        let row = self.diff.rows.get(r)?;
        (0..self.n()).find_map(|p| {
            let note = self.bookmarks[p].get(&row.line(p)?)?;
            (!note.is_empty()).then_some(note.as_str())
        })
    }

    /// Opens the comment of the bookmark on `row` for editing, adding the bookmark if needed.
    fn edit_note_at(&mut self, row: usize, pane: usize) {
        if let Some((p, l)) = self.bookmark_target(row, pane) {
            let text = self.bookmarks[p].get(&l).cloned().unwrap_or_default();
            self.note_edit = Some((p, l, text, true));
        }
    }

    pub fn edit_note(&mut self) {
        if let Some(r) = self.cursor_row {
            self.edit_note_at(r, self.active_pane);
        }
    }

    /// Moves the cursor to the next (or previous) bookmark, wrapping around.
    pub fn goto_bookmark(&mut self, forward: bool) {
        let rows = self.bookmark_rows();
        let cur = self.cursor_row;
        let found = if forward {
            rows.iter()
                .find(|&&r| cur.is_none_or(|c| r > c))
                .or(rows.first())
        } else {
            rows.iter()
                .rev()
                .find(|&&r| cur.is_some_and(|c| r < c))
                .or(rows.last())
        };
        match found {
            Some(&row) => {
                self.sel_anchor = None;
                self.cursor_row = Some(row);
                self.scroll_to = Some(row);
                self.cur_block = self.diff.rows[row]
                    .block()
                    .filter(|&b| !self.diff.blocks[b].ignored);
                self.status = self.row_note(row).unwrap_or_default().to_string();
            }
            None => self.status = tr("msg.no_bookmarks").to_string(),
        }
    }

    // ---------- navigation ----------

    fn significant(&self) -> impl Iterator<Item = usize> + '_ {
        self.diff
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.ignored)
            .map(|(i, _)| i)
    }

    pub fn select_block(&mut self, b: usize) {
        if self
            .review_decision
            .as_ref()
            .is_some_and(|review| review.current_hunk.is_some())
            && self.review_within_budget()
        {
            let id = self.review_hunk_id(&self.diff.blocks[b]);
            if let Some(review) = &mut self.review_decision {
                if review.current_hunk.as_deref() != Some(&id) {
                    review.current_hunk = Some(id);
                    self.review_dirty = true;
                }
            }
        }
        self.cur_block = Some(b);
        let row = self.diff.blocks[b].first_row;
        self.cursor_row = Some(row);
        self.scroll_to = Some(row);
    }

    /// Runs `step` on the Formatted view while it is shown.
    fn formatted_step<R>(&mut self, step: impl FnOnce(&mut MarkdownView) -> R) -> Option<R> {
        match &mut self.formatted.view {
            Some((_, Ok(view))) if self.formatted_active => Some(step(view)),
            _ => None,
        }
    }

    /// Runs `step` on the Tree view and its comparison while the Tree view is shown.
    fn tree_step<R>(
        &mut self,
        step: impl FnOnce(
            &mut super::xml_tree_view::XmlTreeView,
            &crate::core::semantic::xml_tree::TreeDiff,
        ) -> R,
    ) -> Option<R> {
        match &self.tree.cache {
            Some((_, _, Ok(result))) if self.tree_active => {
                Some(step(&mut self.tree.view, &result.diff))
            }
            _ => None,
        }
    }

    pub fn first_diff(&mut self) {
        if self.formatted_step(MarkdownView::first_diff).is_some() {
            return;
        }
        if self.tree_step(|view, diff| view.edge(diff, true)).is_some() {
            return;
        }
        if let Some(rows) = self.keyed_differences() {
            if let Some(&r) = rows.first() {
                self.select_keyed_row(r);
            }
            return;
        }
        let first = self.significant().next();
        if let Some(b) = first {
            self.select_block(b);
        }
    }

    /// Selects difference `n` (0-based), or the last one when there are fewer.
    pub fn go_to_diff(&mut self, n: usize) {
        if let Some(rows) = self.keyed_differences() {
            if let Some(&r) = rows.get(n).or(rows.last()) {
                self.select_keyed_row(r);
            }
            return;
        }
        let sig: Vec<usize> = self.significant().collect();
        if let Some(&b) = sig.get(n).or(sig.last()) {
            self.select_block(b);
        }
    }

    pub fn last_diff(&mut self) {
        if self.formatted_step(MarkdownView::last_diff).is_some() {
            return;
        }
        if self
            .tree_step(|view, diff| view.edge(diff, false))
            .is_some()
        {
            return;
        }
        if let Some(rows) = self.keyed_differences() {
            if let Some(&r) = rows.last() {
                self.select_keyed_row(r);
            }
            return;
        }
        let last = self.significant().last();
        if let Some(b) = last {
            self.select_block(b);
        }
    }

    /// What the three sides of block `b` hold, so a resolved conflict is known again after
    /// the differences are worked out anew.
    fn block_key(&self, b: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let blk = &self.diff.blocks[b];
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (p, pane) in self.panes.iter().enumerate() {
            pane.doc.lines[blk.ranges[p].clone()].hash(&mut h);
        }
        h.finish()
    }

    /// Which blocks are open conflicts, worked out again once the differences or the resolved
    /// conflicts change: hashing a large conflict for every row drawn is slow.
    fn open_conflicts(&self) -> std::cell::Ref<'_, Vec<bool>> {
        let key = (self.layout_gen, self.resolved.len(), self.diff.blocks.len());
        if self.open_cache.borrow().0 != key {
            let open = (0..self.diff.blocks.len())
                .map(|b| {
                    let blk = &self.diff.blocks[b];
                    blk.kind == BlockKind::Conflict
                        && !blk.ignored
                        && (self.resolved.is_empty() || !self.resolved.contains(&self.block_key(b)))
                })
                .collect();
            *self.open_cache.borrow_mut() = (key, open);
        }
        std::cell::Ref::map(self.open_cache.borrow(), |c| &c.1)
    }

    /// A conflict that still needs a decision: not ignored and not marked resolved.
    pub fn is_open_conflict(&self, b: usize) -> bool {
        self.open_conflicts().get(b).copied().unwrap_or(false)
    }

    pub fn conflicts_left(&self) -> usize {
        self.open_conflicts().iter().filter(|&&o| o).count()
    }

    /// Accepts the middle of conflict `b` as it is, typically after editing it by hand.
    pub fn mark_resolved(&mut self, b: usize) {
        if b < self.diff.blocks.len() && self.is_open_conflict(b) {
            self.resolved.insert(self.block_key(b));
            let row = self.diff.blocks[b].first_row;
            self.cursor_row = Some(row);
            if !self.next_diff(true) {
                self.cur_block = Some(b);
            }
        }
    }

    /// Selects the first (or last) open conflict; false when there is none.
    pub fn edge_conflict(&mut self, first: bool) -> bool {
        if self.keyed().is_some() {
            return false;
        }
        let found = {
            let mut open = self.significant().filter(|&b| self.is_open_conflict(b));
            if first {
                open.next()
            } else {
                open.last()
            }
        };
        if let Some(b) = found {
            self.select_block(b);
        }
        found.is_some()
    }

    /// Selects the next difference; false when there is none after the cursor.
    pub fn next_diff(&mut self, conflicts_only: bool) -> bool {
        if let Some(found) = self.formatted_step(|view| !conflicts_only && view.next_diff()) {
            return found;
        }
        if let Some(found) = self.tree_step(|view, diff| !conflicts_only && view.step(diff, true)) {
            return found;
        }
        if self.matching() {
            return false;
        }
        if self.keyed().is_some() {
            return !conflicts_only && self.step_keyed(true) == Some(true);
        }
        let row = self.cursor_row;
        let found = self
            .significant()
            .filter(|&b| !conflicts_only || self.is_open_conflict(b))
            .find(|&b| row.is_none_or(|r| self.diff.blocks[b].first_row > r));
        if let Some(b) = found {
            self.select_block(b);
        }
        found.is_some()
    }

    /// Selects the previous difference; false when there is none before the cursor.
    pub fn prev_diff(&mut self, conflicts_only: bool) -> bool {
        if let Some(found) = self.formatted_step(|view| !conflicts_only && view.prev_diff()) {
            return found;
        }
        if let Some(found) = self.tree_step(|view, diff| !conflicts_only && view.step(diff, false))
        {
            return found;
        }
        if self.matching() {
            return false;
        }
        if self.keyed().is_some() {
            return !conflicts_only && self.step_keyed(false) == Some(true);
        }
        let row = self.cursor_row.unwrap_or(self.diff.rows.len());
        let containing = self.diff.rows.get(row).and_then(|r| r.block());
        let found = self
            .significant()
            .filter(|&b| !conflicts_only || self.is_open_conflict(b))
            .filter(|&b| self.diff.blocks[b].first_row < row && Some(b) != containing)
            .last();
        if let Some(b) = found {
            self.select_block(b);
        }
        found.is_some()
    }

    pub fn diff_position(&self) -> (Option<usize>, usize) {
        // Asked for several times a frame, so counted without collecting.
        let place = |stops: &mut dyn Iterator<Item = usize>, at: Option<usize>| {
            let (mut pos, mut total) = (None, 0);
            for s in stops {
                if Some(s) == at {
                    pos = Some(total);
                }
                total += 1;
            }
            (pos, total)
        };
        if let Some(d) = self.keyed() {
            let mut rows = (0..d.rows.len()).filter(|&i| d.rows[i].differs(d.ignore_order));
            return place(&mut rows, self.table.selected);
        }
        place(&mut self.significant(), self.cur_block)
    }

    // ---------- merging ----------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.panes.iter().map(|p| p.doc.lines.clone()).collect(),
            kept: vec![(0, 0); self.panes.len()],
            modified: self.panes.iter().map(|p| p.doc.modified).collect(),
            saves: self.panes.iter().map(|p| p.saves).collect(),
            formats: vec![None; self.panes.len()],
            sync_points: self.sync_points.clone(),
            carets: if self.multi_caret() {
                self.carets()
            } else {
                Vec::new()
            },
        }
    }

    fn push_undo(&mut self) {
        self.typing_group = false;
        let s = self.snapshot();
        push_step(&mut self.undo, s);
        let sizes: Vec<usize> = self.undo.iter().map(Snapshot::size).collect();
        self.undo.drain(..undo_overflow(&sizes));
        self.redo.clear();
    }

    fn restore(&mut self, s: Snapshot) {
        self.hold_view();
        let keep: Vec<bool> = self.panes.iter().map(|p| p.doc.read_only).collect();
        if (0..self.n()).any(|i| !keep[i] && self.step_changes(&s, i)) {
            // The text is no longer the one that was reviewed.
            self.invalidate_review();
        }
        for (i, f) in s.formats.into_iter().enumerate() {
            if let Some(f) = f.filter(|_| !keep[i]) {
                f.restore(&mut self.panes[i].doc);
            }
        }
        for (i, lines) in s.lines.into_iter().enumerate() {
            if keep[i] || self.panes[i].doc.lines == lines {
                continue;
            }
            let before = std::mem::replace(&mut self.panes[i].doc.lines, lines);
            self.panes[i].hl_dirty = true;
            self.remap_bookmarks(i, &before);
            self.remap_sync_points(i, &before);
            self.remap_view(i, &before);
        }
        // Points set since the step stay; ones the step dropped come back (crossing ones are
        // left out by the compare).
        self.sync_points.extend(s.sync_points);
        self.sync_points.sort_unstable();
        self.sync_points.dedup();
        for (i, p) in self.panes.iter_mut().enumerate() {
            if !keep[i] {
                p.doc.modified = s.modified[i] || s.saves[i] != p.saves;
            }
        }
        self.rediff();
        self.typing_group = false;
        if let Some(c) = self.caret() {
            // the caret moved with its line; the selection stays if both ends survived
            let lines = &self.panes[c.pane].doc.lines;
            let (pos, anchor) = (edit::clamp(lines, c.pos), edit::clamp(lines, c.anchor));
            self.place_caret(c.pane, anchor, false);
            self.place_caret(c.pane, pos, true);
        }
        if !s.carets.is_empty() {
            let list = s
                .carets
                .iter()
                .enumerate()
                .map(|(i, &c)| (c, i == 0))
                .collect();
            self.set_carets(list);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|s| self.steps_into_read_only(s))
        {
            self.status = tr("msg.readonly").to_string();
            return;
        }
        if let Some(s) = pop_step(&mut self.undo) {
            let cur = self.snapshot_for(&s);
            push_step(&mut self.redo, cur);
            self.restore(s);
        }
    }

    pub fn redo(&mut self) {
        if self
            .redo
            .last()
            .is_some_and(|s| self.steps_into_read_only(s))
        {
            self.status = tr("msg.readonly").to_string();
            return;
        }
        if let Some(s) = pop_step(&mut self.redo) {
            let cur = self.snapshot_for(&s);
            push_step(&mut self.undo, cur);
            self.restore(s);
        }
    }

    /// Whether going back to `s` would change pane `i`.
    fn step_changes(&self, s: &Snapshot, i: usize) -> bool {
        let doc = &self.panes[i].doc;
        s.lines[i] != doc.lines || s.formats[i].as_ref().is_some_and(|f| *f != format_of(doc))
    }

    /// Whether `s` would change only panes that are read-only now (made read-only after the
    /// step, or reloaded from disk since): the step then stays, with a message. Undo never
    /// writes a read-only pane; it leaves such panes as they are and restores the others.
    fn steps_into_read_only(&self, s: &Snapshot) -> bool {
        let (mut read_only, mut writable) = (false, false);
        for i in 0..self.n() {
            if self.step_changes(s, i) {
                if self.panes[i].doc.read_only {
                    read_only = true;
                } else {
                    writable = true;
                }
            }
        }
        read_only && !writable
    }

    /// The panes as they are, to go back to after restoring `s`: with the formats `s` changes.
    fn snapshot_for(&self, s: &Snapshot) -> Snapshot {
        let mut cur = self.snapshot();
        // Several carets can merge into one during a deletion. Redo must restore that
        // resulting caret too, rather than remapping the pre-edit carets through the text.
        if !s.carets.is_empty() {
            cur.carets = self.carets();
        }
        for (i, f) in s.formats.iter().enumerate() {
            if f.is_some() {
                cur.formats[i] = Some(format_of(&self.panes[i].doc));
            }
        }
        cur
    }

    fn replace_block_lines(&mut self, b: usize, from: usize, to: usize) {
        let blk = self.diff.blocks[b].clone();
        let src: Vec<String> = self.panes[from].doc.lines[blk.ranges[from].clone()].to_vec();
        let eof = blk.ranges[from].end == self.panes[from].doc.lines.len()
            && blk.ranges[to].end == self.panes[to].doc.lines.len();
        let whole = eof && blk.ranges[from].start == 0 && blk.ranges[to].start == 0;
        let source = format_of(&self.panes[from].doc);
        let old = format_of(&self.panes[to].doc);
        let d = &mut self.panes[to].doc;
        d.lines.splice(blk.ranges[to].clone(), src);
        if eof {
            d.final_newline = source.final_newline;
        }
        if whole && !self.opts.ignore_eol {
            d.eol = source.eol;
            d.mixed_eol = false;
        }
        if whole && !self.opts.ignore_codepage {
            d.encoding = source.encoding;
            d.bom = source.bom;
        }
        if format_of(d) != old {
            if let Some(s) = self.undo.last_mut() {
                s.formats[to].get_or_insert(old);
            }
        }
    }

    /// A new or empty file is written like the one it is first copied from, with one kind of
    /// line ending: `to`, `was_empty` before the last undo step, now has lines of `from`.
    fn take_format(&mut self, from: usize, to: usize, was_empty: bool) {
        if !was_empty || self.panes[to].doc.lines.is_empty() {
            return;
        }
        let (f, old) = (
            format_of(&self.panes[from].doc),
            format_of(&self.panes[to].doc),
        );
        let d = &mut self.panes[to].doc;
        (d.encoding, d.bom, d.eol, d.final_newline) = (f.encoding, f.bom, f.eol, f.final_newline);
        d.mixed_eol = false;
        if let Some(s) = self.undo.last_mut() {
            s.formats[to].get_or_insert(old);
        }
    }

    /// Writes `pane` with the line ending `e` from now on, as a step that can be undone.
    fn set_eol(&mut self, pane: usize, e: Eol) {
        let d = &self.panes[pane].doc;
        if e == d.eol && !d.mixed_eol {
            return;
        }
        let old = format_of(d);
        self.push_undo();
        if let Some(s) = self.undo.last_mut() {
            s.formats[pane] = Some(old);
        }
        let d = &mut self.panes[pane].doc;
        (d.eol, d.mixed_eol, d.modified) = (e, false, true);
        self.rediff();
    }

    fn writable(&mut self, pane: usize) -> bool {
        if self.panes[pane].doc.read_only {
            self.status = self.read_only_message(pane);
            false
        } else {
            true
        }
    }

    /// Why typing into pane `pane` was refused: which side it is, and how to edit it if it can be.
    fn read_only_message(&self, pane: usize) -> String {
        let side = side_name(pane, self.n()).to_string();
        let key = if self.panes[pane].doc.compare_only {
            "msg.side_compare_only"
        } else {
            "msg.side_read_only"
        };
        trf(key, &[("side", side)])
    }

    /// Whether typed text would go into the caret's pane now: it can be edited, no other field
    /// has the keyboard and the window has it. The caret is drawn dimmed otherwise.
    fn caret_live(&self, ctx: &egui::Context) -> bool {
        self.caret()
            .is_some_and(|c| !self.panes[c.pane].doc.read_only)
            && !self.shows_inline()
            && !widgets::typing(ctx)
            && ctx.input(|i| i.focused)
    }

    pub fn copy_block(&mut self, b: usize, from: usize, to: usize) {
        if from == to
            || from >= self.n()
            || to >= self.n()
            || b >= self.diff.blocks.len()
            || !self.writable(to)
        {
            return;
        }
        if self.table_mode {
            // Line blocks pair records by position, which rows matched by key do not.
            if self.keyed().is_some() || self.table.job.is_some() {
                return;
            }
            if let (Some(Ok(a)), Some(Ok(btable))) =
                (self.table.docs.get(from), self.table.docs.get(to))
            {
                let block = &self.diff.blocks[b];
                let source = btable.copy_lines(
                    block.ranges[to].clone(),
                    a,
                    block.ranges[from].clone(),
                    self.panes[to].doc.eol.as_str(),
                );
                if let Err(error) = self.replace_table_source(to, source) {
                    self.status = error;
                }
            }
            return;
        }
        self.push_undo();
        let was_empty = self.panes[to].doc.lines.is_empty();
        let row = self.diff.blocks[b].first_row;
        self.replace_block_lines(b, from, to);
        self.take_format(from, to, was_empty);
        self.mark_changed(&[to]);
        // stay near the merged position
        self.cur_block = None;
        self.cursor_row = Some(row);
        let next = self
            .significant()
            .find(|&nb| self.diff.blocks[nb].first_row >= row);
        if let Some(nb) = next {
            self.select_block(nb);
        }
    }

    /// Alt+Right / Alt+Left semantics.
    /// The selected rows, when more than one row is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let (a, c) = (self.sel_anchor?, self.cursor_row?);
        (a != c && a.max(c) < self.diff.rows.len()).then(|| (a.min(c), a.max(c)))
    }

    /// The lines of `pane` in the selected rows, or on the cursor row.
    pub fn selected_text(&self, pane: usize) -> Option<String> {
        let (r0, r1) = self
            .selection()
            .or_else(|| self.cursor_row.map(|r| (r, r)))?;
        let rows = self.diff.rows.get(r0..=r1)?;
        let doc = &self.panes[pane].doc;
        let lines: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.line(pane))
            .map(|l| doc.lines[l].as_str())
            .collect();
        (!lines.is_empty()).then(|| {
            let mut text = lines.join(doc.eol.as_str());
            if lines.len() > 1 {
                text.push_str(doc.eol.as_str());
            }
            text
        })
    }

    /// Replaces the lines of `to` in rows `r0..=r1` with the lines of `from` in those rows.
    pub fn copy_rows(&mut self, r0: usize, r1: usize, from: usize, to: usize) {
        if from == to
            || to >= self.n()
            || r1 >= self.diff.rows.len()
            || !self.writable(to)
            || self.matching()
        {
            return;
        }
        let rows = &self.diff.rows[r0..=r1];
        let src: Vec<String> = rows
            .iter()
            .filter_map(|r| r.line(from))
            .map(|l| self.panes[from].doc.lines[l].clone())
            .collect();
        let range = self.line_range(to, r0, r1);
        if self.panes[to].doc.lines[range.clone()] == src[..] {
            return;
        }
        let n = src.len().max(range.len());
        let start = range.start;
        self.push_undo();
        let was_empty = self.panes[to].doc.lines.is_empty();
        self.panes[to].doc.lines.splice(range, src);
        self.take_format(from, to, was_empty);
        self.mark_changed(&[to]);
        let row = self.diff.row_of_line(to, start);
        self.cursor_row = Some(row);
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block());
        self.status = trf("msg.copied_lines", &[("n", n.to_string())]);
    }

    /// Source and target panes of copying to the right or to the left.
    fn copy_direction(&self, to_right: bool) -> (usize, usize) {
        if self.n() == 2 {
            if to_right {
                (0, 1)
            } else {
                (1, 0)
            }
        } else if to_right {
            let from = self.active_pane.min(1);
            (from, from + 1)
        } else {
            let from = self.active_pane.max(1);
            (from, from - 1)
        }
    }

    pub fn copy_current(&mut self, to_right: bool) {
        if self.matching() {
            return;
        }
        if self.keyed().is_some() {
            if let Some(row) = self.table_cursor() {
                let (from, to) = if to_right { (0, 1) } else { (1, 0) };
                self.copy_keyed_row(row, from, to);
            }
            return;
        }
        if let Some((r0, r1)) = self.selection() {
            let (from, to) = self.copy_direction(to_right);
            self.copy_rows(r0, r1, from, to);
            return;
        }
        let Some(b) = self.cur_block else { return };
        let (from, to) = if self.n() == 2 {
            if to_right {
                (0, 1)
            } else {
                (1, 0)
            }
        } else if to_right {
            let from = self.active_pane.min(1);
            (from, from + 1)
        } else {
            let from = self.active_pane.max(1);
            (from, from - 1)
        };
        self.copy_block(b, from, to);
    }

    pub fn copy_all(&mut self, from: usize, to: usize) {
        if from == to || to >= self.n() || !self.writable(to) {
            return;
        }
        self.push_undo();
        let was_empty = self.panes[to].doc.lines.is_empty();
        let blocks: Vec<usize> = self.significant().collect();
        for &b in blocks.iter().rev() {
            self.replace_block_lines(b, from, to);
        }
        self.take_format(from, to, was_empty);
        let old = format_of(&self.panes[to].doc);
        let source = format_of(&self.panes[from].doc);
        let d = &mut self.panes[to].doc;
        if !self.opts.ignore_eol {
            (d.eol, d.mixed_eol, d.original_eols) =
                (source.eol, source.mixed_eol, source.original_eols);
        }
        if !self.opts.ignore_codepage {
            (d.encoding, d.bom) = (source.encoding, source.bom);
        }
        if format_of(d) != old {
            self.undo.last_mut().unwrap().formats[to].get_or_insert(old);
        }
        self.mark_changed(&[to]);
        self.cur_block = None;
        self.status = trf("msg.copied_blocks", &[("n", blocks.len().to_string())]);
        if !blocks.is_empty() {
            self.bulk_edit = Some(self.status.clone());
        }
    }

    /// Three-way: replaces the middle of the current difference with the left lines followed by
    /// the right lines (`left_first`), or the other way round.
    pub fn take_both(&mut self, left_first: bool) {
        let Some(b) = self.cur_block else { return };
        self.take_both_at(b, left_first);
    }

    fn take_both_at(&mut self, b: usize, left_first: bool) {
        if self.n() != 3 || b >= self.diff.blocks.len() || !self.writable(1) {
            return;
        }
        let blk = self.diff.blocks[b].clone();
        let (first, second) = if left_first { (0, 2) } else { (2, 0) };
        let mut lines = self.panes[first].doc.lines[blk.ranges[first].clone()].to_vec();
        lines.extend_from_slice(&self.panes[second].doc.lines[blk.ranges[second].clone()]);
        if self.panes[1].doc.lines[blk.ranges[1].clone()] == lines[..] {
            return;
        }
        self.push_undo();
        let was_empty = self.panes[1].doc.lines.is_empty();
        let start = blk.ranges[1].start;
        self.panes[1].doc.lines.splice(blk.ranges[1].clone(), lines);
        let from = if blk.ranges[first].is_empty() {
            second
        } else {
            first
        };
        self.take_format(from, 1, was_empty);
        self.mark_changed(&[1]);
        let row = self.diff.row_of_line(1, start);
        self.cursor_row = Some(row);
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block());
        // The middle now differs from both sides, which alone would still read as a conflict.
        if let Some(nb) = self.cur_block.filter(|&nb| self.is_open_conflict(nb)) {
            self.resolved.insert(self.block_key(nb));
        }
    }

    /// Whether "take both" applies to the current difference.
    pub fn can_take_both(&self) -> bool {
        self.n() == 3
            && !self.panes[1].doc.read_only
            && self
                .cur_block
                .is_some_and(|b| self.diff.blocks.get(b).is_some_and(|b| !b.ignored))
    }

    /// Three-way: merges all non-conflicting changes into the middle pane.
    /// Run the same merge on an isolated in-memory view; never save or modify this view.
    pub fn preview_merge(
        &self,
        cfg: &Config,
    ) -> anyhow::Result<crate::core::merge_validation::MergePlan> {
        anyhow::ensure!(self.n() == 3, "Merge preview requires three panes");
        let docs: Vec<_> = self.panes.iter().map(|p| p.doc.clone()).collect();
        let mut local = cfg.clone();
        local.diff = self.opts.clone();
        let mut preview = Self::new(docs.clone(), &local);
        preview.opts = self.opts.clone();
        preview.sync_points = self.sync_points.clone();
        preview.panes[1].doc.read_only = false;
        let refs: Vec<_> = docs.iter().map(|d| d.lines.as_slice()).collect();
        let mut alignment =
            diff::compute_synced(&refs, &self.opts, self.comment_syntax(), &self.sync_points);
        diff::include_doc_formats(&mut alignment, &docs.iter().collect::<Vec<_>>(), &self.opts);
        preview.auto_merge();
        Ok(crate::core::merge_validation::text_plan(
            &docs,
            &self.opts,
            &preview.panes[1].doc,
            &alignment,
        ))
    }

    pub fn auto_merge(&mut self) {
        if self.n() != 3 || !self.writable(1) {
            return;
        }
        fn merge<T: PartialEq + Clone>(left: &T, base: &T, right: &T) -> T {
            if left == base {
                right.clone()
            } else if right == base || left == right {
                left.clone()
            } else {
                base.clone()
            }
        }
        let formats: Vec<_> = self.panes.iter().map(|p| format_of(&p.doc)).collect();
        let refs: Vec<&[String]> = self.panes.iter().map(|p| p.doc.lines.as_slice()).collect();
        self.diff =
            diff::compute_synced(&refs, &self.opts, self.comment_syntax(), &self.sync_points);
        let mut endings: Vec<Vec<Eol>> = self
            .panes
            .iter()
            .map(|p| {
                (0..p.doc.lines.len())
                    .map(|i| p.doc.line_eol(i).unwrap_or(p.doc.eol))
                    .collect()
            })
            .collect();
        // Reuse text alignment, so terminator changes on retained lines also follow
        // insertions/deletions. Missing sides do not invent a terminator change.
        if !self.opts.ignore_eol {
            for row in &self.diff.rows {
                if let Some(base) = row.line(1) {
                    let b = endings[1][base];
                    let l = row
                        .line(0)
                        .and_then(|i| self.panes[0].doc.line_eol(i))
                        .unwrap_or(b);
                    let r = row
                        .line(2)
                        .and_then(|i| self.panes[2].doc.line_eol(i))
                        .unwrap_or(b);
                    let merged = merge(&l, &b, &r);
                    for (p, line) in row.lines().iter().enumerate() {
                        if let Some(i) = line {
                            endings[p][*i] = merged;
                        }
                    }
                }
            }
        }
        let mut output_endings = endings[1].clone();
        self.push_undo();
        let was_empty = self.panes[1].doc.lines.is_empty();
        let blocks: Vec<(usize, BlockKind)> = self
            .significant()
            .map(|b| (b, self.diff.blocks[b].kind))
            .collect();
        let mut merged = 0;
        // Of the first block merged, as the blocks are merged from the end.
        let mut first_from = 0;
        for &(b, kind) in blocks.iter().rev() {
            let from = match kind {
                BlockKind::LeftOnly | BlockKind::BothSame => 0,
                BlockKind::RightOnly => 2,
                _ => continue,
            };
            let block = &self.diff.blocks[b];
            output_endings.splice(
                block.ranges[1].clone(),
                endings[from][block.ranges[from].clone()].iter().copied(),
            );
            self.replace_block_lines(b, from, 1);
            first_from = from;
            merged += 1;
        }
        self.take_format(first_from, 1, was_empty);
        let [l, b, r] = [&formats[0], &formats[1], &formats[2]];
        let d = &mut self.panes[1].doc;
        // These are independent properties: a text edit need not conflict with a BOM edit.
        if !self.opts.ignore_codepage {
            d.encoding = merge(&l.encoding, &b.encoding, &r.encoding);
            d.bom = merge(&l.bom, &b.bom, &r.bom);
        }
        d.final_newline = merge(&l.final_newline, &b.final_newline, &r.final_newline);
        if !self.opts.ignore_eol {
            let last = output_endings.len().saturating_sub(1);
            let terminators: Vec<_> = output_endings
                .iter()
                .enumerate()
                .map(|(i, &eol)| {
                    if i == last && !d.final_newline {
                        None
                    } else {
                        Some(eol)
                    }
                })
                .collect();
            let mut counts = text::EolCounts::default();
            for &eol in &terminators {
                counts.add(eol);
            }
            d.eol = counts.dominant().unwrap_or(b.eol);
            d.mixed_eol = counts.mixed();
            d.original_eols = if d.mixed_eol { terminators } else { Vec::new() };
            // A successful mixed-EOL merge must retain those edits when it is saved.
            if d.mixed_eol {
                d.preserve_eols = true;
            }
        }
        if format_of(d) != formats[1] {
            self.undo.last_mut().unwrap().formats[1].get_or_insert(formats[1].clone());
        }
        self.mark_changed(&[1]);
        self.cur_block = None;
        self.status = trf(
            "msg.auto_merged",
            &[
                ("n", merged.to_string()),
                ("c", self.conflicts_left().to_string()),
            ],
        );
        if merged > 0 {
            self.bulk_edit = Some(self.status.clone());
        }
        if self.conflicts_left() > 0 {
            self.cursor_row = Some(0);
            self.next_diff(true);
        }
    }

    // ---------- editing ----------

    fn insert_index(&self, pane: usize, row: usize) -> usize {
        for r in (0..row.min(self.diff.rows.len())).rev() {
            if let Some(l) = self.diff.rows[r].line(pane) {
                return l + 1;
            }
        }
        0
    }

    fn begin_edit(&mut self, row: usize, pane: usize) {
        if self.table_mode {
            if let Some(line) = self.diff.rows.get(row).and_then(|r| r.line(pane)) {
                if let Some(record) = self
                    .table
                    .docs
                    .get(pane)
                    .and_then(|t| t.as_ref().ok())
                    .and_then(|t| t.records.iter().position(|r| r.lines.contains(&line)))
                {
                    self.begin_cell_edit(pane, record, self.table.column);
                }
            }
            return;
        }
        if self.panes[pane].doc.read_only {
            self.status = self.read_only_message(pane);
            return;
        }
        self.col_sel = None;
        self.drop_carets();
        let mut e = EditState {
            row,
            pane,
            text: String::new(),
            focus: true,
        };
        e.text = self.edit_original(&e);
        self.edit = Some(e);
    }

    /// The lines of `pane` in rows `r0..=r1`, or the place where lines would go there when those
    /// rows have none of its lines.
    fn line_range(&self, pane: usize, r0: usize, r1: usize) -> std::ops::Range<usize> {
        let rows = self.diff.rows.get(r0..=r1).unwrap_or(&[]);
        let mut lines = rows.iter().filter_map(|r| r.line(pane));
        match (lines.next(), lines.next_back()) {
            (Some(a), b) => a..b.unwrap_or(a) + 1,
            _ => {
                let at = self.insert_index(pane, r0);
                at..at
            }
        }
    }

    /// The text an edit started from: its lines joined by newlines.
    fn edit_original(&self, e: &EditState) -> String {
        self.panes[e.pane].doc.lines[self.line_range(e.pane, e.row, e.row)].join("\n")
    }

    /// Replaces the lines of the edit with its text, as one undo step.
    fn commit_edit(&mut self, e: EditState) {
        let range = self.line_range(e.pane, e.row, e.row);
        let new_lines: Vec<String> = e
            .text
            .split('\n')
            .map(|s| s.trim_end_matches('\r').to_string())
            .collect();
        let doc = &self.panes[e.pane].doc;
        // Leaving an untouched editor, or an empty one on a row without a line, changes nothing.
        if doc.lines[range.clone()] == new_lines[..] || (range.is_empty() && e.text.is_empty()) {
            return;
        }
        self.push_undo();
        self.panes[e.pane].doc.lines.splice(range, new_lines);
        self.mark_changed(&[e.pane]);
    }

    /// Removes the lines of `pane` in rows `r0..=r1` as one undo step, and returns how many.
    fn delete_rows(&mut self, pane: usize, r0: usize, r1: usize) -> usize {
        if !self.writable(pane) {
            return 0;
        }
        let range = self.line_range(pane, r0, r1);
        if range.is_empty() {
            return 0;
        }
        let (start, n) = (range.start, range.len());
        self.push_undo();
        self.panes[pane].doc.lines.drain(range);
        self.mark_changed(&[pane]);
        let len = self.panes[pane].doc.lines.len();
        let row = self
            .diff
            .row_of_line(pane, start.min(len.saturating_sub(1)));
        self.cursor_row = (row < self.diff.rows.len()).then_some(row);
        self.cur_block = self.cursor_row.and_then(|r| self.diff.rows[r].block());
        self.status = trf("msg.deleted_lines", &[("n", n.to_string())]);
        n
    }

    pub fn caret(&self) -> Option<Caret> {
        self.caret.as_deref().map(|s| s.at)
    }

    /// Every caret, the primary one first.
    pub fn carets(&self) -> Vec<Caret> {
        self.caret.as_deref().map_or_else(Vec::new, |s| {
            std::iter::once(s.at)
                .chain(s.others.iter().copied())
                .collect()
        })
    }

    /// Whether there is more than one caret.
    fn multi_caret(&self) -> bool {
        self.caret.as_ref().is_some_and(|s| !s.others.is_empty())
    }

    /// Makes `carets` the carets (marked `true`: the primary one), merged where they overlap,
    /// and points the view at the primary one.
    fn set_carets(&mut self, mut carets: Vec<(Caret, bool)>) {
        merge_carets(&mut carets);
        let Some(i) = carets
            .iter()
            .position(|(_, p)| *p)
            .or(carets.len().checked_sub(1))
        else {
            self.drop_carets();
            return;
        };
        let (primary, _) = carets.remove(i);
        let preedit = self
            .caret
            .as_mut()
            .map(|s| std::mem::take(&mut s.preedit))
            .unwrap_or_default();
        let lines = &self.panes[primary.pane].doc.lines;
        let fit = |c: Caret| Caret {
            pos: edit::clamp(lines, c.pos),
            anchor: edit::clamp(lines, c.anchor),
            ..c
        };
        self.caret = Some(Box::new(CaretState {
            at: fit(primary),
            preedit,
            others: carets.into_iter().map(|(c, _)| fit(c)).collect(),
            batch: None,
        }));
        self.col_sel = None;
        self.follow_caret();
    }

    /// Removes every caret: the keys go back to the rows.
    fn drop_carets(&mut self) {
        self.caret = None;
    }

    /// Keeps only the primary caret.
    fn single_caret(&mut self) {
        if let Some(s) = &mut self.caret {
            s.others.clear();
        }
        self.typing_group = false;
    }

    /// Applies `op`, written for one caret, at each caret in turn, from the last in the text to
    /// the first, as one undo step: carets already done move with the text each edit changes
    /// before them. `op` gets the caret's index in text order and the number of carets.
    fn at_each_caret(&mut self, op: impl FnMut(&mut Self, usize, usize)) {
        self.at_each_caret_with(true, op);
    }

    /// [`Self::at_each_caret`]; without `edits`, `op` only moves carets, so no text is kept
    /// for an undo step.
    fn at_each_caret_with(&mut self, edits: bool, mut op: impl FnMut(&mut Self, usize, usize)) {
        if !self.multi_caret() {
            op(self, 0, 1);
            return;
        }
        let state = self.caret.take().expect("carets");
        let pane = state.at.pane;
        let mut carets: Vec<(Caret, bool)> = std::iter::once((state.at, true))
            .chain(state.others.iter().map(|&c| (c, false)))
            .collect();
        merge_carets(&mut carets);
        // A key at a caret changes only the lines of its selection and the line on each side
        // (Backspace at a line start, Delete at its end), so only those are kept for the undo
        // step, not the whole file at every key.
        let old_len = self.panes[pane].doc.lines.len();
        let lo = carets
            .iter()
            .map(|(c, _)| c.range().0 .0)
            .min()
            .unwrap_or(0)
            .saturating_sub(1)
            .min(old_len);
        let hi = carets
            .iter()
            .map(|(c, _)| c.range().1 .0)
            .max()
            .unwrap_or(0)
            .saturating_add(2)
            .clamp(lo, old_len);
        let kept: Vec<String> = if edits {
            self.panes[pane].doc.lines[lo..hi].to_vec()
        } else {
            Vec::new()
        };
        #[cfg(debug_assertions)]
        let whole = self.panes[pane].doc.lines.clone();
        let count = carets.len();
        let mut batch = Batch {
            changed: false,
            typing: true,
        };
        let mut done: Vec<(Caret, bool)> = Vec::with_capacity(count);
        for (index, &(c, primary)) in carets.iter().enumerate().rev() {
            let lines = &self.panes[pane].doc.lines;
            let old_len = lines.len();
            let tail = |p: edit::Pos| {
                lines
                    .get(p.0)
                    .and_then(|t| t.get(p.1..))
                    .unwrap_or("")
                    .to_string()
            };
            let tails: Vec<(String, String)> = done
                .iter()
                .map(|(d, _)| (tail(d.pos), tail(d.anchor)))
                .collect();
            self.caret = Some(Box::new(CaretState {
                at: c,
                preedit: String::new(),
                others: Vec::new(),
                batch: Some(Batch {
                    changed: false,
                    typing: true,
                }),
            }));
            op(self, index, count);
            let Some(after) = self.caret.take() else {
                done.push((c, primary));
                continue;
            };
            let edited = after.batch.is_some_and(|b| b.changed);
            if edited {
                let b = after.batch.unwrap_or(batch);
                batch.changed = true;
                batch.typing &= b.typing;
                let lines = &self.panes[pane].doc.lines;
                for ((d, _), (pos_tail, anchor_tail)) in done.iter_mut().zip(&tails) {
                    d.pos = shift_past_edit(lines, old_len, d.pos, pos_tail);
                    d.anchor = shift_past_edit(lines, old_len, d.anchor, anchor_tail);
                }
            }
            done.push((after.at, primary));
        }
        debug_assert!(edits || !batch.changed, "a caret move changed the text");
        if batch.changed {
            // One undo step for all of them, taken from the text before the first edit: the
            // kept lines go back in for the snapshot, and the edited ones after it.
            let new_len = self.panes[pane].doc.lines.len();
            let end = (hi + new_len).saturating_sub(old_len).max(lo);
            #[cfg(debug_assertions)]
            {
                let now = &self.panes[pane].doc.lines;
                debug_assert!(
                    now[..lo] == whole[..lo] && now[end..] == whole[hi..],
                    "a caret edit changed lines outside {lo}..{hi}"
                );
            }
            let joins = batch.typing && self.typing_group && !self.undo.is_empty();
            self.caret = Some(state);
            if !joins {
                let lines = &mut self.panes[pane].doc.lines;
                let edited: Vec<String> = lines.splice(lo..end, kept).collect();
                self.push_undo();
                self.panes[pane].doc.lines.splice(lo..hi, edited);
            }
            if batch.typing {
                self.invalidate_review();
                let p = &mut self.panes[pane];
                p.doc.modified = true;
                p.hl_dirty = true;
                self.redo.clear();
                self.rediff_soon();
            } else {
                self.mark_changed(&[pane]);
            }
        } else {
            self.caret = Some(state);
        }
        self.set_carets(done);
        self.typing_group = batch.changed && batch.typing;
    }

    /// Ctrl+click (⌘-click on macOS) at `at` of pane `pane`: adds a caret there, or removes the
    /// one there when there are others. In another pane, or without a caret, it is just placed.
    fn toggle_caret(&mut self, pane: usize, at: edit::Pos) {
        let carets = self.carets();
        if carets.first().is_none_or(|c| c.pane != pane) {
            self.place_caret(pane, at, false);
            return;
        }
        let hit = carets.iter().position(|c| {
            let (a, b) = c.range();
            c.pos == at || (c.has_selection() && a <= at && at <= b)
        });
        let mut list: Vec<(Caret, bool)> = carets.into_iter().map(|c| (c, false)).collect();
        match hit {
            Some(i) if list.len() > 1 => {
                list.remove(i);
                let last = list.len() - 1;
                // The primary one stays, or the one added before it takes over.
                let keep = if i == 0 { last } else { 0 };
                list[keep].1 = true;
            }
            Some(_) => return,
            None => {
                let at = edit::clamp(&self.panes[pane].doc.lines, at);
                list.push((
                    Caret {
                        pane,
                        pos: at,
                        anchor: at,
                        want: None,
                    },
                    true,
                ));
            }
        }
        self.set_carets(list);
        self.typing_group = false;
    }

    /// Ctrl+Alt+Up or Down (⌘⌥ on macOS): a new caret on the line above the first caret, or
    /// below the last, at the same display column.
    fn add_caret_line(&mut self, down: bool) {
        let carets = self.carets();
        let Some(&first) = carets.first() else { return };
        let edge = if down {
            carets.iter().max_by_key(|c| c.pos)
        } else {
            carets.iter().min_by_key(|c| c.pos)
        };
        let Some(&edge) = edge else { return };
        let lines = &self.panes[first.pane].doc.lines;
        let tab = self.tab_size;
        let Some(line) = edge.pos.0.checked_add_signed(if down { 1 } else { -1 }) else {
            return;
        };
        let Some(text) = lines.get(line) else { return };
        let want = edge.want.unwrap_or_else(|| {
            edit::col_at(
                lines.get(edge.pos.0).map_or("", String::as_str),
                edge.pos.1,
                tab,
            )
        });
        let at = (line, edit::byte_at(text, want, tab).0);
        let mut list: Vec<(Caret, bool)> = carets.into_iter().map(|c| (c, false)).collect();
        list.push((
            Caret {
                pane: first.pane,
                pos: at,
                anchor: at,
                want: Some(want),
            },
            true,
        ));
        self.set_carets(list);
        self.typing_group = false;
    }

    /// Whether typed keys go into a pane, so that one-key shortcuts must leave them alone. The
    /// inline view only shows the files: a caret kept from the side-by-side view waits there.
    pub fn takes_text(&self) -> bool {
        self.caret.is_some() && !self.table_mode && !self.shows_inline()
    }

    /// Whether typed characters would go into the caret's pane, which is not read-only, so
    /// shortcuts that type a character (Option+letter on macOS) must leave it alone.
    pub fn types_into_pane(&self) -> bool {
        self.takes_text()
            && self
                .caret
                .as_ref()
                .is_some_and(|s| !self.panes[s.at.pane].doc.read_only)
    }

    /// Whether the inline view is the one shown, which shows the files without editing them.
    pub fn shows_inline(&self) -> bool {
        self.unified && self.n() == 2 && !self.table_mode
    }

    /// Edit side by side: leaves the inline view for the side-by-side one, with the text caret
    /// on the inline view's cursor line (or where it was, when it is still on that line).
    pub fn edit_side_by_side(&mut self) {
        let at = self.cursor_row.and_then(|row| {
            let pane = self.active_pane;
            let line = self.diff.rows.get(row)?.line(pane)?;
            Some((row, pane, line))
        });
        self.unified = false;
        let Some((row, pane, line)) = at else { return };
        if !self
            .caret()
            .is_some_and(|c| c.pane == pane && c.pos.0 == line)
        {
            self.place_caret(pane, (line, 0), false);
        }
        self.cursor_row = Some(row);
        self.scroll_to = Some(row);
    }

    /// Whether the text caret takes `key` with modifiers `m` before the shortcuts see it (the
    /// word keys of macOS, Option+Left and Option+Right, are Alt+Left and Alt+Right).
    pub fn takes_key(&self, key: Key, m: Modifiers) -> bool {
        self.takes_text() && Self::caret_key(key, m)
    }

    /// Puts the caret of `pane` at `pos`; `extend` keeps the other end of the selection.
    pub fn place_caret(&mut self, pane: usize, pos: edit::Pos, extend: bool) {
        if pane >= self.n() {
            return;
        }
        let pos = edit::clamp(&self.panes[pane].doc.lines, pos);
        let anchor = match self.caret() {
            Some(c) if extend && c.pane == pane => c.anchor,
            _ => pos,
        };
        let batch = self.caret.as_ref().and_then(|s| s.batch);
        self.caret = Some(Box::new(CaretState {
            at: Caret {
                pane,
                pos,
                anchor,
                want: None,
            },
            preedit: String::new(),
            others: Vec::new(),
            batch,
        }));
        self.col_sel = None;
        self.typing_group = false;
        self.follow_caret();
    }

    /// Points the cursor row and the row selection at the caret, and scrolls to it.
    fn follow_caret(&mut self) {
        let Some(c) = self.caret() else { return };
        self.active_pane = c.pane;
        self.caret_reveal = true;
        if self.diff.rows.is_empty() {
            return;
        }
        // A selection over several lines selects their rows, for copying them to another side;
        // when it ends at the start of a line (a whole line, say), that line is not one of them.
        let (a, b) = c.range();
        let last = if b.0 > a.0 && b.1 == 0 { b.0 - 1 } else { b.0 };
        let line_of = |end: edit::Pos| if end == b { last } else { end.0 };
        let (pos_line, anchor_line) = (line_of(c.pos), line_of(c.anchor));
        let row = self.diff.row_of_line(c.pane, pos_line);
        self.cursor_row = Some(row);
        self.scroll_to = Some(self.diff.row_of_line(c.pane, c.pos.0));
        self.cur_block = self.diff.rows[row]
            .block()
            .filter(|&b| !self.diff.blocks[b].ignored);
        self.sel_anchor =
            (anchor_line != pos_line).then(|| self.diff.row_of_line(c.pane, anchor_line));
    }

    /// What *Copy text* in the context menu of row `r` of pane `p` copies: the text caret's
    /// selection when the menu is on its lines, else the selected rows when the menu is on
    /// them, else the line clicked.
    fn menu_copy_text(&self, r: usize, p: usize) -> Option<String> {
        let line = self.diff.rows.get(r).and_then(|row| row.line(p));
        let on_caret = self.caret().is_some_and(|c| {
            let (a, b) = c.range();
            c.pane == p && c.has_selection() && line.is_some_and(|l| a.0 <= l && l <= b.0)
        });
        if on_caret {
            return self.caret_text();
        }
        match self.selection() {
            Some((a, b)) if (a..=b).contains(&r) && p == self.active_pane => self.selected_text(p),
            _ => line.map(|l| self.panes[p].doc.lines[l].clone()),
        }
    }

    /// The selected text of the caret's pane, with the document's line ending between lines.
    pub fn caret_text(&self) -> Option<String> {
        let c = self.caret().filter(Caret::has_selection)?;
        let doc = &self.panes[c.pane].doc;
        let (a, b) = c.range();
        Some(edit::text_between(&doc.lines, a, b, doc.eol.as_str()))
    }

    /// Applies `f` to the lines of the caret's pane, given the selection's ends, and puts the
    /// caret where it returns. Each edit is one undo step, except that typing within a line
    /// (`typing`) joins the step of the keystroke before it.
    fn caret_edit(
        &mut self,
        typing: bool,
        f: impl FnOnce(&mut Vec<String>, edit::Pos, edit::Pos) -> edit::Pos,
    ) {
        let Some(c) = self.caret() else { return };
        if c.pane >= self.n() || self.matching() || !self.writable(c.pane) {
            return;
        }
        if let Some(state) = self.caret.as_mut().filter(|s| s.batch.is_some()) {
            // One of several carets: the caller makes the undo step once for all of them.
            let (a, b) = c.range();
            let pos = f(&mut self.panes[c.pane].doc.lines, a, b);
            state.at = Caret {
                pos,
                anchor: pos,
                want: None,
                ..c
            };
            if let Some(batch) = &mut state.batch {
                batch.changed = true;
                batch.typing &= typing;
            }
            return;
        }
        let joins = typing && self.typing_group && !self.undo.is_empty();
        if !joins {
            self.push_undo();
        }
        let (a, b) = c.range();
        let pos = f(&mut self.panes[c.pane].doc.lines, a, b);
        if typing {
            // The lines stay where they were, so bookmarks and synchronization points do too.
            self.invalidate_review();
            let pane = &mut self.panes[c.pane];
            pane.doc.modified = true;
            pane.hl_dirty = true;
            self.redo.clear();
            self.rediff_soon();
        } else {
            self.mark_changed(&[c.pane]);
        }
        self.place_caret(c.pane, pos, false);
        self.typing_group = typing;
    }

    /// Types `text` at the caret, over the selection.
    pub fn caret_insert(&mut self, text: &str) {
        let Some(c) = self.caret() else { return };
        if text.is_empty() && !c.has_selection() {
            return;
        }
        let typing = !c.has_selection() && !text.contains(['\n', '\r']);
        self.caret_edit(typing, |lines, a, b| {
            let at = edit::remove(lines, a, b);
            edit::insert(lines, at, text)
        });
    }

    /// Pastes `text` at the caret, over the selection, as an undo step of its own.
    pub fn caret_paste(&mut self, text: &str) {
        let Some(c) = self.caret() else { return };
        if text.is_empty() && !c.has_selection() {
            return;
        }
        self.caret_edit(false, |lines, a, b| {
            let at = edit::remove(lines, a, b);
            edit::insert(lines, at, text)
        });
    }

    /// Enter: a new line, indented like the one the caret is on.
    fn caret_newline(&mut self) {
        let Some(c) = self.caret() else { return };
        let line = self.panes[c.pane]
            .doc
            .lines
            .get(c.range().0 .0)
            .map_or("", String::as_str);
        let indent: String = line
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .take(c.range().0 .1)
            .collect();
        self.caret_insert(&format!("\n{indent}"));
    }

    /// Backspace (`forward` false) or Delete: removes the selection, or the character before
    /// or after the caret, joining lines at their ends.
    /// Backspace (`forward` false) or Delete at the caret: the selection, or else one character,
    /// or with `word` the word up to the caret (or after it), as one undo step.
    fn caret_delete(&mut self, forward: bool, word: bool) {
        let carets = self.carets();
        let Some(c) = carets.first() else { return };
        let lines = &self.panes[c.pane].doc.lines;
        let ranges: Vec<_> = carets
            .iter()
            .map(|c| {
                if c.has_selection() {
                    c.range()
                } else if forward && word {
                    (c.pos, edit::word_right(lines, c.pos))
                } else if forward {
                    (c.pos, edit::after(lines, c.pos))
                } else if word {
                    (edit::word_left(lines, c.pos), c.pos)
                } else {
                    (edit::before(lines, c.pos), c.pos)
                }
            })
            .collect();
        let typing = !word
            && carets.iter().all(|c| !c.has_selection())
            && ranges.iter().all(|(a, b)| a.0 == b.0);
        self.delete_at_carets(ranges, typing);
    }

    /// Deletes ranges computed against the original text, merging overlaps before editing.
    /// Each caret lands at the start of its range, moved through every deletion. In particular,
    /// overlapping word deletes and whole-line cuts never consume text that moved into place.
    fn delete_at_carets(&mut self, ranges: Vec<(edit::Pos, edit::Pos)>, typing: bool) {
        let carets = self.carets();
        let Some(c) = carets.first() else { return };
        let pane = c.pane;
        if self.matching() || !self.writable(pane) {
            return;
        }
        let mut merged: Vec<(edit::Pos, edit::Pos)> = Vec::new();
        let mut ordered: Vec<_> = ranges.iter().copied().filter(|(a, b)| a < b).collect();
        ordered.sort_unstable();
        for (a, b) in ordered {
            match merged.last_mut() {
                Some((_, end)) if a <= *end => *end = (*end).max(b),
                _ => merged.push((a, b)),
            }
        }
        if merged.is_empty() {
            return;
        }
        let joins = typing && self.typing_group && !self.undo.is_empty();
        if !joins {
            self.push_undo();
        }
        let mut positions: Vec<_> = ranges.iter().map(|&(a, _)| a).collect();
        for (a, b) in merged.into_iter().rev() {
            edit::remove(&mut self.panes[pane].doc.lines, a, b);
            for pos in &mut positions {
                *pos = if *pos < a {
                    *pos
                } else if *pos <= b {
                    a
                } else if pos.0 == b.0 {
                    (a.0, a.1 + pos.1 - b.1)
                } else {
                    (pos.0 - (b.0 - a.0), pos.1)
                };
            }
        }
        if typing {
            self.invalidate_review();
            let p = &mut self.panes[pane];
            p.doc.modified = true;
            p.hl_dirty = true;
            self.redo.clear();
            self.rediff_soon();
        } else {
            self.mark_changed(&[pane]);
        }
        self.set_carets(
            carets
                .iter()
                .zip(positions)
                .enumerate()
                .map(|(i, (c, pos))| {
                    (
                        Caret {
                            pos,
                            anchor: pos,
                            want: None,
                            ..*c
                        },
                        i == 0,
                    )
                })
                .collect(),
        );
        self.typing_group = typing;
    }

    /// Copy or Cut at the carets: their selections, or with none their whole lines. Read every
    /// piece before editing, and copy a whole line only once when several carets are on it.
    fn caret_copy(&mut self, ctx: &egui::Context, cut: bool) {
        let mut carets = self.carets();
        let Some(c) = carets.first() else { return };
        let doc = &self.panes[c.pane].doc;
        let eol = doc.eol.as_str();
        carets.sort_by_key(Caret::range);
        let mut pieces = Vec::new();
        let mut whole_lines = std::collections::BTreeSet::new();
        for c in &carets {
            if c.has_selection() {
                let (a, b) = c.range();
                pieces.push((edit::text_between(&doc.lines, a, b, eol), false));
            } else if whole_lines.insert(c.pos.0) {
                if let Some(line) = doc.lines.get(c.pos.0) {
                    pieces.push((format!("{line}{eol}"), true));
                }
            }
        }
        let text = if pieces.iter().all(|(_, line)| *line) {
            pieces.iter().map(|(t, _)| t.as_str()).collect::<String>()
        } else {
            pieces
                .iter()
                .map(|(t, line)| {
                    if *line {
                        t.strip_suffix(eol).unwrap_or(t)
                    } else {
                        t
                    }
                })
                .collect::<Vec<_>>()
                .join(eol)
        };
        if !pieces.is_empty() {
            ctx.copy_text(text);
        }
        if cut {
            // Keep the primary-first order for mapping the resulting carets and undo state.
            let ranges = self
                .carets()
                .iter()
                .map(|c| {
                    if c.has_selection() {
                        c.range()
                    } else {
                        let (mut first, mut last) = (c.pos.0, c.pos.0);
                        while first > 0 && whole_lines.contains(&(first - 1)) {
                            first -= 1;
                        }
                        while whole_lines.contains(&(last + 1)) {
                            last += 1;
                        }
                        if last + 1 < doc.lines.len() {
                            ((first, 0), (last + 1, 0))
                        } else {
                            let start = if first > 0 {
                                (first - 1, doc.lines[first - 1].len())
                            } else {
                                (0, 0)
                            };
                            (start, (last, doc.lines.get(last).map_or(0, String::len)))
                        }
                    }
                })
                .collect();
            self.delete_at_carets(ranges, false);
        }
    }

    /// Paste at the carets: with as many lines as carets, one line at each, else all of `text`
    /// at each, as one undo step.
    fn paste_at_carets(&mut self, text: &str) {
        let (lines, ..) = text::split_lines(text);
        self.at_each_caret(|v, i, count| match lines.get(i) {
            Some(line) if count > 1 && lines.len() == count => v.caret_paste(line),
            _ => v.caret_paste(text),
        });
    }

    /// Moves the caret to `pos`, extending the selection with Shift.
    fn caret_to(&mut self, pos: edit::Pos, extend: bool) {
        if let Some(c) = self.caret() {
            self.place_caret(c.pane, pos, extend);
        }
    }

    /// Moves the caret `delta` lines up or down, keeping its column.
    /// The display columns lines wrap at, or 0 when they do not wrap.
    fn wrap_cols(&self) -> usize {
        self.layout.key.map_or(0, |k| k.0)
    }

    /// The wrapped display lines of `line` of `pane`, by the display column each starts at.
    fn wrap_starts_of(&self, pane: usize, line: usize) -> Vec<usize> {
        let cols = self.wrap_cols();
        match self.panes[pane].doc.lines.get(line) {
            Some(t) if cols > 0 => wrap_starts(t, self.tab_size, cols),
            _ => vec![0],
        }
    }

    /// The bytes of `line` of `pane` that its wrapped display line `seg` shows: from its start
    /// to its last place before the next display line (its end, for the last one).
    fn wrap_span(&self, pane: usize, line: usize, starts: &[usize], seg: usize) -> (usize, usize) {
        let text = self.panes[pane]
            .doc
            .lines
            .get(line)
            .map_or("", String::as_str);
        let tab = self.tab_size;
        let start = wrap_byte(text, tab, starts[seg]);
        let end = match starts.get(seg + 1) {
            Some(&next) => edit::char_before(text, wrap_byte(text, tab, next)).max(start),
            None => text.len(),
        };
        (start, end)
    }

    /// Which wrapped display line of its line the caret is on (0 without wrapping).
    fn caret_segment(&self, c: &Caret) -> usize {
        let Some(text) = self.panes[c.pane].doc.lines.get(c.pos.0) else {
            return 0;
        };
        let starts = self.wrap_starts_of(c.pane, c.pos.0);
        wrap_segment(&starts, wrap_col(text, self.tab_size, c.pos.1))
    }

    /// Home and End (⌘← and ⌘→ on macOS) on a wrapped line: the start or the end of the
    /// caret's display line, when the caret is not there yet and that is not the line's own.
    fn caret_row_edge(&self, c: &Caret, end: bool) -> Option<edit::Pos> {
        let starts = self.wrap_starts_of(c.pane, c.pos.0);
        let seg = self.caret_segment(c);
        let (a, b) = self.wrap_span(c.pane, c.pos.0, &starts, seg);
        let (to, own) = if end {
            (b, seg + 1 == starts.len())
        } else {
            (a, seg == 0)
        };
        (!own && c.pos.1 != to).then_some((c.pos.0, to))
    }

    /// Up and Down by `delta` display lines of wrapped text, keeping the column within them.
    fn caret_rows(&mut self, c: Caret, delta: isize, extend: bool) {
        let (pane, tab) = (c.pane, self.tab_size);
        let doc_lines = self.panes[pane].doc.lines.len();
        let text = |v: &Self, l: usize| v.panes[pane].doc.lines.get(l).cloned().unwrap_or_default();
        let (mut l, mut starts) = (c.pos.0, self.wrap_starts_of(pane, c.pos.0));
        let mut seg = self.caret_segment(&c);
        let want = c
            .want
            .unwrap_or_else(|| wrap_col(&text(self, l), tab, c.pos.1) - starts[seg]);
        let mut moved = 0;
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 && seg + 1 < starts.len() {
                seg += 1;
            } else if delta > 0 && l + 1 < doc_lines {
                l += 1;
                starts = self.wrap_starts_of(pane, l);
                seg = 0;
            } else if delta < 0 && seg > 0 {
                seg -= 1;
            } else if delta < 0 && l > 0 {
                l -= 1;
                starts = self.wrap_starts_of(pane, l);
                seg = starts.len() - 1;
            } else {
                break;
            }
            moved += 1;
        }
        let pos = if moved == 0 {
            // Past the first or last display line: to its start or end.
            let len = text(self, l).len();
            (l, if delta > 0 { len } else { 0 })
        } else {
            let (a, b) = self.wrap_span(pane, l, &starts, seg);
            let at = wrap_byte(&text(self, l), tab, starts[seg] + want);
            (l, at.clamp(a, b))
        };
        self.place_caret(pane, pos, extend);
        if let Some(c) = &mut self.caret {
            c.at.want = Some(want);
        }
    }

    fn caret_lines(&mut self, delta: isize, extend: bool) {
        let Some(c) = self.caret() else { return };
        if self.wrap_cols() > 0 {
            return self.caret_rows(c, delta, extend);
        }
        let lines = &self.panes[c.pane].doc.lines;
        let tab = self.tab_size;
        let want = c.want.unwrap_or_else(|| {
            let line = lines.get(c.pos.0.min(lines.len().saturating_sub(1)));
            edit::col_at(line.map_or("", String::as_str), c.pos.1, tab)
        });
        let last = lines.len().saturating_sub(1);
        let target = c.pos.0.saturating_add_signed(delta).min(last);
        let pos = match lines.get(target) {
            Some(t) if target != c.pos.0 => (target, edit::byte_at(t, want, tab).0),
            // Past the first or last line: to its start or end.
            Some(t) if delta > 0 => (target, t.len()),
            Some(_) => (target, 0),
            None => (0, 0),
        };
        self.place_caret(c.pane, pos, extend);
        if let Some(c) = &mut self.caret {
            c.at.want = Some(want);
        }
    }

    /// The keys of the caret. Returns false when there is no caret to take them.
    fn caret_keys(&mut self, ui: &Ui) -> bool {
        let Some(c) = self.caret() else { return false };
        if c.pane >= self.n() || self.table_mode {
            self.drop_carets();
            return false;
        }
        let mut inputs = Vec::new();
        let mut preedit = None;
        ui.input_mut(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Text(t) => inputs.push(CaretInput::Text(t.clone())),
                    egui::Event::Ime(egui::ImeEvent::Commit(t)) => {
                        preedit = Some(String::new());
                        inputs.push(CaretInput::Text(t.clone()));
                    }
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        preedit = Some(text.clone());
                    }
                    egui::Event::Paste(t) => inputs.push(CaretInput::Paste(t.clone())),
                    egui::Event::Copy => inputs.push(CaretInput::Copy),
                    egui::Event::Cut => inputs.push(CaretInput::Cut),
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if Self::caret_key(*key, *modifiers) => {
                        inputs.push(CaretInput::Key(*key, *modifiers));
                    }
                    _ => {}
                }
            }
            // Taken here, so that the row keys and the shortcuts leave them alone.
            for input in &inputs {
                if let CaretInput::Key(key, m) = input {
                    i.consume_key(*m, *key);
                }
            }
        });
        if let (Some(p), Some(c)) = (preedit, &mut self.caret) {
            c.preedit = p;
        }
        for input in inputs {
            match input {
                CaretInput::Text(t) => {
                    let t: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !t.is_empty() {
                        self.at_each_caret(|v, _, _| v.caret_insert(&t));
                    }
                }
                CaretInput::Paste(t) => self.paste_at_carets(&t),
                CaretInput::Copy => self.caret_copy(ui.ctx(), false),
                CaretInput::Cut => self.caret_copy(ui.ctx(), true),
                CaretInput::Key(key, m) => self.caret_key_pressed(key, m),
            }
        }
        true
    }

    /// Whether the caret takes `key` with modifiers `m`. Alt combinations stay with the
    /// difference commands, except the word keys of macOS (Option with Left, Right, Backspace
    /// and Delete).
    fn caret_key(key: Key, m: Modifiers) -> bool {
        Self::caret_key_on(MAC, key, m)
    }

    /// [`Self::caret_key`] on macOS (`mac`) or on Windows and Linux, for tests of both.
    fn caret_key_on(mac: bool, key: Key, m: Modifiers) -> bool {
        // Ctrl+Alt+Up and Down (⌘⌥ on macOS) add a caret above or below.
        if m.alt && m.command && matches!(key, Key::ArrowUp | Key::ArrowDown) {
            return true;
        }
        if m.alt {
            // `command` rather than `mac_cmd`: shortcut bindings set only `command`.
            return mac
                && !m.command
                && !m.mac_cmd
                && !m.ctrl
                && matches!(
                    key,
                    Key::ArrowLeft | Key::ArrowRight | Key::Backspace | Key::Delete
                );
        }
        match key {
            Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown => true,
            Key::Home | Key::End | Key::PageUp | Key::PageDown => true,
            Key::Enter => !m.command && !m.shift,
            // Shift+Tab outdents the caret's lines
            Key::Tab => !m.command,
            // Ctrl+Backspace and Ctrl+Delete remove a word, but on macOS ⌘ stays with the menus
            Key::Backspace | Key::Delete => !m.command || !mac,
            Key::Escape => !m.command,
            Key::A => m.command && !m.shift,
            _ => false,
        }
    }

    fn caret_key_pressed(&mut self, key: Key, m: Modifiers) {
        self.caret_key_pressed_on(MAC, key, m);
    }

    /// [`Self::caret_key_pressed`] on macOS (`mac`) or on Windows and Linux, for tests of both.
    fn caret_key_pressed_on(&mut self, mac: bool, key: Key, m: Modifiers) {
        if m.alt && m.command && matches!(key, Key::ArrowUp | Key::ArrowDown) {
            return self.add_caret_line(key == Key::ArrowDown);
        }
        if self.multi_caret() {
            match key {
                // Escape first goes back to the primary caret, with its selection.
                Key::Escape => return self.single_caret(),
                Key::A if m.command => self.single_caret(),
                Key::Backspace | Key::Delete => {
                    return self.caret_delete(key == Key::Delete, word_key_on(mac, m));
                }
                Key::Tab if m.shift => return self.caret_indent(true),
                Key::Tab if self.carets().iter().any(|c| self.tab_indents(c)) => {
                    return self.caret_indent(false)
                }
                _ => {
                    use Key::*;
                    let moves = matches!(
                        key,
                        ArrowLeft
                            | ArrowRight
                            | ArrowUp
                            | ArrowDown
                            | Home
                            | End
                            | PageUp
                            | PageDown
                    );
                    return self.at_each_caret_with(!moves, |v, _, _| v.caret_key_one(mac, key, m));
                }
            }
        }
        self.caret_key_one(mac, key, m);
    }

    /// One caret's key, for [`Self::caret_key_pressed_on`].
    fn caret_key_one(&mut self, mac: bool, key: Key, m: Modifiers) {
        let Some(c) = self.caret() else { return };
        let lines = &self.panes[c.pane].doc.lines;
        let shift = m.shift;
        let end = lines
            .len()
            .checked_sub(1)
            .map_or((0, 0), |l| (l, lines[l].len()));
        let line_len = lines.get(c.pos.0).map_or(0, String::len);
        // Left and Right first end a selection at its side.
        let (start, stop) = c.range();
        let word = word_key_on(mac, m);
        match key {
            Key::ArrowLeft if word => self.caret_to(edit::word_left(lines, c.pos), shift),
            Key::ArrowRight if word => self.caret_to(edit::word_right(lines, c.pos), shift),
            Key::ArrowLeft if m.mac_cmd => {
                let to = self.caret_row_edge(&c, false).unwrap_or((c.pos.0, 0));
                self.caret_to(to, shift);
            }
            Key::ArrowRight if m.mac_cmd => {
                let to = self.caret_row_edge(&c, true).unwrap_or((c.pos.0, line_len));
                self.caret_to(to, shift);
            }
            Key::ArrowUp if m.mac_cmd => self.caret_to((0, 0), shift),
            Key::ArrowDown if m.mac_cmd => self.caret_to(end, shift),
            Key::ArrowLeft if c.has_selection() && !shift => self.caret_to(start, false),
            Key::ArrowRight if c.has_selection() && !shift => self.caret_to(stop, false),
            Key::ArrowLeft => {
                let to = edit::before(lines, c.pos);
                self.caret_to(to, shift);
            }
            Key::ArrowRight => {
                let to = edit::after(lines, c.pos);
                self.caret_to(to, shift);
            }
            Key::ArrowUp => self.caret_lines(-1, shift),
            Key::ArrowDown => self.caret_lines(1, shift),
            Key::PageUp => self.caret_lines(-(self.page_rows.max(1) as isize), shift),
            Key::PageDown => self.caret_lines(self.page_rows.max(1) as isize, shift),
            Key::Home if m.command => self.caret_to((0, 0), shift),
            Key::End if m.command => self.caret_to(end, shift),
            Key::Home => {
                // On a wrapped line, to the start of the display line first; then to the
                // first character that is not a space, then to the line start.
                let indent = lines
                    .get(c.pos.0)
                    .map_or(0, |t| t.len() - t.trim_start_matches([' ', '\t']).len());
                let to = if c.pos.1 == indent { 0 } else { indent };
                let to = self.caret_row_edge(&c, false).unwrap_or((c.pos.0, to));
                self.caret_to(to, shift);
            }
            Key::End => {
                // On a wrapped line, to the end of the display line first, then the line's.
                let to = self.caret_row_edge(&c, true).unwrap_or((c.pos.0, line_len));
                self.caret_to(to, shift);
            }
            Key::Enter => self.caret_newline(),
            Key::Tab if shift => self.caret_indent(true),
            Key::Tab if self.tab_indents(&c) => self.caret_indent(false),
            Key::Tab => self.caret_insert("\t"),
            Key::Backspace => self.caret_delete(false, word),
            Key::Delete => self.caret_delete(true, word),
            Key::A => {
                self.place_caret(c.pane, (0, 0), false);
                self.place_caret(c.pane, end, true);
            }
            Key::Escape if c.has_selection() => self.caret_to(c.pos, false),
            Key::Escape => self.drop_carets(),
            _ => {}
        }
    }

    fn col(&self) -> Option<ColSel> {
        self.col_sel.as_deref().copied()
    }

    /// The text of the column selection, one line per row with a line.
    pub fn col_text(&self) -> Option<String> {
        let s = self.col()?;
        let (r0, r1) = s.rows();
        let doc = &self.panes.get(s.pane)?.doc;
        let lines: Vec<&str> = doc.lines[self.line_range(s.pane, r0, r1)]
            .iter()
            .map(String::as_str)
            .collect();
        (!lines.is_empty())
            .then(|| edit::take(&lines, &s.cols(), self.tab_size).join(doc.eol.as_str()))
    }

    /// Replaces the column selection with `with` (one line goes into every row, several go
    /// row by row) as one undo step, and leaves a caret after the new text.
    pub fn col_replace(&mut self, with: &[String]) {
        let Some(s) = self.col() else { return };
        if !self.writable(s.pane) {
            return;
        }
        let (r0, r1) = s.rows();
        let range = self.line_range(s.pane, r0, r1);
        let cols = s.cols();
        if range.is_empty() || (cols.is_empty() && with.iter().all(String::is_empty)) {
            return;
        }
        let tab = self.tab_size;
        // Only the lines the text goes into are copied and compared, not the whole pane.
        let doc = &self.panes[s.pane].doc.lines;
        let reach = range
            .len()
            .max(if with.len() == 1 { 0 } else { with.len() });
        let end = (range.start + reach).min(doc.len());
        let mut part = doc[range.start..end].to_vec();
        edit::replace(&mut part, 0, range.len(), &cols, with, tab);
        if part == doc[range.start..end] {
            return;
        }
        self.push_undo();
        self.panes[s.pane].doc.lines.splice(range.start..end, part);
        self.mark_changed(&[s.pane]);
        // The caret goes after the text put in, on the rows the selection covered.
        let width = with.iter().map(|w| edit::col_at(w, w.len(), tab)).max();
        let col = cols.start
            + if with.len() == 1 {
                width.unwrap_or(0)
            } else {
                0
            };
        let last = range.start + range.len().max(with.len()) - 1;
        let len = self.panes[s.pane].doc.lines.len();
        let (a, b) = (
            self.diff.row_of_line(s.pane, range.start),
            self.diff
                .row_of_line(s.pane, last.min(len.saturating_sub(1))),
        );
        let b = a.max(b);
        self.col_sel = Some(Box::new(ColSel {
            pane: s.pane,
            anchor: (a, col),
            head: (b, col),
        }));
        self.cursor_row = Some(b.min(self.diff.rows.len().saturating_sub(1)));
    }

    /// Backspace (`forward` false) or Delete on the column selection: removes the selected
    /// columns, or one column on each side of a caret.
    fn col_delete(&mut self, forward: bool) {
        let Some(mut s) = self.col() else { return };
        let cols = s.cols();
        if cols.is_empty() {
            let c = cols.start;
            let (from, to) = if forward {
                (c, c + 1)
            } else {
                (c.saturating_sub(1), c)
            };
            if from == to {
                return;
            }
            s.anchor.1 = from;
            s.head.1 = to;
            self.col_sel = Some(Box::new(s));
        }
        self.col_replace(&[]);
    }

    /// The rows Cut and Delete act on: the selected rows, or the cursor row.
    fn edit_rows(&self) -> Option<(usize, usize)> {
        self.selection()
            .or_else(|| self.cursor_row.map(|r| (r, r)))
            .filter(|&(_, b)| b < self.diff.rows.len())
    }

    fn insert_below(&mut self, row: usize, pane: usize) {
        if !self.writable(pane) {
            return;
        }
        let at = match self.diff.rows.get(row).and_then(|r| r.line(pane)) {
            Some(l) => l + 1,
            None => self.insert_index(pane, row),
        };
        self.push_undo();
        self.panes[pane].doc.lines.insert(at, String::new());
        self.mark_changed(&[pane]);
        let r = self.diff.row_of_line(pane, at);
        self.cursor_row = Some(r);
        self.begin_edit(r, pane);
    }

    /// Pastes `text` into the active pane: it replaces an empty document or the selected lines,
    /// and is otherwise inserted below the cursor row.
    pub fn paste(&mut self, text: &str) {
        let p = self.active_pane;
        if p >= self.n() || !self.writable(p) {
            return;
        }
        let (new_lines, _, _, final_newline) = text::split_lines(text);
        if new_lines.is_empty() {
            return;
        }
        let doc = &self.panes[p].doc;
        let blank = doc.lines.len() <= 1 && doc.lines.iter().all(String::is_empty);
        let range = if blank {
            0..doc.lines.len()
        } else if let Some((r0, r1)) = self.selection() {
            self.line_range(p, r0, r1)
        } else if let Some(r) = self.cursor_row {
            let at = self.insert_index(p, r + 1);
            at..at
        } else {
            doc.lines.len()..doc.lines.len()
        };
        let n = new_lines.len();
        let last = range.start + n - 1;
        self.push_undo();
        let doc = &mut self.panes[p].doc;
        if blank {
            doc.final_newline = final_newline;
        }
        doc.lines.splice(range, new_lines);
        self.mark_changed(&[p]);
        self.sel_anchor = None;
        let row = self.diff.row_of_line(p, last);
        self.cursor_row = Some(row);
        self.scroll_to = Some(row);
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block());
        self.status = trf("msg.pasted_lines", &[("n", n.to_string())]);
    }

    fn delete_line(&mut self, row: usize, pane: usize) {
        if self.diff.rows.get(row).and_then(|r| r.line(pane)).is_some() {
            self.delete_rows(pane, row, row);
        }
    }

    // ---------- files ----------

    pub fn save_pane(&mut self, p: usize) -> anyhow::Result<bool> {
        self.commit_pending_edit();
        if self.panes[p].doc.compare_only {
            anyhow::bail!("{}", self.read_only_message(p));
        }
        let doc = &mut self.panes[p].doc;
        if doc.read_only {
            anyhow::bail!("{}", tr("msg.readonly"));
        }
        let path = match doc.path.clone() {
            Some(path) => path,
            None => {
                let Some(path) = rfd::FileDialog::new()
                    .set_title(tr("dlg.save_as"))
                    .save_file()
                else {
                    return Ok(false);
                };
                path
            }
        };
        // A file read-only on disk is refused before the pane takes its name.
        text::refuse_read_only(&path)?;
        doc.path = Some(path);
        doc.save()?;
        self.status = trf("msg.saved", &[("file", doc.title())]);
        self.mark_saved(p);
        self.saved = true;
        self.rediff();
        Ok(true)
    }

    fn mark_saved(&mut self, p: usize) {
        let pane = &mut self.panes[p];
        pane.saves += 1;
        pane.disk_time = disk_time(&pane.doc);
        self.disk_changed.retain(|&c| c != p);
        // Typing after a save is a new undo step, so that undo can stop at the saved text.
        self.typing_group = false;
    }

    /// Reacts to files changed by another program: unmodified panes are reloaded, and panes with
    /// unsaved edits are listed in a warning bar.
    pub fn check_disk(&mut self) {
        let times: Vec<_> = self.panes.iter().map(|p| disk_time(&p.doc)).collect();
        if self
            .panes
            .iter()
            .zip(&times)
            .any(|(p, now)| p.disk_time.is_some() && now.is_none())
        {
            self.invalidate_review();
            self.merge_validation = None;
        }
        let changed = |(p, now): (&Pane, &Option<SystemTime>)| now.is_some() && *now != p.disk_time;
        if !self.panes.iter().zip(&times).any(changed) {
            return;
        }
        self.invalidate_review();
        self.merge_validation = None;
        // A line still being typed is an edit of its file, which is then not reloaded.
        self.commit_pending_edit();
        let mut reloaded = Vec::new();
        let mut failed = None;
        for (i, (p, now)) in self.panes.iter_mut().zip(times).enumerate() {
            if now.is_none() || now == p.disk_time {
                continue;
            }
            if p.doc.modified || p.doc.description.is_some() {
                p.disk_time = now;
                if p.doc.modified && !self.disk_changed.contains(&i) {
                    self.disk_changed.push(i);
                }
                continue;
            }
            // A file that cannot be read (say, still being written) is tried again next time.
            match p.reload() {
                Ok(()) => reloaded.push(p.doc.title()),
                Err(e) => failed = Some(e),
            }
        }
        if !reloaded.is_empty() {
            self.typing_group = false;
            self.rediff();
            self.status = trf("msg.reloaded", &[("file", reloaded.join(", "))]);
        }
        if let Some(e) = failed {
            self.status = e;
        }
    }

    /// Replaces the listed panes with the file contents on disk, discarding their edits (undoable).
    fn reload_changed(&mut self) {
        let panes = std::mem::take(&mut self.disk_changed);
        self.hold_view();
        self.push_undo();
        for p in panes {
            let pane = &mut self.panes[p];
            let Some(path) = pane.doc.path.clone() else {
                continue;
            };
            match text::load_doc(&path, Some(pane.doc.encoding)) {
                Ok(mut d) => {
                    d.read_only = pane.doc.read_only;
                    d.compare_only = pane.doc.compare_only;
                    let before = std::mem::replace(&mut pane.doc, d).lines;
                    pane.hl_dirty = true;
                    pane.saves += 1;
                    pane.disk_time = disk_time(&pane.doc);
                    pane.reset_baseline();
                    // Bookmarks, synchronization points and the view stay with their lines.
                    self.remap_bookmarks(p, &before);
                    self.remap_sync_points(p, &before);
                    self.remap_view(p, &before);
                }
                Err(e) => self.status = e.to_string(),
            }
        }
        self.rediff();
    }

    pub fn save_pane_as(&mut self, p: usize) -> anyhow::Result<bool> {
        self.commit_pending_edit();
        let mut dlg = rfd::FileDialog::new().set_title(tr("dlg.save_as"));
        if let Some(path) = &self.panes[p].doc.path {
            if let Some(dir) = path.parent() {
                dlg = dlg.set_directory(dir);
            }
            if let Some(name) = path.file_name() {
                dlg = dlg.set_file_name(name.to_string_lossy());
            }
        }
        let Some(path) = dlg.save_file() else {
            return Ok(false);
        };
        self.save_pane_to(p, &path)?;
        Ok(true)
    }

    /// Writes pane `p` to `path`, which it is then the file of, for *Save As*.
    fn save_pane_to(&mut self, p: usize, path: &std::path::Path) -> anyhow::Result<()> {
        let doc = &self.panes[p].doc;
        let canonical = |p: &std::path::Path| std::fs::canonicalize(p).ok();
        if doc.compare_only
            && canonical(path).is_some()
            && doc.path.as_deref().and_then(canonical) == canonical(path)
        {
            anyhow::bail!("{}", self.read_only_message(p));
        }
        // Text read from a document, written over a file of a kind that is itself read by
        // converting it, would replace that file's contents with something it cannot hold;
        // whether such files are read as text now does not change that.
        if doc.compare_only
            && (crate::core::unpacker::find(&self.opts, path).is_some()
                || crate::core::extract::Kind::of(path).is_some())
        {
            anyhow::bail!(
                "{}",
                trf(
                    "msg.extracted_over_document",
                    &[("file", path.display().to_string())]
                )
            );
        }
        text::refuse_read_only(path)?;
        self.panes[p].original = None;
        let doc = &mut self.panes[p].doc;
        doc.save_as(path)?;
        doc.read_only = false;
        doc.compare_only = false;
        doc.description = None;
        self.status = trf("msg.saved", &[("file", doc.title())]);
        self.mark_saved(p);
        self.rediff();
        Ok(())
    }

    pub fn save_modified(&mut self) -> anyhow::Result<()> {
        self.commit_pending_edit();
        for p in 0..self.n() {
            if self.panes[p].doc.modified {
                self.save_pane(p)?;
            }
        }
        Ok(())
    }

    pub fn reload_encoding(&mut self, p: usize, enc: TextEncoding) {
        let Some(path) = self.panes[p].doc.path.clone() else {
            return;
        };
        match text::load_doc(&path, Some(enc)) {
            Ok(mut d) => {
                let old = &self.panes[p].doc;
                d.read_only = old.read_only;
                d.compare_only = old.compare_only;
                d.description = old.description.clone();
                let format = format_of(old);
                self.hold_view();
                if !d.read_only {
                    // Reading the file again discards its edits, which undo brings back.
                    self.push_undo();
                    self.undo.last_mut().unwrap().formats[p] = Some(format);
                }
                let before = std::mem::replace(&mut self.panes[p].doc, d).lines;
                let pane = &mut self.panes[p];
                pane.hl_dirty = true;
                pane.saves += 1;
                pane.disk_time = disk_time(&pane.doc);
                pane.reset_baseline();
                self.remap_bookmarks(p, &before);
                self.remap_sync_points(p, &before);
                self.remap_view(p, &before);
                self.invalidate_review();
                self.rediff();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Reloads unmodified files and the compare rules file from disk and recomputes the
    /// differences.
    pub fn refresh(&mut self) {
        let before = self.review_decision.as_ref().map(|_| {
            (
                self.opts.clone(),
                self.panes
                    .iter()
                    .map(|p| crate::core::merge_validation::doc_fingerprint(&p.doc))
                    .collect::<Vec<_>>(),
            )
        });
        self.commit_pending_edit();
        self.reload_rules();
        if let Some(o) = self.options_for(&self.opts) {
            self.opts = o;
        }
        for p in &mut self.panes {
            if p.doc.modified || p.doc.description.is_some() {
                continue;
            }
            let _ = p.reload();
        }
        self.typing_group = false;
        if before.is_some_and(|(options, digests)| {
            options != self.opts
                || digests
                    != self
                        .panes
                        .iter()
                        .map(|p| crate::core::merge_validation::doc_fingerprint(&p.doc))
                        .collect::<Vec<_>>()
        }) {
            self.invalidate_review();
        }
        self.rediff();
    }

    pub fn make_patch(&self, style: PatchStyle, context: usize) -> Result<Option<String>, String> {
        if self.n() != 2 {
            return Ok(None);
        }
        let a = &self.panes[0].doc;
        let b = &self.panes[1].doc;
        let date = |d: &TextDoc| {
            d.path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .map(|t| crate::core::report::fmt_time(Some(t)))
                .unwrap_or_default()
        };
        let input = patch::PatchInput {
            name_a: &a.title(),
            name_b: &b.title(),
            date_a: &date(a),
            date_b: &date(b),
            a: &a.lines,
            b: &b.lines,
            final_newline_a: a.final_newline,
            final_newline_b: b.final_newline,
        };
        patch::make_doc_patch(&input, &[a, b], &self.opts, style, context).map(Some)
    }

    /// The title of every side.
    pub fn titles(&self) -> Vec<String> {
        self.panes.iter().map(|p| p.doc.title()).collect()
    }

    /// Makes the HTML report of the compare as it is now, on any thread; `default_rules` is the
    /// compare rules file of the options, used when the compare has none of its own. A rules
    /// file that could not be read is not named: the compare runs without it.
    pub fn html_report_job(&self, default_rules: &str) -> crate::core::report::ReportJob {
        self.report_job(crate::core::report::ReportFormat::Html, default_rules)
    }

    /// Makes the report of this compare as `format` (HTML, XML or a slideshow); it can run on
    /// another thread.
    pub fn report_job(
        &self,
        format: crate::core::report::ReportFormat,
        default_rules: &str,
    ) -> crate::core::report::ReportJob {
        let titles = self.titles();
        let lines: Vec<Vec<String>> = self.panes.iter().map(|p| p.doc.lines.clone()).collect();
        let rules = self.report_rules(default_rules);
        let (diff, opts, notes) = (self.diff.clone(), self.opts.clone(), self.bookmarks.clone());
        let table = self.keyed().map(|d| {
            let summary = self.table_report_lines(d);
            (
                self.table_match.clone(),
                d.counts,
                d.warnings.clone(),
                summary,
            )
        });
        Box::new(move || {
            let refs: Vec<&[String]> = lines.iter().map(Vec::as_slice).collect();
            let report = crate::core::report::text_report(
                format, &titles, &refs, &diff, &opts, &notes, &rules,
            );
            match &table {
                Some((m, counts, warnings, summary)) => {
                    table_match::annotate_report(report, format, m, (counts, warnings), summary)
                }
                None => report,
            }
        })
    }

    /// The compare rules file a report names: the tab's own, or `default_rules`, when found.
    fn report_rules(&self, default_rules: &str) -> String {
        let found = match &self.rules {
            Some(_) => self.rule_set.is_some(),
            None => rules::find(default_rules).is_some(),
        };
        match &self.rules {
            Some(r) if found => r.clone(),
            None if found => default_rules.to_string(),
            _ => String::new(),
        }
    }

    /// Makes the PDF report of this compare, with the same content as its HTML report; it loads
    /// the fonts for `lang` and lays out every page, so it runs on another thread.
    pub fn pdf_job(
        &self,
        default_rules: &str,
        pdf: crate::core::pdf_report::PdfOptions,
        lang: String,
    ) -> crate::core::pdf_report::PdfJob {
        let titles = self.titles();
        let lines: Vec<Vec<String>> = self.panes.iter().map(|p| p.doc.lines.clone()).collect();
        let rules = self.report_rules(default_rules);
        let (diff, opts, notes) = (self.diff.clone(), self.opts.clone(), self.bookmarks.clone());
        let sections: Vec<crate::core::pdf_report::Section> = self
            .keyed()
            .map(|d| {
                let mut summary = self.table_report_lines(d).into_iter();
                crate::core::pdf_report::Section {
                    title: summary.next().unwrap_or_default(),
                    lines: summary.collect(),
                    mono: false,
                }
            })
            .into_iter()
            .collect();
        Box::new(move || {
            let refs: Vec<&[String]> = lines.iter().map(Vec::as_slice).collect();
            let fonts = crate::ui::fonts::document_fonts(&lang);
            crate::core::pdf_report::text_pdf(
                &titles, &refs, &diff, &opts, &notes, &rules, &sections, &fonts, &pdf,
            )
        })
    }

    /// What matching by key used and found, in words: a heading, the settings, the counts and
    /// the warnings.
    fn table_report_lines(&self, d: &TableDiff) -> Vec<String> {
        let m = &self.table_match;
        let names: Vec<String> = self
            .table
            .docs
            .first()
            .and_then(|t| t.as_ref().ok())
            .filter(|_| m.header)
            .and_then(|t| t.records.first())
            .map(|r| r.cells.iter().map(|c| c.trim().to_string()).collect())
            .unwrap_or_default();
        let keys: Vec<String> = m.keys.iter().map(|k| key_label(k, &names)).collect();
        let mut settings = vec![format!("{}: {}", tr("table.choose_keys"), keys.join(" + "))];
        if m.header {
            settings.push(tr("table.header_row").to_string());
        }
        settings.push(format!(
            "{}: {}",
            tr("table.columns_by"),
            tr(match m.columns {
                table_match::ColumnPolicy::Position => "table.columns_position",
                table_match::ColumnPolicy::Name => "table.columns_name",
            })
        ));
        settings.push(format!(
            "{}: {}",
            tr("table.order"),
            tr(match m.order {
                table_match::RowOrder::Left => "table.order_left",
                table_match::RowOrder::Right => "table.order_right",
                table_match::RowOrder::Key => "table.order_key",
            })
        ));
        if m.ignore_order {
            settings.push(tr("table.ignore_order").to_string());
        }
        let tolerance = tolerance_text(m);
        if !tolerance.is_empty() {
            settings.push(format!("{}: {tolerance}", tr("table.tolerance")));
        }
        let n = d.counts;
        let mut lines = vec![
            tr("table.report_title").to_string(),
            settings.join(" · "),
            trf(
                "table.summary",
                &[
                    ("added", n.added.to_string()),
                    ("removed", n.removed.to_string()),
                    ("modified", n.modified.to_string()),
                    ("moved", n.moved.to_string()),
                    ("ambiguous", n.ambiguous.to_string()),
                ],
            ),
        ];
        lines.extend(d.warnings.iter().map(warning_text));
        lines
    }

    /// The compare with its bookmarks, to save as one file.
    pub fn saved_compare(&self) -> SavedCompare {
        let n = self.n();
        let mut saved = SavedCompare::new(
            self.panes
                .iter()
                .zip(&self.bookmarks)
                .map(|(p, marks)| SavedPane {
                    title: p.doc.title(),
                    path: p.doc.path.as_ref().map(|p| p.display().to_string()),
                    text: p.doc.text(),
                    syntax: p
                        .doc
                        .path
                        .as_deref()
                        .map(text::extension)
                        .or_else(|| p.doc.syntax.clone()),
                    bookmarks: marks.iter().map(|(&l, n)| (l + 1, n.clone())).collect(),
                })
                .collect(),
        );
        saved.sync_points = self
            .sync_points
            .iter()
            .map(|s| s[..n].iter().map(|l| l + 1).collect())
            .collect();
        saved
    }

    /// A read-only compare of a saved comparison, with its bookmarks.
    pub fn from_saved(saved: &SavedCompare, cfg: &Config) -> TextView {
        let docs = saved
            .panes
            .iter()
            .map(|p| {
                let mut d = TextDoc::from_text(None, &p.text);
                d.description = Some(p.title.clone());
                d.syntax = p.syntax.clone();
                d.read_only = true;
                d.compare_only = true;
                d
            })
            .collect();
        let mut v = TextView::new(docs, cfg);
        for (p, pane) in saved.panes.iter().enumerate() {
            let n = v.panes[p].doc.lines.len();
            v.bookmarks[p] = pane
                .bookmarks
                .iter()
                .filter(|(&l, _)| (1..=n).contains(&l))
                .map(|(&l, c)| (l - 1, c.clone()))
                .collect();
        }
        let n = v.n();
        let points: Vec<SyncPoint> = saved
            .sync_points
            .iter()
            .filter(|s| s.len() == n && s.iter().all(|&l| l >= 1))
            .map(|s| {
                let mut p = [0; 3];
                for (i, &l) in s.iter().enumerate() {
                    p[i] = l - 1;
                }
                p
            })
            .collect();
        if !points.is_empty() {
            // Ones that don't fit these texts are left out by the compare.
            v.sync_points = points;
            v.rediff();
        }
        v
    }

    /// Line statistics of every pair of sides: the pair's name, then its counts.
    pub fn statistics(&self) -> Vec<(String, diff::Stats)> {
        let n = self.n();
        let name = |a: usize, b: usize| format!("{} ↔ {}", side_name(a, n), side_name(b, n));
        if n == 2 {
            return vec![(name(0, 1), self.diff.stats())];
        }
        let syntax = self.comment_syntax();
        [(0, 1), (1, 2), (0, 2)]
            .into_iter()
            .map(|(a, b)| {
                let refs = [
                    self.panes[a].doc.lines.as_slice(),
                    self.panes[b].doc.lines.as_slice(),
                ];
                (
                    name(a, b),
                    diff::compute_with(&refs, &self.opts, syntax).stats(),
                )
            })
            .collect()
    }

    /// What one level of indentation is in `pane`: a tab when the file indents more lines with
    /// tabs than with spaces, and otherwise the tab size in spaces.
    fn indent_unit(&self, pane: usize) -> String {
        let doc = &self.panes[pane].doc;
        let (tabs, spaces) = doc.lines.iter().take(5000).fold((0, 0), |(t, s), l| {
            (
                t + l.starts_with('\t') as usize,
                s + l.starts_with(' ') as usize,
            )
        });
        if tabs > spaces {
            "\t".to_string()
        } else {
            " ".repeat(self.tab_size.max(1))
        }
    }

    /// `line` indented by `unit`, or outdented by one level (a leading tab, or up to the tab
    /// size in spaces), and the bytes added or removed at its start. A blank line is not
    /// indented, so that indenting adds no trailing spaces.
    fn indent_line(line: &str, outdent: bool, unit: &str, tab: usize) -> (String, usize) {
        if outdent {
            let cut = if line.starts_with('\t') {
                1
            } else {
                (line.len() - line.trim_start_matches(' ').len()).min(tab.max(1))
            };
            (line[cut..].to_string(), cut)
        } else if line.trim().is_empty() {
            (line.to_string(), 0)
        } else {
            (format!("{unit}{line}"), unit.len())
        }
    }

    /// Whether Tab at the caret indents its lines rather than typing a tab: when the selection
    /// goes over more than one line, or is a whole line.
    fn tab_indents(&self, c: &Caret) -> bool {
        let (a, b) = c.range();
        let len = self.panes[c.pane].doc.lines.get(a.0).map_or(0, String::len);
        b.0 > a.0 || (a.1 == 0 && b.1 == len && len > 0 && a != b)
    }

    /// Tab (indent) or Shift+Tab (`outdent`) at the caret: the lines of the selection, or the
    /// caret's line, as one undo step; a selection ending at the start of a line leaves that
    /// line alone. Both ends of the selection move with the indentation of their lines, so
    /// outdenting right after indenting gives back the same text and selection.
    fn caret_indent(&mut self, outdent: bool) {
        let carets = self.carets();
        let Some(&c) = carets.first() else { return };
        if c.pane >= self.n() || self.matching() || !self.writable(c.pane) {
            return;
        }
        let (unit, tab) = (self.indent_unit(c.pane), self.tab_size);
        let doc = &self.panes[c.pane].doc;
        let top = doc.lines.len().saturating_sub(1);
        // The lines of every caret, each once.
        let mut wanted: Vec<usize> = carets
            .iter()
            .flat_map(|k| {
                let (a, b) = k.range();
                let last = if b.0 > a.0 && b.1 == 0 { b.0 - 1 } else { b.0 };
                a.0..=last.min(top)
            })
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        let changed: Vec<(usize, String, usize)> = wanted
            .into_iter()
            .filter_map(|l| {
                let (text, n) = Self::indent_line(doc.lines.get(l)?, outdent, &unit, tab);
                (n > 0).then_some((l, text, n))
            })
            .collect();
        if changed.is_empty() {
            return;
        }
        self.push_undo();
        for (l, text, _) in &changed {
            self.panes[c.pane].doc.lines[*l] = text.clone();
        }
        self.mark_changed(&[c.pane]);
        let moved = |(l, at): edit::Pos| match changed.iter().find(|(cl, ..)| *cl == l) {
            Some(&(_, _, n)) if outdent => (l, at.saturating_sub(n)),
            Some(&(_, _, n)) if at > 0 => (l, at + n),
            _ => (l, at),
        };
        let list = carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let k = Caret {
                    pos: moved(k.pos),
                    anchor: moved(k.anchor),
                    want: None,
                    ..*k
                };
                (k, i == 0)
            })
            .collect();
        self.set_carets(list);
    }

    /// Indents (or outdents) the active pane's lines in the selected rows, or on the cursor
    /// row, by a tab when the file indents with tabs and by the tab size in spaces otherwise.
    /// With the text caret in the pane, its lines are the ones indented.
    pub fn indent(&mut self, outdent: bool) {
        if self.shows_inline() {
            return;
        }
        if self.caret().is_some_and(|c| c.pane == self.active_pane) {
            return self.caret_indent(outdent);
        }
        let p = self.active_pane;
        let Some((r0, r1)) = self.selection().or_else(|| self.cursor_row.map(|r| (r, r))) else {
            return;
        };
        let lines: Vec<usize> = self.diff.rows[r0..=r1]
            .iter()
            .filter_map(|r| r.line(p))
            .collect();
        let (Some(&first), Some(&last)) = (lines.first(), lines.last()) else {
            return;
        };
        if !self.writable(p) {
            return;
        }
        let (unit, tab) = (self.indent_unit(p), self.tab_size);
        let doc = &self.panes[p].doc;
        let changed: Vec<String> = lines
            .iter()
            .map(|&l| Self::indent_line(&doc.lines[l], outdent, &unit, tab).0)
            .collect();
        if lines.iter().zip(&changed).all(|(&l, c)| doc.lines[l] == *c) {
            return;
        }
        let forward = self.sel_anchor.is_none_or(|a| Some(a) <= self.cursor_row);
        self.push_undo();
        for (&l, c) in lines.iter().zip(changed) {
            self.panes[p].doc.lines[l] = c;
        }
        self.mark_changed(&[p]);
        let (a, b) = (
            self.diff.row_of_line(p, first),
            self.diff.row_of_line(p, last),
        );
        let (anchor, cursor) = if forward { (a, b) } else { (b, a) };
        self.sel_anchor = (r0 != r1).then_some(anchor);
        self.cursor_row = Some(cursor);
    }

    /// The lines the line commands act on, as their pane and runs of lines (first, last), in
    /// order and apart: with the text caret in the active pane, the lines of each caret's
    /// selection (one ending at the start of a line leaves that line out) or its line;
    /// otherwise the active pane's lines in the selected rows, or on the cursor row.
    fn command_lines(&self) -> Option<(usize, Vec<(usize, usize)>)> {
        let carets = self.carets();
        if let Some(c) = carets.first().filter(|c| c.pane == self.active_pane) {
            let mut runs: Vec<(usize, usize)> = carets
                .iter()
                .map(|k| {
                    let (a, b) = k.range();
                    (a.0, if b.0 > a.0 && b.1 == 0 { b.0 - 1 } else { b.0 })
                })
                .collect();
            runs.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
            for (f, l) in runs {
                match merged.last_mut() {
                    // Next to each other: moving them apart would swap their lines.
                    Some(last) if f <= last.1 + 1 => last.1 = last.1.max(l),
                    _ => merged.push((f, l)),
                }
            }
            return Some((c.pane, merged));
        }
        let p = self.active_pane;
        let (r0, r1) = self
            .selection()
            .or_else(|| self.cursor_row.map(|r| (r, r)))?;
        let mut lines = self
            .diff
            .rows
            .get(r0..=r1)?
            .iter()
            .filter_map(|r| r.line(p));
        let first = lines.next()?;
        Some((p, vec![(first, lines.next_back().unwrap_or(first))]))
    }

    /// Duplicate lines: puts a copy of the lines of [`Self::command_lines`] under them and selects
    /// the copy, as one undo step.
    pub fn duplicate_lines(&mut self) {
        self.line_command(None);
    }

    /// Move lines up (`up`) or down: moves the lines of [`Self::command_lines`] past the line
    /// next to them, with their selection, as one undo step.
    pub fn move_lines(&mut self, up: bool) {
        self.line_command(Some(up));
    }

    /// Duplicates the runs of [`Self::command_lines`] (`moving` `None`), or moves them up or
    /// down; a move none of them can make changes nothing.
    fn line_command(&mut self, moving: Option<bool>) {
        if self.shows_inline() || self.matching() {
            return;
        }
        let Some((p, runs)) = self.command_lines() else {
            return;
        };
        let len = self.panes[p].doc.lines.len();
        if runs.last().is_none_or(|&(_, l)| l >= len) || !self.writable(p) {
            return;
        }
        let mut lines = self.panes[p].doc.lines.clone();
        // How far down each run's selection goes.
        let shifts: Vec<isize> = match moving {
            None => {
                for &(f, l) in runs.iter().rev() {
                    edit::duplicate_lines(&mut lines, f, l);
                }
                let mut total = 0;
                runs.iter()
                    .map(|&(f, l)| {
                        total += (l - f + 1) as isize;
                        total
                    })
                    .collect()
            }
            Some(up) => {
                let blocked = if up {
                    runs[0].0 == 0
                } else {
                    runs[runs.len() - 1].1 + 1 >= len
                };
                if blocked {
                    return;
                }
                for &(f, l) in &runs {
                    edit::move_lines(&mut lines, f, l, up);
                }
                vec![if up { -1 } else { 1 }; runs.len()]
            }
        };
        let carets: Vec<Caret> = self.carets().into_iter().filter(|c| c.pane == p).collect();
        let forward = self.sel_anchor.is_none_or(|a| Some(a) <= self.cursor_row);
        self.push_undo();
        self.panes[p].doc.lines = lines;
        self.mark_changed(&[p]);
        let shift_of = |l: usize| {
            let i = runs
                .iter()
                .position(|&(_, last)| l <= last)
                .unwrap_or(runs.len() - 1);
            shifts[i]
        };
        if carets.is_empty() {
            let (first, last) = runs[0];
            let a = self
                .diff
                .row_of_line(p, first.saturating_add_signed(shifts[0]));
            let b = self
                .diff
                .row_of_line(p, last.saturating_add_signed(shifts[0]));
            let (anchor, cursor) = if forward { (a, b) } else { (b, a) };
            self.sel_anchor = (a != b).then_some(anchor);
            self.cursor_row = Some(cursor);
            self.scroll_to = Some(cursor);
            return;
        }
        // A selection that ended at the start of the line under its lines ends at the end of the
        // file once they are the last lines.
        let doc = &self.panes[p].doc.lines;
        let end = doc
            .len()
            .checked_sub(1)
            .map_or((0, 0), |l| (l, doc[l].len()));
        let list = carets
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let by = shift_of(c.range().0 .0);
                let to = |(l, at): edit::Pos| {
                    let l = l.saturating_add_signed(by);
                    if l > end.0 {
                        end
                    } else {
                        (l, at)
                    }
                };
                let k = Caret {
                    pos: to(c.pos),
                    anchor: to(c.anchor),
                    want: None,
                    ..*c
                };
                (k, i == 0)
            })
            .collect();
        self.set_carets(list);
    }

    // ---------- find ----------

    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    pub fn open_replace(&mut self) {
        self.open_find();
        self.find.show_replace = true;
    }

    /// The compiled find query, or why it does not compile.
    fn matcher(&mut self) -> Option<Result<Regex, String>> {
        if self.find.query.is_empty() {
            return None;
        }
        let key = self.find.key();
        if self.find_re.as_deref().map(|(k, _)| k) != Some(&key) {
            let re = find_regex(&key.0, key.1, key.2, key.3).map_err(|e| e.to_string());
            self.find_re = Some(Box::new((key, re)));
        }
        self.find_re.as_deref().map(|(_, re)| re.clone())
    }

    /// The compiled find query, with a status message when it is not a valid pattern.
    fn valid_matcher(&mut self) -> Option<Regex> {
        match self.matcher()? {
            Ok(re) => Some(re),
            Err(e) => {
                self.status = trf("msg.bad_regex", &[("error", e)]);
                None
            }
        }
    }

    pub fn find_next(&mut self, forward: bool) {
        let Some(re) = self.valid_matcher() else {
            return;
        };
        let pane = self.active_pane;
        let total = self.diff.rows.len();
        if total == 0 {
            return;
        }
        let start = self
            .cursor_row
            .unwrap_or(if forward { total - 1 } else { 0 });
        for k in 1..=total {
            let r = if forward {
                (start + k) % total
            } else {
                (start + total * 2 - k) % total
            };
            if let Some(l) = self.diff.rows[r].line(pane) {
                if re.is_match(&self.panes[pane].doc.lines[l]) {
                    self.cursor_row = Some(r);
                    self.scroll_to = Some(r);
                    self.cur_block = self.diff.rows[r].block();
                    self.status.clear();
                    return;
                }
            }
        }
        self.status = trf("msg.not_found", &[("text", self.find.query.clone())]);
    }

    /// Replaces the first match on the current line of the active pane, then finds the next one.
    pub fn replace_next(&mut self) {
        let Some(re) = self.valid_matcher() else {
            return;
        };
        let pane = self.active_pane;
        let line = self
            .cursor_row
            .and_then(|r| self.diff.rows.get(r))
            .and_then(|r| r.line(pane));
        if let Some(l) = line.filter(|&l| re.is_match(&self.panes[pane].doc.lines[l])) {
            if !self.writable(pane) {
                return;
            }
            let row = self.cursor_row;
            let text = &self.panes[pane].doc.lines[l];
            let new = if self.find.regex {
                re.replacen(text, 1, self.find.replace.as_str())
            } else {
                re.replacen(text, 1, regex::NoExpand(&self.find.replace))
            }
            .into_owned();
            self.push_undo();
            self.panes[pane].doc.lines[l] = new;
            self.mark_changed(&[pane]);
            self.cursor_row = row.map(|_| self.diff.row_of_line(pane, l));
        }
        self.find_next(true);
    }

    /// Replaces every match in the active pane, as one undoable edit.
    pub fn replace_everywhere(&mut self) {
        let Some(re) = self.valid_matcher() else {
            return;
        };
        let pane = self.active_pane;
        if !self.writable(pane) {
            return;
        }
        let mut lines = self.panes[pane].doc.lines.clone();
        let n = replace_all(&mut lines, &re, &self.find.replace, self.find.regex);
        if n == 0 {
            self.status = trf("msg.not_found", &[("text", self.find.query.clone())]);
            return;
        }
        self.push_undo();
        self.panes[pane].doc.lines = lines;
        self.mark_changed(&[pane]);
        self.bulk_edit = Some(trf("msg.replaced", &[("n", n.to_string())]));
    }

    pub fn goto_line(&mut self, line: usize) {
        let r = self
            .diff
            .row_of_line(self.active_pane, line.saturating_sub(1));
        self.cursor_row = Some(r);
        self.scroll_to = Some(r);
    }

    pub fn outcome(&self) -> Outcome {
        if self.diff_position().1 == 0 {
            Outcome::Identical
        } else {
            Outcome::Different
        }
    }

    /// Lines removed from the left and added on the right of a 2-way compare.
    pub fn change_counts(&self) -> Option<(usize, usize)> {
        (self.n() == 2).then(|| {
            self.significant().fold((0, 0), |(del, add), b| {
                let r = &self.diff.blocks[b].ranges;
                (del + r[0].len(), add + r[1].len())
            })
        })
    }

    pub fn status_line(&self) -> String {
        self.status_text(true)
    }

    /// The status line for the status bar: the toolbar shows where among the differences the
    /// cursor is and how many lines changed, and the merge bar how many conflicts are left.
    /// The words for whether a document can be edited and saved: locked in the app, a file the
    /// system will not let us write, or writable.
    fn access_key(doc: &TextDoc) -> &'static str {
        if doc.compare_only {
            "label.compare_only"
        } else if doc.read_only {
            "label.read_only"
        } else if doc
            .path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .is_some_and(|m| m.permissions().readonly())
        {
            "label.read_only_disk"
        } else {
            "label.writable"
        }
    }

    pub fn status_detail(&self) -> String {
        self.status_text(false)
    }

    fn status_text(&self, counts: bool) -> String {
        let (pos, total) = self.diff_position();
        let mut s = if total == 0 {
            tr("msg.identical").to_string()
        } else if !counts {
            tr("view.different").to_string()
        } else {
            match pos {
                Some(p) => trf(
                    "msg.diff_of",
                    &[("n", (p + 1).to_string()), ("total", total.to_string())],
                ),
                None => trf("msg.diff_count", &[("total", total.to_string())]),
            }
        };
        if let Some((del, add)) = self.change_counts().filter(|&(d, a)| counts && d + a > 0) {
            s.push_str(&format!("  (+{add} −{del})"));
        }
        // The merge bar shows the conflicts left, but not in table mode.
        if (counts || self.table_mode) && self.n() == 3 {
            s.push_str(&format!(
                "  |  {}: {}",
                tr("label.conflicts"),
                self.conflicts_left()
            ));
        }
        if let Some(r) = self.cursor_row {
            if let Some(l) = self
                .diff
                .rows
                .get(r)
                .and_then(|row| row.line(self.active_pane))
            {
                s.push_str(&format!("  |  {} {}", tr("label.line"), l + 1));
            }
        }
        if self.tree_active || self.formatted_active {
            // The Tree and Formatted views show the files, but edits go to the text panes.
            s.push_str(&format!("  |  {}", tr("label.compare_only")));
        } else if let Some(pane) = self.panes.get(self.active_pane) {
            s.push_str(&format!(
                "  |  {}: {}",
                side_name(self.active_pane, self.n()),
                tr(Self::access_key(&pane.doc))
            ));
            if pane.doc.modified {
                s.push_str(&format!(", {}", tr("label.modified")));
            }
        }
        if let Some((r0, r1)) = self.selection() {
            let n = r1 - r0 + 1;
            s.push_str(&format!(
                "  |  {}",
                trf("text.rows_selected", &[("n", n.to_string())])
            ));
        }
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    // ---------- rendering ----------

    fn poll_highlight(&mut self, cfg: &Config) {
        for p in &mut self.panes {
            if let Some(rx) = &p.hl_rx {
                match rx.try_recv() {
                    Ok(res) => {
                        p.hl = res;
                        p.hl_rx = None;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        p.hl = None;
                        p.hl_rx = None;
                    }
                }
            }
            if !cfg.syntax_highlight {
                p.hl = None;
                continue;
            }
            if (p.hl_dirty || p.hl_dark != cfg.dark_mode) && p.hl_rx.is_none() {
                p.hl_dirty = false;
                p.hl_dark = cfg.dark_mode;
                let ext = p
                    .doc
                    .path
                    .as_deref()
                    .map(text::extension)
                    .or_else(|| p.doc.syntax.clone())
                    .unwrap_or_default();
                // Check the highlighter's limit before cloning a potentially gigabyte document.
                if syntax::within_limits(&p.doc.lines) {
                    p.hl_rx = Some(syntax::spawn(p.doc.lines.clone(), ext, cfg.dark_mode));
                } else {
                    p.hl = None;
                }
            }
        }
    }

    fn ensure_inline(&mut self, r: usize) {
        if self.inline_cache.contains_key(&r) || self.opts.inline == InlineMode::Off {
            return;
        }
        let row = self.diff.rows[r];
        let mut out: [Vec<Range<usize>>; 3] = Default::default();
        if let Some(b) = row.block() {
            if !self.diff.blocks[b].ignored {
                let line = |p: usize| row.line(p).map(|l| self.panes[p].doc.lines[l].as_str());
                if self.n() == 2 {
                    if let (Some(a), Some(bb)) = (line(0), line(1)) {
                        let (x, y) = diff::inline_diff(a, bb, &self.opts);
                        out[0] = x;
                        out[1] = y;
                    }
                } else {
                    if let (Some(a), Some(m)) = (line(0), line(1)) {
                        let (x, y) = diff::inline_diff(a, m, &self.opts);
                        out[0] = x;
                        out[1] = y;
                    }
                    if let (Some(c), Some(m)) = (line(2), line(1)) {
                        let (x, y) = diff::inline_diff(c, m, &self.opts);
                        out[2] = x;
                        out[1].extend(y);
                        out[1].sort_by_key(|r| r.start);
                    }
                }
            }
        }
        self.inline_cache.insert(r, out);
    }

    fn row_bg(&self, r: usize, p: usize, c: &Colors) -> Option<Color32> {
        let row = &self.diff.rows[r];
        let b = row.block()?;
        let blk = &self.diff.blocks[b];
        let sel = self.cur_block == Some(b);
        if blk.ignored {
            return Some(c.ignored);
        }
        let Some(l) = row.line(p) else {
            return Some(if sel { c.missing_sel } else { c.missing });
        };
        if !sel && self.diff.moved[p].contains(&l) {
            return Some(c.moved);
        }
        if self.n() == 3 {
            // the side that equals the base is drawn lighter
            let unchanged = matches!(
                (blk.kind, p),
                (BlockKind::LeftOnly, 2) | (BlockKind::RightOnly, 0)
            );
            if unchanged && !sel {
                return None;
            }
        }
        if self.n() == 2 && blk.ranges[..2].iter().any(Range::is_empty) {
            return Some(if sel { c.added_sel } else { c.added });
        }
        let conflict = blk.kind == BlockKind::Conflict && self.is_open_conflict(b);
        Some(match (conflict, sel) {
            (true, false) => c.conflict,
            (true, true) => c.conflict_sel,
            (_, false) => c.diff,
            (_, true) => c.diff_sel,
        })
    }

    fn row_bar(&self, r: usize, p: usize, c: &Colors) -> Option<Color32> {
        let row = &self.diff.rows[r];
        let b = row.block()?;
        let blk = &self.diff.blocks[b];
        if blk.ignored || row.line(p).is_none() {
            return None;
        }
        Some(if self.is_open_conflict(b) {
            c.conflict_bar
        } else if self.n() == 2 && blk.ranges[..2].iter().any(Range::is_empty) {
            c.ui.success
        } else {
            c.change_bar
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn line_job(
        text: &str,
        spans: Option<&LineSpans>,
        inline: &[Range<usize>],
        hits: &[Range<usize>],
        inline_bg: Color32,
        hit_bg: Color32,
        color: Color32,
        font: &FontId,
        tab: usize,
        show_ws: bool,
    ) -> LayoutJob {
        let text = clip_line(text);
        let mut cuts: Vec<usize> = vec![0, text.len()];
        if let Some(sp) = spans {
            for (r, _) in sp {
                cuts.push(r.start.min(text.len()));
                cuts.push(r.end.min(text.len()));
            }
        }
        for r in inline.iter().chain(hits) {
            cuts.push(r.start.min(text.len()));
            cuts.push(r.end.min(text.len()));
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut job = LayoutJob::default();
        let mut col = 0usize;
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                continue;
            }
            let fg = spans
                .and_then(|sp| {
                    sp.iter()
                        .find(|(r, _)| r.start <= a && a < r.end)
                        .map(|(_, c)| *c)
                })
                .unwrap_or(color);
            let bg = if hits.iter().any(|r| r.start <= a && a < r.end) {
                hit_bg
            } else if inline.iter().any(|r| r.start <= a && a < r.end) {
                inline_bg
            } else {
                Color32::TRANSPARENT
            };
            let mut seg = String::with_capacity(b - a);
            for ch in text[a..b].chars() {
                match ch {
                    '\t' => {
                        let n = tab.max(1) - col % tab.max(1);
                        if show_ws {
                            seg.push('→');
                            seg.extend(std::iter::repeat_n(' ', n - 1));
                        } else {
                            seg.extend(std::iter::repeat_n(' ', n));
                        }
                        col += n;
                    }
                    ' ' if show_ws => {
                        seg.push('·');
                        col += 1;
                    }
                    c => {
                        seg.push(c);
                        col += 1;
                    }
                }
            }
            job.append(
                &seg,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: fg,
                    background: bg,
                    ..Default::default()
                },
            );
        }
        job
    }

    fn find_hits(&self, line: &str) -> Vec<Range<usize>> {
        if !self.find.open {
            return Vec::new();
        }
        match self.find_re.as_deref() {
            Some((key, Ok(re))) if *key == self.find.key() => re
                .find_iter(line)
                .filter(|m| !m.is_empty())
                .map(|m| m.range())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            let wide = ui.available_width() > 1250.0;
            let caption = |key: &'static str| wide.then(|| tr(key));
            widgets::tool_group(ui, caption("toolbar.differences"), |ui| {
                if widgets::icon_button(ui, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
                    self.first_diff();
                }
                if widgets::icon_button_hint(
                    ui,
                    Icon::Prev,
                    &Cmd::PrevDiff.tip(),
                    tr("tip.nav_diff"),
                )
                .clicked()
                    && !self.prev_diff(false)
                {
                    self.step_file = Some(false);
                }
                let (pos, total) = self.diff_position();
                let counter = match pos {
                    Some(p) => format!("{} / {total}", p + 1),
                    None => format!("– / {total}"),
                };
                widgets::chip(ui, &counter, pal.text, pal.control).on_hover_ui(|ui| {
                    ui.label(self.status_line());
                });
                if let Some((del, add)) = self.change_counts().filter(|&(d, a)| d + a > 0) {
                    let (added, removed) = (format!("+{add}"), format!("−{del}"));
                    let diff = theme::colors(ui.visuals().dark_mode).ui;
                    ui.label(RichText::new(&added).monospace().color(diff.success));
                    ui.label(RichText::new(&removed).monospace().color(diff.danger))
                        .on_hover_text(trf(
                            "text.change_counts",
                            &[("add", add.to_string()), ("del", del.to_string())],
                        ));
                }
                if widgets::icon_button_hint(
                    ui,
                    Icon::Next,
                    &Cmd::NextDiff.tip(),
                    tr("tip.nav_diff"),
                )
                .clicked()
                    && !self.next_diff(false)
                {
                    self.step_file = Some(true);
                }
                if widgets::icon_button(ui, Icon::Last, &Cmd::LastDiff.tip()).clicked() {
                    self.last_diff();
                }
            });
            let merge = if self.n() == 2 {
                "toolbar.copy"
            } else {
                "menu.merge"
            };
            widgets::tool_group(ui, caption(merge), |ui| {
                if self.n() == 2 {
                    if widgets::icon_button(
                        ui,
                        Icon::CopyRight,
                        &Cmd::CopyRight.tip_if(self.keys_enabled),
                    )
                    .clicked()
                    {
                        self.copy_current(true);
                    }
                    if widgets::icon_button(
                        ui,
                        Icon::CopyLeft,
                        &Cmd::CopyLeft.tip_if(self.keys_enabled),
                    )
                    .clicked()
                    {
                        self.copy_current(false);
                    }
                    if widgets::icon_button(
                        ui,
                        Icon::CopyAllRight,
                        &Cmd::CopyAllRight.tip_if(self.keys_enabled),
                    )
                    .clicked()
                    {
                        self.copy_all(0, 1);
                    }
                    if widgets::icon_button(
                        ui,
                        Icon::CopyAllLeft,
                        &Cmd::CopyAllLeft.tip_if(self.keys_enabled),
                    )
                    .clicked()
                    {
                        self.copy_all(1, 0);
                    }
                } else {
                    if widgets::icon_button_hint(
                        ui,
                        Icon::PrevConflict,
                        &Cmd::PrevConflict.tip_if(self.keys_enabled),
                        tr("tip.nav_conflict"),
                    )
                    .clicked()
                        && !self.prev_diff(true)
                    {
                        self.step_conflict = Some(false);
                    }
                    if widgets::icon_button_hint(
                        ui,
                        Icon::NextConflict,
                        &Cmd::NextConflict.tip_if(self.keys_enabled),
                        tr("tip.nav_conflict"),
                    )
                    .clicked()
                        && !self.next_diff(true)
                    {
                        self.step_conflict = Some(true);
                    }
                    widgets::toolbar_sep(ui);
                    if widgets::icon_button(ui, Icon::CopyRight, tr("cmd.copy_from_left")).clicked()
                    {
                        if let Some(b) = self.cur_block {
                            self.copy_block(b, 0, 1);
                        }
                    }
                    if widgets::icon_button(ui, Icon::CopyLeft, tr("cmd.copy_from_right")).clicked()
                    {
                        if let Some(b) = self.cur_block {
                            self.copy_block(b, 2, 1);
                        }
                    }
                    if widgets::icon_button(ui, Icon::CopyAllRight, tr("cmd.copy_all_from_left"))
                        .clicked()
                    {
                        self.copy_all(0, 1);
                    }
                    if widgets::icon_button(ui, Icon::CopyAllLeft, tr("cmd.copy_all_from_right"))
                        .clicked()
                    {
                        self.copy_all(2, 1);
                    }
                    if widgets::ghost_icon_button(ui, Icon::Bolt, tr("cmd.auto_merge")).clicked() {
                        self.auto_merge();
                    }
                }
            });
            widgets::tool_group(ui, caption("menu.edit"), |ui| {
                if widgets::icon_button_enabled(
                    ui,
                    self.can_undo(),
                    Icon::Undo,
                    &Cmd::Undo.tip_if(self.keys_enabled),
                )
                .clicked()
                {
                    self.undo();
                }
                if widgets::icon_button_enabled(
                    ui,
                    self.can_redo(),
                    Icon::Redo,
                    &Cmd::Redo.tip_if(self.keys_enabled),
                )
                .clicked()
                {
                    self.redo();
                }
            });
            widgets::tool_group(ui, caption("menu.view"), |ui| {
                let mut find_open = self.find.open;
                if widgets::toggle_icon_button(
                    ui,
                    &mut find_open,
                    Some(Icon::Search),
                    "",
                    &Cmd::Find.tip_if(self.keys_enabled),
                )
                .clicked()
                {
                    if find_open {
                        self.open_find();
                    } else {
                        self.find.open = false;
                    }
                }
                let mut collapse = self.collapse;
                if widgets::toggle_icon_button(
                    ui,
                    &mut collapse,
                    Some(Icon::Fold),
                    "",
                    &Cmd::ToggleCollapse.tip_if(self.keys_enabled),
                )
                .clicked()
                {
                    self.set_collapse(collapse);
                }
                if self.n() == 2 && !self.table_mode {
                    widgets::toggle_icon_button(
                        ui,
                        &mut self.unified,
                        Some(Icon::Inline),
                        "",
                        tr("cmd.inline_view"),
                    );
                }
                ui.add_enabled_ui(!self.table_mode && !self.unified, |ui| {
                    widgets::toggle_icon_button(
                        ui,
                        &mut self.wrap,
                        Some(Icon::Wrap),
                        "",
                        &Cmd::ToggleWrap.tip_if(self.keys_enabled),
                    );
                });
                if self.table_delim.is_some() {
                    widgets::toggle_icon_button(
                        ui,
                        &mut self.table_mode,
                        Some(Icon::Table),
                        tr("cmd.table_mode"),
                        tr("cmd.table_mode"),
                    );
                }
            });
            ui.add_space(4.0);
            let tip = Cmd::HtmlReport.tip_if(self.keys_enabled);
            if widgets::ghost_icon_button(ui, Icon::Export, tr("btn.export_html"))
                .on_hover_text(tip)
                .clicked()
            {
                self.export_request = true;
            }
            if !self.panel_tabs().is_empty() {
                widgets::toolbar_end(ui, 34.0, |ui| {
                    widgets::drawer_toggle(ui, &mut self.drawer_open);
                });
            }
        });
    }

    /// 3-way: how many conflicts are left, what to do with the current difference, and saving
    /// the result.
    fn merge_bar(&mut self, ui: &mut Ui) {
        let left = self.conflicts_left();
        let cur = self.cur_block.filter(|&b| b < self.diff.blocks.len());
        let open = cur.filter(|&b| self.is_open_conflict(b));
        let conflict = cur.is_some_and(|b| self.diff.blocks[b].kind == BlockKind::Conflict);
        let editable = !self.panes[1].doc.read_only;
        let modified = self.panes[1].doc.modified || self.edit.is_some();
        let keys = self.keys_enabled;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            let (icon, color, text) = if left > 0 {
                let n = left.to_string();
                (
                    Icon::Warning,
                    pal.warning,
                    trf(
                        if left == 1 {
                            "merge.conflicts_left_one"
                        } else {
                            "merge.conflicts_left"
                        },
                        &[("n", n)],
                    ),
                )
            } else {
                (
                    Icon::Check,
                    pal.success,
                    tr("merge.no_conflicts").to_string(),
                )
            };
            ui.add_space(4.0);
            widgets::inline_icon(ui, icon, color);
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(color).strong());
            let next = tr("merge.next_conflict");
            if left > 0
                && widgets::bar_button(ui, true, Some(Icon::NextConflict), next, false)
                    .on_hover_text(Cmd::NextConflict.tip_if(keys))
                    .clicked()
                && !self.next_diff(true)
            {
                self.cursor_row = None;
                self.next_diff(true);
            }
            if let Some(b) = cur.filter(|_| editable) {
                widgets::toolbar_sep(ui);
                let take = |ui: &mut Ui, icon, key| {
                    widgets::bar_button(ui, true, icon, tr(key), false).clicked()
                };
                let from_left = take(ui, Some(Icon::CopyRight), "merge.take_left");
                let both = conflict && take(ui, None, "merge.take_both");
                let from_right = take(ui, Some(Icon::CopyLeft), "merge.take_right");
                let resolve = open.is_some()
                    && widgets::bar_button(
                        ui,
                        true,
                        Some(Icon::Check),
                        tr("merge.mark_resolved"),
                        false,
                    )
                    .on_hover_text(tr("merge.mark_resolved_tip"))
                    .clicked();
                if from_left {
                    self.copy_block(b, 0, 1);
                } else if both {
                    self.take_both_at(b, true);
                } else if from_right {
                    self.copy_block(b, 2, 1);
                } else if resolve {
                    self.mark_resolved(b);
                }
            }
            let label = tr("merge.save_result");
            let width = ui
                .painter()
                .layout_no_wrap(
                    label.to_owned(),
                    FontId::proportional(theme::size::DENSE),
                    Color32::PLACEHOLDER,
                )
                .size()
                .x
                + 32.0;
            widgets::toolbar_end(ui, width, |ui| {
                ui.add_space(4.0);
                let r = widgets::bar_button(ui, modified, None, label, left == 0)
                    .on_hover_text(Cmd::Save.tip_if(keys));
                if r.clicked() {
                    self.commit_pending_edit();
                    if let Err(e) = self.save_pane(1) {
                        self.status = e.to_string();
                    }
                }
            });
        });
    }

    /// The lines of the inline view: unchanged rows once, and each difference as its removed
    /// lines followed by its added lines. Folds like the side-by-side layout.
    fn build_unified(&self) -> Vec<ULine> {
        let rows = self.diff.rows.len();
        let unchanged = |r: usize| {
            self.diff.rows[r]
                .block()
                .is_none_or(|b| self.diff.blocks[b].ignored)
        };
        let mut out = Vec::with_capacity(rows);
        let mut r = 0;
        while r < rows {
            let mut e = r + 1;
            if unchanged(r) {
                while e < rows && unchanged(e) {
                    e += 1;
                }
                let lo = if r == 0 { 0 } else { r + FOLD_CONTEXT };
                let hi = if e == rows {
                    e
                } else {
                    e.saturating_sub(FOLD_CONTEXT)
                };
                let fold = self.collapse && hi >= lo + MIN_FOLD && !self.expanded.contains(&lo);
                for k in r..e {
                    if fold && k == lo {
                        out.push(ULine::Fold(lo, hi));
                    }
                    if fold && (lo..hi).contains(&k) {
                        continue;
                    }
                    let row = self.diff.rows[k];
                    let pane = if row.line(1).is_some() { 1 } else { 0 };
                    out.push(ULine::Line {
                        row: k,
                        pane,
                        change: false,
                    });
                }
            } else {
                let block = self.diff.rows[r].block();
                while e < rows && self.diff.rows[e].block() == block {
                    e += 1;
                }
                for pane in 0..2 {
                    out.extend(
                        (r..e)
                            .filter(|&k| self.diff.rows[k].line(pane).is_some())
                            .map(|row| ULine::Line {
                                row,
                                pane,
                                change: true,
                            }),
                    );
                }
            }
            r = e;
        }
        out
    }

    /// 2-way: both files in one column, the removed lines of each difference above the added
    /// ones.
    fn unified_ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        // The inline view does not edit or select: finish what the side-by-side view started.
        self.commit_pending_edit();
        self.sel_anchor = None;
        let key = (self.layout_gen, self.collapse, self.diff.rows.len());
        let lines = match self.unified_lines.take() {
            Some((k, lines)) if k == key => lines,
            _ => self.build_unified(),
        };
        self.tab_size = cfg.tab_size;
        self.ensure_layout(0, cfg.tab_size);
        let font = FontId::monospace(cfg.font_size);
        let row_h = ui.fonts_mut(|f| f.row_height(&font)) + theme::row_padding();
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
        self.page_rows = ((ui.available_height() / row_h) as usize)
            .saturating_sub(1)
            .max(1);
        // `show_rows` adds the spacing between items to each row.
        let step = row_h + ui.spacing().item_spacing.y;
        // Enter or F2 edits the cursor line side by side, as they edit a line there.
        let free = self.keys_enabled
            && !widgets::typing(ui.ctx())
            && self.goto.is_none()
            && !widgets::modal_open(ui.ctx());
        if free
            && self.cursor_row.is_some()
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter) | i.consume_key(Modifiers::NONE, Key::F2)
            })
        {
            self.edit_side_by_side();
            ui.ctx().request_repaint();
            return;
        }
        // Read-only: say so, with the way to edit.
        let mut to_edit = false;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            widgets::inline_icon(ui, Icon::Lock, pal.text_dim);
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tr("msg.inline_read_only")).color(pal.text_dim));
            if ui
                .button(tr("cmd.edit_side_by_side"))
                .on_hover_text(tr("tip.edit_side_by_side"))
                .clicked()
            {
                to_edit = true;
            }
        });
        if to_edit {
            self.edit_side_by_side();
            ui.ctx().request_repaint();
            return;
        }
        let moved_to = self.unified_keys(ui, &lines, step);
        let digits = self
            .panes
            .iter()
            .map(|p| p.doc.lines.len())
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(3);
        let num_w = if cfg.show_line_numbers {
            (digits as f32 + 1.0) * char_w
        } else {
            0.0
        };
        let gutter_w = 2.0 * num_w + 2.0 * char_w + 8.0;
        let mut area = egui::ScrollArea::both()
            .id_salt(self.id.with("unified"))
            .auto_shrink(false);
        if let Some(i) = moved_to {
            // Scroll only as far as needed to show the whole cursor line.
            let (offset, height) = self.unified_shown;
            let (top, bottom) = (i as f32 * step, (i + 1) as f32 * step);
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if bottom > offset + height {
                area = area.vertical_scroll_offset((bottom - height).max(0.0));
            }
        } else if let Some(row) = self.scroll_to.take() {
            let folded = lines.iter().find_map(|l| match *l {
                ULine::Fold(lo, hi) if (lo..hi).contains(&row) => Some(lo),
                _ => None,
            });
            if let Some(lo) = folded {
                // Unfold first; the next frame scrolls to the row.
                self.expanded.insert(lo);
                self.layout_gen += 1;
                self.scroll_to = Some(row);
                ui.ctx().request_repaint();
            } else if let Some(i) = lines.iter().position(|l| match *l {
                ULine::Line { row: r, .. } => r >= row,
                ULine::Fold(_, hi) => hi > row,
            }) {
                let top = i as f32 * step - ui.available_height() / 3.0;
                area = area.vertical_scroll_offset(top.max(0.0));
            }
        }
        let mut clicked = None;
        let mut double = false;
        let mut open = None;
        let tab = cfg.tab_size;
        let cursor_at = self.unified_cursor(&lines);
        let shown = egui::Frame::new().fill(c.text_bg).show(ui, |ui| {
            area.show_rows(ui, row_h, lines.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                for (i, line) in lines.iter().enumerate().take(range.end).skip(range.start) {
                    let (row, pane, change) = match *line {
                        ULine::Fold(lo, hi) => {
                            let size = vec2(width, row_h);
                            let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                            let hovered = resp.hovered();
                            let fill = if hovered {
                                c.ui.control_hover
                            } else {
                                c.ui.surface_alt
                            };
                            ui.painter().rect_filled(rect, 0.0, fill);
                            let fg = if hovered { c.ui.accent } else { c.ui.text_dim };
                            let label = trf("text.folded", &[("n", (hi - lo).to_string())]);
                            let small = FontId::proportional((cfg.font_size - 1.0).max(10.0));
                            ui.painter().text(
                                pos2(rect.left() + gutter_w + 4.0, rect.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                small,
                                fg,
                            );
                            if resp
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(tr("help.expand_fold"))
                                .clicked()
                            {
                                open = Some(lo);
                            }
                            continue;
                        }
                        ULine::Line { row, pane, change } => (row, pane, change),
                    };
                    let Some(&r) = self.diff.rows.get(row) else {
                        continue;
                    };
                    self.ensure_inline(row);
                    let Some(l) = r.line(pane) else { continue };
                    let text = &self.panes[pane].doc.lines[l];
                    let block = r.block().filter(|&b| !self.diff.blocks[b].ignored);
                    let ignored = r.block().is_some() && block.is_none();
                    let sel = block.is_some() && block == self.cur_block;
                    let (bg, inline_bg, sign) = match (change, pane) {
                        (false, _) => (None, c.inline, ""),
                        (true, 0) => (
                            Some(if sel { c.removed_sel } else { c.removed }),
                            c.inline_removed,
                            "−",
                        ),
                        _ => (
                            Some(if sel { c.added_sel } else { c.added }),
                            c.inline_added,
                            "+",
                        ),
                    };
                    let fg = if ignored { c.gutter_text } else { c.text };
                    let spans = if ignored {
                        None
                    } else {
                        self.panes[pane].hl.as_ref().and_then(|h| h.get(l))
                    };
                    let inline = match self.inline_cache.get(&row) {
                        Some(v) if change => v[pane].as_slice(),
                        _ => &[],
                    };
                    let hits = self.find_hits(text);
                    let job = Self::line_job(
                        text,
                        spans,
                        inline,
                        &hits,
                        inline_bg,
                        c.find_hit,
                        fg,
                        &font,
                        tab,
                        cfg.show_whitespace,
                    );
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    let w = (gutter_w + 12.0 + galley.size().x).max(width);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
                    let painter = ui.painter();
                    let gutter = Rect::from_min_size(rect.min, vec2(gutter_w, row_h));
                    painter.rect_filled(gutter, 0.0, c.gutter_bg);
                    if let Some(bg) = bg {
                        painter.rect_filled(rect.with_min_x(gutter.right()), 0.0, bg);
                        let bar = if pane == 0 { c.ui.danger } else { c.ui.success };
                        let edge = Rect::from_min_size(
                            pos2(gutter.right() - 3.0, rect.top()),
                            vec2(3.0, row_h),
                        );
                        painter.rect_filled(edge, 0.0, bar);
                    }
                    let y = rect.center().y;
                    for (side, x) in [(0, num_w), (1, 2.0 * num_w)] {
                        let shown = cfg.show_line_numbers && (!change || side == pane);
                        if let Some(n) = r.line(side).filter(|_| shown) {
                            painter.text(
                                pos2(rect.left() + x, y),
                                Align2::RIGHT_CENTER,
                                (n + 1).to_string(),
                                font.clone(),
                                c.gutter_text,
                            );
                        }
                    }
                    painter.text(
                        pos2(rect.left() + 2.0 * num_w + char_w + 4.0, y),
                        Align2::CENTER_CENTER,
                        sign,
                        font.clone(),
                        if pane == 0 { c.ui.danger } else { c.ui.success },
                    );
                    painter.galley(pos2(gutter.right() + 6.0, rect.top() + 1.0), galley, fg);
                    if cursor_at == Some(i) {
                        painter.rect_stroke(
                            rect.with_min_x(gutter.right()).shrink(0.5),
                            3.0,
                            Stroke::new(1.5_f32, c.cursor_row),
                            StrokeKind::Inside,
                        );
                    }
                    if resp.double_clicked() {
                        double = true;
                    }
                    if resp.clicked() || resp.double_clicked() {
                        clicked = Some((row, pane));
                    }
                }
            })
        });
        let area = shown.inner;
        self.unified_shown = (area.state.clamped_offset().y, area.inner_rect.height());
        if let Some((row, pane)) = clicked {
            self.sel_anchor = None;
            self.cursor_row = Some(row);
            self.active_pane = pane;
            self.cur_block = self.diff.rows[row]
                .block()
                .filter(|&b| !self.diff.blocks[b].ignored);
        }
        if let Some(lo) = open {
            self.expanded.insert(lo);
            self.layout_gen += 1;
        }
        self.unified_lines = Some((key, lines));
        if double && clicked.is_some() {
            self.edit_side_by_side();
            ui.ctx().request_repaint();
        }
    }

    /// The index in `lines` of the cursor line: the one on the active side, or on the other
    /// side when the cursor row has no line on the active one (going to an added line).
    fn unified_cursor(&self, lines: &[ULine]) -> Option<usize> {
        let at = self.cursor_row?;
        let find = |any_side: bool| {
            lines.iter().position(|l| {
                matches!(*l, ULine::Line { row, pane, change }
                    if row == at && (any_side || pane == self.active_pane || !change))
            })
        };
        find(false).or_else(|| find(true))
    }

    /// The keys of the inline view, which only shows the files: the arrow keys, Page Up/Down
    /// and Cmd+Home/End move the cursor along its lines, and Copy copies the cursor line.
    /// Returns the index in `lines` the cursor moved to; `line_h` is the height of a line.
    fn unified_keys(&mut self, ui: &Ui, lines: &[ULine], line_h: f32) -> Option<usize> {
        if !self.keys_enabled
            || widgets::typing(ui.ctx())
            || self.goto.is_some()
            || widgets::modal_open(ui.ctx())
            || lines.is_empty()
        {
            return None;
        }
        if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            if let Some(ULine::Line { row, pane, .. }) =
                self.unified_cursor(lines).map(|i| lines[i])
            {
                if let Some(l) = self.diff.rows.get(row).and_then(|r| r.line(pane)) {
                    ui.ctx().copy_text(self.panes[pane].doc.lines[l].clone());
                }
            }
        }
        let page = self.page_rows.max(1) as isize;
        let step = ui.input_mut(|i| {
            let mut step = None;
            for (mods, key, d) in [
                (Modifiers::NONE, Key::ArrowDown, 1),
                (Modifiers::NONE, Key::ArrowUp, -1),
                (Modifiers::NONE, Key::PageDown, page),
                (Modifiers::NONE, Key::PageUp, -page),
                (Modifiers::COMMAND, Key::Home, isize::MIN),
                (Modifiers::COMMAND, Key::End, isize::MAX),
            ] {
                if i.consume_key(mods, key) {
                    step = Some(d);
                }
            }
            step
        })?;
        let last = lines.len() - 1;
        let cur = self.unified_cursor(lines);
        let target = match step {
            isize::MIN => 0,
            isize::MAX => last,
            // Without a cursor, start on the first line shown.
            d => match cur {
                Some(i) => (i as isize + d).clamp(0, last as isize) as usize,
                None => ((self.unified_shown.0 / line_h).ceil() as usize).min(last),
            },
        };
        let is_line = |i: &usize| matches!(lines[*i], ULine::Line { .. });
        let found = if step > 0 {
            (target..=last)
                .find(is_line)
                .or_else(|| (0..target).rev().find(is_line))
        } else {
            (0..=target)
                .rev()
                .find(is_line)
                .or_else(|| (target..=last).find(is_line))
        }?;
        let ULine::Line { row, pane, .. } = lines[found] else {
            return None;
        };
        self.cursor_row = Some(row);
        self.active_pane = pane;
        self.cur_block = self.diff.rows[row]
            .block()
            .filter(|&b| !self.diff.blocks[b].ignored);
        Some(found)
    }

    fn disk_changed_bar(&mut self, ui: &mut Ui) {
        let names: Vec<String> = self
            .disk_changed
            .iter()
            .map(|&p| self.panes[p].doc.title())
            .collect();
        let mut reload = false;
        let mut ignore = false;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            widgets::inline_icon(ui, Icon::Warning, pal.warning);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(trf("msg.changed_on_disk", &[("file", names.join(", "))]))
                    .color(pal.warning),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ignore = widgets::ghost_button(ui, tr("btn.ignore")).clicked();
                reload = widgets::ghost_button(ui, tr("btn.reload")).clicked();
            });
        });
        if reload {
            self.reload_changed();
        } else if ignore {
            self.disk_changed.clear();
        }
    }

    fn find_bar(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let hint = tr("label.find").trim_end_matches([':', '：']);
            let dim = widgets::pal(ui).text_dim;
            ui.add_space(4.0);
            widgets::inline_icon(ui, Icon::Search, dim);
            ui.add_space(4.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.find.query)
                    .hint_text(hint)
                    .margin(vec2(8.0, 4.0))
                    .desired_width(280.0),
            );
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let back = ui.input(|i| i.modifiers.shift);
                self.find_next(!back);
                r.request_focus();
            }
            // Escape closes the find bar first, and only it: the selection and the caret stay
            // for the next Escape.
            if self.edit.is_none()
                && self.goto.is_none()
                && !widgets::modal_open(ui.ctx())
                && !egui::Popup::is_any_open(ui.ctx())
                && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            {
                self.find.open = false;
            }
            if let Some(Err(e)) = self.matcher() {
                let danger = widgets::pal(ui).danger;
                ui.painter().rect_stroke(
                    r.rect.expand2(vec2(8.0, 4.0)),
                    4.0,
                    Stroke::new(1.5_f32, danger),
                    egui::StrokeKind::Outside,
                );
                r.on_hover_text(e);
            }
            ui.add_space(4.0);
            widgets::toggle_button(ui, &mut self.find.match_case, "Aa", tr("label.match_case"));
            widgets::toggle_button(ui, &mut self.find.whole_word, "ab", tr("label.whole_word"));
            widgets::toggle_button(ui, &mut self.find.regex, ".*", tr("label.regex"));
            let key = widgets::with_key;
            if widgets::icon_button(ui, Icon::Prev, &Cmd::FindPrev.tip_if(self.keys_enabled))
                .clicked()
            {
                self.find_next(false);
            }
            if widgets::icon_button(ui, Icon::Next, &Cmd::FindNext.tip_if(self.keys_enabled))
                .clicked()
            {
                self.find_next(true);
            }
            widgets::toggle_button(
                ui,
                &mut self.find.show_replace,
                tr("cmd.replace"),
                &Cmd::Replace.tip_if(self.keys_enabled),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Close, &key(tr("btn.close"), "Esc")).clicked() {
                    self.find.open = false;
                }
            });
        });
        if !self.find.show_replace {
            return;
        }
        widgets::toolbar(ui, |ui| {
            ui.add_space(24.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.find.replace)
                    .hint_text(tr("label.replace_with"))
                    .margin(vec2(8.0, 4.0))
                    .desired_width(280.0),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.replace_next();
                r.request_focus();
            }
            ui.add_space(4.0);
            if widgets::secondary_button(ui, tr("cmd.replace")).clicked() {
                self.replace_next();
            }
            if widgets::secondary_button(ui, tr("cmd.replace_all")).clicked() {
                self.replace_everywhere();
            }
        });
    }

    fn pane_header(&mut self, ui: &mut Ui, p: usize, rect: Rect) {
        let n = self.n();
        let active = self.active_pane == p;
        let pal = widgets::pal(ui);
        ui.painter().rect_filled(rect, 0.0, pal.surface_alt);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.border),
        );
        if active {
            ui.painter().hline(
                rect.left() + 1.0..=rect.right() - 2.0,
                rect.bottom() - 1.0,
                Stroke::new(2.0_f32, widgets::side_color(ui, p, n)),
            );
        }
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 3.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    let doc = &self.panes[p].doc;
                    let title = doc.title();
                    let (name, dir) = match (&doc.description, &doc.path) {
                        (Some(d), _) => (d.clone(), String::new()),
                        (None, Some(path)) => (
                            path.file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            path.parent()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default(),
                        ),
                        _ => (tr("untitled").to_string(), String::new()),
                    };
                    let modified = doc.modified;
                    let mut ro = doc.read_only;
                    let compare_only = doc.compare_only;
                    widgets::side_chip(ui, p, n);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let lock = if ro { Icon::Lock } else { Icon::Unlock };
                        // Text read from a file cannot be unlocked: saving it would replace the
                        // file with its text.
                        let unlocked = ui
                            .add_enabled_ui(!compare_only, |ui| {
                                widgets::toggle_icon_button(
                                    ui,
                                    &mut ro,
                                    Some(lock),
                                    "",
                                    tr("label.read_only"),
                                )
                                .on_disabled_hover_text(tr("tip.compare_only"))
                                .clicked()
                            })
                            .inner;
                        if unlocked {
                            self.panes[p].doc.read_only = ro;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let mut job = LayoutJob::default();
                            let name_font = if active {
                                theme::semibold(theme::size::CONTROL)
                            } else {
                                FontId::proportional(theme::size::CONTROL)
                            };
                            job.append(
                                &name,
                                0.0,
                                TextFormat {
                                    font_id: name_font,
                                    color: pal.text,
                                    ..Default::default()
                                },
                            );
                            if modified {
                                job.append(
                                    "  ●",
                                    0.0,
                                    TextFormat {
                                        font_id: FontId::proportional(theme::size::CAPTION),
                                        color: pal.warning,
                                        ..Default::default()
                                    },
                                );
                            }
                            if !dir.is_empty() {
                                job.append(
                                    &format!("   {dir}"),
                                    0.0,
                                    TextFormat {
                                        font_id: FontId::proportional(theme::size::SMALL),
                                        color: pal.text_dim,
                                        ..Default::default()
                                    },
                                );
                            }
                            ui.add(egui::Label::new(job).truncate().selectable(false))
                                .on_hover_text(title);
                        });
                    });
                });
            },
        );
    }

    fn pane_footer(&mut self, ui: &mut Ui, p: usize, rect: Rect) {
        let pal = widgets::pal(ui);
        ui.painter().rect_filled(rect, 0.0, pal.surface_alt);
        ui.painter()
            .hline(rect.x_range(), rect.top(), Stroke::new(1.0_f32, pal.border));
        let mut enc_choice = None;
        let mut eol_choice = None;
        let doc = &self.panes[p].doc;
        let enc_name = doc.encoding.name();
        let bom = if doc.bom { " BOM" } else { "" };
        let eol = doc.eol;
        let mixed = doc.mixed_eol;
        let mut footer_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((self.id, "footer", p))
                .max_rect(rect.shrink2(vec2(8.0, 1.0))),
        );
        let ui = &mut footer_ui;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let eol_label = if mixed {
                format!("{}*", eol.label())
            } else {
                eol.label().to_string()
            };
            egui::ComboBox::from_id_salt((self.id, "eol", p))
                .selected_text(eol_label)
                .width(58.0)
                .show_ui(ui, |ui| {
                    for e in [Eol::CrLf, Eol::Lf, Eol::Cr] {
                        if ui.selectable_label(e == eol, e.label()).clicked() {
                            eol_choice = Some(e);
                        }
                    }
                })
                .response
                .on_hover_text(tr("tip.eol"));
            egui::ComboBox::from_id_salt((self.id, "enc", p))
                .selected_text(format!("{enc_name}{bom}"))
                .width(96.0)
                .show_ui(ui, |ui| {
                    for e in COMMON_ENCODINGS {
                        if ui
                            .selectable_label(enc_name.eq_ignore_ascii_case(e), *e)
                            .clicked()
                        {
                            enc_choice = TextEncoding::from_label(e);
                        }
                    }
                })
                .response
                .on_hover_text(tr("tip.encoding"));
            let count = self.panes[p].doc.lines.len();
            let line = self
                .cursor_row
                .and_then(|r| self.diff.rows.get(r))
                .and_then(|r| r.line(p))
                .map_or(0, |l| l + 1);
            let access_key = Self::access_key(&self.panes[p].doc);
            let read_only = access_key != "label.writable";
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{line} / {count}"))
                            .small()
                            .color(pal.text_dim),
                    )
                    .truncate(),
                );
                if self.panes[p].doc.modified {
                    ui.add(egui::Label::new(
                        RichText::new(tr("label.modified"))
                            .small()
                            .color(pal.accent),
                    ))
                    .on_hover_text(tr("tip.modified"));
                }
                // Whether typing goes into this pane.
                let access = RichText::new(tr(access_key)).small();
                ui.add(egui::Label::new(if read_only {
                    access.color(pal.warning)
                } else {
                    access.color(pal.text_dim)
                }))
                .on_hover_text(match access_key {
                    "label.read_only" => tr("msg.readonly"),
                    "label.compare_only" => tr("tip.compare_only"),
                    "label.read_only_disk" => tr("tip.read_only_disk"),
                    _ => tr("tip.writable"),
                });
            });
        });
        if let Some(e) = enc_choice {
            self.reload_encoding(p, e);
        }
        if let Some(e) = eol_choice {
            self.set_eol(p, e);
        }
    }

    /// Rebuilds the display lines when folding, wrapping or the diff changed.
    fn ensure_layout(&mut self, wrap_cols: usize, tab: usize) {
        let key = (
            wrap_cols,
            tab,
            self.collapse,
            self.layout_gen,
            self.text_layout,
        );
        let rows = self.diff.rows.len();
        if self.layout.key == Some(key) && self.layout.row_line.len() == rows {
            return;
        }
        // after the comparison was computed again, the line that was at the top stays there
        let held_top = self.held.take().and_then(|h| h.top);
        let top = self
            .layout
            .lines
            .get(self.scroll as usize)
            .map(|l| match *l {
                VLine::Row(r, _) => r,
                VLine::Fold(s, _) => s,
            });
        let unchanged = |r: usize| {
            self.diff.rows[r]
                .block()
                .is_none_or(|b| self.diff.blocks[b].ignored)
        };
        let mut lines = Vec::with_capacity(rows);
        let mut row_line = vec![0; rows];
        let mut r = 0;
        while r < rows {
            let (lo, hi, end) = if self.collapse && unchanged(r) {
                let mut e = r;
                while e < rows && unchanged(e) {
                    e += 1;
                }
                let lo = if r == 0 { 0 } else { r + FOLD_CONTEXT };
                let hi = if e == rows {
                    e
                } else {
                    e.saturating_sub(FOLD_CONTEXT)
                };
                if hi >= lo + MIN_FOLD && !self.expanded.contains(&lo) {
                    (lo, hi, e)
                } else {
                    (e, e, e)
                }
            } else {
                (r + 1, r + 1, r + 1)
            };
            for k in r..end {
                if k == lo && hi > lo {
                    row_line[lo..hi].fill(lines.len());
                    lines.push(VLine::Fold(lo, hi));
                }
                if (lo..hi).contains(&k) {
                    continue;
                }
                row_line[k] = lines.len();
                let segs = if wrap_cols > 0 {
                    self.row_segments(k, wrap_cols, tab)
                } else {
                    1
                };
                lines.extend((0..segs).map(|s| VLine::Row(k, s)));
            }
            r = end;
        }
        let panes = (0..self.n())
            .map(|p| {
                let mut pane = PaneLayout::default();
                let mut segments = 0;
                for (v, &line) in lines.iter().enumerate() {
                    pane.boundaries.push(pane.shared.len());
                    let include = self.text_layout == TextLayout::Aligned
                        || match line {
                            VLine::Fold(..) => true,
                            VLine::Row(r, seg) => {
                                if seg == 0 {
                                    segments = self.diff.rows[r].line(p).map_or(0, |l| {
                                        if wrap_cols == 0 {
                                            1
                                        } else {
                                            wrap_starts(&self.panes[p].doc.lines[l], tab, wrap_cols)
                                                .len()
                                                as u32
                                        }
                                    });
                                }
                                seg < segments
                            }
                        };
                    if include {
                        pane.shared.push(v);
                    }
                }
                pane.boundaries.push(pane.shared.len());
                pane
            })
            .collect();
        self.layout = Layout {
            key: Some(key),
            lines,
            row_line,
            panes,
        };
        if let Some((p, l, into)) = held_top.filter(|_| rows > 0) {
            let row = self.diff.row_of_line(p, l);
            if let Some(&v) = self.layout.row_line.get(row) {
                // Within the row as it is laid out now, which an edit may have made shorter.
                let span = self
                    .layout
                    .row_line
                    .get(row + 1)
                    .map_or(self.layout.lines.len(), |&n| n)
                    .saturating_sub(v)
                    .max(1);
                self.scroll = v as f32 + into.min(span as f32 - 0.001);
            }
        } else if let Some(&v) = top.and_then(|t| self.layout.row_line.get(t)) {
            self.scroll = v as f32 + self.scroll.fract();
        }
    }

    fn row_segments(&self, r: usize, cols: usize, tab: usize) -> u32 {
        let row = &self.diff.rows[r];
        (0..self.n())
            .filter_map(|p| row.line(p).map(|l| &self.panes[p].doc.lines[l]))
            .map(|line| wrap_starts(line, tab, cols).len() as u32)
            .max()
            .unwrap_or(1)
    }

    /// Expands the fold hiding `row`, if any.
    fn reveal_row(&mut self, row: usize, wrap_cols: usize, tab: usize) {
        let Some(&v) = self.layout.row_line.get(row) else {
            return;
        };
        if let Some(VLine::Fold(s, _)) = self.layout.lines.get(v) {
            self.expanded.insert(*s);
            self.layout_gen += 1;
            self.ensure_layout(wrap_cols, tab);
        }
    }

    /// First display line of a row and the line after its last wrapped segment.
    fn row_span(&self, r: usize) -> Option<(usize, usize)> {
        let lines = &self.layout.lines;
        let v0 = *self.layout.row_line.get(r)?;
        if !matches!(lines.get(v0), Some(VLine::Row(..))) {
            return None;
        }
        let mut v1 = v0 + 1;
        while matches!(lines.get(v1), Some(VLine::Row(x, _)) if *x == r) {
            v1 += 1;
        }
        Some((v0, v1))
    }

    /// Display lines covered by a block.
    fn block_span(&self, b: usize) -> (usize, usize) {
        let blk = &self.diff.blocks[b];
        let map = &self.layout.row_line;
        let v0 = map.get(blk.first_row).copied().unwrap_or(0);
        let v1 = map
            .get(blk.first_row + blk.rows)
            .copied()
            .unwrap_or(self.layout.lines.len());
        (v0, v1.max(v0 + 1))
    }

    /// The row shown nearest to display line `v`, looking in the given direction first.
    fn row_near(&self, v: usize, forward: bool) -> Option<usize> {
        let lines = &self.layout.panes[self.active_pane].shared;
        let v = v.min(lines.len().checked_sub(1)?);
        let row = |&index: &usize| match self.layout.lines[index] {
            VLine::Row(r, _) => Some(r),
            VLine::Fold(..) => None,
        };
        let find = |fwd: bool| {
            if fwd {
                lines[v..].iter().find_map(row)
            } else {
                lines[..=v].iter().rev().find_map(row)
            }
        };
        find(forward).or_else(|| find(!forward))
    }

    pub fn set_collapse(&mut self, on: bool) {
        self.collapse = on;
        self.expanded.clear();
        self.layout_gen += 1;
        self.scroll_to = self.cursor_row;
    }

    fn handle_keys(&mut self, ui: &Ui) {
        if !self.keys_enabled
            || widgets::typing(ui.ctx())
            || self.edit.is_some()
            || self.goto.is_some()
            || widgets::modal_open(ui.ctx())
            // An open menu takes the keys, Escape first of all: it closes only the menu.
            || egui::Popup::is_any_open(ui.ctx())
        {
            return;
        }
        if self.col_sel.is_some() && self.col_keys(ui) {
            return;
        }
        if self.caret.is_some() && self.caret_keys(ui) {
            return;
        }
        let pasted = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(s) => Some(s.clone()),
                _ => None,
            })
        });
        if let Some(text) = pasted {
            self.paste(&text);
        }
        let cut = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Cut)));
        let delete = self.selection().is_some()
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Delete)
                    | i.consume_key(Modifiers::NONE, Key::Backspace)
            });
        if cut || delete {
            let p = self.active_pane;
            if let Some((r0, r1)) = self.edit_rows().filter(|_| p < self.n()) {
                if cut {
                    if let Some(text) = self.selected_text(p) {
                        ui.ctx().copy_text(text);
                    }
                }
                self.delete_rows(p, r0, r1);
                self.sel_anchor = None;
            }
        }
        let pane_layout = &self.layout.panes[self.active_pane];
        let total = pane_layout.shared.len();
        if total == 0 {
            return;
        }
        let page = self.page_rows.max(1);
        let (cur, below) = match self.cursor_row.and_then(|r| self.row_span(r)) {
            Some((start, end)) => (pane_layout.boundaries[start], pane_layout.boundaries[end]),
            None => {
                let v = (pane_layout.position(self.scroll) as usize).min(total - 1);
                (v, v)
            }
        };
        let mut target: Option<(usize, bool)> = None;
        let mut extend = false;
        let mut copy = false;
        ui.input_mut(|i| {
            copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            // Unshifted patterns also match with Shift held, so the Shift ones go first.
            for (key, fwd) in [
                (Key::ArrowDown, true),
                (Key::ArrowUp, false),
                (Key::PageDown, true),
                (Key::PageUp, false),
            ] {
                if i.consume_key(Modifiers::SHIFT, key) {
                    target = Some(match (key, fwd) {
                        (Key::ArrowDown, _) => (below, true),
                        (Key::ArrowUp, _) => (cur.saturating_sub(1), false),
                        (_, true) => (cur + page, true),
                        _ => (cur.saturating_sub(page), false),
                    });
                    extend = true;
                }
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                target = Some((below, true));
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                target = Some((cur.saturating_sub(1), false));
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                target = Some((cur + page, true));
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                target = Some((cur.saturating_sub(page), false));
            }
            if i.consume_key(Modifiers::COMMAND, Key::Home) {
                target = Some((0, true));
            }
            if i.consume_key(Modifiers::COMMAND, Key::End) {
                target = Some((total - 1, false));
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) && self.active_pane > 0 {
                self.active_pane -= 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight)
                && self.active_pane + 1 < self.panes.len()
            {
                self.active_pane += 1;
            }
        });
        if copy {
            if let Some(text) = self.selected_text(self.active_pane) {
                ui.ctx().copy_text(text);
            }
        }
        if let Some(row) = target.and_then(|(v, fwd)| self.row_near(v, fwd)) {
            self.col_sel = None;
            self.sel_anchor = if extend {
                self.sel_anchor.or(self.cursor_row)
            } else {
                None
            };
            self.cursor_row = Some(row);
            self.scroll_to = Some(row);
            self.cur_block = self.diff.rows[row]
                .block()
                .filter(|&b| !self.diff.blocks[b].ignored);
        }
        if ui.input(|i| {
            i.modifiers.is_none() && (i.key_pressed(Key::F2) || i.key_pressed(Key::Enter))
        }) {
            if let Some(r) = self.cursor_row.filter(|&r| r < self.diff.rows.len()) {
                let p = self.active_pane;
                if self.table_mode {
                    self.begin_edit(r, p);
                } else {
                    // The caret goes to the end of the row's line, ready for typing.
                    let pos = match self.diff.rows[r].line(p) {
                        Some(l) => (l, self.panes[p].doc.lines[l].len()),
                        None => (self.insert_index(p, r), 0),
                    };
                    self.place_caret(p, pos, false);
                }
            }
        }
    }

    /// The keys of a column selection: typing, Backspace, Delete, Copy, Cut and Paste edit it,
    /// and Escape ends it. Returns true when the frame's keys were taken.
    fn col_keys(&mut self, ui: &Ui) -> bool {
        let mut typed = String::new();
        let mut paste = None;
        let (mut copy, mut cut) = (false, false);
        let (mut back, mut del, mut escape) = (false, false, false);
        ui.input_mut(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Text(t) => typed.push_str(t),
                    egui::Event::Paste(t) => paste = Some(t.clone()),
                    egui::Event::Copy => copy = true,
                    egui::Event::Cut => cut = true,
                    _ => {}
                }
            }
            back = i.consume_key(Modifiers::NONE, Key::Backspace);
            del = i.consume_key(Modifiers::NONE, Key::Delete);
            escape = i.consume_key(Modifiers::NONE, Key::Escape);
        });
        if escape {
            self.col_sel = None;
            return true;
        }
        if copy || cut {
            if let Some(text) = self.col_text() {
                ui.ctx().copy_text(text);
            }
        }
        if let Some(text) = paste {
            let (lines, ..) = text::split_lines(&text);
            if !lines.is_empty() {
                self.col_replace(&lines);
            }
        } else if !typed.is_empty() {
            self.col_replace(&[typed]);
        } else if cut || back || del {
            if cut || !self.col().is_some_and(|s| s.cols().is_empty()) {
                self.col_replace(&[]);
            } else {
                self.col_delete(del);
            }
        } else {
            return false;
        }
        true
    }

    /// The cursor row of every side one above the other, wrapped to the full width.
    fn line_detail(&mut self, ui: &Ui, rect: Rect, cfg: &Config, c: &Colors) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, c.ui.surface_alt);
        painter.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0_f32, c.ui.border),
        );
        let Some(r) = self.cursor_row.filter(|&r| r < self.diff.rows.len()) else {
            return;
        };
        self.ensure_inline(r);
        let n = self.n();
        let font = FontId::monospace(cfg.font_size);
        let label_w = 70.0;
        let slot_h = (rect.height() - 6.0) / n as f32;
        let row = self.diff.rows[r];
        let sel = row.block().is_some() && row.block() == self.cur_block;
        let inline_bg = if sel { c.inline_sel } else { c.inline };
        for p in 0..n {
            let slot = Rect::from_min_size(
                pos2(rect.left(), rect.top() + 3.0 + p as f32 * slot_h),
                vec2(rect.width(), slot_h),
            )
            .shrink2(vec2(6.0, 2.0));
            if let Some(bg) = self.row_bg(r, p, c) {
                painter.rect_filled(slot, theme::radius::XS, bg);
            }
            let label = match row.line(p) {
                Some(l) => format!("{} {}", side_name(p, n), l + 1),
                None => side_name(p, n).to_string(),
            };
            painter.text(
                slot.left_top() + vec2(4.0, 1.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(cfg.font_size - 1.0),
                c.gutter_text,
            );
            let Some(l) = row.line(p) else { continue };
            let line = &self.panes[p].doc.lines[l];
            let inline = self
                .inline_cache
                .get(&r)
                .map(|v| v[p].as_slice())
                .unwrap_or(&[]);
            let mut job = Self::line_job(
                line,
                None,
                inline,
                &[],
                inline_bg,
                c.find_hit,
                c.text,
                &font,
                cfg.tab_size,
                cfg.show_whitespace,
            );
            job.wrap.max_width = slot.width() - label_w - 4.0;
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.with_clip_rect(slot).galley(
                slot.left_top() + vec2(label_w, 1.0),
                galley,
                c.text,
            );
        }
    }

    /// The dialog editing a bookmark's comment.
    fn note_window(&mut self, ui: &Ui) {
        let Some((p, l, mut text, focus)) = self.note_edit.take() else {
            return;
        };
        let mut open = !ui.input(|i| i.key_pressed(Key::Escape));
        let mut done = false;
        let mut cancel = false;
        egui::Window::new(tr("dlg.bookmark_comment"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} {} {}",
                    side_name(p, self.n()),
                    tr("label.line"),
                    l + 1
                ));
                let r = ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .desired_rows(4)
                        .desired_width(360.0),
                );
                if focus {
                    r.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("btn.ok")).clicked() {
                        done = true;
                    }
                    if ui.button(tr("btn.cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if done {
            if l < self.panes[p].doc.lines.len() {
                self.bookmarks[p].insert(l, text.trim().to_string());
            }
        } else if open && !cancel {
            self.note_edit = Some((p, l, text, false));
        }
    }

    /// The rows matched by key, when the table view shows them.
    fn keyed(&self) -> Option<&TableDiff> {
        match self.table.keyed.as_deref() {
            Some(Ok(d)) if self.table_mode => Some(d),
            _ => None,
        }
    }

    /// Whether the table rows wait for a background match by key, so no rows can be copied or
    /// stepped through yet.
    fn matching(&self) -> bool {
        self.table_mode && self.table.job.is_some()
    }

    /// Matches the records of the two sides by the key columns of [`Self::table_match`]: at
    /// once, or for large tables in the background, selecting `selected` again when it ends.
    fn match_table_by_key(&mut self, selected: Option<[Option<usize>; 3]>) {
        let (Some(Ok(a)), Some(Ok(b))) = (self.table.docs.first(), self.table.docs.get(1)) else {
            return;
        };
        let (a, b) = (a.clone(), b.clone());
        let limit = self.table.background_at.unwrap_or(BACKGROUND_RECORDS);
        if a.records.len() + b.records.len() <= limit {
            let result = match_records(&a, &b, &self.table_match, &self.opts, &|_| false);
            self.show_keyed(result);
            return;
        }
        let (m, opts) = (self.table_match.clone(), self.opts.clone());
        self.table.job = Some(super::analysis::Task::start(move |cancel, progress| {
            let stop = |done: usize| {
                progress.store(done, std::sync::atomic::Ordering::Relaxed);
                cancel.load(std::sync::atomic::Ordering::Relaxed)
            };
            Ok(match_records(&a, &b, &m, &opts, &stop))
        }));
        self.table.restore = selected;
        self.table.diff = DiffResult::default();
    }

    /// Ends a background match by key that has finished.
    fn poll_table_job(&mut self) {
        let Some(job) = self.table.job.as_mut() else {
            return;
        };
        let cancelled = job.cancel.clone();
        let Some(result) = job.poll() else {
            return;
        };
        self.table.job = None;
        let problem = if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            table_match::Problem::Cancelled
        } else {
            table_match::Problem::Failed
        };
        self.show_keyed(result.unwrap_or(Err(problem)));
        if !matches!(self.table.keyed.as_deref(), Some(Ok(_))) {
            self.table_by_position();
        }
        let restore = self.table.restore.take();
        self.select_table_lines(restore);
    }

    /// Shows the rows matched by key, or keeps why they could not be.
    fn show_keyed(&mut self, result: Result<TableDiff, table_match::Problem>) {
        if let Ok(d) = &result {
            let mut blocks = Vec::new();
            let rows = d
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let block = r.differs(d.ignore_order).then(|| {
                        let range = |x: Option<usize>| x.map_or(0..0, |x| x..x + 1);
                        blocks.push(diff::Block {
                            ranges: [range(r.records[0]), range(r.records[1]), 0..0],
                            kind: BlockKind::Diff,
                            ignored: false,
                            first_row: i,
                            rows: 1,
                        });
                        blocks.len() - 1
                    });
                    diff::Row::new([r.records[0], r.records[1], None], block)
                })
                .collect();
            self.table.diff = DiffResult {
                panes: 2,
                rows,
                blocks,
                moved: Default::default(),
            };
        }
        self.table.keyed = Some(Box::new(result));
    }

    /// Aligns the records of the sides by position, as text lines are.
    fn table_by_position(&mut self) {
        let keys: Vec<Vec<String>> = self
            .table
            .docs
            .iter()
            .filter_map(|t| t.as_ref().ok())
            .map(|t| t.keys(self.opts.ignore_eol))
            .collect();
        self.table.diff = diff::compute_with(
            &keys.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &self.opts,
            None,
        );
    }

    /// Selects the row holding one of the record `lines`, or none.
    fn select_table_lines(&mut self, lines: Option<[Option<usize>; 3]>) {
        self.table.selected =
            lines.and_then(|lines| {
                self.table.diff.rows.iter().position(|r| {
                    (0..self.n()).any(|p| lines[p].is_some() && r.line(p) == lines[p])
                })
            });
    }

    /// The rows matched by key that are differences.
    fn keyed_differences(&self) -> Option<Vec<usize>> {
        let d = self.keyed()?;
        Some(
            (0..d.rows.len())
                .filter(|&i| d.rows[i].differs(d.ignore_order))
                .collect(),
        )
    }

    /// Selects row `row` of the rows matched by key, on a side that has its record, at its
    /// first changed column.
    fn select_keyed_row(&mut self, row: usize) {
        let Some(r) = self.keyed().and_then(|d| d.rows.get(row)) else {
            return;
        };
        let active = self.active_pane.min(1);
        let pane = if r.records[active].is_some() {
            active
        } else {
            usize::from(r.records[0].is_none())
        };
        let column = r.changed[pane]
            .first()
            .copied()
            .unwrap_or(self.table.column);
        self.select_table_row(row, pane, column);
    }

    /// Next or previous difference of the rows matched by key; `None` when rows are not
    /// matched by key.
    fn step_keyed(&mut self, forward: bool) -> Option<bool> {
        let rows = self.keyed_differences()?;
        let cur = self.table.selected;
        let found = if forward {
            rows.into_iter().find(|&r| cur.is_none_or(|c| r > c))
        } else {
            rows.into_iter().rev().find(|&r| cur.is_none_or(|c| r < c))
        };
        if let Some(r) = found {
            self.select_keyed_row(r);
        }
        Some(found.is_some())
    }

    /// Copies row `row` of the rows matched by key from side `from` to side `to`: the changed
    /// cells of a record both sides have, the record only `from` has, or removes the record
    /// only `to` has. Records with an ambiguous key are not copied.
    fn copy_keyed_row(&mut self, row: usize, from: usize, to: usize) {
        let Some(d) = self.keyed() else { return };
        let Some(r) = d.rows.get(row).cloned() else {
            return;
        };
        if r.state == RowState::Ambiguous {
            self.status = tr("table.ambiguous_copy").into();
            return;
        }
        if !r.differs(true) || from == to || !self.writable(to) {
            return;
        }
        let Some(delim) = self.table_delim else {
            return;
        };
        let d = self.keyed().unwrap();
        let (Some(Ok(src)), Some(Ok(dst))) = (self.table.docs.get(from), self.table.docs.get(to))
        else {
            return;
        };
        // Whole records keep their cells in place only when columns pair by position.
        let in_place = d
            .columns
            .iter()
            .all(|c| c[0].is_none() || c[1].is_none() || c[0] == c[1]);
        let eol = self.panes[to].doc.eol.as_str();
        let result = match (r.records[from], r.records[to]) {
            (Some(s), Some(t)) => {
                match copy_record_cells(d, &r, (src, s, from), (dst, t, to), delim) {
                    Some(source) => Ok(source),
                    None if in_place => Ok(dst.copy_lines(
                        dst.records[t].lines.clone(),
                        src,
                        src.records[s].lines.clone(),
                        eol,
                    )),
                    None => Err(tr("table.copy_columns_differ").to_string()),
                }
            }
            (Some(s), None) if in_place => {
                let at = d.rows[..row]
                    .iter()
                    .rev()
                    .find_map(|x| x.records[to])
                    .map_or(0, |t| dst.records[t].lines.end);
                Ok(dst.copy_lines(at..at, src, src.records[s].lines.clone(), eol))
            }
            (Some(_), None) => Err(tr("table.copy_columns_differ").to_string()),
            (None, Some(t)) => Ok(dst.copy_lines(dst.records[t].lines.clone(), src, 0..0, eol)),
            (None, None) => return,
        };
        let removed = r.records[from].is_none();
        match result.and_then(|source| self.replace_table_source(to, source)) {
            Ok(()) => {
                // The rows after a removed record move up into its place.
                if removed {
                    self.table.selected = row.checked_sub(1);
                }
                self.step_keyed(true);
            }
            Err(error) => self.status = error,
        }
    }

    /// Whether table row `row` holds raw row `raw` on side `pane`.
    fn holds(&self, row: &diff::Row, raw: &diff::Row, pane: usize) -> bool {
        match (row.line(pane), raw.line(pane)) {
            (Some(record), Some(line)) => self.table.docs[pane]
                .as_ref()
                .is_ok_and(|table| table.records[record].lines.contains(&line)),
            _ => false,
        }
    }

    /// The table row holding raw row `raw_row`, preferring the record of side `pane`.
    fn table_row_at(&self, raw_row: usize, pane: usize) -> Option<usize> {
        let raw = self.diff.rows.get(raw_row)?;
        let rows = &self.table.diff.rows;
        rows.iter()
            .position(|row| self.holds(row, raw, pane))
            .or_else(|| {
                rows.iter()
                    .position(|row| (0..self.n()).any(|p| self.holds(row, raw, p)))
            })
    }

    fn table_cursor(&self) -> Option<usize> {
        if self.keyed().is_some() {
            if let Some(row) = self
                .table
                .selected
                .filter(|&r| r < self.table.diff.rows.len())
            {
                return Some(row);
            }
        }
        self.table_row_at(self.cursor_row?, self.active_pane)
    }

    /// Pick an actual changed physical line within the record, even when its opening
    /// line is unchanged. Prefer the current pane, then use another side for gaps.
    fn select_table_row(&mut self, row: usize, pane: usize, column: usize) {
        let Some(aligned) = self.table.diff.rows.get(row) else {
            return;
        };
        self.table.selected = Some(row);
        let contains = |raw: &diff::Row, side: usize| match (aligned.line(side), raw.line(side)) {
            (Some(record), Some(line)) => self.table.docs[side]
                .as_ref()
                .is_ok_and(|table| table.records[record].lines.contains(&line)),
            _ => false,
        };
        let significant =
            |raw: &diff::Row| raw.block().is_some_and(|b| !self.diff.blocks[b].ignored);
        let rows = &self.diff.rows;
        let found = rows
            .iter()
            .position(|r| contains(r, pane) && significant(r))
            .or_else(|| {
                rows.iter()
                    .position(|r| significant(r) && (0..self.n()).any(|p| contains(r, p)))
            })
            .or_else(|| rows.iter().position(|r| contains(r, pane)))
            .or_else(|| {
                rows.iter()
                    .position(|r| (0..self.n()).any(|p| contains(r, p)))
            });
        if let Some(raw) = found {
            self.active_pane = pane;
            self.table.column = column;
            self.cursor_row = Some(raw);
            self.scroll_to = Some(raw);
            self.cur_block = rows[raw].block().filter(|&b| !self.diff.blocks[b].ignored);
            self.sel_anchor = None;
        }
    }

    fn handle_table_keys(&mut self, ui: &Ui) {
        if !self.keys_enabled
            || widgets::typing(ui.ctx())
            || self.table.edit.is_some()
            || self.goto.is_some()
            || widgets::modal_open(ui.ctx())
            || self.table.diff.rows.is_empty()
        {
            return;
        }
        let current = self.table_cursor().unwrap_or(0);
        let (mut target, mut pane, mut column) = (current, self.active_pane, self.table.column);
        let last = self.table.diff.rows.len() - 1;
        let mut navigate = false;
        let mut edit = false;
        ui.input_mut(|input| {
            for (key, next) in [
                (Key::ArrowDown, current.saturating_add(1).min(last)),
                (Key::ArrowUp, current.saturating_sub(1)),
                (
                    Key::PageDown,
                    current.saturating_add(self.page_rows).min(last),
                ),
                (Key::PageUp, current.saturating_sub(self.page_rows)),
            ] {
                if input.consume_key(Modifiers::NONE, key) {
                    target = next;
                    navigate = true;
                }
            }
            if input.consume_key(Modifiers::COMMAND, Key::Home) {
                target = 0;
                navigate = true;
            }
            if input.consume_key(Modifiers::COMMAND, Key::End) {
                target = last;
                navigate = true;
            }
            let columns = self
                .table
                .docs
                .iter()
                .filter_map(|t| t.as_ref().ok())
                .flat_map(|t| &t.records)
                .map(|r| r.cells.len())
                .max()
                .unwrap_or(1)
                .max(1);
            if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
                if column + 1 < columns {
                    column += 1;
                } else if pane + 1 < self.n() {
                    pane += 1;
                    column = 0;
                }
                navigate = true;
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                if column > 0 {
                    column -= 1;
                } else if pane > 0 {
                    pane -= 1;
                    column = columns - 1;
                }
                navigate = true;
            }
            edit = input.consume_key(Modifiers::NONE, Key::F2)
                || input.consume_key(Modifiers::NONE, Key::Enter);
        });
        if navigate {
            self.select_table_row(target, pane, column);
        }
        let Some(record) = self.table.diff.rows[target].line(pane) else {
            return;
        };
        if edit {
            self.begin_cell_edit(pane, record, column);
        }
        if let Some(value) = self.table.docs[pane]
            .as_ref()
            .ok()
            .and_then(|t| t.records[record].cells.get(column))
        {
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
                ui.ctx().copy_text(value.clone());
            }
        }
        let pasted = ui.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Paste(value) = e {
                    Some(value.clone())
                } else {
                    None
                }
            })
        });
        if let Some(value) = pasted {
            self.begin_cell_edit(pane, record, column);
            if let Some(mut edit) = self.table.edit.take() {
                edit.value = value;
                if let Err(error) = self.apply_cell_edit(&edit) {
                    self.status = error;
                }
            }
        }
    }

    fn begin_cell_edit(&mut self, pane: usize, record: usize, column: usize) {
        if !self.writable(pane) {
            return;
        }
        let Some(table) = self.table.docs.get(pane).and_then(|t| t.as_ref().ok()) else {
            return;
        };
        let Some(value) = table.records.get(record).and_then(|r| r.cells.get(column)) else {
            return;
        };
        self.table.edit = Some(CellEdit {
            pane,
            record,
            column,
            value: value.clone(),
            source: table.source.clone(),
            focus: true,
        });
    }

    fn apply_cell_edit(&mut self, edit: &CellEdit) -> Result<(), String> {
        if !self.writable(edit.pane) {
            return Err(tr("msg.readonly").into());
        }
        if table::source(&self.panes[edit.pane].doc) != edit.source {
            return Err(tr("table.stale").into());
        }
        let delim = self.table_delim.ok_or("No table delimiter")?;
        let table = table::Table::parse(edit.source.clone(), delim)?;
        let source = table.replace_cell(edit.record, edit.column, &edit.value, delim)?;
        if source == edit.source {
            return Ok(());
        }
        self.replace_table_source(edit.pane, source)
    }

    fn replace_table_source(&mut self, pane: usize, source: String) -> Result<(), String> {
        table::Table::parse(
            source.clone(),
            self.table_delim.ok_or("No table delimiter")?,
        )?;
        self.replace_source(pane, source);
        Ok(())
    }

    /// Replaces the text of pane `pane` as one undoable edit, keeping its line breaks where its
    /// lines stay.
    fn replace_source(&mut self, pane: usize, source: String) {
        if source == table::source(&self.panes[pane].doc) {
            return;
        }
        self.push_undo();
        self.undo.last_mut().unwrap().formats[pane] = Some(format_of(&self.panes[pane].doc));
        let parsed = TextDoc::from_text(None, &source);
        let doc = &mut self.panes[pane].doc;
        doc.lines = parsed.lines;
        doc.eol = parsed.eol;
        doc.final_newline = parsed.final_newline;
        doc.mixed_eol = parsed.mixed_eol;
        doc.original_eols = parsed.original_eols;
        doc.preserve_eols = true;
        self.mark_changed(&[pane]);
    }

    fn copy_cell(&mut self, row: usize, column: usize, from: usize, to: usize) {
        let Some(aligned) = self.table.diff.rows.get(row) else {
            return;
        };
        let (Some(src), Some(dst)) = (aligned.line(from), aligned.line(to)) else {
            return;
        };
        let target = match self.keyed().and_then(|d| Some((d, d.rows.get(row)?))) {
            Some((_, m)) if m.state == RowState::Ambiguous => {
                self.status = tr("table.ambiguous_copy").into();
                return;
            }
            Some((d, _)) => d.paired_column(from, column, to),
            None => Some(column),
        };
        let Some(target) = target else {
            self.status = tr("table.copy_columns_differ").into();
            return;
        };
        let (Ok(a), Ok(b)) = (&self.table.docs[from], &self.table.docs[to]) else {
            return;
        };
        let Some(value) = a.records[src].cells.get(column) else {
            return;
        };
        let edit = CellEdit {
            pane: to,
            record: dst,
            column: target,
            value: value.clone(),
            source: b.source.clone(),
            focus: false,
        };
        if let Err(error) = self.apply_cell_edit(&edit) {
            self.status = error;
        }
    }

    fn cell_window(&mut self, ui: &Ui) {
        let Some(mut edit) = self.table.edit.take() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(tr("table.edit_cell"))
            .id(self.id.with("cell_edit"))
            .collapsible(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} · {} {} · {} {}",
                    self.panes[edit.pane].doc.title(),
                    tr("table.record"),
                    edit.record + 1,
                    tr("table.column"),
                    edit.column + 1
                ));
                let response = ui.add(
                    egui::TextEdit::multiline(&mut edit.value)
                        .desired_rows(5)
                        .desired_width(420.0),
                );
                if edit.focus {
                    response.request_focus();
                    edit.focus = false;
                }
                ui.horizontal(|ui| {
                    apply = ui.button(tr("btn.ok")).clicked();
                    cancel = ui.button(tr("btn.cancel")).clicked();
                });
            });
        if apply {
            if let Err(error) = self.apply_cell_edit(&edit) {
                self.status = error;
            }
        } else if open && !cancel {
            self.table.edit = Some(edit);
        }
    }

    /// Record-aware rendering leaves the raw text diff intact for reports and ordinary merges.
    /// Matches records as `m` says, keeping the tolerance field in step.
    pub fn set_table_match(&mut self, m: TableMatch) {
        self.table.tolerance = tolerance_text(&m);
        if m != self.table_match {
            self.table_match = m;
            self.rediff();
        }
    }

    /// Choosing how the table view matches records, and what matching found.
    fn table_match_bar(&mut self, ui: &mut Ui, c: &Colors) {
        let before = self.table_match.clone();
        let names: Vec<String> = self
            .table
            .docs
            .first()
            .and_then(|t| t.as_ref().ok())
            .and_then(|t| t.records.first())
            .map(|r| r.cells.iter().map(|c| c.trim().to_string()).collect())
            .unwrap_or_default();
        let width = self
            .table
            .docs
            .iter()
            .filter_map(|t| t.as_ref().ok())
            .flat_map(|t| t.records.first())
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .max(names.len());
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("table.match_by"));
            let mut by_key = self.table_match.by_key();
            egui::ComboBox::from_id_salt(self.id.with("match_by"))
                .selected_text(tr(if by_key {
                    "table.match_keys"
                } else {
                    "table.match_position"
                }))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut by_key, false, tr("table.match_position"));
                    ui.selectable_value(&mut by_key, true, tr("table.match_keys"));
                });
            if by_key != self.table_match.by_key() {
                if by_key {
                    self.table_match.header = true;
                    self.table_match.keys = vec![key_column(&names, 0, true)];
                } else {
                    self.table_match.keys.clear();
                }
            }
            if !self.table_match.by_key() {
                return;
            }
            let mut header = self.table_match.header;
            if ui.checkbox(&mut header, tr("table.header_row")).changed() {
                self.table_match.header = header;
                self.table_match.keys = self
                    .table_match
                    .keys
                    .iter()
                    .map(|k| {
                        let column = match k {
                            KeyColumn::Index(i) => Some(*i),
                            KeyColumn::Name(n) => names.iter().position(|x| x == n),
                        };
                        // A name no column has stays, to be shown as missing and removed.
                        column.map_or_else(|| k.clone(), |c| key_column(&names, c, header))
                    })
                    .collect();
                if !header {
                    self.table_match.columns = table_match::ColumnPolicy::Position;
                }
            }
            let chosen: Vec<String> = self
                .table_match
                .keys
                .iter()
                .map(|k| key_label(k, &names))
                .collect();
            ui.menu_button(
                format!("{}: {}", tr("table.choose_keys"), chosen.join(" + ")),
                |ui| {
                    let listed: Vec<KeyColumn> = (0..width)
                        .map(|column| key_column(&names, column, self.table_match.header))
                        .collect();
                    // Saved keys no column has, such as from a project, can be unticked too.
                    let missing: Vec<KeyColumn> = self
                        .table_match
                        .keys
                        .iter()
                        .filter(|k| !listed.contains(k))
                        .cloned()
                        .collect();
                    for key in listed.into_iter().chain(missing) {
                        let mut on = self.table_match.keys.contains(&key);
                        if ui.checkbox(&mut on, key_label(&key, &names)).changed() {
                            if on {
                                self.table_match.keys.push(key);
                            } else if self.table_match.keys.len() > 1 {
                                self.table_match.keys.retain(|k| *k != key);
                            }
                        }
                    }
                },
            );
            ui.label(tr("table.columns_by"));
            let columns = &mut self.table_match.columns;
            egui::ComboBox::from_id_salt(self.id.with("columns_by"))
                .selected_text(tr(match columns {
                    table_match::ColumnPolicy::Position => "table.columns_position",
                    table_match::ColumnPolicy::Name => "table.columns_name",
                }))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        columns,
                        table_match::ColumnPolicy::Position,
                        tr("table.columns_position"),
                    );
                    ui.add_enabled_ui(header, |ui| {
                        ui.selectable_value(
                            columns,
                            table_match::ColumnPolicy::Name,
                            tr("table.columns_name"),
                        );
                    });
                });
            ui.checkbox(&mut self.table_match.ignore_order, tr("table.ignore_order"));
            ui.label(tr("table.order"));
            let order = &mut self.table_match.order;
            let order_key = |o: table_match::RowOrder| match o {
                table_match::RowOrder::Left => "table.order_left",
                table_match::RowOrder::Right => "table.order_right",
                table_match::RowOrder::Key => "table.order_key",
            };
            egui::ComboBox::from_id_salt(self.id.with("row_order"))
                .selected_text(tr(order_key(*order)))
                .show_ui(ui, |ui| {
                    for o in [
                        table_match::RowOrder::Left,
                        table_match::RowOrder::Right,
                        table_match::RowOrder::Key,
                    ] {
                        ui.selectable_value(order, o, tr(order_key(o)));
                    }
                })
                .response
                .on_hover_text(tr("tip.table_order"));
            ui.label(tr("table.tolerance"));
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut self.table.tolerance)
                        .desired_width(64.0)
                        .hint_text("0"),
                )
                .on_hover_text(tr("tip.table_tolerance"));
            if response.changed() {
                if let Some((abs, rel)) = parse_tolerance(&self.table.tolerance) {
                    self.table_match.abs_tolerance = abs;
                    self.table_match.rel_tolerance = rel;
                }
            }
        });
        if self.table_match != before {
            self.rediff();
        }
        let mut show = None;
        let mut again = false;
        if let Some(job) = &self.table.job {
            let total: usize = self
                .table
                .docs
                .iter()
                .filter_map(|t| t.as_ref().ok())
                .map(|t| t.records.len())
                .sum();
            // the records are handled twice: their keys first, then the records themselves
            let done = job.progress.load(std::sync::atomic::Ordering::Relaxed) / 2;
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(trf(
                    "table.matching",
                    &[
                        ("done", done.min(total).to_string()),
                        ("total", total.to_string()),
                    ],
                ));
                if ui.button(tr("btn.cancel")).clicked() {
                    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
        match self.table.keyed.as_deref() {
            Some(Ok(d)) => {
                ui.horizontal_wrapped(|ui| {
                    let n = d.counts;
                    ui.label(trf(
                        "table.summary",
                        &[
                            ("added", n.added.to_string()),
                            ("removed", n.removed.to_string()),
                            ("modified", n.modified.to_string()),
                            ("moved", n.moved.to_string()),
                            ("ambiguous", n.ambiguous.to_string()),
                        ],
                    ));
                    if d.ignore_order && n.moved > 0 {
                        ui.weak(tr("table.order_ignored"));
                    }
                    if n.ambiguous > 0 && ui.button(tr("table.show_ambiguous")).clicked() {
                        let ambiguous = || {
                            (0..d.rows.len()).filter(|&i| d.rows[i].state == RowState::Ambiguous)
                        };
                        let after = self.table.selected;
                        show = ambiguous()
                            .find(|&i| after.is_none_or(|s| i > s))
                            .or_else(|| ambiguous().next());
                    }
                });
                for warning in &d.warnings {
                    ui.colored_label(c.ui.warning, warning_text(warning));
                }
            }
            Some(Err(problem)) => {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        c.ui.warning,
                        format!(
                            "{}: {}",
                            crate::core::problem::Problem::of_table(problem).label(),
                            problem_text(problem)
                        ),
                    );
                    if *problem == table_match::Problem::Cancelled {
                        again = ui.button(tr("table.match_again")).clicked();
                    }
                });
            }
            None => {}
        }
        if let Some(row) = show {
            self.select_keyed_row(row);
        }
        if again {
            self.rediff();
        }
        ui.separator();
    }

    fn table_ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.poll_table_job();
        // matched when table mode is turned on, not while it is off
        if self.table_match.by_key()
            && self.n() == 2
            && self.table.keyed.is_none()
            && self.table.job.is_none()
            && self.table.docs.iter().all(Result::is_ok)
        {
            let selected = self
                .table
                .selected
                .and_then(|r| self.table.diff.rows.get(r))
                .map(|r| r.lines());
            self.match_table_by_key(selected);
            if self.table.job.is_none() {
                self.select_table_lines(selected);
            }
        }
        if let Some(error) = self.table.docs.iter().find_map(|t| t.as_ref().err()) {
            ui.colored_label(c.ui.warning, error);
            ui.label(tr("table.invalid"));
            if ui.button(tr("cmd.table_mode")).clicked() {
                self.table_mode = false;
            }
            return;
        }
        if self.n() == 2 {
            self.table_match_bar(ui, c);
        }
        if self.table.job.is_some() {
            return;
        }
        let n = self.n();
        let columns = self
            .table
            .docs
            .iter()
            .filter_map(|t| t.as_ref().ok())
            .flat_map(|t| &t.records)
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .max(1);
        let line_height = cfg.font_size + 6.0;
        let heights: Vec<f32> = self
            .table
            .diff
            .rows
            .iter()
            .map(|row| {
                (0..n)
                    .filter_map(|p| {
                        row.line(p)
                            .map(|r| &self.table.docs[p].as_ref().unwrap().records[r])
                    })
                    .flat_map(|r| &r.cells)
                    .map(|s| s.lines().count().clamp(1, 8))
                    .max()
                    .unwrap_or(1) as f32
                    * line_height
                    + 8.0
            })
            .collect();
        let keyed = self.keyed().is_some();
        let target = self.scroll_to.take().and_then(|raw_row| {
            let raw = self.diff.rows.get(raw_row)?;
            self.table
                .selected
                .filter(|&s| {
                    keyed
                        && self
                            .table
                            .diff
                            .rows
                            .get(s)
                            .is_some_and(|row| (0..n).any(|p| self.holds(row, raw, p)))
                })
                .or_else(|| self.table_row_at(raw_row, self.active_pane))
        });
        if keyed && target.is_some() {
            self.table.selected = target;
        }
        let cursor = self.table_cursor();
        let normalizer = diff::Normalizer::new(&self.opts);
        let mut clicked = None;
        let mut edit_cell = None;
        let mut copy = None;
        egui::ScrollArea::horizontal()
            .id_salt(self.id.with("table_horizontal"))
            .show(ui, |ui| {
                ui.set_min_width((columns * n) as f32 * 140.0 + 60.0);
                let mut builder = egui_extras::TableBuilder::new(ui)
                    .id_salt(self.id.with("records"))
                    .resizable(true)
                    .striped(true)
                    .sense(Sense::click())
                    .column(egui_extras::Column::initial(55.0).at_least(40.0))
                    .columns(
                        egui_extras::Column::initial(140.0)
                            .at_least(60.0)
                            .clip(true),
                        columns * n,
                    );
                if let Some(row) = target {
                    builder = builder.scroll_to_row(row, Some(egui::Align::Center));
                }
                builder
                    .header(34.0, |mut header| {
                        header.col(|ui| {
                            ui.label(tr("table.record"));
                        });
                        for p in 0..n {
                            for column in 0..columns {
                                header.col(|ui| {
                                    ui.label(format!("{} · {}", side_name(p, n), column + 1))
                                        .on_hover_text(self.panes[p].doc.title());
                                });
                            }
                        }
                    })
                    .body(|body| {
                        body.heterogeneous_rows(heights.into_iter(), |mut row| {
                            let index = row.index();
                            let aligned = self.table.diff.rows[index];
                            let matched = self.keyed().and_then(|d| d.rows.get(index));
                            row.col(|ui| {
                                if cursor == Some(index) {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, c.cursor_row);
                                }
                                match matched {
                                    Some(m) => {
                                        let (mark, tip, fill) = row_marker(m, c);
                                        if let Some(fill) = fill {
                                            let mut rect = ui.max_rect();
                                            rect.set_width(4.0);
                                            ui.painter().rect_filled(rect, 0.0, fill);
                                        }
                                        ui.label(format!("{} {mark}", index + 1))
                                            .on_hover_text(tip);
                                    }
                                    None => {
                                        ui.label((index + 1).to_string());
                                    }
                                }
                            });
                            for p in 0..n {
                                for column in 0..columns {
                                    row.col(|ui| {
                                        let record = aligned.line(p).map(|r| {
                                            &self.table.docs[p].as_ref().unwrap().records[r]
                                        });
                                        let value = record.and_then(|r| r.cells.get(column));
                                        let changed = match matched {
                                            Some(m) => m.changed[p].contains(&column),
                                            None => {
                                                value.is_some()
                                                    && aligned.block().is_some_and(|b| {
                                                        !self.table.diff.blocks[b].ignored
                                                    })
                                                    && (0..n).filter(|&q| q != p).any(|q| {
                                                        let other = aligned.line(q).and_then(|r| {
                                                            self.table.docs[q]
                                                                .as_ref()
                                                                .unwrap()
                                                                .records[r]
                                                                .cells
                                                                .get(column)
                                                        });
                                                        match (value, other) {
                                                            (Some(a), Some(b)) => {
                                                                let clean = |s: &str| {
                                                                    if self.opts.ignore_eol {
                                                                        s.replace("\r\n", "\n")
                                                                            .replace('\r', "\n")
                                                                    } else {
                                                                        s.to_string()
                                                                    }
                                                                };
                                                                !normalizer
                                                                    .close(&clean(a), &clean(b))
                                                            }
                                                            _ => value != other,
                                                        }
                                                    })
                                            }
                                        };
                                        if changed {
                                            ui.painter().rect_filled(ui.max_rect(), 0.0, c.inline);
                                        }
                                        let response = ui
                                            .add(
                                                egui::Label::new(
                                                    value.map(String::as_str).unwrap_or("—"),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_text(value.map(String::as_str).unwrap_or(""));
                                        if response.clicked() {
                                            clicked = Some((index, p, column));
                                        }
                                        if response.double_clicked() {
                                            if let Some(r) = aligned.line(p) {
                                                edit_cell = Some((p, r, column));
                                            }
                                        }
                                        response.context_menu(|ui| {
                                            if let Some(r) = aligned.line(p) {
                                                if ui
                                                    .add_enabled(
                                                        !self.panes[p].doc.read_only
                                                            && value.is_some(),
                                                        egui::Button::new(tr("table.edit_cell")),
                                                    )
                                                    .clicked()
                                                {
                                                    edit_cell = Some((p, r, column));
                                                    ui.close();
                                                }
                                                let safe = matched
                                                    .is_none_or(|m| m.state != RowState::Ambiguous);
                                                for to in 0..n {
                                                    if to != p
                                                        && aligned.line(to).is_some()
                                                        && ui
                                                            .add_enabled(
                                                                !self.panes[to].doc.read_only
                                                                    && value.is_some()
                                                                    && safe,
                                                                egui::Button::new(format!(
                                                                    "{} → {}",
                                                                    tr("table.copy_cell"),
                                                                    side_name(to, n)
                                                                )),
                                                            )
                                                            .clicked()
                                                    {
                                                        copy = Some((index, column, p, to));
                                                        ui.close();
                                                    }
                                                }
                                            }
                                        });
                                    });
                                }
                            }
                        });
                    });
            });
        if let Some((row, pane, column)) = clicked {
            self.select_table_row(row, pane, column);
        }
        if let Some((p, r, col)) = edit_cell {
            self.begin_cell_edit(p, r, col);
        }
        if let Some((r, col, from, to)) = copy {
            self.copy_cell(r, col, from, to);
        }
        self.cell_window(ui);
    }

    fn goto_window(&mut self, ui: &Ui) {
        // go to line dialog
        if let Some(mut g) = self.goto.take() {
            let mut open = !ui.input(|i| i.key_pressed(Key::Escape));
            let mut go = false;
            let max = self.panes[self.active_pane].doc.lines.len().max(1);
            egui::Window::new(tr("dlg.goto"))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("label.line"));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut g).hint_text(format!("1 – {max}")),
                        );
                        r.request_focus();
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            go = true;
                        }
                        if ui.button(tr("btn.ok")).clicked() {
                            go = true;
                        }
                    });
                });
            if go {
                if let Ok(line) = g.trim().parse::<usize>() {
                    self.goto_line(line);
                }
            } else if open {
                self.goto = Some(g);
            }
        }
    }

    /// Works out the edited lines of every pane, when they are marked.
    fn update_edited(&mut self) {
        for pane in &mut self.panes {
            pane.edited = if self.edit_markers {
                edited_lines(&pane.baseline, &pane.doc.lines)
            } else {
                HashSet::new()
            };
        }
    }

    fn review_within_budget(&self) -> bool {
        self.panes.iter().all(|pane| {
            let mut bytes = 0usize;
            pane.doc.lines.iter().enumerate().all(|(i, line)| {
                let ending = if i + 1 < pane.doc.lines.len() || pane.doc.final_newline {
                    if pane.doc.preserve_eols {
                        pane.doc.line_eol(i).map_or(0, |e| e.as_str().len())
                    } else {
                        pane.doc.eol.as_str().len()
                    }
                } else {
                    0
                };
                bytes = bytes.saturating_add(line.len()).saturating_add(ending);
                bytes as u64 <= crate::core::analysis::MAX_FILE
            })
        })
    }
    pub fn restore_review_cursor(&mut self) {
        let Some(id) = self
            .review_decision
            .as_ref()
            .and_then(|review| review.current_hunk.clone())
        else {
            return;
        };
        if !self.review_within_budget() {
            if let Some(review) = &mut self.review_decision {
                review.stale = true;
                self.review_dirty = true;
            }
            return;
        }
        let matches: Vec<_> = self
            .diff
            .blocks
            .iter()
            .enumerate()
            .take(1000)
            .filter_map(|(i, block)| (self.review_hunk_id(block) == id).then_some(i))
            .collect();
        if matches.len() == 1 && self.diff.blocks.len() <= 1000 {
            self.select_block(matches[0]);
        } else if let Some(review) = &mut self.review_decision {
            review.stale = true;
            self.review_dirty = true;
        }
    }
    fn review_hunk_id(&self, block: &crate::core::diff::Block) -> String {
        let context: Vec<_> = self
            .panes
            .iter()
            .zip(&block.ranges)
            .map(|(p, r)| {
                let start = r.start.saturating_sub(2);
                let end = (r.end + 2).min(p.doc.lines.len());
                (&p.doc.lines[start..end], &p.doc.lines[r.clone()])
            })
            .collect();
        crate::core::analysis::identity(&(context, &self.opts))
    }
    fn hunk_review_ui(&mut self, ui: &mut Ui) {
        use crate::core::review_state::{Decision, State};
        if self.review_decision.is_none() {
            return;
        }
        {
            if !self.review_within_budget() {
                ui.label(tr("analysis.too_large"));
                return;
            }
            let Some(block) = self.cur_block.and_then(|i| self.diff.blocks.get(i)) else {
                ui.label(tr("workspace.select_hunk"));
                return;
            };
            let id = self.review_hunk_id(block);
            if let Some(review) = &mut self.review_decision {
                if review.current_hunk.as_deref() != Some(&id) {
                    review.current_hunk = Some(id.clone());
                    self.review_dirty = true;
                }
            }
            let ambiguous = self.diff.blocks.len() > 1000
                || self
                    .diff
                    .blocks
                    .iter()
                    .filter(|b| self.review_hunk_id(b) == id)
                    .take(2)
                    .count()
                    > 1;
            let mut decision = self
                .review_decision
                .as_ref()
                .unwrap()
                .hunks
                .get(&id)
                .map(|d| (**d).clone())
                .unwrap_or_default();
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                for state in State::ALL {
                    if ui
                        .selectable_label(decision.state == state, tr(state.key()))
                        .clicked()
                    {
                        decision.id = id.clone();
                        decision.fingerprint = id.clone();
                        decision.state = if ambiguous { State::NeedsAction } else { state };
                        decision.stale = ambiguous;
                        changed = true;
                    }
                }
            });
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut decision.comment)
                        .hint_text(tr("workspace.comment"))
                        .char_limit(4096)
                        .desired_rows(2),
                )
                .changed();
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut decision.rationale)
                        .hint_text(tr("workspace.rationale"))
                        .char_limit(4096),
                )
                .changed();
            if ambiguous || decision.stale {
                ui.colored_label(ui.visuals().warn_fg_color, tr("workspace.ambiguous"));
            }
            if changed {
                let file = self.review_decision.as_mut().unwrap();
                file.hunks.insert(id, Box::new(Decision { ..decision }));
                if file.state == State::Unreviewed {
                    file.state = State::InReview;
                }
                self.review_dirty = true;
            }
        }
    }

    /// Merge verification of a 3-way compare: keeps its background work going, and draws the
    /// panel when given a place for it.
    fn verification_ui(&mut self, ui: Option<&mut Ui>, cfg: &Config) {
        if self.merge_validation_generation != Some(self.layout_gen) {
            self.merge_validation = None;
        }
        use crate::core::{analysis, merge_validation as mv};
        if self
            .merge_preview_job
            .as_ref()
            .is_some_and(|(generation, _)| *generation != self.layout_gen)
        {
            self.merge_preview_job = None;
        }
        if let Some((_, task)) = &mut self.merge_preview_job {
            if let Some(result) = task.poll() {
                match result {
                    Ok(plan) => {
                        self.merge_preview = Some(Box::new(plan));
                        self.merge_freshness = None;
                        self.merge_validation = None;
                    }
                    Err(e) => self.status = e.to_string(),
                };
                self.merge_preview_job = None;
            }
        }

        if self.n() == 3 {
            if self
                .merge_freshness
                .is_none_or(|(generation, _)| generation != self.layout_gen)
            {
                let current = self.merge_preview.as_ref().is_some_and(|plan| {
                    plan.rules == self.opts
                        && [0, 2].into_iter().all(|side| {
                            plan.inputs.get(side).is_some_and(|(_, digest)| {
                                *digest == mv::doc_fingerprint(&self.panes[side].doc)
                            })
                        })
                });
                self.merge_freshness = Some((self.layout_gen, current));
            }
            let plan_current = self.merge_preview.is_some()
                && self.merge_freshness.is_some_and(|(_, current)| current);
            let Some(ui) = ui else { return };
            {
                ui.horizontal(|ui| {
                    if ui.button(tr("analysis.preview_merge")).clicked() {
                        self.merge_preview = None;
                        self.merge_validation = None;
                        let docs: Vec<_> = self.panes.iter().map(|p| p.doc.clone()).collect();
                        let cfg = cfg.clone();
                        let opts = self.opts.clone();
                        let sync = self.sync_points.clone();
                        self.merge_preview_job = Some((
                            self.layout_gen,
                            super::analysis::Task::start(move |cancel, _| {
                                analysis::check_cancel(cancel)?;
                                anyhow::ensure!(
                                    docs.iter()
                                        .all(|d| d.text().len() as u64 <= analysis::MAX_FILE),
                                    "Merge analysis is limited to 16 MiB per text input"
                                );
                                let mut view = Self::new(docs, &cfg);
                                view.opts = opts;
                                view.sync_points = sync;
                                let plan = view.preview_merge(&cfg)?;
                                analysis::check_cancel(cancel)?;
                                Ok(plan)
                            }),
                        ));
                    }
                    if ui
                        .add_enabled(
                            plan_current,
                            egui::Button::new(tr("analysis.validate_result")),
                        )
                        .clicked()
                    {
                        let plan = self.merge_preview.as_ref().unwrap();
                        if let Some(path) = &self.panes[1].doc.path {
                            match mv::validate_file(plan, path) {
                                Ok(report) => {
                                    self.merge_validation = Some(Box::new(report));
                                    self.merge_validation_generation = Some(self.layout_gen);
                                }
                                Err(e) => self.status = e.to_string(),
                            }
                        } else {
                            self.status = tr("analysis.save_result_first").into();
                        }
                    }
                    if ui
                        .add_enabled(
                            self.merge_preview.is_some(),
                            egui::Button::new(tr("analysis.export")),
                        )
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("merge-plan.json")
                            .save_file()
                        {
                            let inputs = self
                                .panes
                                .iter()
                                .filter_map(|p| p.doc.path.clone())
                                .collect::<Vec<_>>();
                            if let Some(report) = &self.merge_validation {
                                if let Err(e) = analysis::report(report, &path, &inputs) {
                                    self.status = e.to_string();
                                }
                            } else if let Some(plan) = &self.merge_preview {
                                if let Err(e) = analysis::report(plan, &path, &inputs) {
                                    self.status = e.to_string();
                                }
                            }
                        }
                    }
                });
                if let Some((_, task)) = &self.merge_preview_job {
                    task.status(ui);
                }
                ui.small(tr("analysis.mechanical_only"));
                if self.merge_preview.is_some() && !plan_current {
                    ui.colored_label(ui.visuals().warn_fg_color, tr("analysis.repreview"));
                }
                if let Some(plan) = &self.merge_preview {
                    ui.label(format!(
                        "{}: {}",
                        tr("analysis.planned_changes"),
                        plan.actions.len()
                    ));
                    if let Some(preview) = &plan.preview {
                        egui::CollapsingHeader::new(tr("analysis.preview")).show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(160.0)
                                .show(ui, |ui| {
                                    let excerpt = preview
                                        .lines()
                                        .take(200)
                                        .map(|line| line.chars().take(500).collect::<String>())
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    ui.monospace(excerpt);
                                    ui.small(tr("analysis.preview_limit"));
                                });
                        });
                    }
                    let status = self
                        .merge_validation
                        .as_ref()
                        .map_or(mv::ValidationStatus::NotRun, |r| r.status.clone());
                    ui.label(tr(match status {
                        mv::ValidationStatus::NotRun => "analysis.not_run",
                        mv::ValidationStatus::Passed => "analysis.passed",
                        mv::ValidationStatus::Warning => "analysis.warning",
                        mv::ValidationStatus::Failed => "analysis.failed",
                    }));
                    let findings = self
                        .merge_validation
                        .as_ref()
                        .map_or(&plan.findings, |r| &r.findings);
                    let mut navigate = None;
                    egui::ScrollArea::vertical()
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for finding in findings {
                                if ui
                                    .selectable_label(
                                        false,
                                        format!(
                                            "{:?} · {} · {}",
                                            finding.severity,
                                            if finding.mechanical {
                                                tr("analysis.mechanical")
                                            } else {
                                                tr("analysis.heuristic")
                                            },
                                            finding.message
                                        ),
                                    )
                                    .clicked()
                                {
                                    navigate = finding.row;
                                }
                            }
                        });
                    if let Some(row) = navigate {
                        self.cursor_row = Some(row);
                        self.scroll_to = Some(row);
                        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block());
                    }
                }
            }
        }
    }

    /// The side panels this compare has: hunk review for a reviewed file, merge verification
    /// for a 3-way compare.
    fn panel_tabs(&self) -> Vec<(TextPanel, &'static str)> {
        let mut tabs = Vec::new();
        if self.review_decision.is_some() {
            tabs.push((TextPanel::Review, tr("workspace.hunk_title")));
        }
        if self.n() == 3 {
            tabs.push((TextPanel::Verification, tr("analysis.merge_verification")));
        }
        tabs
    }

    /// The Text / Semantic switch of JSON, XML, TOML and YAML compares, and the semantic view.
    /// Returns true when the semantic view took the place of the text panes.
    fn analysis_ui(&mut self, ui: &mut Ui) -> bool {
        use crate::core::{analysis, semantic};
        self.tree_active = false;
        if !matches!(self.n(), 2 | 3) {
            return false;
        }
        let format = self.panes.first().and_then(|p| {
            let hint = p
                .doc
                .syntax
                .as_ref()
                .map(|extension| std::path::PathBuf::from(format!("source.{extension}")));
            let path = p.doc.path.as_deref().or(hint.as_deref());
            path.and_then(|path| {
                semantic::Format::of_extension(&text::extension(path))
                    .filter(|f| matches!(f, semantic::Format::Toml | semantic::Format::Yaml))
            })
            .or_else(|| {
                // Enough of the start to tell an HTML page from an XML document: the doctype
                // may follow an XML declaration, a comment or a blank line.
                let mut head = String::new();
                for line in p.doc.lines.iter().take(8) {
                    if head.len() >= 512 {
                        break;
                    }
                    head.push_str(line);
                    head.push('\n');
                }
                if crate::core::prettify::is_html(path, &head) {
                    // HTML is not one of the formats these views read.
                    return None;
                }
                crate::core::prettify::detect(path, &head).map(|f| match f {
                    crate::core::prettify::Format::Json => semantic::Format::Json,
                    crate::core::prettify::Format::Xml => semantic::Format::Xml,
                })
            })
        });
        let Some(format) = format else {
            return false;
        };
        let tree = self.n() == 2;
        self.tree_mode &= tree;
        ui.horizontal(|ui| {
            let mut mode = if self.tree_mode {
                2
            } else {
                self.semantic_mode as u8
            };
            ui.selectable_value(&mut mode, 0, tr("analysis.text"));
            ui.selectable_value(&mut mode, 1, tr("analysis.semantic"));
            if tree {
                ui.selectable_value(&mut mode, 2, tr("analysis.tree"))
                    .on_hover_text(tr(if format == semantic::Format::Xml {
                        "analysis.tree_tip"
                    } else {
                        "analysis.tree_tip_values"
                    }));
            }
            if mode == 2 && !self.tree_mode {
                // the Tree view opens where the text cursor is
                self.tree.cursor = self.cursor_row.and_then(|row| {
                    let line = self.diff.rows.get(row)?.line(self.active_pane.min(1))?;
                    Some((self.active_pane.min(1), line + 1))
                });
            }
            self.semantic_mode = mode == 1;
            self.tree_mode = mode == 2;
        });
        if self.tree_mode {
            self.tree_ui(ui, format);
            return true;
        }
        if !self.semantic_mode {
            return false;
        }
        // While the array key is being typed, the report keeps the key it was made with. The
        // field stays shown while a report is made, so typing in it goes on.
        let typed_id = self.id.with("array_key_typed");
        if format.keyed() {
            ui.horizontal(|ui| {
                let label = tr("analysis.array_key");
                ui.label(label);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.semantic_array_key)
                        .hint_text(tr("analysis.array_key_hint"))
                        .char_limit(256)
                        .desired_width(160.0),
                );
                let key = self.semantic_array_key.as_str();
                response.widget_info(|| {
                    let mut info = egui::WidgetInfo::text_edit(true, key, key, "");
                    info.label = Some(label.into());
                    info
                });
                let now = ui.input(|i| i.time);
                if response.changed() {
                    ui.data_mut(|d| d.insert_temp(typed_id, now));
                }
                if let Some(typed) = ui.data(|d| d.get_temp::<f64>(typed_id)) {
                    const PAUSE: f64 = 0.3;
                    let waited = now - typed;
                    if waited >= PAUSE || response.lost_focus() {
                        ui.data_mut(|d| d.remove::<f64>(typed_id));
                        ui.ctx().request_repaint();
                    } else {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs_f64(
                                PAUSE - waited,
                            ));
                    }
                }
            });
        }
        let typing = ui.data(|d| d.get_temp::<f64>(typed_id)).is_some();
        let options = match self.semantic_cache.as_ref().filter(|_| typing) {
            Some((_, report)) => report.options().clone(),
            None => semantic::Options {
                array_key: Some(self.semantic_array_key.trim().to_owned())
                    .filter(|k| !k.is_empty()),
            },
        };
        if self
            .semantic_cache
            .as_ref()
            .is_none_or(|(generation, report)| {
                *generation != self.layout_gen || report.options() != &options
            })
            && self
                .semantic_job
                .as_ref()
                .is_none_or(|(generation, _)| *generation != self.layout_gen)
        {
            let docs: Vec<_> = self.panes.iter().map(|p| p.doc.text()).collect();
            let job_options = options.clone();
            self.semantic_job = Some((
                self.layout_gen,
                super::analysis::Task::start(move |cancel, _| {
                    analysis::check_cancel(cancel)?;
                    let report = match &docs[..] {
                        [a, b] => {
                            Structured::Two(semantic::compare_with(a, b, format, &job_options))
                        }
                        [left, base, right] => {
                            Structured::Three(semantic::three_way::compare_three(
                                left,
                                base,
                                right,
                                format,
                                &job_options,
                            ))
                        }
                        _ => anyhow::bail!("Semantic comparison takes two or three files"),
                    };
                    analysis::check_cancel(cancel)?;
                    Ok(report)
                }),
            ));
        }
        if let Some((generation, task)) = &mut self.semantic_job {
            if let Some(result) = task.poll() {
                let report = result.unwrap_or_else(|e| {
                    self.status = e.to_string();
                    Structured::Two(semantic::SemanticReport {
                        version: 1,
                        format,
                        changes: Vec::new(),
                        errors: vec![e.to_string()],
                        policies: Vec::new(),
                        options: options.clone(),
                        notes: Vec::new(),
                    })
                });
                // a page of the same report stays shown after a value is copied
                if self
                    .semantic_cache
                    .as_ref()
                    .is_none_or(|(_, old)| old.options() != report.options())
                {
                    self.semantic_page = 0;
                }
                self.semantic_cache = Some((*generation, report));
                self.semantic_job = None;
            }
        }
        if let Some((_, task)) = &self.semantic_job {
            task.status(ui);
            return true;
        }
        let Some((generation, report)) = self.semantic_cache.take() else {
            ui.label(&self.status);
            return true;
        };
        ui.small(tr("analysis.semantic_policy"));
        if let Some(message) = &self.semantic_message {
            ui.colored_label(widgets::pal(ui).warning, message);
        }
        let mut action = None;
        match &report {
            Structured::Two(r) => self.semantic_two_ui(ui, r, &mut action),
            Structured::Three(r) => self.semantic_three_ui(ui, r, &mut action),
        }
        self.semantic_cache = Some((generation, report));
        match action {
            Some(SemanticAction::Jump(side, line)) => {
                self.show_line(side, line);
                self.semantic_mode = false;
            }
            Some(SemanticAction::Copy(change, from)) => {
                let to = 1 - from;
                if self.writable(to) {
                    let docs = [self.panes[0].doc.text(), self.panes[1].doc.text()];
                    let copied =
                        semantic::write::copy_change(&change, [&docs[0], &docs[1]], format, from);
                    self.semantic_copied(to, copied);
                }
            }
            Some(SemanticAction::Take(entry, side)) if self.writable(1) => {
                let docs: Vec<_> = self.panes.iter().map(|p| p.doc.text()).collect();
                let taken =
                    semantic::three_way::take(&entry, side, [&docs[0], &docs[1], &docs[2]], format);
                self.semantic_copied(1, taken);
            }
            Some(SemanticAction::Take(..)) | None => {}
        }
        true
    }

    /// Moves the cursor of the text panes to 0-based `line` of pane `side`.
    fn show_line(&mut self, side: usize, line: usize) {
        self.active_pane = side;
        self.cursor_row = self
            .diff
            .rows
            .iter()
            .position(|r| r.line(side) == Some(line));
        self.scroll_to = self.cursor_row;
        self.sel_anchor = None;
        self.cur_block = self
            .cursor_row
            .and_then(|row| self.diff.rows[row].block())
            .filter(|&b| !self.diff.blocks[b].ignored);
    }

    /// The Word documents or workbooks the panes show, when every pane shows one of the same
    /// kind read from its file.
    fn documents(&self) -> Option<Vec<std::path::PathBuf>> {
        if !matches!(self.n(), 2 | 3) {
            return None;
        }
        self.panes
            .iter()
            .map(|p| {
                p.doc
                    .path
                    .clone()
                    .filter(|path| p.doc.read_only && formatted_kind(path).is_some())
            })
            .collect::<Option<Vec<_>>>()
            .filter(|paths| {
                let kind = formatted_kind(&paths[0]);
                paths.iter().all(|p| formatted_kind(p) == kind)
            })
    }

    /// The Text / Formatted switch of Word document, workbook and PDF compares, and the
    /// Formatted view: the files as Markdown, block by block side by side, with their tables
    /// drawn as tables, merged cells included; a workbook is a heading and a table for each sheet,
    /// a PDF is read from the structure it is tagged with. Made in the background and kept until
    /// a file or the compare options change. Returns true when the Formatted view took the place
    /// of the text panes.
    fn formatted_ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) -> bool {
        self.formatted_active = false;
        let Some(paths) = self.documents() else {
            return false;
        };
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.formatted_mode, false, tr("analysis.text"));
            ui.selectable_value(&mut self.formatted_mode, true, tr("analysis.formatted"))
                .on_hover_text(tr("analysis.formatted_tip"));
        });
        if !self.formatted_mode {
            return false;
        }
        self.formatted_active = true;
        let key = FormattedKey::of(&paths, &self.opts);
        let made = self.formatted.view.as_ref().is_some_and(|(k, _)| *k == key)
            || self.formatted.job.as_ref().is_some_and(|(k, _)| *k == key);
        if !made {
            let opts = self.opts.clone();
            self.formatted.job = Some((
                key,
                super::analysis::Task::start(move |cancel, _| {
                    let pages = paths
                        .iter()
                        .map(|p| match formatted_kind(p) {
                            Some(FormattedKind::Workbook) => {
                                crate::core::extract::xlsx_blocks(p, Some(cancel))
                            }
                            Some(FormattedKind::Pdf) => {
                                crate::core::extract::pdf_blocks(p, Some(cancel))
                            }
                            _ => crate::core::extract::docx_blocks(p, Some(cancel)),
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    Ok(Prepared::new(pages, &opts))
                }),
            ));
        }
        if let Some((key, task)) = &mut self.formatted.job {
            if let Some(result) = task.poll() {
                let labels = self.panes.iter().map(|p| p.doc.title()).collect();
                let view = result
                    .map(|prepared| MarkdownView::from_prepared(prepared, labels))
                    .map_err(|e| format!("{e:#}"));
                self.formatted.view = Some((key.clone(), view));
                self.formatted.job = None;
            }
        }
        if let Some((_, task)) = &self.formatted.job {
            task.status(ui);
            // the last view stays shown while the next is made
            if !matches!(self.formatted.view, Some((_, Ok(_)))) {
                return true;
            }
        }
        match &mut self.formatted.view {
            Some((_, Ok(view))) => {
                view.ui(ui, c, cfg.show_location_pane);
                if let Some(forward) = view.step_file.take() {
                    self.step_file = Some(forward);
                }
            }
            Some((_, Err(error))) => {
                ui.colored_label(widgets::pal(ui).danger, error.as_str());
                if ui.button(tr("analysis.text_fallback")).clicked() {
                    self.formatted_mode = false;
                }
            }
            None => {}
        }
        true
    }

    /// The Tree view of two XML, JSON, YAML or TOML files, made in the background and kept
    /// until the files or the key rules change.
    fn tree_ui(&mut self, ui: &mut Ui, format: crate::core::semantic::Format) {
        use super::xml_tree_view::{TreeAction, TreeResult};
        use crate::core::semantic::xml_tree::{TreeDiff, TreeOptions};
        self.tree_active = true;
        let values = format != crate::core::semantic::Format::Xml;
        self.tree.view.values = values;
        ui.small(tr(super::xml_tree_view::policy_key(values)));
        let keys = self.tree.view.applied_keys.trim().to_owned();
        let options = TreeOptions::parse(&keys);
        // a comparison of one format is not kept for another
        let keys = format!("{}\u{1f}{keys}", format.label());
        let want = (self.layout_gen, keys.clone());
        let made =
            |entry: Option<(u64, &String)>| entry.is_some_and(|(g, k)| (g, k.clone()) == want);
        if let Ok(options) = &options {
            if !made(self.tree.cache.as_ref().map(|c| (c.0, &c.1)))
                && !made(self.tree.job.as_ref().map(|j| (j.0, &j.1)))
            {
                let docs = [self.panes[0].doc.text(), self.panes[1].doc.text()];
                let options = options.clone();
                self.tree.job = Some((
                    self.layout_gen,
                    keys.clone(),
                    super::analysis::Task::start(move |cancel, _| {
                        let [a, b] = docs;
                        let diff = TreeDiff::compare_as(&a, &b, format, &options, cancel)?;
                        Ok(TreeResult {
                            diff,
                            sources: [a, b],
                            format,
                        })
                    }),
                ));
            }
        }
        if let Some((generation, keys, task)) = &mut self.tree.job {
            if let Some(result) = task.poll() {
                self.tree.cache = Some((
                    *generation,
                    std::mem::take(keys),
                    result.map(Box::new).map_err(|e| format!("{e:#}")),
                ));
                self.tree.job = None;
            }
        }
        if let Err(e) = &options {
            ui.colored_label(widgets::pal(ui).danger, e.to_string());
        }
        if let Some((_, _, task)) = &self.tree.job {
            task.status(ui);
            // the last comparison stays shown, so typing a key rule goes on
            if !matches!(self.tree.cache, Some((_, _, Ok(_)))) {
                ui.horizontal(|ui| self.tree.view.keys_ui(ui));
                return;
            }
        }
        let Some((generation, keys, result)) = self.tree.cache.take() else {
            return;
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                ui.colored_label(widgets::pal(ui).danger, &error);
                let mut retry = false;
                ui.horizontal(|ui| {
                    if ui.button(tr("analysis.text_fallback")).clicked() {
                        self.tree_mode = false;
                    }
                    retry = ui.button(tr("analysis.tree_retry")).clicked();
                });
                if !retry {
                    self.tree.cache = Some((generation, keys, Err(error)));
                }
                return;
            }
        };
        if let Some((side, line)) = self.tree.cursor.take() {
            self.tree.view.attach(&result.diff);
            if let Some(row) = result.diff.row_at_line(side, line) {
                self.tree.view.select(&result.diff, row);
            }
        }
        let titles = [self.panes[0].doc.title(), self.panes[1].doc.title()];
        let action = self
            .tree
            .view
            .ui(ui, &result, [titles[0].as_str(), titles[1].as_str()]);
        self.tree.cache = Some((generation, keys, Ok(result)));
        match action {
            Some(TreeAction::Jump(side, line)) => {
                self.show_line(side, line.saturating_sub(1));
                self.tree_mode = false;
                self.tree_active = false;
            }
            None => {}
        }
    }

    /// Puts the text a semantic copy made into pane `to`, or says why there is none.
    fn semantic_copied(
        &mut self,
        to: usize,
        result: Result<String, crate::core::semantic::write::Refusal>,
    ) {
        use crate::core::semantic::write::Refusal;
        match result {
            Ok(text) => {
                self.semantic_message = None;
                self.replace_source(to, text);
            }
            Err(refusal) => {
                self.semantic_message = Some(
                    tr(match refusal {
                        Refusal::Moved => "analysis.copy_moved",
                        Refusal::Stale => "analysis.copy_stale",
                        Refusal::NoEdit => "analysis.copy_no_edit",
                    })
                    .to_owned(),
                )
            }
        }
    }

    /// Errors, notes, filters and the paged tree of a semantic report of two files.
    fn semantic_two_ui(
        &mut self,
        ui: &mut Ui,
        report: &crate::core::semantic::SemanticReport,
        action: &mut Option<SemanticAction>,
    ) {
        use crate::core::semantic;
        if !report.errors.is_empty() {
            for error in &report.errors {
                ui.colored_label(widgets::pal(ui).danger, error);
            }
            if ui.button(tr("analysis.text_fallback")).clicked() {
                self.semantic_mode = false;
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.semantic_filter, None, tr("analysis.all"));
            for (kind, key) in [
                (semantic::ChangeKind::Added, "analysis.added"),
                (semantic::ChangeKind::Removed, "analysis.removed"),
                (semantic::ChangeKind::Modified, "analysis.modified"),
                (semantic::ChangeKind::TypeChanged, "analysis.type_changed"),
                (semantic::ChangeKind::Moved, "analysis.moved"),
            ] {
                ui.selectable_value(&mut self.semantic_filter, Some(kind), tr(key));
            }
            if ui.button(tr("analysis.export")).clicked() {
                self.export_semantic(report);
            }
        });
        for note in &report.notes {
            ui.colored_label(widgets::pal(ui).warning, note);
        }
        if report.identical() {
            ui.label(tr("analysis.semantic_identical"));
        }
        let filter = self.semantic_filter.clone();
        let shown: Vec<_> = report
            .changes
            .iter()
            .filter(|change| filter.as_ref().is_none_or(|f| f == &change.kind))
            .collect();
        let page = self.semantic_pager(ui, shown.len());
        let tree = semantic::PathTree::of(page.map(|i| shown[i]));
        let writable = [0, 1].map(|p| !self.panes[p].doc.read_only);
        egui::ScrollArea::vertical().show(ui, |ui| {
            semantic_tree_ui(ui, &tree, 0, writable, action);
        });
    }

    /// Errors, notes, filters and the paged entries of a semantic report of three files.
    fn semantic_three_ui(
        &mut self,
        ui: &mut Ui,
        report: &crate::core::semantic::three_way::ThreeWayReport,
        action: &mut Option<SemanticAction>,
    ) {
        use crate::core::semantic::three_way::MergeState;
        if !report.errors.is_empty() {
            for error in &report.errors {
                ui.colored_label(widgets::pal(ui).danger, error);
            }
            if ui.button(tr("analysis.text_fallback")).clicked() {
                self.semantic_mode = false;
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.semantic_state_filter, None, tr("analysis.all"));
            for (state, key) in [
                (MergeState::Conflict, "analysis.state_conflict"),
                (MergeState::Left, "analysis.state_left"),
                (MergeState::Right, "analysis.state_right"),
                (MergeState::Both, "analysis.state_both"),
            ] {
                ui.selectable_value(&mut self.semantic_state_filter, Some(state), tr(key));
            }
            if ui.button(tr("analysis.export")).clicked() {
                self.export_semantic(report);
            }
        });
        for note in &report.notes {
            ui.colored_label(widgets::pal(ui).warning, note);
        }
        if report.identical() {
            ui.label(tr("analysis.semantic_identical"));
        } else {
            ui.label(trf(
                "analysis.three_way_summary",
                &[
                    ("n", report.entries.len().to_string()),
                    ("conflicts", report.conflicts().to_string()),
                ],
            ));
        }
        let filter = self.semantic_state_filter;
        let shown: Vec<_> = report
            .entries
            .iter()
            .filter(|e| filter.is_none_or(|f| f == e.state))
            .collect();
        let page = self.semantic_pager(ui, shown.len());
        let entries = &shown[page];
        let writable = !self.panes[1].doc.read_only;
        egui::ScrollArea::vertical().show(ui, |ui| {
            three_way_ui(ui, entries, writable, action);
        });
    }

    /// Page buttons for `count` items, 100 a page; the range of the page shown.
    fn semantic_pager(&mut self, ui: &mut Ui, count: usize) -> std::ops::Range<usize> {
        const PAGE: usize = 100;
        let pages = count.div_ceil(PAGE).max(1);
        self.semantic_page = self.semantic_page.min(pages - 1);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.semantic_page > 0, egui::Button::new("←"))
                .clicked()
            {
                self.semantic_page -= 1;
            }
            ui.label(format!(
                "{} {}/{} · {}",
                tr("analysis.page"),
                self.semantic_page + 1,
                pages,
                count
            ));
            if ui
                .add_enabled(self.semantic_page + 1 < pages, egui::Button::new("→"))
                .clicked()
            {
                self.semantic_page += 1;
            }
            ui.small(tr("analysis.value_limit"));
        });
        let start = self.semantic_page * PAGE;
        start..(start + PAGE).min(count)
    }

    fn export_semantic(&mut self, report: &impl serde::Serialize) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("semantic.json")
            .save_file()
        {
            let inputs = self
                .panes
                .iter()
                .filter_map(|p| p.doc.path.clone())
                .collect::<Vec<_>>();
            if let Err(e) = crate::core::analysis::report(report, &path, &inputs) {
                self.status = e.to_string();
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.settle_diff_for_input(ui.ctx());
        if self.text_layout != cfg.text_layout {
            self.text_layout = cfg.text_layout;
            self.scroll_anim = None;
        }
        self.poll_highlight(cfg);
        if self.edit_markers != cfg.show_edit_markers {
            self.edit_markers = cfg.show_edit_markers;
            self.update_edited();
        }
        if self.panes.iter().any(|p| p.hl_rx.is_some()) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.toolbar(ui);
        let tabs = self.panel_tabs();
        if self.drawer_open && !tabs.is_empty() {
            let mut tab = self.drawer_tab;
            widgets::drawer(ui, self.id.with("drawer"), &mut tab, &tabs, |ui, tab| {
                let verify = tab == TextPanel::Verification;
                if tab == TextPanel::Review {
                    self.hunk_review_ui(ui);
                }
                self.verification_ui(verify.then_some(ui), cfg);
            });
            self.drawer_tab = tab;
        } else {
            self.verification_ui(None, cfg);
        }
        if self.formatted_ui(ui, cfg, c) || self.analysis_ui(ui) {
            return;
        }
        if self.outcome() == Outcome::Identical {
            let color = widgets::pal(ui).success;
            widgets::info_bar(ui, Icon::Check, tr("msg.identical"), color);
        }
        if self.n() == 3
            && !self.table_mode
            && (self.significant().next().is_some() || self.panes[1].doc.modified)
        {
            self.merge_bar(ui);
        }
        if !self.disk_changed.is_empty() {
            self.disk_changed_bar(ui);
        }
        if self.find.open {
            self.find_bar(ui);
        }

        if self.table_mode && self.table_delim.is_some() {
            self.page_rows = (ui.available_height() / (cfg.font_size + 14.0)).max(1.0) as usize;
            self.handle_table_keys(ui);
            self.table_ui(ui, cfg, c);
            self.note_window(ui);
            self.goto_window(ui);
            return;
        }
        let unified = self.unified && self.n() == 2;
        if unified != self.shown_unified {
            // Each view keeps its own scroll position: show the cursor in the one switched to.
            self.shown_unified = unified;
            self.scroll_to = self.scroll_to.or(self.cursor_row);
        }
        if unified {
            self.unified_ui(ui, cfg, c);
            self.goto_window(ui);
            return;
        }
        let n = self.n();
        self.tab_size = cfg.tab_size;
        let full = ui.available_rect_before_wrap();
        let detail_h = if cfg.show_line_detail {
            let row = ui.fonts_mut(|f| f.row_height(&FontId::monospace(cfg.font_size)));
            (n as f32 * (2.0 * row + 6.0) + 6.0).min(full.height() / 2.0)
        } else {
            0.0
        };
        let mut avail = full;
        avail.max.y -= detail_h;
        let header_h = 36.0;
        let footer_h = 28.0;
        let wrap = self.wrap && !self.table_mode;
        let hbar_h = if wrap { 0.0 } else { 12.0 };
        let loc_w = if cfg.show_location_pane {
            14.0 + 8.0 * n as f32
        } else {
            0.0
        };
        let vbar_w = 12.0;
        let content_left = avail.left();
        let content_right = avail.right() - vbar_w - loc_w;
        let link_w = if n == 2 { 56.0 } else { 44.0 };
        let pane_w = ((content_right - content_left - link_w * (n - 1) as f32) / n as f32).max(1.0);
        let pane_step = pane_w + link_w;
        let pane_at = |x: f32| (((x - content_left) / pane_step).floor() as usize).min(n - 1);
        let body_top = avail.top() + header_h;
        let body_bottom = (avail.bottom() - footer_h - hbar_h).max(body_top + 20.0);
        let font = FontId::monospace(cfg.font_size);
        let row_h = ui.fonts_mut(|f| f.row_height(&font)) + theme::row_padding();
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
        let body_h = body_bottom - body_top;
        let visible = body_h / row_h;
        self.page_rows = (visible as usize).saturating_sub(1).max(1);

        // headers
        for p in 0..n {
            let r = Rect::from_min_size(
                pos2(content_left + p as f32 * pane_step, avail.top()),
                vec2(pane_w, header_h),
            );
            self.pane_header(ui, p, r);
            let footer = Rect::from_min_size(
                pos2(
                    content_left + p as f32 * pane_step,
                    avail.bottom() - footer_h,
                ),
                vec2(pane_w, footer_h),
            );
            self.pane_footer(ui, p, footer);
        }

        let body_rect = Rect::from_min_max(
            pos2(content_left, body_top),
            pos2(content_right, body_bottom),
        );
        let resp = ui.interact(body_rect, self.id.with("body"), Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Document, true, tr("help.text_compare"))
        });

        let digits = self
            .panes
            .iter()
            .map(|p| p.doc.lines.len())
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(3);
        let gutter_w = if cfg.show_line_numbers {
            (digits as f32 + 1.0) * char_w + 6.0
        } else {
            6.0
        };
        let text_w = pane_w - gutter_w - 2.0;
        let tab = cfg.tab_size;
        let wrap_cols = if wrap {
            (((text_w - 8.0) / char_w).floor() as usize).max(8)
        } else {
            0
        };
        let fresh = self.layout.key.is_none();
        self.ensure_layout(wrap_cols, tab);
        self.handle_keys(ui);
        // Keys may have edited the text; the rows drawn below are those of the new compare.
        self.ensure_layout(wrap_cols, tab);

        // scrolling
        let now = ui.input(|i| i.time);
        if ui.rect_contains_pointer(body_rect) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d != egui::Vec2::ZERO {
                self.scroll_anim = None;
                self.scroll -= d.y / row_h;
                self.hscroll -= d.x;
            }
        }
        if let Some(row) = self.scroll_to.take() {
            self.reveal_row(row, wrap_cols, tab);
            if let Some(&v) = self.layout.row_line.get(row) {
                // A caret further down a wrapped line: its own display line.
                let seg = self
                    .caret()
                    .filter(|c| self.caret_reveal && self.diff.row_of_line(c.pane, c.pos.0) == row)
                    .map_or(0, |c| self.caret_segment(&c));
                let span = self
                    .layout
                    .row_line
                    .get(row + 1)
                    .map_or(self.layout.lines.len(), |&n| n)
                    .saturating_sub(v)
                    .max(1);
                let v = (v + seg.min(span - 1)) as f32;
                let (top, bottom) = (self.scroll, self.scroll + visible - 2.0);
                // A caret that moved or typed just past an edge scrolls only as far as needed;
                // a jump further away puts its line a third of the way down.
                let near = self.caret_reveal && v > top - visible && v < bottom + visible;
                if near && (v < top || v > bottom) {
                    self.scroll_anim = None;
                    self.scroll = if v < top { v } else { v - visible + 2.0 };
                } else if v < top || v > bottom {
                    let target = v - (visible / 3.0).floor();
                    if fresh || ui.style().animation_time <= 0.0 {
                        self.scroll = target;
                    } else {
                        self.scroll_anim = Some((self.scroll, target, now));
                    }
                }
            }
        }
        let total = self.layout.lines.len();
        let max_scroll = (total as f32 - visible + 1.0).max(0.0);
        if let Some((from, to, start)) = self.scroll_anim {
            let t = ((now - start) / SCROLL_ANIM_SECS).clamp(0.0, 1.0) as f32;
            let ease = 1.0 - (1.0 - t).powi(3);
            self.scroll = from + (to.clamp(0.0, max_scroll) - from) * ease;
            if t >= 1.0 {
                self.scroll_anim = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let content_w = if self.table_mode {
            self.col_widths
                .iter()
                .map(|w| (*w as f32 + 1.0) * char_w + 8.0)
                .sum::<f32>()
                + 20.0
        } else {
            self.max_cols as f32 * char_w + 40.0
        };
        self.hscroll = if wrap {
            0.0
        } else {
            self.hscroll.clamp(0.0, (content_w - text_w).max(0.0))
        };

        let pane_scroll: Vec<f32> = self
            .layout
            .panes
            .iter()
            .map(|p| p.position(self.scroll))
            .collect();
        let pane_shown: Vec<(usize, Vec<VLine>)> = self
            .layout
            .panes
            .iter()
            .zip(&pane_scroll)
            .map(|(p, &scroll)| {
                let start = (scroll.floor() as usize).min(p.shared.len());
                let end = (start + visible.ceil() as usize + 1).min(p.shared.len());
                (
                    start,
                    p.shared[start..end]
                        .iter()
                        .map(|&v| self.layout.lines[v])
                        .collect(),
                )
            })
            .collect();
        for l in pane_shown.iter().flat_map(|(_, lines)| lines) {
            if let VLine::Row(r, _) = *l {
                self.ensure_inline(r);
            }
        }
        let pane_shared: Vec<Vec<usize>> = self
            .layout
            .panes
            .iter()
            .map(|p| {
                let start = p.position(self.scroll).floor() as usize;
                p.shared
                    .iter()
                    .skip(start)
                    .take(visible.ceil() as usize + 2)
                    .copied()
                    .collect()
            })
            .collect();
        let line_at = |p: usize, y: f32| {
            let v = (pane_scroll[p].fract() + (y - body_top) / row_h).floor();
            (v >= 0.0)
                .then(|| pane_shared[p].get(v as usize).copied())
                .flatten()
        };
        let hover_v = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| body_rect.contains(*p))
            .and_then(|p| line_at(pane_at(p.x), p.y));
        let hover_block = hover_v
            .and_then(|v| match self.layout.lines[v] {
                VLine::Row(r, _) => self.diff.rows[r].block(),
                VLine::Fold(..) => None,
            })
            .filter(|&b| !self.diff.blocks[b].ignored);
        let hover_fold = hover_v.filter(|&v| matches!(self.layout.lines[v], VLine::Fold(..)));
        if hover_fold.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if let Some(pos) = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| body_rect.contains(*p))
        {
            let p = pane_at(pos.x);
            let in_gutter = pos.x - (content_left + p as f32 * pane_step) < gutter_w;
            let note = match hover_v.map(|v| self.layout.lines[v]) {
                Some(VLine::Row(r, _)) if in_gutter => self.diff.rows[r]
                    .line(p)
                    .and_then(|l| self.bookmarks[p].get(&l))
                    .filter(|n| !n.is_empty())
                    .cloned(),
                _ => None,
            };
            if let Some(note) = note {
                resp.clone().on_hover_text_at_pointer(note);
            }
        }

        // Where a point is in the text of pane `p`: the line and byte the caret goes to.
        let show_ws = cfg.show_whitespace;
        let caret_hit = |this: &Self, p: usize, v: usize, x: f32| -> Option<edit::Pos> {
            let VLine::Row(r, seg) = *this.layout.lines.get(v)? else {
                return None;
            };
            let Some(l) = this.diff.rows[r].line(p) else {
                // A gap row: the start of the line that comes after it.
                return Some((this.insert_index(p, r), 0));
            };
            let line = &this.panes[p].doc.lines[l];
            let left = content_left + p as f32 * pane_step + gutter_w + 4.0;
            let job = Self::line_job(
                line,
                None,
                &[],
                &[],
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                Color32::WHITE,
                &font,
                tab,
                show_ws,
            );
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            // With wrapping, the x where the display line of the point ends, if another follows.
            let (gx, end) = if wrap {
                let xs = segment_x(&galley, &wrap_starts(line, tab, wrap_cols));
                let seg = seg as usize;
                let x0 = xs.get(seg).copied().unwrap_or(0.0);
                (x0 + (x - left), xs.get(seg + 1).copied())
            } else {
                (x - left + this.hscroll, None)
            };
            let mut cursor = galley.cursor_from_pos(vec2(gx.max(0.0), 1.0));
            // Past the end of a wrapped display line, the caret stays on it, before the
            // character the next display line starts with.
            if let Some(end) = end {
                let at_next = |c| galley.pos_from_cursor(c).min.x >= end - 0.5;
                while cursor.index.0 > 0 && at_next(cursor) {
                    cursor.index.0 -= 1;
                }
            }
            let col = cursor.index.0;
            Some((l, edit::byte_at(line, col, tab).0))
        };

        // A caret the pointer puts down is already where the pointer is, so it does not scroll
        // to it: a click or drag on the last lines would otherwise jump the view. Dragging past
        // an edge scrolls by itself, below.
        let scroll_asked = self.scroll_to;

        // clicks
        if resp.clicked() || resp.double_clicked() || resp.secondary_clicked() {
            if let Some(pos) = resp
                .interact_pointer_pos()
                .filter(|pos| pos.x < content_left + pane_at(pos.x) as f32 * pane_step + pane_w)
            {
                let p = pane_at(pos.x);
                match line_at(p, pos.y).map(|v| self.layout.lines[v]) {
                    Some(VLine::Row(r, _)) => {
                        let hit = line_at(p, pos.y).and_then(|v| caret_hit(self, p, v, pos.x));
                        // In the text caret's selection, or in the selected rows without one.
                        let in_sel = match self.caret().filter(|c| c.has_selection()) {
                            Some(c) => {
                                let (a, b) = c.range();
                                c.pane == p && hit.is_some_and(|at| a <= at && at <= b)
                            }
                            None => self.selection().is_some_and(|(a, b)| (a..=b).contains(&r)),
                        };
                        // Right-clicking the selection keeps it for the context menu.
                        if !(resp.secondary_clicked() && in_sel && p == self.active_pane) {
                            self.col_sel = None;
                            self.sel_anchor = if ui.input(|i| i.modifiers.shift) {
                                self.sel_anchor.or(self.cursor_row)
                            } else {
                                None
                            };
                            self.active_pane = p;
                            self.cursor_row = Some(r);
                            if let Some(b) = self.diff.rows[r].block() {
                                if !self.diff.blocks[b].ignored {
                                    self.cur_block = Some(b);
                                }
                            }
                            if let Some(at) = hit.filter(|_| !self.table_mode) {
                                // The caret goes where the text was clicked (a right-click
                                // outside the selection too, for the context menu); a
                                // double-click selects the word there, and a triple-click the
                                // line with its line break.
                                let shift = ui.input(|i| i.modifiers.shift);
                                // Ctrl+click (⌘-click on macOS) adds a caret, or removes one;
                                // with a double- or triple-click, the new one selects.
                                let add = ui.input(|i| i.modifiers.command)
                                    && !shift
                                    && !resp.secondary_clicked()
                                    && self.caret().is_some_and(|c| c.pane == p);
                                let others: Vec<Caret> = if add {
                                    self.carets().into_iter().skip(1).collect()
                                } else {
                                    Vec::new()
                                };
                                if add && !resp.double_clicked() && !resp.triple_clicked() {
                                    self.toggle_caret(p, at);
                                } else if resp.triple_clicked() {
                                    let lines = &self.panes[p].doc.lines;
                                    let end = if at.0 + 1 < lines.len() {
                                        (at.0 + 1, 0)
                                    } else {
                                        (at.0, lines.get(at.0).map_or(0, String::len))
                                    };
                                    self.place_caret(p, (at.0, 0), false);
                                    self.place_caret(p, end, true);
                                } else if resp.double_clicked() {
                                    let line = &self.panes[p].doc.lines;
                                    let w = line.get(at.0).map_or(0..0, |t| edit::word_at(t, at.1));
                                    self.place_caret(p, (at.0, w.start), false);
                                    self.place_caret(p, (at.0, w.end), true);
                                } else {
                                    self.place_caret(p, at, shift && !resp.secondary_clicked());
                                }
                                if add && (resp.double_clicked() || resp.triple_clicked()) {
                                    if let Some(primary) = self.caret() {
                                        let list = std::iter::once((primary, true))
                                            .chain(others.into_iter().map(|c| (c, false)))
                                            .collect();
                                        self.set_carets(list);
                                    }
                                }
                            } else if resp.double_clicked() {
                                self.begin_edit(r, p);
                            }
                        }
                        if resp.secondary_clicked() {
                            self.ctx_target = Some((r, p));
                        }
                    }
                    Some(VLine::Fold(s, _)) if resp.clicked() => {
                        self.expanded.insert(s);
                        self.layout_gen += 1;
                    }
                    _ => {}
                }
            }
        }

        // dragging selects rows of the pane where the drag started; Alt+dragging selects columns
        let hscroll = self.hscroll;
        let col_galley = |ui: &Ui, line: &str| {
            let job = Self::line_job(
                line,
                None,
                &[],
                &[],
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                Color32::WHITE,
                &font,
                tab,
                show_ws,
            );
            ui.fonts_mut(|f| f.layout_job(job))
        };
        // Columns are characters as laid out, so a wide character is one column however wide
        // it is drawn; past the end of a line they are `char_w` wide.
        let col_at_x = |this: &Self, p: usize, r: usize, x: f32| {
            let left = content_left + p as f32 * pane_step + gutter_w + 4.0;
            let gx = (x - left + hscroll).max(0.0);
            let cells = |w: f32| (w / char_w.max(1.0)).round().max(0.0) as usize;
            let Some(line) = this.diff.rows[r]
                .line(p)
                .map(|l| &this.panes[p].doc.lines[l])
            else {
                return cells(gx);
            };
            let galley = col_galley(ui, line);
            if gx > galley.rect.width() {
                galley.text().chars().count() + cells(gx - galley.rect.width())
            } else {
                galley.cursor_from_pos(vec2(gx, 1.0)).index.0
            }
        };
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if resp.drag_started() {
                    let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                    self.active_pane = pane_at(origin.x);
                    self.col_sel = None;
                    let columns = ui.input(|i| i.modifiers.alt) && !wrap && !self.table_mode;
                    if let Some(VLine::Row(r, _)) = line_at(self.active_pane, origin.y)
                        .map(|v| self.layout.lines[v])
                        .filter(|_| columns)
                    {
                        let at = (r, col_at_x(self, self.active_pane, r, origin.x));
                        self.drop_carets();
                        self.col_sel = Some(Box::new(ColSel {
                            pane: self.active_pane,
                            anchor: at,
                            head: at,
                        }));
                    }
                    let at = line_at(self.active_pane, origin.y)
                        .and_then(|v| caret_hit(self, self.active_pane, v, origin.x));
                    if let Some(at) = at.filter(|_| self.col_sel.is_none() && !self.table_mode) {
                        let shift = ui.input(|i| i.modifiers.shift);
                        self.place_caret(self.active_pane, at, shift);
                    } else if self.col_sel.is_none() && !ui.input(|i| i.modifiers.shift) {
                        self.sel_anchor = match line_at(self.active_pane, origin.y)
                            .map(|v| self.layout.lines[v])
                        {
                            Some(VLine::Row(r, _)) => Some(r),
                            _ => None,
                        };
                    }
                }
                let edge = if pos.y < body_top {
                    pos.y - body_top
                } else {
                    (pos.y - body_bottom).max(0.0)
                };
                if edge != 0.0 {
                    self.scroll = (self.scroll + edge / row_h * 0.5).clamp(0.0, max_scroll);
                    ui.ctx().request_repaint();
                }
                let y = pos.y.clamp(body_top, body_bottom - 1.0);
                let row = line_at(self.active_pane, y).map(|v| self.layout.lines[v]);
                let at = line_at(self.active_pane, y)
                    .and_then(|v| caret_hit(self, self.active_pane, v, pos.x));
                let head = match (self.col_sel.as_deref(), row) {
                    (Some(s), Some(VLine::Row(r, _))) => {
                        Some((r, col_at_x(self, s.pane, r, pos.x)))
                    }
                    _ => None,
                };
                if let (Some(s), Some((r, col))) = (self.col_sel.as_deref_mut(), head) {
                    s.head = (r, col);
                    self.sel_anchor = None;
                    self.cursor_row = Some(r);
                } else if let (Some(at), Some(c)) = (at, self.caret()) {
                    if c.pane == self.active_pane && !self.table_mode {
                        self.place_caret(c.pane, at, true);
                    }
                } else if let Some(VLine::Row(r, _)) = row {
                    if self.sel_anchor.is_none() {
                        self.sel_anchor = Some(r);
                    }
                    self.cursor_row = Some(r);
                    self.cur_block = self.diff.rows[r]
                        .block()
                        .filter(|&b| !self.diff.blocks[b].ignored);
                }
            }
        }
        self.scroll_to = scroll_asked;
        let selection = self.selection();
        let caret = self.caret().filter(|_| !self.table_mode);
        let carets = if self.table_mode {
            Vec::new()
        } else {
            self.carets()
        };
        let caret_live = self.caret_live(ui.ctx());
        let mut caret_rect: Option<Rect> = None;

        // painting
        let painter = ui.painter();
        for p in 0..n {
            let x0 = content_left + p as f32 * pane_step;
            let gutter_rect =
                Rect::from_min_max(pos2(x0, body_top), pos2(x0 + gutter_w, body_bottom));
            let text_rect = Rect::from_min_max(
                pos2(x0 + gutter_w, body_top),
                pos2(x0 + pane_w - 2.0, body_bottom),
            );
            painter.rect_filled(gutter_rect, 0.0, c.gutter_bg);
            painter.rect_filled(text_rect, 0.0, c.text_bg);
            let gp = ui.painter_at(gutter_rect);
            let tp = ui.painter_at(text_rect);
            let pane = &self.panes[p];
            let (pane_first, shown) = &pane_shown[p];
            let line_y = |v: usize| body_top + (v as f32 - pane_scroll[p]) * row_h;
            let mut laid: Option<(usize, Arc<Galley>, Vec<f32>)> = None;
            for (k, l) in shown.iter().enumerate() {
                let VLine::Row(r, seg) = *l else { continue };
                let seg = seg as usize;
                let y = line_y(pane_first + k);
                let row_rect =
                    Rect::from_min_size(pos2(text_rect.left(), y), vec2(text_rect.width(), row_h));
                if let Some(bg) = self.row_bg(r, p, c) {
                    tp.rect_filled(row_rect, 0.0, bg);
                    if let Some(bar) = self.row_bar(r, p, c) {
                        gp.rect_filled(
                            Rect::from_min_size(
                                pos2(gutter_rect.right() - 3.0, y),
                                vec2(3.0, row_h),
                            ),
                            0.0,
                            bar,
                        );
                    }
                }
                // With a caret, its own selection is drawn instead of whole rows.
                if p == self.active_pane
                    && caret.is_none()
                    && selection.is_some_and(|(a, b)| (a..=b).contains(&r))
                {
                    tp.rect_filled(
                        row_rect,
                        0.0,
                        ui.visuals().selection.bg_fill.gamma_multiply(0.7),
                    );
                }
                if let Some(s) = self.col().filter(|s| s.pane == p) {
                    let (a, b) = s.rows();
                    if (a..=b).contains(&r) {
                        let cols = s.cols();
                        let x = text_rect.left() + 4.0 - self.hscroll;
                        // Over the characters the columns are, as laid out.
                        let line = self.diff.rows[r]
                            .line(p)
                            .map(|l| &self.panes[p].doc.lines[l]);
                        let galley = line.map(|t| col_galley(ui, t));
                        let col_x = |col: usize| match &galley {
                            Some(g) => {
                                let n = g.text().chars().count();
                                if col <= n {
                                    g.pos_from_cursor(egui::text::CCursor::new(col)).min.x
                                } else {
                                    g.rect.width() + (col - n) as f32 * char_w
                                }
                            }
                            None => col as f32 * char_w,
                        };
                        let x0 = x + col_x(cols.start);
                        let x1 = (x + col_x(cols.end)).max(x0 + 1.5);
                        tp.rect_filled(
                            Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_h)),
                            0.0,
                            ui.visuals().selection.bg_fill.gamma_multiply(0.8),
                        );
                    }
                }
                let row = self.diff.rows[r];
                let Some(l) = row.line(p) else { continue };
                if seg == 0 {
                    if self.sync_points.iter().any(|s| s[p] == l) {
                        // A synchronization point: a line across the top of the row.
                        tp.line_segment(
                            [row_rect.left_top(), row_rect.right_top()],
                            Stroke::new(2.0_f32, c.ui.accent),
                        );
                    } else if self.sync_pending.get(p).copied().flatten() == Some(l) {
                        tp.rect_filled(row_rect, 0.0, c.ui.accent.gamma_multiply(0.18));
                        tp.line_segment(
                            [row_rect.left_top(), row_rect.right_top()],
                            Stroke::new(1.0_f32, c.ui.accent.gamma_multiply(0.6)),
                        );
                    }
                }
                let bookmarked = self.bookmarks[p].contains_key(&l);
                if bookmarked && seg == 0 {
                    gp.rect_filled(
                        Rect::from_min_size(pos2(x0 + 1.0, y + 2.0), vec2(4.0, row_h - 4.0)),
                        2.0,
                        c.ui.warning,
                    );
                }
                if cfg.show_edit_markers && pane.edited.contains(&l) {
                    gp.rect_filled(
                        Rect::from_min_size(pos2(x0 + 6.0, y), vec2(3.0, row_h)),
                        0.0,
                        c.ui.accent,
                    );
                }
                if cfg.show_line_numbers && seg == 0 {
                    gp.text(
                        pos2(gutter_rect.right() - 6.0, y + row_h / 2.0),
                        Align2::RIGHT_CENTER,
                        (l + 1).to_string(),
                        font.clone(),
                        if bookmarked {
                            c.ui.warning
                        } else {
                            c.gutter_text
                        },
                    );
                }
                let line = &pane.doc.lines[l];
                let ignored = row
                    .block()
                    .map(|b| self.diff.blocks[b].ignored)
                    .unwrap_or(false);
                let fg = if ignored { c.gutter_text } else { c.text };
                let sel = row.block().is_some() && row.block() == self.cur_block;
                let inline_bg = if sel { c.inline_sel } else { c.inline };
                if let (true, Some(delim)) = (self.table_mode, self.table_delim) {
                    let cells = table::split_row(line, delim);
                    let others: Vec<Vec<String>> = (0..n)
                        .filter(|&q| q != p)
                        .filter_map(|q| {
                            row.line(q)
                                .map(|ol| table::split_row(&self.panes[q].doc.lines[ol], delim))
                        })
                        .collect();
                    let mut x = text_rect.left() + 4.0 - self.hscroll;
                    for (ci, cell) in cells.iter().enumerate() {
                        let w = (self.col_widths.get(ci).copied().unwrap_or(3) as f32 + 1.0)
                            * char_w
                            + 8.0;
                        let cell_rect = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
                        let differs = row.block().is_some()
                            && !ignored
                            && others.iter().any(|o| o.get(ci) != Some(cell));
                        if differs {
                            tp.rect_filled(cell_rect.shrink(1.0), 0.0, inline_bg);
                        }
                        let cp = tp.with_clip_rect(cell_rect.intersect(text_rect));
                        cp.text(
                            pos2(x + 4.0, y + row_h / 2.0),
                            Align2::LEFT_CENTER,
                            cell,
                            font.clone(),
                            fg,
                        );
                        tp.line_segment(
                            [pos2(x + w, y), pos2(x + w, y + row_h)],
                            Stroke::new(1.0_f32, c.table_grid),
                        );
                        x += w;
                    }
                    continue;
                }
                if laid.as_ref().map(|(lr, ..)| *lr) != Some(r) {
                    let spans = if ignored {
                        None
                    } else {
                        pane.hl.as_ref().and_then(|h| h.get(l))
                    };
                    let inline = self
                        .inline_cache
                        .get(&r)
                        .map(|v| v[p].as_slice())
                        .unwrap_or(&[]);
                    let hits = self.find_hits(line);
                    let job = Self::line_job(
                        line,
                        spans,
                        inline,
                        &hits,
                        inline_bg,
                        c.find_hit,
                        fg,
                        &font,
                        tab,
                        cfg.show_whitespace,
                    );
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    let xs = if wrap {
                        segment_x(&galley, &wrap_starts(line, tab, wrap_cols))
                    } else {
                        vec![0.0]
                    };
                    laid = Some((r, galley, xs));
                }
                let Some((_, galley, xs)) = &laid else {
                    continue;
                };
                let left = text_rect.left() + 4.0;
                // Screen x of display column `col` of this line, when it is in this segment.
                let col_x = |col: usize| -> Option<f32> {
                    let gx = galley.pos_from_cursor(CCursor::new(col)).min.x;
                    if !wrap {
                        return Some(left - self.hscroll + gx);
                    }
                    let start = *xs.get(seg)?;
                    let end = xs.get(seg + 1).copied().unwrap_or(f32::INFINITY);
                    (start <= gx && (gx < end || xs.len() == seg + 1)).then_some(left + gx - start)
                };
                for k in carets.iter().filter(|k| k.pane == p && k.has_selection()) {
                    let (a, b) = k.range();
                    if (a.0..=b.0).contains(&l) {
                        let from = if l == a.0 {
                            edit::col_at(line, a.1, tab)
                        } else {
                            0
                        };
                        let to_byte = if l == b.0 { b.1 } else { line.len() };
                        let to = edit::col_at(line, to_byte, tab);
                        // In galley x, then cut to this wrapped segment.
                        let gx = |col: usize| galley.pos_from_cursor(CCursor::new(col)).min.x;
                        let mut g0 = gx(from);
                        let mut g1 = gx(to) + if l < b.0 { char_w * 0.6 } else { 0.0 };
                        let (x0, x1) = if wrap {
                            let start = xs.get(seg).copied().unwrap_or(0.0);
                            let end = xs.get(seg + 1).copied().unwrap_or(f32::INFINITY);
                            g0 = g0.max(start);
                            g1 = g1.min(end);
                            (left + g0 - start, left + g1 - start)
                        } else {
                            (left - self.hscroll + g0, left - self.hscroll + g1)
                        };
                        if x1 > x0 {
                            tp.rect_filled(
                                Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_h)),
                                0.0,
                                ui.visuals().selection.bg_fill.gamma_multiply(0.8),
                            );
                        }
                    }
                }
                if !wrap {
                    tp.galley(pos2(left - self.hscroll, y + 1.0), galley.clone(), fg);
                } else if let Some(&sx) = xs.get(seg) {
                    let right = xs
                        .get(seg + 1)
                        .map_or(text_rect.right(), |&e| left + e - sx);
                    let clip =
                        Rect::from_min_max(pos2(text_rect.left(), y), pos2(right, y + row_h))
                            .intersect(text_rect);
                    tp.with_clip_rect(clip)
                        .galley(pos2(left - sx, y + 1.0), galley.clone(), fg);
                }
                // The other carets: bars only (the input method composes at the primary one).
                for k in carets
                    .iter()
                    .skip(1)
                    .filter(|k| k.pane == p && k.pos.0 == l)
                {
                    if let Some(x) = col_x(edit::col_at(line, k.pos.1, tab)) {
                        let rect =
                            Rect::from_min_max(pos2(x, y + 1.0), pos2(x + 2.0, y + row_h - 1.0));
                        if caret_live {
                            tp.rect_filled(rect, 0.0, c.text);
                        } else {
                            let thin = Rect::from_min_size(rect.min, vec2(1.0, rect.height()));
                            tp.rect_filled(thin, 0.0, c.text.gamma_multiply(0.45));
                        }
                    }
                }
                if let Some(k) = caret.filter(|k| k.pane == p && k.pos.0 == l) {
                    if let Some(x) = col_x(edit::col_at(line, k.pos.1, tab)) {
                        let rect =
                            Rect::from_min_max(pos2(x, y + 1.0), pos2(x + 2.0, y + row_h - 1.0));
                        let preedit = self.caret.as_ref().map_or("", |s| s.preedit.as_str());
                        if !preedit.is_empty() {
                            // Text the input method is composing, underlined at the caret.
                            let g = ui.fonts_mut(|f| {
                                f.layout_no_wrap(preedit.to_string(), font.clone(), fg)
                            });
                            let w = g.size().x;
                            tp.rect_filled(
                                Rect::from_min_size(pos2(x, y), vec2(w, row_h)),
                                0.0,
                                c.text_bg,
                            );
                            tp.galley(pos2(x, y + 1.0), g, fg);
                            tp.line_segment(
                                [pos2(x, y + row_h - 2.0), pos2(x + w, y + row_h - 2.0)],
                                Stroke::new(1.0_f32, fg),
                            );
                        }
                        // Dimmed and thin where typing would not go into the text.
                        if caret_live {
                            tp.rect_filled(rect, 0.0, c.text);
                        } else {
                            let thin = Rect::from_min_size(rect.min, vec2(1.0, rect.height()));
                            tp.rect_filled(thin, 0.0, c.text.gamma_multiply(0.45));
                        }
                        caret_rect = Some(rect);
                    }
                }
            }
            if let Some((v0, v1)) = self.cursor_row.and_then(|cr| self.row_span(cr)) {
                let v0 = self.layout.panes[p].boundaries[v0];
                let v1 = self.layout.panes[p].boundaries[v1];
                if v1 > *pane_first && v0 < pane_first + shown.len() {
                    let rr = Rect::from_min_max(
                        pos2(text_rect.left() + 1.0, line_y(v0)),
                        pos2(text_rect.right() - 1.0, line_y(v1)),
                    );
                    let color = if p == self.active_pane {
                        c.cursor_row
                    } else {
                        c.cursor_row.gamma_multiply(0.35)
                    };
                    tp.rect_stroke(
                        rr,
                        theme::radius::XS,
                        Stroke::new(1.5_f32, color),
                        StrokeKind::Inside,
                    );
                }
            }
            painter.line_segment(
                [
                    pos2(x0 + pane_w - 1.0, avail.top()),
                    pos2(x0 + pane_w - 1.0, body_bottom),
                ],
                Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
        }

        // folded runs of unchanged lines
        for (p, (pane_first, shown)) in pane_shown.iter().enumerate() {
            let x = content_left + p as f32 * pane_step;
            let pane_rect = Rect::from_min_max(pos2(x, body_top), pos2(x + pane_w, body_bottom));
            let bp = ui.painter_at(pane_rect);
            for (k, l) in shown.iter().enumerate() {
                let VLine::Fold(s, e) = *l else { continue };
                let y = body_top + ((pane_first + k) as f32 - pane_scroll[p]) * row_h;
                let rect = Rect::from_min_max(pos2(x, y), pos2(x + pane_w, y + row_h));
                let hovered = hover_fold == Some(self.layout.panes[p].shared[pane_first + k]);
                bp.rect_filled(
                    rect,
                    0.0,
                    if hovered {
                        c.ui.control_hover
                    } else {
                        c.ui.surface_alt
                    },
                );
                for edge in [rect.top() + 0.5, rect.bottom() - 0.5] {
                    bp.hline(rect.x_range(), edge, Stroke::new(1.0_f32, c.ui.border));
                }
                let fg = if hovered { c.ui.accent } else { c.ui.text_dim };
                let label = trf("text.folded", &[("n", (e - s).to_string())]);
                let small = FontId::proportional((cfg.font_size - 1.0).max(10.0));
                let galley = ui.fonts_mut(|f| f.layout_no_wrap(label, small, fg));
                let icon = row_h - 6.0;
                let w = icon + 6.0 + galley.size().x;
                let x = (rect.center().x - w / 2.0).max(rect.left() + 8.0);
                icons::paint(
                    &bp,
                    Rect::from_min_size(pos2(x, y + 3.0), vec2(icon, icon)),
                    Icon::Fold,
                    fg,
                );
                bp.galley(
                    pos2(x + icon + 6.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    fg,
                );
            }
        }

        // Link maps use each pane’s display coordinates in either spacing mode.
        // Only visit blocks intersecting a visible pane, even for very large files.
        let blocks: std::collections::BTreeSet<usize> = pane_shown
            .iter()
            .flat_map(|(_, lines)| lines)
            .filter_map(|line| match *line {
                VLine::Row(r, _) => self.diff.rows[r].block(),
                VLine::Fold(..) => None,
            })
            .collect();
        let mut merge = None;
        for div in 0..n - 1 {
            let x = content_left + div as f32 * pane_step + pane_w;
            let gap = Rect::from_min_max(pos2(x, body_top), pos2(x + link_w, body_bottom));
            let painter = ui.painter_at(gap);
            painter.rect_filled(gap, 0.0, c.ui.surface_alt);
            for &b in &blocks {
                let blk = &self.diff.blocks[b];
                if blk.ignored {
                    continue;
                }
                let lines = |q: usize| &self.panes[q].doc.lines[blk.ranges[q].clone()];
                if lines(div) == lines(div + 1) {
                    continue;
                }
                let (start, end) = self.block_span(b);
                let span = |p: usize| {
                    let layout = &self.layout.panes[p];
                    let top = body_top + (layout.boundaries[start] as f32 - pane_scroll[p]) * row_h;
                    let bottom =
                        body_top + (layout.boundaries[end] as f32 - pane_scroll[p]) * row_h;
                    (top, bottom)
                };
                let left = span(div);
                let right = span(div + 1);
                let selected = self.cur_block == Some(b) || hover_block == Some(b);
                let insertion = blk.ranges[div].is_empty() || blk.ranges[div + 1].is_empty();
                let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| {
                    gap.contains(*p) && p.y >= left.0.min(right.0) && p.y <= left.1.max(right.1)
                });
                let selected = selected || pointer.is_some();
                let (fill, edge) = if self.is_open_conflict(b) {
                    (
                        if selected { c.conflict_sel } else { c.conflict },
                        c.conflict_bar,
                    )
                } else if insertion {
                    (if selected { c.added_sel } else { c.added }, c.ui.success)
                } else {
                    (if selected { c.diff_sel } else { c.diff }, c.change_bar)
                };
                // Only the current or pointed-at difference is drawn at full strength.
                let (fill, edge) = if selected {
                    (fill, edge)
                } else {
                    (fill.gamma_multiply(0.6), edge.gamma_multiply(0.35))
                };
                paint_link(&painter, x, x + link_w, left, right, fill, edge);
                if self.edit.is_some() {
                    continue;
                }
                let arrows: [MergeArrow; 2] = [
                    (
                        div,
                        div + 1,
                        Icon::CopyRight,
                        if n == 2 {
                            "cmd.copy_right"
                        } else if div == 0 {
                            "cmd.copy_from_left"
                        } else {
                            "cmd.copy_middle_right"
                        },
                    ),
                    (
                        div + 1,
                        div,
                        Icon::CopyLeft,
                        if n == 2 {
                            "cmd.copy_left"
                        } else if div == 0 {
                            "cmd.copy_middle_left"
                        } else {
                            "cmd.copy_from_right"
                        },
                    ),
                ];
                let d = row_h.clamp(16.0, 22.0);
                for (i, &(from, to, icon, key)) in arrows.iter().enumerate() {
                    if self.panes[to].doc.read_only {
                        continue;
                    }
                    let (top, bottom) = if i == 0 { left } else { right };
                    let cy = top + ((bottom - top) / 2.0).min(row_h / 2.0);
                    if cy < body_top || cy > body_bottom {
                        continue;
                    }
                    let cx = if i == 0 {
                        x + d / 2.0 + 1.0
                    } else {
                        x + link_w - d / 2.0 - 1.0
                    };
                    let rect = Rect::from_center_size(pos2(cx, cy), vec2(d, d)).intersect(gap);
                    let r = ui
                        .interact(rect, self.id.with(("merge", b, from, to)), Sense::click())
                        .on_hover_text(tr(key))
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tr(key)));
                    if r.hovered() {
                        painter.rect_filled(rect, theme::radius::XS, c.ui.accent);
                    }
                    if selected || r.hovered() {
                        icons::paint(
                            &painter,
                            rect.shrink(3.0),
                            icon,
                            if r.hovered() {
                                c.ui.on_accent
                            } else {
                                c.ui.text
                            },
                        );
                    }
                    if r.clicked() {
                        merge = Some((b, from, to));
                    }
                }
            }
        }
        if let Some((b, from, to)) = merge {
            self.copy_block(b, from, to);
        }

        // the caret: keep it in view sideways, and tell the input method where it is
        if let (Some(k), Some(rect)) = (caret, caret_rect) {
            if self.caret_reveal && !wrap {
                let x0 = content_left + k.pane as f32 * pane_step + gutter_w + 4.0;
                let x1 = x0 + text_w - 12.0;
                if rect.left() < x0 {
                    self.hscroll = (self.hscroll - (x0 - rect.left()) - 4.0 * char_w).max(0.0);
                } else if rect.right() > x1 {
                    self.hscroll += rect.right() - x1 + 4.0 * char_w;
                }
                ui.ctx().request_repaint();
            }
            self.caret_reveal = false;
            if !widgets::typing(ui.ctx()) && !self.panes[k.pane].doc.read_only {
                ui.output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        purpose: egui::IMEPurpose::Normal,
                        rect: body_rect,
                        cursor_rect: rect,
                        should_interrupt_composition: false,
                    });
                });
            }
        }

        // inline editor
        let edit_line = self.edit.as_ref().and_then(|e| {
            let (v0, v1) = self.row_span(e.row)?;
            let b = &self.layout.panes[e.pane].boundaries;
            Some((b[v0], b[v1]))
        });
        if let Some(e) = &mut self.edit {
            let mut done: Option<bool> = None;
            match edit_line.filter(|&(v0, _)| {
                (v0 as f32) >= pane_scroll[e.pane].floor()
                    && (v0 as f32) < pane_scroll[e.pane] + visible
            }) {
                Some((v0, _)) => {
                    let x0 = content_left + e.pane as f32 * pane_step + gutter_w;
                    let y = body_top + (v0 as f32 - pane_scroll[e.pane]) * row_h;
                    let rect = Rect::from_min_size(pos2(x0, y - 1.0), vec2(text_w, row_h + 2.0));
                    let te = egui::TextEdit::singleline(&mut e.text)
                        .font(font.clone())
                        .desired_width(text_w);
                    let r = ui.put(rect, te);
                    if e.focus {
                        r.request_focus();
                        e.focus = false;
                    } else if r.lost_focus() {
                        done = Some(!ui.input(|i| i.key_pressed(Key::Escape)));
                    }
                }
                None => done = Some(true),
            }
            if let Some(commit) = done {
                let e = self.edit.take().unwrap();
                if commit {
                    self.commit_edit(e);
                }
            }
        }

        // context menu
        let mut action: Option<MenuAction> = None;
        if let Some((r, p)) = self.ctx_target {
            let block = self.diff.rows.get(r).and_then(|row| row.block());
            let lines = selection.filter(|&(a, b)| (a..=b).contains(&r));
            resp.context_menu(|ui| {
                widgets::menu_setup(ui);
                let copy = |from, to| match (lines, block) {
                    (Some(_), _) => Some(MenuAction::CopyLines(from, to)),
                    (None, Some(b)) => Some(MenuAction::Copy(b, from, to)),
                    _ => None,
                };
                if lines.is_some() || block.is_some() {
                    let items: &[(String, usize, usize)] = if n == 2 {
                        &[
                            (format!("→  {}", tr("cmd.copy_right")), 0, 1),
                            (format!("←  {}", tr("cmd.copy_left")), 1, 0),
                        ]
                    } else {
                        &[
                            (tr("cmd.copy_from_left").to_string(), 0, 1),
                            (tr("cmd.copy_from_right").to_string(), 2, 1),
                            (tr("cmd.copy_middle_left").to_string(), 1, 0),
                            (tr("cmd.copy_middle_right").to_string(), 1, 2),
                        ]
                    };
                    for (label, from, to) in items {
                        if ui.button(label).clicked() {
                            action = copy(*from, *to);
                        }
                    }
                    ui.separator();
                }
                if ui.button(tr("cmd.edit_line")).clicked() {
                    action = Some(MenuAction::Edit(r, p));
                }
                if ui.button(tr("cmd.insert_line")).clicked() {
                    action = Some(MenuAction::InsertBelow(r, p));
                }
                if ui.button(tr("cmd.delete_line")).clicked() {
                    action = Some(MenuAction::DeleteLine(r, p));
                }
                ui.separator();
                if ui.button(tr("cmd.copy_text")).clicked() {
                    action = Some(MenuAction::CopyText(r, p));
                }
                let mark = if self
                    .bookmark_target(r, p)
                    .is_some_and(|(bp, _)| self.row_bookmarked(r, bp))
                {
                    "cmd.remove_bookmark"
                } else {
                    "cmd.add_bookmark"
                };
                if ui.button(tr(mark)).clicked() {
                    action = Some(MenuAction::ToggleBookmark(r, p));
                }
                if ui.button(tr("cmd.bookmark_comment")).clicked() {
                    action = Some(MenuAction::EditNote(r, p));
                }
                ui.separator();
                let synced = self.diff.rows[r]
                    .line(p)
                    .is_some_and(|l| self.sync_points.iter().any(|s| s[p] == l));
                let picked = self.diff.rows[r].line(p).is_some()
                    && self.sync_pending.get(p).copied().flatten() == self.diff.rows[r].line(p);
                let sync_label = if synced {
                    "cmd.remove_sync_point"
                } else if picked {
                    "cmd.unpick_sync_point"
                } else {
                    "cmd.sync_point"
                };
                let enabled = self.diff.rows[r].line(p).is_some();
                if widgets::menu_item(ui, enabled, tr(sync_label), &Cmd::SyncPoint.shown_key())
                    .clicked()
                {
                    action = Some(MenuAction::SyncPoint(r, p));
                }
                if self.has_sync_points() && ui.button(tr("cmd.clear_sync_points")).clicked() {
                    action = Some(MenuAction::ClearSyncPoints);
                }
                widgets::submenu(ui, tr("cmd.add_rule"), |ui| {
                    for kind in RuleKind::ALL {
                        if ui.button(tr(kind.key())).clicked() {
                            action = Some(MenuAction::AddRule(kind, r, p));
                        }
                    }
                });
                if n == 3 {
                    if let Some(b) = block.filter(|_| !self.panes[1].doc.read_only) {
                        ui.separator();
                        if ui.button(tr("cmd.take_left_right")).clicked() {
                            action = Some(MenuAction::TakeBoth(b, true));
                        }
                        if ui.button(tr("cmd.take_right_left")).clicked() {
                            action = Some(MenuAction::TakeBoth(b, false));
                        }
                    }
                }
                if action.is_some() {
                    ui.close();
                }
            });
        }
        if let Some(a) = action {
            match a {
                MenuAction::Copy(b, from, to) => self.copy_block(b, from, to),
                MenuAction::Edit(r, p) => self.begin_edit(r, p),
                MenuAction::InsertBelow(r, p) => self.insert_below(r, p),
                MenuAction::DeleteLine(r, p) => self.delete_line(r, p),
                MenuAction::CopyText(r, p) => {
                    if let Some(t) = self.menu_copy_text(r, p) {
                        ui.ctx().copy_text(t);
                    }
                }
                MenuAction::CopyLines(from, to) => {
                    if let Some((a, b)) = selection {
                        self.copy_rows(a, b, from, to);
                    }
                }
                MenuAction::ToggleBookmark(r, p) => self.toggle_bookmark_at(r, p),
                MenuAction::SyncPoint(r, p) => self.set_sync_point_at(r, p),
                MenuAction::ClearSyncPoints => self.clear_sync_points(),
                MenuAction::EditNote(r, p) => self.edit_note_at(r, p),
                MenuAction::TakeBoth(b, left_first) => self.take_both_at(b, left_first),
                MenuAction::AddRule(kind, r, p) => {
                    self.rule_request = Some(self.rule_draft(kind, r, p));
                }
            }
        }

        // location pane
        let scroll_before = self.scroll;
        if cfg.show_location_pane && self.layout.row_line.len() == self.diff.rows.len() {
            let loc_rect = Rect::from_min_max(
                pos2(content_right, body_top),
                pos2(content_right + loc_w - 2.0, body_bottom),
            );
            let mut marks: Vec<Mark> = self
                .significant()
                .map(|i| {
                    let b = &self.diff.blocks[i];
                    let sel = self.cur_block == Some(i);
                    let columns = std::array::from_fn(|p| {
                        Some(if b.ranges[p].is_empty() {
                            c.ui.border_strong
                        } else if sel {
                            c.ui.accent
                        } else if self.is_open_conflict(i) {
                            c.conflict_bar
                        } else {
                            c.change_bar
                        })
                    });
                    let (start, end) = self.block_span(i);
                    Mark {
                        start,
                        len: end - start,
                        columns,
                    }
                })
                .collect();
            if self.has_bookmarks() {
                for r in self.bookmark_rows() {
                    marks.push(Mark {
                        start: self.layout.row_line[r],
                        len: 1,
                        columns: std::array::from_fn(|p| {
                            (p < n && self.row_bookmarked(r, p)).then_some(c.ui.warning)
                        }),
                    });
                }
                marks.sort_by_key(|m| m.start);
            }
            if let Some(target) = widgets::location_pane(
                ui,
                self.id.with("loc"),
                loc_rect,
                total,
                n,
                &marks,
                self.scroll,
                visible,
                c.location_bg,
                c.viewport,
            ) {
                self.scroll = (target - visible / 2.0).clamp(0.0, max_scroll);
            }
        }
        let vbar = Rect::from_min_max(
            pos2(content_right + loc_w, body_top),
            pos2(avail.right(), body_bottom),
        );
        widgets::vscrollbar(
            ui,
            self.id.with("vbar"),
            vbar,
            &mut self.scroll,
            total + 1,
            visible,
        );
        if self.scroll != scroll_before {
            self.scroll_anim = None;
        }
        if !wrap {
            let hbar = Rect::from_min_max(
                pos2(content_left, body_bottom),
                pos2(content_right, avail.bottom() - footer_h),
            );
            widgets::hscrollbar(
                ui,
                self.id.with("hbar"),
                hbar,
                &mut self.hscroll,
                content_w,
                text_w,
            );
        }
        ui.allocate_rect(avail, Sense::hover());
        if detail_h > 0.0 {
            let rect = Rect::from_min_max(pos2(full.left(), avail.bottom()), full.max);
            self.line_detail(ui, rect, cfg, c);
            ui.allocate_rect(rect, Sense::hover());
        }
        self.note_window(ui);

        self.goto_window(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_cursor_restores_unique_hunks_and_flags_ambiguous_contexts() {
        let cfg = Config::default();
        let docs = [
            "one\nold a\nthree\nfour\nfive\nold b\nend\n",
            "one\nnew a\nthree\nfour\nfive\nnew b\nend\n",
        ]
        .map(|text| TextDoc::from_text(None, text))
        .to_vec();
        let original = TextView::new(docs.clone(), &cfg);
        let last = original.diff.blocks.len() - 1;
        let id = original.review_hunk_id(&original.diff.blocks[last]);
        let mut reopened = TextView::new(docs, &cfg);
        reopened.review_decision = Some(Box::new(crate::core::review_state::Decision {
            current_hunk: Some(id),
            ..Default::default()
        }));
        reopened.restore_review_cursor();
        assert_eq!(reopened.cur_block, Some(last));
        let docs = [
            "same\nsame\nold\nsame\nsame\nseparator\nsame\nsame\nold\nsame\nsame\n",
            "same\nsame\nnew\nsame\nsame\nseparator\nsame\nsame\nnew\nsame\nsame\n",
        ]
        .map(|text| TextDoc::from_text(None, text))
        .to_vec();
        let mut ambiguous = TextView::new(docs, &cfg);
        let id = ambiguous.review_hunk_id(&ambiguous.diff.blocks[0]);
        ambiguous.review_decision = Some(Box::new(crate::core::review_state::Decision {
            current_hunk: Some(id),
            state: crate::core::review_state::State::Accepted,
            ..Default::default()
        }));
        ambiguous.restore_review_cursor();
        assert!(ambiguous.review_decision.as_ref().unwrap().stale);
    }

    #[test]
    fn spacing_modes_rebuild_cached_rows_without_changing_documents_or_differences() {
        for texts in [
            vec![
                "start\nend\n",
                "start\ninserted long line that wraps\nextra\nend\n",
            ],
            vec![
                "start\nleft\nend\n",
                "start\nend\n",
                "start\ninserted long line that wraps\nextra\nend\n",
            ],
        ] {
            let docs = texts.iter().map(|t| TextDoc::from_text(None, t)).collect();
            let mut view = TextView::new(docs, &Config::default());
            let rows = format!("{:?}", view.diff.rows);
            for cols in [0, 8] {
                view.text_layout = TextLayout::Aligned;
                view.ensure_layout(cols, 4);
                for pane in &view.layout.panes {
                    assert_eq!(
                        pane.shared
                            .iter()
                            .map(|&v| view.layout.lines[v])
                            .collect::<Vec<_>>(),
                        view.layout.lines
                    );
                    assert!(pane
                        .boundaries
                        .iter()
                        .copied()
                        .eq(0..=view.layout.lines.len()));
                }
                assert!(view.layout.lines.iter().any(
                    |line| matches!(line, VLine::Row(r, _) if view.diff.rows[*r].line(0).is_none())
                ));
                view.text_layout = TextLayout::Compact;
                view.ensure_layout(cols, 4);
                assert!(view.layout.panes[0].shared.len() < view.layout.lines.len());
                for (p, expected) in texts.iter().enumerate() {
                    assert_eq!(view.panes[p].doc.text(), *expected);
                }
                assert_eq!(format!("{:?}", view.diff.rows), rows);
            }
            view.set_collapse(true);
            view.text_layout = TextLayout::Aligned;
            view.ensure_layout(8, 4);
            for pane in &view.layout.panes {
                assert_eq!(
                    pane.shared
                        .iter()
                        .map(|&v| view.layout.lines[v])
                        .collect::<Vec<_>>(),
                    view.layout.lines
                );
            }
        }
    }

    #[test]
    fn compact_panes_preserve_real_lines_and_wrapped_segments_in_two_and_three_way_views() {
        for texts in [
            vec![
                "start\nleft\nend\n",
                "start\ninserted\nextra long right hand line\nend\n",
            ],
            vec![
                "start\nleft\nend\n",
                "start\nend\n",
                "start\ninserted\nextra long right hand line\nend\n",
            ],
        ] {
            let docs = texts.iter().map(|t| TextDoc::from_text(None, t)).collect();
            let mut v = TextView::new(docs, &Config::default());
            for cols in [0, 8] {
                v.ensure_layout(cols, 4);
                for p in 0..v.n() {
                    let expected: Vec<(usize, u32)> = v.panes[p]
                        .doc
                        .lines
                        .iter()
                        .enumerate()
                        .flat_map(|(l, text)| {
                            let count = if cols == 0 {
                                1
                            } else {
                                wrap_starts(text, 4, cols).len()
                            };
                            (0..count).map(move |seg| (l, seg as u32))
                        })
                        .collect();
                    let pane = &v.layout.panes[p];
                    let actual: Vec<_> = pane
                        .shared
                        .iter()
                        .map(|&index| {
                            let line = &v.layout.lines[index];
                            let VLine::Row(r, seg) = *line else {
                                panic!("unexpected fold")
                            };
                            (v.diff.rows[r].line(p).expect("no alignment filler"), seg)
                        })
                        .collect();
                    assert_eq!(actual, expected);
                    for (local, &shared) in pane.shared.iter().enumerate() {
                        assert_eq!(pane.position(shared as f32), local as f32);
                        assert_eq!(pane.position(shared as f32 + 0.5), local as f32 + 0.5);
                    }
                    assert_eq!(
                        pane.position(v.layout.lines.len() as f32),
                        expected.len() as f32
                    );
                }
            }
        }
    }

    #[test]
    fn table_block_merge_preserves_unselected_field_changes_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let left = "\"SOURCE\nsame\nsame2\nend\",keep\n";
        let right = "\"TARGET\nsame\nsame2\nend\",custom\n";
        let path = dir.path().join("right.csv");
        std::fs::write(&path, right).unwrap();
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(Some("left.csv".into()), left),
                text::load_doc(&path, None).unwrap(),
            ],
            &Config::default(),
        );
        assert_eq!(v.diff.blocks.len(), 2);
        v.copy_block(0, 0, 1);
        let expected = right.replacen("TARGET", "SOURCE", 1);
        assert_eq!(v.panes[1].doc.text(), expected);
        assert_eq!(
            v.table.docs[1].as_ref().unwrap().records[0].cells[1],
            "custom"
        );
        assert_eq!(v.diff.blocks.len(), 1);
        v.undo();
        assert_eq!(v.panes[1].doc.text(), right);
        v.redo();
        assert_eq!(v.panes[1].doc.text(), expected);
        v.save_pane(1).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    }

    #[test]
    fn table_block_merge_rejects_partial_quotes_without_changing_the_document() {
        let right = "a\nsame\nsame2\nb,x\n";
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(Some("left.csv".into()), "\"a\nsame\nsame2\nb\",x\n"),
                TextDoc::from_text(Some("right.csv".into()), right),
            ],
            &Config::default(),
        );
        assert_eq!(v.diff.blocks.len(), 2);
        v.copy_block(0, 0, 1);
        assert_eq!(v.panes[1].doc.text(), right);
        assert!(!v.panes[1].doc.modified);
        assert!(!v.can_undo());
        assert!(!v.status.is_empty());
    }

    #[test]
    fn selecting_a_multiline_table_record_finds_its_later_difference() {
        let left = "id,note\n1,\"same\nold\"\n";
        let right = "id,note\n1,\"same\nnew\"\n";
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(Some("left.csv".into()), left),
                TextDoc::from_text(Some("right.csv".into()), right),
            ],
            &Config::default(),
        );
        let row = v
            .table
            .diff
            .rows
            .iter()
            .position(|r| r.line(1) == Some(1))
            .unwrap();
        assert!(v.diff.rows[v.diff.row_of_line(1, 1)].block().is_none());
        v.select_table_row(row, 1, 1);
        assert!(v.cur_block.is_some());
        assert_eq!(v.diff.rows[v.cursor_row.unwrap()].line(1), Some(2));
        assert_eq!(v.table.column, 1);
        v.copy_current(true);
        assert_eq!(v.panes[1].doc.text(), left);
    }

    #[test]
    fn table_cell_merge_edit_save_and_undo_preserve_records() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.csv");
        let b = dir.path().join("b.csv");
        let original = "id,note,other\r\n1,\"old\r\n世界\",\"untouched\"\n2,last,\r\n";
        std::fs::write(&a, original.replace("old", "new")).unwrap();
        std::fs::write(&b, original).unwrap();
        let mut v = TextView::new(
            vec![
                text::load_doc(&a, None).unwrap(),
                text::load_doc(&b, None).unwrap(),
            ],
            &Config::default(),
        );
        assert_eq!(v.table.docs[1].as_ref().unwrap().records.len(), 3);
        let row = v
            .table
            .diff
            .rows
            .iter()
            .position(|r| r.line(0) == Some(1))
            .unwrap();
        v.copy_cell(row, 1, 0, 1);
        assert_eq!(v.panes[1].doc.text(), original.replace("old", "new"));
        v.save_pane(1).unwrap();
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            original.replace("old", "new")
        );
        v.undo();
        assert_eq!(table::source(&v.panes[1].doc), original);
        v.redo();
        assert_eq!(v.panes[1].doc.text(), original.replace("old", "new"));
        v.begin_cell_edit(1, 1, 1);
        v.table.edit.as_mut().unwrap().value = "quoted, \"value\"\nnext".into();
        assert!(v.is_modified());
        v.commit_pending_edit();
        let output = table::source(&v.panes[1].doc);
        assert!(output.contains("\"quoted, \"\"value\"\"\nnext\",\"untouched\"\n"));
        assert_eq!(v.table.docs[1].as_ref().unwrap().records.len(), 3);
        v.panes[1].doc.read_only = true;
        let before = output;
        v.copy_cell(row, 1, 0, 1);
        assert_eq!(table::source(&v.panes[1].doc), before);
    }

    fn keyed_view(left: &str, right: &str, m: TableMatch) -> TextView {
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(Some("left.csv".into()), left),
                TextDoc::from_text(Some("right.csv".into()), right),
            ],
            &Config::default(),
        );
        v.set_table_match(m);
        v
    }

    fn by_id() -> TableMatch {
        TableMatch {
            header: true,
            keys: vec![KeyColumn::Name("id".into())],
            ..Default::default()
        }
    }

    /// The row of the rows matched by key whose record on `side` has `id` in its first cell.
    fn keyed_row(v: &TextView, side: usize, id: &str) -> usize {
        let d = v.keyed().unwrap();
        let table = v.table.docs[side].as_ref().unwrap();
        (0..d.rows.len())
            .find(|&i| d.rows[i].records[side].is_some_and(|r| table.records[r].cells[0] == id))
            .unwrap()
    }

    #[test]
    fn large_tables_match_by_key_in_the_background_and_can_be_stopped() {
        let left: String = std::iter::once("id,v\n".to_string())
            .chain((0..300).map(|i| format!("{i},{i}\n")))
            .collect();
        let right: String = std::iter::once("id,v\n".to_string())
            .chain(
                (0..300)
                    .rev()
                    .map(|i| format!("{i},{}\n", i + usize::from(i == 7))),
            )
            .collect();
        let mut v = keyed_view(&left, &right, by_id());
        let at_once = v.keyed().unwrap().clone();
        v.select_keyed_row(keyed_row(&v, 0, "7"));
        let finish = |v: &mut TextView| {
            let start = std::time::Instant::now();
            while v.table.job.is_some() {
                assert!(start.elapsed() < std::time::Duration::from_secs(20));
                std::thread::sleep(std::time::Duration::from_millis(5));
                v.poll_table_job();
            }
        };

        v.table.background_at = Some(100);
        v.set_table_match(TableMatch {
            order: table_match::RowOrder::Key,
            ..by_id()
        });
        assert!(v.table.job.is_some());
        assert!(v.keyed().is_none() && v.table.diff.rows.is_empty());
        assert_eq!(v.table.selected, None);
        // nothing pairs records by position while the match runs
        let lines = v.panes[1].doc.lines.clone();
        v.copy_current(true);
        let first = v.significant().next().unwrap();
        v.copy_block(first, 0, 1);
        v.copy_rows(0, v.diff.rows.len() - 1, 0, 1);
        assert_eq!(v.panes[1].doc.lines, lines);
        assert!(!v.next_diff(false) && !v.prev_diff(false));
        // compared again before it ends, the selection to make is kept
        v.rediff();
        assert!(v.table.job.is_some());
        finish(&mut v);
        let d = v.keyed().unwrap();
        assert_eq!(d.counts, at_once.counts);
        assert_eq!(d.rows.len(), at_once.rows.len());
        assert_eq!(v.table_cursor(), Some(keyed_row(&v, 0, "7")));

        // Stopped: by position, and saying so, until matched again.
        v.set_table_match(by_id());
        v.table
            .job
            .as_ref()
            .unwrap()
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        finish(&mut v);
        assert_eq!(
            v.table.keyed.as_deref(),
            Some(&Err(table_match::Problem::Cancelled))
        );
        assert!(!v.table.diff.rows.is_empty());
        v.rediff();
        finish(&mut v);
        assert_eq!(v.keyed().unwrap().counts, at_once.counts);
    }

    #[test]
    fn reports_of_tables_matched_by_key_record_how_and_what_was_found() {
        use crate::core::report::ReportFormat;
        let left = "id,v\n1,a\n2,b\n2,c\n";
        let right = "id,v\n2,b\n1,z\n3,d\n";
        let m = TableMatch {
            order: table_match::RowOrder::Key,
            rel_tolerance: 0.01,
            ..by_id()
        };
        let v = keyed_view(left, right, m);
        let xml = (v.report_job(ReportFormat::Xml, ""))();
        let at = xml
            .find("<table-match version=\"1\"")
            .expect("table-match element");
        assert!(at < xml.find("</comparison>").unwrap());
        assert!(xml.contains(
            "header=\"true\" columns=\"position\" order=\"key\" ignore-order=\"false\" abs-tolerance=\"0\" rel-tolerance=\"0.01\""
        ));
        assert!(xml.contains("<key name=\"id\"/>"));
        assert!(xml.contains("modified=\"1\""), "{xml}");
        assert!(xml.contains("<warning kind=\"duplicate_keys\" side=\"0\" records=\"2\"/>"));
        // The differences are still those of the text.
        assert!(xml.contains("<difference index=\"1\""));

        let html = (v.report_job(ReportFormat::Html, ""))();
        let section = html.find("<section class=\"table-match\">").unwrap();
        assert!(section < html.find("<p class=\"foot\">").unwrap());
        assert!(html.contains("Rows in: Key order"), "{html}");
        assert!(html.contains("Numeric tolerance: 1%"));

        let plain = keyed_view(left, right, TableMatch::default());
        assert!(!(plain.report_job(ReportFormat::Xml, ""))().contains("table-match"));
        assert!(!(plain.report_job(ReportFormat::Html, ""))().contains("table-match"));
    }

    #[test]
    fn rows_matched_by_key_navigate_and_copy_cells_records_and_removals() {
        let left = "id,name,price\n1,Ann,10\n2,Bob,20\n3,Cy,30\n4,Dee,40\n";
        let right = "id,name,price\n3,Cy,30\n1,Ann,11\n2,Bob,20\n5,Eve,50\n";
        let mut v = keyed_view(left, right, by_id());
        let d = v.keyed().unwrap();
        assert_eq!(
            (
                d.counts.modified,
                d.counts.added,
                d.counts.removed,
                d.counts.ambiguous
            ),
            (1, 1, 1, 0)
        );
        assert!(d.counts.moved > 0);
        let mut m = by_id();
        m.ignore_order = true;
        v.set_table_match(m);
        assert_eq!(v.diff_position(), (None, 3));

        v.first_diff();
        assert_eq!(v.table_cursor(), Some(keyed_row(&v, 0, "1")));
        assert_eq!(v.table.column, 2);
        assert_eq!(v.diff_position(), (Some(0), 3));
        v.copy_current(false);
        let left1 = left.replace("1,Ann,10", "1,Ann,11");
        assert_eq!(v.panes[0].doc.text(), left1);
        assert_eq!(v.diff_position().1, 2);

        let added = keyed_row(&v, 1, "5");
        v.select_keyed_row(added);
        v.copy_current(false);
        let left2 = left1.replace("2,Bob,20\n", "2,Bob,20\n5,Eve,50\n");
        assert_eq!(v.panes[0].doc.text(), left2);

        let removed = keyed_row(&v, 0, "4");
        v.select_keyed_row(removed);
        v.copy_current(false);
        assert_eq!(v.panes[0].doc.text(), left2.replace("4,Dee,40\n", ""));
        assert_eq!(v.diff_position(), (None, 0));
        assert!(!v.next_diff(false));

        v.undo();
        v.undo();
        v.undo();
        assert_eq!(v.panes[0].doc.text(), left);
        assert_eq!(v.panes[1].doc.text(), right);
    }

    #[test]
    fn removing_a_record_moves_on_to_the_next_difference_without_passing_it() {
        let mut v = keyed_view("id,v\n2,b\n3,c\n", "id,v\n", by_id());
        v.select_keyed_row(keyed_row(&v, 0, "2"));
        v.copy_current(false);
        assert_eq!(v.panes[0].doc.text(), "id,v\n3,c\n");
        assert_eq!(v.table_cursor(), Some(keyed_row(&v, 0, "3")));
        assert_eq!(v.diff_position(), (Some(0), 1));
    }

    #[test]
    fn rows_with_ambiguous_keys_are_never_copied() {
        let left = "id,v\n1,a\n2,b\n";
        let right = "id,v\n1,x\n1,y\n2,b\n";
        let mut v = keyed_view(left, right, by_id());
        assert_eq!(v.keyed().unwrap().counts.ambiguous, 2);
        v.first_diff();
        let row = v.table_cursor().unwrap();
        assert_eq!(v.keyed().unwrap().rows[row].state, RowState::Ambiguous);
        v.copy_current(true);
        v.copy_current(false);
        v.copy_cell(row, 1, 0, 1);
        assert_eq!(v.panes[0].doc.text(), left);
        assert_eq!(v.panes[1].doc.text(), right);
        assert!(!v.can_undo());
        assert_eq!(v.status, tr("table.ambiguous_copy"));
    }

    #[test]
    fn columns_paired_by_name_copy_into_the_paired_column() {
        let left = "id,a,b\n1,x,y\n2,p,q\n";
        let right = "id,b,a\n1,y,z\n2,q,p\n";
        let mut m = by_id();
        m.columns = table_match::ColumnPolicy::Name;
        let mut v = keyed_view(left, right, m);
        assert_eq!(v.diff_position(), (None, 1));
        v.first_diff();
        v.copy_current(true);
        assert_eq!(v.panes[1].doc.text(), "id,b,a\n1,y,x\n2,q,p\n");
        v.undo();
        let row = keyed_row(&v, 0, "1");
        v.copy_cell(row, 1, 0, 1);
        assert_eq!(v.panes[1].doc.text(), "id,b,a\n1,y,x\n2,q,p\n");
    }

    #[test]
    fn a_missing_key_column_falls_back_to_position_and_says_why() {
        let left = "id,v\n1,a\n";
        let right = "code,v\n1,a\n";
        let v = keyed_view(left, right, by_id());
        assert!(v.keyed().is_none());
        assert!(matches!(
            v.table.keyed.as_deref(),
            Some(Err(table_match::Problem::MissingKey { side: 1, .. }))
        ));
        assert_eq!(v.table.diff.rows.len(), 2);
    }

    #[test]
    fn tolerances_parse_as_absolute_or_percent() {
        assert_eq!(parse_tolerance(""), Some((0.0, 0.0)));
        assert_eq!(parse_tolerance(" 0.5 "), Some((0.5, 0.0)));
        assert_eq!(parse_tolerance("1%"), Some((0.0, 0.01)));
        assert_eq!(parse_tolerance("-1"), None);
        assert_eq!(parse_tolerance("abc"), None);
        assert_eq!(parse_tolerance("inf"), None);
        let m = TableMatch {
            rel_tolerance: 0.01,
            ..Default::default()
        };
        assert_eq!(tolerance_text(&m), "1%");
    }

    #[test]
    fn table_rejects_edits_after_underlying_document_changes() {
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(Some("a.csv".into()), "a,b\n"),
                TextDoc::from_text(Some("b.csv".into()), "a,c\n"),
            ],
            &Config::default(),
        );
        v.begin_cell_edit(1, 0, 1);
        let mut edit = v.table.edit.take().unwrap();
        edit.value = "edited".into();
        v.panes[1].doc.lines[0] = "external,change".into();
        assert!(v.apply_cell_edit(&edit).is_err());
        assert_eq!(v.panes[1].doc.lines[0], "external,change");
    }

    fn strict_config() -> Config {
        Config {
            diff: DiffOptions {
                ignore_eol: false,
                ignore_codepage: false,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn copy_all_transfers_formats_across_content_blocks_and_undoes() {
        let cfg = strict_config();
        let mut a = TextDoc::from_text(None, "one\r\nTWO\r\nthree\r\n");
        a.encoding = TextEncoding::Utf16Le;
        a.bom = true;
        let b = TextDoc::from_text(None, "one\ntwo\nthree\n");
        let old = format_of(&b);
        let mut v = TextView::new(vec![a.clone(), b], &cfg);
        assert!(v.diff.blocks.len() > 1);
        v.copy_all(0, 1);
        assert!(v.diff.is_identical());
        assert_eq!(format_of(&v.panes[1].doc), format_of(&a));
        v.undo();
        assert_eq!(format_of(&v.panes[1].doc), old);
        assert_eq!(v.panes[1].doc.lines[1], "two");
        v.redo();
        assert!(v.diff.is_identical());
    }

    #[test]
    fn auto_merge_combines_text_and_each_independent_format_property() {
        let cfg = strict_config();
        let left = TextDoc::from_text(None, "ONE\ntwo\n");
        let base = TextDoc::from_text(None, "one\ntwo\n");
        let mut right = TextDoc::from_text(None, "one\r\ntwo\r\n");
        right.bom = true;
        right.encoding = TextEncoding::Utf16Le;
        let original = format_of(&base);
        let mut v = TextView::new(vec![left, base, right.clone()], &cfg);
        assert_eq!(v.diff.conflicts(), 0);
        assert!(v
            .diff
            .blocks
            .iter()
            .any(|b| b.kind == BlockKind::Compatible));
        v.auto_merge();
        assert_eq!(v.panes[1].doc.lines, ["ONE", "two"]);
        assert_eq!(format_of(&v.panes[1].doc), format_of(&right));
        assert_eq!(v.diff.conflicts(), 0);
        v.undo();
        assert_eq!(v.panes[1].doc.lines, ["one", "two"]);
        assert_eq!(format_of(&v.panes[1].doc), original);
        v.redo();
        assert_eq!(v.panes[1].doc.lines, ["ONE", "two"]);
        assert_eq!(format_of(&v.panes[1].doc), format_of(&right));
    }

    #[test]
    fn format_regression_mixed_eol_merge_save_undo_redo() {
        for (base, left, right, expected) in [
            ("a\nb\nc\n", "a\r\nb\nc\n", "a\nb\nc\r\n", "a\r\nb\nc\r\n"),
            (
                "a\nb\nc\nd\n",
                "added\na\r\nb\nc\nd\n",
                "a\nb\nc\r\nd\n",
                "added\na\r\nb\nc\r\nd\n",
            ),
            (
                "a\nb\nc\nd\n",
                "a\r\nc\nd\n",
                "a\nb\nc\r\nd\n",
                "a\r\nc\r\nd\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("merged.txt");
            std::fs::write(&out, base).unwrap();
            let docs = vec![
                TextDoc::from_text(None, left),
                TextDoc::from_text(Some(out.clone()), base),
                TextDoc::from_text(None, right),
            ];
            let mut v = TextView::new(docs, &strict_config());
            assert_eq!(v.diff.conflicts(), 0);
            v.auto_merge();
            assert_eq!(v.diff.conflicts(), 0);
            assert_eq!(v.panes[1].doc.text(), expected);
            assert!(v.panes[1].doc.preserve_eols);
            v.save_pane(1).unwrap();
            assert_eq!(std::fs::read(&out).unwrap(), expected.as_bytes());
            v.undo();
            assert_eq!(v.panes[1].doc.text(), base);
            assert!(!v.panes[1].doc.preserve_eols);
            assert!(v.panes[1].doc.modified);
            v.redo();
            assert_eq!(v.panes[1].doc.text(), expected);
            assert!(v.panes[1].doc.preserve_eols);
        }
    }

    #[test]
    fn merge_preview_cannot_validate_unresolved_eol_or_encoding_conflicts() {
        use crate::core::merge_validation::{validate, ValidationStatus};
        let cfg = strict_config();
        let eols = ["one\r\n", "one\n", "one\r"]
            .map(|s| TextDoc::from_text(None, s))
            .to_vec();
        let mut encodings = vec![TextDoc::from_text(None, "one\n"); 3];
        encodings[0].encoding = TextEncoding::Utf16Le;
        encodings[2].encoding = TextEncoding::Utf16Be;
        for docs in [eols, encodings] {
            let output = docs[1].clone();
            let view = TextView::new(docs, &cfg);
            let plan = view.preview_merge(&cfg).unwrap();
            assert!(plan.actions.iter().any(|a| a.operation == "conflict"));
            let bytes = text::encode(&output.text(), output.encoding, output.bom);
            assert_eq!(
                validate(&plan, &output, &bytes).status,
                ValidationStatus::Failed
            );
        }
    }

    #[test]
    fn preview_and_auto_merge_use_the_same_nontrivial_synchronization_points() {
        let cfg = Config::default();
        let docs = [
            "left\nsame\nend\n",
            "base\nsame\nend\n",
            "right\nsame\nend\n",
        ]
        .map(|s| TextDoc::from_text(None, s))
        .to_vec();
        let unsynced = TextView::new(docs.clone(), &cfg)
            .preview_merge(&cfg)
            .unwrap();
        let mut actual = TextView::new(docs, &cfg);
        actual.sync_points = vec![[1, 0, 1]];
        let plan = actual.preview_merge(&cfg).unwrap();
        // Conflicts retain the base, so different alignments can produce the same bytes.
        // Check that the synchronization point changes the planned ranges instead.
        assert_ne!(
            plan.actions.iter().map(|a| &a.ranges).collect::<Vec<_>>(),
            unsynced
                .actions
                .iter()
                .map(|a| &a.ranges)
                .collect::<Vec<_>>()
        );
        actual.auto_merge();
        let output = &actual.panes[1].doc;
        assert_eq!(plan.preview.as_deref(), Some(output.text().as_str()));
        assert_eq!(
            plan.expected_digest,
            Some(crate::core::analysis::digest(&text::encode(
                &output.text(),
                output.encoding,
                output.bom
            )))
        );
    }

    #[test]
    fn empty_document_bom_edits_are_not_reported_as_deletions() {
        use crate::core::merge_validation::{validate, ValidationStatus};
        let cfg = strict_config();
        let mut left = TextDoc::empty(None);
        left.bom = true;
        let mut actual =
            TextView::new(vec![left, TextDoc::empty(None), TextDoc::empty(None)], &cfg);
        let plan = actual.preview_merge(&cfg).unwrap();
        assert_eq!(plan.actions.len(), 1);
        actual.auto_merge();
        let output = &actual.panes[1].doc;
        let bytes = text::encode(&output.text(), output.encoding, output.bom);
        assert_eq!(bytes, b"\xef\xbb\xbf");
        assert_eq!(
            validate(&plan, output, &bytes).status,
            ValidationStatus::Passed
        );
    }

    #[test]
    fn merge_preview_accepts_independent_text_and_format_changes_with_sync_points() {
        use crate::core::merge_validation::{validate, ValidationStatus};
        let cfg = strict_config();
        let left = TextDoc::from_text(None, "start\nLEFT\nanchor\nend\n");
        let base = TextDoc::from_text(None, "start\nbase\nanchor\nend\n");
        let mut right = base.clone();
        right.encoding = TextEncoding::Utf16Le;
        right.bom = true;
        let mut actual = TextView::new(vec![left, base, right], &cfg);
        actual.sync_points = vec![[2, 2, 2]];
        let plan = actual.preview_merge(&cfg).unwrap();
        assert!(plan
            .findings
            .iter()
            .all(|f| f.severity != crate::core::merge_validation::ValidationSeverity::Error));
        actual.auto_merge();
        let output = &actual.panes[1].doc;
        assert_eq!(output.text(), "start\nLEFT\nanchor\nend\n");
        assert_eq!(output.encoding, TextEncoding::Utf16Le);
        assert!(output.bom);
        let bytes = text::encode(&output.text(), output.encoding, output.bom);
        assert_eq!(
            validate(&plan, output, &bytes).status,
            ValidationStatus::Passed
        );
    }

    #[test]
    fn automatic_merge_keeps_real_format_conflicts_visible() {
        let cfg = strict_config();
        let left = TextDoc::from_text(None, "one\r\n");
        let base = TextDoc::from_text(None, "one\n");
        let right = TextDoc::from_text(None, "one\r");
        let mut v = TextView::new(vec![left, base, right], &cfg);
        assert_eq!(v.diff.conflicts(), 1);
        v.auto_merge();
        assert_eq!(v.panes[1].doc.eol, Eol::Lf);
        assert_eq!(v.diff.conflicts(), 1);
    }

    #[test]
    fn saving_mixed_endings_refreshes_the_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("a.txt"), dir.path().join("b.txt")];
        std::fs::write(&paths[0], b"ONE\r\ntwo\nthree\n").unwrap();
        std::fs::write(&paths[1], b"one\r\ntwo\nthree\n").unwrap();
        let docs = paths
            .iter()
            .map(|p| text::load_doc(p, None).unwrap())
            .collect();
        let mut v = TextView::new(docs, &strict_config());
        v.copy_block(0, 0, 1);
        assert!(v.diff.is_identical());
        v.save_pane(1).unwrap();
        assert!(!v.diff.is_identical());
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"ONE\ntwo\nthree\n");
    }

    #[test]
    fn rediff_and_eol_changes_use_document_formats() {
        let cfg = Config {
            diff: DiffOptions {
                ignore_eol: false,
                ignore_codepage: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let a = TextDoc::from_text(None, "one\r\n");
        let b = TextDoc::from_text(None, "one\n");
        let mut v = TextView::new(vec![a, b], &cfg);
        assert!(!v.diff.is_identical());
        v.copy_block(0, 0, 1);
        assert!(v.diff.is_identical());
        v.undo();
        assert!(!v.diff.is_identical());
        v.set_eol(1, Eol::CrLf);
        assert!(v.diff.is_identical());
        v.undo();
        assert!(!v.diff.is_identical());
        let a = TextDoc::from_text(None, "one\n");
        let b = TextDoc::from_text(None, "one");
        let mut v = TextView::new(vec![a, b], &cfg);
        v.copy_block(0, 0, 1);
        assert!(v.diff.is_identical());
        assert!(v.panes[1].doc.final_newline);
        v.undo();
        assert!(!v.panes[1].doc.final_newline);
    }

    #[test]
    fn finds_words_and_patterns_and_replaces_them() {
        let re = find_regex("foo", false, false, false).unwrap();
        assert!(re.is_match("a FOO b"));
        let re = find_regex("foo", true, true, false).unwrap();
        assert!(!re.is_match("a FOO b") && !re.is_match("food") && re.is_match("a foo."));
        let re = find_regex("a.c", true, false, false).unwrap();
        assert!(re.is_match("xa.cx") && !re.is_match("abc"));
        assert!(find_regex("(", true, false, true).is_err());

        let mut lines = vec!["x = 1; y = 2".to_string(), "none".into(), "z = 3".into()];
        let re = find_regex(r"(\w) = (\d)", true, false, true).unwrap();
        assert_eq!(replace_all(&mut lines, &re, "$2 = $1", true), 3);
        assert_eq!(lines, ["1 = x; 2 = y", "none", "3 = z"]);
        let re = find_regex("none", true, false, false).unwrap();
        assert_eq!(replace_all(&mut lines, &re, "$1", false), 1);
        assert_eq!(lines[1], "$1");
    }
    use std::time::Duration;

    fn touch(path: &std::path::Path, text: &str, age: u64) {
        std::fs::write(path, text).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn wraps_after_spaces_and_counts_wide_chars_twice() {
        assert_eq!(wrap_starts("hello world foo", 4, 8), [0, 6, 12]);
        assert_eq!(wrap_starts("abcdefghij", 4, 4), [0, 4, 8]);
        assert_eq!(wrap_starts("漢字漢字漢", 4, 4), [0, 4, 8]);
        assert_eq!(wrap_starts("", 4, 8), [0]);
    }

    fn two_way(a: &str, b: &str) -> TextView {
        let docs = vec![TextDoc::from_text(None, a), TextDoc::from_text(None, b)];
        TextView::new(docs, &Config::default())
    }

    fn lines(v: &TextView, p: usize) -> Vec<&str> {
        v.panes[p].doc.lines.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_inline_view_lists_removed_lines_before_added_ones_and_folds_the_rest() {
        let same: String = (1..=20).map(|i| format!("same {i}\n")).collect();
        let mut v = two_way(
            &format!("a\nold 1\nold 2\n{same}z\n"),
            &format!("a\nnew\n{same}z\n"),
        );
        let text = |v: &TextView| -> Vec<String> {
            v.build_unified()
                .iter()
                .map(|l| match *l {
                    ULine::Line { row, pane, change } => {
                        let line = v.diff.rows[row].line(pane).unwrap();
                        let sign = match (change, pane) {
                            (false, _) => " ",
                            (true, 0) => "-",
                            _ => "+",
                        };
                        format!("{sign}{}", v.panes[pane].doc.lines[line])
                    }
                    ULine::Fold(lo, hi) => format!("~{}", hi - lo),
                })
                .collect()
        };
        let all = text(&v);
        assert_eq!(all[..5], [" a", "-old 1", "-old 2", "+new", " same 1"]);
        assert_eq!(all.len(), 1 + 3 + 20 + 1);

        v.set_collapse(true);
        let folded = text(&v);
        let fold = folded.iter().position(|l| l.starts_with('~')).unwrap();
        assert_eq!(folded[fold - 1], " same 3");
        assert_eq!(folded[fold..], ["~18"]);

        // Going to a line folded away unfolds it.
        v.unified = true;
        v.goto_line(15);
        let (ctx, cfg, colors) = (
            egui::Context::default(),
            Config::default(),
            theme::colors(false),
        );
        for _ in 0..2 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| v.unified_ui(ui, &cfg, &colors));
            })
            .textures_delta
            .clear();
        }
        assert!(text(&v).contains(&" same 13".to_string()));
    }

    #[test]
    fn switching_to_the_side_by_side_view_shows_the_cursor() {
        let left: String = (1..=200).map(|i| format!("l{i}\n")).collect();
        let mut v = two_way(&left, &left.replace("l150\n", "changed\n"));
        v.unified = true;
        let (ctx, cfg, colors) = (
            egui::Context::default(),
            Config::default(),
            theme::colors(false),
        );
        crate::ui::fonts::setup(&ctx, "en");
        let frame = |v: &mut TextView| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0))),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| v.ui(ui, &cfg, &colors));
            })
            .textures_delta
            .clear();
        };
        // The inline view takes the scroll to the first difference.
        for _ in 0..3 {
            frame(&mut v);
        }
        assert_eq!((v.cursor_row, v.scroll_to), (Some(149), None));
        v.unified = false;
        for _ in 0..3 {
            frame(&mut v);
        }
        assert!(v.scroll > 0.0);
    }

    #[test]
    fn the_inline_cursor_moves_on_from_an_added_line() {
        let left: String = (1..=60).map(|i| format!("l{i}\n")).collect();
        let right = left.replace("l30\n", "l30\nadded\n");
        let mut v = two_way(&left, &right);
        v.unified = true;
        // The first difference is on the right only, while the left side is active.
        assert_eq!((v.cursor_row, v.active_pane), (Some(30), 0));
        let (ctx, cfg, colors) = (
            egui::Context::default(),
            Config::default(),
            theme::colors(false),
        );
        let frame = |v: &mut TextView, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| v.unified_ui(ui, &cfg, &colors));
            });
            out.textures_delta.clear();
            out
        };
        frame(&mut v, vec![]);
        let copied = frame(&mut v, vec![egui::Event::Copy])
            .platform_output
            .commands;
        assert!(copied.contains(&egui::OutputCommand::CopyText("added".into())));
        let down = egui::Event::Key {
            key: Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        frame(&mut v, vec![down]);
        assert_eq!(v.cursor_row, Some(31));
    }

    #[test]
    fn sync_points_align_picked_lines_and_follow_edits() {
        let mut v = two_way("intro\nkeep\nend\n", "head\nmore\nintro\nkeep\nend\n");
        // Pick "head" on the right and "intro" on the left: they share a row.
        v.set_sync_point_at(v.diff.row_of_line(1, 0), 1);
        assert!(
            v.has_sync_points() && v.sync_points.is_empty(),
            "one side picked"
        );
        assert!(!v.status.is_empty());
        v.set_sync_point_at(v.diff.row_of_line(0, 0), 0);
        assert_eq!(v.sync_points, [[0, 0, 0]]);
        let row = v.diff.row_of_line(0, 0);
        assert_eq!(v.diff.rows[row].lines()[..2], [Some(0), Some(0)]);
        // An edit above the point on the left moves it along with its line.
        v.push_undo();
        v.panes[0].doc.lines.insert(0, "zero".into());
        v.mark_changed(&[0]);
        assert_eq!(lines(&v, 0)[1], "intro");
        assert_eq!(v.sync_points, [[1, 0, 0]]);
        // Undo and redo move it back and forth with its line.
        v.undo();
        assert_eq!(v.sync_points, [[0, 0, 0]], "undo");
        v.redo();
        assert_eq!(v.sync_points, [[1, 0, 0]], "redo");
        // Picking a picked line again cancels the pick.
        v.set_sync_point_at(v.diff.row_of_line(0, 3), 0);
        assert!(v.has_sync_points() && v.sync_pending[0] == Some(3));
        v.set_sync_point_at(v.diff.row_of_line(0, 3), 0);
        assert_eq!(v.sync_pending[0], None);
        assert_eq!(v.sync_points, [[1, 0, 0]]);
        // Picking a line of a point again removes the point.
        v.set_sync_point_at(v.diff.row_of_line(1, 0), 1);
        assert!(v.sync_points.is_empty() && !v.has_sync_points());
        // Points that would cross an earlier one are not kept.
        v.set_sync_point_at(v.diff.row_of_line(0, 3), 0);
        v.set_sync_point_at(v.diff.row_of_line(1, 1), 1);
        v.set_sync_point_at(v.diff.row_of_line(0, 1), 0);
        v.set_sync_point_at(v.diff.row_of_line(1, 3), 1);
        assert_eq!(v.sync_points, [[3, 1, 0]], "the first point is kept");
        // Editing a point's line keeps the point; deleting it drops it until undone.
        v.push_undo();
        v.panes[0].doc.lines[3] = "end!".into();
        v.mark_changed(&[0]);
        assert_eq!(v.sync_points, [[3, 1, 0]], "edited");
        v.delete_line(v.diff.row_of_line(0, 3), 0);
        assert!(v.sync_points.is_empty(), "deleted");
        v.undo();
        assert_eq!(v.sync_points, [[3, 1, 0]], "undo brings it back");
        v.redo();
        assert!(v.sync_points.is_empty(), "redo drops it again");
        v.clear_sync_points();
        assert!(!v.has_sync_points());
    }

    #[test]
    fn takes_both_sides_pastes_and_keeps_bookmarks_on_their_lines() {
        let docs = ["a\nL\nz\n", "a\nb\nz\n", "a\nR\nz\n"]
            .iter()
            .map(|t| TextDoc::from_text(None, t))
            .collect();
        let mut v = TextView::new(docs, &Config::default());
        assert!(v.can_take_both());
        v.take_both(true);
        assert_eq!(lines(&v, 1), ["a", "L", "R", "z"]);
        v.undo();
        v.take_both(false);
        assert_eq!(lines(&v, 1), ["a", "R", "L", "z"]);

        let mut empty = TextDoc::empty(None);
        empty.lines.push(String::new());
        let mut v = TextView::new(vec![empty.clone(), empty], &Config::default());
        v.active_pane = 1;
        v.paste("x\ny\n");
        assert_eq!(lines(&v, 1), ["x", "y"]);
        assert!(v.panes[1].doc.final_newline && v.panes[1].doc.modified);
        v.cursor_row = Some(v.diff.row_of_line(1, 0));
        v.paste("between");
        assert_eq!(lines(&v, 1), ["x", "between", "y"]);

        let mut v = two_way("one\ntwo\nthree\n", "one\ntwo\nthree\n");
        v.active_pane = 0;
        v.cursor_row = Some(2);
        v.toggle_bookmark();
        v.cursor_row = Some(0);
        v.paste("zero");
        assert_eq!(v.bookmarks[0].keys().copied().collect::<Vec<_>>(), [3]);
        v.cursor_row = Some(0);
        v.goto_bookmark(true);
        assert_eq!(v.cursor_row, Some(v.diff.row_of_line(0, 3)));
        v.undo();
        assert_eq!(v.bookmarks[0].keys().copied().collect::<Vec<_>>(), [2]);
        v.toggle_bookmark_at(v.diff.row_of_line(0, 2), 0);
        assert!(!v.has_bookmarks());
    }

    #[test]
    fn keeps_bookmark_comments_and_marks_edited_lines() {
        let mut v = two_way("one\ntwo\nthree\n", "one\nTWO\nthree\n");
        v.active_pane = 0;
        v.edit_note_at(2, 0);
        let (p, l, ..) = v.note_edit.take().unwrap();
        v.bookmarks[p].insert(l, "check this".into());
        v.edit_note_at(2, 0);
        v.cursor_row = Some(0);
        v.paste("zero");
        assert_eq!(v.note_edit.take().map(|n| n.1), Some(3));
        assert_eq!(
            v.bookmarks[0].get(&3).map(String::as_str),
            Some("check this")
        );
        assert_eq!(v.panes[0].edited, HashSet::from([1]));
        assert_eq!(v.row_note(v.diff.row_of_line(0, 3)), Some("check this"));

        let mut d = two_way("a\nb\nc\nd\n", "a\nb\nc\nd\n");
        d.bookmarks[0].insert(1, String::new());
        d.bookmarks[0].insert(2, "keep me".into());
        d.bookmarks[1].insert(1, "first".into());
        d.bookmarks[1].insert(2, "second".into());
        for p in 0..2 {
            let before = d.panes[p].doc.lines.clone();
            d.panes[p].doc.lines.remove(1);
            d.remap_bookmarks(p, &before);
        }
        assert_eq!(d.bookmarks[0].get(&1).map(String::as_str), Some("keep me"));
        assert_eq!(
            d.bookmarks[1].get(&1).map(String::as_str),
            Some("second\nfirst")
        );

        let saved = v.saved_compare();
        assert_eq!(
            saved.panes[0].bookmarks.get(&4).map(String::as_str),
            Some("check this")
        );
        let r = TextView::from_saved(&saved, &Config::default());
        assert_eq!(r.bookmarks[0], v.bookmarks[0]);
        assert_eq!(lines(&r, 0), lines(&v, 0));
        assert!(r.panes.iter().all(|p| p.doc.read_only));
        assert!(saved.sync_points.is_empty());
    }

    #[test]
    fn saved_comparisons_keep_their_sync_points() {
        let mut v = two_way("a\nb\nc\nd\n", "x\na\nc\nd\n");
        v.sync_points = vec![[1, 0, 0], [3, 3, 0]];
        v.rediff();
        let saved = v.saved_compare();
        assert_eq!(
            saved.sync_points,
            [vec![2, 1], vec![4, 4]],
            "1-based, one per side"
        );
        let r = TextView::from_saved(&saved, &Config::default());
        assert_eq!(r.sync_points, v.sync_points);
        assert_eq!(
            r.diff.rows.len(),
            v.diff.rows.len(),
            "compared the same way"
        );
        // Points that don't fit the texts are left out, not an error.
        let mut odd = saved.clone();
        odd.sync_points = vec![vec![9, 9], vec![1], vec![0, 1]];
        assert!(TextView::from_saved(&odd, &Config::default())
            .sync_points
            .is_empty());
        // Files saved before sync points were kept still open.
        let json = serde_json::to_string(&saved.panes).unwrap();
        let old: crate::core::saved::SavedCompare = serde_json::from_str(&format!(
            "{{\"version\":1,\"saved\":\"x\",\"panes\":{json}}}"
        ))
        .unwrap();
        assert!(old.sync_points.is_empty());
    }

    #[test]
    fn indents_selected_lines_and_counts_statistics() {
        let mut v = two_way("a\n\tb\nc\n", "a\nx\nc\nd\n");
        v.active_pane = 1;
        v.tab_size = 4;
        v.sel_anchor = Some(0);
        v.cursor_row = Some(1);
        v.indent(false);
        assert_eq!(lines(&v, 1), ["    a", "    x", "c", "d"]);
        v.indent(true);
        assert_eq!(lines(&v, 1), ["a", "x", "c", "d"]);
        v.active_pane = 0;
        v.sel_anchor = None;
        v.cursor_row = Some(v.diff.row_of_line(0, 1));
        v.indent(true);
        assert_eq!(lines(&v, 0), ["a", "b", "c"]);

        let stats = v.statistics();
        assert_eq!(stats.len(), 1);
        assert_eq!(
            stats[0].1,
            diff::Stats {
                identical: 2,
                changed: 1,
                added: 1,
                differences: 2,
                ..Default::default()
            }
        );
    }

    #[test]
    fn shows_json_reformatted_while_the_option_is_on() {
        let dir = std::env::temp_dir().join(format!("om_pretty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.json"), dir.join("b.json"));
        std::fs::write(&a, r#"{"b":1,"a":[1,2]}"#).unwrap();
        std::fs::write(&b, "{\n  \"a\": [1, 2],\n  \"b\": 1\n}\n").unwrap();
        let docs = vec![
            text::load_doc(&a, None).unwrap(),
            text::load_doc(&b, None).unwrap(),
        ];
        let mut v = TextView::new(docs, &Config::default());
        assert!(!v.diff.is_identical());
        let mut opts = DiffOptions {
            prettify: true,
            ..Default::default()
        };
        v.set_options(&opts);
        assert!(v.panes[0].original.is_some() && v.diff.is_identical());
        assert!(v.panes[0].doc.read_only);
        opts.prettify = false;
        v.set_options(&opts);
        assert!(v.panes[0].original.is_none() && lines(&v, 0) == [r#"{"b":1,"a":[1,2]}"#]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_caret_types_splits_and_pastes_lines_like_an_editor() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\nb\nc\n", "a\nb\nc\n");
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (1, 1), false);
        assert!(v.takes_text());
        let text = |t: &str| egui::Event::Text(t.into());
        frame(&mut v, &ctx, vec![text("X"), text("Y")]);
        assert_eq!(lines(&v, 1), ["a", "bXY", "c"]);
        // Enter splits the line; the caret goes to the new one.
        frame(
            &mut v,
            &ctx,
            vec![key(Key::Enter, Modifiers::NONE), text("Z")],
        );
        assert_eq!(lines(&v, 1), ["a", "bXY", "Z", "c"]);
        assert_eq!(v.caret().map(|c| c.pos), Some((2, 1)));
        // Several lines paste at the caret at once.
        frame(&mut v, &ctx, vec![egui::Event::Paste("1\n2\n3".into())]);
        assert_eq!(lines(&v, 1), ["a", "bXY", "Z1", "2", "3", "c"]);
        assert_eq!(v.caret().map(|c| c.pos), Some((4, 1)));
        // Backspace at a line start joins it to the line above.
        frame(&mut v, &ctx, vec![key(Key::Home, Modifiers::NONE)]);
        frame(&mut v, &ctx, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1), ["a", "bXY", "Z1", "23", "c"]);
        // Shift+arrows select across lines; copying gives the text, typing replaces it.
        v.place_caret(1, (1, 1), false);
        frame(&mut v, &ctx, vec![key(Key::ArrowDown, Modifiers::SHIFT)]);
        let out = frame(&mut v, &ctx, vec![egui::Event::Copy]);
        assert!(out
            .platform_output
            .commands
            .contains(&egui::OutputCommand::CopyText("XY\nZ".into())));
        assert_eq!(
            v.selection(),
            Some((1, 2)),
            "the rows follow, for copying across"
        );
        frame(&mut v, &ctx, vec![text("-")]);
        assert_eq!(lines(&v, 1), ["a", "b-1", "23", "c"]);
        assert_eq!(lines(&v, 0), ["a", "b", "c"], "only the caret's pane");
        // Cut without a selection takes the whole line.
        let out = frame(&mut v, &ctx, vec![egui::Event::Cut]);
        assert!(out
            .platform_output
            .commands
            .contains(&egui::OutputCommand::CopyText("b-1\n".into())));
        assert_eq!(lines(&v, 1), ["a", "23", "c"]);
        // Each action is one undo step; typing in a line is one step until the caret moves.
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "b-1", "23", "c"]);
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "bXY", "Z1", "23", "c"]);
        for _ in 0..4 {
            v.undo();
        }
        assert_eq!(lines(&v, 1), ["a", "bXY", "c"]);
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "b", "c"], "XY typed as one step");
        // Select all, then Escape ends the selection and a second Escape the caret.
        v.place_caret(1, (0, 0), false);
        frame(&mut v, &ctx, vec![key(Key::A, Modifiers::COMMAND)]);
        assert_eq!(v.caret_text().as_deref(), Some("a\nb\nc"));
        frame(&mut v, &ctx, vec![key(Key::Escape, Modifiers::NONE)]);
        assert!(v.caret().is_some() && v.caret_text().is_none());
        frame(&mut v, &ctx, vec![key(Key::Escape, Modifiers::NONE)]);
        assert!(v.caret().is_none());
    }

    #[test]
    fn clicks_place_the_caret_and_drags_select_text() {
        let ctx = egui::Context::default();
        crate::ui::fonts::setup(&ctx, "en");
        let text = "alpha beta\ngamma\ndelta\n";
        let mut v = two_way(text, text);
        frame(&mut v, &ctx, vec![]);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let click = |v: &mut TextView, p: egui::Pos2| {
            frame(v, &ctx, vec![egui::Event::PointerMoved(p), button(p, true)]);
            frame(v, &ctx, vec![button(p, false)]);
            frame(v, &ctx, vec![]);
            frame(v, &ctx, vec![]);
        };
        // Find a point in the first and third lines of the left pane.
        let mut at = [None; 3];
        for y in (0..500).step_by(3) {
            let p = pos2(400.0, y as f32);
            click(&mut v, p);
            if let Some(c) = v.caret().filter(|c| c.pane == 0 && c.pos.0 < 3) {
                at[c.pos.0].get_or_insert(p.y);
            }
        }
        let (Some(y0), Some(y2)) = (at[0], at[2]) else {
            panic!("lines not found: {at:?}");
        };
        // Find the x of each column by clicking along the line.
        let mut x_of = std::collections::HashMap::new();
        for x in (0..400).step_by(2) {
            click(&mut v, pos2(x as f32, y0 + 2.0));
            if let Some(c) = v.caret().filter(|c| c.pane == 0 && c.pos.0 == 0) {
                x_of.entry(c.pos.1).or_insert(x as f32);
            }
        }
        assert!((0..=10).all(|c| x_of.contains_key(&c)), "{x_of:?}");
        // A double-click selects the word.
        let p = pos2(x_of[&7] + 1.0, y0 + 2.0);
        frame(
            &mut v,
            &ctx,
            vec![egui::Event::PointerMoved(p), button(p, true)],
        );
        frame(&mut v, &ctx, vec![button(p, false)]);
        frame(&mut v, &ctx, vec![button(p, true), button(p, false)]);
        assert_eq!(v.caret_text().as_deref(), Some("beta"));
        // A drag selects from the press to the pointer, across lines.
        let (from, to) = (
            pos2(x_of[&2] + 1.0, y0 + 2.0),
            pos2(x_of[&3] + 1.0, y2 + 2.0),
        );
        frame(
            &mut v,
            &ctx,
            vec![egui::Event::PointerMoved(from), button(from, true)],
        );
        for k in 1..=4 {
            let p = from + (to - from) * (k as f32 / 4.0);
            frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)]);
        }
        frame(&mut v, &ctx, vec![button(to, false)]);
        assert_eq!(v.caret_text().as_deref(), Some("pha beta\ngamma\ndel"));
        // Clicking right of the text puts the caret at the line end, where typing goes.
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        click(&mut v, pos2(400.0, y0 + 2.0));
        assert_eq!(v.caret().map(|c| (c.pane, c.pos)), Some((0, (0, 10))));
        frame(&mut v, &ctx, vec![egui::Event::Text("!".into())]);
        assert_eq!(lines(&v, 0)[0], "alpha beta!");
    }

    /// The modifiers egui reports for a key on macOS (`mac`) or on Windows and Linux.
    fn mods(mac: bool, names: &str) -> Modifiers {
        let mut m = Modifiers::NONE;
        for n in names.split('+').filter(|n| !n.is_empty()) {
            match n {
                "shift" => m.shift = true,
                "alt" => m.alt = true,
                "ctrl" if mac => m.ctrl = true,
                "ctrl" => (m.ctrl, m.command) = (true, true),
                "cmd" => (m.mac_cmd, m.command) = (true, true),
                _ => panic!("{n}"),
            }
        }
        m
    }

    #[test]
    fn the_caret_keys_follow_each_platform_and_leave_the_difference_commands_alone() {
        use Key::*;
        // (platform, modifiers, key, whether the caret takes it)
        let both = [
            ("", ArrowLeft, true),
            ("shift", ArrowDown, true),
            ("", Home, true),
            ("shift", End, true),
            ("", PageUp, true),
            ("shift", PageDown, true),
            ("", Enter, true),
            ("", Tab, true),
            ("shift", Tab, true),
            ("", Escape, true),
            ("", Backspace, true),
            ("shift", Delete, true),
            // the difference commands
            ("alt", ArrowUp, false),
            ("alt", ArrowDown, false),
            ("alt", Home, false),
            ("alt", End, false),
            ("ctrl+alt", ArrowLeft, false),
            ("ctrl+alt", ArrowRight, false),
        ];
        let mac = [
            ("alt", ArrowLeft, true),
            ("alt+shift", ArrowRight, true),
            ("alt", Backspace, true),
            ("alt", Delete, true),
            ("cmd", ArrowLeft, true),
            ("cmd+shift", ArrowDown, true),
            ("cmd", A, true),
            ("cmd+shift", A, false),
            ("cmd", Backspace, false),
            ("cmd", Enter, false),
            ("cmd", Escape, false),
            ("cmd+alt", ArrowLeft, false),
            ("ctrl", A, false),
        ];
        let other = [
            ("ctrl", ArrowLeft, true),
            ("ctrl+shift", ArrowRight, true),
            ("ctrl", Backspace, true),
            ("ctrl", Delete, true),
            ("ctrl", Home, true),
            ("ctrl", A, true),
            ("ctrl+shift", A, false),
            ("ctrl", Enter, false),
            ("alt", ArrowLeft, false),
            ("alt", ArrowRight, false),
            ("alt", Backspace, false),
        ];
        for (is_mac, extra) in [(true, &mac[..]), (false, &other[..])] {
            for &(names, k, takes) in both.iter().chain(extra) {
                let m = mods(is_mac, names);
                assert_eq!(
                    TextView::caret_key_on(is_mac, k, m),
                    takes,
                    "{} {names}+{k:?}",
                    if is_mac { "macOS" } else { "Windows/Linux" }
                );
            }
        }
        // A shortcut binding sets `command` only, without `mac_cmd`: ⌘⌥→ stays Copy All.
        let binding = Modifiers::COMMAND | Modifiers::ALT;
        assert!(!TextView::caret_key_on(true, ArrowRight, binding));
        assert!(!TextView::caret_key_on(true, ArrowLeft, binding));
        // Only Option (macOS) or Ctrl (elsewhere) is the word key.
        assert!(word_key_on(true, mods(true, "alt+shift")));
        assert!(!word_key_on(true, mods(true, "ctrl")));
        assert!(!word_key_on(true, mods(true, "cmd+alt")));
        assert!(word_key_on(false, mods(false, "ctrl+shift")));
        assert!(!word_key_on(false, mods(false, "alt")));
        assert!(!word_key_on(false, mods(false, "ctrl+alt")));
    }

    #[test]
    fn the_caret_moves_and_deletes_by_each_platforms_keys() {
        use Key::*;
        let text = "  let foo_bar = x;\nnext\n";
        for is_mac in [true, false] {
            let mut v = two_way("x\n", text);
            let at = |v: &mut TextView, pos, names: &str, k: Key| {
                v.place_caret(1, pos, false);
                v.caret_key_pressed_on(is_mac, k, mods(is_mac, names));
                let c = v.caret().unwrap();
                (c.pos, v.caret_text())
            };
            let (word, line, doc) = if is_mac {
                ("alt", "cmd", "cmd")
            } else {
                ("ctrl", "", "ctrl")
            };
            let p = |l, b| ((l, b), None);
            let name = if is_mac { "macOS" } else { "Windows/Linux" };
            assert_eq!(at(&mut v, (0, 6), word, ArrowRight), p(0, 13), "{name}");
            assert_eq!(at(&mut v, (0, 13), word, ArrowLeft), p(0, 6), "{name}");
            assert_eq!(
                at(&mut v, (0, 6), &format!("{word}+shift"), ArrowRight)
                    .1
                    .as_deref(),
                Some("foo_bar"),
                "{name}"
            );
            // to the line's first character, then its start, and to its end
            let (start, end) = if is_mac {
                (ArrowLeft, ArrowRight)
            } else {
                (Home, End)
            };
            // (⌘← on macOS goes straight to the line start)
            let first = if is_mac { 0 } else { 2 };
            assert_eq!(at(&mut v, (0, 9), line, start), p(0, first), "{name}");
            assert_eq!(at(&mut v, (0, 2), line, start), p(0, 0), "{name}");
            assert_eq!(at(&mut v, (0, 9), line, end), p(0, 18), "{name}");
            // to the document's start and end
            let (top, bottom) = if is_mac {
                (ArrowUp, ArrowDown)
            } else {
                (Home, End)
            };
            assert_eq!(at(&mut v, (1, 2), doc, top), p(0, 0), "{name}");
            assert_eq!(at(&mut v, (0, 2), doc, bottom), p(1, 4), "{name}");
            // word deletes, each one undo step
            v.place_caret(1, (0, 13), false);
            v.caret_key_pressed_on(is_mac, Backspace, mods(is_mac, word));
            v.caret_key_pressed_on(is_mac, Delete, mods(is_mac, word));
            assert_eq!(lines(&v, 1)[0], "  let  x;", "{name}");
            v.undo();
            assert_eq!(lines(&v, 1)[0], "  let  = x;", "{name}");
            v.undo();
            assert_eq!(lines(&v, 1)[0], "  let foo_bar = x;", "{name}");
            // the other platform's word key is not one here
            let other = if is_mac { "ctrl" } else { "alt" };
            assert_eq!(at(&mut v, (0, 6), other, ArrowRight), p(0, 7), "{name}");
            assert_eq!(lines(&v, 0), ["x"], "{name}: the other pane is untouched");
        }
    }

    #[test]
    fn arrows_and_deletes_keep_accents_emoji_and_wide_characters_whole() {
        let ctx = egui::Context::default();
        let line = "e\u{301}🇹🇼👩\u{200d}💻中\tz";
        let mut v = two_way("x\n", &format!("{line}\n"));
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (0, 0), false);
        let mut stops = vec![0];
        for _ in 0..7 {
            frame(&mut v, &ctx, vec![key(Key::ArrowRight, Modifiers::NONE)]);
            stops.push(v.caret().unwrap().pos.1);
        }
        assert_eq!(stops, [0, 3, 11, 22, 25, 26, 27, 27]);
        stops.dedup();
        for &b in stops.iter().rev().skip(1) {
            frame(&mut v, &ctx, vec![key(Key::ArrowLeft, Modifiers::NONE)]);
            assert_eq!(v.caret().unwrap().pos.1, b);
        }
        // Up and Down, and a click, never land inside a character.
        v.place_caret(1, (0, 5), false);
        assert_eq!(v.caret().unwrap().pos, (0, 3));
        // Backspace and Delete take whole characters.
        v.place_caret(1, (0, 22), false);
        frame(&mut v, &ctx, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1)[0], "e\u{301}🇹🇼中\tz");
        v.place_caret(1, (0, 0), false);
        frame(&mut v, &ctx, vec![key(Key::Delete, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1)[0], "🇹🇼中\tz");
        // Typed and pasted text of any script goes in whole.
        frame(
            &mut v,
            &ctx,
            vec![
                egui::Event::Text("é".into()),
                egui::Event::Paste("한글\u{1f469}\u{200d}\u{1f52c}".into()),
            ],
        );
        assert_eq!(lines(&v, 1)[0], "é한글👩\u{200d}🔬🇹🇼中\tz");
        assert_eq!(v.caret().unwrap().pos, (0, 2 + 6 + 11));
        v.undo();
        v.undo();
        v.undo();
        v.undo();
        assert_eq!(lines(&v, 1)[0], line);
    }

    #[test]
    fn clicks_land_between_characters_with_tabs_wide_characters_and_wrapping() {
        let ctx = egui::Context::default();
        crate::ui::fonts::setup(&ctx, "en");
        let mixed = "a\tb中文e\u{301}x";
        let long = "word ".repeat(60);
        let mut v = two_way(&format!("{mixed}\n{long}\n"), "x\n");
        v.wrap = true;
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let click = |v: &mut TextView, p: egui::Pos2| {
            frame(v, &ctx, vec![egui::Event::PointerMoved(p), button(p, true)]);
            frame(v, &ctx, vec![button(p, false)]);
            frame(v, &ctx, vec![]);
            frame(v, &ctx, vec![]);
            v.caret().filter(|c| c.pane == 0).map(|c| c.pos)
        };
        // The y of each display line of the left pane, and the x where its text starts.
        let mut ys: Vec<(edit::Pos, f32)> = Vec::new();
        for y in (0..300).step_by(2) {
            if let Some(pos) = click(&mut v, pos2(400.0, y as f32)) {
                if ys.last().is_none_or(|&(p, _)| p != pos) {
                    ys.push((pos, y as f32 + 1.0));
                }
            }
        }
        let y0 = ys.iter().find(|(p, _)| p.0 == 0).unwrap().1;
        // Just left of where a click moves past the first character: inside it.
        let left = (0..300)
            .map(|x| x as f32)
            .find(|&x| click(&mut v, pos2(x, y0)) == Some((0, 1)))
            .unwrap()
            - 2.0;
        assert_eq!(click(&mut v, pos2(left, y0)), Some((0, 0)));
        // Along the mixed line every place is a character boundary, in order, and every
        // boundary can be clicked.
        let mut seen = Vec::new();
        for x in (left as usize..400).step_by(1) {
            let pos = click(&mut v, pos2(x as f32, y0)).unwrap();
            assert_eq!(pos.0, 0);
            if seen.last() != Some(&pos.1) {
                assert!(
                    seen.last().is_none_or(|&b| b < pos.1),
                    "{seen:?} then {pos:?}"
                );
                seen.push(pos.1);
            }
        }
        assert_eq!(seen, [0, 1, 2, 3, 6, 9, 12, 13]);
        // On the wrapped line, a click at the left of each display line is at its wrap start.
        let (cols, tab) = (v.layout.key.unwrap().0, v.layout.key.unwrap().1);
        assert!(cols > 0);
        let starts = wrap_starts(&long, tab, cols);
        assert!(starts.len() >= 3, "{starts:?}");
        let rows: Vec<f32> = ys
            .iter()
            .filter(|(p, _)| p.0 == 1)
            .map(|&(_, y)| y)
            .collect();
        assert_eq!(rows.len(), starts.len());
        for (k, (&y, &start)) in rows.iter().zip(&starts).enumerate() {
            assert_eq!(click(&mut v, pos2(left, y)), Some((1, start)), "row {k}");
            // Right of the text, the caret stays on the display line.
            let end = starts.get(k + 1).map_or(long.len(), |&next| next - 1);
            assert_eq!(click(&mut v, pos2(400.0, y)), Some((1, end)), "row {k}");
        }
    }

    #[test]
    fn an_input_method_composes_over_a_selection_and_can_give_up() {
        let ctx = egui::Context::default();
        let mut v = two_way("hello world\n", "hello world\n");
        frame(&mut v, &ctx, vec![]);
        let pre = |t: &str| {
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: t.into(),
                active_range_chars: None,
            })
        };
        let commit = |t: &str| egui::Event::Ime(egui::ImeEvent::Commit(t.into()));
        let select = |v: &mut TextView| {
            v.place_caret(1, (0, 0), false);
            v.place_caret(1, (0, 5), true);
        };
        let preedit = |v: &TextView| v.caret.as_ref().map(|c| c.preedit.clone());
        select(&mut v);
        frame(&mut v, &ctx, vec![pre("n"), pre("ni")]);
        // Composing shows the text but changes nothing, and the selection stays.
        assert_eq!(preedit(&v).as_deref(), Some("ni"));
        assert_eq!(lines(&v, 1), ["hello world"]);
        assert_eq!(v.caret_text().as_deref(), Some("hello"));
        // The differences are found again meanwhile (the other side changed).
        v.panes[0].doc.lines[0] = "hello there".into();
        v.mark_changed(&[0]);
        assert_eq!(preedit(&v).as_deref(), Some("ni"));
        assert_eq!(v.caret_text().as_deref(), Some("hello"));
        // The result replaces the selection, as one undo step.
        frame(&mut v, &ctx, vec![commit("你好")]);
        assert_eq!(lines(&v, 1), ["你好 world"]);
        assert_eq!(v.caret().unwrap().pos, (0, 6));
        assert_eq!(preedit(&v).as_deref(), Some(""));
        v.undo();
        assert_eq!(lines(&v, 1), ["hello world"]);
        // Given up (an empty preedit), or ended with a bare Enter: nothing is typed.
        for end in [pre(""), commit("\n")] {
            select(&mut v);
            frame(&mut v, &ctx, vec![pre("x")]);
            frame(&mut v, &ctx, vec![end]);
            assert_eq!(preedit(&v).as_deref(), Some(""));
            assert_eq!(lines(&v, 1), ["hello world"]);
            assert_eq!(v.caret_text().as_deref(), Some("hello"));
        }
        assert_eq!(lines(&v, 0), ["hello there"], "only the caret's pane");
    }

    #[test]
    fn ime_preedit_and_commit_keep_the_selection_while_a_diff_is_pending() {
        for layout in [TextLayout::Aligned, TextLayout::Compact] {
            for cancel in [false, true] {
                let ctx = egui::Context::default();
                let mut v = two_way("alpha tail\n", "alpha tail\n");
                v.text_layout = layout;
                frame(&mut v, &ctx, vec![]);
                v.place_caret(1, (0, 10), false);
                v.rediff_ms = 100.0;
                frame(&mut v, &ctx, vec![egui::Event::Text("!".into())]);
                assert!(v.rediff_due.is_some());
                // Select backwards while that comparison is pending.
                v.place_caret(1, (0, 5), false);
                v.place_caret(1, (0, 0), true);
                let selected = v.caret().unwrap();
                let undo_steps = v.undo.len();
                let pre = |t: &str| {
                    egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: t.into(),
                        active_range_chars: None,
                    })
                };
                let out = frame(&mut v, &ctx, vec![pre("n"), pre("ni")]);
                assert!(
                    out.platform_output.ime.is_some(),
                    "the native backend is told to allow IME"
                );
                assert!(v.rediff_due.is_some());
                assert_eq!(v.caret(), Some(selected));
                assert_eq!(v.undo.len(), undo_steps);
                assert_eq!(lines(&v, 1), ["alpha tail!"]);
                let end = if cancel {
                    pre("")
                } else {
                    egui::Event::Ime(egui::ImeEvent::Commit("你好 e\u{301} 👩\u{200d}💻".into()))
                };
                frame(&mut v, &ctx, vec![end]);
                v.settle_diff();
                assert!(v.rediff_due.is_none());
                if cancel {
                    assert_eq!(lines(&v, 1), ["alpha tail!"]);
                    assert_eq!(v.caret(), Some(selected));
                    assert_eq!(v.undo.len(), undo_steps);
                } else {
                    assert_eq!(lines(&v, 1), ["你好 e\u{301} 👩\u{200d}💻 tail!"]);
                    assert_eq!(v.undo.len(), undo_steps + 1);
                    v.undo();
                    assert_eq!(lines(&v, 1), ["alpha tail!"]);
                    v.redo();
                    assert_eq!(lines(&v, 1), ["你好 e\u{301} 👩\u{200d}💻 tail!"]);
                }
                assert_eq!(lines(&v, 0), ["alpha tail"]);
            }
        }
    }

    #[test]
    fn edits_keep_the_shape_of_blank_unterminated_mixed_long_and_large_files() {
        let ctx = &egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let edit = |path: &std::path::Path, f: &dyn Fn(&mut TextView)| {
            let docs = vec![
                TextDoc::from_text(None, "x\n"),
                text::load_doc(path, None).unwrap(),
            ];
            let mut v = TextView::new(docs, &Config::default());
            frame(&mut v, ctx, vec![]);
            f(&mut v);
            v.save_pane(1).unwrap();
            std::fs::read(path).unwrap()
        };
        let typing = |pos: edit::Pos, text: &'static str| {
            move |v: &mut TextView| {
                v.place_caret(1, pos, false);
                frame(v, ctx, vec![egui::Event::Text(text.into())]);
            }
        };
        // A blank file takes typing; a file without a final newline keeps it that way.
        let blank = file("blank.txt", b"");
        // (a blank file has no line ending to keep, and gets the usual final newline)
        assert_eq!(
            edit(&blank, &typing((0, 0), "hi")),
            format!("hi{}", Eol::platform().as_str()).as_bytes()
        );
        let open = file("open.txt", b"a\nb");
        assert_eq!(edit(&open, &typing((1, 1), "c")), b"a\nbc");
        // Mixed line endings are written as the file's usual one, as Save always has; Enter
        // uses the file's line ending.
        let mixed = file("mixed.txt", b"a\r\nb\nc\r\n");
        assert_eq!(edit(&mixed, &typing((1, 1), "X")), b"a\r\nbX\r\nc\r\n");
        let crlf = file("crlf.txt", b"a\r\nb\r\n");
        let enter = |v: &mut TextView| {
            v.place_caret(1, (0, 1), false);
            frame(v, ctx, vec![key(Key::Enter, Modifiers::NONE)]);
        };
        assert_eq!(edit(&crlf, &enter), b"a\r\n\r\nb\r\n");
        // A long line: End reaches its end, and typing there keeps the rest.
        let long = "a".repeat(200_000);
        let path = file("long.txt", format!("{long}\n").as_bytes());
        let out = edit(&path, &|v: &mut TextView| {
            v.place_caret(1, (0, 7), false);
            frame(v, ctx, vec![key(Key::End, Modifiers::NONE)]);
            assert_eq!(v.caret().unwrap().pos, (0, 200_000));
            frame(v, ctx, vec![egui::Event::Text("!".into())]);
        });
        assert_eq!(out, format!("{long}!\n").as_bytes());
        // A large file: the end of the document, an edit and a page up.
        let big: String = (0..30_000).map(|i| format!("line {i}\n")).collect();
        let path = file("big.txt", big.as_bytes());
        let to_end = mods(MAC, if MAC { "cmd" } else { "ctrl" });
        let out = edit(&path, &|v: &mut TextView| {
            v.place_caret(1, (5, 0), false);
            let end = if MAC { Key::ArrowDown } else { Key::End };
            frame(v, ctx, vec![key(end, to_end)]);
            assert_eq!(v.caret().unwrap().pos, (29_999, 10));
            v.place_caret(1, (29_999, 0), false);
            frame(v, ctx, vec![egui::Event::Text("#".into())]);
            frame(v, ctx, vec![key(Key::PageUp, Modifiers::NONE)]);
            assert!(v.caret().unwrap().pos.0 < 29_999);
            assert_eq!(lines(v, 1)[29_999], "#line 29999");
        });
        assert_eq!(out, big.replacen("line 29999", "#line 29999", 1).as_bytes());
    }

    #[test]
    fn home_end_and_the_vertical_keys_follow_wrapped_display_lines_and_keep_the_selection() {
        use Key::*;
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let wide = "中文中文中文中文中文中文中文中文";
        let text = format!("{long}\nshort\n{wide}\n");
        let mut v = two_way("x\n", &text);
        let tab = v.tab_size;
        v.ensure_layout(20, tab);
        v.page_rows = 2;
        let starts = wrap_starts(long, tab, 20);
        assert!(starts.len() >= 3, "{starts:?}");
        // the bytes each display line of the long line shows
        let span = |k: usize| {
            let a = wrap_byte(long, tab, starts[k]);
            let b = starts.get(k + 1).map_or(long.len(), |&n| {
                edit::char_before(long, wrap_byte(long, tab, n))
            });
            (a, b)
        };
        let last = starts.len() - 1;
        for mac in [true, false] {
            let name = if mac { "macOS" } else { "Windows/Linux" };
            let press = |v: &mut TextView, names: &str, k: Key| {
                v.caret_key_pressed_on(mac, k, mods(mac, names));
                v.caret().unwrap().pos
            };
            let (home, end, line) = if mac {
                (ArrowLeft, ArrowRight, "cmd")
            } else {
                (Home, End, "")
            };
            // End: the display line's end, then the line's; Home: its start, then the line's.
            v.place_caret(1, (0, 2), false);
            assert_eq!(press(&mut v, line, end), (0, span(0).1), "{name}");
            assert_eq!(press(&mut v, line, end), (0, long.len()), "{name}");
            let shift = format!("{line}+shift").trim_start_matches('+').to_string();
            assert_eq!(press(&mut v, &shift, home), (0, span(last).0), "{name}");
            assert_eq!(
                v.caret_text().as_deref(),
                Some(&long[span(last).0..]),
                "{name}"
            );
            assert_eq!(press(&mut v, &shift, home), (0, 0), "{name}");
            assert_eq!(
                v.caret_text().as_deref(),
                Some(long),
                "{name}: one selection"
            );
            // Shift+End from the middle of a display line selects to its end only.
            v.place_caret(1, (0, span(1).0 + 2), false);
            press(&mut v, &shift, end);
            assert_eq!(
                v.caret_text().as_deref(),
                Some(&long[span(1).0 + 2..span(1).1]),
                "{name}"
            );
        }
        // Down and Up go through the display lines, keeping the column.
        let down = |v: &mut TextView, names: &str, k: Key| {
            v.caret_key_pressed_on(MAC, k, mods(MAC, names));
            v.caret().unwrap().pos
        };
        v.place_caret(1, (0, 3), false);
        for k in 1..=last {
            assert_eq!(
                down(&mut v, "", ArrowDown),
                (0, span(k).0 + 3),
                "display line {k}"
            );
        }
        // on to the short line (its end), and back to the same column on the wide line
        assert_eq!(down(&mut v, "", ArrowDown), (1, 3));
        v.place_caret(1, (0, 8), false);
        for _ in 0..last {
            down(&mut v, "", ArrowDown);
        }
        assert_eq!(down(&mut v, "", ArrowDown), (1, 5));
        // 8 columns are 4 wide characters (12 bytes); the second display line starts at 20
        let wide_starts = wrap_starts(wide, tab, 20);
        assert_eq!(wide_starts, [0, 20]);
        assert_eq!(down(&mut v, "", ArrowDown), (2, 12));
        assert_eq!(down(&mut v, "", ArrowDown), (2, 30 + 12));
        assert_eq!(
            down(&mut v, "", ArrowDown),
            (2, wide.len()),
            "past the last line"
        );
        assert_eq!(
            down(&mut v, "", ArrowUp),
            (2, 12),
            "the column is kept past the end"
        );
        // Shift+PageDown and Shift+PageUp move two display lines and keep one selection.
        v.place_caret(1, (0, 3), false);
        assert_eq!(down(&mut v, "shift", PageDown), (0, span(2).0 + 3));
        assert_eq!(v.caret_text().as_deref(), Some(&long[3..span(2).0 + 3]));
        assert_eq!(down(&mut v, "shift", PageUp), (0, 3));
        assert_eq!(v.caret_text(), None);
        assert_eq!(down(&mut v, "shift", PageUp), (0, 0), "past the first line");
        assert_eq!(v.caret_text().as_deref(), Some("alp"));
        // Without wrapping, the keys go by whole lines as before.
        v.ensure_layout(0, tab);
        v.place_caret(1, (0, 3), false);
        assert_eq!(down(&mut v, "", ArrowDown), (1, 3));
        v.place_caret(1, (0, 3), false);
        let end = if MAC { ArrowRight } else { End };
        assert_eq!(
            down(&mut v, if MAC { "cmd" } else { "" }, end),
            (0, long.len())
        );
        assert_eq!(lines(&v, 0), ["x"]);
    }

    #[test]
    fn the_view_follows_the_caret_down_a_long_wrapped_line() {
        let ctx = egui::Context::default();
        crate::ui::fonts::setup(&ctx, "en");
        let long = "word ".repeat(600);
        let mut v = two_way(&format!("{long}\nend\n"), "x\n");
        v.wrap = true;
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        v.place_caret(0, (0, 0), false);
        let visible = |v: &TextView| {
            let c = v.caret().unwrap();
            let line = v.layout.row_line[v.diff.row_of_line(0, c.pos.0)] + v.caret_segment(&c);
            (
                line as f32 >= v.scroll.floor(),
                (line as f32) < v.scroll + v.page_rows as f32,
                line,
            )
        };
        let segs = v.wrap_starts_of(0, 0).len();
        assert!(
            segs > v.page_rows + 5,
            "{segs} display lines, {} on a page",
            v.page_rows
        );
        for _ in 0..segs - 1 {
            frame(&mut v, &ctx, vec![key(Key::ArrowDown, Modifiers::NONE)]);
            frame(&mut v, &ctx, vec![]);
            let (above, below, line) = visible(&v);
            assert!(above && below, "display line {line} at scroll {}", v.scroll);
        }
        assert_eq!(v.caret().unwrap().pos.0, 0, "still on the long line");
        frame(&mut v, &ctx, vec![key(Key::PageUp, Modifiers::NONE)]);
        frame(&mut v, &ctx, vec![]);
        let (above, below, line) = visible(&v);
        assert!(above && below, "display line {line} at scroll {}", v.scroll);
    }

    #[test]
    fn tab_and_shift_tab_indent_the_selected_lines_and_keep_the_selection() {
        let ctx = egui::Context::default();
        let text = "fn a() {\nx = 1;\n\ny = 2;\n}\n";
        let mut v = two_way("other\n", text);
        v.tab_size = 4;
        frame(&mut v, &ctx, vec![]);
        let select = |v: &mut TextView, a: edit::Pos, b: edit::Pos| {
            v.place_caret(1, a, false);
            v.place_caret(1, b, true);
        };
        let tab = |v: &mut TextView, m: Modifiers| frame(v, &ctx, vec![key(Key::Tab, m)]);
        let ends = |v: &TextView| {
            let c = v.caret().unwrap();
            (c.anchor, c.pos)
        };
        // Several lines: each is indented by the tab size in spaces (the file has no tabs), the
        // blank line is left alone, and the selection moves with its text.
        select(&mut v, (1, 2), (3, 3));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(
            lines(&v, 1),
            ["fn a() {", "    x = 1;", "", "    y = 2;", "}"]
        );
        assert_eq!(ends(&v), ((1, 6), (3, 7)));
        assert_eq!(v.caret_text().as_deref(), Some("= 1;\n\n    y ="));
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1), lines_of(text));
        assert_eq!(
            ends(&v),
            ((1, 2), (3, 3)),
            "outdenting gives back the selection"
        );
        // A backward selection keeps its direction.
        select(&mut v, (3, 3), (1, 2));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(ends(&v), ((3, 7), (1, 6)));
        v.undo();
        assert_eq!(lines(&v, 1), lines_of(text), "one undo step");
        // Whole lines (ending at the start of the next one) keep starting at column 0, and the
        // line after them is not indented.
        select(&mut v, (1, 0), (3, 0));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(lines(&v, 1)[1..4], ["    x = 1;", "", "y = 2;"]);
        assert_eq!(v.caret_text().as_deref(), Some("    x = 1;\n\n"));
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1), lines_of(text));
        // One whole line is indented too; part of a line, or no selection, types a tab.
        select(&mut v, (1, 0), (1, 6));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(lines(&v, 1)[1], "    x = 1;");
        assert_eq!(v.caret_text().as_deref(), Some("    x = 1;"));
        v.undo();
        select(&mut v, (1, 0), (1, 1));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(lines(&v, 1)[1], "\t = 1;");
        v.place_caret(1, (4, 0), false);
        tab(&mut v, Modifiers::NONE);
        assert_eq!(lines(&v, 1)[4], "\t}");
        v.undo();
        v.undo();
        // Shift+Tab with a plain caret outdents its line by up to the tab size, and the caret
        // stays with its text; a shorter indent goes entirely; no indent changes nothing.
        let mut v = two_way("other\n", "      z\n  w\nq\n \tm\n");
        v.tab_size = 4;
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (0, 7), false);
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!((lines(&v, 1)[0], ends(&v).1), ("  z", (0, 3)));
        v.place_caret(1, (1, 1), false);
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!((lines(&v, 1)[1], ends(&v).1), ("w", (1, 0)));
        v.place_caret(1, (2, 1), false);
        let steps = v.undo.len();
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1)[2], "q");
        assert_eq!(v.undo.len(), steps, "no undo step for no change");
        // Mixed: a space before a tab is removed first, then the tab.
        v.place_caret(1, (3, 3), false);
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1)[3], "\tm");
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1)[3], "m");
        assert_eq!(lines(&v, 0), ["other"], "only the caret's pane");
        // A file indented with tabs is indented with tabs, by Tab and by Ctrl+] alike.
        let mut v = two_way("other\n", "\ta\n\tb\nc\n");
        frame(&mut v, &ctx, vec![]);
        select(&mut v, (0, 1), (2, 1));
        tab(&mut v, Modifiers::NONE);
        assert_eq!(lines(&v, 1), ["\t\ta", "\t\tb", "\tc"]);
        v.active_pane = 1;
        v.indent(true);
        assert_eq!(lines(&v, 1), ["\ta", "\tb", "c"]);
        assert_eq!(
            ends(&v),
            ((0, 1), (2, 1)),
            "Ctrl+[ outdents the caret's lines"
        );
        // A read-only pane is not changed.
        v.panes[1].doc.read_only = true;
        select(&mut v, (0, 1), (2, 1));
        tab(&mut v, Modifiers::NONE);
        tab(&mut v, Modifiers::SHIFT);
        assert_eq!(lines(&v, 1), ["\ta", "\tb", "c"]);
    }

    fn lines_of(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn triple_clicks_drags_and_right_clicks_select_the_same_text_in_both_layouts() {
        for layout in [TextLayout::Compact, TextLayout::Aligned] {
            let ctx = egui::Context::default();
            crate::ui::fonts::setup(&ctx, "en");
            let mut v = two_way(
                "alpha beta\ngamma\ndelta\n",
                "alpha beta\nnew line\ngamma\ndelta\n",
            );
            v.text_layout = layout;
            frame(&mut v, &ctx, vec![]);
            frame(&mut v, &ctx, vec![]);
            let button = |pos, pressed, secondary: bool| egui::Event::PointerButton {
                pos,
                button: if secondary {
                    egui::PointerButton::Secondary
                } else {
                    egui::PointerButton::Primary
                },
                pressed,
                modifiers: Modifiers::NONE,
            };
            let settle = |v: &mut TextView| {
                frame(v, &ctx, vec![]);
                frame(v, &ctx, vec![]);
            };
            let click = |v: &mut TextView, p: egui::Pos2| {
                frame(
                    v,
                    &ctx,
                    vec![egui::Event::PointerMoved(p), button(p, true, false)],
                );
                frame(v, &ctx, vec![button(p, false, false)]);
                settle(v);
                v.caret().map(|c| (c.pane, c.pos))
            };
            // The y of each line of each pane, and an x inside each pane's text.
            let mut ys = std::collections::HashMap::new();
            // (below the pane headers, and clear of the right pane's scroll bar at its edge)
            for (pane, x) in [(0, 400.0), (1, 650.0)] {
                for y in (90..300).step_by(2) {
                    if let Some((p, pos)) = click(&mut v, pos2(x, y as f32)) {
                        if p == pane {
                            ys.entry((pane, pos.0)).or_insert(y as f32 + 2.0);
                        }
                    }
                }
            }
            let y = |pane: usize, line: usize| ys[&(pane, line)];
            let name = format!("{layout:?}");
            // A triple-click selects the line with its line break, the last one to its end.
            let p = pos2(650.0, y(1, 1));
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(p), button(p, true, false)],
            );
            frame(&mut v, &ctx, vec![button(p, false, false)]);
            frame(
                &mut v,
                &ctx,
                vec![button(p, true, false), button(p, false, false)],
            );
            assert_eq!(
                v.caret_text().as_deref(),
                Some("line"),
                "{name}: a double-click, a word"
            );
            frame(
                &mut v,
                &ctx,
                vec![button(p, true, false), button(p, false, false)],
            );
            assert_eq!(v.caret_text().as_deref(), Some("new line\n"), "{name}");
            // Its row is the selected one: not the next, where the selection ends at column 0.
            let row = v.diff.row_of_line(1, 1);
            assert_eq!((v.cursor_row, v.selection()), (Some(row), None), "{name}");
            // the same as the keys select
            let mouse = v.caret().map(|c| (c.anchor, c.pos));
            v.place_caret(1, (1, 4), false);
            v.caret_key_pressed_on(MAC, Key::Home, Modifiers::NONE);
            v.caret_key_pressed_on(MAC, Key::ArrowDown, Modifiers::SHIFT);
            assert_eq!(v.caret().map(|c| (c.anchor, c.pos)), mouse, "{name}");
            settle(&mut v);
            for _ in 0..4 {
                settle(&mut v);
            }
            let p = pos2(650.0, y(1, 3));
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(p), button(p, true, false)],
            );
            frame(&mut v, &ctx, vec![button(p, false, false)]);
            frame(
                &mut v,
                &ctx,
                vec![button(p, true, false), button(p, false, false)],
            );
            frame(
                &mut v,
                &ctx,
                vec![button(p, true, false), button(p, false, false)],
            );
            assert_eq!(
                v.caret_text().as_deref(),
                Some("delta"),
                "{name}: the last line"
            );
            for _ in 0..4 {
                settle(&mut v);
            }
            // A drag across a line the other side does not have selects its text in order.
            let (from, to) = (pos2(650.0, y(1, 0)), pos2(650.0, y(1, 2)));
            click(&mut v, pos2(650.0, y(1, 3)));
            for _ in 0..4 {
                settle(&mut v);
            }
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(from), button(from, true, false)],
            );
            for k in 1..=4 {
                let p = from + (to - from) * (k as f32 / 4.0);
                frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)]);
            }
            frame(&mut v, &ctx, vec![button(to, false, false)]);
            let text = v.caret_text().unwrap();
            assert!(
                text.ends_with("\nnew line\ngam") || text.ends_with("\nnew line\ngamma"),
                "{name}: {text:?}"
            );
            let (a, b) = v.caret().unwrap().range();
            assert_eq!((a.0, b.0), (0, 2), "{name}");
            // A right-click in the selection keeps it, and Copy text copies just it.
            settle(&mut v);
            let r = v.diff.row_of_line(1, 1);
            let inside = pos2(650.0, y(1, 1));
            frame(
                &mut v,
                &ctx,
                vec![
                    egui::Event::PointerMoved(inside),
                    button(inside, true, true),
                ],
            );
            frame(&mut v, &ctx, vec![button(inside, false, true)]);
            assert_eq!(v.caret_text().as_deref(), Some(text.as_str()), "{name}");
            assert_eq!(v.ctx_target, Some((r, 1)));
            assert_eq!(
                v.menu_copy_text(r, 1).as_deref(),
                Some(text.as_str()),
                "{name}"
            );
            // Outside the selection, it moves the caret there first. (A click closes the menu
            // first, as a click outside an open menu only closes it.)
            let outside = pos2(650.0, y(1, 3));
            click(&mut v, pos2(650.0, y(1, 0)));
            for _ in 0..4 {
                settle(&mut v);
            }
            v.place_caret(1, (0, 10), false);
            v.place_caret(1, (2, 5), true);
            for _ in 0..4 {
                settle(&mut v);
            }
            frame(
                &mut v,
                &ctx,
                vec![
                    egui::Event::PointerMoved(outside),
                    button(outside, true, true),
                ],
            );
            frame(&mut v, &ctx, vec![button(outside, false, true)]);
            let c = v.caret().unwrap();
            assert!(!c.has_selection() && c.pos.0 == 3, "{name}: {c:?}");
            assert_eq!(
                v.menu_copy_text(v.diff.row_of_line(1, 3), 1).as_deref(),
                Some("delta")
            );
            assert_eq!(lines(&v, 1), ["alpha beta", "new line", "gamma", "delta"]);
        }
    }

    #[test]
    fn a_drag_past_the_bottom_scrolls_and_keeps_selecting() {
        for layout in [TextLayout::Aligned, TextLayout::Compact] {
            let ctx = egui::Context::default();
            crate::ui::fonts::setup(&ctx, "en");
            let text: String = (0..300)
                .map(|i| format!("line {i} 中文 e\u{301} 👩\u{200d}💻\n"))
                .collect();
            let other: String = (0..300)
                .map(|i| {
                    if i % 7 == 0 {
                        format!("changed {i} 中文 e\u{301} 👩\u{200d}💻\n")
                    } else {
                        format!("line {i} 中文 e\u{301} 👩\u{200d}💻\n")
                    }
                })
                .collect();
            let mut v = two_way(&text, &other);
            v.text_layout = layout;
            frame(&mut v, &ctx, vec![]);
            frame(&mut v, &ctx, vec![]);
            let press = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            // Find the first line, then drag from it to below the window and hold there.
            let start = (0..200)
                .step_by(2)
                .map(|y| pos2(400.0, y as f32))
                .find(|&p| {
                    frame(
                        &mut v,
                        &ctx,
                        vec![egui::Event::PointerMoved(p), press(p, true)],
                    );
                    frame(&mut v, &ctx, vec![press(p, false)]);
                    frame(&mut v, &ctx, vec![]);
                    frame(&mut v, &ctx, vec![]);
                    v.caret().is_some_and(|c| c.pane == 0 && c.pos.0 == 0)
                })
                .unwrap();
            for _ in 0..4 {
                frame(&mut v, &ctx, vec![]);
            }
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(start), press(start, true)],
            );
            let below = pos2(400.0, 560.0);
            for k in 1..=5 {
                let p = start + (below - start) * (k as f32 / 5.0);
                frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)]);
            }
            let first = v.caret().unwrap().pos.0;
            for _ in 0..40 {
                frame(&mut v, &ctx, vec![]);
            }
            let c = v.caret().unwrap();
            assert!(v.scroll > 5.0, "scrolled: {}", v.scroll);
            assert!(
                c.pos.0 > first + 5,
                "kept selecting: {first} then {:?}",
                c.pos
            );
            // Pressed right of the first line, so selection starts at its end.
            assert_eq!(c.anchor, (0, v.panes[0].doc.lines[0].len()));
            frame(&mut v, &ctx, vec![press(below, false)]);
            let c = v.caret().unwrap();
            let selected = v.caret_text().unwrap();
            let offset = |p: edit::Pos| {
                v.panes[0].doc.lines[..p.0]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    + p.1
            };
            let expected = &text[offset(c.anchor)..offset(c.pos)];
            assert_eq!(selected, expected);
            assert!(c.pos.0 >= 14, "drag crossed at least two differences");
            let output = frame(&mut v, &ctx, vec![egui::Event::Copy]);
            assert!(output
                .platform_output
                .commands
                .contains(&egui::OutputCommand::CopyText(expected.into())));
            // Round-trip the clipboard through the other pane, undo/redo and saving.
            v.place_caret(1, (0, 0), false);
            frame(&mut v, &ctx, vec![egui::Event::Paste(selected)]);
            let pasted = format!("{expected}{other}");
            assert_eq!(v.panes[1].doc.text(), pasted);
            v.undo();
            assert_eq!(v.panes[1].doc.text(), other);
            v.redo();
            assert_eq!(v.panes[1].doc.text(), pasted);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("pasted.txt");
            v.panes[1].doc.path = Some(path.clone());
            v.save_pane(1).unwrap();
            assert_eq!(std::fs::read(path).unwrap(), pasted.as_bytes());
            assert_eq!(v.panes[0].doc.text(), text);
        }
    }

    #[test]
    fn the_status_says_whether_the_active_pane_is_writable_and_modified() {
        let mut v = two_way("a\n", "a\n");
        v.panes[0].doc.read_only = true;
        v.active_pane = 0;
        let read_only = format!("{}: {}", side_name(0, 2), tr("label.read_only"));
        assert!(
            v.status_detail().contains(&read_only),
            "{}",
            v.status_detail()
        );
        v.place_caret(1, (0, 1), false);
        v.caret_insert("b");
        let s = v.status_detail();
        let writable = format!(
            "{}: {}, {}",
            side_name(1, 2),
            tr("label.writable"),
            tr("label.modified")
        );
        assert!(s.contains(&writable), "{s}");
        // A file the system will not let us write says so, though the pane can be edited.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.txt");
        std::fs::write(&path, "a\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        v.panes[1].doc.path = Some(path.clone());
        assert!(v.status_detail().contains(tr("label.read_only_disk")));
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    #[test]
    fn saving_over_a_file_read_only_on_disk_is_refused_and_keeps_the_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.txt");
        std::fs::write(&path, "a\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        let mut v = two_way("a\n", "a\n");
        v.panes[1].doc.path = Some(path.clone());
        v.active_pane = 1;
        v.place_caret(1, (0, 1), false);
        v.caret_insert("b");
        let err = v.save_pane(1).unwrap_err().to_string();
        assert_eq!(
            err,
            trf("msg.read_only_disk", &[("file", "locked.txt".into())])
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n");
        assert!(v.panes[1].doc.modified);
        assert_eq!(lines(&v, 1), ["ab"]);
        // Once the file may be written again, saving works.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(v.save_pane(1).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ab\n");
        assert!(!v.panes[1].doc.modified);
    }

    #[test]
    fn a_large_oldest_undo_step_dropped_by_the_step_cap_drops_no_other() {
        // 201 steps: the oldest, a whole large file, goes for the 200-step cap; the rest
        // hold far less than the line budget and must all stay.
        let mut sizes = vec![4_900_000];
        sizes.extend(std::iter::repeat_n(1_500, 199));
        sizes.push(10);
        assert_eq!(undo_overflow(&sizes), 1);
        // Over the line budget, the oldest go until it fits, but never the last ten.
        let mut sizes = vec![3_000_000, 3_000_000];
        sizes.extend([10; 11]);
        assert_eq!(undo_overflow(&sizes), 1);
        assert_eq!(undo_overflow(&[6_000_000; 12]), 2);
        assert_eq!(undo_overflow(&[3_000_000, 3_000_000, 10]), 0);
        assert_eq!(undo_overflow(&[]), 0);
    }

    #[test]
    fn compare_only_text_says_so_and_stays_locked() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\n", "a\n");
        v.panes[0].doc.read_only = true;
        v.panes[0].doc.compare_only = true;
        let s = v.status_detail();
        let expected = format!("{}: {}", side_name(0, 2), tr("label.compare_only"));
        assert!(s.contains(&expected), "{s}");
        // Typing does nothing, and the lock in the header cannot be opened.
        frame(&mut v, &ctx, vec![]);
        v.place_caret(0, (0, 1), false);
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 0), ["a"]);
        // The Tree and Formatted views only compare.
        v.tree_active = true;
        assert!(v.status_detail().contains(tr("label.compare_only")));
        assert!(!v.status_detail().contains(tr("label.writable")));
    }

    #[test]
    fn a_read_only_pane_takes_the_caret_but_not_edits() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\n", "a\n");
        v.panes[0].doc.read_only = true;
        frame(&mut v, &ctx, vec![]);
        v.place_caret(0, (0, 1), false);
        // The caret still moves, but shortcuts that type a character are free to run.
        assert!(v.takes_text() && !v.types_into_pane());
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 0), ["a"]);
        assert!(!v.status.is_empty());
        v.place_caret(1, (0, 1), false);
        assert!(v.types_into_pane());
    }

    #[test]
    fn the_arrow_keys_in_an_empty_pane_keep_the_caret_at_its_start() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\nb\n", "");
        frame(&mut v, &ctx, vec![]);
        assert!(v.panes[1].doc.lines.is_empty());
        v.place_caret(1, (0, 0), false);
        for key in [Key::ArrowDown, Key::ArrowUp, Key::PageDown, Key::PageUp] {
            v.caret_key_pressed(key, Modifiers::NONE);
            assert_eq!(v.caret().map(|c| c.pos), Some((0, 0)), "{key:?}");
        }
    }

    #[test]
    fn typing_into_a_read_only_side_says_which_side_and_how_to_edit_it() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\n", "a\n");
        v.panes[1].doc.read_only = true;
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (0, 1), false);
        // The caret is drawn dimmed: typing would not go in.
        assert!(!v.caret_live(&ctx));
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 1), ["a"]);
        let want = trf("msg.side_read_only", &[("side", side_name(1, 2).into())]);
        assert_eq!(v.status, want);
        // Unlocked, the same caret takes the text.
        v.panes[1].doc.read_only = false;
        assert!(v.caret_live(&ctx));
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 1), ["ax"]);
        // Text read from a document cannot be unlocked, and the message says so.
        v.panes[0].doc.read_only = true;
        v.panes[0].doc.compare_only = true;
        v.place_caret(0, (0, 0), false);
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 0), ["a"]);
        let want = trf("msg.side_compare_only", &[("side", side_name(0, 2).into())]);
        assert_eq!(v.status, want);
    }

    #[test]
    fn the_caret_is_dimmed_while_the_find_field_has_the_keys() {
        let ctx = egui::Context::default();
        let mut v = two_way("alpha\n", "alpha\n");
        frame(&mut v, &ctx, vec![]);
        v.place_caret(0, (0, 2), false);
        assert!(v.caret_live(&ctx));
        v.find.open = true;
        v.find.focus = true;
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        assert!(!v.caret_live(&ctx));
        // Typed text goes to the find field, not the file.
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 0), ["alpha"]);
    }

    #[test]
    fn escape_closes_a_menu_the_find_bar_the_selection_and_the_caret_one_at_a_time() {
        let ctx = egui::Context::default();
        let esc = || key(Key::Escape, Modifiers::NONE);
        let mut v = two_way("alpha beta\n", "alpha beta\n");
        frame(&mut v, &ctx, vec![]);
        v.place_caret(0, (0, 0), false);
        v.place_caret(0, (0, 5), true);
        v.find.open = true;
        frame(&mut v, &ctx, vec![]);
        // An open menu takes Escape for itself.
        egui::Popup::open_id(&ctx, egui::Id::unique("menu"));
        frame(&mut v, &ctx, vec![esc()]);
        assert!(v.find.open);
        assert!(v.caret().is_some_and(|c| c.has_selection()));
        egui::Popup::close_all(&ctx);
        // Then the find bar.
        frame(&mut v, &ctx, vec![esc()]);
        assert!(!v.find.open);
        assert!(v.caret().is_some_and(|c| c.has_selection()));
        // Then the selection: the caret stays where it was.
        assert!(v.handles_escape());
        frame(&mut v, &ctx, vec![esc()]);
        let c = v.caret().unwrap();
        assert!(!c.has_selection());
        assert_eq!(c.pos, (0, 5));
        // Then the caret, and the next Escape is the tab's.
        frame(&mut v, &ctx, vec![esc()]);
        assert!(v.caret().is_none());
        assert!(!v.handles_escape());
        // A column selection goes the same way.
        v.find.open = true;
        v.col_sel = Some(Box::new(ColSel {
            pane: 0,
            anchor: (0, 0),
            head: (0, 3),
        }));
        frame(&mut v, &ctx, vec![esc()]);
        assert!(!v.find.open);
        assert!(v.col_sel.is_some());
        frame(&mut v, &ctx, vec![esc()]);
        assert!(v.col_sel.is_none());
    }

    #[test]
    fn enter_keeps_the_indentation_up_to_the_caret() {
        let ctx = egui::Context::default();
        let enter = || key(Key::Enter, Modifiers::NONE);
        let mut v = two_way("x\n", "    if a {\n\t\tb\n");
        frame(&mut v, &ctx, vec![]);
        // Spaces, from the end of the line.
        v.place_caret(1, (0, 10), false);
        frame(&mut v, &ctx, vec![enter(), egui::Event::Text("c".into())]);
        assert_eq!(lines(&v, 1), ["    if a {", "    c", "\t\tb"]);
        // Tabs, and a caret inside the indentation keeps only what is before it.
        v.place_caret(1, (2, 1), false);
        frame(&mut v, &ctx, vec![enter()]);
        assert_eq!(lines(&v, 1), ["    if a {", "    c", "\t", "\t\tb"]);
        assert_eq!(v.caret().map(|c| c.pos), Some((3, 1)));
        // A selection is replaced by the line break, indented like its first line.
        v.place_caret(1, (0, 4), false);
        v.place_caret(1, (0, 10), true);
        frame(&mut v, &ctx, vec![enter()]);
        assert_eq!(lines(&v, 1)[..2], ["    ", "    "]);
        // One undo step brings the selected text back.
        v.undo();
        assert_eq!(lines(&v, 1)[0], "    if a {");
    }

    #[test]
    fn duplicate_and_move_lines_take_the_caret_selection_or_the_selected_rows() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\nb\nc\nd\n", "a\nb\nc\nd\n");
        frame(&mut v, &ctx, vec![]);
        // The caret's line, with the caret on the copy.
        v.place_caret(1, (1, 1), false);
        v.duplicate_lines();
        assert_eq!(lines(&v, 1), ["a", "b", "b", "c", "d"]);
        assert_eq!(v.caret().map(|c| c.pos), Some((2, 1)));
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "b", "c", "d"]);
        // A selection to the start of a line leaves that line out, and moves with its lines.
        v.place_caret(1, (1, 0), false);
        v.place_caret(1, (3, 0), true);
        v.move_lines(true);
        assert_eq!(lines(&v, 1), ["b", "c", "a", "d"]);
        let c = v.caret().unwrap();
        assert_eq!((c.anchor, c.pos), ((0, 0), (2, 0)));
        // At the top nothing changes, and there is nothing to undo for it.
        let steps = v.undo.len();
        v.move_lines(true);
        assert_eq!(lines(&v, 1), ["b", "c", "a", "d"]);
        assert_eq!(v.undo.len(), steps);
        v.move_lines(false);
        v.move_lines(false);
        assert_eq!(lines(&v, 1), ["a", "d", "b", "c"]);
        // At the bottom, the selection ends at the end of the file.
        assert_eq!(v.caret().map(|c| c.range()), Some(((2, 0), (3, 1))));
        // Duplicating a selection selects the copy.
        v.duplicate_lines();
        assert_eq!(lines(&v, 1), ["a", "d", "b", "c", "b", "c"]);
        assert_eq!(v.caret().map(|c| c.range()), Some(((4, 0), (5, 1))));
        // Without a caret: the active pane's lines in the selected rows.
        v.caret = None;
        v.panes[0].doc.lines = ["1", "2", "3"].map(String::from).to_vec();
        v.rediff();
        v.active_pane = 0;
        v.sel_anchor = Some(0);
        v.cursor_row = Some(v.diff.row_of_line(0, 1));
        v.move_lines(false);
        assert_eq!(lines(&v, 0), ["3", "1", "2"]);
        assert_eq!(v.cursor_row, Some(v.diff.row_of_line(0, 2)));
        assert_eq!(v.sel_anchor, Some(v.diff.row_of_line(0, 1)));
        // A read-only side says so and keeps its lines.
        v.panes[0].doc.read_only = true;
        v.duplicate_lines();
        assert_eq!(lines(&v, 0), ["3", "1", "2"]);
        assert_eq!(v.status, v.read_only_message(0));
    }

    /// Puts carets at `at` in pane `pane`, the last one primary.
    fn put_carets(v: &mut TextView, pane: usize, at: &[(edit::Pos, edit::Pos)]) {
        let n = at.len();
        let list = at
            .iter()
            .enumerate()
            .map(|(i, &(anchor, pos))| {
                let c = Caret {
                    pane,
                    pos,
                    anchor,
                    want: None,
                };
                (c, i + 1 == n)
            })
            .collect();
        v.set_carets(list);
    }

    /// The carets in text order, as (anchor, pos).
    fn caret_spots(v: &TextView) -> Vec<(edit::Pos, edit::Pos)> {
        let mut c: Vec<_> = v.carets().iter().map(|c| (c.anchor, c.pos)).collect();
        c.sort();
        c
    }

    #[test]
    fn typing_after_escape_from_several_carets_is_its_own_undo_step() {
        let ctx = egui::Context::default();
        let mut v = two_way("x\n", "a\nb\n");
        frame(&mut v, &ctx, vec![]);
        put_carets(&mut v, 1, &[((0, 1), (0, 1)), ((1, 1), (1, 1))]);
        let text = |t: &str| egui::Event::Text(t.into());
        frame(&mut v, &ctx, vec![text("x"), text("y")]);
        assert_eq!(lines(&v, 1), ["axy", "bxy"]);
        v.caret_key_pressed(Key::Escape, Modifiers::NONE);
        assert_eq!(v.carets().len(), 1);
        frame(&mut v, &ctx, vec![text("z")]);
        assert_eq!(lines(&v, 1), ["axy", "bxyz"]);
        v.undo();
        assert_eq!(lines(&v, 1), ["axy", "bxy"]);
    }

    #[test]
    fn overlapping_multi_caret_deletions_use_the_original_text_and_undo_once() {
        for mac in [false, true] {
            let modifiers = if mac { Modifiers::ALT } else { Modifiers::CTRL };
            for (key, spots, expected) in [
                (
                    Key::Backspace,
                    vec![((0, 2), (0, 2)), ((0, 4), (0, 4))],
                    "o world",
                ),
                (
                    Key::Delete,
                    vec![((0, 1), (0, 1)), ((0, 3), (0, 3))],
                    "h world",
                ),
                (
                    Key::Backspace,
                    vec![((0, 1), (0, 3)), ((0, 4), (0, 4))],
                    "o world",
                ),
            ] {
                let mut v = two_way("untouched\n", "hello world\n");
                put_carets(&mut v, 1, &spots);
                let original = caret_spots(&v);
                v.caret_key_pressed_on(mac, key, modifiers);
                assert_eq!(lines(&v, 1), [expected]);
                assert_eq!(v.carets().len(), 1, "overlapping ranges collapse together");
                let after = caret_spots(&v);
                assert_eq!(lines(&v, 0), ["untouched"]);
                assert_eq!(v.undo.len(), 1);
                v.undo();
                assert_eq!(lines(&v, 1), ["hello world"]);
                assert_eq!(caret_spots(&v), original);
                v.redo();
                assert_eq!(lines(&v, 1), [expected]);
                assert_eq!(caret_spots(&v), after);
            }
        }
        // Newline joins, grapheme clusters, and non-overlapping positions on one line.
        for (text, spots, key, expected) in [
            (
                "a\nb\nc\n",
                vec![((1, 0), (1, 0)), ((2, 0), (2, 0))],
                Key::Backspace,
                vec!["abc"],
            ),
            (
                "é漢😀\n",
                vec![((0, 2), (0, 2)), ((0, 5), (0, 5))],
                Key::Backspace,
                vec!["😀"],
            ),
            (
                "abcd\n",
                vec![((0, 0), (0, 0)), ((0, 2), (0, 2))],
                Key::Delete,
                vec!["bd"],
            ),
        ] {
            let mut v = two_way("x\n", text);
            put_carets(&mut v, 1, &spots);
            v.caret_key_pressed_on(false, key, Modifiers::NONE);
            assert_eq!(lines(&v, 1), expected);
            v.undo();
            assert_eq!(v.panes[1].doc.text(), text);
            v.redo();
            assert_eq!(lines(&v, 1), expected);
        }
    }

    #[test]
    fn multi_caret_cuts_copy_original_lines_once_and_preserve_the_rest() {
        for (spots, expected, copied) in [
            (
                vec![((0, 1), (0, 1)), ((0, 2), (0, 2))],
                vec!["def", "ghi"],
                "abc\n",
            ),
            (
                vec![((0, 0), (0, 0)), ((0, 1), (0, 1)), ((0, 2), (0, 2))],
                vec!["def", "ghi"],
                "abc\n",
            ),
            (
                vec![((1, 1), (1, 1)), ((2, 1), (2, 1))],
                vec!["abc"],
                "def\nghi\n",
            ),
            (
                vec![((0, 1), (0, 1)), ((2, 1), (2, 1))],
                vec!["def"],
                "abc\nghi\n",
            ),
            (
                vec![((0, 1), (0, 2)), ((0, 3), (0, 3))],
                vec!["def", "ghi"],
                "b\nabc",
            ),
            (
                vec![((0, 0), (0, 0)), ((1, 0), (1, 0)), ((2, 0), (2, 0))],
                vec![""],
                "abc\ndef\nghi\n",
            ),
        ] {
            for read_only in [false, true] {
                let ctx = egui::Context::default();
                let mut v = two_way("untouched\n", "abc\ndef\nghi\n");
                put_carets(&mut v, 1, &spots);
                let original = caret_spots(&v);
                v.panes[1].doc.read_only = read_only;
                let output = frame(&mut v, &ctx, vec![egui::Event::Cut]);
                assert!(output
                    .platform_output
                    .commands
                    .contains(&egui::OutputCommand::CopyText(copied.into())));
                assert_eq!(lines(&v, 0), ["untouched"]);
                if read_only {
                    assert_eq!(lines(&v, 1), ["abc", "def", "ghi"]);
                    assert_eq!(caret_spots(&v), original);
                    assert!(v.undo.is_empty());
                } else {
                    assert_eq!(lines(&v, 1), expected);
                    assert_eq!(v.undo.len(), 1);
                    let after = caret_spots(&v);
                    v.undo();
                    assert_eq!(lines(&v, 1), ["abc", "def", "ghi"]);
                    assert_eq!(caret_spots(&v), original);
                    v.redo();
                    assert_eq!(lines(&v, 1), expected);
                    assert_eq!(caret_spots(&v), after);
                }
            }
        }
    }

    #[test]
    fn carets_merge_where_they_meet_and_follow_edits_before_them() {
        let k = |a: edit::Pos, p: edit::Pos| Caret {
            pane: 0,
            pos: p,
            anchor: a,
            want: None,
        };
        let mut list = vec![
            (k((0, 5), (0, 5)), false),
            (k((0, 1), (0, 3)), true),
            (k((0, 3), (0, 3)), false),
            (k((1, 0), (1, 2)), false),
            (k((1, 2), (1, 4)), false),
            (k((0, 1), (0, 1)), false),
        ];
        merge_carets(&mut list);
        let got: Vec<_> = list.iter().map(|(c, p)| (c.anchor, c.pos, *p)).collect();
        // A bare caret inside or at the end of a selection joins it, with the primary mark;
        // two selections that only touch stay apart.
        assert_eq!(
            got,
            [
                ((0, 1), (0, 3), true),
                ((0, 5), (0, 5), false),
                ((1, 0), (1, 2), false),
                ((1, 2), (1, 4), false),
            ]
        );
        let after: Vec<String> = ["xy", "ab", "aXcd"].map(String::from).to_vec();
        // An edit before it on its line: the text after it says where it is now.
        assert_eq!(shift_past_edit(&after, 3, (2, 2), "cd"), (2, 2));
        // A line added above.
        assert_eq!(shift_past_edit(&after, 2, (1, 1), "cd"), (2, 2));
        assert_eq!(shift_past_edit(&after, 3, (0, 9), ""), (0, 2));
    }

    #[test]
    fn typing_deleting_and_moving_at_several_carets_is_one_step_each() {
        let ctx = egui::Context::default();
        let text = |t: &str| egui::Event::Text(t.into());
        let k = |key| super::tests::key(key, Modifiers::NONE);
        let mut v = two_way("x\n", "alpha\nbeta\ngamma\n");
        frame(&mut v, &ctx, vec![]);
        put_carets(
            &mut v,
            1,
            &[((0, 0), (0, 0)), ((1, 0), (1, 0)), ((2, 0), (2, 0))],
        );
        frame(&mut v, &ctx, vec![text("-"), text("-")]);
        assert_eq!(lines(&v, 1), ["--alpha", "--beta", "--gamma"]);
        assert_eq!(v.carets().len(), 3);
        // Typing joins one undo step, which puts the carets back.
        v.undo();
        assert_eq!(lines(&v, 1), ["alpha", "beta", "gamma"]);
        assert_eq!(
            caret_spots(&v),
            [((0, 0), (0, 0)), ((1, 0), (1, 0)), ((2, 0), (2, 0))]
        );
        v.redo();
        assert_eq!(lines(&v, 1), ["--alpha", "--beta", "--gamma"]);
        assert_eq!(
            caret_spots(&v),
            [((0, 2), (0, 2)), ((1, 2), (1, 2)), ((2, 2), (2, 2))]
        );
        // Backspace, End and Enter with each caret's own indentation.
        frame(&mut v, &ctx, vec![k(Key::Backspace)]);
        assert_eq!(lines(&v, 1), ["-alpha", "-beta", "-gamma"]);
        frame(&mut v, &ctx, vec![k(Key::End), text("!")]);
        assert_eq!(lines(&v, 1), ["-alpha!", "-beta!", "-gamma!"]);
        // Up from the first line goes to its start, so two carets meet there and merge.
        frame(&mut v, &ctx, vec![k(Key::ArrowUp)]);
        assert_eq!(v.carets().len(), 3);
        assert_eq!(v.carets().iter().filter(|c| c.pos == (0, 0)).count(), 1);
        frame(&mut v, &ctx, vec![k(Key::ArrowUp)]);
        assert_eq!(v.carets().len(), 2, "two reach the top and merge");
        // Escape goes back to the primary caret; the next one removes it.
        frame(&mut v, &ctx, vec![k(Key::Escape)]);
        assert_eq!(v.carets().len(), 1);
        frame(&mut v, &ctx, vec![k(Key::Escape)]);
        assert!(v.caret().is_none());

        // Several carets on one line, with characters of several bytes.
        let mut v = two_way("x\n", "é漢😀\n");
        frame(&mut v, &ctx, vec![]);
        put_carets(
            &mut v,
            1,
            &[((0, 2), (0, 2)), ((0, 5), (0, 5)), ((0, 9), (0, 9))],
        );
        frame(&mut v, &ctx, vec![text("|")]);
        assert_eq!(lines(&v, 1), ["é|漢|😀|"]);
        frame(&mut v, &ctx, vec![k(Key::Backspace), k(Key::Backspace)]);
        assert_eq!(lines(&v, 1), [""]);
        assert_eq!(v.carets().len(), 1, "all at the start, merged");
    }

    #[test]
    fn several_carets_paste_one_line_each_copy_in_order_and_indent_once() {
        let ctx = egui::Context::default();
        let mut v = two_way("x\n", "a\nb\nc\n");
        frame(&mut v, &ctx, vec![]);
        put_carets(
            &mut v,
            1,
            &[((0, 1), (0, 1)), ((1, 1), (1, 1)), ((2, 1), (2, 1))],
        );
        // As many lines as carets: one each.
        frame(&mut v, &ctx, vec![egui::Event::Paste("1\n2\n3".into())]);
        assert_eq!(lines(&v, 1), ["a1", "b2", "c3"]);
        // Otherwise the whole text at each, as one undo step.
        frame(&mut v, &ctx, vec![egui::Event::Paste("z".into())]);
        assert_eq!(lines(&v, 1), ["a1z", "b2z", "c3z"]);
        v.undo();
        assert_eq!(lines(&v, 1), ["a1", "b2", "c3"]);
        // Copy: the selections one per line, in text order.
        put_carets(&mut v, 1, &[((2, 0), (2, 1)), ((0, 0), (0, 2))]);
        let copied = |out: egui::FullOutput, want: &str| {
            assert!(
                out.platform_output
                    .commands
                    .contains(&egui::OutputCommand::CopyText(want.into())),
                "{:?}",
                out.platform_output.commands
            );
        };
        copied(frame(&mut v, &ctx, vec![egui::Event::Copy]), "a1\nc");
        // Without selections, each caret's whole line.
        put_carets(&mut v, 1, &[((0, 0), (0, 0)), ((2, 0), (2, 0))]);
        copied(frame(&mut v, &ctx, vec![egui::Event::Copy]), "a1\nc3\n");
        // Tab with two carets on one line indents it once; Shift+Tab takes it back.
        put_carets(
            &mut v,
            1,
            &[((1, 0), (1, 0)), ((1, 1), (1, 1)), ((0, 0), (0, 2))],
        );
        frame(&mut v, &ctx, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1), ["    a1", "    b2", "c3"]);
        frame(&mut v, &ctx, vec![key(Key::Tab, Modifiers::SHIFT)]);
        assert_eq!(lines(&v, 1), ["a1", "b2", "c3"]);
        // Duplicate and move act on each caret's lines, as runs.
        put_carets(&mut v, 1, &[((0, 0), (0, 0)), ((2, 1), (2, 1))]);
        v.duplicate_lines();
        assert_eq!(lines(&v, 1), ["a1", "a1", "b2", "c3", "c3"]);
        assert_eq!(caret_spots(&v), [((1, 0), (1, 0)), ((4, 1), (4, 1))]);
        v.move_lines(false);
        assert_eq!(
            lines(&v, 1),
            ["a1", "a1", "b2", "c3", "c3"],
            "the last can't move: none do"
        );
        put_carets(&mut v, 1, &[((2, 0), (2, 0)), ((4, 0), (4, 0))]);
        v.move_lines(true);
        assert_eq!(lines(&v, 1), ["a1", "b2", "a1", "c3", "c3"]);
        assert_eq!(caret_spots(&v), [((1, 0), (1, 0)), ((3, 0), (3, 0))]);
    }

    #[test]
    fn carets_move_with_lines_copied_in_and_a_read_only_side_refuses_them_all() {
        let ctx = egui::Context::default();
        let mut v = two_way("new\na\nb\n", "a\nb\n");
        frame(&mut v, &ctx, vec![]);
        put_carets(&mut v, 1, &[((0, 1), (0, 1)), ((1, 1), (1, 1))]);
        v.copy_block(0, 0, 1);
        assert_eq!(lines(&v, 1), ["new", "a", "b"]);
        assert_eq!(caret_spots(&v), [((1, 1), (1, 1)), ((2, 1), (2, 1))]);
        // Ctrl+Alt+Down (⌘⌥ on macOS) adds a caret below the last, at its column.
        let add = Modifiers::ALT.plus(Modifiers::COMMAND);
        frame(&mut v, &ctx, vec![key(Key::ArrowUp, add)]);
        assert_eq!(caret_spots(&v)[0], ((0, 1), (0, 1)));
        assert_eq!(v.carets().len(), 3);
        v.panes[1].doc.read_only = true;
        frame(&mut v, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(lines(&v, 1), ["new", "a", "b"]);
        assert_eq!(v.status, v.read_only_message(1));
        assert_eq!(v.carets().len(), 3, "the carets stay");
    }

    #[test]
    fn ctrl_click_adds_and_removes_carets_in_both_layouts() {
        for layout in [TextLayout::Compact, TextLayout::Aligned] {
            let ctx = egui::Context::default();
            crate::ui::fonts::setup(&ctx, "en");
            let mut v = two_way("one\ntwo\nthree\n", "one\nextra\ntwo\nthree\n");
            v.text_layout = layout;
            frame(&mut v, &ctx, vec![]);
            frame(&mut v, &ctx, vec![]);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let click = |v: &mut TextView, p: egui::Pos2, m: Modifiers| {
                let down = vec![egui::Event::PointerMoved(p), button(p, true)];
                frame_with(v, &ctx, down, m);
                frame_with(v, &ctx, vec![button(p, false)], m);
                // Apart from the next click, so it does not count as a double-click.
                for _ in 0..3 {
                    frame(v, &ctx, vec![]);
                }
            };
            let mut ys = std::collections::HashMap::new();
            for y in (90..300).step_by(2) {
                click(&mut v, pos2(650.0, y as f32), Modifiers::NONE);
                if let Some(c) = v.caret().filter(|c| c.pane == 1) {
                    ys.entry(c.pos.0).or_insert(y as f32 + 2.0);
                }
            }
            let name = format!("{layout:?}");
            let at = |l: usize| pos2(650.0, ys[&l]);
            click(&mut v, at(0), Modifiers::NONE);
            click(&mut v, at(1), Modifiers::COMMAND);
            click(&mut v, at(3), Modifiers::COMMAND);
            let lines_of_carets: Vec<usize> = caret_spots(&v).iter().map(|c| c.1 .0).collect();
            assert_eq!(lines_of_carets, [0, 1, 3], "{name}");
            assert_eq!(
                v.caret().map(|c| c.pos.0),
                Some(3),
                "{name}: the last added leads"
            );
            // Ctrl+click on a caret removes it.
            click(&mut v, at(1), Modifiers::COMMAND);
            let lines_of_carets: Vec<usize> = caret_spots(&v).iter().map(|c| c.1 .0).collect();
            assert_eq!(lines_of_carets, [0, 3], "{name}");
            // Typing goes in at both, as one undo step.
            frame(&mut v, &ctx, vec![key(Key::Home, Modifiers::NONE)]);
            frame(&mut v, &ctx, vec![egui::Event::Text("#".into())]);
            assert_eq!(lines(&v, 1), ["#one", "extra", "two", "#three"], "{name}");
            v.undo();
            assert_eq!(lines(&v, 1), ["one", "extra", "two", "three"], "{name}");
            // A plain click goes back to one caret.
            click(&mut v, at(2), Modifiers::NONE);
            assert_eq!(v.carets().len(), 1, "{name}");
        }
    }

    #[test]
    fn a_slow_comparison_waits_while_typing_goes_on_and_never_longer() {
        let ctx = egui::Context::default();
        let text = |t: &str| egui::Event::Text(t.into());
        let mut v = two_way("a\nb\n", "a\nb\n");
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (0, 1), false);
        let slow = |v: &mut TextView| v.rediff_ms = 100.0;
        // Typing in a line: the text changes at once, the comparison waits.
        slow(&mut v);
        frame(&mut v, &ctx, vec![text("x")]);
        frame(&mut v, &ctx, vec![text("y")]);
        assert_eq!(lines(&v, 1), ["axy", "b"]);
        assert!(v.rediff_due.is_some());
        assert!(v.diff.blocks.is_empty(), "not compared yet");
        // A pause in typing compares.
        for _ in 0..3 {
            frame(&mut v, &ctx, vec![]);
        }
        assert!(v.rediff_due.is_none());
        assert_eq!(v.diff.blocks.len(), 1);
        // So does any other key, before it acts,
        slow(&mut v);
        frame(&mut v, &ctx, vec![text("z")]);
        assert!(v.rediff_due.is_some());
        frame(&mut v, &ctx, vec![key(Key::ArrowLeft, Modifiers::NONE)]);
        assert!(v.rediff_due.is_none());
        // and any command, which the app runs after settling the comparison.
        slow(&mut v);
        frame(&mut v, &ctx, vec![text("w")]);
        assert!(v.rediff_due.is_some());
        v.settle_diff();
        assert!(v.rediff_due.is_none());
        assert_eq!(lines(&v, 1), ["axywz", "b"]);
        // A new line always compares at once; the undo steps are those of typing as before:
        // the new line, "w", then "xyz" typed in one go.
        slow(&mut v);
        frame(&mut v, &ctx, vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(v.rediff_due.is_none());
        v.undo();
        v.undo();
        assert_eq!(lines(&v, 1), ["axyz", "b"]);
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "b"]);
    }

    /// Editing latency on large files, run by hand in a release build:
    /// `cargo test --release --bin oxidemerge editing_latency -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn editing_latency() {
        for (lines_n, every) in [(10_000, 20), (50_000, 20), (50_000, 2), (200_000, 50)] {
            let a: String = (0..lines_n)
                .map(|i| format!("    let value_{i} = compute({i}, \"text\");\n"))
                .collect();
            let b: String = (0..lines_n)
                .map(|i| {
                    if i % every == 0 {
                        format!("    let value_{i} = changed({i});\n")
                    } else {
                        format!("    let value_{i} = compute({i}, \"text\");\n")
                    }
                })
                .collect();
            let ctx = egui::Context::default();
            let mut v = two_way(&a, &b);
            frame(&mut v, &ctx, vec![]);
            let time = |v: &mut TextView, events: Vec<egui::Event>, n: usize| {
                let mut ts = Vec::new();
                for _ in 0..n {
                    let t = std::time::Instant::now();
                    frame(v, &ctx, events.clone());
                    ts.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                ts.sort_by(f64::total_cmp);
                (ts[ts.len() / 2], ts[ts.len() - 1])
            };
            let idle = time(&mut v, vec![], 10);
            let mid = lines_n / 2;
            v.place_caret(1, (mid, 8), false);
            frame(&mut v, &ctx, vec![]);
            // The first keystroke takes an undo snapshot; the next ones join it.
            let first = time(&mut v, vec![egui::Event::Text("x".into())], 1);
            let typing = time(&mut v, vec![egui::Event::Text("x".into())], 20);
            let enter = time(&mut v, vec![key(Key::Enter, Modifiers::NONE)], 5);
            let select = time(&mut v, vec![key(Key::ArrowDown, Modifiers::SHIFT)], 20);
            let t = std::time::Instant::now();
            v.rediff();
            let rediff = t.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{lines_n} lines, 1 in {every} changed ({} blocks): idle frame {:.1} ms, first key {:.1}, typing {:.1} (max {:.1}), Enter {:.1} (max {:.1}), Shift+Down {:.1} (max {:.1}), rediff {rediff:.1}",
                v.diff.blocks.len(), idle.0, first.0, typing.0, typing.1, enter.0, enter.1, select.0, select.1
            );
        }
    }

    #[test]
    fn a_click_or_drag_at_the_bottom_edge_never_jumps_the_view() {
        let text: String = (0..300).map(|i| format!("line {i}\n")).collect();
        let mut v = two_way(&text, &format!("x\n{text}"));
        let ctx = egui::Context::default();
        crate::ui::fonts::setup(&ctx, "en");
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        let button = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        // Clicking the lowest lines puts the caret there and leaves the view alone.
        let mut lowest = None;
        for y in (300..500).rev().step_by(4) {
            let p = pos2(200.0, y as f32);
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(p), button(p, true)],
            );
            frame(&mut v, &ctx, vec![button(p, false)]);
            for _ in 0..4 {
                frame(&mut v, &ctx, vec![]);
            }
            if v.caret().is_some() {
                lowest = Some(p);
                break;
            }
        }
        assert!(lowest.is_some(), "no line found to click");
        assert_eq!(v.scroll, 0.0);
        // Dragging past the bottom scrolls a little each frame, with the selection's end at
        // the bottom edge.
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        let from = pos2(200.0, 120.0);
        frame(
            &mut v,
            &ctx,
            vec![egui::Event::PointerMoved(from), button(from, true)],
        );
        for y in [200.0, 300.0, 400.0, 495.0] {
            frame(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(pos2(200.0, y))],
            );
        }
        let mut last = v.scroll;
        for _ in 0..12 {
            frame(&mut v, &ctx, vec![]);
            let step = v.scroll - last;
            assert!((0.0..2.0).contains(&step), "the view jumped {step} lines");
            last = v.scroll;
            let end = v.caret().map(|c| c.pos.0).unwrap_or(0) as f32;
            assert!(
                end >= v.scroll + 10.0,
                "end {end} left the bottom (top {})",
                v.scroll
            );
        }
        assert!(v.scroll > 10.0);
        frame(&mut v, &ctx, vec![button(pos2(200.0, 495.0), false)]);
        for _ in 0..4 {
            frame(&mut v, &ctx, vec![]);
        }
        assert_eq!(v.scroll, last, "the view moved after the drag");
    }

    /// Runs one frame of the side-by-side view with `events`.
    fn frame(v: &mut TextView, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        frame_with(v, ctx, events, Modifiers::NONE)
    }

    /// Runs one frame with `modifiers` held.
    fn frame_with(
        v: &mut TextView,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        modifiers: Modifiers,
    ) -> egui::FullOutput {
        let (cfg, colors) = (Config::default(), theme::colors(false));
        let mut events = events;
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        thread_local!(static CLOCK: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) });
        // Frames are 0.15 s apart: clicks a frame apart make a double-click, three apart do not.
        let time = CLOCK.with(|c| {
            c.set(c.get() + 0.15);
            c.get()
        });
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 500.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| v.ui(ui, &cfg, &colors));
        });
        out.textures_delta.clear();
        out
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// The text of the line at the top of pane `p` as it is scrolled now.
    fn top_text(v: &TextView, p: usize) -> Option<String> {
        let VLine::Row(r, _) = *v.layout.lines.get(v.scroll as usize)? else {
            return None;
        };
        let l = v.diff.rows[r].line(p)?;
        Some(v.panes[p].doc.lines[l].clone())
    }

    #[test]
    fn the_view_and_the_caret_keep_their_text_when_rows_above_them_change() {
        let ctx = egui::Context::default();
        let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut v = two_way(&format!("extra 1\nextra 2\nextra 3\n{body}"), &body);
        frame(&mut v, &ctx, vec![]);
        assert_eq!(v.diff.blocks.len(), 1, "the three extra lines");
        v.scroll = 120.0;
        frame(&mut v, &ctx, vec![]);
        let top = top_text(&v, 1).unwrap();
        // A caret with a selection in the right side, in view, below the copied lines.
        v.place_caret(1, (125, 2), false);
        v.place_caret(1, (126, 4), true);
        v.scroll = 120.0;
        frame(&mut v, &ctx, vec![]);
        let selected = v.caret_text();

        // Copying the block adds three rows of text to the right side above both.
        v.copy_block(0, 0, 1);
        frame(&mut v, &ctx, vec![]);
        assert_eq!(lines(&v, 1)[..3], ["extra 1", "extra 2", "extra 3"]);
        assert_eq!(
            top_text(&v, 1).as_ref(),
            Some(&top),
            "the same text at the top"
        );
        let c = v.caret().unwrap();
        assert_eq!(
            (c.pos, c.anchor),
            ((129, 4), (128, 2)),
            "moved with its lines"
        );
        assert_eq!(v.caret_text(), selected);

        // Undo takes them out again: the view, the caret and the selection go back with them.
        v.undo();
        frame(&mut v, &ctx, vec![]);
        assert_eq!(top_text(&v, 1).as_ref(), Some(&top));
        let c = v.caret().unwrap();
        assert_eq!((c.pos, c.anchor), ((126, 4), (125, 2)));
        assert_eq!(v.caret_text(), selected);
        v.redo();
        frame(&mut v, &ctx, vec![]);
        assert_eq!(v.caret().unwrap().pos, (129, 4));

        // Without a caret, the cursor row keeps to its line.
        v.caret = None;
        v.cursor_row = Some(v.diff.row_of_line(1, 160));
        v.undo();
        frame(&mut v, &ctx, vec![]);
        let row = v.cursor_row.unwrap();
        assert_eq!(v.diff.rows[row].line(1), Some(157));
        assert_eq!(lines(&v, 1)[157], "line 157");
    }

    /// The display line where pane `p`'s line `l` starts.
    fn display_line_of(v: &TextView, p: usize, l: usize) -> usize {
        v.layout.row_line[v.diff.row_of_line(p, l)]
    }

    #[test]
    fn the_view_keeps_its_text_with_folded_and_wrapped_lines() {
        let ctx = egui::Context::default();
        // Folded: unchanged lines between two differences are collapsed.
        let body: String = (0..300).map(|i| format!("line {i}\n")).collect();
        // A difference every 20 lines, so that the folded view is longer than the window.
        let changed: String = (0..300)
            .map(|i| match i % 20 {
                10 => format!("line {i} changed\n"),
                _ => format!("line {i}\n"),
            })
            .collect();
        let mut v = two_way(&format!("extra 1\nextra 2\nextra 3\n{body}"), &changed);
        v.set_collapse(true);
        frame(&mut v, &ctx, vec![]);
        assert!(v.layout.lines.iter().any(|l| matches!(l, VLine::Fold(..))));
        v.scroll = display_line_of(&v, 1, 149) as f32;
        frame(&mut v, &ctx, vec![]);
        assert_eq!(top_text(&v, 1).as_deref(), Some("line 149"));
        v.place_caret(1, (150, 4), false);
        v.scroll = display_line_of(&v, 1, 149) as f32;
        frame(&mut v, &ctx, vec![]);
        v.copy_block(0, 0, 1);
        frame(&mut v, &ctx, vec![]);
        assert_eq!(lines(&v, 1)[..3], ["extra 1", "extra 2", "extra 3"]);
        assert_eq!(top_text(&v, 1).as_deref(), Some("line 149"), "folded");
        assert_eq!(v.caret().map(|c| c.pos), Some((153, 4)));
        // Copying moves on to the next difference; back to where the test looks.
        v.scroll_anim = None;
        v.scroll = display_line_of(&v, 1, 152) as f32;
        frame(&mut v, &ctx, vec![]);
        v.undo();
        for _ in 0..6 {
            frame(&mut v, &ctx, vec![]);
        }
        assert_eq!(
            top_text(&v, 1).as_deref(),
            Some("line 149"),
            "folded, undone"
        );

        // Wrapped: the view stays on the same display line of a long line.
        let long = |i: usize| format!("{i:03} {}", "word ".repeat(60));
        let body: String = (0..120).map(|i| long(i) + "\n").collect();
        let mut v = two_way(&format!("extra 1\nextra 2\nextra 3\n{body}"), &body);
        v.wrap = true;
        frame(&mut v, &ctx, vec![]);
        frame(&mut v, &ctx, vec![]);
        let first = display_line_of(&v, 1, 80);
        assert!(display_line_of(&v, 1, 81) > first + 1, "line 80 wraps");
        v.scroll = (first + 1) as f32 + 0.25;
        frame(&mut v, &ctx, vec![]);
        v.copy_block(0, 0, 1);
        frame(&mut v, &ctx, vec![]);
        assert_eq!(
            v.scroll,
            (display_line_of(&v, 1, 83) + 1) as f32 + 0.25,
            "wrapped"
        );
    }

    #[test]
    fn a_caret_moving_past_the_edge_scrolls_one_line_at_a_time() {
        let ctx = egui::Context::default();
        let body: String = (0..300).map(|i| format!("line {i}\n")).collect();
        let mut v = two_way(&body, &body);
        frame(&mut v, &ctx, vec![]);
        v.place_caret(1, (0, 0), false);
        frame(&mut v, &ctx, vec![]);
        let mut last = v.scroll;
        let mut moved = 0;
        for _ in 0..80 {
            frame(&mut v, &ctx, vec![key(Key::ArrowDown, Modifiers::NONE)]);
            let step = v.scroll - last;
            assert!(
                (0.0..=1.001).contains(&step),
                "scrolled {step} lines at once"
            );
            moved += (step > 0.0) as usize;
            last = v.scroll;
        }
        assert!(moved > 20, "the view followed the caret");
        // Typing a new line at the bottom edge scrolls one line too.
        let before = v.scroll;
        frame(&mut v, &ctx, vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(v.scroll - before <= 1.001, "{} -> {}", before, v.scroll);
        // A jump far away still centres the caret's line a third of the way down.
        frame(&mut v, &ctx, vec![key(Key::End, Modifiers::COMMAND)]);
        for _ in 0..4 {
            frame(&mut v, &ctx, vec![]);
        }
        assert!(v.scroll > 200.0, "{}", v.scroll);
    }

    #[test]
    fn word_keys_move_select_and_delete_by_word() {
        let ctx = egui::Context::default();
        let mut v = two_way("x\n", "let foo_bar = x.len();\nnext\n");
        frame(&mut v, &ctx, vec![]);
        // Option on macOS, Ctrl (which egui also reports as the command key) elsewhere.
        let word = if MAC {
            Modifiers::ALT
        } else {
            Modifiers::CTRL.plus(Modifiers::COMMAND)
        };
        let shift_word = word.plus(Modifiers::SHIFT);
        let press = |v: &mut TextView, k: Key, m: Modifiers| frame(v, &ctx, vec![key(k, m)]);
        v.place_caret(1, (0, 0), false);
        press(&mut v, Key::ArrowRight, word);
        assert_eq!(v.caret().unwrap().pos, (0, 3));
        press(&mut v, Key::ArrowRight, word);
        assert_eq!(v.caret().unwrap().pos, (0, 11));
        press(&mut v, Key::ArrowLeft, word);
        assert_eq!(v.caret().unwrap().pos, (0, 4));
        // With Shift the selection grows by words, and shrinks back.
        press(&mut v, Key::ArrowRight, shift_word);
        press(&mut v, Key::ArrowRight, shift_word);
        assert_eq!(v.caret_text().as_deref(), Some("foo_bar ="));
        press(&mut v, Key::ArrowLeft, shift_word);
        assert_eq!(v.caret_text().as_deref(), Some("foo_bar "));
        // A word key deletes the selection first.
        press(&mut v, Key::Backspace, word);
        assert_eq!(lines(&v, 1)[0], "let = x.len();");
        v.undo();
        assert_eq!(lines(&v, 1)[0], "let foo_bar = x.len();");
        // Backspace and Delete by word, each one undo step.
        v.place_caret(1, (0, 15), false);
        press(&mut v, Key::Backspace, word);
        assert_eq!(lines(&v, 1)[0], "let foo_bar = .len();");
        press(&mut v, Key::Delete, word);
        assert_eq!(lines(&v, 1)[0], "let foo_bar = len();");
        v.undo();
        assert_eq!(lines(&v, 1)[0], "let foo_bar = .len();");
        v.undo();
        assert_eq!(lines(&v, 1)[0], "let foo_bar = x.len();");
        // At a line end the word move goes to the next line, and Delete by word joins it.
        v.place_caret(1, (0, 22), false);
        press(&mut v, Key::ArrowRight, word);
        assert_eq!(v.caret().unwrap().pos, (1, 0));
        // The plain keys still move one character.
        press(&mut v, Key::ArrowRight, Modifiers::NONE);
        assert_eq!(v.caret().unwrap().pos, (1, 1));
        assert_eq!(lines(&v, 0), ["x"], "the other side is untouched");

        // A read-only side moves by word but does not change.
        v.panes[0].doc.read_only = true;
        v.place_caret(0, (0, 1), false);
        press(&mut v, Key::Backspace, word);
        assert_eq!(lines(&v, 0), ["x"]);
        press(&mut v, Key::ArrowLeft, word);
        assert_eq!(v.caret().unwrap().pos, (0, 0));
    }

    #[test]
    fn cut_and_delete_remove_the_selected_rows() {
        let ctx = egui::Context::default();
        let mut v = two_way("a\nb\nc\nd\n", "a\nb\nc\nd\n");
        frame(&mut v, &ctx, vec![]);
        v.active_pane = 1;
        v.sel_anchor = Some(1);
        v.cursor_row = Some(2);
        let out = frame(&mut v, &ctx, vec![egui::Event::Cut]);
        assert!(out
            .platform_output
            .commands
            .contains(&egui::OutputCommand::CopyText("b\nc\n".into())));
        assert_eq!(lines(&v, 1), ["a", "d"]);
        assert_eq!(lines(&v, 0), ["a", "b", "c", "d"], "only the active pane");
        v.undo();
        assert_eq!(lines(&v, 1), ["a", "b", "c", "d"]);

        // Delete needs a selection: on the cursor row alone it does nothing.
        v.sel_anchor = None;
        v.cursor_row = Some(0);
        frame(&mut v, &ctx, vec![key(Key::Delete, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1), ["a", "b", "c", "d"]);
        v.sel_anchor = Some(0);
        v.cursor_row = Some(1);
        frame(&mut v, &ctx, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1), ["c", "d"]);
    }

    #[test]
    fn column_selections_copy_type_delete_and_paste() {
        let ctx = egui::Context::default();
        let mut v = two_way("let a = 1;\nlet b = 2;\nx\n", "let a = 1;\nlet b = 2;\nx\n");
        frame(&mut v, &ctx, vec![]);
        let sel = |a, b| {
            Some(Box::new(ColSel {
                pane: 1,
                anchor: a,
                head: b,
            }))
        };
        v.col_sel = sel((2, 6), (0, 4));
        let out = frame(&mut v, &ctx, vec![egui::Event::Copy]);
        assert!(out
            .platform_output
            .commands
            .contains(&egui::OutputCommand::CopyText("a \nb \n".into())));
        // Typing replaces the columns on every row and leaves a caret after the text.
        frame(&mut v, &ctx, vec![egui::Event::Text("v".into())]);
        assert_eq!(lines(&v, 1), ["let v= 1;", "let v= 2;", "x   v"]);
        assert_eq!(v.col().map(|s| (s.rows(), s.cols())), Some(((0, 2), 5..5)));
        frame(&mut v, &ctx, vec![egui::Event::Text(" ".into())]);
        assert_eq!(lines(&v, 1), ["let v = 1;", "let v = 2;", "x   v "]);
        // Backspace at a caret removes a column on each row.
        frame(&mut v, &ctx, vec![key(Key::Backspace, Modifiers::NONE)]);
        frame(&mut v, &ctx, vec![key(Key::Backspace, Modifiers::NONE)]);
        assert_eq!(lines(&v, 1), ["let = 1;", "let = 2;", "x   "]);
        // Each action is one undo step.
        v.undo();
        assert_eq!(lines(&v, 1), ["let v= 1;", "let v= 2;", "x   v"]);
        // A pasted block goes row by row.
        v.col_sel = sel((0, 0), (1, 3));
        frame(
            &mut v,
            &ctx,
            vec![egui::Event::Paste("var\nconst\n".into())],
        );
        assert_eq!(lines(&v, 1), ["var v= 1;", "const v= 2;", "x   v"]);
        // Cut copies the columns and removes them; Escape ends the selection.
        v.col_sel = sel((0, 0), (1, 4));
        let out = frame(&mut v, &ctx, vec![egui::Event::Cut]);
        assert!(out
            .platform_output
            .commands
            .contains(&egui::OutputCommand::CopyText("var \ncons".into())));
        assert_eq!(lines(&v, 1), ["v= 1;", "t v= 2;", "x   v"]);
        frame(&mut v, &ctx, vec![key(Key::Escape, Modifiers::NONE)]);
        assert!(v.col_sel.is_none());
        assert_eq!(
            lines(&v, 0),
            ["let a = 1;", "let b = 2;", "x"],
            "only its pane"
        );
    }

    #[test]
    fn alt_dragging_selects_columns() {
        let ctx = egui::Context::default();
        let text = "abcdefgh\nabcdefgh\nabcdefgh\n";
        let mut v = two_way(text, text);
        crate::ui::fonts::setup(&ctx, "en");
        frame(&mut v, &ctx, vec![]);
        let button = |pos, pressed, modifiers| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        // Find the heights of the first and third rows of the left pane by clicking.
        let mut at = [None; 3];
        for y in (0..500).step_by(3) {
            let p = pos2(120.0, y as f32);
            frame(
                &mut v,
                &ctx,
                vec![
                    egui::Event::PointerMoved(p),
                    button(p, true, Modifiers::NONE),
                ],
            );
            frame(&mut v, &ctx, vec![button(p, false, Modifiers::NONE)]);
            if let Some(r) = v.cursor_row.filter(|&r| r < 3 && v.active_pane == 0) {
                at[r].get_or_insert(p.y);
            }
            v.cursor_row = None;
        }
        let (Some(y0), Some(y2)) = (at[0], at[2]) else {
            panic!("rows not found: {at:?}");
        };
        let (from, to) = (pos2(120.0, y0 + 2.0), pos2(170.0, y2 + 2.0));
        frame_with(
            &mut v,
            &ctx,
            vec![
                egui::Event::PointerMoved(from),
                button(from, true, Modifiers::ALT),
            ],
            Modifiers::ALT,
        );
        for k in 1..=4 {
            let p = from + (to - from) * (k as f32 / 4.0);
            frame_with(
                &mut v,
                &ctx,
                vec![egui::Event::PointerMoved(p)],
                Modifiers::ALT,
            );
        }
        frame_with(
            &mut v,
            &ctx,
            vec![button(to, false, Modifiers::ALT)],
            Modifiers::ALT,
        );
        let s = v.col().expect("a column selection");
        assert_eq!((s.pane, s.rows()), (0, (0, 2)));
        assert!(!s.cols().is_empty(), "{s:?}");
        assert!(v.selection().is_none(), "no row selection with it");
        // A plain click ends it.
        frame(
            &mut v,
            &ctx,
            vec![
                egui::Event::PointerMoved(from),
                button(from, true, Modifiers::NONE),
            ],
        );
        frame(&mut v, &ctx, vec![button(from, false, Modifiers::NONE)]);
        assert!(v.col_sel.is_none());
    }

    #[test]
    fn block_and_column_edits_keep_line_endings_and_bom_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        std::fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\r\nthree\r\n").unwrap();
        let left = TextDoc::from_text(None, "one\ntwo\nthree\n");
        let right = text::load_doc(&path, None).unwrap();
        let mut v = TextView::new(vec![left, right], &Config::default());
        // A paste over a selection across lines, then a column insert.
        v.place_caret(1, (0, 0), false);
        v.place_caret(1, (1, 3), true);
        v.caret_insert("ONE\nTWO\nnew");
        v.col_sel = Some(Box::new(ColSel {
            pane: 1,
            anchor: (0, 0),
            head: (3, 0),
        }));
        v.col_replace(&["> ".to_string()]);
        assert_eq!(lines(&v, 1), ["> ONE", "> TWO", "> new", "> three"]);
        v.save_pane(1).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"\xEF\xBB\xBF> ONE\r\n> TWO\r\n> new\r\n> three\r\n"
        );
        // Two undo steps bring back the exact text.
        v.undo();
        v.undo();
        assert_eq!(lines(&v, 1), ["one", "two", "three"]);
    }

    #[test]
    fn edits_in_a_large_file_stay_quick() {
        let text: String = (0..100_000)
            .map(|i| format!("line {i} of the file\n"))
            .collect();
        let mut v = two_way(&text, &text.replace("line 50000 ", "LINE 50000 "));
        let start = std::time::Instant::now();
        v.col_sel = Some(Box::new(ColSel {
            pane: 1,
            anchor: (10, 0),
            head: (20, 0),
        }));
        for ch in ["a", "b", "c"] {
            v.col_replace(&[ch.to_string()]);
        }
        // Typing at the caret: three keys, Enter and a paste of two lines.
        v.place_caret(1, (30, 0), false);
        for ch in ["x", "y", "z"] {
            v.caret_insert(ch);
        }
        v.caret_newline();
        v.caret_insert("p\nq");
        let took = start.elapsed();
        eprintln!("8 edits of a 100,000-line file: {took:?}");
        assert!(lines(&v, 1)[10].starts_with("abcline 10"));
        assert_eq!(lines(&v, 1)[30..33], ["xyz", "p", "qline 30 of the file"]);
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    }

    #[test]
    fn copies_selected_lines_and_undoes() {
        let mut v = two_way("a\nb\nc\nd\n", "a\nB\nC\nd\n");
        v.cursor_row = Some(0);
        assert_eq!(v.selected_text(0).as_deref(), Some("a"));
        v.sel_anchor = Some(0);
        v.cursor_row = Some(1);
        assert_eq!(v.selection(), Some((0, 1)));
        assert_eq!(v.selected_text(1).as_deref(), Some("a\nB\n"));

        // copy only the first changed line of the block to the right
        let row_b = v.diff.row_of_line(0, 1);
        v.sel_anchor = Some(row_b);
        v.cursor_row = Some(v.diff.row_of_line(0, 0));
        v.copy_current(true);
        assert_eq!(lines(&v, 1), ["a", "b", "C", "d"]);
        assert!(v.panes[1].doc.modified && v.selection().is_none());

        v.undo();
        assert_eq!(lines(&v, 1), ["a", "B", "C", "d"]);
        v.redo();
        assert_eq!(lines(&v, 1), ["a", "b", "C", "d"]);

        // copying into a pane with no lines in those rows inserts them
        let mut v = two_way("a\nnew\nb\n", "a\nb\n");
        let r = v.diff.row_of_line(0, 1);
        v.copy_rows(r, r, 0, 1);
        assert_eq!(lines(&v, 1), ["a", "new", "b"]);
        assert_eq!(v.outcome(), Outcome::Identical);
    }

    #[test]
    fn merges_three_way_and_replaces_in_the_active_pane() {
        let base = "one\ntwo\nthree\nfour\n";
        let docs = vec![
            TextDoc::from_text(None, "one\nTWO\nthree\nfour\n"),
            TextDoc::from_text(None, base),
            TextDoc::from_text(None, "one\ntwo\nthree\nFOUR\n"),
        ];
        let mut v = TextView::new(docs, &Config::default());
        v.auto_merge();
        assert_eq!(lines(&v, 1), ["one", "TWO", "three", "FOUR"]);
        assert_eq!(v.diff.conflicts(), 0);

        v.active_pane = 1;
        v.find.query = "t(\\w+)".into();
        v.find.regex = true;
        v.find.replace = "T$1".into();
        v.replace_everywhere();
        assert_eq!(lines(&v, 1), ["one", "TWO", "Three", "FOUR"]);
        v.find.match_case = true;
        v.find.query = "one".into();
        v.find.regex = false;
        v.find.replace = "1".into();
        v.cursor_row = None;
        v.replace_next();
        assert_eq!(v.cursor_row, Some(0));
        v.replace_next();
        assert_eq!(lines(&v, 1), ["1", "TWO", "Three", "FOUR"]);
        v.panes[0].doc.read_only = true;
        v.active_pane = 0;
        v.find.query = "TWO".into();
        v.replace_everywhere();
        assert_eq!(lines(&v, 0)[1], "TWO");
    }

    #[test]
    fn merges_into_the_output_file_like_git_mergetool() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p
        };
        let local = file("local", "a\nLOCAL\nc\nd\n");
        let base = file("base", "a\nb\nc\nd\n");
        let remote = file("remote", "a\nb\nc\nREMOTE\n");
        let merged = file("merged", "<<<<<<< conflict markers from git\n");
        let mut docs: Vec<TextDoc> = [&local, &base, &remote]
            .iter()
            .map(|p| text::load_doc(p, None).unwrap())
            .collect();
        redirect_output(&mut docs, &merged);
        let mut v = TextView::new(docs, &Config::default());
        assert!(v.panes[1].doc.modified);
        v.auto_merge();
        v.save_modified().unwrap();
        assert_eq!(
            std::fs::read_to_string(&merged).unwrap(),
            "a\nLOCAL\nc\nREMOTE\n"
        );
        assert_eq!(std::fs::read_to_string(&base).unwrap(), "a\nb\nc\nd\n");
        assert!(!v.is_modified());

        // two files: the right one is the result, even when nothing was changed
        let docs = vec![
            text::load_doc(&local, None).unwrap(),
            text::load_doc(&remote, None).unwrap(),
        ];
        let out = dir.path().join("out");
        let mut docs = docs;
        redirect_output(&mut docs, &out);
        let mut v = TextView::new(docs, &Config::default());
        v.save_modified().unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "a\nb\nc\nREMOTE\n");
    }

    #[test]
    fn folds_unchanged_runs_around_differences() {
        let a: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let b = a.replace("line 9\n", "changed\n");
        let docs = vec![TextDoc::from_text(None, &a), TextDoc::from_text(None, &b)];
        let mut v = TextView::new(docs, &Config::default());
        let folds = |v: &TextView| {
            v.layout
                .lines
                .iter()
                .filter(|l| matches!(l, VLine::Fold(..)))
                .count()
        };
        v.collapse = true;
        v.ensure_layout(0, 4);
        assert_eq!(folds(&v), 2);
        assert_eq!(v.layout.row_line.len(), v.diff.rows.len());
        assert_eq!(v.layout.lines[0], VLine::Fold(0, 6));
        v.reveal_row(2, 0, 4);
        assert_eq!(folds(&v), 1);
        assert_eq!(v.layout.lines[2], VLine::Row(2, 0));
    }

    #[test]
    fn stops_waiting_for_a_highlighter_that_died() {
        let cfg = Config::default();
        let docs = vec![
            text::TextDoc::from_text(Some("a.rs".into()), "fn main() {}\n"),
            text::TextDoc::from_text(Some("b.rs".into()), ""),
        ];
        let mut v = TextView::new(docs, &cfg);
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        let p = &mut v.panes[0];
        (p.hl_rx, p.hl_dirty, p.hl_dark) = (Some(rx), false, cfg.dark_mode);
        v.poll_highlight(&cfg);
        assert!(v.panes[0].hl_rx.is_none() && v.panes[0].hl.is_none());
    }

    #[test]
    fn handles_files_changed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        touch(&a, "one\ntwo\n", 100);
        touch(&b, "one\nTWO\n", 100);
        let docs = [&a, &b].map(|p| text::load_doc(p, None).unwrap()).to_vec();
        let mut v = TextView::new(docs, &Config::default());

        // Unmodified pane: reloaded silently.
        touch(&a, "one\nthree\n", 50);
        v.check_disk();
        assert_eq!(v.panes[0].doc.lines, ["one", "three"]);
        assert!(v.disk_changed.is_empty());

        // Pane with edits: reported, then reloaded on request; undo brings the edits back.
        v.push_undo();
        v.panes[0].doc.lines[1] = "edited".into();
        v.mark_changed(&[0]);
        touch(&a, "one\nfour\n", 10);
        v.check_disk();
        assert_eq!(v.disk_changed, [0]);
        assert_eq!(v.panes[0].doc.lines[1], "edited");
        v.reload_changed();
        assert_eq!(v.panes[0].doc.lines, ["one", "four"]);
        assert!(!v.panes[0].doc.modified);
        v.undo();
        assert_eq!(v.panes[0].doc.lines[1], "edited");
        assert!(v.panes[0].doc.modified);
    }

    #[test]
    fn undo_after_save_marks_pane_modified() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        touch(&a, "one\n", 100);
        touch(&b, "two\n", 100);
        let docs = [&a, &b].map(|p| text::load_doc(p, None).unwrap()).to_vec();
        let mut v = TextView::new(docs, &Config::default());
        v.push_undo();
        v.panes[0].doc.lines[0] = "edited".into();
        v.mark_changed(&[0]);
        v.save_pane(0).unwrap();
        assert!(!v.panes[0].doc.modified);
        v.undo();
        assert!(
            v.panes[0].doc.modified,
            "disk has the edit, the pane does not"
        );
        v.redo();
        assert!(!v.panes[0].doc.modified, "back to the saved text");
    }

    /// The differences of `v`, as a fresh comparison of the same documents would find them.
    fn diff_state(v: &TextView) -> String {
        format!("{:?}{:?}", v.diff.rows, v.diff.blocks)
    }

    fn fresh_diff_state(v: &TextView) -> String {
        let docs = v.panes.iter().map(|p| p.doc.clone()).collect();
        diff_state(&TextView::new(docs, &Config::default()))
    }

    #[test]
    fn typing_pastes_hunk_copies_and_merges_undo_and_redo_one_step_at_a_time() {
        let docs = [
            "a\nleft 1\nb\nleft 2\nc\nd\ne\n",
            "a\nbase 1\nb\nbase 2\nc\nd\ne\n",
            "a\nright 1\nb\nright 2\nc\nd\ne\n",
        ]
        .map(|t| TextDoc::from_text(None, t))
        .to_vec();
        let mut v = TextView::new(docs, &Config::default());
        let sides = |v: &TextView| (lines(v, 0).join("|"), lines(v, 2).join("|"));
        let outside = sides(&v);
        let mut states = vec![lines(&v, 1).join("|")];
        let mut step = |v: &mut TextView, f: &dyn Fn(&mut TextView)| {
            f(v);
            states.push(lines(v, 1).join("|"));
        };
        // typing in a line is one step, however many keys
        step(&mut v, &|v| {
            v.place_caret(1, (6, 1), false);
            for ch in ["x", "y", "z"] {
                v.caret_insert(ch);
            }
        });
        // a paste right after typing is a step of its own
        step(&mut v, &|v| v.caret_paste("P"));
        step(&mut v, &|v| v.caret_insert("q"));
        // a hunk copied from the left, then the next one taken from both sides
        step(&mut v, &|v| {
            let b = v.significant().next().unwrap();
            v.copy_block(b, 0, 1)
        });
        step(&mut v, &|v| {
            let b = v.diff.blocks.iter().position(|b| b.ranges[1].contains(&3));
            v.select_block(b.unwrap());
            v.take_both(true);
        });
        // typing again after the merge starts a new step
        step(&mut v, &|v| {
            v.place_caret(1, (0, 0), false);
            v.caret_insert("!");
            v.caret_insert("!");
        });
        assert_eq!(
            states,
            [
                "a|base 1|b|base 2|c|d|e",
                "a|base 1|b|base 2|c|d|exyz",
                "a|base 1|b|base 2|c|d|exyzP",
                "a|base 1|b|base 2|c|d|exyzPq",
                "a|left 1|b|base 2|c|d|exyzPq",
                "a|left 1|b|left 2|right 2|c|d|exyzPq",
                "!!a|left 1|b|left 2|right 2|c|d|exyzPq",
            ]
        );
        for want in states.iter().rev().skip(1) {
            v.undo();
            assert_eq!(lines(&v, 1).join("|"), *want);
            assert_eq!(sides(&v), outside, "undo never touches the other panes");
            assert_eq!(diff_state(&v), fresh_diff_state(&v));
        }
        assert!(!v.can_undo());
        assert!(!v.panes[1].doc.modified, "back to the text as loaded");
        for want in states.iter().skip(1) {
            v.redo();
            assert_eq!(lines(&v, 1).join("|"), *want);
            assert_eq!(sides(&v), outside);
            assert_eq!(diff_state(&v), fresh_diff_state(&v));
            assert!(v.panes[1].doc.modified);
        }
        assert!(!v.can_redo());
    }

    #[test]
    fn redo_is_dropped_only_by_a_new_edit() {
        let mut v = two_way("one\n", "two\n");
        v.copy_all(0, 1);
        v.undo();
        assert!(v.can_redo());
        // no change: copying what is already there, a caret move, a delete at the start
        v.copy_rows(0, 0, 1, 1);
        v.place_caret(1, (0, 0), false);
        v.caret_delete(false, false);
        v.caret_paste("");
        assert!(v.can_redo(), "nothing was edited");
        v.panes[0].doc.read_only = true;
        v.copy_all(1, 0);
        v.place_caret(0, (0, 0), false);
        v.caret_insert("x");
        assert_eq!(lines(&v, 0), ["one"]);
        assert!(v.can_redo(), "a read-only pane takes no edit");
        v.place_caret(1, (0, 0), false);
        v.caret_insert("x");
        assert!(!v.can_redo());
        assert_eq!(lines(&v, 1), ["xtwo"]);
    }

    #[test]
    fn a_save_ends_the_typing_step_so_that_undo_stops_at_the_saved_text() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        touch(&a, "one\n", 100);
        touch(&b, "two\n", 100);
        let docs = [&a, &b].map(|p| text::load_doc(p, None).unwrap()).to_vec();
        let mut v = TextView::new(docs, &Config::default());
        v.place_caret(1, (0, 3), false);
        v.caret_insert("a");
        v.caret_insert("b");
        v.save_pane(1).unwrap();
        v.caret_insert("c");
        assert!(v.panes[1].doc.modified);
        v.undo();
        assert_eq!(lines(&v, 1), ["twoab"]);
        assert!(!v.panes[1].doc.modified, "the text is the saved one");
        v.undo();
        assert_eq!(lines(&v, 1), ["two"]);
        assert!(v.panes[1].doc.modified, "the file has the typing");
        v.redo();
        assert!(!v.panes[1].doc.modified);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "twoab\n");
    }

    #[test]
    fn save_pane_flags_saved_so_the_app_can_repack_its_archive_but_save_pane_as_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        touch(&a, "one\n", 100);
        touch(&b, "two\n", 100);
        let docs = [&a, &b].map(|p| text::load_doc(p, None).unwrap()).to_vec();
        let mut v = TextView::new(docs, &Config::default());
        v.place_caret(1, (0, 3), false);
        v.caret_insert("a");

        // However it is called (`Cmd::Save`, or a button like the merge view's own "Save
        // result", which calls it directly), `save_pane` must flag that a save happened, so
        // the app can repack the archive this tab was opened from, if any (see
        // `App::refresh_saved_folders`). `save_pane_as` writes to a path the user picked
        // instead, unrelated to any extraction this tab may have come from, so it does not
        // set the flag (it needs a file dialog, so it is not exercised here).
        assert!(!v.saved);
        v.save_pane(1).unwrap();
        assert!(v.saved);
    }

    /// Text read from a notebook can be saved as a text file, which it can then be edited as,
    /// but not over the notebook or any other file read by converting it.
    #[test]
    fn text_read_from_a_notebook_saves_as_a_text_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (nb, b) = (dir.path().join("a.ipynb"), dir.path().join("b.txt"));
        let notebook = r#"{"cells":[{"cell_type":"code","source":["print(1)\n"],"metadata":{},"outputs":[],"execution_count":null}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(&nb, notebook).unwrap();
        touch(&b, "two\n", 100);
        let cfg = Config::default();
        let read = crate::core::unpacker::load_doc(&nb, &cfg.comparison_options(), None).unwrap();
        assert!(read.compare_only);
        let docs = vec![read, text::load_doc(&b, None).unwrap()];
        let mut v = TextView::new(docs, &cfg);

        let other = dir.path().join("c.ipynb");
        assert!(v.save_pane_to(0, &nb).is_err());
        assert!(v.save_pane_to(0, &other).is_err());
        // Nor once documents are no longer read as text: the file still is one.
        v.opts.extract_documents = false;
        assert!(v.save_pane_to(0, &other).is_err());
        v.opts.extract_documents = true;
        assert!(!other.exists());
        assert_eq!(std::fs::read_to_string(&nb).unwrap(), notebook);

        let txt = dir.path().join("c.txt");
        v.save_pane_to(0, &txt).unwrap();
        assert!(!v.panes[0].doc.compare_only && !v.panes[0].doc.read_only);
        assert!(std::fs::read_to_string(&txt).unwrap().contains("print(1)"));
        v.place_caret(0, (0, 0), false);
        v.caret_insert("x");
        v.save_pane(0).unwrap();
        assert!(std::fs::read_to_string(&txt).unwrap().starts_with('x'));
    }

    #[test]
    fn undo_leaves_read_only_panes_alone_and_older_text_counts_as_unsaved() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        touch(&a, "one\n", 100);
        touch(&b, "two\n", 100);
        let docs = [&a, &b].map(|p| text::load_doc(p, None).unwrap()).to_vec();
        let mut v = TextView::new(docs, &Config::default());

        // A pane made read-only after its edit keeps it: undo refuses, and keeps the step.
        v.place_caret(0, (0, 0), false);
        v.caret_insert("x");
        v.panes[0].doc.read_only = true;
        v.undo();
        assert_eq!(lines(&v, 0), ["xone"]);
        assert!(v.can_undo());
        assert_eq!(v.status, tr("msg.readonly"));
        v.panes[0].doc.read_only = false;
        v.undo();
        assert_eq!(lines(&v, 0), ["one"]);

        // A read-only pane reloaded from disk keeps the file's text through an undo of
        // another pane's edit.
        v.panes[0].doc.read_only = true;
        v.place_caret(1, (0, 0), false);
        v.caret_insert("y");
        touch(&a, "ONE\n", 50);
        v.check_disk();
        assert_eq!(lines(&v, 0), ["ONE"]);
        v.undo();
        assert_eq!((lines(&v, 0), lines(&v, 1)), (vec!["ONE"], vec!["two"]));
        assert!(!v.panes[0].doc.modified && !v.panes[1].doc.modified);
        v.redo();
        assert_eq!((lines(&v, 0), lines(&v, 1)), (vec!["ONE"], vec!["ytwo"]));

        // A writable pane reloaded from disk gets its older text back as unsaved.
        v.panes[0].doc.read_only = false;
        v.place_caret(1, (0, 0), false);
        v.caret_paste("z");
        touch(&a, "uno\n", 20);
        v.check_disk();
        assert_eq!(lines(&v, 0), ["uno"]);
        v.undo();
        assert_eq!(lines(&v, 0), ["ONE"]);
        assert!(v.panes[0].doc.modified, "the file holds another text now");
        assert_eq!(diff_state(&v), fresh_diff_state(&v));
    }

    #[test]
    fn an_edit_makes_stale_only_the_review_of_the_hunk_it_changed() {
        use crate::core::review_state::{Decision, State};
        let a: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let b = a
            .replace("line 5\n", "five\n")
            .replace("line 20\n", "twenty\n");
        let mut v = two_way(&a, &b);
        assert_eq!(v.diff.blocks.len(), 2);
        let ids: Vec<String> = v.diff.blocks.iter().map(|b| v.review_hunk_id(b)).collect();
        let mut review = Decision::default();
        for id in &ids {
            let d = Decision {
                id: id.clone(),
                state: State::Accepted,
                ..Default::default()
            };
            review.hunks.insert(id.clone(), Box::new(d));
        }
        v.review_decision = Some(Box::new(review));
        v.review_dirty = false;
        let stale = |v: &TextView| -> Vec<bool> {
            let r = v.review_decision.as_ref().unwrap();
            ids.iter().map(|id| r.hunks[id].stale).collect()
        };
        // Typing in the second hunk: the file and that hunk are stale, the first is not.
        v.place_caret(1, (20, 6), false);
        v.caret_insert("!");
        assert_eq!(lines(&v, 1)[20], "twenty!");
        assert!(v.review_decision.as_ref().unwrap().stale && v.review_dirty);
        assert_eq!(stale(&v), [false, true]);
        // Undo gives the hunk back as it was, but its decision stays stale.
        v.undo();
        assert_eq!(lines(&v, 1)[20], "twenty");
        assert_eq!(stale(&v), [false, true]);
        // A line next to the first hunk is part of its identity.
        v.place_caret(1, (6, 0), false);
        v.caret_insert("x");
        assert_eq!(stale(&v), [true, true]);
    }

    #[test]
    fn undo_marks_a_review_stale_and_keeps_the_review_out_of_the_saved_state() {
        let mut v = two_way("one\n", "two\n");
        v.copy_all(0, 1);
        v.review_decision = Some(Box::default());
        v.review_dirty = false;
        let modified = v.panes[1].doc.modified;
        v.undo();
        let review = v.review_decision.as_ref().unwrap();
        assert!(review.stale && v.review_dirty);
        assert!(
            !v.panes[1].doc.modified && modified,
            "only the text says what is unsaved"
        );
        v.review_decision.as_mut().unwrap().stale = false;
        v.undo();
        assert!(
            !v.review_decision.as_ref().unwrap().stale,
            "nothing to undo, nothing changed"
        );
    }

    #[test]
    fn a_new_file_is_written_like_the_one_copied_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new.txt");
        let docs = vec![
            TextDoc::from_text(None, "one\r\ntwo"),
            TextDoc::empty(Some(new.clone())),
        ];
        let mut v = TextView::new(docs, &Config::default());
        v.copy_all(0, 1);
        let d = &v.panes[1].doc;
        assert_eq!((d.eol, d.final_newline), (Eol::CrLf, false));
        v.save_pane(1).unwrap();
        assert_eq!(std::fs::read(&new).unwrap(), b"one\r\ntwo");
        v.undo();
        assert_eq!(v.panes[1].doc.eol, Eol::platform());
        assert!(v.panes[1].doc.final_newline);
        v.redo();
        assert_eq!(v.panes[1].doc.eol, Eol::CrLf);

        // Undoing an edit keeps the line ending chosen since, and a mixed file stays mixed only
        // until saved.
        let mixed = TextDoc::from_text(Some(dir.path().join("mixed.txt")), "a\nb\r\n");
        let mut v = TextView::new(
            vec![TextDoc::from_text(None, "x\n"), mixed],
            &Config::default(),
        );
        v.push_undo();
        v.panes[1].doc.lines[0] = "edited".into();
        v.mark_changed(&[1]);
        v.panes[1].doc.eol = Eol::Cr;
        v.save_pane(1).unwrap();
        assert!(!v.panes[1].doc.mixed_eol);
        v.undo();
        assert_eq!(v.panes[1].doc.eol, Eol::Cr);
        assert!(!v.panes[1].doc.mixed_eol);

        let mut v = TextView::new(
            vec![v.panes[0].doc.clone(), TextDoc::empty(None)],
            &Config::default(),
        );
        v.panes[0].doc.mixed_eol = true;
        v.copy_all(0, 1);
        assert!(!v.panes[1].doc.mixed_eol);

        // A line ending chosen is undone like an edit, and with it the mark of a change.
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(None, "x\n"),
                TextDoc::from_text(None, "a\nb\n"),
            ],
            &Config::default(),
        );
        v.set_eol(1, Eol::CrLf);
        assert!(v.panes[1].doc.modified);
        v.undo();
        assert_eq!(v.panes[1].doc.eol, Eol::Lf);
        assert!(!v.panes[1].doc.modified);
        v.redo();
        assert_eq!(v.panes[1].doc.eol, Eol::CrLf);
        assert!(v.panes[1].doc.modified);

        // A line ending chosen for a new file before typing into it survives undo.
        let mut v = TextView::new(
            vec![TextDoc::from_text(None, "x\n"), TextDoc::empty(None)],
            &Config::default(),
        );
        v.set_eol(1, Eol::Cr);
        v.push_undo();
        v.panes[1].doc.lines = vec!["typed".into()];
        v.mark_changed(&[1]);
        v.undo();
        assert_eq!(v.panes[1].doc.eol, Eol::Cr);
        v.redo();
        assert_eq!(v.panes[1].doc.eol, Eol::Cr);

        // Copying nothing into an empty file leaves its format alone.
        let mut v = TextView::new(
            vec![TextDoc::empty(None), TextDoc::empty(None)],
            &Config::default(),
        );
        v.panes[0].doc.eol = Eol::Cr;
        v.copy_all(0, 1);
        assert_eq!(v.panes[1].doc.eol, Eol::platform());

        // Taking both sides or merging into an empty middle takes a side's format too.
        let three = || {
            let crlf = TextDoc::from_text(None, "a\r\nb\r\n");
            let lf = TextDoc::from_text(None, "c\n");
            TextView::new(vec![crlf, TextDoc::empty(None), lf], &Config::default())
        };
        let mut v = three();
        let b = v.significant().next().unwrap();
        v.take_both_at(b, true);
        assert_eq!(v.panes[1].doc.eol, Eol::CrLf);
        v.undo();
        assert_eq!(v.panes[1].doc.eol, Eol::platform());
        let mut v = three();
        let b = v.significant().next().unwrap();
        v.take_both_at(b, false);
        assert_eq!(v.panes[1].doc.eol, Eol::Lf);
        let mut v = TextView::new(
            vec![
                TextDoc::from_text(None, "a\r\n"),
                TextDoc::empty(None),
                TextDoc::empty(None),
            ],
            &Config::default(),
        );
        v.auto_merge();
        assert_eq!(v.panes[1].doc.lines, vec!["a".to_string()]);
        assert_eq!(v.panes[1].doc.eol, Eol::CrLf);
    }

    #[test]
    fn undo_steps_below_the_top_keep_only_the_lines_they_changed() {
        let text: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let mut v = two_way(&text, &text);
        let mut states = vec![v.panes[1].doc.lines.clone()];
        for step in 0..30usize {
            v.push_undo();
            let lines = &mut v.panes[1].doc.lines;
            match step % 3 {
                0 => lines[step * 600] = format!("changed {step}"),
                1 => lines.insert(step * 600, format!("inserted {step}")),
                _ => drop(lines.remove(step * 600)),
            }
            v.mark_changed(&[1]);
            states.push(v.panes[1].doc.lines.clone());
        }
        let below =
            |steps: &[Snapshot]| -> usize { steps.iter().rev().skip(1).map(Snapshot::size).sum() };
        assert_eq!(v.undo.len(), 30);
        assert!(below(&v.undo) <= 2 * 30, "{} lines held", below(&v.undo));
        for state in states.iter().rev().skip(1) {
            v.undo();
            assert!(v.panes[1].doc.lines == *state);
            assert_eq!(v.panes[0].doc.lines, states[0]);
        }
        assert!(!v.can_undo());
        assert!(below(&v.redo) <= 2 * 30, "{} lines held", below(&v.redo));
        for state in states.iter().skip(1) {
            v.redo();
            assert!(v.panes[1].doc.lines == *state);
        }
        assert!(!v.can_redo());
        for state in states.iter().rev().skip(1).take(5) {
            v.undo();
            assert!(v.panes[1].doc.lines == *state);
        }
    }
}
