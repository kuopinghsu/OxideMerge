//! File compare / merge view for text files (2-way and 3-way).

use crate::config::Config;
use crate::core::diff::{self, BlockKind, DiffOptions, DiffResult, InlineMode};
use crate::core::patch::{self, PatchStyle};
use crate::core::table;
use crate::core::text::{self, Eol, TextDoc, TextEncoding, COMMON_ENCODINGS};
use crate::i18n::{tr, trf};
use crate::ui::syntax::{self, LineSpans};
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, Mark};
use egui::text::LayoutJob;
use egui::{pos2, vec2, Align2, Color32, FontId, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, TextFormat, Ui};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::mpsc::Receiver;

pub struct Pane {
    pub doc: TextDoc,
    hl: Option<Vec<LineSpans>>,
    hl_rx: Option<Receiver<Option<Vec<LineSpans>>>>,
    hl_dirty: bool,
    hl_dark: bool,
}

impl Pane {
    fn new(doc: TextDoc) -> Self {
        Pane { doc, hl: None, hl_rx: None, hl_dirty: true, hl_dark: false }
    }
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
    pub focus: bool,
}

struct Snapshot {
    lines: Vec<Vec<String>>,
    modified: Vec<bool>,
}

#[derive(Clone, Copy)]
enum MenuAction {
    Copy(usize, usize, usize),
    Edit(usize, usize),
    InsertBelow(usize, usize),
    DeleteLine(usize, usize),
    CopyText(usize, usize),
}

pub struct TextView {
    pub panes: Vec<Pane>,
    pub diff: DiffResult,
    opts: DiffOptions,
    pub cur_block: Option<usize>,
    scroll: f32,
    hscroll: f32,
    pub active_pane: usize,
    pub cursor_row: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    inline_cache: HashMap<usize, [Vec<Range<usize>>; 3]>,
    edit: Option<EditState>,
    pub find: FindState,
    pub goto: Option<String>,
    pub table_mode: bool,
    pub table_delim: Option<char>,
    col_widths: Vec<usize>,
    pub status: String,
    scroll_to: Option<usize>,
    max_cols: usize,
    ctx_target: Option<(usize, usize)>,
    page_rows: usize,
    id: egui::Id,
}

