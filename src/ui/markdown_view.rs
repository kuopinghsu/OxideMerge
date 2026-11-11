//! Formatted web page compare: the pages as Markdown, shown block by block side by side, with
//! the blocks that differ highlighted.

use crate::core::diff::DiffOptions;
use crate::core::web_markdown::{self, Block, Cell, Marks, Row, Slot};
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::theme::Colors;
use crate::ui::widgets::{self, Icon, Mark, Outcome};
use egui::{
    pos2, vec2, Align, Color32, Id, Layout, Rect, Shape, Stroke, StrokeKind, Ui, UiBuilder,
};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::ops::Range;

/// Room between the pages.
const GAP: f32 = 12.0;
/// Room between the blocks of a page.
const ROW_GAP: f32 = 4.0;
/// Room around a block, inside its highlight, across and down.
const PAD: [f32; 2] = [12.0, 7.0];
/// Rounding of a block's highlight.
const RADIUS: f32 = 6.0;
/// Width of the stripe down the edge of a block that differs, which says so without color.
const STRIPE: f32 = 3.0;
/// Room left and right of the pages.
const MARGIN: f32 = 8.0;
/// Room between the lines hatched over the place of a block a page lacks.
const HATCH: f32 = 8.0;
/// Height of the strip naming the pages.
const HEADER: f32 = 28.0;
/// Indentation of a block per list it is nested in.
const INDENT: f32 = 18.0;
/// Height of a block not shown yet.
const ESTIMATE: f32 = 28.0;
/// How far beyond the visible area blocks are laid out, so scrolling finds them measured.
const OVERSCAN: f32 = 300.0;
/// Room between a table cell's lines and its text, across and down.
const CELL_PAD: [f32; 2] = [10.0, 6.0];
/// Narrowest a table column is made to fit the page.
const MIN_COLUMN: f32 = 36.0;
/// Width of the scroll bar right of the pages, shown while the location pane is not.
const BAR: f32 = 12.0;

/// A scroll under way to a row, eased rather than jumped so the eye can follow it.
#[derive(Clone, Copy)]
struct Glide {
    row: usize,
    /// Where the pages were scrolled to when it started.
    from: f32,
    /// When it started, in the window's time.
    start: f64,
}

pub struct MarkdownView {
    pages: Vec<Vec<Block>>,
    labels: Vec<String>,
    rows: Vec<Row>,
    /// What differs within each block of each row, page by page.
    marks: Vec<Vec<Option<Marks>>>,
    /// The rows that differ, each a difference of its own even next to another: a block is a
    /// whole paragraph, list item or table.
    diffs: Vec<Range<usize>>,
    cur: Option<usize>,
    /// The row compared side by side: outlined, and where stepping through differences starts.
    cursor: Option<usize>,
    /// The page last clicked, whose outline is drawn strongest.
    active: usize,
    /// Height of each row as last laid out. `pub(crate)` so that a window test can tell
    /// whether a row has been measured yet.
    pub(crate) heights: Vec<f32>,
    /// A height for each row worked out from its content alone. Rows are measured only once
    /// they are drawn, so `heights` grows as the pages are scrolled and everything below a row
    /// moves down with it. The overview and the scroll bar are drawn in these heights instead,
    /// so a difference mark stays where it is however far the pages have been scrolled.
    stable: Vec<f32>,
    /// The column width and line height `stable` was worked out for.
    stable_for: (f32, f32),
    /// The tables laid out so far, by row and page.
    tables: std::collections::HashMap<(usize, usize), TableLayout>,
    /// How far the pages are scrolled down, in pixels.
    scroll: f32,
    /// How much taller the rows above the view were found while drawn, added to `scroll` so
    /// that what is in view stays put.
    shift: f32,
    scroll_to: Option<usize>,
    glide: Option<Glide>,
    /// How far from the middle of its box the location pane is held while dragged.
    grab: Option<f32>,
    /// Whether [`Self::nav`] was drawn this frame in a toolbar of the view around this one.
    nav_drawn: bool,
    /// Whether the view around this one steps through the differences with buttons of its own.
    hosted: bool,
    cache: CommonMarkCache,
    /// URLs used to open fragment links in the page belonging to the clicked pane.
    page_urls: Vec<url::Url>,
    /// Set when stepping ran past the last (`true`) or first (`false`) difference.
    pub step_file: Option<bool>,
}

/// The pages lined up and compared for the view, worked out away from the window: for large
/// pages this takes a while.
pub struct Prepared {
    pages: Vec<Vec<Block>>,
    rows: Vec<Row>,
    marks: Vec<Vec<Option<Marks>>>,
    diffs: Vec<Range<usize>>,
}

impl Prepared {
    /// `opts` says whether and how changed words are found, as in the text compare.
    pub fn new(pages: Vec<Vec<Block>>, opts: &DiffOptions) -> Self {
        let rows = web_markdown::align(&pages);
        let mut marks = web_markdown::marks(&pages, &rows, opts);
        for (row, marks) in rows.iter().zip(&mut marks) {
            for (p, m) in marks.iter_mut().enumerate() {
                match (m, row.cells[p]) {
                    (Some(Marks::Text(r)), Some(b)) => *r = viewer_ranges(&pages[p][b].text, r),
                    (Some(Marks::Table(t)), _) => prepare_table(t),
                    _ => {}
                }
            }
        }
        let diffs = (0..rows.len())
            .filter(|&i| rows[i].changed)
            .map(|i| i..i + 1)
            .collect();
        Prepared {
            pages,
            rows,
            marks,
            diffs,
        }
    }
}

impl MarkdownView {
    #[cfg(test)]
    pub fn new(pages: Vec<Vec<Block>>, labels: Vec<String>, opts: &DiffOptions) -> Self {
        Self::from_prepared(Prepared::new(pages, opts), labels)
    }

    pub fn from_prepared(prepared: Prepared, labels: Vec<String>) -> Self {
        let Prepared {
            pages,
            rows,
            marks,
            diffs,
        } = prepared;
        MarkdownView {
            heights: vec![ESTIMATE; rows.len()],
            stable: Vec::new(),
            stable_for: (0.0, 0.0),
            tables: Default::default(),
            shift: 0.0,
            pages,
            labels,
            rows,
            marks,
            diffs,
            cur: None,
            cursor: None,
            active: 0,
            scroll: 0.0,
            scroll_to: None,
            glide: None,
            grab: None,
            nav_drawn: false,
            hosted: false,
            cache: CommonMarkCache::default(),
            page_urls: Vec::new(),
            step_file: None,
        }
    }

    pub fn with_page_urls(mut self, urls: Vec<url::Url>) -> Self {
        self.page_urls = urls;
        self
    }

    /// For a view whose own toolbar steps through the differences and shows
    /// [`Self::position`]: none is drawn here.
    pub fn hosted(mut self) -> Self {
        self.hosted = true;
        self
    }

    /// Which difference is selected, and how many there are.
    pub fn position(&self) -> (Option<usize>, usize) {
        (self.cur, self.diffs.len())
    }

    pub fn outcome(&self) -> Outcome {
        if self.diffs.is_empty() {
            Outcome::Identical
        } else {
            Outcome::Different
        }
    }

    pub fn status_line(&self) -> String {
        let total = self.diffs.len().to_string();
        let mut s = if self.diffs.is_empty() {
            tr("web.md_identical").to_string()
        } else {
            trf("msg.diff_count", &[("total", total.clone())])
        };
        if let Some(c) = self.cur {
            s.push_str("  |  ");
            s.push_str(&trf(
                "msg.diff_of",
                &[("n", (c + 1).to_string()), ("total", total)],
            ));
        }
        s
    }

    fn go(&mut self, d: usize) {
        self.cur = Some(d);
        self.cursor = Some(self.diffs[d].start);
        self.scroll_to = Some(self.diffs[d].start);
    }

    /// Puts the cursor on `row`, selecting the difference it is in, if any.
    fn click(&mut self, row: usize, page: usize) {
        self.cursor = Some(row);
        self.active = page;
        self.cur = self.diffs.iter().position(|d| d.contains(&row));
    }

    /// Steps to the first difference after the cursor row.
    pub fn next_diff(&mut self) -> bool {
        let next = match self.cursor {
            Some(r) => self.diffs.iter().position(|d| d.start > r),
            None => (!self.diffs.is_empty()).then_some(0),
        };
        next.map(|d| self.go(d)).is_some()
    }

