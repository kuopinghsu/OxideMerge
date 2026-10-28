//! Help & Shortcuts dialog, opened with F1, `?`, the Help menu, or the help button in the header.

use crate::i18n::{tr, trf};
use crate::ui::commands::{EXIT_KEY, REPLACE_KEY};
use crate::ui::{about, theme, widgets};
use egui::{FontId, Key, Margin, Modifiers, RichText, Stroke, Vec2};
use std::path::Path;

/// Inputs that do the same thing, and the language key of what they do.
type Shortcut = (&'static [&'static str], &'static str);
/// Language key of a heading, and its rows.
type Section = (&'static str, &'static [Shortcut]);

const KEYBOARD: &[Section] = &[
    (
        "help.general",
        &[
            (&["F1", "?"], "help.toggle"),
            (&["Ctrl+K", "Ctrl+Shift+P"], "palette.title"),
            (&["Ctrl+N"], "menu.new"),
            (&["Ctrl+O"], "help.open"),
            (&["Ctrl+S"], "menu.save"),
            (&["Ctrl+Shift+S"], "btn.save_all"),
            (&["Ctrl+Shift+E"], "menu.export_html"),
            (&["Ctrl+W", "Ctrl+F4"], "help.close_tab"),
            (&["Control+Tab", "Control+Shift+Tab"], "help.next_prev_tab"),
            (&["F5"], "cmd.refresh"),
            (&["Ctrl++", "Ctrl+−"], "help.zoom"),
            (&["Ctrl+0"], "cmd.zoom_reset"),
            (&["Esc"], "help.close_dialog"),
            (&[EXIT_KEY], "menu.exit"),
        ],
    ),
    (
        "help.differences",
        &[
            (&["Alt+↓", "Alt+↑"], "help.next_prev_diff"),
            (&["Alt+Home", "Alt+End"], "help.first_last_diff"),
            (&["Alt+PgDn", "Alt+PgUp"], "help.next_prev_conflict"),
            (&["Alt+→", "Alt+←"], "help.copy_right_left"),
            (&["Ctrl+Alt+→", "Ctrl+Alt+←"], "help.copy_all_right_left"),
            (&["Ctrl+Z"], "cmd.undo"),
            (&["Ctrl+Y", "Ctrl+Shift+Z"], "cmd.redo"),
        ],
    ),
    (
        "help.text_compare",
        &[
            (&["↑ ↓", "PgUp PgDn"], "help.move_line"),
            (&["Ctrl+Home", "Ctrl+End"], "help.first_last_line"),
            (&["← →"], "help.switch_pane"),
            (&["Shift+↑ ↓", "Shift+PgUp PgDn"], "help.select_lines"),
            (&["Ctrl+C"], "help.copy_lines"),
            (&["Ctrl+V"], "help.paste"),
            (&["F2", "Enter"], "cmd.edit_line"),
            (&["Ctrl+F2"], "cmd.toggle_bookmark"),
            (&["F4", "Shift+F4"], "help.next_prev_bookmark"),
            (&["Alt+S"], "help.sync_point"),
            (&["Ctrl+]", "Ctrl+["], "help.indent_outdent"),
            (&["Ctrl+F"], "cmd.find"),
            (&[REPLACE_KEY], "cmd.replace"),
            (&["F3", "Shift+F3"], "help.find_next_prev"),
            (&["Ctrl+G"], "dlg.goto"),
            (&["Alt+C"], "cmd.collapse_unchanged"),
            (&["Alt+Z"], "cmd.word_wrap"),
            (&["Esc"], "help.close_find"),
        ],
    ),
    (
        "help.folder_compare",
        &[
            (&["↑ ↓", "PgUp PgDn", "Home End"], "help.move_selection"),
            (&["Shift+↑ ↓"], "help.extend_selection"),
            (&["Ctrl+A"], "help.select_all"),
            (&["Enter"], "help.open_item"),
            (&["Space"], "help.quick_preview"),
            (&["→", "←"], "help.expand_collapse"),
            (&["Alt+→", "Alt+←"], "help.copy_selection"),
        ],
    ),
    (
        "help.binary_compare",
        &[
            (&["Ctrl+F"], "cmd.find"),
            (&["F3", "Shift+F3"], "help.find_next_prev"),
            (&["Ctrl+G"], "cmd.goto_offset"),
        ],
    ),
];

