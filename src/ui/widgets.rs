//! Shared widgets: buttons, toolbars, chips, segmented controls, scrollbars and the location pane.

use crate::ui::icons;
pub use crate::ui::icons::Icon;
use crate::ui::theme::{self, Palette};
use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Rect, Response, Role, Sense, Stroke, StrokeKind,
    Ui, Vec2, WidgetInfo,
};

pub fn pal(ui: &Ui) -> Palette {
    theme::palette(ui.visuals().dark_mode)
}

fn modal_id() -> egui::Id {
    egui::Id::unique("oxidemerge_modal_open")
}

/// Marks whether an application dialog is open, so views ignore keys meant for the dialog.
pub fn set_modal_open(ctx: &egui::Context, open: bool) {
    ctx.data_mut(|d| d.insert_temp(modal_id(), open));
}

pub fn modal_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(modal_id()).unwrap_or(false))
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Ghost,
    Primary,
    /// Bordered, for secondary dialog actions.
    Secondary,
    Toggle(bool),
    Segment(bool),
}

const ICON_SIZE: f32 = 16.0;

/// Formats a tooltip with the keyboard shortcut that does the same thing, written like the
/// Help & Shortcuts dialog writes it.
pub fn with_key(tip: &str, key: &str) -> String {
    format!("{tip}  ({})", key_text(key))
}

/// A shortcut written like "Ctrl+Shift+S", as this platform shows it. Ctrl stands for Command on
/// macOS, which writes modifiers as symbols in its own order: ⇧⌘S. "Control+" is the Control key
/// on every platform.
pub fn key_text(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        mac_keys(keys)
    } else {
        keys.replace("Control+", "Ctrl+")
    }
}

fn mac_keys(keys: &str) -> String {
    let mut rest = keys;
    let mut held = [false; 4];
    'strip: loop {
        for (i, prefix) in ["Control+", "Alt+", "Shift+", "Ctrl+"]
            .into_iter()
            .enumerate()
        {
            if let Some(r) = rest.strip_prefix(prefix).filter(|r| !r.is_empty()) {
                held[i] = true;
                rest = r;
                continue 'strip;
            }
        }
        break;
    }
    let mut out: String = ['⌃', '⌥', '⇧', '⌘']
        .into_iter()
        .zip(held)
        .filter_map(|(symbol, on)| on.then_some(symbol))
        .collect();
    out.push_str(rest);
    out
}

/// Whether a text field has keyboard focus, so keys type rather than run commands.
pub fn typing(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some())
}