    /// Steps to the last difference before the cursor row, or the last one without a cursor.
    pub fn prev_diff(&mut self) -> bool {
        let prev = match self.cursor {
            Some(r) => self.diffs.iter().rposition(|d| d.start < r),
            None => self.diffs.len().checked_sub(1),
        };
        prev.map(|d| self.go(d)).is_some()
    }

    pub fn first_diff(&mut self) {
        if !self.diffs.is_empty() {
            self.go(0);
        }
    }

    pub fn last_diff(&mut self) {
        if let Some(last) = self.diffs.len().checked_sub(1) {
            self.go(last);
        }
    }

    /// Left edges of the pages' columns in a width starting at `left`, and their width.
    fn columns(&self, left: f32, width: f32) -> (Vec<f32>, f32) {
        let n = self.pages.len().max(1);
        let col = ((width - GAP * (n - 1) as f32) / n as f32).max(40.0);
        ((0..n).map(|p| left + p as f32 * (col + GAP)).collect(), col)
    }

    /// Where row `r` starts below the first.
    fn top(&self, r: usize) -> f32 {
        self.heights[..r].iter().map(|h| h + ROW_GAP).sum()
    }

    /// Works out a height for every row from its content, for a column `col` wide, unless that
    /// has been done for this width already. Nothing here lays text out: the estimate has only
    /// to be steady and roughly in proportion, since what it places is the overview.
    fn update_stable(&mut self, ui: &Ui, col: f32) {
        let line = ui.text_style_height(&egui::TextStyle::Body);
        if self.stable.len() == self.rows.len() && self.stable_for == (col, line) {
            return;
        }
        let body = egui::TextStyle::Body.resolve(ui.style());
        let char_w = ui.ctx().fonts_mut(|f| f.glyph_width(&body, 'n')).max(1.0);
        self.stable = (0..self.rows.len())
            .map(|i| {
                let mut tall: f32 = 2.0 * PAD[1] + 4.0;
                for p in 0..self.pages.len() {
                    let Some(b) = self.rows[i].cells.get(p).copied().flatten() else {
                        continue;
                    };
                    let block = &self.pages[p][b];
                    let indent = INDENT * block.depth as f32;
                    let width = (col - 2.0 * PAD[0] - indent).max(20.0);
                    let inner = match self
                        .marks
                        .get(i)
                        .and_then(|m| m.get(p))
                        .and_then(Option::as_ref)
                    {
                        Some(Marks::Table(t)) => t.len() as f32 * (line + 2.0 * CELL_PAD[1]),
                        _ => {
                            let per = (width / char_w).max(1.0);
                            let lines: f32 = block
                                .text
                                .lines()
                                .map(|l| (l.chars().count() as f32 / per).ceil().max(1.0))
                                .sum();
                            lines.max(1.0) * line
                        }
                    };
                    tall = tall.max(inner + 2.0 * PAD[1]);
                }
                tall
            })
            .collect();
        self.stable_for = (col, line);
    }

    /// Where row `r` starts below the first, in stable heights.
    fn stable_top(&self, r: usize) -> f32 {
        self.stable[..r.min(self.stable.len())]
            .iter()
            .map(|h| h + ROW_GAP)
            .sum()
    }

    fn stable_total(&self) -> f32 {
        self.stable_top(self.stable.len())
    }

    /// The same place in the pages, measured in `to` heights rather than `from` heights. The
    /// overview is drawn in stable heights while the pages are scrolled in measured ones, so
    /// the viewport box and a click in the overview are carried from one to the other.
    fn convert(&self, at: f32, from: &[f32], to: &[f32]) -> f32 {
        if from.len() != to.len() {
            return at;
        }
        let (mut a, mut b) = (0.0_f32, 0.0_f32);
        for (i, h) in from.iter().enumerate() {
            let step = h + ROW_GAP;
            if at < a + step {
                let part = if step > 0.0 { (at - a) / step } else { 0.0 };
                return b + part.clamp(0.0, 1.0) * (to[i] + ROW_GAP);
            }
            a += step;
            b += to[i] + ROW_GAP;
        }
        b + (at - a)
    }

    /// The overview of the differences in the location pane, in stable heights down the pages.
    /// `pub(crate)` so that a window test can check it does not move while the pages scroll.
    pub(crate) fn location_marks(&self, c: &Colors) -> Vec<Mark> {
        let heights = if self.stable.len() == self.rows.len() {
            &self.stable
        } else {
            &self.heights
        };
        let mut y = 0.0;
        let tops: Vec<f32> = heights
            .iter()
            .map(|h| {
                let top = y;
                y += h + ROW_GAP;
                top
            })
            .collect();
        let mut out = Vec::new();
        for (d, run) in self.diffs.iter().enumerate() {
            let start = tops[run.start];
            let end = tops[run.end - 1] + heights[run.end - 1];
            let color = if self.cur == Some(d) {
                c.ui.accent
            } else {
                c.change_bar
            };
            // The pages marked from the top of the difference to its end.
            let mut whole = [None; 3];
            // The rows of a page's table that differ, marked where they are: a sheet is one
            // block, and marked whole it would say no more than that something in it differs.
            let mut within = Vec::new();
            for (p, column) in whole.iter_mut().enumerate().take(self.pages.len()) {
                let present = self.rows[run.clone()]
                    .iter()
                    .any(|r| r.cells.get(p).copied().flatten().is_some());
                let table = match self.marks.get(run.start).and_then(|m| m.get(p)) {
                    Some(Some(Marks::Table(t))) if present && run.len() == 1 && !t.is_empty() => t,
                    _ => {
                        *column = Some(if present { color } else { c.ui.border_strong });
                        continue;
                    }
                };
                if !table.iter().any(|row| row_differs(row)) {
                    // Nothing says where: changed words are not looked for, or this page's
                    // table only lacks what another has.
                    *column = Some(color);
                    continue;
                }
                let each = (end - start - 2.0 * PAD[1]).max(1.0) / table.len() as f32;
                let mut r = 0;
                while r < table.len() {
                    let from = r;
                    while r < table.len() && row_differs(&table[r]) {
                        r += 1;
                    }
                    if r == from {
                        r += 1;
                        continue;
                    }
                    let mut columns = [None; 3];
                    columns[p] = Some(color);
                    within.push(Mark {
                        start: (start + PAD[1] + from as f32 * each) as usize,
                        len: ((r - from) as f32 * each).max(1.0) as usize,
                        columns,
                    });
                }
            }
            if whole.iter().any(Option::is_some) {
                out.push(Mark {
                    start: start as usize,
                    len: (end - start).max(1.0) as usize,
                    columns: whole,
                });
            }
            out.extend(within);
        }
        out
    }

    /// The buttons that step through the differences, and where among them the view is. Drawn
    /// in the toolbar of the view around this one, which saves a row; [`Self::ui`] draws them in
    /// a toolbar of its own otherwise.
    pub fn nav(&mut self, ui: &mut Ui) {
        self.nav_drawn = true;
        let any = !self.diffs.is_empty();
        if widgets::icon_button_enabled(ui, any, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
            self.first_diff();
        }
        if widgets::icon_button_hint(ui, Icon::Prev, &Cmd::PrevDiff.tip(), tr("tip.nav_diff"))
            .clicked()
            && !self.prev_diff()
        {
            self.step_file = Some(false);
        }
        if widgets::icon_button_hint(ui, Icon::Next, &Cmd::NextDiff.tip(), tr("tip.nav_diff"))
            .clicked()
            && !self.next_diff()
        {
            self.step_file = Some(true);
        }
        if widgets::icon_button_enabled(ui, any, Icon::Last, &Cmd::LastDiff.tip()).clicked() {
            self.last_diff();
        }
        ui.add_space(6.0);
        let pal = widgets::pal(ui);
        let total = self.diffs.len();
        let (text, fg, bg) = match self.cur {
            _ if total == 0 => (
                tr("web.md_identical").to_string(),
                pal.text_dim,
                pal.surface,
            ),
            Some(d) => (format!("{} / {total}", d + 1), pal.accent, pal.accent_soft),
            None => (
                trf("msg.diff_count", &[("total", total.to_string())]),
                pal.text_dim,
                pal.surface,
            ),
        };
        widgets::chip(ui, &text, fg, bg).on_hover_text(self.status_line());
    }