/// Gestures are language keys; a `Ctrl+` or `Shift+` prefix is kept as is.
const MOUSE: &[Section] = &[
    (
        "help.window",
        &[
            (&["gesture.drop"], "help.drop"),
            (&["gesture.middle_click"], "help.close_tab"),
            (&["gesture.right_click"], "help.tab_menu"),
        ],
    ),
    (
        "help.text_compare",
        &[
            (&["gesture.click"], "help.select_line"),
            (
                &["gesture.drag", "Shift+gesture.click"],
                "help.select_lines",
            ),
            (&["gesture.double_click"], "cmd.edit_line"),
            (&["gesture.right_click"], "help.line_menu"),
            (&["gesture.click"], "help.expand_fold"),
            (&["gesture.wheel"], "help.scroll"),
            (&["gesture.click", "gesture.drag"], "help.location_pane"),
        ],
    ),
    (
        "help.folder_compare",
        &[
            (&["gesture.click"], "help.select_item"),
            (&["Ctrl+gesture.click"], "help.toggle_selection"),
            (&["Shift+gesture.click"], "help.select_range"),
            (&["gesture.double_click"], "help.open_item"),
            (&["gesture.right_click"], "help.item_menu"),
            (&["gesture.click"], "help.sort"),
        ],
    ),
    (
        "help.image_compare",
        &[
            (&["gesture.wheel", "gesture.pinch"], "help.zoom_pointer"),
            (&["gesture.drag", "gesture.two_finger"], "help.pan"),
            (&["gesture.click"], "help.select_region"),
            (&["gesture.drag"], "help.swipe"),
        ],
    ),
    (
        "help.binary_compare",
        &[
            (&["gesture.click"], "help.select_byte"),
            (&["gesture.wheel"], "help.scroll"),
            (&["gesture.click", "gesture.drag"], "help.location_pane"),
        ],
    ),
];

const KEYS: f32 = 196.0;
const ACTION: f32 = 196.0;
const GAP: f32 = 12.0;
const PANEL_MARGIN: i8 = 14;
/// Space between the dialog edge and the panels.
const MARGIN: i8 = 16;
/// Outer width of one panel, including its margin and outline.
const PANEL: f32 = KEYS + GAP + ACTION + 2.0 * PANEL_MARGIN as f32 + 2.0;

/// Draws the dialog with the settings file at the bottom. Returns true once it should close.
pub fn show(ctx: &egui::Context, settings: Option<&Path>) -> bool {
    let mut close = !crate::ui::widgets::typing(ctx)
        && ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::F1)
                || i.consume_key(Modifiers::NONE, Key::Questionmark)
        });
    let screen = ctx.screen_rect();
    let side_by_side = screen.width() >= 2.0 * PANEL + GAP + 120.0;
    let content = if side_by_side {
        2.0 * PANEL + GAP
    } else {
        PANEL
    };
    let max_height = (screen.height() - 160.0).max(200.0);
    let response = egui::Modal::new(egui::Id::new("help_dialog"))
        .frame(about::dialog_frame(ctx))
        .show(ctx, |ui| {
            ui.set_width(content + 2.0 * MARGIN as f32 + 4.0);
            about::title_bar(ui, tr("help.title"), &mut close);
            egui::Frame::new()
                .inner_margin(Margin::same(MARGIN))
                .show(ui, |ui| {
                    // A modal is laid out in the size it had the frame before, so without a
                    // minimum it would grow a little each frame instead of opening at full size.
                    egui::ScrollArea::vertical()
                        .max_height(max_height)
                        .min_scrolled_height(max_height)
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::splat(GAP);
                                if side_by_side {
                                    panel(ui, tr("help.keyboard"), KEYBOARD, false);
                                    panel(ui, tr("help.mouse"), MOUSE, true);
                                } else {
                                    ui.vertical(|ui| {
                                        panel(ui, tr("help.keyboard"), KEYBOARD, false);
                                        panel(ui, tr("help.mouse"), MOUSE, true);
                                    });
                                }
                                // Room for the scroll bar.
                                ui.add_space(4.0);
                            });
                        });
                    if let Some(path) = settings {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(trf(
                                "help.settings_file",
                                &[("path", path.display().to_string())],
                            ))
                            .size(11.5)
                            .color(widgets::pal(ui).text_dim),
                        );
                    }
                });
        });
    close || response.should_close()
}

