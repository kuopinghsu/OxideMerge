//! Formatted web page compare: the pages as Markdown, shown block by block side by side, with
//! the blocks that differ highlighted.

use crate::core::diff::DiffOptions;
use crate::core::web_markdown::{self, Block, Cell, Marks, Row};
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
const GAP: f32 = 10.0;
/// Room between the blocks of a page.
const ROW_GAP: f32 = 4.0;
/// Room around a block, inside its highlight.
const PAD: f32 = 6.0;
/// Indentation of a block per list it is nested in.
const INDENT: f32 = 18.0;
/// Height of a block not shown yet.
const ESTIMATE: f32 = 28.0;
/// How far beyond the visible area blocks are laid out, so scrolling finds them measured.
const OVERSCAN: f32 = 300.0;
/// Room between a table cell's lines and its text, across and down.
const CELL_PAD: [f32; 2] = [6.0, 3.0];
/// Narrowest a table column is made to fit the page.
const MIN_COLUMN: f32 = 36.0;
/// Width of the scroll bar right of the pages.
const BAR: f32 = 12.0;

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
    /// Height of each row as last laid out.
    heights: Vec<f32>,
    /// The tables laid out so far, by row and page.
    tables: std::collections::HashMap<(usize, usize), TableLayout>,
    /// How far the pages are scrolled down, in pixels.
    scroll: f32,
    /// How much taller the rows above the view were found while drawn, added to `scroll` so
    /// that what is in view stays put.
    shift: f32,
    scroll_to: Option<usize>,
    cache: CommonMarkCache,
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
            cache: CommonMarkCache::default(),
            step_file: None,
        }
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

    /// Steps to the last difference before the cursor row.
    pub fn prev_diff(&mut self) -> bool {
        let prev = self
            .cursor
            .and_then(|r| self.diffs.iter().rposition(|d| d.start < r));
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

    /// The overview of the differences in the location pane, in pixels down the pages.
    fn location_marks(&self, c: &Colors) -> Vec<Mark> {
        let mut y = 0.0;
        let tops: Vec<f32> = self
            .heights
            .iter()
            .map(|h| {
                let top = y;
                y += h + ROW_GAP;
                top
            })
            .collect();
        self.diffs
            .iter()
            .enumerate()
            .map(|(d, run)| {
                let start = tops[run.start];
                let end = tops[run.end - 1] + self.heights[run.end - 1];
                let columns = std::array::from_fn(|p| {
                    let present = self.rows[run.clone()]
                        .iter()
                        .any(|r| r.cells.get(p).copied().flatten().is_some());
                    Some(if !present {
                        c.ui.border_strong
                    } else if self.cur == Some(d) {
                        c.ui.accent
                    } else {
                        c.change_bar
                    })
                });
                Mark {
                    start: start as usize,
                    len: (end - start).max(1.0) as usize,
                    columns,
                }
            })
            .collect()
    }

    /// `location` shows the location pane, as it does in the text compare.
    pub fn ui(&mut self, ui: &mut Ui, c: &Colors, location: bool) {
        widgets::toolbar(ui, |ui| {
            let any = !self.diffs.is_empty();
            if widgets::icon_button_enabled(ui, any, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
                self.first_diff();
            }
            if widgets::icon_button(ui, Icon::Prev, &Cmd::PrevDiff.tip()).clicked()
                && !self.prev_diff()
            {
                self.step_file = Some(false);
            }
            if widgets::icon_button(ui, Icon::Next, &Cmd::NextDiff.tip()).clicked()
                && !self.next_diff()
            {
                self.step_file = Some(true);
            }
            if widgets::icon_button_enabled(ui, any, Icon::Last, &Cmd::LastDiff.tip()).clicked() {
                self.last_diff();
            }
        });
        let pal = widgets::pal(ui);
        let n = self.pages.len().clamp(1, 3);
        let loc_w = if location { 14.0 + 8.0 * n as f32 } else { 0.0 };
        let side = loc_w + BAR;
        let header = ui.allocate_space(vec2(ui.available_width(), 24.0)).1;
        let (lefts, col) = self.columns(header.left(), header.width() - side);
        for (left, label) in lefts.iter().zip(&self.labels) {
            let rect = Rect::from_min_size(pos2(*left + PAD, header.top()), vec2(col - PAD, 24.0));
            let mut cell = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            cell.add(egui::Label::new(egui::RichText::new(label).color(pal.text_dim)).truncate())
                .on_hover_text(label);
        }

        let body = ui.available_rect_before_wrap();
        ui.allocate_rect(body, egui::Sense::hover());
        let view = Rect::from_min_max(body.min, pos2(body.right() - side, body.bottom()));
        let view_h = view.height().max(1.0);
        let total = self.top(self.heights.len());
        let max_scroll = (total - view_h).max(0.0);
        let mut offset = self.scroll + std::mem::take(&mut self.shift);
        if let Some(r) = self.scroll_to.take() {
            if let Some(to) = reveal(self.top(r), self.heights[r], offset, view_h) {
                offset = to;
            }
        }
        if location {
            let rect = Rect::from_min_max(
                pos2(view.right(), body.top()),
                pos2(view.right() + loc_w - 2.0, body.bottom()),
            );
            let marks = self.location_marks(c);
            if let Some(to) = widgets::location_pane(
                ui,
                Id::unique("md_location"),
                rect,
                total as usize,
                n,
                &marks,
                offset,
                view_h,
                c.location_bg,
                c.viewport,
            ) {
                offset = to - view_h / 2.0;
            }
        }
        let bar = Rect::from_min_max(pos2(body.right() - BAR, body.top()), body.max);
        widgets::vscrollbar(
            ui,
            Id::unique("md_vbar"),
            bar,
            &mut offset,
            total as usize,
            view_h,
        );
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
                            pos2(left + PAD + indent, top + PAD),
                            vec2((col - 2.0 * PAD - indent).max(20.0), f32::INFINITY),
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
                        match marks.get(i).and_then(|m| m.get(p)).and_then(Option::as_ref) {
                            Some(Marks::Table(t)) => {
                                let layout = tables.entry((i, p)).or_default();
                                let (r, g) = table(&mut cell, cache, layout, id, t, words);
                                relaid |= r;
                                grew = grew.max(g);
                            }
                            Some(Marks::Text(r)) => {
                                show_marked(&mut cell, cache, id, &block.text, r, words);
                            }
                            None => show_marked(&mut cell, cache, id, &block.text, &[], words),
                        }
                        height = height.max(cell.min_rect().height() + 2.0 * PAD);
                    }
                    let height = height.max(2.0 * PAD + 4.0);
                    if y + heights[i] <= viewport.min.y {
                        shift += height - heights[i];
                    } else if y < viewport.min.y {
                        shift += grew;
                    }
                    if (height - heights[i]).abs() > 0.5 {
                        heights[i] = height;
                        relaid = true;
                    }
                    if row.changed {
                        let all = row.cells.iter().all(Option::is_some);
                        for (p, left) in lefts.iter().enumerate() {
                            let color = match (row.cells.get(p).copied().flatten(), current) {
                                (None, false) => c.missing,
                                (None, true) => c.missing_sel,
                                (Some(_), false) if all => c.diff,
                                (Some(_), true) if all => c.diff_sel,
                                (Some(_), false) => c.added,
                                (Some(_), true) => c.added_sel,
                            };
                            let rect = Rect::from_min_size(pos2(*left, top), vec2(col, heights[i]));
                            ui.painter()
                                .set(backs[p], Shape::rect_filled(rect, 3.0, color));
                        }
                    }
                    for (p, left) in lefts.iter().enumerate() {
                        let rect = Rect::from_min_size(pos2(*left, top), vec2(col, heights[i]));
                        if pressed && ui.rect_contains_pointer(rect) {
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
                                crate::ui::theme::radius::XS,
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

/// Readies a table's cells to be shown: the header row bold, and the changed words placed as
/// the viewer places them.
fn prepare_table(rows: &mut [Vec<(String, Cell)>]) {
    for (r, row) in rows.iter_mut().enumerate() {
        for (text, cell) in row {
            if r == 0 && !text.is_empty() {
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
}

/// Draws a table as a grid of cells with lines around them and the header row shaded. What
/// differs is highlighted in `words`: changed words, and whole cells the other page lacks.
/// Only the rows in view are laid out. Returns whether a row's height changed, and how much
/// taller the rows above the view were found.
fn table(
    ui: &mut Ui,
    cache: &mut CommonMarkCache,
    layout: &mut TableLayout,
    id: Id,
    rows: &[Vec<(String, Cell)>],
    words: Color32,
) -> (bool, f32) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return (false, 0.0);
    }
    let [px, py] = CELL_PAD;
    let fonts =
        [egui::TextStyle::Body, egui::TextStyle::Monospace].map(|s| s.resolve(ui.style()).size);
    if layout.fonts != fonts || layout.natural.len() != cols {
        layout.fonts = fonts;
        layout.natural = vec![0.0_f32; cols];
        let body = egui::TextStyle::Body.resolve(ui.style());
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        ui.ctx().fonts_mut(|fonts| {
            for row in rows {
                for (k, (text, _)) in row.iter().enumerate() {
                    let w = natural_width(fonts, &body, &mono, text);
                    layout.natural[k] = layout.natural[k].max(w);
                }
            }
        });
        layout.heights.clear();
    }
    let line_height = ui.text_style_height(&egui::TextStyle::Body);
    if layout.heights.len() != rows.len() {
        layout.heights = vec![line_height + 2.0 * py; rows.len()];
    }
    let widths = fit(
        &layout.natural,
        ui.available_width() - cols as f32 * 2.0 * px,
    );
    let origin = ui.cursor().min;
    let mut xs = vec![origin.x];
    for w in &widths {
        xs.push(xs[xs.len() - 1] + w + 2.0 * px);
    }
    let clip = ui.clip_rect();
    let shown = clip.expand2(vec2(0.0, OVERSCAN)).y_range();
    let shade = ui.visuals().text_color().gamma_multiply(0.07);
    let mut relaid = false;
    let mut grew = 0.0;
    let mut ys = vec![origin.y];
    for (r, row) in rows.iter().enumerate() {
        let top = ys[r];
        if top + layout.heights[r] < shown.min || top > shown.max {
            ys.push(top + layout.heights[r]);
            continue;
        }
        let backs: Vec<_> = (0..cols).map(|_| ui.painter().add(Shape::Noop)).collect();
        let mut height = line_height;
        for (k, (text, cell)) in row.iter().enumerate() {
            let rect =
                Rect::from_min_size(pos2(xs[k] + px, top + py), vec2(widths[k], f32::INFINITY));
            let mut c = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::top_down(Align::Min))
                    .id_salt(("cell", r, k)),
            );
            c.set_clip_rect(
                Rect::from_x_y_ranges(xs[k]..=xs[k + 1], clip.y_range()).intersect(clip),
            );
            c.set_width(widths[k]);
            let marks = match cell {
                Cell::Words(v) => v.as_slice(),
                Cell::Whole => &[],
            };
            show_marked(&mut c, cache, id.with(("cell", r, k)), text, marks, words);
            height = height.max(c.min_rect().height());
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
            let fill = match row.get(k) {
                Some((_, Cell::Whole)) => words,
                _ if r == 0 => shade,
                _ => continue,
            };
            let rect = Rect::from_x_y_ranges(xs[k]..=xs[k + 1], top..=bottom);
            ui.painter().set(back, Shape::rect_filled(rect, 0.0, fill));
        }
        ys.push(bottom);
    }
    let color = ui.visuals().text_color().gamma_multiply(0.35);
    let stroke = Stroke::new(1.0, color);
    let outer = Rect::from_x_y_ranges(xs[0]..=xs[cols], origin.y..=ys[ys.len() - 1]);
    let painter = ui.painter();
    for &x in &xs[1..cols] {
        painter.vline(x, outer.y_range(), stroke);
    }
    for (r, &y) in ys.iter().enumerate().take(ys.len() - 1).skip(1) {
        if !shown.contains(y) {
            continue;
        }
        // The header row ends with a heavier line.
        let width = if r == 1 { 2.0 } else { 1.0 };
        painter.hline(outer.x_range(), y, Stroke::new(width, color));
    }
    painter.rect_stroke(outer, 2.0, stroke, StrokeKind::Inside);
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
            v.top(0) + PAD + h[..k].iter().sum::<f32>() - v.scroll
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
        let mut v = MarkdownView::new(vec![left, right], vec!["l".into(), "r".into()], &opts);
        assert_eq!(v.diffs, [1..2, 3..4, 4..5]);
        assert_eq!(v.outcome(), Outcome::Different);
        assert!(!v.prev_diff());
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
}