    /// `location` shows the location pane, as it does in the text compare.
    pub fn ui(&mut self, ui: &mut Ui, c: &Colors, location: bool) {
        if !std::mem::take(&mut self.nav_drawn) && !self.hosted {
            widgets::toolbar(ui, |ui| self.nav(ui));
            self.nav_drawn = false;
        }
        let pal = widgets::pal(ui);
        let n = self.pages.len().clamp(1, 3);
        // The location pane is the scroll bar too: its box is the part in view, and is dragged.
        let side = if location { 8.0 + 7.0 * n as f32 } else { BAR };
        let header = ui.allocate_space(vec2(ui.available_width(), HEADER)).1;
        ui.painter().rect_filled(header, 0.0, pal.surface_alt);
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.border),
        );
        let (lefts, col) =
            self.columns(header.left() + MARGIN, header.width() - side - 2.0 * MARGIN);
        for (p, (left, label)) in lefts.iter().zip(&self.labels).enumerate() {
            if p > 0 {
                ui.painter().vline(
                    left - GAP / 2.0,
                    header.y_range(),
                    Stroke::new(1.0_f32, pal.border),
                );
            }
            let rect = Rect::from_min_size(
                pos2(*left + PAD[0], header.top()),
                vec2(col - 2.0 * PAD[0], HEADER),
            );
            let mut cell = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            // The page last clicked is named in full strength, as its outline is drawn.
            let color = if self.cursor.is_some() && p == self.active {
                pal.text
            } else {
                pal.text_dim
            };
            let text = egui::RichText::new(label)
                .color(color)
                .size(crate::ui::theme::size::DENSE);
            cell.add(egui::Label::new(text).truncate())
                .on_hover_text(label);
        }

        let body = ui.available_rect_before_wrap();
        ui.allocate_rect(body, egui::Sense::hover());
        let view = Rect::from_min_max(
            pos2(body.left() + MARGIN, body.top()),
            pos2(body.right() - side - MARGIN, body.bottom()),
        );
        let view_h = view.height().max(1.0);
        let total = self.top(self.heights.len());
        let max_scroll = (total - view_h).max(0.0);
        let shift = std::mem::take(&mut self.shift);
        let mut offset = self.scroll + shift;
        let now = ui.input(|i| i.time);
        if let Some(g) = &mut self.glide {
            g.from += shift;
        }
        if let Some(r) = self.scroll_to.take() {
            self.glide = reveal(self.top(r), self.heights[r], offset, view_h).map(|_| Glide {
                row: r,
                from: offset,
                start: now,
            });
        }
        let taken_over = ui.input(|i| {
            i.smooth_scroll_delta != egui::Vec2::ZERO
                || i.pointer.is_decidedly_dragging()
                || i.any_touches()
        });
        if taken_over {
            // The wheel, a drag or a finger scrolls the pages now.
            self.glide = None;
        }
        if let Some(g) = self.glide {
            // Where the row is now: rows measured on the way move it.
            let to = (self.top(g.row) - (view_h / 3.0).floor()).clamp(0.0, max_scroll);
            let time = ui.style().animation_time * 2.0;
            let t = if time > 0.0 {
                ((now - g.start) as f32 / time).clamp(0.0, 1.0)
            } else {
                1.0
            };
            offset = g.from + (to - g.from) * (1.0 - (1.0 - t).powi(3));
            if t < 1.0 {
                ui.ctx().request_repaint();
            } else {
                self.glide = None;
            }
        }
        // The overview and the scroll bar are drawn in the steady heights, so they do not
        // shift as rows are measured for the first time on their way into view.
        self.update_stable(ui, self.columns(view.left(), view.width()).1);
        let steady = self.stable_total();
        let at = |v: &Self, y: f32| v.convert(y, &v.heights, &v.stable);
        let strip = Rect::from_min_max(pos2(body.right() - side, body.top()), body.max);
        if location {
            let marks = self.location_marks(c);
            let first = at(self, offset);
            let shown = (at(self, offset + view_h) - first).max(1.0);
            let to = widgets::location_pane(
                ui,
                Id::unique("md_location"),
                strip,
                steady as usize,
                n,
                &marks,
                at(self, offset),
                shown,
                c.location_bg,
                c.viewport,
            );
            match to {
                Some(to) => {
                    // The box of the part in view is dragged by where it was taken hold of,
                    // as a scroll bar's is; a press anywhere else brings that place to the
                    // middle of the view.
                    let inside = (first..=first + shown).contains(&to);
                    let from_middle = if inside {
                        to - (first + shown / 2.0)
                    } else {
                        0.0
                    };
                    let held = *self.grab.get_or_insert(from_middle);
                    offset = self.convert(to - held, &self.stable, &self.heights) - view_h / 2.0;
                    self.glide = None;
                }
                None => self.grab = None,
            }
        } else {
            let mut dragged = at(self, offset);
            let was = dragged;
            widgets::vscrollbar(
                ui,
                Id::unique("md_vbar"),
                strip,
                &mut dragged,
                steady as usize,
                (at(self, offset + view_h) - was).max(1.0),
            );
            if (dragged - was).abs() > 0.01 {
                offset = self.convert(dragged, &self.stable, &self.heights);
                self.glide = None;
            }
        }
        let offset = offset.clamp(0.0, max_scroll);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("web_markdown")
            .auto_shrink(false)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
        if offset != self.scroll {
            area = area.vertical_scroll_offset(offset);
        }
        let sel = self.cur.map(|d| self.diffs[d].clone());
        let (cursor, active) = (self.cursor, self.active);
        let mut relaid = false;
        let mut shift = 0.0;
        let mut clicked = None;
        let mut pages_ui = ui.new_child(UiBuilder::new().max_rect(view));
        let out = area.show_viewport(&mut pages_ui, |ui, viewport| {
            let origin = ui.min_rect().min;
            let width = ui.available_width();
            let (lefts, col) = self.columns(origin.x, width);
            let Self {
                pages,
                rows,
                marks,
                heights,
                tables,
                cache,
                page_urls,
                ..
            } = self;
            let pressed = ui.input(|i| i.pointer.primary_clicked());
            let mut y = 0.0;
            for (i, row) in rows.iter().enumerate() {
                let shown =
                    y + heights[i] >= viewport.min.y - OVERSCAN && y <= viewport.max.y + OVERSCAN;
                if shown {
                    let top = origin.y + y;
                    let current = sel.as_ref().is_some_and(|s| s.contains(&i));
                    let words = if current { c.inline_sel } else { c.inline };
                    let backs: Vec<_> = lefts
                        .iter()
                        .map(|_| ui.painter().add(Shape::Noop))
                        .collect();
                    // The widest each column of the row's tables is on any page, so that the
                    // tables of a sheet or a document stand column under column side by side.
                    let shared = (0..lefts.len())
                        .filter_map(|q| tables.get(&(i, q)))
                        .map(|t| &t.natural)
                        .filter(|n| !n.is_empty())
                        .fold(Vec::new(), |mut all: Vec<f32>, n| {
                            for (k, w) in n.iter().enumerate() {
                                match all.get_mut(k) {
                                    Some(a) => *a = a.max(*w),
                                    None => all.push(*w),
                                }
                            }
                            all
                        });
                    let is_table = |p: usize| {
                        matches!(
                            marks.get(i).and_then(|m| m.get(p)).and_then(Option::as_ref),
                            Some(Marks::Table(_))
                        )
                    };
                    let mut height: f32 = 0.0;
                    // How much taller the rows of its tables above the view were found.
                    let mut grew: f32 = 0.0;
                    for (p, left) in lefts.iter().enumerate() {
                        let Some(b) = row.cells.get(p).copied().flatten() else {
                            continue;
                        };
                        let block = &pages[p][b];
                        let indent = INDENT * block.depth as f32;
                        let inner = Rect::from_min_size(
                            pos2(left + PAD[0] + indent, top + PAD[1]),
                            vec2((col - 2.0 * PAD[0] - indent).max(20.0), f32::INFINITY),
                        );
                        let mut cell = ui.new_child(
                            UiBuilder::new()
                                .max_rect(inner)
                                .layout(Layout::top_down(Align::Min))
                                .id_salt(("block", i, p)),
                        );
                        let column =
                            Rect::from_x_y_ranges(*left..=left + col, ui.clip_rect().y_range());
                        cell.set_clip_rect(column.intersect(ui.clip_rect()));
                        cell.set_width(inner.width());
                        let id = Id::unique(("md_block", i, p));
                        let page = page_urls.get(p);
                        let commands = page.map(|_| cell.ctx().output(|out| out.commands.len()));
                        match marks.get(i).and_then(|m| m.get(p)).and_then(Option::as_ref) {
                            Some(Marks::Table(t)) => {
                                let layout = tables.entry((i, p)).or_default();
                                let (r, g) = table(&mut cell, cache, layout, id, t, words, &shared);
                                relaid |= r;
                                grew = grew.max(g);
                            }
                            Some(Marks::Text(r)) => {
                                show_marked(&mut cell, cache, id, &block.text, r, words);
                            }
                            None => show_marked(&mut cell, cache, id, &block.text, &[], words),
                        }
                        if let (Some(page), Some(start)) = (page, commands) {
                            cell.ctx().output_mut(|out| {
                                resolve_fragment_links(page, &mut out.commands[start..]);
                            });
                        }
                        height = height.max(cell.min_rect().height() + 2.0 * PAD[1]);
                    }
                    let height = height.max(2.0 * PAD[1] + 4.0);
                    if y + heights[i] <= viewport.min.y {
                        shift += height - heights[i];
                    } else if y < viewport.min.y {
                        shift += grew;
                    }
                    if (height - heights[i]).abs() > 0.5 {
                        heights[i] = height;
                        relaid = true;
                    }
                    let all = row.cells.iter().all(Option::is_some);
                    for (p, left) in lefts.iter().enumerate() {
                        let rect = Rect::from_min_size(pos2(*left, top), vec2(col, heights[i]));
                        let present = row.cells.get(p).copied().flatten().is_some();
                        let mut under = Vec::new();
                        if row.changed && !present {
                            // Nothing here: a faint hatched place rather than a solid block.
                            let fill = if current { c.missing_sel } else { c.missing };
                            under.push(Shape::rect_filled(rect, RADIUS, fill));
                            let lines = Stroke::new(1.0_f32, c.ui.border);
                            hatch(&mut under, rect.shrink(2.0), lines);
                        } else if row.changed {
                            // Changed on every page; or on some only, where of two pages the
                            // left one has what the right one lost.
                            let (fill, fill_sel, bar) = if all {
                                (c.diff, c.diff_sel, c.change_bar)
                            } else if lefts.len() == 2 && p == 0 {
                                (c.removed, c.removed_sel, c.inline_removed)
                            } else {
                                (c.added, c.added_sel, c.inline_added)
                            };
                            let fill = match (current, all && is_table(p)) {
                                (true, _) => fill_sel,
                                // A changed table marks its cells: tinted whole as strongly as
                                // a paragraph, a sheet would be one slab with the marks lost.
                                (false, true) => fill.gamma_multiply(0.45),
                                (false, false) => fill,
                            };
                            under.push(Shape::rect_filled(rect, RADIUS, fill));
                            let stripe = Rect::from_min_size(
                                pos2(*left, top + 2.0),
                                vec2(STRIPE, (heights[i] - 4.0).max(0.0)),
                            );
                            under.push(Shape::rect_filled(stripe, STRIPE / 2.0, bar));
                        }
                        let hovered = ui.rect_contains_pointer(rect);
                        // Not a table, which may be a whole sheet.
                        if hovered && cursor != Some(i) && !is_table(p) {
                            let tint = ui.visuals().text_color().gamma_multiply(0.05);
                            under.push(Shape::rect_filled(rect, RADIUS, tint));
                        }
                        if !under.is_empty() {
                            ui.painter().set(backs[p], Shape::Vec(under));
                        }
                        if pressed && hovered {
                            clicked = Some((i, p));
                        }
                        // The compared row, as the text compare outlines its cursor row.
                        if cursor == Some(i) {
                            let color = if p == active {
                                c.cursor_row
                            } else {
                                c.cursor_row.gamma_multiply(0.35)
                            };
                            ui.painter().rect_stroke(
                                rect,
                                RADIUS,
                                Stroke::new(1.5_f32, color),
                                StrokeKind::Inside,
                            );
                        }
                    }
                }
                y += heights[i] + ROW_GAP;
            }
            let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
            for left in lefts.iter().skip(1) {
                let x = left - GAP / 2.0;
                ui.painter().vline(
                    x,
                    origin.y + viewport.min.y..=origin.y + viewport.max.y,
                    stroke,
                );
            }
            ui.advance_cursor_after_rect(Rect::from_min_size(origin, vec2(width, y)));
        });
        self.scroll = out.state.clamped_offset().y;
        if shift.abs() > 0.5 {
            // Drawn again at once, scrolled by as much, before this pass is shown.
            self.shift = shift;
            ui.ctx()
                .request_discard("rows above the web page view were measured");
        }
        if let Some((row, page)) = clicked {
            self.click(row, page);
        }
        if relaid {
            // Rows measured anew move the ones below: draw again with the heights settled.
            ui.ctx().request_repaint();
        }
    }
}