/// Accent outline around a widget that has keyboard focus (focus only moves with Tab).
pub fn focus_ring(ui: &Ui, resp: &Response, rect: Rect, radius: u8) {
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            radius as f32 + 1.5,
            Stroke::new(2.0_f32, pal(ui).accent),
            StrokeKind::Outside,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_button(
    ui: &mut Ui,
    icon: Option<Icon>,
    text: &str,
    a11y_label: &str,
    font: FontId,
    min: Vec2,
    look: Look,
    enabled: bool,
) -> Response {
    let p = pal(ui);
    let fg = match look {
        _ if !enabled => p.text_disabled,
        Look::Primary => p.on_accent,
        Look::Toggle(true) => p.accent,
        _ => p.text,
    };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let text_w = if text.is_empty() {
        0.0
    } else {
        galley.size().x
    };
    let icon_w = match (icon.is_some(), text.is_empty()) {
        (false, _) => 0.0,
        (true, true) => ICON_SIZE,
        (true, false) => ICON_SIZE + 6.0,
    };
    let size = Vec2::new(
        (icon_w + text_w + 20.0).max(min.x),
        min.y.max(galley.size().y + 8.0),
    );
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| match look {
        Look::Toggle(on) => WidgetInfo::selected(Role::CheckBox, enabled, on, a11y_label),
        Look::Segment(on) => WidgetInfo::selected(Role::RadioButton, enabled, on, a11y_label),
        _ => WidgetInfo::labeled(Role::Button, enabled, a11y_label),
    });
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let (fill, stroke) = match look {
            Look::Primary => (
                if !enabled {
                    p.accent.gamma_multiply(0.45)
                } else if pressed {
                    p.accent_pressed
                } else if hovered {
                    p.accent_hover
                } else {
                    p.accent
                },
                Stroke::NONE,
            ),
            Look::Secondary => (
                if pressed {
                    p.control_active
                } else if hovered {
                    p.control_hover
                } else {
                    p.surface
                },
                Stroke::new(1.0_f32, p.border_strong),
            ),
            Look::Toggle(true) => (
                p.accent_soft.lerp_to_gamma(
                    p.accent,
                    match (pressed, hovered) {
                        (true, _) => 0.3,
                        (_, true) => 0.15,
                        _ => 0.0,
                    },
                ),
                Stroke::NONE,
            ),
            Look::Segment(true) => (
                if ui.visuals().dark_mode {
                    p.control_active
                } else {
                    p.surface
                },
                Stroke::new(1.0_f32, p.border),
            ),
            _ if pressed => (p.control_active, Stroke::NONE),
            _ if hovered => (p.control_hover, Stroke::NONE),
            _ => (Color32::TRANSPARENT, Stroke::NONE),
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(theme::radius::MD),
            fill,
            stroke,
            StrokeKind::Inside,
        );
        let left = rect.center().x - (icon_w + text_w) / 2.0;
        if let Some(icon) = icon {
            let r = Rect::from_center_size(
                egui::pos2(left + ICON_SIZE / 2.0, rect.center().y),
                Vec2::splat(ICON_SIZE),
            );
            icons::paint(ui.painter(), r, icon, fg);
        }
        if !text.is_empty() {
            let pos = egui::pos2(left + icon_w, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(pos, galley, fg);
        }
        focus_ring(ui, &resp, rect, theme::radius::MD);
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Non-interactive icon in the flow of a layout, for example before a label.
pub fn inline_icon(ui: &mut Ui, icon: Icon, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(ICON_SIZE), Sense::hover());
    icons::paint(ui.painter(), rect, icon, color);
    resp
}

/// Icon at caption size in the flow of a layout, such as a sort or priority mark.
pub fn small_icon(ui: &mut Ui, icon: Icon, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    icons::paint(ui.painter(), rect, icon, color);
    resp
}

/// Frameless icon button the size of a row's text, such as a tree disclosure; `label` names it
/// for assistive technologies.
pub fn mini_icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        if hovered {
            ui.painter()
                .rect_filled(rect, theme::radius::XS, p.control_hover);
        }
        let fg = if hovered { p.text } else { p.text_dim };
        icons::paint(ui.painter(), rect.shrink(2.5), icon, fg);
        focus_ring(ui, &resp, rect, theme::radius::XS);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Square toolbar button showing an icon; highlights on hover.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    icon_button_enabled(ui, true, icon, tip)
}

pub fn icon_button_enabled(ui: &mut Ui, enabled: bool, icon: Icon, tip: &str) -> Response {
    paint_button(
        ui,
        Some(icon),
        "",
        tip,
        FontId::proportional(theme::size::CONTROL),
        Vec2::new(30.0, 28.0),
        Look::Ghost,
        enabled,
    )
    .on_hover_text(tip)
}

/// [`icon_button`] whose tooltip adds `hint`, dimmed, below `tip`; it is named by `tip` alone.
pub fn icon_button_hint(ui: &mut Ui, icon: Icon, tip: &str, hint: &str) -> Response {
    paint_button(
        ui,
        Some(icon),
        "",
        tip,
        FontId::proportional(theme::size::CONTROL),
        Vec2::new(30.0, 28.0),
        Look::Ghost,
        true,
    )
    .on_hover_ui(|ui| {
        ui.set_max_width(340.0);
        ui.label(tip);
        ui.weak(hint);
    })
}

/// Frameless text button that highlights on hover.
pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    ghost_button_named(ui, label, label)
}

/// [`ghost_button`] whose short label needs a fuller name for assistive technologies.
pub fn ghost_button_named(ui: &mut Ui, label: &str, name: &str) -> Response {
    paint_button(
        ui,
        None,
        label,
        name,
        FontId::proportional(theme::size::CONTROL),
        Vec2::new(0.0, 28.0),
        Look::Ghost,
        true,
    )
}

/// Frameless button with an icon before its label.
pub fn ghost_icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    paint_button(
        ui,
        Some(icon),
        label,
        label,
        FontId::proportional(theme::size::CONTROL),
        Vec2::new(0.0, 28.0),
        Look::Ghost,
        true,
    )
}

/// Compact button for a bar of actions; accent-filled when `primary`.
pub fn bar_button(
    ui: &mut Ui,
    enabled: bool,
    icon: Option<Icon>,
    label: &str,
    primary: bool,
) -> Response {
    let look = if primary { Look::Primary } else { Look::Ghost };
    let size = Vec2::new(0.0, 26.0);
    paint_button(
        ui,
        icon,
        label,
        label,
        FontId::proportional(theme::size::DENSE),
        size,
        look,
        enabled,
    )
}

