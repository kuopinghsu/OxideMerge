//! The Tree view of two XML files: the aligned rows of [`TreeDiff`], both sides on one row so
//! they scroll together, with the details and source of the selected row below. Read-only:
//! nothing here edits the files.
use super::{syntax, theme, widgets};
use crate::core::semantic::xml_tree::{Collection, Pairing, RowKind, Side, TreeDiff, XKind};
use crate::core::semantic::Format;
use crate::i18n::{tr, trf};
use egui::{Color32, Rect, Sense, Ui};
use std::collections::HashSet;

/// A comparison and the text it was made from, which the source excerpts show.
pub struct TreeResult {
    pub diff: TreeDiff,
    pub sources: [String; 2],
    pub format: Format,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    Added,
    Removed,
    Modified,
    Moved,
    Ambiguous,
}

impl Filter {
    fn takes(self, diff: &TreeDiff, row: usize) -> bool {
        let r = &diff.rows[row];
        match self {
            Filter::Added => r.kind == RowKind::Added,
            Filter::Removed => r.kind == RowKind::Removed,
            Filter::Modified => r.kind == RowKind::Modified,
            Filter::Moved => r.moved.is_some(),
            Filter::Ambiguous => r.ambiguous,
        }
    }
}

/// The Tree view of a text compare: its comparison, for the layout generation and key rules
/// it was made with or why there is none, the one being made, and the view state.
#[derive(Default)]
pub struct TreeSession {
    pub cache: Option<(u64, String, Result<Box<TreeResult>, String>)>,
    pub job: Option<(u64, String, super::analysis::Task<TreeResult>)>,
    pub view: XmlTreeView,
    /// The text line to select once the comparison is ready.
    pub cursor: Option<(usize, usize)>,
}

/// What a click in the Tree view asks the text view for.
pub enum TreeAction {
    /// Show 1-based line `.1` of side `.0` in the Text view.
    Jump(usize, usize),
}

/// The state of the Tree view, which outlives the comparisons it shows.
#[derive(Default)]
pub struct XmlTreeView {
    pub selected: Option<usize>,
    expanded: HashSet<usize>,
    pub filter: Option<Filter>,
    /// Rows shown, in order, and what they were worked out for.
    visible: Vec<usize>,
    visible_for: Option<(u64, Option<Filter>, String, u64)>,
    /// Changes of the filter in each row and below it.
    filtered: Vec<usize>,
    /// Each row's place in tree order.
    rank: Vec<usize>,
    /// Bumped whenever the expansion changes.
    expansion: u64,
    pub search: String,
    hits: Vec<usize>,
    search_error: Option<String>,
    searched: Option<(u64, String)>,
    /// The key rules as typed.
    pub keys: String,
    /// The key rules the comparison is made with: those typed, once typing pauses.
    pub applied_keys: String,
    /// When the key rules were last typed in, until they are applied.
    keys_typed: Option<f64>,
    level: usize,
    scroll_to: Option<usize>,
    /// The rows the tree showed last frame, by their place among the visible rows.
    shown: std::ops::Range<usize>,
    /// What the selection was, by path, so it can be found again in a new comparison.
    remembered: Option<[Option<String>; 2]>,
    pub message: Option<String>,
    /// The source excerpts of a row of a comparison, highlighted for a dark or light theme.
    excerpt: Option<(u64, usize, bool, [Vec<Excerpt>; 2])>,
    /// The repeated elements shown as a table: their parent row, expanded name and name as
    /// written.
    collection: Option<(usize, String, String)>,
    /// The table of [`Self::collection`], and the comparison, parent row and name it is of.
    collection_table: Option<((u64, usize, String), Collection)>,
    collection_hidden: HashSet<String>,
    collection_filter: Option<Filter>,
    /// The files are JSON, YAML or TOML rather than XML.
    pub values: bool,
    /// The comparison the state belongs to.
    attached: u64,
    focused: bool,
}

struct Excerpt {
    line: usize,
    text: String,
    spans: Option<syntax::LineSpans>,
}

/// Which comparison a state is for, to tell a new one from the last.
fn identity(diff: &TreeDiff) -> u64 {
    diff.serial
}

impl XmlTreeView {
    /// Takes up a new comparison: changes expanded, and the selection found again by path.
    pub fn attach(&mut self, diff: &TreeDiff) {
        let id = identity(diff);
        if self.attached == id {
            return;
        }
        let old = self.selected.and(self.remembered.clone());
        self.attached = id;
        self.rank = vec![0; diff.rows.len()];
        for (k, r) in diff.preorder().into_iter().enumerate() {
            self.rank[r] = k;
        }
        self.reset(diff);
        self.searched = None;
        self.excerpt = None;
        self.collection = None;
        self.collection_table = None;
        self.selected = None;
        self.message = None;
        if let Some(paths) = old {
            match diff.row_at_path(&paths) {
                Some(r) => self.select(diff, r),
                None => self.message = Some(tr("analysis.tree_stale").to_owned()),
            }
        }
    }

    /// The default expansion: every row with a change below it.
    fn reset(&mut self, diff: &TreeDiff) {
        self.expanded = (0..diff.rows.len())
            .filter(|&r| diff.rows[r].changes > 0 && !diff.rows[r].children.is_empty())
            .collect();
        self.expansion += 1;
    }

    pub fn select(&mut self, diff: &TreeDiff, row: usize) {
        if row >= diff.rows.len() {
            return;
        }
        let mut at = diff.rows[row].parent;
        while let Some(p) = at {
            if self.expanded.insert(p) {
                self.expansion += 1;
            }
            at = diff.rows[p].parent;
        }
        self.selected = Some(row);
        self.scroll_to = Some(row);
        self.remembered = Some([0, 1].map(|s| diff.path(s, row)));
        self.message = None;
    }