/// Whether any cell of a table's row differs from the other pages.
fn row_differs(row: &[(String, Cell)]) -> bool {
    row.iter().any(|(_, cell)| match cell {
        Cell::Whole => true,
        Cell::Words(marks) => !marks.is_empty(),
    })
}

/// Adds lines slanting across `rect` to `out`, each cut off at its edges.
fn hatch(out: &mut Vec<Shape>, rect: Rect, stroke: Stroke) {
    let (w, h) = (rect.width(), rect.height());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    // A line is the points `d` from the top left corner, across and down added up.
    let mut d = HATCH;
    while d < w + h {
        let a = if d <= w {
            pos2(rect.left() + d, rect.top())
        } else {
            pos2(rect.right(), rect.top() + d - w)
        };
        let b = if d <= h {
            pos2(rect.left(), rect.top() + d)
        } else {
            pos2(rect.left() + d - h, rect.bottom())
        };
        out.push(Shape::line_segment([a, b], stroke));
        d += HATCH;
    }
}

/// The viewer emits link clicks as URL commands. Resolve fragments against the clicked pane.
fn resolve_fragment_links(page: &url::Url, commands: &mut [egui::OutputCommand]) {
    for command in commands {
        if let egui::OutputCommand::OpenUrl(open) = command {
            if open.url.starts_with('#') {
                if let Ok(url) = page.join(&open.url) {
                    open.url = url.into();
                }
            }
        }
    }
}

/// Where to scroll to show a row starting `top` down the pages and `height` high, when the pages
/// are scrolled `offset` down and `view` high: as in the text compare, nowhere while its start is
/// in view, and otherwise so that it starts a third of the way down.
fn reveal(top: f32, height: f32, offset: f32, view: f32) -> Option<f32> {
    let shown = height.min(2.0 * ESTIMATE).min(view);
    (top < offset || top + shown > offset + view).then(|| (top - (view / 3.0).floor()).max(0.0))
}

/// The Markdown extensions the viewer parses with.
fn viewer_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
}

/// The byte ranges `marks` of Markdown `text` as the viewer places highlights: at their offset
/// from where the source of each piece of text starts, which for inline code is its backticks.
fn viewer_ranges(text: &str, marks: &[Range<usize>]) -> Vec<Range<usize>> {
    use pulldown_cmark::{Event, Parser};
    if marks.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (event, span) in Parser::new_ext(text, viewer_options()).into_offset_iter() {
        let (Event::Text(t) | Event::Code(t)) = event else {
            continue;
        };
        // The viewer lays highlights over `t`: text it shows otherwise than written (a character
        // reference) has no place where they would land safely.
        let Some(at) = text[span.clone()].find(&*t) else {
            continue;
        };
        let start = span.start + at;
        let end = start + t.len();
        for m in marks {
            let (s, e) = (m.start.max(start), m.end.min(end));
            if s < e && t.is_char_boundary(s - start) && t.is_char_boundary(e - start) {
                out.push(span.start + (s - start)..span.start + (e - start));
            }
        }
    }
    out
}

