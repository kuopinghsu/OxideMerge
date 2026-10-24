//! File compare / merge view for text files (2-way and 3-way).

use crate::config::Config;
use crate::core::comments;
use crate::core::diff::{self, BlockKind, DiffOptions, DiffResult, InlineMode};
use crate::core::patch::{self, PatchStyle};
use crate::core::prettify;
use crate::core::report::side_name;
use crate::core::saved::{SavedCompare, SavedPane};
use crate::core::table;
use crate::core::text::{self, Eol, TextDoc, TextEncoding, COMMON_ENCODINGS};
use crate::i18n::{tr, trf};
use crate::ui::icons;
use crate::ui::syntax::{self, LineSpans};
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, Icon, Mark, Outcome};
use egui::text::LayoutJob;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Galley, Key, Modifiers, Rect, RichText, Sense, Stroke,
    StrokeKind, TextFormat, Ui,
};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::mpsc::Receiver;
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
    /// The lines as loaded, to tell which lines were edited since.
    baseline: Vec<String>,
    /// Lines inserted or changed since the document was loaded.
    edited: HashSet<usize>,
}

/// Modification time of a document's file. Documents with a description (unpacked or
/// generated text) do not mirror their file, so they are not watched.
fn disk_time(doc: &TextDoc) -> Option<SystemTime> {
    if doc.description.is_some() {
        return None;
    }
    std::fs::metadata(doc.path.as_ref()?).ok()?.modified().ok()
}

impl Pane {
    fn new(doc: TextDoc) -> Self {
        let disk_time = disk_time(&doc);
        let baseline = doc.lines.clone();
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
        self.baseline = self.doc.lines.clone();
        self.edited.clear();
    }
}

