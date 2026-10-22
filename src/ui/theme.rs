//! Design system: UI palette, diff colors and the egui style built from them.

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

/// Font family with a heavier weight, used for titles (falls back to the regular face).
pub const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Colors of the application chrome (panels, controls, accents).
#[derive(Clone, Copy)]
pub struct Palette {
    /// Title bar, tab strip and status bar.
    pub chrome: Color32,
    /// Main content surface, cards and dialogs.
    pub surface: Color32,
    /// Toolbars and pane headers.
    pub surface_alt: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    pub input: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            chrome: rgb(0x17181B),
            surface: rgb(0x1E1F23),
            surface_alt: rgb(0x232429),
            border: rgb(0x2F3137),
            border_strong: rgb(0x43464E),
            control: rgb(0x2A2C31),
            control_hover: rgb(0x34363C),
            control_active: rgb(0x3D4047),
            input: rgb(0x16171A),
            text: rgb(0xE4E6EA),
            text_dim: rgb(0x949AA4),
            accent: rgb(0x7B93FF),
            accent_hover: rgb(0x93A6FF),
            accent_soft: rgb(0x2C3558),
            on_accent: rgb(0x10131F),
            success: rgb(0x4CC38A),
            warning: rgb(0xE8B04B),
            danger: rgb(0xF2706A),
        }
    } else {
        Palette {
            chrome: rgb(0xF1F2F5),
            surface: rgb(0xFFFFFF),
            surface_alt: rgb(0xF8F9FB),
            border: rgb(0xE1E4E9),
            border_strong: rgb(0xC9CED6),
            control: rgb(0xF0F1F4),
            control_hover: rgb(0xE6E8EC),
            control_active: rgb(0xDADDE3),
            input: rgb(0xFFFFFF),
            text: rgb(0x1D2128),
            text_dim: rgb(0x687080),
            accent: rgb(0x4E62E0),
            accent_hover: rgb(0x3F52CC),
            accent_soft: rgb(0xE5E9FD),
            on_accent: rgb(0xFFFFFF),
            success: rgb(0x1F9254),
            warning: rgb(0xB7791F),
            danger: rgb(0xD63B3B),
        }
    }
}

/// Colors used by the compare views.
#[derive(Clone, Copy)]
pub struct Colors {
    pub diff: Color32,
    pub diff_sel: Color32,
    pub inline: Color32,
    pub inline_sel: Color32,
    pub missing: Color32,
    pub missing_sel: Color32,
    pub moved: Color32,
    pub ignored: Color32,
    pub conflict: Color32,
    pub conflict_sel: Color32,
    /// Change markers in the gutter and the location pane.
    pub change_bar: Color32,
    pub conflict_bar: Color32,
    pub gutter_bg: Color32,
    pub gutter_text: Color32,
    pub text: Color32,
    pub text_bg: Color32,
    pub cursor_row: Color32,
    pub find_hit: Color32,
    pub folder_diff: Color32,
    pub folder_unique: Color32,
    pub folder_error: Color32,
    pub location_bg: Color32,
    pub viewport: Color32,
    pub table_grid: Color32,
    pub ui: Palette,
}

pub fn colors(dark: bool) -> Colors {
    let ui = palette(dark);
    if dark {
        Colors {
            diff: rgb(0x3A3322),
            diff_sel: rgb(0x4D3F24),
            inline: rgb(0x6E5A26),
            inline_sel: rgb(0x8A6A2A),
            missing: rgb(0x26272C),
            missing_sel: rgb(0x31333A),
            moved: rgb(0x23304A),
            ignored: rgb(0x232428),
            conflict: rgb(0x47272A),
            conflict_sel: rgb(0x5C2C30),
            change_bar: rgb(0xD9A440),
            conflict_bar: rgb(0xF2706A),
            gutter_bg: rgb(0x1B1C20),
            gutter_text: rgb(0x5F646E),
            text: rgb(0xDCDFE4),
            text_bg: ui.surface,
            cursor_row: ui.accent,
            find_hit: rgb(0x2E5F96),
            folder_diff: rgb(0x2E2A21),
            folder_unique: rgb(0x24262B),
            folder_error: rgb(0x3D2427),
            location_bg: rgb(0x1B1C20),
            viewport: rgb(0xA9B6FF),
            table_grid: rgb(0x34363C),
            ui,
        }
    } else {
        Colors {
            diff: rgb(0xFFF5DA),
            diff_sel: rgb(0xFFE7B8),
            inline: rgb(0xFBD98A),
            inline_sel: rgb(0xF6BE62),
            missing: rgb(0xF2F3F6),
            missing_sel: rgb(0xE7E9EE),
            moved: rgb(0xE5EDFF),
            ignored: rgb(0xF7F8FA),
            conflict: rgb(0xFFE4E4),
            conflict_sel: rgb(0xFFCACA),
            change_bar: rgb(0xE0A030),
            conflict_bar: rgb(0xE05252),
            gutter_bg: rgb(0xF8F9FB),
            gutter_text: rgb(0xA2A9B4),
            text: rgb(0x1D2128),
            text_bg: ui.surface,
            cursor_row: ui.accent,
            find_hit: rgb(0xBBD6FF),
            folder_diff: rgb(0xFFF8EA),
            folder_unique: rgb(0xF4F5F8),
            folder_error: rgb(0xFFEDED),
            location_bg: rgb(0xF4F5F7),
            viewport: rgb(0x4E62E0),
            table_grid: rgb(0xE6E8EC),
            ui,
        }
    }
}

/// Installs the visual style (colors, spacing, rounding, typography) for the given mode.
pub fn apply(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let mut style = (*ctx.style()).clone();

    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.5)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Heading, semibold(20.0)),
        (TextStyle::Monospace, FontId::monospace(13.5)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(10.0, 4.0);
    s.interact_size = egui::vec2(28.0, 26.0);
    s.window_margin = Margin::same(18);
    s.menu_margin = Margin::same(6);
    s.menu_spacing = 4.0;
    s.combo_height = 320.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.slider_rail_height = 4.0;
    s.scroll = egui::style::ScrollStyle::thin();

    style.interaction.tooltip_delay = 0.35;

    let v = &mut style.visuals;
    *v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = None;
    v.panel_fill = p.surface;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 32,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 120 } else { 38 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 100 } else { 30 }),
    };
    v.window_highlight_topmost = false;
    v.faint_bg_color = p.surface_alt;
    v.extreme_bg_color = p.input;
    v.code_bg_color = p.control;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = if dark { rgb(0x34407A) } else { rgb(0xD6DCFB) };
    v.selection.stroke = Stroke::new(1.0_f32, if dark { rgb(0xDDE3FF) } else { rgb(0x2B3AA8) });
    v.slider_trailing_fill = true;
    v.striped = false;
    v.text_cursor.stroke = Stroke::new(2.0_f32, p.accent);

    let radius = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control_hover;
    w.open.weak_bg_fill = p.control_hover;
    w.open.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.open.corner_radius = radius;
    w.open.expansion = 0.0;

    ctx.set_style(style);
}