/// Readies a table's cells to be shown: the header rows bold, and the changed words placed as
/// the viewer places them.
fn prepare_table(rows: &mut [Vec<(String, Cell)>]) {
    let slots = web_markdown::spans(
        &rows
            .iter()
            .map(|r| r.iter().map(|c| &c.0).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
    );
    let header = header_rows(&slots);
    for (r, row) in rows.iter_mut().enumerate() {
        for (k, (text, cell)) in row.iter_mut().enumerate() {
            let covered = matches!(slots[r].get(k), Some(Slot::Covered { .. }));
            let bold = text.len() > 4 && text.starts_with("**") && text.ends_with("**");
            if r < header && !text.is_empty() && !covered && !bold {
                *text = format!("**{text}**");
                if let Cell::Words(v) = cell {
                    v.iter_mut().for_each(|m| *m = m.start + 2..m.end + 2);
                }
            }
            if let Cell::Words(v) = cell {
                *v = viewer_ranges(text, v);
            }
        }
    }
}

/// How many rows the header of a table is: the first row, and those its merged cells reach.
fn header_rows(slots: &[Vec<Slot>]) -> usize {
    slots.first().map_or(0, |first| {
        first
            .iter()
            .map(|s| match s {
                Slot::Anchor { rows, .. } => *rows,
                Slot::Covered { .. } => 1,
            })
            .max()
            .unwrap_or(1)
    })
}

/// Shows Markdown `text` with the byte ranges `marks` of it highlighted in `color`.
fn show_marked(
    ui: &mut Ui,
    cache: &mut CommonMarkCache,
    id: Id,
    text: &str,
    marks: &[Range<usize>],
    color: Color32,
) {
    let search = cache.search_cache_mut(&id);
    if search.search_ranges != marks {
        search.search_ranges = marks.to_vec();
    }
    // A line of text is as tall as its text, not as a button: the viewer draws a list's bullet
    // in a text-high place at the top of the line, which sat it above the words.
    ui.spacing_mut().interact_size.y = ui.text_style_height(&egui::TextStyle::Body);
    CommonMarkViewer::new()
        .search_match_color(color)
        .show_with_id(id, ui, cache, text);
}

/// What is kept of a table between frames, so that only its rows in view are laid out.
#[derive(Default)]
struct TableLayout {
    /// The body and code font sizes the natural widths were measured with.
    fonts: [f32; 2],
    /// How wide each column is on one line.
    natural: Vec<f32>,
    /// Height of each row as last laid out, its padding included.
    heights: Vec<f32>,
    /// Where each cell is, when cells are merged.
    slots: Vec<Vec<Slot>>,
    /// The last row each row's merged cells reach down to.
    reach: Vec<usize>,
    /// How tall the text of each cell merged over several rows was, by its row and column.
    tall: std::collections::HashMap<(usize, usize), f32>,
}

impl TableLayout {
    /// The row and column of the merged area cell `k` of row `r` belongs to.
    fn owner(&self, r: usize, k: usize) -> (usize, usize) {
        match self.slots.get(r).and_then(|s| s.get(k)) {
            Some(Slot::Covered { row, col }) => (*row, *col),
            _ => (r, k),
        }
    }
}

/// Draws a table as a grid of cells with lines around them and the header rows shaded. A
/// merged cell is drawn across the cells it covers. What differs is highlighted in `words`:
/// changed words, and whole cells the other page lacks. Only the rows in view are laid out.
/// `shared` is how wide the columns are on one line on any of the pages, as far as known: no
/// column is made narrower than that while there is room, so the tables line up across pages.
/// Returns whether a row's height changed, and how much taller the rows above the view were
/// found.
fn table(
    ui: &mut Ui,
    cache: &mut CommonMarkCache,
    layout: &mut TableLayout,
    id: Id,
    rows: &[Vec<(String, Cell)>],
    words: Color32,
    shared: &[f32],
) -> (bool, f32) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return (false, 0.0);
    }
    let [px, py] = CELL_PAD;
    if layout.slots.len() != rows.len() {
        let texts: Vec<Vec<&String>> = rows
            .iter()
            .map(|r| r.iter().map(|c| &c.0).collect())
            .collect();
        layout.slots = web_markdown::spans(&texts);
        layout.reach = layout
            .slots
            .iter()
            .enumerate()
            .map(|(r, line)| {
                line.iter()
                    .map(|s| match s {
                        Slot::Anchor { rows, .. } => r + rows - 1,
                        Slot::Covered { .. } => r,
                    })
                    .max()
                    .unwrap_or(r)
            })
            .collect();
        layout.natural.clear();
    }
    let slot = |layout: &TableLayout, r: usize, k: usize| {
        layout
            .slots
            .get(r)
            .and_then(|s| s.get(k))
            .copied()
            .unwrap_or(Slot::Anchor { cols: 1, rows: 1 })
    };
    let fonts =
        [egui::TextStyle::Body, egui::TextStyle::Monospace].map(|s| s.resolve(ui.style()).size);
    if layout.fonts != fonts || layout.natural.len() != cols {
        layout.fonts = fonts;
        layout.natural = vec![0.0_f32; cols];
        let body = egui::TextStyle::Body.resolve(ui.style());
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let mut wide = Vec::new();
        // A large table is measured from its first rows and rows spread over the rest.
        let step = (rows.len() * cols).div_ceil(MEASURED_CELLS).max(1);
        ui.ctx().fonts_mut(|fonts| {
            for (r, row) in rows.iter().enumerate() {
                if r >= FIRST_MEASURED && r % step != 0 {
                    continue;
                }
                for (k, (text, _)) in row.iter().enumerate() {
                    match slot(layout, r, k) {
                        Slot::Anchor { cols: 1, .. } => {
                            let w = natural_width(fonts, &body, &mono, text);
                            layout.natural[k] = layout.natural[k].max(w);
                        }
                        Slot::Anchor { cols: n, .. } => {
                            wide.push((k, n, natural_width(fonts, &body, &mono, text)))
                        }
                        Slot::Covered { .. } => {}
                    }
                }
            }
        });
        // A merged cell wider than the columns it covers widens them all alike.
        for (k, n, w) in wide {
            let end = (k + n).min(cols);
            let have: f32 = layout.natural[k..end].iter().sum::<f32>()
                + (end - k).saturating_sub(1) as f32 * 2.0 * px;
            if w > have {
                let more = (w - have) / (end - k) as f32;
                layout.natural[k..end].iter_mut().for_each(|c| *c += more);
            }
        }
        layout.heights.clear();
        // Drawn again with the widths the other pages' tables now share with this one.
        ui.ctx().request_repaint();
    }
    let line_height = ui.text_style_height(&egui::TextStyle::Body);
    if layout.heights.len() != rows.len() {
        layout.heights = vec![line_height + 2.0 * py; rows.len()];
    }
    let header = header_rows(&layout.slots);
    let natural: Vec<f32> = layout
        .natural
        .iter()
        .enumerate()
        .map(|(k, w)| shared.get(k).map_or(*w, |s| w.max(*s)))
        .collect();
    let widths = fit(&natural, ui.available_width() - cols as f32 * 2.0 * px);
    let origin = ui.cursor().min;
    let mut xs = vec![origin.x];
    for w in &widths {
        xs.push(xs[xs.len() - 1] + w + 2.0 * px);
    }
    let clip = ui.clip_rect();
    let shown = clip.expand2(vec2(0.0, OVERSCAN)).y_range();
    let shade = ui.visuals().text_color().gamma_multiply(0.05);
    let mut relaid = false;
    let mut grew = 0.0;
    let mut ys = vec![origin.y];
    for (r, row) in rows.iter().enumerate() {
        let top = ys[r];
        // A row is laid out while it or a cell merged down from it is in view.
        let reach = layout
            .reach
            .get(r)
            .copied()
            .unwrap_or(r)
            .min(rows.len() - 1);
        let extent: f32 = layout.heights[r..=reach].iter().sum();
        if top + extent < shown.min || top > shown.max {
            ys.push(top + layout.heights[r]);
            continue;
        }
        let backs: Vec<_> = (0..cols).map(|_| ui.painter().add(Shape::Noop)).collect();
        let mut height = line_height;
        for (k, (text, cell)) in row.iter().enumerate() {
            let Slot::Anchor {
                cols: n,
                rows: down,
            } = slot(layout, r, k)
            else {
                continue;
            };
            let end = (k + n).min(cols);
            let width = xs[end] - xs[k] - 2.0 * px;
            let rect = Rect::from_min_size(pos2(xs[k] + px, top + py), vec2(width, f32::INFINITY));
            let mut c = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::top_down(Align::Min))
                    .id_salt(("cell", r, k)),
            );
            c.set_clip_rect(Rect::from_x_y_ranges(xs[k]..=xs[end], clip.y_range()).intersect(clip));
            c.set_width(width);
            let marks = match cell {
                Cell::Words(v) => v.as_slice(),
                Cell::Whole => &[],
            };
            show_marked(&mut c, cache, id.with(("cell", r, k)), text, marks, words);
            let tall = c.min_rect().height();
            if down == 1 {
                height = height.max(tall);
            } else {
                let was = layout.tall.insert((r, k), tall);
                relaid |= was.is_none_or(|w| (w - tall).abs() > 0.5);
            }
        }
        // The last row a merged cell covers is made tall enough for the cell's text.
        for k in 0..cols {
            let (ar, ak) = layout.owner(r, k);
            if let (Slot::Anchor { rows: down, .. }, Some(&tall)) =
                (slot(layout, ar, ak), layout.tall.get(&(ar, ak)))
            {
                if down > 1 && ar + down - 1 == r && ak == k {
                    let above: f32 = layout.heights[ar..r].iter().sum();
                    height = height.max(tall - above);
                }
            }
        }
        let bottom = top + height + 2.0 * py;
        if (bottom - top - layout.heights[r]).abs() > 0.5 {
            if top < clip.min.y {
                grew += bottom - top - layout.heights[r];
            }
            layout.heights[r] = bottom - top;
            relaid = true;
        }
        for (k, back) in backs.into_iter().enumerate() {
            let changed = match row.get(k).map(|c| &c.1) {
                Some(Cell::Whole) => true,
                Some(Cell::Words(v)) => !v.is_empty(),
                None => false,
            };
            let (fill, rect) = match slot(layout, r, k) {
                Slot::Anchor {
                    cols: n,
                    rows: down,
                } => {
                    let end = (k + n).min(cols);
                    let last = (r + down).min(rows.len());
                    let below: f32 = layout.heights[r + 1..last].iter().sum();
                    let rect = Rect::from_x_y_ranges(xs[k]..=xs[end], top..=bottom + below);
                    match row.get(k) {
                        Some((_, Cell::Whole)) => (words, rect),
                        _ if r < header => (shade, rect),
                        _ => continue,
                    }
                }
                // A covered cell of a row or column the other page lacks, or one merged on
                // one page only, is highlighted where it is.
                Slot::Covered { .. } if changed => (
                    words,
                    Rect::from_x_y_ranges(xs[k]..=xs[k + 1], top..=bottom),
                ),
                Slot::Covered { .. } => continue,
            };
            ui.painter().set(back, Shape::rect_filled(rect, 0.0, fill));
        }
        ys.push(bottom);
    }
    // Rows are ruled off lightly and columns more lightly still, so the text leads.
    let text = ui.visuals().text_color();
    let stroke = Stroke::new(1.0, text.gamma_multiply(0.16));
    let between = Stroke::new(1.0, text.gamma_multiply(0.08));
    let outer = Rect::from_x_y_ranges(xs[0]..=xs[cols], origin.y..=ys[ys.len() - 1]);
    let painter = ui.painter();
    // Lines between cells, except within a merged cell.
    for r in 0..rows.len() {
        let (top, bottom) = (ys[r], ys[r + 1]);
        if bottom < shown.min || top > shown.max {
            continue;
        }
        for (k, &x) in xs.iter().enumerate().take(cols).skip(1) {
            if layout.owner(r, k - 1) != layout.owner(r, k) {
                painter.vline(x, top..=bottom, between);
            }
        }
        if r == 0 {
            continue;
        }
        // The header rows end with a stronger line.
        let line = if r == header {
            Stroke::new(1.0, text.gamma_multiply(0.34))
        } else {
            stroke
        };
        for k in 0..cols {
            if layout.owner(r - 1, k) != layout.owner(r, k) {
                painter.hline(xs[k]..=xs[k + 1], top, line);
            }
        }
    }
    painter.rect_stroke(outer, 4.0, stroke, StrokeKind::Inside);
    ui.advance_cursor_after_rect(outer);
    (relaid, grew)
}