/// Outlined box with an upper-case `title` and one block per section.
fn panel(ui: &mut egui::Ui, title: &str, sections: &[Section], mouse: bool) {
    let p = widgets::pal(ui);
    egui::Frame::new()
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(8)
        .inner_margin(Margin::same(PANEL_MARGIN))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(KEYS + GAP + ACTION);
                ui.spacing_mut().item_spacing = Vec2::new(GAP, 6.0);
                ui.spacing_mut().interact_size.y = 18.0;
                ui.label(
                    RichText::new(title.to_uppercase())
                        .font(theme::semibold(12.0))
                        .color(p.text_dim),
                );
                for (heading, rows) in sections {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(tr(heading))
                            .font(theme::semibold(13.0))
                            .color(p.text),
                    );
                    for (inputs, action) in *rows {
                        row(ui, &input_text(inputs, mouse), tr(action));
                    }
                }
            });
        });
}

fn row(ui: &mut egui::Ui, inputs: &str, action: &str) {
    let text = widgets::pal(ui).text;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(KEYS, 0.0),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                ui.set_width(KEYS);
                keycap(ui, inputs);
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(ACTION, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(ACTION);
                ui.add_space(1.0);
                ui.add(egui::Label::new(RichText::new(action).size(13.0).color(text)).wrap());
            },
        );
    });
}

/// Inputs joined with " / ", as this platform writes shortcuts.
fn input_text(inputs: &[&'static str], mouse: bool) -> String {
    inputs
        .iter()
        .map(|&input| match input.rfind('+') {
            Some(at) if mouse => {
                let (modifiers, gesture) = input.split_at(at + 1);
                widgets::key_text(&format!("{modifiers}{}", tr(gesture)))
            }
            _ if mouse => tr(input).to_string(),
            _ => widgets::key_text(input),
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Key cap in the accent color. Latin text uses the monospace font, which has no CJK glyphs,
/// so translated gestures in those scripts stay proportional.
fn keycap(ui: &mut egui::Ui, text: &str) {
    let latin = text.chars().all(|c| (c as u32) < 0x2E80);
    let font = if latin {
        FontId::monospace(11.5)
    } else {
        FontId::proportional(12.0)
    };
    let p = widgets::pal(ui);
    egui::Frame::new()
        .fill(p.control)
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(font).color(p.accent))
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_translated() {
        let keys = KEYBOARD
            .iter()
            .chain(MOUSE)
            .flat_map(|(heading, rows)| {
                let gestures = MOUSE
                    .iter()
                    .flat_map(|(_, rows)| rows.iter().flat_map(|(inputs, _)| inputs.iter()))
                    .map(|g| g.rsplit('+').next().unwrap());
                std::iter::once(*heading)
                    .chain(rows.iter().map(|(_, action)| *action))
                    .chain(gestures)
            })
            .collect::<Vec<_>>();
        let en: serde_json::Value =
            serde_json::from_str(include_str!("../../lang/en.json")).unwrap();
        for k in keys.into_iter().chain([
            "help.title",
            "help.keyboard",
            "help.mouse",
            "help.settings_file",
        ]) {
            assert!(en.get(k).is_some(), "missing translation key {k}");
        }
    }
}