    /// The change rows navigation visits, under the filter.
    fn targets<'a>(&self, diff: &'a TreeDiff) -> impl Iterator<Item = usize> + 'a {
        let filter = self.filter;
        diff.targets
            .iter()
            .copied()
            .filter(move |&t| filter.is_none_or(|f| f.takes(diff, t)))
    }

    /// Selects the next (or previous) change; false when there is none that way.
    pub fn step(&mut self, diff: &TreeDiff, forward: bool) -> bool {
        self.attach(diff);
        let at = self.selected.map(|s| self.rank[s]);
        let found = if forward {
            self.targets(diff)
                .find(|&t| at.is_none_or(|a| self.rank[t] > a))
        } else {
            self.targets(diff)
                .filter(|&t| at.is_none_or(|a| self.rank[t] < a))
                .last()
        };
        if let Some(t) = found {
            self.select(diff, t);
        }
        found.is_some()
    }

    pub fn edge(&mut self, diff: &TreeDiff, first: bool) {
        self.attach(diff);
        let found = if first {
            self.targets(diff).next()
        } else {
            self.targets(diff).last()
        };
        if let Some(t) = found {
            self.select(diff, t);
        }
    }

    /// The selected change's place among the changes, and their number.
    fn position(&self, diff: &TreeDiff) -> (Option<usize>, usize) {
        let mut at = None;
        let mut total = 0;
        for t in self.targets(diff) {
            if Some(t) == self.selected {
                at = Some(total);
            }
            total += 1;
        }
        (at, total)
    }

    fn update_visible(&mut self, diff: &TreeDiff) {
        let key = (
            identity(diff),
            self.filter,
            self.search.clone(),
            self.expansion,
        );
        if self.visible_for.as_ref() == Some(&key) {
            return;
        }
        if self
            .visible_for
            .as_ref()
            .is_none_or(|v| v.0 != key.0 || v.1 != key.1)
        {
            self.filtered = vec![0; diff.rows.len()];
            for t in self.targets(diff).collect::<Vec<_>>() {
                let mut at = Some(t);
                while let Some(r) = at {
                    self.filtered[r] += 1;
                    at = diff.rows[r].parent;
                }
            }
        }
        let searching = self.search.trim();
        if !searching.is_empty() {
            if self.searched.as_ref() != Some(&(key.0, searching.to_owned())) {
                (self.hits, self.search_error) = match diff.search(searching) {
                    Ok(hits) => (hits, None),
                    Err(e) => (Vec::new(), Some(e)),
                };
                self.searched = Some((key.0, searching.to_owned()));
            }
            let hits = &self.hits;
            let mut shown = HashSet::new();
            for &h in hits {
                let mut at = Some(h);
                while let Some(r) = at {
                    if !shown.insert(r) {
                        break;
                    }
                    at = diff.rows[r].parent;
                }
            }
            self.visible = diff
                .preorder()
                .into_iter()
                .filter(|r| shown.contains(r))
                .collect();
        } else {
            self.visible.clear();
            let mut stack: Vec<usize> = diff.top.iter().rev().copied().collect();
            while let Some(r) = stack.pop() {
                let row = &diff.rows[r];
                if row.blank || self.filter.is_some() && self.filtered[r] == 0 {
                    continue;
                }
                self.visible.push(r);
                if self.expanded.contains(&r) {
                    stack.extend(row.children.iter().rev());
                }
            }
        }
        self.visible_for = Some(key);
    }

    /// The toolbar, the aligned rows and the details of the selected row.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        result: &TreeResult,
        titles: [&str; 2],
    ) -> Option<TreeAction> {
        let diff = &result.diff;
        self.attach(diff);
        let mut action = None;
        self.toolbar(ui, diff);
        for warning in &diff.warnings {
            ui.colored_label(
                widgets::pal(ui).warning,
                trf(
                    "analysis.tree_key_warning",
                    &[
                        ("rule", warning.rule.clone()),
                        ("duplicates", warning.duplicates.to_string()),
                        ("missing", warning.missing.to_string()),
                    ],
                ),
            );
        }
        if let Some(message) = &self.message {
            ui.colored_label(widgets::pal(ui).warning, message);
        }
        if diff.identical() {
            ui.label(tr("analysis.semantic_identical"));
        }
        let details = 190.0_f32.min(ui.available_height() * 0.45);
        let tree_height = (ui.available_height() - details - 8.0).max(80.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), tree_height), |ui| {
            // keep the details pane in place when the content is shorter
            ui.set_min_height(tree_height);
            match self.collection.clone() {
                Some((parent, name, display)) => {
                    self.collection_ui(ui, diff, parent, &name, &display)
                }
                None => self.rows_ui(ui, diff, titles),
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("xml_tree_details")
            .auto_shrink([false, true])
            .show(ui, |ui| self.details_ui(ui, result, &mut action));
        action
    }

    fn toolbar(&mut self, ui: &mut Ui, diff: &TreeDiff) {
        let c = diff.counts;
        ui.horizontal_wrapped(|ui| {
            ui.label(trf(
                "analysis.tree_summary",
                &[
                    ("added", c.added.to_string()),
                    ("removed", c.removed.to_string()),
                    ("modified", c.modified.to_string()),
                    ("moved", c.moved.to_string()),
                    ("ambiguous", c.ambiguous.to_string()),
                ],
            ));
            widgets::toolbar_sep(ui);
            let (at, total) = self.position(diff);
            if widgets::icon_button(
                ui,
                super::icons::Icon::First,
                &super::commands::Cmd::FirstDiff.tip(),
            )
            .clicked()
            {
                self.edge(diff, true);
            }
            if widgets::icon_button(
                ui,
                super::icons::Icon::Prev,
                &super::commands::Cmd::PrevDiff.tip(),
            )
            .clicked()
            {
                self.step(diff, false);
            }
            ui.label(match at {
                Some(n) => trf(
                    "analysis.tree_position",
                    &[("n", (n + 1).to_string()), ("total", total.to_string())],
                ),
                None => trf(
                    "analysis.tree_position_none",
                    &[("total", total.to_string())],
                ),
            });
            if widgets::icon_button(
                ui,
                super::icons::Icon::Next,
                &super::commands::Cmd::NextDiff.tip(),
            )
            .clicked()
            {
                self.step(diff, true);
            }
            if widgets::icon_button(
                ui,
                super::icons::Icon::Last,
                &super::commands::Cmd::LastDiff.tip(),
            )
            .clicked()
            {
                self.edge(diff, false);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let before = self.filter;
            ui.selectable_value(&mut self.filter, None, tr("analysis.all"));
            for (f, key) in [
                (Filter::Added, "analysis.added"),
                (Filter::Removed, "analysis.removed"),
                (Filter::Modified, "analysis.modified"),
                (Filter::Moved, "analysis.moved"),
                (Filter::Ambiguous, "analysis.tree_ambiguous"),
            ] {
                ui.selectable_value(&mut self.filter, Some(f), tr(key));
            }
            if before != self.filter {
                if let Some(s) = self.selected {
                    if diff.rows[s].is_change() && self.filter.is_some_and(|f| !f.takes(diff, s)) {
                        self.message = Some(tr("analysis.tree_hidden_by_filter").to_owned());
                    }
                }
            }
            widgets::toolbar_sep(ui);
            if ui.button(tr("analysis.tree_expand_changes")).clicked() {
                self.reset(diff);
            }
            if ui.button(tr("analysis.tree_collapse_unchanged")).clicked() {
                self.expanded.retain(|&r| diff.rows[r].changes > 0);
                self.expansion += 1;
            }
            ui.label(tr("analysis.tree_expand_level"));
            let level = ui.add(egui::DragValue::new(&mut self.level).range(0..=64));
            if level.changed() {
                let level = self.level;
                self.expanded = (0..diff.rows.len())
                    .filter(|&r| diff.rows[r].depth < level && !diff.rows[r].children.is_empty())
                    .collect();
                self.expansion += 1;
            }
            if ui.button(tr("analysis.tree_reset")).clicked() {
                self.reset(diff);
                self.level = 0;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("analysis.tree_search"));
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(tr(if self.values {
                        "analysis.tree_search_hint_values"
                    } else {
                        "analysis.tree_search_hint"
                    }))
                    .char_limit(512)
                    .desired_width(220.0),
            );
            labeled(&search, tr("analysis.tree_search"), &self.search);
            if !self.search.trim().is_empty() {
                if let Some(e) = &self.search_error {
                    ui.colored_label(widgets::pal(ui).danger, e);
                } else if self.searched.is_some() {
                    if self.hits.is_empty() {
                        ui.label(tr("analysis.tree_search_none"));
                    } else {
                        ui.label(trf(
                            "analysis.tree_search_hits",
                            &[("n", self.hits.len().to_string())],
                        ));
                    }
                }
            }
            widgets::toolbar_sep(ui);
            self.keys_ui(ui);
            if diff.keyed > 0 {
                ui.small(trf("analysis.tree_keyed", &[("n", diff.keyed.to_string())]));
            }
            for (member, n) in &diff.identified {
                ui.small(trf(
                    "analysis.tree_identified",
                    &[("n", n.to_string()), ("member", member.to_string())],
                ))
                .on_hover_text(tr("analysis.tree_identified_tip"));
            }
        });
    }

    /// The key rules field. What is typed is applied once typing pauses or the field is left,
    /// so a comparison is not made for every key pressed.
    pub fn keys_ui(&mut self, ui: &mut Ui) {
        let (hint, tip) = if self.values {
            (
                "analysis.tree_keys_hint_values",
                "analysis.tree_keys_tip_values",
            )
        } else {
            ("analysis.tree_keys_hint", "analysis.tree_keys_tip")
        };
        ui.label(tr("analysis.tree_keys")).on_hover_text(tr(tip));
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut self.keys)
                    // A fixed id keeps focus while typing when the field moves
                    // into the toolbar once the comparison finishes.
                    .id(egui::Id::unique("xml_tree_keys"))
                    .hint_text(tr(hint))
                    .char_limit(512)
                    .desired_width(200.0),
            )
            .on_hover_text(tr(tip));
        labeled(&response, tr("analysis.tree_keys"), &self.keys);
        let now = ui.input(|i| i.time);
        if response.changed() {
            self.keys_typed = Some(now);
        }
        if let Some(typed) = self.keys_typed {
            const PAUSE: f64 = 0.3;
            let waited = now - typed;
            if waited >= PAUSE || response.lost_focus() {
                self.keys_typed = None;
                self.applied_keys = self.keys.clone();
                ui.ctx().request_repaint();
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(PAUSE - waited));
            }
        }
    }

    fn rows_ui(&mut self, ui: &mut Ui, diff: &TreeDiff, titles: [&str; 2]) {
        self.update_visible(diff);
        let colors = theme::colors(ui.visuals().dark_mode);
        let height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        ui.horizontal(|ui| {
            let half = (ui.available_width() - GUTTER) / 2.0;
            for (side, title) in titles.iter().enumerate() {
                ui.allocate_ui(egui::vec2(half, height), |ui| {
                    widgets::side_chip(ui, side, 2);
                    let name = std::path::Path::new(title)
                        .file_name()
                        .map_or(*title, |n| n.to_str().unwrap_or(title));
                    ui.add(egui::Label::new(name).truncate())
                        .on_hover_text(*title);
                });
                if side == 0 {
                    ui.add_space(GUTTER - 8.0);
                }
            }
        });
        // keyboard: up and down move through rows, left and right fold them
        if self.focused && !widgets::typing(ui.ctx()) && !widgets::modal_open(ui.ctx()) {
            let (up, down, left, right) = ui.input(|i| {
                let plain = i.modifiers.is_none();
                (
                    plain && i.key_pressed(egui::Key::ArrowUp),
                    plain && i.key_pressed(egui::Key::ArrowDown),
                    plain && i.key_pressed(egui::Key::ArrowLeft),
                    plain && i.key_pressed(egui::Key::ArrowRight),
                )
            });
            let at = self
                .selected
                .and_then(|s| self.visible.iter().position(|&v| v == s));
            if up || down {
                let next = match at {
                    Some(a) if up => a.checked_sub(1),
                    Some(a) => (a + 1 < self.visible.len()).then_some(a + 1),
                    None => (!self.visible.is_empty()).then_some(0),
                };
                if let Some(n) = next {
                    let r = self.visible[n];
                    self.select(diff, r);
                }
            }
            if let Some(s) = self.selected {
                if right && !diff.rows[s].children.is_empty() && self.expanded.insert(s) {
                    self.expansion += 1;
                }
                if left {
                    if self.expanded.remove(&s) {
                        self.expansion += 1;
                    } else if let Some(p) = diff.rows[s].parent {
                        self.select(diff, p);
                    }
                }
            }
            self.update_visible(diff);
        }
        let target = self
            .scroll_to
            .take()
            .and_then(|s| self.visible.iter().position(|&v| v == s));
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("xml_tree_rows")
            .auto_shrink([false, false]);
        if let Some(t) = target {
            let spacing = ui.spacing().item_spacing.y;
            let top = t as f32 * (height + spacing);
            let view = ui.available_height();
            let shown = self.shown.clone();
            // fully shown rows are left in place; a row next to them scrolls just enough, so
            // stepping with the arrow keys does not jump, and one further away is centred
            let whole = shown.start + 1..shown.end.saturating_sub(1);
            let offset = if whole.contains(&t) {
                None
            } else if t < whole.start && t + 2 >= whole.start {
                Some(top)
            } else if t >= whole.end && t <= whole.end + 1 {
                Some(top + height - view)
            } else {
                Some(top - view / 2.0)
            };
            if let Some(offset) = offset {
                scroll = scroll.vertical_scroll_offset(offset.max(0.0));
            }
        }
        let mut toggled = None;
        let mut clicked = None;
        let visible = std::mem::take(&mut self.visible);
        scroll.show_rows(ui, height, visible.len(), |ui, range| {
            self.shown = range.clone();
            for &r in &visible[range] {
                let (rect, response) = ui
                    .allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
                let label = self.row_text(diff, r);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::ListItem,
                        true,
                        self.selected == Some(r),
                        &label,
                    )
                });
                if ui.is_rect_visible(rect) {
                    if let Some(on_toggle) = self.paint_row(ui, diff, r, rect, &colors) {
                        if response.clicked()
                            && response
                                .interact_pointer_pos()
                                .is_some_and(|p| on_toggle.contains(p))
                        {
                            toggled = Some(r);
                        }
                    }
                }
                if response.clicked() && toggled != Some(r) {
                    clicked = Some(r);
                }
                if response.double_clicked() && !diff.rows[r].children.is_empty() {
                    toggled = Some(r);
                }
                response.on_hover_text(&label);
            }
        });
        self.visible = visible;
        if let Some(r) = toggled {
            if !self.expanded.remove(&r) {
                self.expanded.insert(r);
            }
            self.expansion += 1;
            self.focused = true;
        }
        if let Some(r) = clicked {
            self.select(diff, r);
            self.scroll_to = None;
            self.focused = true;
        }
        if ui.input(|i| i.pointer.any_click()) && clicked.is_none() && toggled.is_none() {
            let pointer = ui.input(|i| i.pointer.interact_pos());
            if pointer.is_some_and(|p| !ui.min_rect().contains(p)) {
                self.focused = false;
            }
        }
    }

    /// What row `r` says, for its tooltip and screen readers.
    fn row_text(&self, diff: &TreeDiff, r: usize) -> String {
        let row = &diff.rows[r];
        let mut status = vec![status_name(diff, r).to_owned()];
        if let Some([from, to]) = row.moved {
            status.push(trf(
                "analysis.tree_moved_detail",
                &[("from", from.to_string()), ("to", to.to_string())],
            ));
        }
        if row.ambiguous {
            status.push(tr("analysis.ambiguous").to_owned());
        }
        if row.changes > row.is_change() as usize {
            status.push(trf(
                "analysis.tree_changes_badge",
                &[("n", (row.changes - row.is_change() as usize).to_string())],
            ));
        }
        let side = |s: usize| match row.sides[s] {
            None => tr("analysis.tree_missing").to_owned(),
            Some(_) => node_label(diff, s, r),
        };
        format!("{}: {} | {}", status.join(" · "), side(0), side(1))
    }

    /// Paints row `r`; returns where its disclosure triangle is, when it has one.
    fn paint_row(
        &self,
        ui: &Ui,
        diff: &TreeDiff,
        r: usize,
        rect: Rect,
        colors: &theme::Colors,
    ) -> Option<Rect> {
        let row = &diff.rows[r];
        let painter = ui.painter_at(rect);
        let half = (rect.width() - GUTTER) / 2.0;
        let sides = [
            Rect::from_min_size(rect.min, egui::vec2(half, rect.height())),
            Rect::from_min_size(
                egui::pos2(rect.min.x + half + GUTTER, rect.min.y),
                egui::vec2(half, rect.height()),
            ),
        ];
        let gutter = Rect::from_min_max(
            egui::pos2(sides[0].max.x, rect.min.y),
            egui::pos2(sides[1].min.x, rect.max.y),
        );
        let selected = self.selected == Some(r);
        if selected {
            painter.rect_filled(rect, 2.0, widgets::pal(ui).accent_soft);
        }
        let text_color = ui.visuals().text_color();
        let dim = widgets::pal(ui).text_dim;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let mut toggle = None;
        for (s, &area) in sides.iter().enumerate() {
            let fill = match (row.kind, row.sides[s].is_some()) {
                (_, false) => Some(colors.missing),
                (RowKind::Added, true) => Some(colors.added),
                (RowKind::Removed, true) => Some(colors.removed),
                (RowKind::Modified, true) if !row.within => Some(colors.diff),
                _ if row.moved.is_some() => Some(colors.moved),
                _ => None,
            };
            if let Some(fill) = fill {
                painter.rect_filled(area.shrink2(egui::vec2(0.0, 1.0)), 2.0, fill);
            }
            if row.sides[s].is_none() {
                // a placeholder: no text, so it does not look like data
                let y = area.center().y;
                painter.line_segment(
                    [
                        egui::pos2(area.min.x + 8.0, y),
                        egui::pos2(area.max.x - 8.0, y),
                    ],
                    egui::Stroke::new(1.0, dim.gamma_multiply(0.4)),
                );
                continue;
            }
            let indent = area.min.x + 6.0 + row.depth as f32 * INDENT;
            let has_children =
                !row.children.is_empty() && row.children.iter().any(|&c| !diff.rows[c].blank);
            if has_children {
                let open = self.expanded.contains(&r) || !self.search.trim().is_empty();
                let tri = Rect::from_center_size(
                    egui::pos2(indent + 5.0, area.center().y),
                    egui::vec2(12.0, 12.0),
                );
                let points = if open {
                    vec![
                        egui::pos2(tri.min.x + 1.0, tri.min.y + 3.0),
                        egui::pos2(tri.max.x - 1.0, tri.min.y + 3.0),
                        egui::pos2(tri.center().x, tri.max.y - 2.0),
                    ]
                } else {
                    vec![
                        egui::pos2(tri.min.x + 3.0, tri.min.y + 1.0),
                        egui::pos2(tri.max.x - 2.0, tri.center().y),
                        egui::pos2(tri.min.x + 3.0, tri.max.y - 1.0),
                    ]
                };
                painter.add(egui::Shape::convex_polygon(points, dim, egui::Stroke::NONE));
                let mut hit = tri.expand(3.0);
                hit.max.y = rect.max.y;
                hit.min.y = rect.min.y;
                toggle = Some(toggle.map_or(hit, |t: Rect| t.union(hit)));
            }
            let x = indent + 16.0;
            let clip = Rect::from_min_max(egui::pos2(x, area.min.y), area.max);
            let p = painter.with_clip_rect(clip);
            let mut job = egui::text::LayoutJob::default();
            let fmt = |color: Color32, font: &egui::FontId| egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            };
            self.label_job(
                diff, s, r, &mut job, text_color, dim, colors, &font, &mono, &fmt,
            );
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let y = area.center().y - galley.size().y / 2.0;
            p.galley(egui::pos2(x, y), galley, text_color);
        }
        // the status column: a symbol, never color alone
        let (symbol, color) = status_symbol(diff, r, ui);
        if !symbol.is_empty() {
            painter.text(
                gutter.center(),
                egui::Align2::CENTER_CENTER,
                symbol,
                font.clone(),
                color,
            );
        }
        if selected {
            painter.rect_stroke(
                rect.shrink(0.5),
                2.0,
                egui::Stroke::new(1.5, widgets::pal(ui).accent),
                egui::StrokeKind::Inside,
            );
        }
        let open = self.expanded.contains(&r) || !self.search.trim().is_empty();
        let hidden = if self.filter.is_some() {
            self.filtered.get(r).copied().unwrap_or(0)
                - self
                    .filter
                    .is_some_and(|f| row.is_change() && f.takes(diff, r)) as usize
        } else {
            row.changes - row.is_change() as usize
        };
        if !open && hidden > 0 {
            let text = trf("analysis.tree_changes_badge", &[("n", hidden.to_string())]);
            let side = if row.sides[1].is_some() { 1 } else { 0 };
            let small = egui::TextStyle::Small.resolve(ui.style());
            let galley =
                ui.fonts_mut(|f| f.layout_no_wrap(text, small, widgets::pal(ui).on_accent));
            let size = galley.size() + egui::vec2(10.0, 2.0);
            let chip = Rect::from_min_size(
                egui::pos2(
                    sides[side].max.x - size.x - 4.0,
                    rect.center().y - size.y / 2.0,
                ),
                size,
            );
            painter.rect_filled(chip, 6.0, widgets::pal(ui).accent);
            painter.galley(chip.min + egui::vec2(5.0, 1.0), galley, Color32::WHITE);
        }
        toggle
    }

    #[allow(clippy::too_many_arguments)]
    fn label_job(
        &self,
        diff: &TreeDiff,
        s: usize,
        r: usize,
        job: &mut egui::text::LayoutJob,
        text: Color32,
        dim: Color32,
        colors: &theme::Colors,
        font: &egui::FontId,
        mono: &egui::FontId,
        fmt: &dyn Fn(Color32, &egui::FontId) -> egui::TextFormat,
    ) {
        let row = &diff.rows[r];
        let Some(side) = row.sides[s] else {
            return;
        };
        let doc = &diff.docs[s];
        match side {
            Side::Attr(n, a) => {
                let attr = &doc.nodes[n].attrs[a];
                job.append(&format!("@{}", attr.display), 0.0, fmt(dim, mono));
                job.append(" = ", 0.0, fmt(dim, mono));
                job.append(&quoted(&attr.value), 0.0, fmt(text, mono));
            }
            Side::Node(n) => {
                let node = &doc.nodes[n];
                match node.kind {
                    XKind::Element => {
                        job.append(&format!("<{}>", node.display), 0.0, fmt(text, mono));
                        // key attributes, and those that changed, as short chips
                        let other = row.sides[1 - s].map(|o| &diff.docs[1 - s].nodes[o.node()]);
                        let mut shown = 0;
                        for attr in &node.attrs {
                            let changed = other.is_some_and(|o| {
                                o.attrs
                                    .iter()
                                    .find(|x| x.name == attr.name)
                                    .map(|x| &x.value)
                                    != Some(&attr.value)
                            });
                            let local = attr.name.rsplit('}').next().unwrap_or("");
                            let key = matches!(local, "id" | "name" | "key" | "ref")
                                || self
                                    .applied_keys
                                    .split(['@', '+', ';'])
                                    .any(|k| k.trim() == local);
                            if (key || changed) && shown < 3 {
                                shown += 1;
                                let mut f = fmt(if changed { text } else { dim }, mono);
                                if changed {
                                    f.background = colors.inline;
                                }
                                job.append(" ", 0.0, fmt(dim, mono));
                                job.append(
                                    &format!(
                                        "@{}={}",
                                        attr.display,
                                        quoted(&shorten(&attr.value, 24))
                                    ),
                                    0.0,
                                    f,
                                );
                            }
                        }
                        if node.attrs.len() > shown && shown < 3 {
                            job.append(&format!("  [{}]", node.attrs.len()), 0.0, fmt(dim, font));
                        }
                    }
                    XKind::Text => {
                        job.append(&quoted(&shorten(&node.value, 200)), 0.0, fmt(text, mono));
                    }
                    XKind::Comment => {
                        job.append(
                            &format!("<!--{}-->", shorten(&node.value, 200)),
                            0.0,
                            fmt(dim, mono),
                        );
                    }
                    XKind::Pi => {
                        job.append(
                            &format!("<?{}?>", shorten(&node.value, 200)),
                            0.0,
                            fmt(dim, mono),
                        );
                    }
                    XKind::Object | XKind::Array | XKind::Value => {
                        // array positions and the root are dimmed: keys are what pairs
                        let key = if node.name.is_empty() { dim } else { text };
                        job.append(&node.display, 0.0, fmt(key, mono));
                        match node.kind {
                            XKind::Value => {
                                job.append(": ", 0.0, fmt(dim, mono));
                                let other =
                                    row.sides[1 - s].map(|o| &diff.docs[1 - s].nodes[o.node()]);
                                let mut f = fmt(text, mono);
                                if other.is_some_and(|o| o.kind != node.kind || o.tag != node.tag) {
                                    f.background = colors.inline;
                                }
                                job.append(&shorten(&node.value, 200), 0.0, f);
                            }
                            XKind::Object => {
                                // the key members of an array element, as short chips
                                let mut shown = 0;
                                for &c in &node.children {
                                    let member = &doc.nodes[c];
                                    let key = matches!(
                                        member.name.as_str(),
                                        "id" | "name" | "key" | "ref"
                                    ) || self
                                        .applied_keys
                                        .split(['@', '+', ';'])
                                        .any(|k| k.trim() == member.name);
                                    if member.kind == XKind::Value && key && shown < 2 {
                                        shown += 1;
                                        job.append(
                                            &format!(
                                                " {}={}",
                                                member.name,
                                                shorten(&member.value, 24)
                                            ),
                                            0.0,
                                            fmt(dim, mono),
                                        );
                                    }
                                }
                                job.append(
                                    &format!("  {{{}}}", node.children.len()),
                                    0.0,
                                    fmt(dim, font),
                                );
                            }
                            _ => {
                                job.append(
                                    &format!("  [{}]", node.children.len()),
                                    0.0,
                                    fmt(dim, font),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn details_ui(&mut self, ui: &mut Ui, result: &TreeResult, action: &mut Option<TreeAction>) {
        let diff = &result.diff;
        let Some(r) = self.selected.filter(|&r| r < diff.rows.len()) else {
            ui.small(tr(policy_key(self.values)));
            return;
        };
        let row = &diff.rows[r];
        if self.collection.is_none() {
            let groups = diff.collections(r);
            if !groups.is_empty() {
                ui.horizontal(|ui| {
                    for (name, display, n) in groups {
                        if ui
                            .button(format!(
                                "{} {} ({n})",
                                tr("analysis.tree_collection"),
                                shown_name(diff, &display)
                            ))
                            .clicked()
                        {
                            self.collection = Some((r, name, display));
                        }
                    }
                });
            }
        }
        let mut status = vec![status_name(diff, r).to_owned()];
        if let Some([from, to]) = row.moved {
            status.push(trf(
                "analysis.tree_moved_detail",
                &[("from", from.to_string()), ("to", to.to_string())],
            ));
        }
        ui.horizontal_wrapped(|ui| {
            ui.strong(status.join(" · "));
            ui.label(tr(match row.pairing {
                Pairing::Exact => "analysis.tree_pairing_exact",
                Pairing::Key => "analysis.tree_pairing_key",
                Pairing::Name => "analysis.tree_pairing_name",
                Pairing::Member => "analysis.tree_pairing_member",
                Pairing::Identity => "analysis.tree_pairing_identity",
                Pairing::Unpaired => "analysis.tree_pairing_unpaired",
            }));
        });
        if row.ambiguous {
            ui.colored_label(widgets::pal(ui).warning, tr("analysis.ambiguous_hint"));
        }
        let dark = ui.visuals().dark_mode;
        if self
            .excerpt
            .as_ref()
            .is_none_or(|e| (e.0, e.1, e.2) != (identity(diff), r, dark))
        {
            let excerpts = [0, 1].map(|s| excerpt(result, s, r, dark));
            self.excerpt = Some((identity(diff), r, dark, excerpts));
        }
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        ui.columns(2, |cols| {
            for (s, ui) in cols.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    widgets::side_chip(ui, s, 2);
                    match diff.path(s, r) {
                        Some(path) => {
                            ui.add(egui::Label::new(egui::RichText::new(path).monospace()).wrap());
                        }
                        None => {
                            ui.label(tr("analysis.tree_missing"));
                        }
                    }
                });
                let Some((_, line, col)) = diff.location(s, r) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    ui.label(trf(
                        "analysis.tree_location",
                        &[("line", line.to_string()), ("col", col.to_string())],
                    ));
                    if ui
                        .button(format!("{} {}:{}", tr("analysis.source"), s + 1, line))
                        .clicked()
                    {
                        *action = Some(TreeAction::Jump(s, line));
                    }
                });
                if let Some(value) = diff.value(s, r).filter(|v| !v.is_empty()) {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(quoted(&shorten(value, 512))).monospace(),
                        )
                        .wrap(),
                    );
                }
                if let Some((_, _, _, excerpts)) = &self.excerpt {
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            for e in &excerpts[s] {
                                let mut job = egui::text::LayoutJob::default();
                                let plain = egui::TextFormat {
                                    font_id: mono.clone(),
                                    color: ui.visuals().weak_text_color(),
                                    ..Default::default()
                                };
                                job.append(&format!("{:>5}  ", e.line), 0.0, plain.clone());
                                let text_color = ui.visuals().text_color();
                                match &e.spans {
                                    Some(spans) if !spans.is_empty() => {
                                        let mut at = 0;
                                        for (range, color) in spans {
                                            if range.start > at {
                                                job.append(
                                                    &e.text[at..range.start],
                                                    0.0,
                                                    egui::TextFormat {
                                                        color: text_color,
                                                        ..plain.clone()
                                                    },
                                                );
                                            }
                                            job.append(
                                                &e.text[range.clone()],
                                                0.0,
                                                egui::TextFormat {
                                                    color: *color,
                                                    ..plain.clone()
                                                },
                                            );
                                            at = range.end;
                                        }
                                        if at < e.text.len() {
                                            job.append(
                                                &e.text[at..],
                                                0.0,
                                                egui::TextFormat {
                                                    color: text_color,
                                                    ..plain.clone()
                                                },
                                            );
                                        }
                                    }
                                    _ => job.append(
                                        &e.text,
                                        0.0,
                                        egui::TextFormat {
                                            color: text_color,
                                            ..plain.clone()
                                        },
                                    ),
                                }
                                ui.add(egui::Label::new(job).truncate());
                            }
                        });
                }
            }
        });
    }

    fn collection_ui(
        &mut self,
        ui: &mut Ui,
        diff: &TreeDiff,
        parent: usize,
        name: &str,
        display: &str,
    ) {
        let key = (identity(diff), parent, name.to_owned());
        let table = match self.collection_table.take() {
            Some((made, table)) if made == key => table,
            _ => diff.collection(parent, name),
        };
        self.collection_table_ui(ui, diff, display, &table);
        self.collection_table = Some((key, table));
    }

    fn collection_table_ui(
        &mut self,
        ui: &mut Ui,
        diff: &TreeDiff,
        display: &str,
        table: &Collection,
    ) {
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("analysis.tree_collection_tree")).clicked() {
                self.collection = None;
            }
            ui.strong(format!(
                "{} ({})",
                shown_name(diff, display),
                table.rows.len()
            ));
            ui.selectable_value(&mut self.collection_filter, None, tr("analysis.all"));
            for (f, key) in [
                (Filter::Added, "analysis.added"),
                (Filter::Removed, "analysis.removed"),
                (Filter::Modified, "analysis.modified"),
                (Filter::Moved, "analysis.moved"),
            ] {
                ui.selectable_value(&mut self.collection_filter, Some(f), tr(key));
            }
            ui.menu_button(tr("analysis.tree_columns"), |ui| {
                for column in &table.columns {
                    let mut on = !self.collection_hidden.contains(column);
                    if ui.checkbox(&mut on, column).changed() {
                        if on {
                            self.collection_hidden.remove(column);
                        } else {
                            self.collection_hidden.insert(column.clone());
                        }
                    }
                }
            });
        });
        if table.nested {
            ui.colored_label(
                widgets::pal(ui).warning,
                tr("analysis.tree_collection_nested"),
            );
        }
        let shown_columns: Vec<usize> = (0..table.columns.len())
            .filter(|&c| !self.collection_hidden.contains(&table.columns[c]))
            .collect();
        let filter = self.collection_filter;
        let rows: Vec<_> = table
            .rows
            .iter()
            .filter(|item| {
                filter.is_none_or(|f| match f {
                    // a modified element has its changes below it
                    Filter::Modified => {
                        let row = &diff.rows[item.row];
                        row.kind == RowKind::Same && row.changes > row.is_change() as usize
                    }
                    f => f.takes(diff, item.row),
                })
            })
            .collect();
        let mut clicked = None;
        let height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::horizontal()
            .id_salt("xml_collection_h")
            .show(ui, |ui| {
                egui_extras::TableBuilder::new(ui)
                    .id_salt("xml_collection")
                    .striped(true)
                    .resizable(true)
                    .auto_shrink([false, false])
                    .sense(Sense::click())
                    .column(egui_extras::Column::initial(170.0).at_least(80.0))
                    .columns(
                        egui_extras::Column::initial(160.0)
                            .at_least(60.0)
                            .clip(true),
                        shown_columns.len(),
                    )
                    .header(height, |mut header| {
                        header.col(|ui| {
                            ui.strong(tr("analysis.tree_status"));
                        });
                        for &c in &shown_columns {
                            header.col(|ui| {
                                ui.strong(&table.columns[c]);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(height, rows.len(), |mut table_row| {
                            let item = rows[table_row.index()];
                            table_row.set_selected(self.selected == Some(item.row));
                            table_row.col(|ui| {
                                let (symbol, color) = status_symbol(diff, item.row, ui);
                                ui.colored_label(
                                    color,
                                    format!("{symbol} {}", status_name(diff, item.row)),
                                );
                            });
                            for &c in &shown_columns {
                                table_row.col(|ui| {
                                    let [a, b] = &item.cells[c];
                                    let text = match (a, b) {
                                        (Some(a), Some(b)) if a == b => a.clone(),
                                        (Some(a), Some(b)) => format!("{a} → {b}"),
                                        (Some(a), None) => format!("{a} → —"),
                                        (None, Some(b)) => format!("— → {b}"),
                                        (None, None) => String::new(),
                                    };
                                    if a != b {
                                        ui.colored_label(widgets::pal(ui).warning, text);
                                    } else {
                                        ui.label(text);
                                    }
                                });
                            }
                            if table_row.response().clicked() {
                                clicked = Some(item.row);
                            }
                        });
                    });
            });
        if let Some(r) = clicked {
            self.select(diff, r);
        }
    }
}

/// Names text field `response` for screen readers.
fn labeled(response: &egui::Response, label: &str, text: &str) {
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::text_edit(true, text, text, "");
        info.label = Some(label.into());
        info
    });
}