/// Accent-filled call-to-action button.
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    paint_button(
        ui,
        None,
        label,
        label,
        theme::semibold(theme::size::BODY),
        Vec2::new(96.0, 32.0),
        Look::Primary,
        true,
    )
}

/// Neutral bordered button for secondary dialog actions.
pub fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    paint_button(
        ui,
        None,
        label,
        label,
        FontId::proportional(theme::size::BODY),
        Vec2::new(88.0, 32.0),
        Look::Secondary,
        true,
    )
}

/// Button that stays highlighted while `on` is true.
pub fn toggle_button(ui: &mut Ui, on: &mut bool, label: &str, tip: &str) -> Response {
    toggle_icon_button(ui, on, None, label, tip)
}

/// Toggle showing an icon, optionally followed by a label.
pub fn toggle_icon_button(
    ui: &mut Ui,
    on: &mut bool,
    icon: Option<Icon>,
    label: &str,
    tip: &str,
) -> Response {
    // named by the tip without the key `with_key` adds to it
    let name = if label.is_empty() {
        tip.rsplit_once("  (").map_or(tip, |(name, _)| name)
    } else {
        label
    };
    let r = paint_button(
        ui,
        icon,
        label,
        name,
        FontId::proportional(theme::size::CONTROL),
        Vec2::new(30.0, 28.0),
        Look::Toggle(*on),
        true,
    )
    .on_hover_text(tip);
    if r.clicked() {
        *on = !*on;
    }
    r
}

/// Pill-shaped group of mutually exclusive options.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    egui::Frame::new()
        .fill(p.control)
        .corner_radius(theme::radius::LG)
        .inner_margin(Margin::same(theme::space::XXS))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (v, label) in options {
                    let sel = *value == *v;
                    if paint_button(
                        ui,
                        None,
                        label,
                        label,
                        FontId::proportional(theme::size::DENSE),
                        Vec2::new(0.0, 24.0),
                        Look::Segment(sel),
                        true,
                    )
                    .clicked()
                        && !sel
                    {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A button drawn like a search field, for opening a search such as the command palette:
/// an icon, `hint`, and the shortcut `keys` at the right end. Named `label` for accessibility.
pub fn search_button(ui: &mut Ui, width: f32, hint: &str, keys: &str, label: &str) -> Response {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();
    painter.rect(
        rect,
        theme::radius::MD,
        if resp.is_pointer_button_down_on() {
            p.control_active
        } else if hovered {
            p.control_hover
        } else {
            p.control
        },
        Stroke::new(1.0_f32, if hovered { p.border_strong } else { p.border }),
        egui::StrokeKind::Inside,
    );
    let icon = Rect::from_center_size(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        Vec2::splat(13.0),
    );
    icons::paint(painter, icon, Icon::Search, p.text_dim);
    painter.text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        hint,
        FontId::proportional(theme::size::SMALL),
        p.text_dim,
    );
    if !keys.is_empty() {
        let galley = painter.layout_no_wrap(
            key_text(keys),
            FontId::proportional(theme::size::CAPTION),
            p.text_dim,
        );
        let key = Rect::from_min_size(
            egui::pos2(rect.right() - galley.size().x - 16.0, rect.center().y - 9.0),
            Vec2::new(galley.size().x + 10.0, 18.0),
        );
        painter.rect_stroke(
            key,
            theme::radius::XS,
            Stroke::new(1.0_f32, p.border),
            egui::StrokeKind::Inside,
        );
        painter.galley(
            egui::pos2(key.left() + 5.0, key.center().y - galley.size().y / 2.0),
            galley,
            p.text_dim,
        );
    }
    focus_ring(ui, &resp, rect, theme::radius::MD);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Full-width toolbar strip with a bottom border. Controls that do not fit continue on another
/// row, whole.
pub fn toolbar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    let r = egui::Frame::new()
        .fill(p.surface_alt)
        .inner_margin(Margin::symmetric(theme::space::MD, theme::space::XS))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.data_mut(|d| d.insert_temp(ui.scope_id().with("tool_groups"), 0usize));
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.spacing_mut().item_spacing.x = 2.0;
                add(ui)
            })
            .inner
        });
    let rect = r.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0_f32, p.border),
    );
    r.inner
}