impl TextView {
    pub fn new(docs: Vec<TextDoc>, cfg: &Config) -> Self {
        let table_delim = docs.iter().filter_map(|d| d.path.as_deref()).find_map(table::delimiter_for);
        let mut v = TextView {
            panes: docs.into_iter().map(Pane::new).collect(),
            diff: DiffResult::default(),
            opts: cfg.diff.clone(),
            cur_block: None,
            scroll: 0.0,
            hscroll: 0.0,
            active_pane: 0,
            cursor_row: None,
            undo: Vec::new(),
            redo: Vec::new(),
            inline_cache: HashMap::new(),
            edit: None,
            find: FindState::default(),
            goto: None,
            table_mode: table_delim.is_some(),
            table_delim,
            col_widths: Vec::new(),
            status: String::new(),
            scroll_to: None,
            max_cols: 0,
            ctx_target: None,
            page_rows: 30,
            id: egui::Id::new(("textview", std::time::Instant::now())),
        };
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
                    (None, Some(path)) => path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
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
            self.rediff();
        }
    }

    pub fn rediff(&mut self) {
        let refs: Vec<&[String]> = self.panes.iter().map(|p| p.doc.lines.as_slice()).collect();
        self.diff = diff::compute(&refs, &self.opts);
        self.inline_cache.clear();
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
        }
        self.rediff();
    }

    // ---------- navigation ----------

    fn significant(&self) -> impl Iterator<Item = usize> + '_ {
        self.diff.blocks.iter().enumerate().filter(|(_, b)| !b.ignored).map(|(i, _)| i)
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

    pub fn next_diff(&mut self, conflicts_only: bool) {
        let row = self.cursor_row;
        let found = self
            .significant()
            .filter(|&b| !conflicts_only || self.diff.blocks[b].kind == BlockKind::Conflict)
            .find(|&b| row.is_none_or(|r| self.diff.blocks[b].first_row > r));
        if let Some(b) = found {
            self.select_block(b);
        }
    }

    pub fn prev_diff(&mut self, conflicts_only: bool) {
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
    }

    pub fn diff_position(&self) -> (Option<usize>, usize) {
        let sig: Vec<usize> = self.significant().collect();
        let pos = self.cur_block.and_then(|c| sig.iter().position(|&b| b == c));
        (pos, sig.len())
    }

    // ---------- merging ----------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.panes.iter().map(|p| p.doc.lines.clone()).collect(),
            modified: self.panes.iter().map(|p| p.doc.modified).collect(),
        }
    }

    fn push_undo(&mut self) {
        let s = self.snapshot();
        self.undo.push(s);
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn restore(&mut self, s: Snapshot) {
        for (i, p) in self.panes.iter_mut().enumerate() {
            if p.doc.lines != s.lines[i] {
                p.hl_dirty = true;
            }
            p.doc.lines = s.lines[i].clone();
            p.doc.modified = s.modified[i];
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
        if from == to || from >= self.n() || to >= self.n() || b >= self.diff.blocks.len() || !self.writable(to) {
            return;
        }
        self.push_undo();
        let row = self.diff.blocks[b].first_row;
        self.replace_block_lines(b, from, to);
        self.mark_changed(&[to]);
        // stay near the merged position
        self.cur_block = None;
        self.cursor_row = Some(row);
        let next = self.significant().find(|&nb| self.diff.blocks[nb].first_row >= row);
        if let Some(nb) = next {
            self.select_block(nb);
        }
    }

    /// Alt+Right / Alt+Left semantics.
    pub fn copy_current(&mut self, to_right: bool) {
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
    }

    /// Three-way: merges all non-conflicting changes into the middle pane.
    pub fn auto_merge(&mut self) {
        if self.n() != 3 || !self.writable(1) {
            return;
        }
        self.push_undo();
        let blocks: Vec<(usize, BlockKind)> = self.significant().map(|b| (b, self.diff.blocks[b].kind)).collect();
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
            &[("n", merged.to_string()), ("c", self.diff.conflicts().to_string())],
        );
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
        self.edit = Some(EditState { row, pane, text, focus: true });
    }

    fn commit_edit(&mut self, e: EditState) {
        let new_lines: Vec<String> = e.text.split('\n').map(|s| s.trim_end_matches('\r').to_string()).collect();
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
            let Some(path) = rfd::FileDialog::new().set_title(tr("dlg.save_as")).save_file() else {
                return Ok(false);
            };
            doc.path = Some(path);
        }
        doc.save()?;
        self.status = trf("msg.saved", &[("file", doc.title())]);
        Ok(true)
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
        let Some(path) = dlg.save_file() else { return Ok(false) };
        let doc = &mut self.panes[p].doc;
        doc.save_as(&path)?;
        doc.read_only = false;
        doc.description = None;
        self.status = trf("msg.saved", &[("file", doc.title())]);
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
        let Some(path) = self.panes[p].doc.path.clone() else { return };
        match text::load_doc(&path, Some(enc)) {
            Ok(mut d) => {
                let old = &self.panes[p].doc;
                d.read_only = old.read_only;
                d.description = old.description.clone();
                self.panes[p].doc = d;
                self.panes[p].hl_dirty = true;
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

    // ---------- find ----------

    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.find.query.is_empty() {
            return;
        }
        let q = if self.find.match_case { self.find.query.clone() } else { self.find.query.to_lowercase() };
        let pane = self.active_pane;
        let total = self.diff.rows.len();
        if total == 0 {
            return;
        }
        let start = self.cursor_row.unwrap_or(0);
        for k in 1..=total {
            let r = if forward { (start + k) % total } else { (start + total * 2 - k) % total };
            if let Some(l) = self.diff.rows[r].line[pane] {
                let line = &self.panes[pane].doc.lines[l];
                let hit = if self.find.match_case { line.contains(&q) } else { line.to_lowercase().contains(&q) };
                if hit {
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

    pub fn goto_line(&mut self, line: usize) {
        let r = self.diff.row_of_line(self.active_pane, line.saturating_sub(1));
        self.cursor_row = Some(r);
        self.scroll_to = Some(r);
    }

    pub fn status_line(&self) -> String {
        let (pos, total) = self.diff_position();
        let mut s = if total == 0 {
            tr("msg.identical").to_string()
        } else {
            match pos {
                Some(p) => trf("msg.diff_of", &[("n", (p + 1).to_string()), ("total", total.to_string())]),
                None => trf("msg.diff_count", &[("total", total.to_string())]),
            }
        };
        if self.n() == 3 {
            s.push_str(&format!("  |  {}: {}", tr("label.conflicts"), self.diff.conflicts()));
        }
        if let Some(r) = self.cursor_row {
            if let Some(l) = self.diff.rows.get(r).and_then(|row| row.line[self.active_pane]) {
                s.push_str(&format!("  |  {} {}", tr("label.line"), l + 1));
            }
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
                let ext = p.doc.path.as_deref().map(text::extension).unwrap_or_default();
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
            let unchanged = matches!((blk.kind, p), (BlockKind::LeftOnly, 2) | (BlockKind::RightOnly, 0));
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
        Some(if blk.kind == BlockKind::Conflict { c.conflict_bar } else { c.change_bar })
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
        let text = if text.len() > 8000 {
            let mut end = 8000;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            &text[..end]
        } else {
            text
        };
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
                .and_then(|sp| sp.iter().find(|(r, _)| r.start <= a && a < r.end).map(|(_, c)| *c))
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
            job.append(&seg, 0.0, TextFormat { font_id: font.clone(), color: fg, background: bg, ..Default::default() });
        }
        job
    }

    fn find_hits(&self, line: &str) -> Vec<Range<usize>> {
        if !self.find.open || self.find.query.is_empty() {
            return Vec::new();
        }
        let (hay, q) = if self.find.match_case {
            (line.to_string(), self.find.query.clone())
        } else {
            (line.to_lowercase(), self.find.query.to_lowercase())
        };
        if hay.len() != line.len() {
            return Vec::new();
        }
        hay.match_indices(&q).map(|(i, m)| i..i + m.len()).collect()
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            if widgets::icon_button(ui, "⇈", tr("cmd.first_diff")).clicked() {
                self.first_diff();
            }
            if widgets::icon_button(ui, "↑", tr("cmd.prev_diff")).clicked() {
                self.prev_diff(false);
            }
            let (pos, total) = self.diff_position();
            let counter = match pos {
                Some(p) => format!("{} / {total}", p + 1),
                None => format!("– / {total}"),
            };
            widgets::chip(ui, &counter, pal.text, pal.control).on_hover_text(self.status_line());
            if widgets::icon_button(ui, "↓", tr("cmd.next_diff")).clicked() {
                self.next_diff(false);
            }
            if widgets::icon_button(ui, "⇊", tr("cmd.last_diff")).clicked() {
                self.last_diff();
            }
            widgets::toolbar_sep(ui);
            if self.n() == 2 {
                if widgets::icon_button(ui, "→", tr("cmd.copy_right")).clicked() {
                    self.copy_current(true);
                }
                if widgets::icon_button(ui, "←", tr("cmd.copy_left")).clicked() {
                    self.copy_current(false);
                }
                if widgets::icon_button(ui, "⇉", tr("cmd.copy_all_right")).clicked() {
                    self.copy_all(0, 1);
                }
                if widgets::icon_button(ui, "⇇", tr("cmd.copy_all_left")).clicked() {
                    self.copy_all(1, 0);
                }
            } else {
                if widgets::icon_button(ui, "⚠↑", tr("cmd.prev_conflict")).clicked() {
                    self.prev_diff(true);
                }
                if widgets::icon_button(ui, "⚠↓", tr("cmd.next_conflict")).clicked() {
                    self.next_diff(true);
                }
                widgets::toolbar_sep(ui);
                if widgets::icon_button(ui, "L→M", tr("cmd.copy_from_left")).clicked() {
                    if let Some(b) = self.cur_block {
                        self.copy_block(b, 0, 1);
                    }
                }
                if widgets::icon_button(ui, "M←R", tr("cmd.copy_from_right")).clicked() {
                    if let Some(b) = self.cur_block {
                        self.copy_block(b, 2, 1);
                    }
                }
                if widgets::icon_button(ui, "⇉M", tr("cmd.copy_all_from_left")).clicked() {
                    self.copy_all(0, 1);
                }
                if widgets::icon_button(ui, "M⇇", tr("cmd.copy_all_from_right")).clicked() {
                    self.copy_all(2, 1);
                }
                if widgets::ghost_button(ui, &format!("⚡ {}", tr("cmd.auto_merge"))).clicked() {
                    self.auto_merge();
                }
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button_enabled(ui, self.can_undo(), "↶", tr("cmd.undo")).clicked() {
                self.undo();
            }
            if widgets::icon_button_enabled(ui, self.can_redo(), "↷", tr("cmd.redo")).clicked() {
                self.redo();
            }
            widgets::toolbar_sep(ui);
            let mut find_open = self.find.open;
            if widgets::toggle_button(ui, &mut find_open, "🔍", tr("cmd.find")).clicked() {
                if find_open {
                    self.open_find();
                } else {
                    self.find.open = false;
                }
            }
            if self.table_delim.is_some() {
                widgets::toggle_button(ui, &mut self.table_mode, &format!("▦ {}", tr("cmd.table_mode")), tr("cmd.table_mode"));
            }
        });
    }

    fn find_bar(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let hint = tr("label.find").trim_end_matches([':', '：']);
            let r = ui.add(egui::TextEdit::singleline(&mut self.find.query).hint_text(format!("🔍 {hint}")).margin(vec2(8.0, 4.0)).desired_width(280.0));
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let back = ui.input(|i| i.modifiers.shift);
                self.find_next(!back);
                r.request_focus();
            }
            ui.add_space(4.0);
            widgets::toggle_button(ui, &mut self.find.match_case, "Aa", tr("label.match_case"));
            if widgets::icon_button(ui, "↑", tr("cmd.find_prev")).clicked() {
                self.find_next(false);
            }
            if widgets::icon_button(ui, "↓", tr("cmd.find_next")).clicked() {
                self.find_next(true);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, "✖", tr("btn.cancel")).clicked() {
                    self.find.open = false;
                }
            });
        });
    }

    fn pane_header(&mut self, ui: &mut Ui, p: usize, rect: Rect) {
        let n = self.n();
        let active = self.active_pane == p;
        let pal = widgets::pal(ui);
        ui.painter().rect_filled(rect, 0.0, pal.surface_alt);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, pal.border));
        if active {
            ui.painter().hline(rect.left() + 1.0..=rect.right() - 2.0, rect.bottom() - 1.0, Stroke::new(2.0_f32, widgets::side_color(ui, p, n)));
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 3.0))), |ui| {
            ui.horizontal_centered(|ui| {
                let doc = &self.panes[p].doc;
                let title = doc.title();
                let (name, dir) = match (&doc.description, &doc.path) {
                    (Some(d), _) => (d.clone(), String::new()),
                    (None, Some(path)) => (
                        path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                        path.parent().map(|d| d.display().to_string()).unwrap_or_default(),
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
                    let lock = if ro { "🔒" } else { "🔓" };
                    if widgets::toggle_button(ui, &mut ro, lock, tr("label.read_only")).clicked() {
                        self.panes[p].doc.read_only = ro;
                    }
                    let eol_label = if mixed { format!("{}*", eol.label()) } else { eol.label().to_string() };
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
                                if ui.selectable_label(enc_name.eq_ignore_ascii_case(e), *e).clicked() {
                                    enc_choice = TextEncoding::from_label(e);
                                }
                            }
                        })
                        .response
                        .on_hover_text(tr("tip.encoding"));
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let mut job = LayoutJob::default();
                        let name_font = if active { theme::semibold(13.5) } else { FontId::proportional(13.5) };
                        job.append(&name, 0.0, TextFormat { font_id: name_font, color: pal.text, ..Default::default() });
                        if modified {
                            job.append("  ●", 0.0, TextFormat { font_id: FontId::proportional(11.0), color: pal.warning, ..Default::default() });
                        }
                        if !dir.is_empty() {
                            job.append(&format!("   {dir}"), 0.0, TextFormat { font_id: FontId::proportional(12.5), color: pal.text_dim, ..Default::default() });
                        }
                        ui.add(egui::Label::new(job).truncate().selectable(false)).on_hover_text(title);
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
        });
    }

    fn handle_keys(&mut self, ui: &Ui) {
        if ui.ctx().wants_keyboard_input() || self.edit.is_some() {
            return;
        }
        let total = self.diff.rows.len();
        if total == 0 {
            return;
        }
        let page = self.page_rows.max(1);
        let mut row = self.cursor_row.unwrap_or(self.scroll as usize);
        let mut moved = false;
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                row = (row + 1).min(total - 1);
                moved = true;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                row = row.saturating_sub(1);
                moved = true;
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                row = (row + page).min(total - 1);
                moved = true;
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                row = row.saturating_sub(page);
                moved = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Home) {
                row = 0;
                moved = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::End) {
                row = total - 1;
                moved = true;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) && self.active_pane > 0 {
                self.active_pane -= 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) && self.active_pane + 1 < self.panes.len() {
                self.active_pane += 1;
            }
        });
        if moved {
            self.cursor_row = Some(row);
            self.scroll_to = Some(row);
            self.cur_block = self.diff.rows[row].block.filter(|&b| !self.diff.blocks[b].ignored);
        }
        if ui.input(|i| i.key_pressed(Key::F2) || i.key_pressed(Key::Enter)) {
            if let Some(r) = self.cursor_row {
                self.begin_edit(r, self.active_pane);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        self.poll_highlight(cfg);
        if self.panes.iter().any(|p| p.hl_rx.is_some()) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.toolbar(ui);
        if self.find.open {
            self.find_bar(ui);
        }
        self.handle_keys(ui);

        let n = self.n();
        let avail = ui.available_rect_before_wrap();
        let header_h = 36.0;
        let hbar_h = 12.0;
        let loc_w = if cfg.show_location_pane { 14.0 + 8.0 * n as f32 } else { 0.0 };
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
        let total = self.diff.rows.len();

        // headers
        for p in 0..n {
            let r = Rect::from_min_size(pos2(content_left + p as f32 * pane_w, avail.top()), vec2(pane_w, header_h));
            self.pane_header(ui, p, r);
        }

        let body_rect = Rect::from_min_max(pos2(content_left, body_top), pos2(content_right, body_bottom));
        let resp = ui.interact(body_rect, self.id.with("body"), Sense::click());

        // scrolling
        if ui.rect_contains_pointer(body_rect) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.scroll -= d.y / row_h;
            self.hscroll -= d.x;
        }
        if let Some(row) = self.scroll_to.take() {
            let r = row as f32;
            if r < self.scroll || r > self.scroll + visible - 2.0 {
                self.scroll = r - (visible / 3.0).floor();
            }
        }
        let max_scroll = (total as f32 - visible + 1.0).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let digits = self.panes.iter().map(|p| p.doc.lines.len()).max().unwrap_or(1).to_string().len().max(3);
        let gutter_w = if cfg.show_line_numbers { (digits as f32 + 1.0) * char_w + 6.0 } else { 6.0 };
        let text_w = pane_w - gutter_w - 2.0;
        let content_w = if self.table_mode {
            self.col_widths.iter().map(|w| (*w as f32 + 1.0) * char_w + 8.0).sum::<f32>() + 20.0
        } else {
            self.max_cols as f32 * char_w + 40.0
        };
        self.hscroll = self.hscroll.clamp(0.0, (content_w - text_w).max(0.0));

        let first = self.scroll.floor() as usize;
        let last = (first + visible.ceil() as usize + 1).min(total);
        for r in first..last {
            self.ensure_inline(r);
        }

        // clicks
        if resp.clicked() || resp.double_clicked() || resp.secondary_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let r = (self.scroll + (pos.y - body_top) / row_h).floor() as usize;
                let p = (((pos.x - content_left) / pane_w).floor() as usize).min(n - 1);
                if r < total {
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
                    if resp.secondary_clicked() {
                        self.ctx_target = Some((r, p));
                    }
                }
            }
        }

        // painting
        let painter = ui.painter();
        let tab = cfg.tab_size;
        for p in 0..n {
            let x0 = content_left + p as f32 * pane_w;
            let gutter_rect = Rect::from_min_max(pos2(x0, body_top), pos2(x0 + gutter_w, body_bottom));
            let text_rect = Rect::from_min_max(pos2(x0 + gutter_w, body_top), pos2(x0 + pane_w - 2.0, body_bottom));
            painter.rect_filled(gutter_rect, 0.0, c.gutter_bg);
            painter.rect_filled(text_rect, 0.0, c.text_bg);
            let gp = ui.painter_at(gutter_rect);
            let tp = ui.painter_at(text_rect);
            let pane = &self.panes[p];
            for r in first..last {
                let y = body_top + (r as f32 - self.scroll) * row_h;
                let row_rect = Rect::from_min_size(pos2(text_rect.left(), y), vec2(text_rect.width(), row_h));
                if let Some(bg) = self.row_bg(r, p, c) {
                    tp.rect_filled(row_rect, 0.0, bg);
                    if let Some(bar) = self.row_bar(r, p, c) {
                        gp.rect_filled(Rect::from_min_size(pos2(gutter_rect.right() - 3.0, y), vec2(3.0, row_h)), 0.0, bar);
                    }
                }
                let row = self.diff.rows[r];
                let Some(l) = row.line[p] else { continue };
                if cfg.show_line_numbers {
                    gp.text(
                        pos2(gutter_rect.right() - 6.0, y + row_h / 2.0),
                        Align2::RIGHT_CENTER,
                        (l + 1).to_string(),
                        font.clone(),
                        c.gutter_text,
                    );
                }
                let line = &pane.doc.lines[l];
                let ignored = row.block.map(|b| self.diff.blocks[b].ignored).unwrap_or(false);
                let fg = if ignored { c.gutter_text } else { c.text };
                let sel = row.block.is_some() && row.block == self.cur_block;
                let inline_bg = if sel { c.inline_sel } else { c.inline };
                if let (true, Some(delim)) = (self.table_mode, self.table_delim) {
                    let cells = table::split_row(line, delim);
                    let others: Vec<Vec<String>> = (0..n)
                        .filter(|&q| q != p)
                        .filter_map(|q| row.line[q].map(|ol| table::split_row(&self.panes[q].doc.lines[ol], delim)))
                        .collect();
                    let mut x = text_rect.left() + 4.0 - self.hscroll;
                    for (ci, cell) in cells.iter().enumerate() {
                        let w = (self.col_widths.get(ci).copied().unwrap_or(3) as f32 + 1.0) * char_w + 8.0;
                        let cell_rect = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
                        let differs = row.block.is_some() && !ignored && others.iter().any(|o| o.get(ci) != Some(cell));
                        if differs {
                            tp.rect_filled(cell_rect.shrink(1.0), 0.0, inline_bg);
                        }
                        let cp = tp.with_clip_rect(cell_rect.intersect(text_rect));
                        cp.text(pos2(x + 4.0, y + row_h / 2.0), Align2::LEFT_CENTER, cell, font.clone(), fg);
                        tp.line_segment([pos2(x + w, y), pos2(x + w, y + row_h)], Stroke::new(1.0_f32, c.table_grid));
                        x += w;
                    }
                    continue;
                }
                let spans = if ignored { None } else { pane.hl.as_ref().and_then(|h| h.get(l)) };
                let inline = self.inline_cache.get(&r).map(|v| v[p].as_slice()).unwrap_or(&[]);
                let hits = self.find_hits(line);
                let job = Self::line_job(line, spans, inline, &hits, inline_bg, c.find_hit, fg, &font, tab, cfg.show_whitespace);
                let galley = ui.fonts(|f| f.layout_job(job));
                tp.galley(pos2(text_rect.left() + 4.0 - self.hscroll, y + 1.0), galley, fg);
            }
            if let Some(cr) = self.cursor_row {
                if cr >= first && cr < last {
                    let y = body_top + (cr as f32 - self.scroll) * row_h;
                    let rr = Rect::from_min_size(pos2(text_rect.left() + 1.0, y), vec2(text_rect.width() - 2.0, row_h));
                    let color = if p == self.active_pane { c.cursor_row } else { c.cursor_row.gamma_multiply(0.35) };
                    tp.rect_stroke(rr, 3.0, Stroke::new(1.5_f32, color), StrokeKind::Inside);
                }
            }
            painter.line_segment(
                [pos2(x0 + pane_w - 1.0, avail.top()), pos2(x0 + pane_w - 1.0, body_bottom)],
                Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
        }

        // inline editor
        if let Some(e) = &mut self.edit {
            let visible_row = e.row >= first && e.row < last;
            let mut done: Option<bool> = None;
            if visible_row {
                let x0 = content_left + e.pane as f32 * pane_w + gutter_w;
                let y = body_top + (e.row as f32 - self.scroll) * row_h;
                let rect = Rect::from_min_size(pos2(x0, y - 1.0), vec2(text_w, row_h + 2.0));
                let te = egui::TextEdit::singleline(&mut e.text).font(font.clone()).desired_width(text_w);
                let r = ui.put(rect, te);
                if e.focus {
                    r.request_focus();
                    e.focus = false;
                } else if r.lost_focus() {
                    done = Some(!ui.input(|i| i.key_pressed(Key::Escape)));
                }
            } else {
                done = Some(true);
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
            resp.context_menu(|ui| {
                if let Some(b) = block {
                    if n == 2 {
                        if ui.button(format!("→  {}", tr("cmd.copy_right"))).clicked() {
                            action = Some(MenuAction::Copy(b, 0, 1));
                        }
                        if ui.button(format!("←  {}", tr("cmd.copy_left"))).clicked() {
                            action = Some(MenuAction::Copy(b, 1, 0));
                        }
                    } else {
                        if ui.button(tr("cmd.copy_from_left")).clicked() {
                            action = Some(MenuAction::Copy(b, 0, 1));
                        }
                        if ui.button(tr("cmd.copy_from_right")).clicked() {
                            action = Some(MenuAction::Copy(b, 2, 1));
                        }
                        if ui.button(tr("cmd.copy_middle_left")).clicked() {
                            action = Some(MenuAction::Copy(b, 1, 0));
                        }
                        if ui.button(tr("cmd.copy_middle_right")).clicked() {
                            action = Some(MenuAction::Copy(b, 1, 2));
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
                    if let Some(l) = self.diff.rows.get(r).and_then(|row| row.line[p]) {
                        ui.ctx().copy_text(self.panes[p].doc.lines[l].clone());
                    }
                }
            }
        }

        // location pane
        if cfg.show_location_pane {
            let loc_rect = Rect::from_min_max(pos2(avail.left(), body_top), pos2(avail.left() + loc_w - 2.0, body_bottom));
            let marks: Vec<Mark> = self
                .diff
                .blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| !b.ignored)
                .map(|(i, b)| {
                    let sel = self.cur_block == Some(i);
                    let columns = (0..n)
                        .map(|p| {
                            Some(if b.ranges[p].is_empty() {
                                c.ui.border_strong
                            } else if sel {
                                c.ui.accent
                            } else if b.kind == BlockKind::Conflict {
                                c.conflict_bar
                            } else {
                                c.change_bar
                            })
                        })
                        .collect();
                    Mark { start: b.first_row, len: b.rows, columns }
                })
                .collect();
            if let Some(target) =
                widgets::location_pane(ui, self.id.with("loc"), loc_rect, total, &marks, self.scroll, visible, c.location_bg, c.viewport)
            {
                self.scroll = (target - visible / 2.0).clamp(0.0, max_scroll);
            }
        }
        let vbar = Rect::from_min_max(pos2(content_right, body_top), pos2(avail.right(), body_bottom));
        widgets::vscrollbar(ui, self.id.with("vbar"), vbar, &mut self.scroll, total + 1, visible);
        let hbar = Rect::from_min_max(pos2(content_left, body_bottom), pos2(content_right, avail.bottom()));
        widgets::hscrollbar(ui, self.id.with("hbar"), hbar, &mut self.hscroll, content_w, text_w);
        ui.allocate_rect(avail, Sense::hover());

        // go to line dialog
        if let Some(mut g) = self.goto.take() {
            let mut open = true;
            let mut go = false;
            egui::Window::new(tr("dlg.goto"))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("label.line"));
                        let r = ui.text_edit_singleline(&mut g);
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
