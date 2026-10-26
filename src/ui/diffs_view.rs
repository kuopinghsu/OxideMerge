//! Every difference of a folder compare in one tab: the changed text files listed beside cards
//! showing their differences side by side, which can be copied across, undone and saved.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, Sense, Stroke, Ui, UiBuilder,
};

use crate::config::Config;
use crate::core::comments;
use crate::core::diff::{self, DiffOptions, DiffResult};
use crate::core::folder::{CmpStatus, DirItem};
use crate::core::html_report::{self, FileDiffs};
use crate::core::report::side_name;
use crate::core::rules;
use crate::core::text::{Eol, TextDoc, TextEncoding};
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::folder_view::FolderAction;
use crate::ui::icons::{self, Icon};
use crate::ui::theme::{self, Colors};
use crate::ui::widgets::{self, pal, Outcome};

/// Characters of a line laid out; the rest is never visible in a column.
const SHOWN_CHARS: usize = 1_000;
/// Characters of a line shown when hovering it.
const TIP_CHARS: usize = 2_000;
/// Text of the files kept in memory; past it, files out of view are read again when needed.
const MEMORY_BUDGET: usize = 256 << 20;
/// Word differences kept for rows already shown.
const CELL_CACHE: usize = 20_000;
/// How often the files in view are checked for changes on disk.
const DISK_CHECK: Duration = Duration::from_secs(1);
const FILE_H: f32 = 34.0;
const HUNK_H: f32 = 28.0;
const NOTE_H: f32 = 30.0;
const GAP_H: f32 = 14.0;
const LIST_ROW_H: f32 = 26.0;

/// Modification time and size of a file, or `None` without one: how a change on disk is noticed.
type Stamp = Option<(Option<SystemTime>, u64)>;

fn stamp(path: &Path) -> Stamp {
    std::fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| (m.modified().ok(), m.len()))
}

/// A file read on another thread: its index, then what [`read_card`] returns.
type Read = (usize, DirItem, FileDiffs, Vec<Stamp>);

/// How each side of a row is shown and the byte ranges that differ.
type Cells = Vec<(&'static str, Vec<Range<usize>>)>;

/// Encoding, byte order mark, line ending and final newline: what a file is written with.
type Format = (TextEncoding, bool, Eol, bool);

fn format_of(d: &TextDoc) -> Format {
    (d.encoding, d.bom, d.eol, d.final_newline)
}

fn set_format(d: &mut TextDoc, f: Format) {
    (d.encoding, d.bom, d.eol, d.final_newline) = f;
}

struct Diffs {
    docs: Vec<TextDoc>,
    result: DiffResult,
    /// Each difference shown: its block and its rows, with the unchanged rows around it.
    hunks: Vec<(usize, Range<usize>)>,
    /// Sides with no file: missing when read, or emptied by copying from a missing side (the
    /// file is deleted when saved).
    absent: Vec<bool>,
    stamps: Vec<Stamp>,
    /// Why no differences are listed, as a language key, when there are none.
    quiet: Option<&'static str>,
    /// Bytes the text takes in memory; 0 once dropped to save memory.
    size: usize,
}

impl Diffs {
    fn new(docs: Vec<TextDoc>, result: DiffResult, absent: Vec<bool>, stamps: Vec<Stamp>) -> Self {
        let mut d = Diffs {
            docs,
            result,
            hunks: Vec::new(),
            absent,
            stamps,
            quiet: None,
            size: 0,
        };
        d.settle();
        d
    }

    fn recompute(&mut self, opts: &DiffOptions) {
        let refs: Vec<&[String]> = self.docs.iter().map(|d| d.lines.as_slice()).collect();
        let path = self
            .docs
            .iter()
            .find_map(|d| d.path.clone())
            .unwrap_or_default();
        self.result = diff::compute_with(&refs, opts, comments::for_path(&path));
        self.settle();
    }

    fn settle(&mut self) {
        self.hunks = html_report::hunks(&self.result);
        self.quiet = self.hunks.is_empty().then(|| {
            let first = &self.docs[0];
            let same_lines = self.docs.iter().all(|d| d.lines == first.lines);
            let present: Vec<Format> = self
                .docs
                .iter()
                .zip(&self.absent)
                .filter(|(_, &a)| !a)
                .map(|(d, _)| format_of(d))
                .collect();
            let same_files = self.absent.iter().all(|&a| a == self.absent[0])
                && present.windows(2).all(|w| w[0] == w[1]);
            if same_lines && same_files {
                "report.no_diffs"
            } else if same_lines {
                "diffs.format_only"
            } else {
                "diffs.ignored_only"
            }
        });
        self.size = self
            .docs
            .iter()
            .flat_map(|d| &d.lines)
            .map(|l| l.len() + std::mem::size_of::<String>())
            .sum::<usize>()
            .max(1);
    }

    fn loaded(&self) -> bool {
        self.size > 0
    }

    /// Nothing differs, not even how the files are written.
    fn same(&self) -> bool {
        self.quiet == Some("report.no_diffs")
    }

    fn modified(&self) -> bool {
        self.docs.iter().any(|d| d.modified)
    }
}

enum Body {
    Loading,
    /// Why no differences are shown.
    Note(String),
    Diffs(Box<Diffs>),
}

struct Card {
    item: DirItem,
    body: Body,
    open: bool,
    /// Frame the card was last in view.
    seen: u64,
}

impl Card {
    fn diffs(&self) -> Option<&Diffs> {
        match &self.body {
            Body::Diffs(d) => Some(d),
            _ => None,
        }
    }

    fn hunk_count(&self) -> usize {
        self.diffs().map_or(0, |d| d.hunks.len())
    }

    fn modified(&self) -> bool {
        self.diffs().is_some_and(Diffs::modified)
    }

    fn differs(&self) -> bool {
        match &self.body {
            Body::Diffs(d) => !d.same(),
            Body::Note(_) => true,
            Body::Loading => false,
        }
    }
}

/// Reads and compares the sides of `item` as they are now on disk.
fn read_card(
    mut item: DirItem,
    roots: &[PathBuf],
    opts: &DiffOptions,
) -> (DirItem, FileDiffs, Vec<Stamp>) {
    // Taken before reading: a change while reading shows as a change on disk.
    let stamps: Vec<Stamp> = roots.iter().map(|r| stamp(&item.path(r))).collect();
    for (s, st) in stamps.iter().enumerate() {
        item.exists[s] = st.is_some();
        item.size[s] = st.map(|x| x.1);
    }
    let found = html_report::file_docs(&item, roots, opts);
    (item, found, stamps)
}

fn body_of(item: &DirItem, found: FileDiffs, stamps: Vec<Stamp>) -> Body {
    match found {
        FileDiffs::Diffs(docs, result) => {
            let absent = (0..docs.len()).map(|s| !item.exists[s]).collect();
            Body::Diffs(Box::new(Diffs::new(docs, result, absent, stamps)))
        }
        FileDiffs::Note(note) => Body::Note(note),
        FileDiffs::None => Body::Note(tr("report.no_diffs").into()),
    }
}

/// A line of the scrolled list of cards.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// Header of file `f`.
    File(usize),
    /// Header of difference `h` of file `f`.
    Hunk(usize, usize),
    /// Row `r` of the compare of file `f`.
    Row(usize, usize),
    /// Why file `f` shows no differences.
    Note(usize),
    /// Space after a card.
    Gap,
}

/// A copy that can be undone: the lines it replaced on a side and what that side was before.
struct Undo {
    file: usize,
    side: usize,
    at: usize,
    old: Vec<String>,
    new_len: usize,
    modified: bool,
    absent: bool,
    format: Format,
}