const GUTTER: f32 = 28.0;
const INDENT: f32 = 14.0;

fn status_name(diff: &TreeDiff, r: usize) -> &'static str {
    let row = &diff.rows[r];
    tr(match row.kind {
        RowKind::Added => "analysis.added",
        RowKind::Removed => "analysis.removed",
        RowKind::Modified if diff.type_changed(r) => "analysis.type_changed",
        RowKind::Modified => "analysis.modified",
        RowKind::Same if row.moved.is_some() => "analysis.moved",
        RowKind::Same if row.changes > 0 => "analysis.tree_contains",
        RowKind::Same => "analysis.tree_same",
    })
}

fn status_symbol(diff: &TreeDiff, r: usize, ui: &Ui) -> (String, Color32) {
    let row = &diff.rows[r];
    let pal = widgets::pal(ui);
    let mut symbol = match row.kind {
        _ if row.within => String::new(),
        RowKind::Added => "+".into(),
        RowKind::Removed => "−".into(),
        RowKind::Modified => "~".into(),
        RowKind::Same if row.moved.is_some() => "↕".into(),
        RowKind::Same if row.changes > 0 => "•".into(),
        RowKind::Same => String::new(),
    };
    if row.moved.is_some() && row.kind != RowKind::Same {
        symbol.push('↕');
    }
    if row.ambiguous {
        symbol.push('?');
    }
    let color = if row.ambiguous {
        pal.warning
    } else {
        pal.text_dim
    };
    (symbol, color)
}