/// Lays out `add` from the right end of a toolbar row; on a row of its own when less than
/// `width` is left on this one.
pub fn toolbar_end<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    if ui.max_rect().right() - ui.cursor().min.x < width {
        ui.end_row();
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add)
        .inner
}

/// Related toolbar buttons in a rounded outline, after an optional caption; only inside
/// [`toolbar`].
pub fn tool_group<R>(ui: &mut Ui, caption: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    // A group starts a new toolbar row when it did not fit on this one last time. Groups are
    // told apart by their order in the toolbar, which counts them afresh each frame.
    let order = ui.data_mut(|d| {
        let id = ui.scope_id().with("tool_groups");
        debug_assert!(
            d.get_temp::<usize>(id).is_some(),
            "tool_group outside a toolbar"
        );
        let n = d.get_temp_mut_or_default::<usize>(id);
        *n += 1;
        *n
    });
    let width_id = ui.scope_id().with(("tool_group", order));
    let width: f32 = ui.data(|d| d.get_temp(width_id)).unwrap_or(0.0);
    if ui.max_rect().right() - ui.cursor().min.x < width
        && ui.cursor().min.x > ui.max_rect().min.x + 1.0
    {
        ui.end_row();
    }
    let start = ui.cursor().min.x;
    ui.add_space(4.0);
    if let Some(caption) = caption {
        ui.label(
            egui::RichText::new(caption)
                .size(theme::size::CAPTION)
                .color(p.text_dim),
        );
    }
    let r = egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(theme::radius::MD)
        .inner_margin(Margin::symmetric(theme::space::XXS, 0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                add(ui)
            })
            .inner
        });
    ui.add_space(4.0);
    let used = r.response.rect.right() + 4.0 - start;
    if (used - width).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(width_id, used));
        ui.ctx().request_repaint();
    }
    r.inner
}

pub fn toolbar_sep(ui: &mut Ui) {
    let p = pal(ui);
    let (r, _) = ui.allocate_exact_size(Vec2::new(13.0, 20.0), Sense::hover());
    ui.painter().vline(
        r.center().x,
        r.y_range(),
        Stroke::new(1.0_f32, p.border_strong),
    );
}

/// Small rounded label.
pub fn chip(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(theme::radius::LG)
        .inner_margin(Margin::symmetric(theme::space::MD, 1))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .color(fg)
                        .font(theme::semibold(theme::size::SMALL)),
                )
                .selectable(false),
            );
        })
        .response
}

/// Chip showing a setting and its value, with a chevron; clicking it opens an editor.
/// `show_label` leaves out the caption where space is short.
pub fn chip_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    value: &str,
    show_label: bool,
) -> Response {
    let p = pal(ui);
    let caption = if show_label {
        format!("{label}  ")
    } else {
        String::new()
    };
    let label_g = ui.painter().layout_no_wrap(
        caption,
        FontId::proportional(theme::size::SMALL),
        p.text_dim,
    );
    let value_g = ui.painter().layout_no_wrap(
        value.to_string(),
        theme::semibold(theme::size::SMALL),
        p.text,
    );
    let w = 8.0 + 14.0 + 5.0 + label_g.size().x + value_g.size().x.min(260.0) + 4.0 + 10.0 + 8.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, format!("{label} {value}")));
    if ui.is_rect_visible(rect) {
        let fill = if resp.is_pointer_button_down_on() {
            p.control_active
        } else if resp.hovered() || resp.has_focus() {
            p.control_hover
        } else {
            p.control
        };
        let painter = ui.painter();
        painter.rect_filled(rect, theme::radius::XL, fill);
        let mut x = rect.left() + 8.0;
        let icon_rect =
            Rect::from_min_size(egui::pos2(x, rect.center().y - 7.0), Vec2::splat(14.0));
        icons::paint(painter, icon_rect, icon, p.text_dim);
        x += 14.0 + 5.0;
        let y = rect.center().y;
        let label_w = label_g.size().x;
        painter.galley(
            egui::pos2(x, y - label_g.size().y / 2.0),
            label_g,
            p.text_dim,
        );
        x += label_w;
        let value_rect = Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2(rect.right() - 22.0, rect.bottom()),
        );
        painter.with_clip_rect(value_rect).galley(
            egui::pos2(x, y - value_g.size().y / 2.0),
            value_g,
            p.text,
        );
        let chevron = Rect::from_center_size(egui::pos2(rect.right() - 14.0, y), Vec2::splat(12.0));
        icons::paint(painter, chevron, Icon::Next, p.text_dim);
        focus_ring(ui, &resp, rect, theme::radius::XL);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Single-line text field with a search icon in front and a clear button once it has text.