/// What a frame paints rows with.
struct Look<'a> {
    mono: &'a FontId,
    char_w: f32,
    colors: &'a Colors,
    tab: usize,
}

pub struct DiffsView {
    roots: Vec<PathBuf>,
    display_roots: Vec<String>,
    read_only: Vec<bool>,
    dopts: DiffOptions,
    rules: String,
    /// Names of the compared folders, for the title.
    names: String,
    cards: Vec<Card>,
    loading: Option<Receiver<Read>>,
    lines: Vec<Line>,
    /// Top of every line, and the height of all of them.
    tops: Vec<f32>,
    height: f32,
    /// Height of a row of text `tops` was measured with.
    row_h: f32,
    stale: bool,
    cells: HashMap<(usize, usize), Cells>,
    frame: u64,
    disk_checked: Instant,
    search: String,
    focus_search: bool,
    /// The selected difference: a file and the index of one of its differences.
    current: Option<(usize, usize)>,
    scroll_to: Option<Line>,
    /// First line in view, where moving to the next difference starts without a selection.
    top_line: usize,
    undo: Vec<Undo>,
    pub status: String,
    /// Id of the folder compare tab this was opened from.
    pub folder: u64,
    /// A file to open in its own tab, until the app takes it.
    pub open_request: Option<FolderAction>,
    /// Files were saved, so the folder compare should compare again; until the app takes it.
    pub saved: bool,
    id: egui::Id,
}

pub struct Source {
    pub roots: Vec<PathBuf>,
    pub display_roots: Vec<String>,
    pub read_only: Vec<bool>,
    pub dopts: DiffOptions,
    pub rules: String,
    pub title: String,
    /// The files with differences, in the order to list them.
    pub items: Vec<DirItem>,
}

impl DiffsView {
    pub fn new(src: Source, folder: u64) -> Self {
        let cards: Vec<Card> = src
            .items
            .into_iter()
            .filter(|it| {
                !it.is_dir
                    && !it.binary
                    && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
            })
            .map(|item| Card {
                item,
                body: Body::Loading,
                open: true,
                seen: 0,
            })
            .collect();
        let mut v = DiffsView {
            roots: src.roots,
            display_roots: src.display_roots,
            read_only: src.read_only,
            dopts: src.dopts,
            rules: src.rules,
            names: src.title,
            cards,
            loading: None,
            lines: Vec::new(),
            tops: Vec::new(),
            height: 0.0,
            row_h: 0.0,
            stale: true,
            cells: HashMap::new(),
            frame: 0,
            disk_checked: Instant::now(),
            search: String::new(),
            focus_search: false,
            current: None,
            scroll_to: None,
            top_line: 0,
            undo: Vec::new(),
            status: String::new(),
            folder,
            open_request: None,
            saved: false,
            id: egui::Id::new(("diffs_view", folder, SystemTime::now())),
        };
        v.load((0..v.cards.len()).collect());
        v
    }

    /// No file of the folder compare can be shown.
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    fn n(&self) -> usize {
        self.roots.len()
    }

    fn writable(&self, side: usize) -> bool {
        !self.read_only.get(side).copied().unwrap_or(false)
    }

    /// Reads and compares the files `which` on another thread.
    fn load(&mut self, which: Vec<usize>) {
        let (tx, rx) = mpsc::channel();
        let items: Vec<(usize, DirItem)> = which
            .iter()
            .map(|&f| (f, self.cards[f].item.clone()))
            .collect();
        let (roots, dopts) = (self.roots.clone(), self.dopts.clone());
        std::thread::spawn(move || {
            for (f, it) in items {
                let (it, found, stamps) = read_card(it, &roots, &dopts);
                if tx.send((f, it, found, stamps)).is_err() {
                    return;
                }
            }
        });
        for f in which {
            self.cards[f].body = Body::Loading;
            self.forget_cells(f);
        }
        self.loading = Some(rx);
        self.stale = true;
    }

    fn poll(&mut self) {
        let Some(rx) = &self.loading else { return };
        loop {
            match rx.try_recv() {
                Ok((f, item, found, stamps)) => {
                    self.cards[f].body = body_of(&item, found, stamps);
                    self.cards[f].item = item;
                    self.stale = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.loading = None;
                    break;
                }
            }
        }
    }

    /// Reads file `f` again now. Returns whether it was the same on disk as before.
    fn reload_card(&mut self, f: usize) -> bool {
        let before = self.cards[f].diffs().map(|d| d.stamps.clone());
        let (item, found, stamps) = read_card(self.cards[f].item.clone(), &self.roots, &self.dopts);
        let same = before.as_ref() == Some(&stamps);
        self.cards[f].body = body_of(&item, found, stamps);
        self.cards[f].item = item;
        self.forget_cells(f);
        if !same {
            self.undo.retain(|u| u.file != f);
            let left = self.cards[f].hunk_count();
            if let Some((cf, h)) = self.current.filter(|&(cf, _)| cf == f) {
                self.current = (left > 0).then_some((cf, h.min(left.saturating_sub(1))));
            }
        }
        self.stale = true;
        same
    }

    /// Makes sure the text of file `f` is in memory. `None` when it has no differences to
    /// show, otherwise whether it was the same on disk as when first read.
    fn ensure_loaded(&mut self, f: usize) -> Option<bool> {
        match &self.cards[f].body {
            Body::Diffs(d) if d.loaded() => Some(true),
            Body::Diffs(_) => {
                let same = self.reload_card(f);
                self.cards[f].diffs().map(|_| same)
            }
            _ => None,
        }
    }

    fn forget_cells(&mut self, f: usize) {
        self.cells.retain(|k, _| k.0 != f);
    }

    fn ensure_cells(&mut self, f: usize, r: usize) {
        if self.cells.contains_key(&(f, r)) {
            return;
        }
        let Some(d) = self.cards[f].diffs().filter(|d| d.loaded()) else {
            return;
        };
        if r >= d.result.rows.len() {
            return;
        }
        let refs: Vec<&[String]> = d.docs.iter().map(|d| d.lines.as_slice()).collect();
        let cells = html_report::row_cells(&d.result, &refs, &self.dopts, r);
        if self.cells.len() >= CELL_CACHE {
            self.cells.clear();
        }
        self.cells.insert((f, r), cells);
    }

    /// Drops the text of files out of view, without changes, while over [`MEMORY_BUDGET`].
    fn trim_memory(&mut self) {
        let mut total: usize = self
            .cards
            .iter()
            .filter_map(|c| c.diffs())
            .map(|d| d.size)
            .sum();
        if total <= MEMORY_BUDGET {
            return;
        }
        let frame = self.frame;
        let mut idle: Vec<usize> = (0..self.cards.len())
            .filter(|&f| {
                let c = &self.cards[f];
                c.seen + 1 < frame && c.diffs().is_some_and(|d| d.loaded() && !d.modified())
            })
            .collect();
        idle.sort_by_key(|&f| self.cards[f].seen);
        for f in idle {
            if total <= MEMORY_BUDGET / 4 * 3 {
                break;
            }
            if let Body::Diffs(d) = &mut self.cards[f].body {
                for doc in &mut d.docs {
                    doc.lines = Vec::new();
                }
                total -= d.size;
                d.size = 0;
            }
            self.forget_cells(f);
        }
    }