/// Lines of `new` inserted or changed since `old`.
fn edited_lines(old: &[String], new: &[String]) -> HashSet<usize> {
    if old == new {
        return HashSet::new();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    similar::capture_diff_deadline(
        similar::Algorithm::Myers,
        old,
        0..old.len(),
        new,
        0..new.len(),
        Some(deadline),
    )
    .iter()
    .filter(|op| op.tag() != similar::DiffTag::Equal)
    .flat_map(|op| op.new_range())
    .collect()
}

/// Where each line of `old` ended up in `new` (deleted lines map to where they were).
fn line_map(old: &[String], new: &[String]) -> Vec<usize> {
    let mut map = vec![0; old.len()];
    for op in similar::capture_diff_slices(similar::Algorithm::Myers, old, new) {
        let (tag, o, n) = op.as_tag_tuple();
        for (k, i) in o.enumerate() {
            map[i] = if tag == similar::DiffTag::Equal {
                n.start + k
            } else {
                n.start
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
    let name = doc
        .description
        .clone()
        .or_else(|| doc.path.as_ref().map(|p| p.display().to_string()))
        .unwrap_or_else(|| tr("untitled").to_string());
    d.description = Some(format!("{name} [{}]", tr("label.formatted")));
    Some(d)
}

struct EditState {
    row: usize,
    pane: usize,
    text: String,
    focus: bool,
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
    lines: Vec<Vec<String>>,
    modified: Vec<bool>,
    saves: Vec<u64>,
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
    d.read_only = false;
}

/// Opens the find bar with the replace field.
pub const REPLACE_KEY: &str = if cfg!(target_os = "macos") {
    "Ctrl+Alt+F"
} else {
    "Ctrl+H"
};

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
    /// (wrap columns or 0, tab size, folding, layout generation) the lines were built for.
    key: Option<(usize, usize, bool, u64)>,
    lines: Vec<VLine>,
    /// First display line of every diff row (the fold line for hidden rows).
    row_line: Vec<usize>,
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
}

/// A bookmark comment being edited: pane, line, text, and whether the field needs focus.
type NoteEdit = (usize, usize, String, bool);

pub struct TextView {
    pub panes: Vec<Pane>,
    pub diff: DiffResult,
    opts: DiffOptions,
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
    pub find: FindState,
    /// The compiled find query, rebuilt when the query or its options change.
    find_re: Option<(FindKey, Result<Regex, String>)>,
    pub goto: Option<String>,
    pub table_mode: bool,
    pub table_delim: Option<char>,
    col_widths: Vec<usize>,
    pub status: String,
    /// Message about an edit that changed many differences at once, for an undo toast.
    pub bulk_edit: Option<String>,
    scroll_to: Option<usize>,
    max_cols: usize,
    ctx_target: Option<(usize, usize)>,
    page_rows: usize,
    /// Modified panes whose file was changed by another program.
    disk_changed: Vec<usize>,
    layout: Layout,
    /// Bumped whenever rows or folds change so the layout is rebuilt.
    layout_gen: u64,
    /// Folds (by first hidden row) the user opened.
    expanded: HashSet<usize>,
    pub collapse: bool,
    pub wrap: bool,
    /// Animated scroll: (from, to, start time).
    scroll_anim: Option<(f32, f32, f64)>,
    /// Set when difference navigation ran past the last (true) or first (false) difference,
    /// so the app can move on to the next or previous file of the folder compare.
    pub step_file: Option<bool>,
    /// Bookmarked lines of each pane, with their comments (empty when there is none).
    pub bookmarks: Vec<BTreeMap<usize, String>>,
    note_edit: Option<NoteEdit>,
    /// Off for a preview that must leave the keyboard to the view around it.
    pub keys_enabled: bool,
    tab_size: usize,
    id: egui::Id,
}

impl TextView {
    pub fn new(docs: Vec<TextDoc>, cfg: &Config) -> Self {
        let table_delim = docs
            .iter()
            .filter_map(|d| d.path.as_deref())
            .find_map(table::delimiter_for);
        let bookmarks = vec![BTreeMap::new(); docs.len()];
        let mut v = TextView {
            panes: docs.into_iter().map(Pane::new).collect(),
            diff: DiffResult::default(),
            opts: cfg.diff.clone(),
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
            find: FindState::default(),
            find_re: None,
            goto: None,
            table_mode: table_delim.is_some(),
            table_delim,
            col_widths: Vec::new(),
            status: String::new(),
            bulk_edit: None,
            scroll_to: None,
            max_cols: 0,
            ctx_target: None,
            page_rows: 30,
            disk_changed: Vec::new(),
            layout: Layout::default(),
            layout_gen: 0,
            expanded: HashSet::new(),
            collapse: cfg.collapse_unchanged,
            wrap: cfg.word_wrap,
            scroll_anim: None,
            step_file: None,
            bookmarks,
            note_edit: None,
            keys_enabled: true,
            tab_size: cfg.tab_size,
            id: egui::Id::new(("textview", std::time::Instant::now())),
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

    pub fn is_modified(&self) -> bool {
        self.panes.iter().any(|p| p.doc.modified)
    }

    pub fn set_options(&mut self, opts: &DiffOptions) {
        if self.opts != *opts {
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
        let syntax = self.comment_syntax();
        let refs: Vec<&[String]> = self.panes.iter().map(|p| p.doc.lines.as_slice()).collect();
        self.diff = diff::compute_with(&refs, &self.opts, syntax);
        for (p, marks) in self.bookmarks.iter_mut().enumerate() {
            let n = self.panes[p].doc.lines.len();
            marks.retain(|&l, _| l < n);
        }
        for pane in &mut self.panes {
            pane.edited = edited_lines(&pane.baseline, &pane.doc.lines);
        }
        self.inline_cache.clear();
        self.sel_anchor = None;
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

    fn mark_changed(&mut self, panes: &[usize]) {
        for &p in panes {
            self.panes[p].doc.modified = true;
            self.panes[p].hl_dirty = true;
            if let Some(before) = self.undo.last().map(|s| s.lines[p].clone()) {
                self.remap_bookmarks(p, &before);
            }
        }
        self.rediff();
    }

    /// Moves the bookmarks of pane `p` along with its lines after they changed from `before`.
    fn remap_bookmarks(&mut self, p: usize, before: &[String]) {
        if self.bookmarks[p].is_empty() {
            return;
        }
        let map = line_map(before, &self.panes[p].doc.lines);
        let mut moved = BTreeMap::new();
        for (l, note) in std::mem::take(&mut self.bookmarks[p]) {
            if let Some(&to) = map.get(l) {
                moved.entry(to).or_insert(note);
            }
        }
        self.bookmarks[p] = moved;
    }

    // ---------- bookmarks ----------

    /// The pane and line a bookmark toggled on `row` refers to: the active pane's line, or the
    /// first line on that row.
    fn bookmark_target(&self, row: usize, pane: usize) -> Option<(usize, usize)> {
        let r = self.diff.rows.get(row)?;
        r.line[pane]
            .map(|l| (pane, l))
            .or_else(|| (0..self.n()).find_map(|p| r.line[p].map(|l| (p, l))))
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
        self.diff.rows[r].line[p].is_some_and(|l| self.bookmarks[p].contains_key(&l))
    }

    /// The comment of a bookmark on row `r`, in the pane that has one.
    fn row_note(&self, r: usize) -> Option<&str> {
        let row = self.diff.rows.get(r)?;
        (0..self.n()).find_map(|p| {
            let note = self.bookmarks[p].get(&row.line[p]?)?;
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
                    .block
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
        self.cur_block = Some(b);
        let row = self.diff.blocks[b].first_row;
        self.cursor_row = Some(row);
        self.scroll_to = Some(row);
    }

    pub fn first_diff(&mut self) {
        let first = self.significant().next();
        if let Some(b) = first {
            self.select_block(b);
        }
    }

    pub fn last_diff(&mut self) {
        let last = self.significant().last();
        if let Some(b) = last {
            self.select_block(b);
        }
    }

    /// Selects the next difference; false when there is none after the cursor.
    pub fn next_diff(&mut self, conflicts_only: bool) -> bool {
        let row = self.cursor_row;
        let found = self
            .significant()
            .filter(|&b| !conflicts_only || self.diff.blocks[b].kind == BlockKind::Conflict)
            .find(|&b| row.is_none_or(|r| self.diff.blocks[b].first_row > r));
        if let Some(b) = found {
            self.select_block(b);
        }
        found.is_some()
    }

    /// Selects the previous difference; false when there is none before the cursor.
    pub fn prev_diff(&mut self, conflicts_only: bool) -> bool {
        let row = self.cursor_row.unwrap_or(self.diff.rows.len());
        let containing = self.diff.rows.get(row).and_then(|r| r.block);
        let found = self
            .significant()
            .filter(|&b| !conflicts_only || self.diff.blocks[b].kind == BlockKind::Conflict)
            .filter(|&b| self.diff.blocks[b].first_row < row && Some(b) != containing)
            .last();
        if let Some(b) = found {
            self.select_block(b);
        }
        found.is_some()
    }

    pub fn diff_position(&self) -> (Option<usize>, usize) {
        let sig: Vec<usize> = self.significant().collect();
        let pos = self
            .cur_block
            .and_then(|c| sig.iter().position(|&b| b == c));
        (pos, sig.len())
    }

    // ---------- merging ----------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.panes.iter().map(|p| p.doc.lines.clone()).collect(),
            modified: self.panes.iter().map(|p| p.doc.modified).collect(),
            saves: self.panes.iter().map(|p| p.saves).collect(),
        }
    }

    fn push_undo(&mut self) {
        let s = self.snapshot();
        // Each snapshot copies every pane, so keep fewer levels for large files.
        let lines: usize = s.lines.iter().map(Vec::len).sum();
        let max = (5_000_000 / lines.max(1)).clamp(10, 200);
        self.undo.push(s);
        if self.undo.len() > max {
            self.undo.drain(..self.undo.len() - max);
        }
        self.redo.clear();
    }

    fn restore(&mut self, s: Snapshot) {
        for (i, lines) in s.lines.into_iter().enumerate() {
            if self.panes[i].doc.lines == lines {
                continue;
            }
            let before = std::mem::replace(&mut self.panes[i].doc.lines, lines);
            self.panes[i].hl_dirty = true;
            self.remap_bookmarks(i, &before);
        }
        for (i, p) in self.panes.iter_mut().enumerate() {
            p.doc.modified = s.modified[i] || s.saves[i] != p.saves;
        }
        self.rediff();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let cur = self.snapshot();
            self.redo.push(cur);
            self.restore(s);
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let cur = self.snapshot();
            self.undo.push(cur);
            self.restore(s);
        }
    }

    fn replace_block_lines(&mut self, b: usize, from: usize, to: usize) {
        let blk = self.diff.blocks[b].clone();
        let src: Vec<String> = self.panes[from].doc.lines[blk.ranges[from].clone()].to_vec();
        self.panes[to].doc.lines.splice(blk.ranges[to].clone(), src);
    }

    fn writable(&mut self, pane: usize) -> bool {
        if self.panes[pane].doc.read_only {
            self.status = tr("msg.readonly").to_string();
            false
        } else {
            true
        }
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
        self.push_undo();
        let row = self.diff.blocks[b].first_row;
        self.replace_block_lines(b, from, to);
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
            .filter_map(|r| r.line[pane])
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
        if from == to || to >= self.n() || r1 >= self.diff.rows.len() || !self.writable(to) {
            return;
        }
        let rows = &self.diff.rows[r0..=r1];
        let src: Vec<String> = rows
            .iter()
            .filter_map(|r| r.line[from])
            .map(|l| self.panes[from].doc.lines[l].clone())
            .collect();
        let dst: Vec<usize> = rows.iter().filter_map(|r| r.line[to]).collect();
        let range = match (dst.first(), dst.last()) {
            (Some(&a), Some(&b)) => a..b + 1,
            _ => {
                let at = self.insert_index(to, r0);
                at..at
            }
        };
        if self.panes[to].doc.lines[range.clone()] == src[..] {
            return;
        }
        let n = src.len().max(range.len());
        let start = range.start;
        self.push_undo();
        self.panes[to].doc.lines.splice(range, src);
        self.mark_changed(&[to]);
        let row = self.diff.row_of_line(to, start);
        self.cursor_row = Some(row);
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block);
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
        let blocks: Vec<usize> = self.significant().collect();
        for &b in blocks.iter().rev() {
            self.replace_block_lines(b, from, to);
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
        let start = blk.ranges[1].start;
        self.panes[1].doc.lines.splice(blk.ranges[1].clone(), lines);
        self.mark_changed(&[1]);
        let row = self.diff.row_of_line(1, start);
        self.cursor_row = Some(row);
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block);
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
    pub fn auto_merge(&mut self) {
        if self.n() != 3 || !self.writable(1) {
            return;
        }
        self.push_undo();
        let blocks: Vec<(usize, BlockKind)> = self
            .significant()
            .map(|b| (b, self.diff.blocks[b].kind))
            .collect();
        let mut merged = 0;
        for &(b, kind) in blocks.iter().rev() {
            let from = match kind {
                BlockKind::LeftOnly | BlockKind::BothSame => 0,
                BlockKind::RightOnly => 2,
                _ => continue,
            };
            self.replace_block_lines(b, from, 1);
            merged += 1;
        }
        self.mark_changed(&[1]);
        self.cur_block = None;
        self.status = trf(
            "msg.auto_merged",
            &[
                ("n", merged.to_string()),
                ("c", self.diff.conflicts().to_string()),
            ],
        );
        if merged > 0 {
            self.bulk_edit = Some(self.status.clone());
        }
        if self.diff.conflicts() > 0 {
            self.cursor_row = Some(0);
            self.next_diff(true);
        }
    }

    // ---------- editing ----------

    fn insert_index(&self, pane: usize, row: usize) -> usize {
        for r in (0..row.min(self.diff.rows.len())).rev() {
            if let Some(l) = self.diff.rows[r].line[pane] {
                return l + 1;
            }
        }
        0
    }

    fn begin_edit(&mut self, row: usize, pane: usize) {
        if self.panes[pane].doc.read_only {
            self.status = tr("msg.readonly").to_string();
            return;
        }
        let text = self
            .diff
            .rows
            .get(row)
            .and_then(|r| r.line[pane])
            .map(|l| self.panes[pane].doc.lines[l].clone())
            .unwrap_or_default();
        self.edit = Some(EditState {
            row,
            pane,
            text,
            focus: true,
        });
    }

    fn commit_edit(&mut self, e: EditState) {
        let new_lines: Vec<String> = e
            .text
            .split('\n')
            .map(|s| s.trim_end_matches('\r').to_string())
            .collect();
        let existing = self.diff.rows.get(e.row).and_then(|r| r.line[e.pane]);
        if let Some(l) = existing {
            if new_lines.len() == 1 && self.panes[e.pane].doc.lines[l] == new_lines[0] {
                return;
            }
        }
        let insert_at = self.insert_index(e.pane, e.row);
        self.push_undo();
        let lines = &mut self.panes[e.pane].doc.lines;
        match existing {
            Some(l) => {
                lines.splice(l..l + 1, new_lines);
            }
            None => {
                lines.splice(insert_at..insert_at, new_lines);
            }
        }
        self.mark_changed(&[e.pane]);
    }

    fn insert_below(&mut self, row: usize, pane: usize) {
        if !self.writable(pane) {
            return;
        }
        let at = match self.diff.rows.get(row).and_then(|r| r.line[pane]) {
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
            let lines: Vec<usize> = self.diff.rows[r0..=r1]
                .iter()
                .filter_map(|r| r.line[p])
                .collect();
            match (lines.first(), lines.last()) {
                (Some(&a), Some(&b)) => a..b + 1,
                _ => {
                    let at = self.insert_index(p, r0);
                    at..at
                }
            }
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
        self.cur_block = self.diff.rows.get(row).and_then(|r| r.block);
        self.status = trf("msg.pasted_lines", &[("n", n.to_string())]);
    }

    fn delete_line(&mut self, row: usize, pane: usize) {
        if !self.writable(pane) {
            return;
        }
        if let Some(l) = self.diff.rows.get(row).and_then(|r| r.line[pane]) {
            self.push_undo();
            self.panes[pane].doc.lines.remove(l);
            self.mark_changed(&[pane]);
        }
    }

    // ---------- files ----------

    pub fn save_pane(&mut self, p: usize) -> anyhow::Result<bool> {
        let doc = &mut self.panes[p].doc;
        if doc.read_only {
            anyhow::bail!("{}", tr("msg.readonly"));
        }
        if doc.path.is_none() {
            let Some(path) = rfd::FileDialog::new()
                .set_title(tr("dlg.save_as"))
                .save_file()
            else {
                return Ok(false);
            };
            doc.path = Some(path);
        }
        doc.save()?;
        self.status = trf("msg.saved", &[("file", doc.title())]);
        self.saved(p);
        Ok(true)
    }

    fn saved(&mut self, p: usize) {
        let pane = &mut self.panes[p];
        pane.saves += 1;
        pane.disk_time = disk_time(&pane.doc);
        self.disk_changed.retain(|&c| c != p);
    }

    /// Reacts to files changed by another program: unmodified panes are reloaded, and panes with
    /// unsaved edits are listed in a warning bar.
    pub fn check_disk(&mut self) {
        let mut reload = Vec::new();
        for (i, p) in self.panes.iter_mut().enumerate() {
            let now = disk_time(&p.doc);
            if now.is_none() || now == p.disk_time {
                continue;
            }
            p.disk_time = now;
            if p.doc.modified {
                if !self.disk_changed.contains(&i) {
                    self.disk_changed.push(i);
                }
            } else {
                reload.push(i);
            }
        }
        if !reload.is_empty() {
            self.refresh();
            let names: Vec<String> = reload.iter().map(|&i| self.panes[i].doc.title()).collect();
            self.status = trf("msg.reloaded", &[("file", names.join(", "))]);
        }
    }

    /// Replaces the listed panes with the file contents on disk, discarding their edits (undoable).
    fn reload_changed(&mut self) {
        let panes = std::mem::take(&mut self.disk_changed);
        self.push_undo();
        for p in panes {
            let pane = &mut self.panes[p];
            let Some(path) = pane.doc.path.clone() else {
                continue;
            };
            match text::load_doc(&path, Some(pane.doc.encoding)) {
                Ok(mut d) => {
                    d.read_only = pane.doc.read_only;
                    pane.doc = d;
                    pane.hl_dirty = true;
                    pane.saves += 1;
                    pane.disk_time = disk_time(&pane.doc);
                    pane.reset_baseline();
                }
                Err(e) => self.status = e.to_string(),
            }
        }
        self.rediff();
    }

    pub fn save_pane_as(&mut self, p: usize) -> anyhow::Result<bool> {
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
        self.panes[p].original = None;
        let doc = &mut self.panes[p].doc;
        doc.save_as(&path)?;
        doc.read_only = false;
        doc.description = None;
        self.status = trf("msg.saved", &[("file", doc.title())]);
        self.saved(p);
        Ok(true)
    }

    pub fn save_modified(&mut self) -> anyhow::Result<()> {
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
                d.description = old.description.clone();
                self.panes[p].doc = d;
                self.panes[p].hl_dirty = true;
                self.panes[p].disk_time = disk_time(&self.panes[p].doc);
                self.panes[p].reset_baseline();
                self.rediff();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Reloads unmodified files from disk and recomputes the differences.
    pub fn refresh(&mut self) {
        for p in &mut self.panes {
            if p.doc.modified || p.doc.description.is_some() {
                continue;
            }
            if let Some(path) = p.doc.path.clone() {
                if let Ok(mut d) = text::load_doc(&path, Some(p.doc.encoding)) {
                    d.read_only = p.doc.read_only;
                    if d.lines != p.doc.lines {
                        p.hl_dirty = true;
                    }
                    p.doc = d;
                    p.disk_time = disk_time(&p.doc);
                    p.reset_baseline();
                }
            }
        }
        self.rediff();
    }

    pub fn make_patch(&self, style: PatchStyle, context: usize) -> Option<String> {
        if self.n() != 2 {
            return None;
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
        };
        Some(patch::make_patch(&input, &self.diff, style, context))
    }

    /// Side-by-side HTML report of the compare as currently shown.
    pub fn html_report(&self) -> String {
        let titles: Vec<String> = self.panes.iter().map(|p| p.doc.title()).collect();
        let refs: Vec<&[String]> = self.panes.iter().map(|p| p.doc.lines.as_slice()).collect();
        crate::core::report::text_html(&titles, &refs, &self.diff, &self.opts, &self.bookmarks)
    }

    /// The compare with its bookmarks, to save as one file.
    pub fn saved_compare(&self) -> SavedCompare {
        SavedCompare::new(
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
        )
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

    /// Indents (or outdents) the active pane's lines in the selected rows, or on the cursor
    /// row, by a tab when the file indents with tabs and by the tab size in spaces otherwise.
    pub fn indent(&mut self, outdent: bool) {
        let p = self.active_pane;
        let Some((r0, r1)) = self.selection().or_else(|| self.cursor_row.map(|r| (r, r))) else {
            return;
        };
        let lines: Vec<usize> = self.diff.rows[r0..=r1]
            .iter()
            .filter_map(|r| r.line[p])
            .collect();
        let (Some(&first), Some(&last)) = (lines.first(), lines.last()) else {
            return;
        };
        if !self.writable(p) {
            return;
        }
        let tab = self.tab_size.max(1);
        let doc = &self.panes[p].doc;
        let (tabs, spaces) = doc.lines.iter().take(5000).fold((0, 0), |(t, s), l| {
            (
                t + l.starts_with('\t') as usize,
                s + l.starts_with(' ') as usize,
            )
        });
        let unit = if tabs > spaces {
            "\t".to_string()
        } else {
            " ".repeat(tab)
        };
        let changed: Vec<String> = lines
            .iter()
            .map(|&l| {
                let line = &doc.lines[l];
                if outdent {
                    let cut = if line.starts_with('\t') {
                        1
                    } else {
                        line.len() - line.trim_start_matches(' ').len()
                    };
                    line[cut.min(tab)..].to_string()
                } else if line.trim().is_empty() {
                    line.clone()
                } else {
                    format!("{unit}{line}")
                }
            })
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
        if self.find_re.as_ref().map(|(k, _)| k) != Some(&key) {
            let re = find_regex(&key.0, key.1, key.2, key.3).map_err(|e| e.to_string());
            self.find_re = Some((key, re));
        }
        self.find_re.as_ref().map(|(_, re)| re.clone())
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
            if let Some(l) = self.diff.rows[r].line[pane] {
                if re.is_match(&self.panes[pane].doc.lines[l]) {
                    self.cursor_row = Some(r);
                    self.scroll_to = Some(r);
                    self.cur_block = self.diff.rows[r].block;
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
            .and_then(|r| r.line[pane]);
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
        let (pos, total) = self.diff_position();
        let mut s = if total == 0 {
            tr("msg.identical").to_string()
        } else {
            match pos {
                Some(p) => trf(
                    "msg.diff_of",
                    &[("n", (p + 1).to_string()), ("total", total.to_string())],
                ),
                None => trf("msg.diff_count", &[("total", total.to_string())]),
            }
        };
        if let Some((del, add)) = self.change_counts().filter(|&(d, a)| d + a > 0) {
            s.push_str(&format!("  (+{add} −{del})"));
        }
        if self.n() == 3 {
            s.push_str(&format!(
                "  |  {}: {}",
                tr("label.conflicts"),
                self.diff.conflicts()
            ));
        }
        if let Some(r) = self.cursor_row {
            if let Some(l) = self
                .diff
                .rows
                .get(r)
                .and_then(|row| row.line[self.active_pane])
            {
                s.push_str(&format!("  |  {} {}", tr("label.line"), l + 1));
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
                if let Ok(res) = rx.try_recv() {
                    p.hl = res;
                    p.hl_rx = None;
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
                p.hl_rx = Some(syntax::spawn(p.doc.lines.clone(), ext, cfg.dark_mode));
            }
        }
    }

    fn ensure_inline(&mut self, r: usize) {
        if self.inline_cache.contains_key(&r) || self.opts.inline == InlineMode::Off {
            return;
        }
        let row = self.diff.rows[r];
        let mut out: [Vec<Range<usize>>; 3] = Default::default();
        if let Some(b) = row.block {
            if !self.diff.blocks[b].ignored {
                let line = |p: usize| row.line[p].map(|l| self.panes[p].doc.lines[l].as_str());
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
        let b = row.block?;
        let blk = &self.diff.blocks[b];
        let sel = self.cur_block == Some(b);
        if blk.ignored {
            return Some(c.ignored);
        }
        let Some(l) = row.line[p] else {
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
        Some(match (blk.kind, sel) {
            (BlockKind::Conflict, false) => c.conflict,
            (BlockKind::Conflict, true) => c.conflict_sel,
            (_, false) => c.diff,
            (_, true) => c.diff_sel,
        })
    }

    fn row_bar(&self, r: usize, p: usize, c: &Colors) -> Option<Color32> {
        let row = &self.diff.rows[r];
        let blk = &self.diff.blocks[row.block?];
        if blk.ignored || row.line[p].is_none() {
            return None;
        }
        Some(if blk.kind == BlockKind::Conflict {
            c.conflict_bar
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
        match &self.find_re {
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
            let key = widgets::with_key;
            if widgets::icon_button(ui, Icon::First, &key(tr("cmd.first_diff"), "Alt+Home"))
                .clicked()
            {
                self.first_diff();
            }
            if widgets::icon_button(ui, Icon::Prev, &key(tr("cmd.prev_diff"), "Alt+↑")).clicked()
                && !self.prev_diff(false)
            {
                self.step_file = Some(false);
            }
            let (pos, total) = self.diff_position();
            let counter = match pos {
                Some(p) => format!("{} / {total}", p + 1),
                None => format!("– / {total}"),
            };
            widgets::chip(ui, &counter, pal.text, pal.control).on_hover_text(self.status_line());
            if let Some((del, add)) = self.change_counts().filter(|&(d, a)| d + a > 0) {
                let (added, removed) = (format!("+{add}"), format!("−{del}"));
                ui.label(RichText::new(&added).monospace().color(pal.success));
                ui.label(RichText::new(&removed).monospace().color(pal.danger))
                    .on_hover_text(trf(
                        "text.change_counts",
                        &[("add", add.to_string()), ("del", del.to_string())],
                    ));
            }
            if widgets::icon_button(ui, Icon::Next, &key(tr("cmd.next_diff"), "Alt+↓")).clicked()
                && !self.next_diff(false)
            {
                self.step_file = Some(true);
            }
            if widgets::icon_button(ui, Icon::Last, &key(tr("cmd.last_diff"), "Alt+End")).clicked()
            {
                self.last_diff();
            }
            widgets::toolbar_sep(ui);
            if self.n() == 2 {
                if widgets::icon_button(ui, Icon::CopyRight, &key(tr("cmd.copy_right"), "Alt+→"))
                    .clicked()
                {
                    self.copy_current(true);
                }
                if widgets::icon_button(ui, Icon::CopyLeft, &key(tr("cmd.copy_left"), "Alt+←"))
                    .clicked()
                {
                    self.copy_current(false);
                }
                if widgets::icon_button(
                    ui,
                    Icon::CopyAllRight,
                    &key(tr("cmd.copy_all_right"), "Ctrl+Alt+→"),
                )
                .clicked()
                {
                    self.copy_all(0, 1);
                }
                if widgets::icon_button(
                    ui,
                    Icon::CopyAllLeft,
                    &key(tr("cmd.copy_all_left"), "Ctrl+Alt+←"),
                )
                .clicked()
                {
                    self.copy_all(1, 0);
                }
            } else {
                if widgets::icon_button(
                    ui,
                    Icon::PrevConflict,
                    &key(tr("cmd.prev_conflict"), "Alt+PgUp"),
                )
                .clicked()
                {
                    self.prev_diff(true);
                }
                if widgets::icon_button(
                    ui,
                    Icon::NextConflict,
                    &key(tr("cmd.next_conflict"), "Alt+PgDn"),
                )
                .clicked()
                {
                    self.next_diff(true);
                }
                widgets::toolbar_sep(ui);
                if widgets::icon_button(ui, Icon::CopyRight, tr("cmd.copy_from_left")).clicked() {
                    if let Some(b) = self.cur_block {
                        self.copy_block(b, 0, 1);
                    }
                }
                if widgets::icon_button(ui, Icon::CopyLeft, tr("cmd.copy_from_right")).clicked() {
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
            widgets::toolbar_sep(ui);
            if widgets::icon_button_enabled(
                ui,
                self.can_undo(),
                Icon::Undo,
                &key(tr("cmd.undo"), "Ctrl+Z"),
            )
            .clicked()
            {
                self.undo();
            }
            if widgets::icon_button_enabled(
                ui,
                self.can_redo(),
                Icon::Redo,
                &key(tr("cmd.redo"), "Ctrl+Y"),
            )
            .clicked()
            {
                self.redo();
            }
            widgets::toolbar_sep(ui);
            let mut find_open = self.find.open;
            if widgets::toggle_icon_button(
                ui,
                &mut find_open,
                Some(Icon::Search),
                "",
                &key(tr("cmd.find"), "Ctrl+F"),
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
                &key(tr("cmd.collapse_unchanged"), "Alt+C"),
            )
            .clicked()
            {
                self.set_collapse(collapse);
            }
            ui.add_enabled_ui(!self.table_mode, |ui| {
                widgets::toggle_icon_button(
                    ui,
                    &mut self.wrap,
                    Some(Icon::Wrap),
                    "",
                    &key(tr("cmd.word_wrap"), "Alt+Z"),
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
            if ui.input(|i| i.key_pressed(Key::Escape))
                && self.edit.is_none()
                && self.goto.is_none()
                && !widgets::modal_open(ui.ctx())
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
            if widgets::icon_button(ui, Icon::Prev, &key(tr("cmd.find_prev"), "Shift+F3")).clicked()
            {
                self.find_next(false);
            }
            if widgets::icon_button(ui, Icon::Next, &key(tr("cmd.find_next"), "F3")).clicked() {
                self.find_next(true);
            }
            widgets::toggle_button(
                ui,
                &mut self.find.show_replace,
                tr("cmd.replace"),
                &key(tr("cmd.replace"), REPLACE_KEY),
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
                    let mut enc_choice: Option<TextEncoding> = None;
                    let mut eol_choice: Option<Eol> = None;
                    let enc_name = doc.encoding.name();
                    let bom = if doc.bom { " BOM" } else { "" };
                    let eol = doc.eol;
                    let mixed = doc.mixed_eol;
                    let mut ro = doc.read_only;
                    widgets::side_chip(ui, p, n);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let lock = if ro { Icon::Lock } else { Icon::Unlock };
                        if widgets::toggle_icon_button(
                            ui,
                            &mut ro,
                            Some(lock),
                            "",
                            tr("label.read_only"),
                        )
                        .clicked()
                        {
                            self.panes[p].doc.read_only = ro;
                        }
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
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let mut job = LayoutJob::default();
                            let name_font = if active {
                                theme::semibold(13.5)
                            } else {
                                FontId::proportional(13.5)
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
                                        font_id: FontId::proportional(11.0),
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
                                        font_id: FontId::proportional(12.5),
                                        color: pal.text_dim,
                                        ..Default::default()
                                    },
                                );
                            }
                            ui.add(egui::Label::new(job).truncate().selectable(false))
                                .on_hover_text(title);
                        });
                    });
                    if let Some(e) = enc_choice {
                        self.reload_encoding(p, e);
                    }
                    if let Some(e) = eol_choice {
                        if e != self.panes[p].doc.eol || self.panes[p].doc.mixed_eol {
                            self.panes[p].doc.eol = e;
                            self.panes[p].doc.mixed_eol = false;
                            self.panes[p].doc.modified = true;
                        }
                    }
                });
            },
        );
    }

    /// Rebuilds the display lines when folding, wrapping or the diff changed.
    fn ensure_layout(&mut self, wrap_cols: usize, tab: usize) {
        let key = (wrap_cols, tab, self.collapse, self.layout_gen);
        let rows = self.diff.rows.len();
        if self.layout.key == Some(key) && self.layout.row_line.len() == rows {
            return;
        }
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
                .block
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
        self.layout = Layout {
            key: Some(key),
            lines,
            row_line,
        };
        if let Some(&v) = top.and_then(|t| self.layout.row_line.get(t)) {
            self.scroll = v as f32 + self.scroll.fract();
        }
    }

    fn row_segments(&self, r: usize, cols: usize, tab: usize) -> u32 {
        let row = &self.diff.rows[r];
        (0..self.n())
            .filter_map(|p| row.line[p].map(|l| &self.panes[p].doc.lines[l]))
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
        let lines = &self.layout.lines;
        let v = v.min(lines.len().checked_sub(1)?);
        let row = |l: &VLine| match *l {
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
            || ui.ctx().wants_keyboard_input()
            || self.edit.is_some()
            || self.goto.is_some()
            || widgets::modal_open(ui.ctx())
        {
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
        let total = self.layout.lines.len();
        if total == 0 {
            return;
        }
        let page = self.page_rows.max(1);
        let (cur, below) = match self.cursor_row.and_then(|r| self.row_span(r)) {
            Some(span) => span,
            None => {
                let v = (self.scroll as usize).min(total - 1);
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
            self.sel_anchor = if extend {
                self.sel_anchor.or(self.cursor_row)
            } else {
                None
            };
            self.cursor_row = Some(row);
            self.scroll_to = Some(row);
            self.cur_block = self.diff.rows[row]
                .block
                .filter(|&b| !self.diff.blocks[b].ignored);
        }
        if ui.input(|i| {
            i.modifiers.is_none() && (i.key_pressed(Key::F2) || i.key_pressed(Key::Enter))
        }) {
            if let Some(r) = self.cursor_row {
                self.begin_edit(r, self.active_pane);
            }
        }
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
        let sel = row.block.is_some() && row.block == self.cur_block;
        let inline_bg = if sel { c.inline_sel } else { c.inline };
        for p in 0..n {
            let slot = Rect::from_min_size(
                pos2(rect.left(), rect.top() + 3.0 + p as f32 * slot_h),
                vec2(rect.width(), slot_h),
            )
            .shrink2(vec2(6.0, 2.0));
            if let Some(bg) = self.row_bg(r, p, c) {
                painter.rect_filled(slot, 3.0, bg);
            }
            let label = match row.line[p] {
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
            let Some(l) = row.line[p] else { continue };
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
            let galley = ui.fonts(|f| f.layout_job(job));
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

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.poll_highlight(cfg);
        if self.panes.iter().any(|p| p.hl_rx.is_some()) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.toolbar(ui);
        if self.outcome() == Outcome::Identical {
            let color = widgets::pal(ui).success;
            widgets::info_bar(ui, Icon::Check, tr("msg.identical"), color);
        }
        if !self.disk_changed.is_empty() {
            self.disk_changed_bar(ui);
        }
        if self.find.open {
            self.find_bar(ui);
        }

        let n = self.n();
        self.tab_size = cfg.tab_size;
        let full = ui.available_rect_before_wrap();
        let detail_h = if cfg.show_line_detail {
            let row = ui.fonts(|f| f.row_height(&FontId::monospace(cfg.font_size)));
            (n as f32 * (2.0 * row + 6.0) + 6.0).min(full.height() / 2.0)
        } else {
            0.0
        };
        let mut avail = full;
        avail.max.y -= detail_h;
        let header_h = 36.0;
        let wrap = self.wrap && !self.table_mode;
        let hbar_h = if wrap { 0.0 } else { 12.0 };
        let loc_w = if cfg.show_location_pane {
            14.0 + 8.0 * n as f32
        } else {
            0.0
        };
        let vbar_w = 12.0;
        let content_left = avail.left() + loc_w;
        let content_right = avail.right() - vbar_w;
        let pane_w = ((content_right - content_left) / n as f32).max(50.0);
        let body_top = avail.top() + header_h;
        let body_bottom = (avail.bottom() - hbar_h).max(body_top + 20.0);
        let font = FontId::monospace(cfg.font_size);
        let row_h = ui.fonts(|f| f.row_height(&font)) + 2.0;
        let char_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
        let body_h = body_bottom - body_top;
        let visible = body_h / row_h;
        self.page_rows = (visible as usize).saturating_sub(1).max(1);

        // headers
        for p in 0..n {
            let r = Rect::from_min_size(
                pos2(content_left + p as f32 * pane_w, avail.top()),
                vec2(pane_w, header_h),
            );
            self.pane_header(ui, p, r);
        }

        let body_rect = Rect::from_min_max(
            pos2(content_left, body_top),
            pos2(content_right, body_bottom),
        );
        let resp = ui.interact(body_rect, self.id.with("body"), Sense::click_and_drag());

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
                let v = v as f32;
                if v < self.scroll || v > self.scroll + visible - 2.0 {
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

        let first = (self.scroll.floor() as usize).min(total);
        let last = (first + visible.ceil() as usize + 1).min(total);
        let shown: Vec<VLine> = self.layout.lines[first..last].to_vec();
        for l in &shown {
            if let VLine::Row(r, _) = *l {
                self.ensure_inline(r);
            }
        }
        let line_at = |scroll: f32, y: f32| {
            let v = (scroll + (y - body_top) / row_h).floor();
            (v >= 0.0 && (v as usize) < total).then_some(v as usize)
        };
        let hover_v = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| body_rect.contains(*p))
            .and_then(|p| line_at(self.scroll, p.y));
        let hover_block = hover_v
            .and_then(|v| match self.layout.lines[v] {
                VLine::Row(r, _) => self.diff.rows[r].block,
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
            let p = (((pos.x - content_left) / pane_w).floor() as usize).min(n - 1);
            let in_gutter = pos.x - (content_left + p as f32 * pane_w) < gutter_w;
            let note = match hover_v.map(|v| self.layout.lines[v]) {
                Some(VLine::Row(r, _)) if in_gutter => self.diff.rows[r].line[p]
                    .and_then(|l| self.bookmarks[p].get(&l))
                    .filter(|n| !n.is_empty())
                    .cloned(),
                _ => None,
            };
            if let Some(note) = note {
                resp.clone().on_hover_text_at_pointer(note);
            }
        }

        // clicks
        if resp.clicked() || resp.double_clicked() || resp.secondary_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let p = (((pos.x - content_left) / pane_w).floor() as usize).min(n - 1);
                match line_at(self.scroll, pos.y).map(|v| self.layout.lines[v]) {
                    Some(VLine::Row(r, _)) => {
                        let in_sel = self.selection().is_some_and(|(a, b)| (a..=b).contains(&r));
                        // Right-clicking the selection keeps it for the context menu.
                        if !(resp.secondary_clicked() && in_sel && p == self.active_pane) {
                            self.sel_anchor = if ui.input(|i| i.modifiers.shift) {
                                self.sel_anchor.or(self.cursor_row)
                            } else {
                                None
                            };
                            self.active_pane = p;
                            self.cursor_row = Some(r);
                            if let Some(b) = self.diff.rows[r].block {
                                if !self.diff.blocks[b].ignored {
                                    self.cur_block = Some(b);
                                }
                            }
                            if resp.double_clicked() {
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

        // dragging selects rows of the pane where the drag started
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if resp.drag_started() {
                    let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                    self.active_pane =
                        (((origin.x - content_left) / pane_w).floor() as usize).min(n - 1);
                    if !ui.input(|i| i.modifiers.shift) {
                        self.sel_anchor =
                            match line_at(self.scroll, origin.y).map(|v| self.layout.lines[v]) {
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
                if let Some(VLine::Row(r, _)) =
                    line_at(self.scroll, y).map(|v| self.layout.lines[v])
                {
                    if self.sel_anchor.is_none() {
                        self.sel_anchor = Some(r);
                    }
                    self.cursor_row = Some(r);
                    self.cur_block = self.diff.rows[r]
                        .block
                        .filter(|&b| !self.diff.blocks[b].ignored);
                }
            }
        }
        let selection = self.selection();

        // painting
        let painter = ui.painter();
        let line_y = |v: usize| body_top + (v as f32 - self.scroll) * row_h;
        for p in 0..n {
            let x0 = content_left + p as f32 * pane_w;
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
            let mut laid: Option<(usize, Arc<Galley>, Vec<f32>)> = None;
            for (k, l) in shown.iter().enumerate() {
                let VLine::Row(r, seg) = *l else { continue };
                let seg = seg as usize;
                let y = line_y(first + k);
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
                if p == self.active_pane && selection.is_some_and(|(a, b)| (a..=b).contains(&r)) {
                    tp.rect_filled(
                        row_rect,
                        0.0,
                        ui.visuals().selection.bg_fill.gamma_multiply(0.7),
                    );
                }
                let row = self.diff.rows[r];
                let Some(l) = row.line[p] else { continue };
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
                    .block
                    .map(|b| self.diff.blocks[b].ignored)
                    .unwrap_or(false);
                let fg = if ignored { c.gutter_text } else { c.text };
                let sel = row.block.is_some() && row.block == self.cur_block;
                let inline_bg = if sel { c.inline_sel } else { c.inline };
                if let (true, Some(delim)) = (self.table_mode, self.table_delim) {
                    let cells = table::split_row(line, delim);
                    let others: Vec<Vec<String>> = (0..n)
                        .filter(|&q| q != p)
                        .filter_map(|q| {
                            row.line[q]
                                .map(|ol| table::split_row(&self.panes[q].doc.lines[ol], delim))
                        })
                        .collect();
                    let mut x = text_rect.left() + 4.0 - self.hscroll;
                    for (ci, cell) in cells.iter().enumerate() {
                        let w = (self.col_widths.get(ci).copied().unwrap_or(3) as f32 + 1.0)
                            * char_w
                            + 8.0;
                        let cell_rect = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
                        let differs = row.block.is_some()
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
                    let galley = ui.fonts(|f| f.layout_job(job));
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
            }
            if let Some((v0, v1)) = self.cursor_row.and_then(|cr| self.row_span(cr)) {
                if v1 > first && v0 < last {
                    let rr = Rect::from_min_max(
                        pos2(text_rect.left() + 1.0, line_y(v0)),
                        pos2(text_rect.right() - 1.0, line_y(v1)),
                    );
                    let color = if p == self.active_pane {
                        c.cursor_row
                    } else {
                        c.cursor_row.gamma_multiply(0.35)
                    };
                    tp.rect_stroke(rr, 3.0, Stroke::new(1.5_f32, color), StrokeKind::Inside);
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
        let bp = ui.painter_at(body_rect);
        for (k, l) in shown.iter().enumerate() {
            let VLine::Fold(s, e) = *l else { continue };
            let y = line_y(first + k);
            let rect = Rect::from_min_max(pos2(content_left, y), pos2(content_right, y + row_h));
            let hovered = hover_fold == Some(first + k);
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
            let galley = ui.fonts(|f| f.layout_no_wrap(label, small, fg));
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

        // merge arrows on the dividers
        let mut merge: Option<(usize, usize, usize)> = None;
        let arrow_block = hover_block.or(self.cur_block).filter(|&b| {
            self.edit.is_none() && b < self.diff.blocks.len() && !self.diff.blocks[b].ignored
        });
        if let Some(b) = arrow_block {
            let (v0, v1) = self.block_span(b);
            if v1 > first && v0 < last {
                let blk = &self.diff.blocks[b];
                let y = line_y(v0.max(first)).clamp(body_top, body_bottom - row_h);
                let cy = y + row_h / 2.0;
                let pairs: &[(usize, [MergeArrow; 2])] = if n == 2 {
                    &[(
                        0,
                        [
                            (0, 1, Icon::CopyRight, "cmd.copy_right"),
                            (1, 0, Icon::CopyLeft, "cmd.copy_left"),
                        ],
                    )]
                } else {
                    &[
                        (
                            0,
                            [
                                (0, 1, Icon::CopyRight, "cmd.copy_from_left"),
                                (1, 0, Icon::CopyLeft, "cmd.copy_middle_left"),
                            ],
                        ),
                        (
                            1,
                            [
                                (1, 2, Icon::CopyRight, "cmd.copy_middle_right"),
                                (2, 1, Icon::CopyLeft, "cmd.copy_from_right"),
                            ],
                        ),
                    ]
                };
                let d = (row_h + 2.0).clamp(18.0, 24.0);
                for (div, arrows) in pairs {
                    let x = content_left + (*div + 1) as f32 * pane_w - 1.0;
                    for (i, &(from, to, icon, key)) in arrows.iter().enumerate() {
                        let lines = |q: usize| &self.panes[q].doc.lines[blk.ranges[q].clone()];
                        if self.panes[to].doc.read_only || lines(from) == lines(to) {
                            continue;
                        }
                        let cx = if i == 0 {
                            x - d / 2.0 - 1.0
                        } else {
                            x + d / 2.0 + 1.0
                        };
                        let rect = Rect::from_center_size(pos2(cx, cy), vec2(d, d));
                        let r = ui
                            .interact(rect, self.id.with(("merge", from, to)), Sense::click())
                            .on_hover_text(tr(key))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr(key))
                        });
                        let hot = r.hovered();
                        let painter = ui.painter();
                        painter.circle(
                            rect.center(),
                            d / 2.0,
                            if hot { c.ui.accent } else { c.ui.surface },
                            Stroke::new(
                                1.0_f32,
                                if hot { c.ui.accent } else { c.ui.border_strong },
                            ),
                        );
                        icons::paint(
                            painter,
                            rect.shrink(d * 0.22),
                            icon,
                            if hot { c.ui.on_accent } else { c.ui.text },
                        );
                        if r.clicked() {
                            merge = Some((b, from, to));
                        }
                    }
                }
            }
        }
        if let Some((b, from, to)) = merge {
            self.copy_block(b, from, to);
        }

        // inline editor
        let edit_line = self.edit.as_ref().and_then(|e| self.row_span(e.row));
        if let Some(e) = &mut self.edit {
            let mut done: Option<bool> = None;
            match edit_line.filter(|&(v0, _)| v0 >= first && v0 < last) {
                Some((v0, _)) => {
                    let x0 = content_left + e.pane as f32 * pane_w + gutter_w;
                    let y = body_top + (v0 as f32 - self.scroll) * row_h;
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
            let block = self.diff.rows.get(r).and_then(|row| row.block);
            let lines = selection.filter(|&(a, b)| (a..=b).contains(&r));
            resp.context_menu(|ui| {
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
                    ui.close_menu();
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
                    let text = match selection {
                        Some((a, b)) if (a..=b).contains(&r) && p == self.active_pane => {
                            self.selected_text(p)
                        }
                        _ => self
                            .diff
                            .rows
                            .get(r)
                            .and_then(|row| row.line[p])
                            .map(|l| self.panes[p].doc.lines[l].clone()),
                    };
                    if let Some(t) = text {
                        ui.ctx().copy_text(t);
                    }
                }
                MenuAction::CopyLines(from, to) => {
                    if let Some((a, b)) = selection {
                        self.copy_rows(a, b, from, to);
                    }
                }
                MenuAction::ToggleBookmark(r, p) => self.toggle_bookmark_at(r, p),
                MenuAction::EditNote(r, p) => self.edit_note_at(r, p),
                MenuAction::TakeBoth(b, left_first) => self.take_both_at(b, left_first),
            }
        }

        // location pane
        let scroll_before = self.scroll;
        if cfg.show_location_pane && self.layout.row_line.len() == self.diff.rows.len() {
            let loc_rect = Rect::from_min_max(
                pos2(avail.left(), body_top),
                pos2(avail.left() + loc_w - 2.0, body_bottom),
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
                        } else if b.kind == BlockKind::Conflict {
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
            pos2(content_right, body_top),
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
                pos2(content_right, avail.bottom()),
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        v.cursor_row = Some(0);
        v.paste("zero");
        assert_eq!(
            v.bookmarks[0].get(&3).map(String::as_str),
            Some("check this")
        );
        assert_eq!(v.panes[0].edited, HashSet::from([1]));
        assert_eq!(v.row_note(v.diff.row_of_line(0, 3)), Some("check this"));

        let saved = v.saved_compare();
        assert_eq!(
            saved.panes[0].bookmarks.get(&4).map(String::as_str),
            Some("check this")
        );
        let r = TextView::from_saved(&saved, &Config::default());
        assert_eq!(r.bookmarks[0], v.bookmarks[0]);
        assert_eq!(lines(&r, 0), lines(&v, 0));
        assert!(r.panes.iter().all(|p| p.doc.read_only));
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
}