pub fn search_edit(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let p = pal(ui);
    let margin = Margin {
        left: 28,
        right: 26,
        top: 5,
        bottom: 5,
    };
    let mut resp = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(width)
            .margin(margin),
    );
    // The response covers only the text, inside the margin.
    let r = resp.rect + margin;
    let icon_rect =
        Rect::from_center_size(egui::pos2(r.left() + 15.0, r.center().y), Vec2::splat(14.0));
    icons::paint(ui.painter(), icon_rect, Icon::Search, p.text_dim);
    if !text.is_empty() {
        let clear_rect = Rect::from_center_size(
            egui::pos2(r.right() - 14.0, r.center().y),
            Vec2::splat(18.0),
        );
        let clear = ui
            .interact(clear_rect, resp.id.with("clear"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        clear.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, crate::i18n::tr("btn.clear_search"))
        });
        let color = if clear.hovered() { p.text } else { p.text_dim };
        icons::paint(ui.painter(), clear_rect.shrink(3.0), Icon::Close, color);
        if clear.clicked() {
            text.clear();
            resp.request_focus();
            resp.mark_changed();
        }
    }
    resp
}

/// Tinted strip under a toolbar announcing the state of the compare, e.g. that files are identical.
pub fn info_bar(ui: &mut Ui, icon: Icon, text: &str, color: Color32) {
    let p = pal(ui);
    let r = egui::Frame::new()
        .fill(color.gamma_multiply(if ui.visuals().dark_mode { 0.16 } else { 0.10 }))
        .inner_margin(Margin::symmetric(theme::space::LG, theme::space::SM))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                inline_icon(ui, icon, color);
                ui.add(egui::Label::new(egui::RichText::new(text).color(p.text)).selectable(false));
            });
        });
    let rect = r.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0_f32, p.border),
    );
}

/// Result of the compare shown in a tab, for the status indicator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Busy,
    Identical,
    Different,
    Error,
}

impl Outcome {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Outcome::Busy => p.accent,
            Outcome::Identical => p.success,
            Outcome::Different => p.warning,
            Outcome::Error => p.danger,
        }
    }
}

/// Identity color of a compare side (left / middle / right).
pub fn side_color(ui: &Ui, side: usize, panes: usize) -> Color32 {
    let idx = if panes == 2 && side == 1 { 2 } else { side };
    pal(ui).sides[idx.min(2)]
}

/// Chip naming a compare side in its identity color.
pub fn side_chip(ui: &mut Ui, side: usize, panes: usize) -> Response {
    let c = side_color(ui, side, panes);
    let bg = c.gamma_multiply(if ui.visuals().dark_mode { 0.22 } else { 0.13 });
    chip(ui, crate::core::report::side_name(side, panes), c, bg)
}

/// Rounded, bordered container for grouped content.
pub fn card(ui: &Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(theme::radius::XL)
        .inner_margin(Margin::same(theme::space::XL))
}

/// Side drawer on the right of `ui`, for panels that work beside the main view (review,
/// verification, baselines). With more than one tab a segmented control picks one; `add`
/// draws the picked tab. The rest of `ui` stays for the main view.
pub fn drawer<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: egui::Id,
    tab: &mut T,
    tabs: &[(T, &str)],
    add: impl FnOnce(&mut Ui, T),
) {
    let p = pal(ui);
    if !tabs.iter().any(|(t, _)| t == tab) {
        if let Some((t, _)) = tabs.first() {
            *tab = *t;
        }
    }
    egui::Panel::right(id)
        .resizable(true)
        .default_size(360.0)
        .size_range(280.0..=720.0)
        .frame(
            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0_f32, p.border))
                .inner_margin(Margin::symmetric(theme::space::LG, theme::space::CONTROL)),
        )
        .show(ui, |ui| {
            match tabs {
                [] => return,
                [(_, title)] => {
                    ui.label(egui::RichText::new(*title).font(theme::semibold(theme::size::BODY)));
                }
                _ => {
                    segmented(ui, tab, tabs);
                }
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .auto_shrink([false, false])
                .show(ui, |ui| add(ui, *tab));
        });
}