/// Column widths for columns `natural` wide on one line, in `room`: all as wide as they need
/// when they fit; otherwise narrow columns keep their width and the wide ones share the rest.
fn fit(natural: &[f32], room: f32) -> Vec<f32> {
    let mut widths = natural.to_vec();
    if natural.iter().sum::<f32>() <= room {
        return widths;
    }
    let mut wide: Vec<usize> = (0..natural.len()).collect();
    let mut left = room;
    while !wide.is_empty() {
        let share = (left / wide.len() as f32).max(MIN_COLUMN);
        let (narrow, rest): (Vec<usize>, Vec<usize>) =
            wide.iter().partition(|&&k| natural[k] <= share);
        if narrow.is_empty() {
            for k in rest {
                widths[k] = share;
            }
            break;
        }
        left -= narrow.iter().map(|&k| natural[k]).sum::<f32>();
        wide = rest;
    }
    widths
}

/// The most cells of a table measured for the widths of its columns, and the rows at its top
/// always measured.
const MEASURED_CELLS: usize = 50_000;
const FIRST_MEASURED: usize = 50;

/// About how wide Markdown `text` is shown on one line in `body`, with its code in `mono`.
fn natural_width(
    fonts: &mut egui::epaint::FontsView<'_>,
    body: &egui::FontId,
    mono: &egui::FontId,
    text: &str,
) -> f32 {
    let mut width = 2.0;
    for (i, part) in text.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        width += if i % 2 == 1 {
            // Inline code, with room for its background.
            part.chars()
                .map(|c| fonts.glyph_width(mono, c))
                .sum::<f32>()
                + 4.0
        } else {
            visible(part)
                .chars()
                .map(|c| fonts.glyph_width(body, c))
                .sum()
        };
    }
    width
}