    /// Reads again the files in view that changed on disk and have no changes here.
    fn check_disk(&mut self) {
        if self.disk_checked.elapsed() < DISK_CHECK || self.loading.is_some() {
            return;
        }
        self.disk_checked = Instant::now();
        let frame = self.frame;
        let changed: Vec<usize> = (0..self.cards.len())
            .filter(|&f| {
                let c = &self.cards[f];
                c.seen + 1 >= frame
                    && c.diffs().is_some_and(|d| {
                        !d.modified()
                            && self
                                .roots
                                .iter()
                                .zip(&d.stamps)
                                .any(|(r, st)| stamp(&c.item.path(r)) != *st)
                    })
            })
            .collect();
        for f in changed {
            self.reload_card(f);
        }
    }

    pub fn title(&self) -> String {
        format!("{}: {}", tr("cmd.all_diffs"), self.names)
    }

    pub fn is_modified(&self) -> bool {
        self.cards.iter().any(Card::modified)
    }

    pub fn outcome(&self) -> Outcome {
        if self.loading.is_some() {
            Outcome::Busy
        } else if self.cards.iter().any(Card::differs) {
            Outcome::Different
        } else {
            Outcome::Identical
        }
    }

    pub fn status_line(&self) -> String {
        let diffs: usize = self.cards.iter().map(Card::hunk_count).sum();
        let mut s = trf(
            "diffs.summary",
            &[
                ("files", self.cards.len().to_string()),
                ("n", diffs.to_string()),
            ],
        );
        if self.loading.is_some() {
            let done = self
                .cards
                .iter()
                .filter(|c| !matches!(c.body, Body::Loading))
                .count();
            s = format!(
                "{} {done}/{}  |  {s}",
                tr("msg.comparing"),
                self.cards.len()
            );
        }
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    /// Reads the compare rules and options again, then the files without changes here.
    pub fn refresh(&mut self, cfg: &Config) {
        self.dopts = rules::options_with(&cfg.diff, rules::find(&self.rules).as_ref());
        self.cells.clear();
        let (kept, reread): (Vec<usize>, Vec<usize>) =
            (0..self.cards.len()).partition(|&f| self.cards[f].modified());
        for &f in &kept {
            if let Body::Diffs(d) = &mut self.cards[f].body {
                d.recompute(&self.dopts);
            }
        }
        self.undo.retain(|u| kept.contains(&u.file));
        if self.current.is_some_and(|(f, _)| !kept.contains(&f)) {
            self.current = None;
        }
        self.status = if kept.is_empty() {
            String::new()
        } else {
            tr("diffs.kept").into()
        };
        self.load(reread);
    }

    /// The compare rules file was renamed (or deleted, `new` = "").
    pub fn rules_renamed(&mut self, new: &str) {
        self.rules = new.to_string();
    }

    pub fn rules(&self) -> &str {
        &self.rules
    }

    pub fn save_modified(&mut self) -> anyhow::Result<()> {
        let mut saved = 0;
        let mut errors = Vec::new();
        for card in &mut self.cards {
            let Body::Diffs(d) = &mut card.body else {
                continue;
            };
            for side in 0..d.docs.len() {
                if !d.docs[side].modified {
                    continue;
                }
                let Some(path) = d.docs[side].path.clone() else {
                    continue;
                };
                if stamp(&path) != d.stamps[side] {
                    let file = path.display().to_string();
                    errors.push(trf("diffs.changed_on_disk", &[("file", file)]));
                    continue;
                }
                let done = if !d.absent[side] {
                    d.docs[side].save()
                } else if path.exists() {
                    crate::core::fsutil::remove(&path)
                } else {
                    Ok(())
                };
                match done {
                    Ok(()) => {
                        saved += 1;
                        d.docs[side].modified = false;
                        card.item.exists[side] = !d.absent[side];
                        d.stamps[side] = stamp(&path);
                    }
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
        if saved > 0 {
            // Undoing a saved copy changes the file again.
            for u in &mut self.undo {
                u.modified = true;
            }
            self.saved = true;
            self.status = trf("diffs.saved", &[("n", saved.to_string())]);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(errors.join("\n"))
        }
    }

    pub fn focus_search(&mut self) {
        self.focus_search = true;
    }

    pub fn expand_all(&mut self, open: bool) {
        for c in &mut self.cards {
            c.open = open;
        }
        self.stale = true;
    }

    fn shown(&self, f: usize) -> bool {
        let q = self.search.trim();
        q.is_empty()
            || self.cards[f]
                .item
                .rel_path()
                .to_lowercase()
                .contains(&q.to_lowercase())
    }

    fn rebuild(&mut self, row_h: f32) {
        if self.stale {
            self.lines.clear();
            for f in 0..self.cards.len() {
                if !self.shown(f) {
                    continue;
                }
                self.lines.push(Line::File(f));
                let card = &self.cards[f];
                if card.open {
                    match card.diffs() {
                        Some(d) if !d.hunks.is_empty() => {
                            for (h, (_, rows)) in d.hunks.iter().enumerate() {
                                self.lines.push(Line::Hunk(f, h));
                                self.lines.extend(rows.clone().map(|r| Line::Row(f, r)));
                            }
                        }
                        _ => self.lines.push(Line::Note(f)),
                    }
                }
                self.lines.push(Line::Gap);
            }
        } else if self.row_h == row_h {
            return;
        }
        self.stale = false;
        self.row_h = row_h;
        self.tops.clear();
        let mut y = 0.0;
        for l in &self.lines {
            self.tops.push(y);
            y += match l {
                Line::File(_) => FILE_H,
                Line::Hunk(..) => HUNK_H,
                Line::Row(..) => row_h,
                Line::Note(_) => NOTE_H,
                Line::Gap => GAP_H,
            };
        }
        self.height = y;
    }

    // ---------- moving between differences ----------

    /// Every difference of the files listed, in order.
    fn all_hunks(&self) -> Vec<(usize, usize)> {
        (0..self.cards.len())
            .filter(|&f| self.shown(f))
            .flat_map(|f| (0..self.cards[f].hunk_count()).map(move |h| (f, h)))
            .collect()
    }

    fn select(&mut self, target: (usize, usize)) {
        self.current = Some(target);
        self.status.clear();
    }

    fn go(&mut self, target: (usize, usize)) {
        self.select(target);
        if !self.cards[target.0].open {
            self.cards[target.0].open = true;
            self.stale = true;
        }
        self.scroll_to = Some(Line::Hunk(target.0, target.1));
    }

    pub fn step(&mut self, forward: bool) {
        let all = self.all_hunks();
        let k = match self.current.and_then(|c| all.iter().position(|&x| x == c)) {
            Some(k) if forward => k + 1,
            Some(k) => k.wrapping_sub(1),
            None => {
                // From what is in view: the first difference at or below its top.
                let at = |&(f, h): &(usize, usize)| {
                    self.lines
                        .iter()
                        .position(|&l| l == Line::Hunk(f, h) || l == Line::File(f))
                        .unwrap_or(0)
                };
                let i = all.partition_point(|x| at(x) < self.top_line);
                if forward {
                    i
                } else {
                    i.wrapping_sub(1)
                }
            }
        };
        match all.get(k) {
            Some(&t) => self.go(t),
            None => self.status = tr("diffs.no_more").into(),
        }
    }

    pub fn first(&mut self) {
        match self.all_hunks().first() {
            Some(&t) => self.go(t),
            None => self.status = tr("report.no_diffs").into(),
        }
    }

    pub fn last(&mut self) {
        match self.all_hunks().last() {
            Some(&t) => self.go(t),
            None => self.status = tr("report.no_diffs").into(),
        }
    }

    fn reveal(&mut self, f: usize) {
        if !self.cards[f].open {
            self.cards[f].open = true;
            self.stale = true;
        }
        self.current = (self.cards[f].hunk_count() > 0).then_some((f, 0));
        self.status.clear();
        self.scroll_to = Some(Line::File(f));
    }

    // ---------- merging ----------

    /// Copies the selected difference to the right (from the left side), or to the left.
    pub fn copy_current(&mut self, to_right: bool) {
        let Some((f, h)) = self.current else { return };
        let (from, to) = match (self.n(), to_right) {
            (2, true) => (0, 1),
            (2, false) => (1, 0),
            (_, true) => (0, 1),
            (_, false) => (2, 1),
        };
        self.copy_hunk(f, h, from, to);
    }

    fn copy_hunk(&mut self, f: usize, h: usize, from: usize, to: usize) {
        if !self.writable(to) {
            self.status = tr("msg.readonly").into();
            return;
        }
        if self.ensure_loaded(f) != Some(true) {
            return;
        }
        let Some(d) = self.cards[f].diffs() else {
            return;
        };
        let Some(&(b, _)) = d.hunks.get(h) else {
            return;
        };
        let blk = &d.result.blocks[b];
        let new = d.docs[from].lines[blk.ranges[from].clone()].to_vec();
        let at = blk.ranges[to].clone();
        self.change(f, from, to, at, new, false);
        let left = self.cards[f].hunk_count();
        self.current = (left > 0).then_some((f, h.min(left.saturating_sub(1))));
    }

    /// Makes side `to` of file `f` the same as side `from`, written the same way too.
    fn copy_file(&mut self, f: usize, from: usize, to: usize) {
        if !self.writable(to) {
            self.status = tr("msg.readonly").into();
            return;
        }
        if self.ensure_loaded(f) != Some(true) {
            return;
        }
        let Some(d) = self.cards[f].diffs() else {
            return;
        };
        let new = d.docs[from].lines.clone();
        let at = 0..d.docs[to].lines.len();
        self.change(f, from, to, at, new, true);
        if self.current.is_some_and(|(cf, _)| cf == f) {
            self.current = (self.cards[f].hunk_count() > 0).then_some((f, 0));
        }
    }

    /// Replaces lines `at` of side `to` with `new`, copied from side `from`.
    fn change(
        &mut self,
        f: usize,
        from: usize,
        to: usize,
        at: Range<usize>,
        new: Vec<String>,
        whole: bool,
    ) {
        let Body::Diffs(d) = &mut self.cards[f].body else {
            return;
        };
        let new_len = new.len();
        let old: Vec<String> = d.docs[to].lines.splice(at.clone(), new).collect();
        self.undo.push(Undo {
            file: f,
            side: to,
            at: at.start,
            new_len,
            old,
            modified: d.docs[to].modified,
            absent: d.absent[to],
            format: format_of(&d.docs[to]),
        });
        // A new file is written like the one it is copied from.
        if !d.absent[from] && (whole || d.absent[to]) {
            let f = format_of(&d.docs[from]);
            set_format(&mut d.docs[to], f);
        }
        d.absent[to] = d.absent[from] && d.docs[to].lines.is_empty();
        d.docs[to].modified = true;
        d.recompute(&self.dopts);
        self.forget_cells(f);
        self.status.clear();
        self.stale = true;
    }

    pub fn undo(&mut self) {
        let Some(u) = self.undo.pop() else { return };
        if self.ensure_loaded(u.file) != Some(true) {
            return;
        }
        let Body::Diffs(d) = &mut self.cards[u.file].body else {
            return;
        };
        let doc = &mut d.docs[u.side];
        if u.at + u.new_len > doc.lines.len() {
            return;
        }
        doc.lines.splice(u.at..u.at + u.new_len, u.old);
        doc.modified = u.modified;
        set_format(doc, u.format);
        d.absent[u.side] = u.absent;
        d.recompute(&self.dopts);
        let n = d.hunks.len();
        self.current = (n > 0).then_some((u.file, 0));
        self.scroll_to = Some(Line::File(u.file));
        self.forget_cells(u.file);
        self.status.clear();
        self.stale = true;
    }

    fn open_file(&mut self, f: usize) {
        if self.cards[f].modified() {
            self.status = tr("diffs.unsaved").into();
            return;
        }
        let it = &self.cards[f].item;
        let n = self.n();
        let rel = it.rel_path();
        let descriptions = (0..n)
            .map(|s| {
                let root = self.display_roots.get(s).filter(|_| self.read_only[s])?;
                Some(format!("{root}/{rel}"))
            })
            .collect();
        self.open_request = Some(FolderAction::OpenFiles {
            paths: self.roots.iter().map(|r| it.path(r)).collect(),
            missing: (0..n).map(|s| !it.exists[s]).collect(),
            read_only: self.read_only.clone(),
            descriptions,
            kind: None,
            rel,
            rules: self.rules.clone(),
        });
    }

    // ---------- UI ----------

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, colors: &Colors) {
        self.frame += 1;
        self.poll();
        self.check_disk();
        self.trim_memory();
        if self.loading.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        } else {
            ui.ctx().request_repaint_after(DISK_CHECK);
        }
        self.toolbar(ui, cfg);
        egui::SidePanel::left(self.id.with("files"))
            .resizable(true)
            .default_width(260.0)
            .width_range(160.0..=520.0)
            .frame(
                egui::Frame::new()
                    .fill(pal(ui).surface_alt)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show_inside(ui, |ui| self.file_list(ui));
        if self.cards.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(tr("diffs.empty")).color(pal(ui).text_dim));
            });
            return;
        }
        self.cards_ui(ui, cfg, colors);
    }

    fn toolbar(&mut self, ui: &mut Ui, cfg: &Config) {
        let n = self.n();
        let cur = self.current.is_some();
        let mut refresh = false;
        widgets::toolbar(ui, |ui| {
            if widgets::ghost_icon_button(ui, Icon::Refresh, tr("cmd.refresh"))
                .on_hover_text(Cmd::Refresh.shown_key())
                .clicked()
            {
                refresh = true;
            }
            if widgets::icon_button(ui, Icon::ExpandAll, tr("cmd.expand_all")).clicked() {
                self.expand_all(true);
            }
            if widgets::icon_button(ui, Icon::CollapseAll, tr("cmd.collapse_all")).clicked() {
                self.expand_all(false);
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
                self.first();
            }
            if widgets::icon_button(ui, Icon::Prev, &Cmd::PrevDiff.tip()).clicked() {
                self.step(false);
            }
            if widgets::icon_button(ui, Icon::Next, &Cmd::NextDiff.tip()).clicked() {
                self.step(true);
            }
            if widgets::icon_button(ui, Icon::Last, &Cmd::LastDiff.tip()).clicked() {
                self.last();
            }
            widgets::toolbar_sep(ui);
            let (right_tip, left_tip) = if n == 2 {
                (Cmd::CopyRight.tip(), Cmd::CopyLeft.tip())
            } else {
                (
                    tr("cmd.copy_from_left").to_string(),
                    tr("cmd.copy_from_right").to_string(),
                )
            };
            let (to_right, to_left) = if n == 2 { (1, 0) } else { (1, 1) };
            if widgets::icon_button_enabled(
                ui,
                cur && self.writable(to_right),
                Icon::CopyRight,
                &right_tip,
            )
            .clicked()
            {
                self.copy_current(true);
            }
            if widgets::icon_button_enabled(
                ui,
                cur && self.writable(to_left),
                Icon::CopyLeft,
                &left_tip,
            )
            .clicked()
            {
                self.copy_current(false);
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button_enabled(ui, !self.undo.is_empty(), Icon::Undo, &Cmd::Undo.tip())
                .clicked()
            {
                self.undo();
            }
            if widgets::icon_button_enabled(ui, self.is_modified(), Icon::File, &Cmd::Save.tip())
                .clicked()
            {
                if let Err(e) = self.save_modified() {
                    self.status = e.to_string();
                }
            }
        });
        if refresh {
            self.refresh(cfg);
        }
    }

    fn file_list(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let n = self.n();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr("report.files"))
                    .font(theme::semibold(13.0))
                    .color(p.text),
            );
            ui.label(egui::RichText::new(self.cards.len().to_string()).color(p.text_dim));
        });
        ui.add_space(6.0);
        let hint = widgets::with_key(tr("report.search"), Cmd::Find.key());
        let search = widgets::search_edit(ui, &mut self.search, &hint, ui.available_width() - 56.0);
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.changed() {
            self.stale = true;
        }
        ui.add_space(6.0);
        let listed: Vec<usize> = (0..self.cards.len()).filter(|&f| self.shown(f)).collect();
        let current_file = self.current.map(|(f, _)| f);
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt(self.id.with("list"))
            .auto_shrink(false)
            .show_rows(ui, LIST_ROW_H, listed.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &f in &listed[range] {
                    let card = &self.cards[f];
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), LIST_ROW_H),
                        Sense::click(),
                    );
                    let painter = ui.painter_at(rect);
                    if current_file == Some(f) {
                        painter.rect_filled(rect, 6.0, p.accent_soft);
                    } else if resp.hovered() {
                        painter.rect_filled(rect, 6.0, p.control_hover);
                    }
                    let dot = card_color(&p, card, n);
                    painter.circle_filled(pos2(rect.left() + 10.0, rect.center().y), 3.5, dot);
                    let count = match &card.body {
                        Body::Loading => "…".to_string(),
                        Body::Note(_) => "–".to_string(),
                        Body::Diffs(d) => d.hunks.len().to_string(),
                    };
                    let count_g =
                        painter.layout_no_wrap(count, FontId::proportional(12.0), p.text_dim);
                    let right = rect.right() - 8.0 - count_g.size().x;
                    painter.galley(
                        pos2(right, rect.center().y - count_g.size().y / 2.0),
                        count_g,
                        p.text_dim,
                    );
                    let mut name = card.item.name.clone();
                    if card.modified() {
                        name.insert_str(0, "● ");
                    }
                    let name_rect = Rect::from_min_max(
                        pos2(rect.left() + 20.0, rect.top()),
                        pos2(right - 6.0, rect.bottom()),
                    );
                    let mut job = LayoutJob::default();
                    job.append(
                        &name,
                        0.0,
                        TextFormat::simple(FontId::proportional(13.0), p.text),
                    );
                    if !card.item.rel_dir.is_empty() {
                        job.append(
                            &card.item.rel_dir,
                            8.0,
                            TextFormat::simple(FontId::proportional(11.5), p.text_dim),
                        );
                    }
                    job.wrap = one_line(name_rect.width());
                    let g = ui.fonts(|fonts| fonts.layout_job(job));
                    painter.galley(
                        pos2(name_rect.left(), rect.center().y - g.size().y / 2.0),
                        g,
                        p.text,
                    );
                    if resp
                        .on_hover_text(card.item.rel_path())
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        clicked = Some(f);
                    }
                }
            });
        if let Some(f) = clicked {
            self.reveal(f);
        }
    }

    fn cards_ui(&mut self, ui: &mut Ui, cfg: &Config, colors: &Colors) {
        let p = pal(ui);
        let mono = FontId::monospace(cfg.font_size);
        let row_h = (ui.fonts(|f| f.row_height(&mono)) + 3.0).round();
        let look = Look {
            mono: &mono,
            char_w: ui.fonts(|f| f.glyph_width(&mono, '0')),
            colors,
            tab: cfg.tab_size.max(1),
        };
        self.rebuild(row_h);
        let n = self.n();

        // Names of the sides above the columns.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        let painter = ui.painter_at(head);
        painter.rect_filled(head, 0.0, p.surface_alt);
        painter.hline(
            head.x_range(),
            head.bottom() - 0.5,
            Stroke::new(1.0_f32, p.border),
        );
        let inner = head.shrink2(vec2(12.0, 0.0));
        let col_w = inner.width() / n as f32;
        for s in 0..n {
            let col = Rect::from_min_size(
                pos2(inner.left() + s as f32 * col_w, head.top()),
                vec2(col_w, head.height()),
            );
            let mut job = LayoutJob::default();
            job.append(
                side_name(s, n),
                0.0,
                TextFormat::simple(theme::semibold(12.0), widgets::side_color(ui, s, n)),
            );
            let root = self.display_roots.get(s).cloned().unwrap_or_default();
            job.append(
                &root,
                8.0,
                TextFormat::simple(FontId::proportional(12.0), p.text_dim),
            );
            job.wrap = one_line(col.width() - 8.0);
            let g = ui.fonts(|f| f.layout_job(job));
            painter.galley(
                pos2(col.left() + 4.0, col.center().y - g.size().y / 2.0),
                g,
                p.text,
            );
        }

        let mut area = egui::ScrollArea::vertical()
            .id_salt(self.id.with("cards"))
            .auto_shrink(false);
        if let Some(target) = self.scroll_to.take() {
            if let Some(i) = self.lines.iter().position(|&l| l == target) {
                area = area.vertical_scroll_offset((self.tops[i] - FILE_H).max(0.0));
            }
        }
        let mut act: Option<Act> = None;
        let found = &mut act;
        area.show_viewport(ui, |ui, view| {
            let width = ui.available_width();
            ui.set_min_height(self.height);
            ui.set_min_width(width);
            let origin = ui.max_rect().min;
            let first = self
                .tops
                .partition_point(|&t| t <= view.min.y)
                .saturating_sub(1);
            self.top_line = first;
            let end = first + self.tops[first..].partition_point(|&t| t <= view.max.y);
            // The text and word differences of what is in view, before painting it.
            for i in first..end {
                match self.lines[i] {
                    Line::File(f) | Line::Hunk(f, _) | Line::Note(f) => {
                        self.cards[f].seen = self.frame;
                    }
                    Line::Row(f, r) => {
                        self.cards[f].seen = self.frame;
                        if self.ensure_loaded(f) == Some(true) {
                            self.ensure_cells(f, r);
                        }
                    }
                    Line::Gap => {}
                }
            }
            if self.stale {
                // A file changed on disk while being read again; lay it out next frame.
                ui.ctx().request_repaint();
                return;
            }
            for i in first..end {
                let top = self.tops[i];
                let bottom = self.tops.get(i + 1).copied().unwrap_or(self.height);
                let rect = Rect::from_min_max(
                    pos2(origin.x + 12.0, origin.y + top),
                    pos2(origin.x + width - 12.0, origin.y + bottom),
                );
                let a = match self.lines[i] {
                    Line::File(f) => self.file_header(ui, rect, f),
                    Line::Hunk(f, h) => self.hunk_header(ui, rect, f, h),
                    Line::Row(f, r) => self.row(ui, rect, f, r, &look),
                    Line::Note(f) => {
                        self.note(ui, rect, f);
                        None
                    }
                    Line::Gap => {
                        ui.painter().hline(
                            rect.x_range(),
                            rect.top() + 0.5,
                            Stroke::new(1.0_f32, p.border),
                        );
                        None
                    }
                };
                if found.is_none() {
                    *found = a;
                }
            }
        });
        match act {
            Some(Act::Toggle(f)) => {
                self.cards[f].open ^= true;
                self.stale = true;
            }
            Some(Act::Select(f, h)) => self.select((f, h)),
            Some(Act::Copy(f, h, from, to)) => {
                self.select((f, h));
                self.copy_hunk(f, h, from, to);
            }
            Some(Act::CopyFile(f, from, to)) => self.copy_file(f, from, to),
            Some(Act::Open(f)) => self.open_file(f),
            None => {}
        }
    }

    fn file_header(&self, ui: &mut Ui, rect: Rect, f: usize) -> Option<Act> {
        let p = pal(ui);
        let card = &self.cards[f];
        let n = self.n();
        let resp = ui.interact(rect, self.id.with(("file", f)), Sense::click());
        let painter = ui.painter();
        let fill = if resp.hovered() {
            p.control_hover
        } else {
            p.surface_alt
        };
        painter.rect(
            rect,
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: if card.open { 0 } else { 8 },
                se: if card.open { 0 } else { 8 },
            },
            fill,
            Stroke::new(1.0_f32, p.border),
            egui::StrokeKind::Inside,
        );
        let c = pos2(rect.left() + 16.0, rect.center().y);
        let tri = if card.open {
            vec![
                c + vec2(-4.5, -2.5),
                c + vec2(4.5, -2.5),
                c + vec2(0.0, 3.5),
            ]
        } else {
            vec![
                c + vec2(-2.5, -4.5),
                c + vec2(3.5, 0.0),
                c + vec2(-2.5, 4.5),
            ]
        };
        painter.add(egui::Shape::convex_polygon(tri, p.text_dim, Stroke::NONE));
        icons::paint(
            painter,
            Rect::from_center_size(pos2(rect.left() + 36.0, c.y), vec2(15.0, 15.0)),
            Icon::Text,
            card_color(&p, card, n),
        );

        // Buttons on the right, then what is known of the file before them.
        let buttons = Rect::from_min_max(
            pos2(rect.right() - 120.0, rect.top()),
            pos2(rect.right() - 6.0, rect.bottom()),
        );
        let mut act = None;
        let mut bui = ui.new_child(
            UiBuilder::new()
                .id_salt(("file_buttons", f))
                .max_rect(buttons)
                .layout(Layout::right_to_left(Align::Center)),
        );
        bui.spacing_mut().item_spacing.x = 2.0;
        if widgets::icon_button(&mut bui, Icon::Compare, tr("diffs.open")).clicked() {
            act = Some(Act::Open(f));
        }
        if n == 2 && card.diffs().is_some_and(|d| !d.same()) {
            let tip = tr("cmd.copy_all_left");
            if widgets::icon_button_enabled(&mut bui, self.writable(0), Icon::CopyAllLeft, tip)
                .clicked()
            {
                act = Some(Act::CopyFile(f, 1, 0));
            }
            let tip = tr("cmd.copy_all_right");
            if widgets::icon_button_enabled(&mut bui, self.writable(1), Icon::CopyAllRight, tip)
                .clicked()
            {
                act = Some(Act::CopyFile(f, 0, 1));
            }
        }
        let text_right = bui.min_rect().left() - 8.0;

        let mut summary = LayoutJob::default();
        let small = FontId::proportional(12.0);
        match &card.body {
            Body::Loading => summary.append(
                tr("msg.comparing"),
                0.0,
                TextFormat::simple(small.clone(), p.text_dim),
            ),
            Body::Note(_) => {}
            Body::Diffs(d) => {
                let count = trf("report.text_summary", &[("n", d.hunks.len().to_string())]);
                summary.append(&count, 0.0, TextFormat::simple(small.clone(), p.text_dim));
                if n == 2 {
                    let st = d.result.stats();
                    for (v, sign, color) in [
                        (st.added, "+", p.success),
                        (st.removed, "−", p.danger),
                        (st.changed, "~", p.warning),
                    ] {
                        if v > 0 {
                            summary.append(
                                &format!("{sign}{v}"),
                                8.0,
                                TextFormat::simple(theme::semibold(12.0), color),
                            );
                        }
                    }
                }
            }
        }
        let sg = ui.fonts(|fonts| fonts.layout_job(summary));
        let sx = text_right - sg.size().x;
        let painter = ui.painter();
        painter.galley(pos2(sx, c.y - sg.size().y / 2.0), sg, p.text);

        let mut name = LayoutJob::default();
        if card.modified() {
            name.append(
                "● ",
                0.0,
                TextFormat::simple(theme::semibold(13.5), p.accent),
            );
        }
        if !card.item.rel_dir.is_empty() {
            name.append(
                &format!("{}/", card.item.rel_dir),
                0.0,
                TextFormat::simple(FontId::proportional(13.5), p.text_dim),
            );
        }
        name.append(
            &card.item.name,
            0.0,
            TextFormat::simple(theme::semibold(13.5), p.text),
        );
        let left = rect.left() + 50.0;
        name.wrap = one_line((sx - 12.0 - left).max(20.0));
        let g = ui.fonts(|fonts| fonts.layout_job(name));
        painter.galley(pos2(left, c.y - g.size().y / 2.0), g, p.text);

        if resp
            .on_hover_text(card.item.rel_path())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
            && act.is_none()
        {
            act = Some(Act::Toggle(f));
        }
        act
    }

    fn hunk_header(&self, ui: &mut Ui, rect: Rect, f: usize, h: usize) -> Option<Act> {
        let p = pal(ui);
        let d = self.cards[f].diffs()?;
        let &(b, _) = d.hunks.get(h)?;
        let n = self.n();
        let current = self.current == Some((f, h));
        let resp = ui.interact(rect, self.id.with(("hunk", f, h)), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, if current { p.accent_soft } else { p.surface });
        let border = Stroke::new(1.0_f32, p.border);
        painter.vline(rect.left() + 0.5, rect.y_range(), border);
        painter.vline(rect.right() - 0.5, rect.y_range(), border);
        if current {
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                0.0,
                p.accent,
            );
        }
        let (_, kind) = html_report::kind(&d.result, b);
        let ranges = d.result.blocks[b]
            .ranges
            .iter()
            .take(n)
            .map(html_report::line_range)
            .collect::<Vec<_>>()
            .join("  ↔  ");
        let mut job = LayoutJob::default();
        job.append(
            &trf(
                "report.diff_n",
                &[("i", (h + 1).to_string()), ("n", d.hunks.len().to_string())],
            ),
            0.0,
            TextFormat::simple(theme::semibold(12.5), p.text),
        );
        job.append(
            kind,
            10.0,
            TextFormat::simple(FontId::proportional(12.5), p.accent),
        );
        job.append(
            &ranges,
            10.0,
            TextFormat::simple(FontId::proportional(12.0), p.text_dim),
        );
        job.wrap = one_line((rect.width() - 110.0).max(20.0));
        let g = ui.fonts(|fonts| fonts.layout_job(job));
        painter.galley(
            pos2(rect.left() + 12.0, rect.center().y - g.size().y / 2.0),
            g,
            p.text,
        );

        let buttons = Rect::from_min_max(
            pos2(rect.right() - 100.0, rect.top()),
            pos2(rect.right() - 6.0, rect.bottom()),
        );
        let mut bui = ui.new_child(
            UiBuilder::new()
                .id_salt(("hunk_buttons", f, h))
                .max_rect(buttons)
                .layout(Layout::right_to_left(Align::Center)),
        );
        bui.spacing_mut().item_spacing.x = 2.0;
        let mut act = None;
        let copies: [(usize, usize, Icon, &str); 2] = if n == 2 {
            [
                (1, 0, Icon::CopyLeft, tr("cmd.copy_left")),
                (0, 1, Icon::CopyRight, tr("cmd.copy_right")),
            ]
        } else {
            [
                (2, 1, Icon::CopyLeft, tr("cmd.copy_from_right")),
                (0, 1, Icon::CopyRight, tr("cmd.copy_from_left")),
            ]
        };
        for (from, to, icon, tip) in copies {
            if widgets::icon_button_enabled(&mut bui, self.writable(to), icon, tip).clicked() {
                act = Some(Act::Copy(f, h, from, to));
            }
        }
        if resp.clicked() && act.is_none() {
            act = Some(Act::Select(f, h));
        }
        act
    }

    fn row(&self, ui: &mut Ui, rect: Rect, f: usize, r: usize, look: &Look) -> Option<Act> {
        let p = pal(ui);
        let d = self.cards[f].diffs().filter(|d| d.loaded())?;
        let cells = self.cells.get(&(f, r))?;
        let row = d.result.rows.get(r)?;
        let n = self.n();
        let colors = look.colors;
        let longest = d.docs.iter().map(|d| d.lines.len()).max().unwrap_or(0);
        let gutter = (longest.max(1).to_string().len().max(3) as f32) * look.char_w + 14.0;
        let col_w = rect.width() / n as f32;
        let resp = ui.interact(rect, self.id.with(("row", f, r)), Sense::click());
        let hover = resp.hover_pos();
        let mut tip = None;
        let painter = ui.painter();
        for (s, (class, marks)) in cells.iter().enumerate() {
            let col = Rect::from_min_size(
                pos2(rect.left() + s as f32 * col_w, rect.top()),
                vec2(col_w, rect.height()),
            );
            let num = Rect::from_min_size(col.min, vec2(gutter, col.height()));
            let text = Rect::from_min_max(pos2(num.right(), col.top()), col.max);
            let bg = match *class {
                "ctx" => colors.text_bg,
                "ign" => colors.ignored,
                "gap" => colors.missing,
                "conf" => colors.conflict,
                _ => colors.diff,
            };
            painter.rect_filled(num, 0.0, colors.gutter_bg);
            painter.rect_filled(text, 0.0, bg);
            if s > 0 {
                painter.vline(col.left(), col.y_range(), Stroke::new(1.0_f32, p.border));
            }
            let Some(line) = row.line[s].and_then(|l| Some((l, d.docs[s].lines.get(l)?))) else {
                continue;
            };
            painter.text(
                pos2(num.right() - 6.0, num.center().y),
                Align2::RIGHT_CENTER,
                (line.0 + 1).to_string(),
                look.mono.clone(),
                colors.gutter_text,
            );
            let job = line_job(
                line.1,
                marks,
                look.mono,
                (colors.text, colors.inline),
                text.width() - 10.0,
                look.tab,
            );
            let g = ui.fonts(|fonts| fonts.layout_job(job));
            if g.elided && hover.is_some_and(|h| text.contains(h)) {
                tip = Some(line.1);
            }
            painter.with_clip_rect(text).galley(
                pos2(text.left() + 6.0, text.center().y - g.size().y / 2.0),
                g,
                colors.text,
            );
        }
        let border = Stroke::new(1.0_f32, p.border);
        painter.vline(rect.left() + 0.5, rect.y_range(), border);
        painter.vline(rect.right() - 0.5, rect.y_range(), border);
        let h = d.hunks.iter().position(|(_, rows)| rows.contains(&r))?;
        if self.current == Some((f, h)) && row.block == Some(d.hunks[h].0) {
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                0.0,
                p.accent,
            );
        }
        let resp = match tip {
            Some(t) => resp.on_hover_text(tooltip(t)),
            None => resp,
        };
        resp.clicked().then_some(Act::Select(f, h))
    }

    fn note(&self, ui: &mut Ui, rect: Rect, f: usize) {
        let p = pal(ui);
        let text = match &self.cards[f].body {
            Body::Loading => tr("msg.comparing").to_string(),
            Body::Note(note) => note.clone(),
            Body::Diffs(d) => tr(d.quiet.unwrap_or("report.no_diffs")).to_string(),
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            },
            p.surface,
            Stroke::new(1.0_f32, p.border),
            egui::StrokeKind::Inside,
        );
        painter.text(
            pos2(rect.left() + 16.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(13.0),
            p.text_dim,
        );
    }
}