/// Toolbar toggle that opens and closes the side drawer.
pub fn drawer_toggle(ui: &mut Ui, open: &mut bool) -> Response {
    toggle_icon_button(
        ui,
        open,
        Some(Icon::Sidebar),
        "",
        crate::i18n::tr("drawer.toggle"),
    )
}

/// Draws a vertical scrollbar in `rect`. `scroll` is the first visible row.
pub fn vscrollbar(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    scroll: &mut f32,
    total: usize,
    visible: f32,
) {
    let p = pal(ui);
    let painter = ui.painter_at(rect);
    let total_f = total.max(1) as f32;
    let max_scroll = (total_f - visible).max(0.0);
    let h = rect.height() - 4.0;
    let thumb_h = (visible / total_f * h).max(24.0).min(h.max(0.0));
    let frac = if max_scroll > 0.0 {
        *scroll / max_scroll
    } else {
        0.0
    };
    let thumb_top = rect.top() + 2.0 + frac * (h - thumb_h);
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(Role::ScrollBar, true, crate::i18n::tr("help.scroll")));
    let active = resp.dragged() || resp.hovered();
    let w = if active { 8.0 } else { 5.0 };
    let thumb = Rect::from_min_size(
        egui::pos2(rect.center().x - w / 2.0, thumb_top),
        egui::vec2(w, thumb_h),
    );
    if max_scroll > 0.0 {
        painter.rect_filled(
            thumb,
            w / 2.0,
            if active { p.text_dim } else { p.border_strong },
        );
    }
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let usable = (h - thumb_h).max(1.0);
            let f = ((pos.y - rect.top() - thumb_h / 2.0) / usable).clamp(0.0, 1.0);
            *scroll = (f * max_scroll).round();
        }
    }
}

/// Horizontal scrollbar; `scroll` is in pixels.
pub fn hscrollbar(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    scroll: &mut f32,
    content: f32,
    visible: f32,
) {
    let p = pal(ui);
    let painter = ui.painter_at(rect);
    let max_scroll = (content - visible).max(0.0);
    if max_scroll <= 0.0 {
        *scroll = 0.0;
        return;
    }
    let w = rect.width() - 4.0;
    let thumb_w = (visible / content * w).max(24.0).min(w.max(0.0));
    let thumb_left = rect.left() + 2.0 + (*scroll / max_scroll) * (w - thumb_w);
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| WidgetInfo::labeled(Role::ScrollBar, true, crate::i18n::tr("help.scroll")));
    let active = resp.dragged() || resp.hovered();
    let hgt = if active { 8.0 } else { 5.0 };
    let thumb = Rect::from_min_size(
        egui::pos2(thumb_left, rect.center().y - hgt / 2.0),
        egui::vec2(thumb_w, hgt),
    );
    painter.rect_filled(
        thumb,
        hgt / 2.0,
        if active { p.text_dim } else { p.border_strong },
    );
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f =
                ((pos.x - rect.left() - thumb_w / 2.0) / (w - thumb_w).max(1.0)).clamp(0.0, 1.0);
            *scroll = f * max_scroll;
        }
    }
}

pub struct Mark {
    pub start: usize,
    pub len: usize,
    /// Color of each column of the location bar (one per pane); `None` leaves it empty.
    pub columns: [Option<Color32>; 3],
}