/// The policy line of the Tree view of XML, or of JSON, YAML and TOML.
pub fn policy_key(values: bool) -> &'static str {
    if values {
        "analysis.tree_policy_values"
    } else {
        "analysis.tree_policy"
    }
}

/// How repeated elements, or the objects of an array, are named.
fn shown_name(diff: &TreeDiff, display: &str) -> String {
    if diff.docs[0].values {
        display.to_owned()
    } else {
        format!("<{display}>")
    }
}

/// A short description of one side of row `r`.
fn node_label(diff: &TreeDiff, s: usize, r: usize) -> String {
    match diff.rows[r].sides[s] {
        None => String::new(),
        Some(Side::Attr(n, a)) => {
            let attr = &diff.docs[s].nodes[n].attrs[a];
            format!("@{} = {}", attr.display, quoted(&shorten(&attr.value, 80)))
        }
        Some(Side::Node(n)) => {
            let node = &diff.docs[s].nodes[n];
            match node.kind {
                XKind::Element => format!("<{}>", node.display),
                XKind::Text => quoted(&shorten(&node.value, 80)),
                XKind::Comment => format!("<!--{}-->", shorten(&node.value, 80)),
                XKind::Pi => format!("<?{}?>", shorten(&node.value, 80)),
                XKind::Object => format!("{} {{{}}}", node.display, node.children.len()),
                XKind::Array => format!("{} [{}]", node.display, node.children.len()),
                XKind::Value => format!("{}: {}", node.display, shorten(&node.value, 80)),
            }
        }
    }
}

