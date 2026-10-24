//! Command palette: one search field over commands, recent comparisons and settings.

use crate::i18n::tr;
use crate::ui::icons;
use crate::ui::widgets::{self, Icon};
use egui::text::LayoutJob;
use egui::{pos2, vec2, Align2, FontId, Key, Modifiers, Rect, Sense, TextFormat};

pub struct Entry<A> {
    pub icon: Icon,
    pub label: String,
    /// Shortcut or category, shown dimmed on the right.
    pub detail: String,
    pub action: A,
}

pub enum Outcome<A> {
    Keep,
    Close,
    Pick(A),
}

pub struct Palette {
    query: String,
    selected: usize,
    focus: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            query: String::new(),
            selected: 0,
            focus: true,
        }
    }
}

/// Matches `query` as a subsequence of `text`, ignoring case and spaces in the query.
/// Returns a score (higher is better) and the matched character positions.
pub fn fuzzy_match(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold)
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut pos = Vec::with_capacity(q.len());
    let mut score = 0;
    let mut prev: Option<char> = None;
    for (i, c) in text.chars().enumerate() {
        if pos.len() < q.len() && fold(c) == q[pos.len()] {
            score += 1;
            if pos.last().is_some_and(|&p| p + 1 == i) {
                score += 5;
            }
            if prev.is_none_or(|p| !p.is_alphanumeric()) {
                score += 8;
            }
            pos.push(i);
        }
        prev = Some(c);
    }
    (pos.len() == q.len()).then(|| (score - pos[0] as i32, pos))
}

impl Palette {
    pub fn show<A: Clone>(&mut self, ctx: &egui::Context, entries: &[Entry<A>]) -> Outcome<A> {
        let mut matches: Vec<(i32, usize, Vec<usize>)> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| fuzzy_match(&self.query, &e.label).map(|(s, p)| (s, i, p)))
            .collect();
        matches.sort_by_key(|m| std::cmp::Reverse(m.0));
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let mut moved = false;
        let mut enter = false;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) && !matches.is_empty() {
                self.selected = (self.selected + 1) % matches.len();
                moved = true;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) && !matches.is_empty() {
                self.selected = (self.selected + matches.len() - 1) % matches.len();
                moved = true;
            }
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
        });
        let id = egui::Id::new("command_palette");
        let mut picked: Option<usize> = None;
        let modal = egui::Modal::new(id)
            .area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 90.0)))
            .show(ctx, |ui| {
                let width = 600.0_f32.min(ctx.screen_rect().width() - 60.0);
                ui.set_width(width);
                let p = widgets::pal(ui);
                let field = widgets::search_edit(ui, &mut self.query, tr("palette.hint"), width);
                if self.focus {
                    field.request_focus();
                    self.focus = false;
                }
                if field.changed() {
                    self.selected = 0;
                }
                ui.add_space(6.0);
                if matches.is_empty() {
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(tr("palette.no_results")).color(p.text_dim));
                    });
                    ui.add_space(8.0);
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (k, (_, i, pos)) in matches.iter().enumerate() {
                            let r = entry_row(ui, &entries[*i], pos, k == self.selected, &p);
                            if moved && k == self.selected {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                picked = Some(*i);
                            }
                        }
                    });
            });
        if enter {
            picked = matches.get(self.selected).map(|m| m.1);
        }
        if let Some(i) = picked {
            return Outcome::Pick(entries[i].action.clone());
        }
        if modal.should_close() {
            return Outcome::Close;
        }
        Outcome::Keep
    }
}

fn entry_row<A>(
    ui: &mut egui::Ui,
    e: &Entry<A>,
    pos: &[usize],
    selected: bool,
    p: &crate::ui::theme::Palette,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, &e.label)
    });
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 6.0, p.accent_soft);
    } else if resp.hovered() {
        painter.rect_filled(rect, 6.0, p.control_hover);
    }
    let cy = rect.center().y;
    icons::paint(
        &painter,
        Rect::from_center_size(pos2(rect.left() + 18.0, cy), vec2(16.0, 16.0)),
        e.icon,
        if selected { p.accent } else { p.text_dim },
    );
    let detail =
        ui.fonts(|f| f.layout_no_wrap(e.detail.clone(), FontId::proportional(12.5), p.text_dim));
    let detail_x = rect.right() - 10.0 - detail.size().x;
    painter.galley(
        pos2(detail_x, cy - detail.size().y / 2.0),
        detail,
        p.text_dim,
    );

    let font = FontId::proportional(14.0);
    let mut job = LayoutJob::default();
    let mut run = String::new();
    let mut run_hit = false;
    let flush = |job: &mut LayoutJob, run: &mut String, hit: bool| {
        if !run.is_empty() {
            job.append(
                run,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: if hit { p.accent } else { p.text },
                    ..Default::default()
                },
            );
            run.clear();
        }
    };
    for (i, ch) in e.label.chars().enumerate() {
        let hit = pos.binary_search(&i).is_ok();
        if hit != run_hit {
            flush(&mut job, &mut run, run_hit);
            run_hit = hit;
        }
        run.push(ch);
    }
    flush(&mut job, &mut run, run_hit);
    let label = ui.fonts(|f| f.layout_job(job));
    let clip = Rect::from_min_max(rect.min, pos2(detail_x - 12.0, rect.max.y));
    painter.with_clip_rect(clip).galley(
        pos2(rect.left() + 36.0, cy - label.size().y / 2.0),
        label,
        p.text,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::fuzzy_match;

    #[test]
    fn subsequence_matches_ignore_case_and_spaces() {
        assert!(fuzzy_match("cr", "Copy to Right").is_some());
        assert!(fuzzy_match("copy right", "Copy to Right").is_some());
        assert!(fuzzy_match("xyz", "Copy to Right").is_none());
        assert_eq!(fuzzy_match("", "anything").map(|m| m.0), Some(0));
    }

    #[test]
    fn word_starts_and_runs_score_higher() {
        let score = |q, t| fuzzy_match(q, t).unwrap().0;
        assert!(score("nd", "Next Difference") > score("nd", "Undo"));
        assert!(score("find", "Find") > score("find", "Refined"));
    }
}