/// Location pane: overview of all differences with the visible region outlined.
/// `marks` must be sorted by `start`. Returns the row the user clicked/dragged to (to be centered).
#[allow(clippy::too_many_arguments)]
pub fn location_pane(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    total: usize,
    columns: usize,
    marks: &[Mark],
    first_visible: f32,
    visible: f32,
    bg: Color32,
    viewport: Color32,
) -> Option<f32> {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, bg);
    let total_f = total.max(1) as f32;
    let inner = rect.shrink2(egui::vec2(3.0, 2.0));
    let h = inner.height();
    let scale = h / total_f;
    let n = columns.clamp(1, 3);
    let col_w = inner.width() / n as f32;
    let paint = |c: usize, (y0, y1, color): (f32, f32, Color32)| {
        let r = Rect::from_min_max(
            egui::pos2(inner.left() + c as f32 * col_w, y0),
            egui::pos2(inner.left() + (c + 1) as f32 * col_w - 1.5, y1),
        );
        painter.rect_filled(r, 1.5, color);
    };
    // Marks closer than a pixel are merged, so large diffs paint at most one rect per pixel row.
    let mut pending: [Option<(f32, f32, Color32)>; 3] = [None; 3];
    for m in marks {
        let y0 = inner.top() + m.start as f32 * scale;
        let y1 = y0 + (m.len as f32 * scale).max(3.0);
        for (c, color) in m.columns.iter().take(n).enumerate() {
            let Some(color) = *color else { continue };
            match &mut pending[c] {
                Some(p) if p.2 == color && y0 <= p.1 + 1.0 => p.1 = p.1.max(y1),
                slot => {
                    if let Some(p) = slot.replace((y0, y1, color)) {
                        paint(c, p);
                    }
                }
            }
        }
    }
    for (c, p) in pending.into_iter().enumerate() {
        if let Some(p) = p {
            paint(c, p);
        }
    }
    let vr = Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, inner.top() + first_visible * scale),
        egui::pos2(
            rect.right() - 1.0,
            inner.top() + ((first_visible + visible) * scale).min(h),
        ),
    );
    painter.rect(
        vr,
        theme::radius::XS,
        viewport.gamma_multiply(0.12),
        Stroke::new(1.0_f32, viewport.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    resp.widget_info(|| {
        WidgetInfo::labeled(Role::ScrollBar, true, crate::i18n::tr("view.location_pane"))
    });
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            return Some(((p.y - inner.top()) / scale).max(0.0));
        }
    }
    None
}

/// App mark: two side-by-side pages on an accent tile.
pub const LOGO_PNG: &[u8] = include_bytes!("../../assets/oxidemerge.png");

/// The application logo as a square RGBA image.
pub fn logo_image() -> Option<image::RgbaImage> {
    image::load_from_memory_with_format(LOGO_PNG, image::ImageFormat::Png)
        .ok()
        .map(|i| i.to_rgba8())
}

pub fn logo(ui: &mut Ui, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let id = egui::Id::unique("oxidemerge-logo");
    let tex = match ui
        .ctx()
        .data(|d| d.get_temp::<Option<egui::TextureHandle>>(id))
    {
        Some(t) => t,
        None => {
            let t = logo_image().map(|img| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                );
                // Mipmaps keep the logo smooth when drawn far below its native size.
                let options = egui::TextureOptions::LINEAR
                    .with_mipmap_mode(Some(egui::TextureFilter::Linear));
                ui.ctx().load_texture("oxidemerge-logo", image, options)
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, t.clone()));
            t
        }
    };
    match tex {
        Some(t) => {
            ui.painter().image(
                t.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            ui.painter().rect_filled(rect, size * 0.22, pal(ui).accent);
        }
    }
    resp
}

/// Full-width sidebar navigation entry.
pub fn nav_item(ui: &mut Ui, selected: bool, icon: Icon, label: &str) -> Response {
    let p = pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
    let fill = if selected {
        p.accent_soft
    } else if resp.is_pointer_button_down_on() {
        p.control_active
    } else if resp.hovered() {
        p.control_hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, theme::radius::MD, fill);
    let fg = if selected { p.accent } else { p.text };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(
            egui::pos2(rect.left() + 20.0, rect.center().y),
            Vec2::splat(ICON_SIZE),
        ),
        icon,
        fg,
    );
    let font = if selected {
        theme::semibold(theme::size::CONTROL)
    } else {
        FontId::proportional(theme::size::CONTROL)
    };
    ui.painter().text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    focus_ring(ui, &resp, rect, theme::radius::MD);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Dialog title row with a close button; returns true when the close button was clicked.
pub fn dialog_title(ui: &mut Ui, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).font(theme::semibold(theme::size::TITLE)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = icon_button(
                ui,
                Icon::Close,
                &with_key(crate::i18n::tr("btn.cancel"), "Esc"),
            )
            .clicked();
        });
    });
    ui.add_space(6.0);
    close
}

/// Right-aligned dialog button row.
pub fn dialog_footer(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        });
    });
}

/// Small dimmed section caption.
pub fn caption(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(
        egui::RichText::new(text)
            .font(theme::semibold(theme::size::SMALL))
            .color(p.text_dim),
    );
}

// ---------- menus ----------
//
// Every pull-down menu (the menu bar's menus and their submenus, context menus and the toolbar's
// drop-downs) opens equally wide, with the same roomy rows, shortcuts kept clear of their labels
// and check marks in a gutter of their own.

/// Narrowest a menu opens.
pub const MENU_MIN_WIDTH: f32 = 220.0;
/// Padding of menu items: rows of about 28 points instead of egui's dense 22.
pub const MENU_ITEM_PADDING: Vec2 = Vec2::new(8.0, 4.0);