/// The text of Markdown without code that a reader sees: no formatting marks, escapes or link
/// addresses.
fn visible(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut chars = md.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '*' | '~' | '[' => {}
            ']' if chars.peek() == Some(&'(') => {
                for n in chars.by_ref() {
                    if n == ')' {
                        break;
                    }
                }
            }
            ']' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_columns_fit_the_room() {
        assert_eq!(fit(&[10.0, 20.0], 100.0), [10.0, 20.0]);
        // The narrow column keeps its width; the wide ones share what is left.
        assert_eq!(fit(&[10.0, 200.0, 300.0], 110.0), [10.0, 50.0, 50.0]);
        // No column gets narrower than the least.
        assert_eq!(fit(&[100.0, 100.0], 20.0), [MIN_COLUMN, MIN_COLUMN]);
    }

    #[test]
    fn formatted_fragment_clicks_open_their_own_page() {
        let pages = [
            "file:///D:/reports/standard.html",
            "file:///D:/reports/full.html",
        ]
        .map(|url| url::Url::parse(url).unwrap());
        let mut view = MarkdownView::new(
            vec![vec![block("[Jump](#entry)")]; 2],
            vec!["Standard".into(), "Full".into()],
            &DiffOptions::default(),
        )
        .with_page_urls(pages.to_vec());
        let ctx = egui::Context::default();
        let colors = crate::ui::theme::colors(false);
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| view.ui(ui, &colors, false),
            );
            output.textures_delta.clear();
            output
        };
        frame(Vec::new());
        let output = frame(Vec::new());
        let mut targets: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                Shape::Text(t) if t.galley.job.text == "Jump" => {
                    Some(t.pos + t.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .collect();
        targets.sort_by(|a, b| a.x.total_cmp(&b.x));
        assert_eq!(targets.len(), 2);
        for (target, page) in targets.into_iter().zip(pages) {
            let button = |pressed| egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(vec![egui::Event::PointerMoved(target), button(true)]);
            let output = frame(vec![button(false)]);
            assert!(output.platform_output.commands.iter().any(|command| {
                matches!(command, egui::OutputCommand::OpenUrl(open)
                    if open.url == page.join("#entry").unwrap().as_str())
            }));
        }
    }

    #[test]
    fn highlights_are_placed_as_the_viewer_places_them() {
        // Within inline code the viewer counts from the opening backticks.
        let text = "See `sync.interval` and ``a `b` c`` now";
        let at = |s: &str| text.find(s).unwrap();
        let placed = |text: &str, start: usize, end: usize| -> Vec<(usize, usize)> {
            viewer_ranges(text, std::slice::from_ref(&(start..end)))
                .iter()
                .map(|r| (r.start, r.end))
                .collect()
        };
        let tick = at("`sync");
        assert_eq!(
            placed(text, at("interval"), at("interval") + 8),
            [(tick + 5, tick + 13)]
        );
        let fence = at("``a");
        assert_eq!(
            placed(text, at("`b`"), at("`b`") + 3),
            [(fence + 2, fence + 5)]
        );
        // Plain text stays where it is, also around formatting.
        assert_eq!(
            placed(text, at("now"), text.len()),
            [(at("now"), text.len())]
        );
        assert_eq!(placed("**Free** plan", 0, 13), [(2, 6), (8, 13)]);
    }

    #[test]
    fn highlights_never_split_a_character() {
        use crate::core::diff::InlineMode;
        use pulldown_cmark::{Event, Parser};
        let pairs = [
            ("<p>AT&amp;copy;中文x</p>", "<p>AT&amp;reg;中文y</p>"),
            (
                "<ul><li><pre>a\n中文x\n</pre></li></ul>",
                "<ul><li><pre>a\n中文y\n</pre></li></ul>",
            ),
            (
                "<blockquote><p>中文<br>字x</p></blockquote>",
                "<blockquote><p>中文<br>字y</p></blockquote>",
            ),
            (
                "<p><a href=\"x\">連結x</a> <code>中`x</code></p>",
                "<p><a href=\"x\">連結y</a> <code>中`y</code></p>",
            ),
        ];
        for inline in [InlineMode::Word, InlineMode::Char] {
            let opts = DiffOptions {
                inline,
                ..DiffOptions::default()
            };
            for (l, r) in pairs {
                let pages = vec![
                    web_markdown::html_to_blocks(l, None),
                    web_markdown::html_to_blocks(r, None),
                ];
                let v = MarkdownView::new(pages.clone(), Vec::new(), &opts);
                for (row, marks) in v.rows.iter().zip(&v.marks) {
                    for (p, m) in marks.iter().enumerate() {
                        let (Some(Marks::Text(ranges)), Some(b)) = (m, row.cells[p]) else {
                            continue;
                        };
                        let text = &pages[p][b].text;
                        // As the viewer places them: from where each piece's source starts.
                        for (event, span) in
                            Parser::new_ext(text, viewer_options()).into_offset_iter()
                        {
                            let (Event::Text(t) | Event::Code(t)) = event else {
                                continue;
                            };
                            for r in ranges
                                .iter()
                                .filter(|r| r.start < span.end && r.end > span.start)
                            {
                                let s = r.start.saturating_sub(span.start).min(t.len());
                                let e = r.end.saturating_sub(span.start).min(t.len());
                                assert!(
                                    t.is_char_boundary(s) && t.is_char_boundary(e),
                                    "{inline:?} {text:?}: {s}..{e} in {t:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn visible_text_leaves_out_formatting_and_addresses() {
        assert_eq!(
            visible("**Free** \\| [docs](https://x/y) ~~old~~"),
            "Free | docs old"
        );
    }

    #[test]
    fn only_the_rows_of_a_table_in_view_are_laid_out() {
        let mut html = String::from("<table><tr><th>Id</th><th>Text</th></tr>");
        for r in 0..2000 {
            html.push_str(&format!(
                "<tr><td>{r}</td><td>{}</td></tr>",
                "word ".repeat(80)
            ));
        }
        html.push_str("</table>");
        let page = web_markdown::html_to_blocks(&html, None);
        let opts = DiffOptions::default();
        let mut v = MarkdownView::new(vec![page.clone(), page], Vec::new(), &opts);
        let ctx = egui::Context::default();
        let c = crate::ui::theme::colors(false);
        let mut measured = || {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))),
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| v.ui(ui, &c, false))
                .textures_delta
                .clear();
            let heights = &v.tables[&(0, 0)].heights;
            // Rows not laid out keep the one-line estimate, which the wrapped rows exceed.
            let estimate = heights[heights.len() - 1];
            heights.iter().filter(|&&h| h > estimate + 0.5).count()
        };
        let first = measured();
        assert!(first > 0 && first < 100, "{first} rows laid out");
        // Drawn again, the rows in view keep their measured heights.
        assert_eq!(measured(), first);
    }

    #[test]
    fn merged_cells_span_the_rows_and_columns_they_cover() {
        let long = "word ".repeat(60);
        let text = format!(
            "| Area | Revenue | < |\n| --- | --- | --- |\n| ^ | Q2 | Q3 |\n\
             | {long} | 1 | 2 |\n| ^ | 3 | 4 |\n| Total spanning the first two columns | < | 9 |"
        );
        let page = vec![Block { depth: 0, text }];
        let opts = DiffOptions::default();
        let mut v = MarkdownView::new(vec![page.clone(), page], Vec::new(), &opts);
        let ctx = egui::Context::default();
        let c = crate::ui::theme::colors(false);
        let mut frame = || {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 800.0))),
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| v.ui(ui, &c, false))
                .textures_delta
                .clear();
            let t = &v.tables[&(0, 0)];
            (t.heights.clone(), t.tall.clone(), t.natural.clone())
        };
        let mut last = frame();
        for _ in 0..4 {
            let next = frame();
            if next.0 == last.0 {
                break;
            }
            last = next;
        }
        let (heights, tall, natural) = frame();
        // Laid out again, nothing moves.
        assert_eq!(heights, last.0);
        // The long merged cell is taller than one row, and its two rows make room for it.
        let need = tall[&(2, 0)];
        assert!(need > heights[2], "{need} {heights:?}");
        assert!(heights[2] + heights[3] >= need + 2.0 * CELL_PAD[1] - 0.5);
        // The second row is a header row too, since a header cell reaches down into it.
        let t = &v.tables[&(0, 0)];
        assert_eq!(header_rows(&t.slots), 2);
        assert_eq!(t.owner(1, 0), (0, 0));
        assert_eq!(t.owner(4, 1), (4, 0));
        assert_eq!(natural.len(), 3);
    }

    #[test]
    fn scrolling_up_through_a_table_moves_it_as_far_as_scrolled() {
        let mut html = String::from("<table><tr><th>Id</th><th>Text</th></tr>");
        for r in 0..2000 {
            html.push_str(&format!(
                "<tr><td>{r}</td><td>{}</td></tr>",
                "word ".repeat(80)
            ));
        }
        html.push_str("</table><p>after</p>");
        let page = web_markdown::html_to_blocks(&html, None);
        let mut v = MarkdownView::new(
            vec![page.clone(), page],
            Vec::new(),
            &DiffOptions::default(),
        );
        let ctx = egui::Context::default();
        let c = crate::ui::theme::colors(false);
        let mut time = 0.0;
        let mut frame = |v: &mut MarkdownView, wheel: f32| {
            time += 0.25;
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(time),
                ..Default::default()
            };
            if wheel != 0.0 {
                raw.events
                    .push(egui::Event::PointerMoved(egui::pos2(400.0, 400.0)));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, wheel),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            ctx.run_ui(raw, |ui| v.ui(ui, &c, false))
                .textures_delta
                .clear();
        };
        let settle = |v: &mut MarkdownView, frame: &mut dyn FnMut(&mut MarkdownView, f32)| {
            for _ in 0..4 {
                frame(v, 0.0);
            }
        };
        settle(&mut v, &mut frame);
        // To the paragraph after the table: the rows above it were never laid out.
        v.scroll_to = Some(1);
        settle(&mut v, &mut frame);
        // Where table row `k` is in the view.
        let y_of = |v: &MarkdownView, k: usize| {
            let h = &v.tables[&(0, 0)].heights;
            v.top(0) + PAD[1] + h[..k].iter().sum::<f32>() - v.scroll
        };
        let mut k = 0;
        while y_of(&v, k) < 200.0 {
            k += 1;
        }
        for _ in 0..6 {
            let before = y_of(&v, k);
            frame(&mut v, 120.0);
            settle(&mut v, &mut frame);
            let moved = y_of(&v, k) - before;
            assert!((moved - 120.0).abs() < 1.0, "row {k} moved {moved}");
        }
    }

    fn block(t: &str) -> Block {
        Block {
            depth: 0,
            text: t.into(),
        }
    }

    #[test]
    fn each_changed_row_is_a_difference_and_can_be_stepped_through() {
        let left = vec![block("a"), block("b"), block("c"), block("d")];
        let right = vec![block("a"), block("B"), block("c"), block("D"), block("e")];
        let opts = DiffOptions::default();
        let pages = vec![left, right];
        // Without a cursor, previous goes to the last difference, as in the other views.
        let mut back = MarkdownView::new(pages.clone(), Vec::new(), &opts);
        assert!(back.prev_diff());
        assert_eq!(back.cur, Some(2));
        let mut v = MarkdownView::new(pages, vec!["l".into(), "r".into()], &opts);
        assert_eq!(v.diffs, [1..2, 3..4, 4..5]);
        assert_eq!(v.outcome(), Outcome::Different);
        assert!(v.next_diff());
        assert_eq!((v.cur, v.cursor, v.scroll_to), (Some(0), Some(1), Some(1)));
        assert!(v.next_diff());
        assert_eq!((v.cur, v.cursor), (Some(1), Some(3)));
        assert!(v.next_diff());
        assert!(!v.next_diff());
        assert_eq!((v.cur, v.cursor), (Some(2), Some(4)));
        v.first_diff();
        assert_eq!((v.cur, v.cursor), (Some(0), Some(1)));
        // A clicked row becomes the compared one; stepping goes on from there.
        v.click(2, 1);
        assert_eq!((v.cur, v.cursor, v.active), (None, Some(2), 1));
        assert!(v.prev_diff());
        assert_eq!((v.cur, v.cursor), (Some(0), Some(1)));
        v.click(4, 0);
        assert_eq!((v.cur, v.cursor), (Some(2), Some(4)));
        assert!(!v.next_diff());
        assert!(v.prev_diff());
        assert_eq!(v.cur, Some(1));
        let same = MarkdownView::new(vec![vec![block("a")], vec![block("a")]], Vec::new(), &opts);
        assert_eq!(same.outcome(), Outcome::Identical);
    }

    /// A page of `samples/web` with its known issues filled in, as its script does in a browser.
    fn release_notes(html: &str) -> Vec<Block> {
        let list = html.find("const issues = [").unwrap();
        let end = list + html[list..].find("];").unwrap();
        let items: String = html[list..end]
            .lines()
            .skip(1)
            .map(|l| l.trim().trim_end_matches(',').trim_matches('"'))
            .filter(|l| !l.is_empty())
            .map(|l| format!("<li>{l}</li>"))
            .collect();
        let html = html.replace(
            r#"<ul id="issues"></ul>"#,
            &format!(r#"<ul id="issues">{items}</ul>"#),
        );
        web_markdown::html_to_blocks(&html, None)
    }

    #[test]
    fn stepping_stops_at_every_changed_block_of_the_three_release_notes() {
        let pages = vec![
            release_notes(include_str!("../../samples/web/left.html")),
            release_notes(include_str!("../../samples/web/base.html")),
            release_notes(include_str!("../../samples/web/right.html")),
        ];
        let labels = vec!["left".into(), "base".into(), "right".into()];
        let mut v = MarkdownView::new(pages, labels, &DiffOptions::default());
        let mut stops = Vec::new();
        while v.next_diff() {
            let row = &v.rows[v.cursor.unwrap()];
            stops.push(row.cells[2].map_or(String::new(), |b| v.pages[2][b].text.clone()));
        }
        assert_eq!(stops.len(), v.rows.iter().filter(|r| r.changed).count());
        assert!(
            stops.iter().any(|s| s == "- Tags can have colors"),
            "{stops:#?}"
        );
        assert!(
            stops[stops.len() - 2].starts_with("- Sync can pause"),
            "{stops:#?}"
        );
        assert_eq!(
            stops.last().map(String::as_str),
            Some("- Colored tags are not shown in exported PDF files")
        );
    }

    #[test]
    fn stepping_scrolls_only_to_differences_out_of_view() {
        // In view, also when only its start is: the pages stay where they are.
        assert_eq!(reveal(100.0, 20.0, 0.0, 600.0), None);
        assert_eq!(reveal(500.0, 400.0, 0.0, 600.0), None);
        // Below or above the view, or starting too near its bottom: a third of the way down.
        assert_eq!(reveal(590.0, 40.0, 0.0, 600.0), Some(390.0));
        assert_eq!(reveal(1000.0, 20.0, 0.0, 600.0), Some(800.0));
        assert_eq!(reveal(50.0, 20.0, 400.0, 600.0), Some(0.0));
    }

    #[test]
    fn location_pane_marks_each_difference_where_it_is() {
        let left = vec![block("a"), block("b"), block("c")];
        let right = vec![block("a"), block("B"), block("c"), block("d")];
        let opts = DiffOptions::default();
        let mut v = MarkdownView::new(vec![left, right], vec!["l".into(), "r".into()], &opts);
        v.heights = vec![10.0, 20.0, 30.0, 40.0];
        v.next_diff();
        let c = crate::ui::theme::colors(true);
        let marks = v.location_marks(&c);
        let at: Vec<_> = marks.iter().map(|m| (m.start, m.len)).collect();
        assert_eq!(at, [(14, 20), (72, 40)]);
        assert_eq!(marks[0].columns[..2], [Some(c.ui.accent); 2]);
        // The block only the right page has: the left column shows the gap.
        assert_eq!(
            marks[1].columns[..2],
            [Some(c.ui.border_strong), Some(c.change_bar)]
        );
    }

    /// A table is one block however many rows it has: the overview marks the rows of it that
    /// differ, not the whole table.
    #[test]
    fn the_overview_marks_the_changed_rows_of_a_table() {
        let table = |last: &str| {
            let mut t = String::from("| a | b |\n| --- | --- |\n");
            for r in 0..8 {
                t.push_str(&format!("| {r} | same |\n"));
            }
            t.push_str(&format!("| 8 | {last} |\n"));
            block(&t)
        };
        let opts = DiffOptions::default();
        let pages = vec![vec![table("left")], vec![table("right")]];
        let mut v = MarkdownView::new(pages, vec!["l".into(), "r".into()], &opts);
        v.heights = vec![2.0 * PAD[1] + 100.0];
        let c = crate::ui::theme::colors(true);
        let marks = v.location_marks(&c);
        // Ten rows with the header, the last of them changed, on each page.
        let at: Vec<_> = marks.iter().map(|m| (m.start, m.len)).collect();
        let last = (PAD[1] as usize + 90, 10);
        assert_eq!(at, [last, last]);
        assert_eq!(marks[0].columns[..2], [Some(c.change_bar), None]);
        assert_eq!(marks[1].columns[..2], [None, Some(c.change_bar)]);
    }

    /// Rows are measured only as they are drawn, so a row further down grows from its estimate
    /// the first time it comes into view, and everything below it moves. The overview must not
    /// move with it: a mark that jumped while the pages were scrolled would be useless.
    #[test]
    fn the_overview_stays_put_as_rows_are_measured() {
        let table = "| a | b |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n";
        let left: Vec<Block> = ["one", "two", table, "four", "five"]
            .iter()
            .map(|t| block(t))
            .collect();
        let mut right = left.clone();
        right[4] = block("FIVE");
        let opts = DiffOptions::default();
        let mut v = MarkdownView::new(vec![left, right], vec!["l".into(), "r".into()], &opts);
        let c = crate::ui::theme::colors(true);
        // Steady heights as the window works them out, which drawing never changes.
        v.stable = vec![30.0, 30.0, 300.0, 30.0, 30.0];
        v.stable_for = (500.0, 14.0);
        v.heights = vec![ESTIMATE; v.rows.len()];
        let before = v.location_marks(&c);
        assert!(!before.is_empty(), "no difference to mark");

        // The tall table is reached and measured: ten times the estimate it started with.
        v.heights[2] = 10.0 * ESTIMATE;
        assert_eq!(
            v.location_marks(&c)
                .iter()
                .map(|m| (m.start, m.len))
                .collect::<Vec<_>>(),
            before.iter().map(|m| (m.start, m.len)).collect::<Vec<_>>(),
            "the overview moved when a row was measured"
        );

        // A place in the pages is the same place in either set of heights.
        let top_of_last = v.heights[..4].iter().map(|h| h + ROW_GAP).sum::<f32>();
        let steady = v.convert(top_of_last, &v.heights, &v.stable);
        assert!((steady - v.stable_top(4)).abs() < 0.5, "{steady}");
        let back = v.convert(steady, &v.stable, &v.heights);
        assert!((back - top_of_last).abs() < 0.5, "{back}");
    }
}
