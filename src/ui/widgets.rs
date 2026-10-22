//! Shared widgets: buttons, toolbars, chips, segmented controls, scrollbars and the location pane.

use crate::ui::theme::{self, Palette};
use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

pub fn pal(ui: &Ui) -> Palette {
    theme::palette(ui.visuals().dark_mode)
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Ghost,
    Primary,
    Toggle(bool),
    Segment(bool),
}

fn paint_button(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    min: Vec2,
    look: Look,
    enabled: bool,
) -> Response {
    let p = pal(ui);
    let fg = match look {
        _ if !enabled => p.text_dim.gamma_multiply(0.55),
        Look::Primary => p.on_accent,
        Look::Toggle(true) => p.accent,
        _ => p.text,
    };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let size = Vec2::new(
        (galley.size().x + 20.0).max(min.x),
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
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let (fill, stroke) = match look {
            Look::Primary => (
                if !enabled {
                    p.accent.gamma_multiply(0.45)
                } else if hovered {
                    p.accent_hover
                } else {
                    p.accent
                },
                Stroke::NONE,
            ),
            Look::Toggle(true) => (p.accent_soft, Stroke::NONE),
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
            CornerRadius::same(7),
            fill,
            stroke,
            StrokeKind::Inside,
        );
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, fg);
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Square toolbar button showing a single glyph; highlights on hover.
pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    icon_button_enabled(ui, true, icon, tip)
}

pub fn icon_button_enabled(ui: &mut Ui, enabled: bool, icon: &str, tip: &str) -> Response {
    paint_button(
        ui,
        icon,
        FontId::proportional(16.0),
        Vec2::new(30.0, 28.0),
        Look::Ghost,
        enabled,
    )
    .on_hover_text(tip)
}

/// Frameless text button that highlights on hover.
pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    paint_button(
        ui,
        label,
        FontId::proportional(13.5),
        Vec2::new(0.0, 28.0),
        Look::Ghost,
        true,
    )
}

/// Accent-filled call-to-action button.
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    paint_button(
        ui,
        label,
        theme::semibold(14.0),
        Vec2::new(96.0, 32.0),
        Look::Primary,
        true,
    )
}

/// Neutral bordered button for secondary dialog actions.
pub fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(label)
            .min_size(Vec2::new(88.0, 32.0))
            .stroke(Stroke::new(1.0_f32, p.border_strong))
            .fill(p.surface),
    )
}

/// Button that stays highlighted while `on` is true.
pub fn toggle_button(ui: &mut Ui, on: &mut bool, label: &str, tip: &str) -> Response {
    let r = paint_button(
        ui,
        label,
        FontId::proportional(13.5),
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
        .corner_radius(9)
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (v, label) in options {
                    let sel = *value == *v;
                    if paint_button(
                        ui,
                        label,
                        FontId::proportional(13.0),
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

/// Full-width toolbar strip with a bottom border.
pub fn toolbar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    let r = egui::Frame::new()
        .fill(p.surface_alt)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
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
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .color(fg)
                        .font(theme::semibold(12.0)),
                )
                .selectable(false),
            );
        })
        .response
}

/// Identity color of a compare side (left / middle / right).
pub fn side_color(ui: &Ui, side: usize, panes: usize) -> Color32 {
    let dark = ui.visuals().dark_mode;
    let idx = if panes == 2 && side == 1 { 2 } else { side };
    match (idx, dark) {
        (0, false) => Color32::from_rgb(0x4E, 0x62, 0xE0),
        (0, true) => Color32::from_rgb(0x8C, 0x9E, 0xFF),
        (1, false) => Color32::from_rgb(0x8E, 0x4E, 0xC6),
        (1, true) => Color32::from_rgb(0xC0, 0x8C, 0xF0),
        (_, false) => Color32::from_rgb(0x0E, 0x8F, 0x7E),
        (_, true) => Color32::from_rgb(0x4F, 0xD1, 0xBC),
    }
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
        .corner_radius(12)
        .inner_margin(Margin::same(16))
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
    let thumb_h = (visible / total_f * h).clamp(24.0, h);
    let frac = if max_scroll > 0.0 {
        *scroll / max_scroll
    } else {
        0.0
    };
    let thumb_top = rect.top() + 2.0 + frac * (h - thumb_h);
    let resp = ui.interact(rect, id, Sense::click_and_drag());
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
    let thumb_w = (visible / content * w).clamp(24.0, w);
    let thumb_left = rect.left() + 2.0 + (*scroll / max_scroll) * (w - thumb_w);
    let resp = ui.interact(rect, id, Sense::click_and_drag());
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
    /// Which columns of the location bar to paint (one per pane).
    pub columns: Vec<Option<Color32>>,
}

/// Location pane: overview of all differences with the visible region outlined.
/// Returns the row the user clicked/dragged to (to be centered).
#[allow(clippy::too_many_arguments)]
pub fn location_pane(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    total: usize,
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
    for m in marks {
        let n = m.columns.len().max(1);
        let col_w = inner.width() / n as f32;
        let y0 = inner.top() + m.start as f32 * scale;
        let hh = (m.len as f32 * scale).max(3.0);
        for (c, color) in m.columns.iter().enumerate() {
            if let Some(color) = color {
                let r = Rect::from_min_size(
                    egui::pos2(inner.left() + c as f32 * col_w, y0),
                    egui::vec2(col_w - 1.5, hh),
                );
                painter.rect_filled(r, 1.5, *color);
            }
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
        3.0,
        viewport.gamma_multiply(0.12),
        Stroke::new(1.0_f32, viewport.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    let resp = ui.interact(rect, id, Sense::click_and_drag());
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
    let id = egui::Id::new("oxidemerge-logo");
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
pub fn nav_item(ui: &mut Ui, selected: bool, icon: &str, label: &str) -> Response {
    let p = pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    let fill = if selected {
        p.accent_soft
    } else if resp.hovered() {
        p.control_hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let fg = if selected { p.accent } else { p.text };
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(15.0),
        fg,
    );
    let font = if selected {
        theme::semibold(13.5)
    } else {
        FontId::proportional(13.5)
    };
    ui.painter().text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Dialog title row with a close button; returns true when the close button was clicked.
pub fn dialog_title(ui: &mut Ui, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).font(theme::semibold(17.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = icon_button(ui, "✖", crate::i18n::tr("btn.cancel")).clicked();
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
            .font(theme::semibold(12.5))
            .color(p.text_dim),
    );
}