fn menu_width_id() -> egui::Id {
    egui::Id::unique("oxidemerge_menu_width")
}

/// Sets the width all menus open with: that of the widest menu of the menu bar, measured with
/// its submenus, and at least [`MENU_MIN_WIDTH`].
pub fn set_menu_width(ctx: &egui::Context, width: f32) {
    let width = width.max(MENU_MIN_WIDTH).ceil();
    ctx.data_mut(|d| d.insert_temp(menu_width_id(), width));
}

/// The width all menus open with (see [`set_menu_width`]).
pub fn menu_width(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp(menu_width_id()))
        .unwrap_or(MENU_MIN_WIDTH)
}

/// Styles the contents of a menu: the shared width and row padding. Called first in every menu.
pub fn menu_setup(ui: &mut Ui) {
    ui.spacing_mut().button_padding = MENU_ITEM_PADDING;
    ui.set_min_width(menu_width(ui.ctx()));
}

/// Room between a menu item's label and its shortcut.
const MENU_SHORTCUT_GAP: f32 = 24.0;
/// Room before a check item's label, where its check mark goes.
const MENU_CHECK_GUTTER: f32 = 20.0;

/// Shortcut text of a menu item, kept clear of its label.
fn menu_shortcut(ui: &Ui, keys: &str) -> egui::WidgetText {
    if keys.is_empty() {
        return egui::WidgetText::default();
    }
    let mut job = egui::text::LayoutJob::default();
    job.append(
        keys,
        MENU_SHORTCUT_GAP,
        egui::TextFormat {
            font_id: egui::TextStyle::Button.resolve(ui.style()),
            // Takes the color the button gives its shortcut.
            color: Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    job.into()
}

/// A menu item with the shortcut `keys` at its right end, named `label` alone for accessibility.
pub fn menu_item(ui: &mut Ui, enabled: bool, label: &str, keys: &str) -> Response {
    let button = egui::Button::new(label).shortcut_text(menu_shortcut(ui, keys));
    let r = ui.add_enabled(enabled, button);
    r.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    r
}

/// A submenu of a context menu or drop-down, styled like the menu it opens from.
pub fn submenu<R>(
    ui: &mut Ui,
    title: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    ui.menu_button(title, |ui| {
        menu_setup(ui);
        add(ui)
    })
}

/// A menu item that turns something on or off, or picks one of a few options: a check mark in a
/// gutter before the label when `on`, the same room left empty when not, so labels line up.
/// `keys` is its shortcut, if any. The mark is drawn, so it needs no symbol in the fonts.
pub fn menu_check(ui: &mut Ui, on: bool, label: &str, keys: &str) -> Response {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        MENU_CHECK_GUTTER,
        egui::TextFormat {
            font_id: egui::TextStyle::Button.resolve(ui.style()),
            // Takes the button's own text color: dimmed when disabled, strong when hovered.
            color: Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    let mut button = egui::Button::new(job);
    if !keys.is_empty() {
        button = button.shortcut_text(menu_shortcut(ui, keys));
    }
    let r = ui.add(button);
    if on && ui.is_rect_visible(r.rect) {
        let color = ui.style().interact(&r).text_color();
        let x = r.rect.left() + ui.spacing().button_padding.x;
        let c = egui::pos2(x + MENU_CHECK_GUTTER * 0.4, r.rect.center().y);
        let s = MENU_CHECK_GUTTER * 0.22;
        ui.painter().add(egui::Shape::line(
            vec![
                c + egui::vec2(-s, 0.0),
                c + egui::vec2(-s * 0.3, s * 0.75),
                c + egui::vec2(s * 1.1, -s * 0.8),
            ],
            Stroke::new(1.6_f32, color),
        ));
    }
    r.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), on, label));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_shortcuts_use_symbols_in_mac_order() {
        assert_eq!(mac_keys("Ctrl+Shift+S"), "⇧⌘S");
        assert_eq!(mac_keys("Ctrl+Alt+→"), "⌥⌘→");
        assert_eq!(mac_keys("Ctrl++"), "⌘+");
        assert_eq!(mac_keys("Shift+F3"), "⇧F3");
        assert_eq!(mac_keys("F5"), "F5");
        assert_eq!(mac_keys("Shift+↑ ↓"), "⇧↑ ↓");
        assert_eq!(mac_keys("Control+Shift+Tab"), "⌃⇧Tab");
    }
}