/// What was clicked in the list of cards.
enum Act {
    Toggle(usize),
    Select(usize, usize),
    /// Copy difference `h` of file `f` from one side to another.
    Copy(usize, usize, usize, usize),
    CopyFile(usize, usize, usize),
    Open(usize),
}

/// Color of a file's marks: the same, missing on a side, or different.
fn card_color(p: &theme::Palette, card: &Card, n: usize) -> Color32 {
    let missing = match card.diffs() {
        Some(d) if d.same() => return p.success,
        Some(d) => d.absent.iter().take(n).any(|&a| a),
        None => !card.item.exists[..n].iter().all(|&e| e),
    };
    if missing {
        p.accent
    } else {
        p.warning
    }
}

fn one_line(width: f32) -> TextWrapping {
    TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    }
}

/// A long line shortened for a tooltip.
fn tooltip(line: &str) -> String {
    match line.char_indices().nth(TIP_CHARS) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// Appends `seg` with its tabs turned into spaces up to the next multiple of `tab` columns,
/// counting from `col`.
fn append_expanded(job: &mut LayoutJob, seg: &str, col: &mut usize, tab: usize, f: TextFormat) {
    let mut out = String::with_capacity(seg.len());
    for ch in seg.chars() {
        if ch == '\t' {
            let k = tab - *col % tab;
            out.extend(std::iter::repeat_n(' ', k));
            *col += k;
        } else {
            out.push(ch);
            *col += 1;
        }
    }
    job.append(&out, 0.0, f);
}

/// A line of text on one row, cut with "…" at `width`, with the `marks` (byte ranges) that
/// differ from the other side highlighted; `colors` are of the text and of the marks.
fn line_job(
    line: &str,
    marks: &[Range<usize>],
    font: &FontId,
    colors: (Color32, Color32),
    width: f32,
    tab: usize,
) -> LayoutJob {
    let mut cut = line.len().min(SHOWN_CHARS * 4);
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let text = &line[..cut];
    let plain = TextFormat::simple(font.clone(), colors.0);
    let marked = TextFormat {
        background: colors.1,
        ..plain.clone()
    };
    let mut job = LayoutJob::default();
    let (mut pos, mut col) = (0, 0);
    for m in marks {
        let (start, end) = (m.start.max(pos).min(cut), m.end.min(cut));
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if start > pos {
            append_expanded(&mut job, &text[pos..start], &mut col, tab, plain.clone());
        }
        append_expanded(&mut job, &text[start..end], &mut col, tab, marked.clone());
        pos = end;
    }
    if pos < text.len() {
        append_expanded(&mut job, &text[pos..], &mut col, tab, plain);
    }
    job.wrap = one_line(width);
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, exists: [bool; 3], status: CmpStatus) -> DirItem {
        DirItem {
            rel_dir: String::new(),
            name: name.into(),
            is_dir: false,
            exists,
            size: [None; 3],
            mtime: [None; 3],
            status,
            diff_side: None,
            binary: false,
            error: None,
            depth: 0,
            parent: None,
            children: Vec::new(),
        }
    }

    /// A view of `files` (name, then the text of each side, `None` = missing) under `n` roots.
    fn view(
        dir: &Path,
        files: &[(&str, &[Option<&str>])],
        read_only: Vec<bool>,
    ) -> (DiffsView, Vec<PathBuf>) {
        let n = read_only.len();
        let roots: Vec<PathBuf> = (0..n).map(|s| dir.join(format!("s{s}"))).collect();
        let mut items = Vec::new();
        for (name, sides) in files {
            let mut exists = [false; 3];
            for (s, text) in sides.iter().enumerate() {
                std::fs::create_dir_all(&roots[s]).unwrap();
                if let Some(t) = text {
                    std::fs::write(roots[s].join(name), t).unwrap();
                    exists[s] = true;
                }
            }
            let status = if exists[..n].iter().all(|&e| e) {
                CmpStatus::Different
            } else {
                CmpStatus::Unique
            };
            items.push(item(name, exists, status));
        }
        let src = Source {
            roots: roots.clone(),
            display_roots: roots.iter().map(|r| r.display().to_string()).collect(),
            read_only,
            dopts: DiffOptions::default(),
            rules: String::new(),
            title: "test".into(),
            items,
        };
        let mut v = DiffsView::new(src, 1);
        wait(&mut v);
        (v, roots)
    }

    fn wait(v: &mut DiffsView) {
        while v.loading.is_some() {
            v.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        v.rebuild(18.0);
    }

    fn read(p: PathBuf) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn copies_a_difference_undoes_it_and_saves_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, roots) = view(
            dir.path(),
            &[
                (
                    "a.txt",
                    &[Some("one\ntwo\nthree\n"), Some("one\nTWO\nthree\n")],
                ),
                ("new.txt", &[Some("fresh\n"), None]),
            ],
            vec![false, false],
        );
        assert_eq!(v.all_hunks(), vec![(0, 0), (1, 0)]);
        assert!(v.lines.contains(&Line::Hunk(1, 0)));

        v.step(true);
        assert_eq!(v.current, Some((0, 0)));
        v.copy_current(true);
        assert!(v.is_modified());
        assert_eq!(v.cards[0].hunk_count(), 0);
        assert_eq!(v.undo[0].old, ["TWO"]);
        v.undo();
        assert!(!v.is_modified());
        assert_eq!(v.cards[0].hunk_count(), 1);

        v.expand_all(false);
        v.rebuild(18.0);
        assert_eq!(v.lines.len(), 4);
        v.step(true);
        v.step(true);
        assert_eq!(v.current, Some((1, 0)));
        assert!(v.cards[1].open);
        v.copy_current(true);
        v.save_modified().unwrap();
        assert!(v.saved && !v.is_modified());
        assert_eq!(read(roots[1].join("new.txt")), "fresh\n");
        assert_eq!(read(roots[1].join("a.txt")), "one\nTWO\nthree\n");
        assert_eq!(
            card_color(&theme::palette(true), &v.cards[1], 2),
            theme::palette(true).success
        );

        // Undoing the copy that made the file, then saving, removes it again.
        v.undo();
        v.save_modified().unwrap();
        assert!(!roots[1].join("new.txt").exists());
        assert!(!v.cards[1].item.exists[1]);
    }

    #[test]
    fn copying_from_a_missing_side_deletes_the_file_when_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, roots) = view(
            dir.path(),
            &[("old.txt", &[Some("a\nb\n"), None])],
            vec![false, false],
        );
        v.copy_file(0, 1, 0);
        assert!(v.cards[0].diffs().unwrap().same());
        v.save_modified().unwrap();
        assert!(!roots[0].join("old.txt").exists());
    }

    #[test]
    fn refuses_to_save_over_a_file_changed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, roots) = view(
            dir.path(),
            &[("a.txt", &[Some("x\n"), Some("y\n")])],
            vec![false, false],
        );
        v.first();
        v.copy_current(true);
        std::fs::write(roots[1].join("a.txt"), "edited elsewhere, longer\n").unwrap();
        assert!(v.save_modified().is_err());
        assert_eq!(read(roots[1].join("a.txt")), "edited elsewhere, longer\n");
        assert!(v.is_modified());

        // Without changes here, a file changed on disk is simply read again.
        v.undo();
        assert!(!v.is_modified());
        v.frame = 5;
        v.cards[0].seen = 5;
        v.disk_checked = Instant::now() - DISK_CHECK;
        v.check_disk();
        let d = v.cards[0].diffs().unwrap();
        assert_eq!(d.docs[1].lines, ["edited elsewhere, longer"]);
    }

    #[test]
    fn three_sides_copy_into_the_middle_and_respect_read_only_sides() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, roots) = view(
            dir.path(),
            &[("m.txt", &[Some("L\n"), Some("base\n"), Some("base\n")])],
            vec![false, true, false],
        );
        v.first();
        v.copy_current(true);
        assert!(!v.is_modified());
        assert_eq!(v.status, tr("msg.readonly"));
        v.read_only = vec![false, false, false];
        v.copy_current(true);
        assert_eq!(v.cards[0].diffs().unwrap().docs[1].lines, ["L"]);
        v.save_modified().unwrap();
        assert_eq!(read(roots[1].join("m.txt")), "L\n");
    }

    #[test]
    fn line_endings_only_are_told_apart_and_can_be_copied() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, roots) = view(
            dir.path(),
            &[("e.txt", &[Some("a\r\nb\r\n"), Some("a\nb\n")])],
            vec![false, false],
        );
        let d = v.cards[0].diffs().unwrap();
        assert_eq!(d.quiet, Some("diffs.format_only"));
        assert!(v.cards[0].differs());
        v.copy_file(0, 0, 1);
        assert!(v.cards[0].diffs().unwrap().same());
        v.save_modified().unwrap();
        assert_eq!(read(roots[1].join("e.txt")), "a\r\nb\r\n");
    }

    #[test]
    fn moving_between_differences_skips_files_filtered_out() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, _) = view(
            dir.path(),
            &[
                ("a.txt", &[Some("1\n"), Some("2\n")]),
                ("b.txt", &[Some("1\n"), Some("2\n")]),
                ("c.txt", &[Some("1\n"), Some("2\n")]),
            ],
            vec![false, false],
        );
        v.search = "B.TXT".into();
        v.stale = true;
        v.rebuild(18.0);
        v.step(true);
        assert_eq!(v.current, Some((1, 0)));
        v.step(true);
        assert_eq!(v.current, Some((1, 0)));
        assert_eq!(v.status, tr("diffs.no_more"));
    }

    #[test]
    fn text_dropped_to_save_memory_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut v, _) = view(
            dir.path(),
            &[("a.txt", &[Some("1\n"), Some("2\n")])],
            vec![false, false],
        );
        if let Body::Diffs(d) = &mut v.cards[0].body {
            d.docs.iter_mut().for_each(|doc| doc.lines.clear());
            d.size = 0;
        }
        assert_eq!(v.ensure_loaded(0), Some(true));
        assert_eq!(v.cards[0].diffs().unwrap().docs[0].lines, ["1"]);
    }

    #[test]
    fn marks_cut_with_the_line_and_tabs_expand() {
        let font = FontId::monospace(12.0);
        let colors = (Color32::WHITE, Color32::RED);
        let line = "é".repeat(SHOWN_CHARS * 3);
        let job = line_job(&line, &[0..2, 10..line.len()], &font, colors, 100.0, 4);
        assert_eq!(job.sections.len(), 3);
        assert!(job.text.len() <= SHOWN_CHARS * 4);

        let mark = 2..3;
        let job = line_job(
            "a\tb\tc",
            std::slice::from_ref(&mark),
            &font,
            colors,
            100.0,
            4,
        );
        assert_eq!(job.text, "a   b   c");
        assert_eq!(&job.text[job.sections[1].byte_range.clone()], "b");
        assert!(tooltip(&"x".repeat(TIP_CHARS * 2)).chars().count() == TIP_CHARS + 1);
    }
}