/// `value` in quotes, with line breaks and tabs shown.
fn quoted(value: &str) -> String {
    let shown: String = value
        .chars()
        .map(|c| match c {
            '\n' => '↵',
            '\r' => '␍',
            '\t' => '⇥',
            c => c,
        })
        .collect();
    format!("\"{shown}\"")
}

fn shorten(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

/// At most 12 lines of the source where side `s` of row `r` is written, with line numbers and
/// highlighting, read from the text the comparison was made from.
fn excerpt(result: &TreeResult, s: usize, r: usize, dark: bool) -> Vec<Excerpt> {
    let diff = &result.diff;
    let Some((_, line, _)) = diff.location(s, r) else {
        return Vec::new();
    };
    let end = match diff.rows[r].sides[s] {
        Some(Side::Node(n)) => diff.docs[s].nodes[n].end_line,
        _ => line,
    };
    let last = end.min(line + 11);
    let lines: Vec<String> = crate::core::text::lines(&result.sources[s])
        .skip(line - 1)
        .take(last + 1 - line)
        .map(|(text, _)| shorten(text, 500))
        .collect();
    let spans = syntax::highlight(&lines, &result.format.label().to_lowercase(), dark);
    lines
        .into_iter()
        .enumerate()
        .map(|(k, text)| Excerpt {
            line: line + k,
            spans: spans.as_ref().and_then(|s| s.get(k).cloned()).filter(|sp| {
                sp.iter().all(|(range, _)| {
                    text.is_char_boundary(range.start)
                        && text.is_char_boundary(range.end)
                        && range.end <= text.len()
                })
            }),
            text,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::semantic::xml_tree::TreeOptions;
    use std::sync::atomic::AtomicBool;

    fn diff(a: &str, b: &str) -> TreeDiff {
        TreeDiff::compare(a, b, &TreeOptions::default(), &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn navigation_visits_each_change_once_each_way_however_folded() {
        let make = |v: usize| {
            let mut s = String::from("<r>");
            for i in 0..100 {
                s.push_str(&format!("<g n='{i}'><h><v>{}</v></h></g>", (i + 1) * v));
            }
            s.push_str("</r>");
            s
        };
        let d = diff(&make(1), &make(2));
        let mut view = XmlTreeView::default();
        view.attach(&d);
        // everything folded
        view.expanded.clear();
        let mut seen = Vec::new();
        while view.step(&d, true) {
            seen.push(view.selected.unwrap());
        }
        assert_eq!(seen, d.targets);
        let mut back = Vec::new();
        view.selected = None;
        while view.step(&d, false) {
            back.push(view.selected.unwrap());
        }
        back.reverse();
        assert_eq!(back, d.targets);
        // the last one reached is shown: its ancestors are expanded
        let mut at = d.rows[d.targets[0]].parent;
        while let Some(p) = at {
            assert!(view.expanded.contains(&p));
            at = d.rows[p].parent;
        }
        view.update_visible(&d);
        assert!(view.visible.contains(&d.targets[0]));
    }

    #[test]
    fn filters_limit_navigation_and_folding_keeps_the_selection() {
        let d = diff("<r><a>1</a><b x='1'/></r>", "<r><a>2</a><b x='1'/><c/></r>");
        let mut view = XmlTreeView::default();
        view.attach(&d);
        view.filter = Some(Filter::Added);
        assert!(view.step(&d, true));
        let added = view.selected.unwrap();
        assert_eq!(d.rows[added].kind, RowKind::Added);
        assert!(!view.step(&d, true));
        assert_eq!(view.selected, Some(added));
        view.update_visible(&d);
        assert!(view.visible.iter().all(|&r| view.filtered[r] > 0));
        view.filter = None;
        view.expanded.clear();
        view.expansion += 1;
        view.update_visible(&d);
        assert_eq!(view.selected, Some(added));
    }

    #[test]
    fn a_new_comparison_finds_the_selection_again_by_path_or_says_it_is_gone() {
        let d = diff("<r><a>1</a></r>", "<r><a>2</a></r>");
        let mut view = XmlTreeView::default();
        view.edge(&d, true);
        let text = view.selected.unwrap();
        let again = diff("<r><z/><a>1</a></r>", "<r><a>3</a></r>");
        view.attach(&again);
        let found = view.selected.expect("found by path");
        assert_eq!(again.path(1, found), d.path(1, text));
        let gone = diff("<q/>", "<q/>");
        view.attach(&gone);
        assert!(view.selected.is_none());
        assert!(view.message.is_some());
    }

    #[test]
    fn search_shows_matches_with_their_ancestors() {
        let d = diff(
            "<r><a><b id='x'/></a><c/></r>",
            "<r><a><b id='y'/></a><c/></r>",
        );
        let mut view = XmlTreeView::default();
        view.attach(&d);
        view.search = "//b".into();
        view.update_visible(&d);
        assert_eq!(view.visible.len(), 3, "r, a and b");
        view.search.clear();
        view.update_visible(&d);
        assert!(view.visible.len() > 3);
    }

    #[test]
    fn excerpts_are_the_source_as_written() {
        let a = "<r>\n  <a\n    x='1'/>\n</r>\n";
        let result = TreeResult {
            diff: diff(a, "<r/>"),
            sources: [a.into(), "<r/>".into()],
            format: Format::Xml,
        };
        let removed = *result
            .diff
            .targets
            .iter()
            .find(|&&t| {
                result
                    .diff
                    .node(0, t)
                    .is_some_and(|n| n.kind == XKind::Element)
            })
            .unwrap();
        let lines = excerpt(&result, 0, removed, false);
        let text: Vec<_> = lines.iter().map(|e| (e.line, e.text.as_str())).collect();
        assert!(
            text.contains(&(2, "  <a")) && text.contains(&(3, "    x='1'/>")),
            "{text:?}"
        );

        let (a, b) = ("{\n  \"n\": [1,\n    2]\n}", "{\"n\": [1]}");
        let options = TreeOptions::default();
        let result = TreeResult {
            diff: TreeDiff::compare_as(a, b, Format::Json, &options, &AtomicBool::new(false))
                .unwrap(),
            sources: [a.into(), b.into()],
            format: Format::Json,
        };
        let row = result.diff.rows[result.diff.top[0]].children[0];
        let lines = excerpt(&result, 0, row, false);
        let text: Vec<String> = lines.iter().map(|e| e.text.clone()).collect();
        assert_eq!(text, ["  \"n\": [1,", "    2]"], "from the key on");
        let spans: Vec<_> = lines.into_iter().map(|e| e.spans).collect();
        let json = syntax::highlight(&text, "json", false).unwrap();
        assert_ne!(Some(&json), syntax::highlight(&text, "xml", false).as_ref());
        assert_eq!(spans, json.into_iter().map(Some).collect::<Vec<_>>());
    }
}
