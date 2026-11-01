//! Design system: UI palette, diff colors and the egui style built from them.

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

/// Font family with a heavier weight, used for titles (falls back to the regular face).
pub const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Type scale, in points.
pub mod size {
    /// Shortcut hints, counts and secondary lines.
    pub const CAPTION: f32 = 11.5;
    /// Column headers, chips, badges and details.
    pub const SMALL: f32 = 12.0;
    /// Dense rows and compact action bars.
    pub const DENSE: f32 = 13.0;
    /// Buttons and other controls.
    pub const CONTROL: f32 = 13.5;
    pub const BODY: f32 = 14.0;
    /// Section and card titles.
    pub const SUBTITLE: f32 = 15.0;
    /// Dialog titles.
    pub const TITLE: f32 = 17.0;
    pub const HEADING: f32 = 20.0;
    /// The application name on the start page and in About.
    pub const DISPLAY: f32 = 28.0;
}

/// Corner radii, for `CornerRadius::same` and the painter.
pub mod radius {
    /// Marks inside text and small badges.
    pub const XS: u8 = 3;
    /// Controls inside rows.
    pub const SM: u8 = 5;
    /// Buttons, inputs and menu items.
    pub const MD: u8 = 7;
    /// Cards, menus and panels.
    pub const LG: u8 = 10;
    /// Dialogs, windows and pills.
    pub const XL: u8 = 12;
}

/// Spacing steps, for margins and the gaps between controls.
pub mod space {
    pub const XXS: i8 = 2;
    pub const XS: i8 = 4;
    pub const SM: i8 = 6;
    pub const MD: i8 = 8;
    /// Horizontal padding inside controls.
    pub const CONTROL: i8 = 10;
    pub const LG: i8 = 12;
    pub const XL: i8 = 16;
    /// Dialog margins.
    pub const XXL: i8 = 18;
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
    /// Labels of controls that cannot be used.
    pub text_disabled: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    /// Identity colors of the left, middle and right sides.
    pub sides: [Color32; 3],
    /// Folder icons in lists.
    pub folder: Color32,
    /// Checkerboard behind transparent images.
    pub checker: [Color32; 2],
}

/// Accessibility choices that change the palette, the diff colors and the style.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Look {
    pub color_blind: bool,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub compact: bool,
}

impl Look {
    pub fn of(cfg: &crate::config::Config) -> Self {
        Self {
            color_blind: cfg.color_blind_colors,
            high_contrast: cfg.high_contrast,
            reduce_motion: cfg.reduce_motion,
            compact: cfg.compact_density,
        }
    }

    fn bits(self) -> u8 {
        self.color_blind as u8
            | (self.high_contrast as u8) << 1
            | (self.reduce_motion as u8) << 2
            | (self.compact as u8) << 3
    }

    fn from_bits(b: u8) -> Self {
        Self {
            color_blind: b & 1 != 0,
            high_contrast: b & 2 != 0,
            reduce_motion: b & 4 != 0,
            compact: b & 8 != 0,
        }
    }
}

static LOOK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The look in use, set by [`apply`].
pub fn look() -> Look {
    Look::from_bits(LOOK.load(std::sync::atomic::Ordering::Relaxed))
}

/// Extra height of a text row beyond its font, by density.
pub fn row_padding() -> f32 {
    if look().compact {
        0.0
    } else {
        2.0
    }
}

pub fn palette(dark: bool) -> Palette {
    palette_for(dark, look())
}

pub fn palette_for(dark: bool, look: Look) -> Palette {
    let mut p = base_palette(dark);
    if look.high_contrast {
        if dark {
            p.chrome = rgb(0x000000);
            p.surface = rgb(0x0B0B0D);
            p.surface_alt = rgb(0x111215);
            p.border = rgb(0x6A6F7A);
            p.border_strong = rgb(0x9AA0AB);
            p.text = rgb(0xFFFFFF);
            p.text_dim = rgb(0xD0D4DB);
            p.text_disabled = rgb(0x8A909C);
            p.accent = rgb(0x9DB0FF);
            p.accent_hover = rgb(0xB8C6FF);
            p.accent_pressed = rgb(0xD2DBFF);
        } else {
            p.chrome = rgb(0xFFFFFF);
            p.surface_alt = rgb(0xFFFFFF);
            p.border = rgb(0x8A909C);
            p.border_strong = rgb(0x505662);
            p.text = rgb(0x000000);
            p.text_dim = rgb(0x30353F);
            p.text_disabled = rgb(0x6A707C);
            p.accent = rgb(0x2B3AB8);
            p.accent_hover = rgb(0x212D93);
            p.accent_pressed = rgb(0x18216E);
        }
    }
    if look.color_blind {
        // Blue and orange (Okabe-Ito) instead of green and red.
        (p.success, p.danger) = if dark {
            (rgb(0x56B4E9), rgb(0xE69F00))
        } else {
            (rgb(0x0072B2), rgb(0xD55E00))
        };
    }
    p
}

fn base_palette(dark: bool) -> Palette {
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
            text_disabled: rgb(0x5C616B),
            accent: rgb(0x7B93FF),
            accent_hover: rgb(0x93A6FF),
            accent_pressed: rgb(0xAEBCFF),
            accent_soft: rgb(0x2C3558),
            on_accent: rgb(0x10131F),
            success: rgb(0x4CC38A),
            warning: rgb(0xE8B04B),
            danger: rgb(0xF2706A),
            sides: [rgb(0x8C9EFF), rgb(0xC08CF0), rgb(0x4FD1BC)],
            folder: rgb(0xE0A42E),
            checker: [rgb(0x26272C), rgb(0x2E3035)],
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
            text_disabled: rgb(0xAEB3BC),
            accent: rgb(0x4E62E0),
            accent_hover: rgb(0x3F52CC),
            accent_pressed: rgb(0x3343AE),
            accent_soft: rgb(0xE5E9FD),
            on_accent: rgb(0xFFFFFF),
            success: rgb(0x1F9254),
            warning: rgb(0xB7791F),
            danger: rgb(0xD63B3B),
            sides: [rgb(0x4E62E0), rgb(0x8E4EC6), rgb(0x0E8F7E)],
            folder: rgb(0xE0A42E),
            checker: [rgb(0xF4F5F7), rgb(0xE8EAEE)],
        }
    }
}

/// Colors used by the compare views.
#[derive(Clone, Copy)]
pub struct Colors {
    pub diff: Color32,
    pub diff_sel: Color32,
    pub added: Color32,
    pub added_sel: Color32,
    pub inline: Color32,
    pub inline_sel: Color32,
    pub missing: Color32,
    pub missing_sel: Color32,
    pub moved: Color32,
    pub ignored: Color32,
    pub conflict: Color32,
    pub conflict_sel: Color32,
    /// Removed and added lines of the inline view, and the changed text within them.
    pub removed: Color32,
    pub removed_sel: Color32,
    pub inline_removed: Color32,
    pub inline_added: Color32,
    /// Change markers in the gutter and the location pane.
    pub change_bar: Color32,
    pub conflict_bar: Color32,
    pub gutter_bg: Color32,
    pub gutter_text: Color32,
    pub text: Color32,
    pub text_bg: Color32,
    pub cursor_row: Color32,
    pub find_hit: Color32,
    pub folder_error: Color32,
    pub location_bg: Color32,
    pub viewport: Color32,
    pub table_grid: Color32,
    pub ui: Palette,
}

pub fn colors(dark: bool) -> Colors {
    colors_for(dark, look())
}

pub fn colors_for(dark: bool, look: Look) -> Colors {
    let mut c = base_colors(dark, palette_for(dark, look));
    if look.color_blind {
        // Blue and orange (Okabe-Ito) instead of green and red; changes in purple, conflicts
        // in yellow, moved lines in bluish green and search hits in strong yellow, so none of
        // them looks like another.
        if dark {
            (c.diff, c.diff_sel, c.inline, c.inline_sel) =
                (rgb(0x33294A), rgb(0x45375F), rgb(0x5A4785), rgb(0x6E58A0));
            (c.added, c.added_sel, c.inline_added) = (rgb(0x1B3553), rgb(0x23466D), rgb(0x2D5F95));
            (c.removed, c.removed_sel, c.inline_removed) =
                (rgb(0x44301A), rgb(0x5A3F20), rgb(0x86592A));
            (c.conflict, c.conflict_sel, c.conflict_bar) =
                (rgb(0x3D4214), rgb(0x525919), rgb(0xF0E442));
            c.change_bar = rgb(0xB79CF0);
            (c.moved, c.find_hit) = (rgb(0x173D35), rgb(0x8A7413));
        } else {
            (c.diff, c.diff_sel, c.inline, c.inline_sel) =
                (rgb(0xEBDDF9), rgb(0xDECBF4), rgb(0xD3C0F0), rgb(0xC2A8EA));
            (c.added, c.added_sel, c.inline_added) = (rgb(0xDCEBFA), rgb(0xC6DDF5), rgb(0xA8CBF0));
            (c.removed, c.removed_sel, c.inline_removed) =
                (rgb(0xFCEEDC), rgb(0xF9E0C0), rgb(0xF2C48A));
            (c.conflict, c.conflict_sel, c.conflict_bar) =
                (rgb(0xFAF5C4), rgb(0xF5EDB0), rgb(0xA89200));
            c.change_bar = rgb(0x7B5BC4);
            (c.moved, c.find_hit) = (rgb(0xD8F0E6), rgb(0xFFD84A));
        }
    }
    if look.high_contrast {
        let more = |a: Color32, b: Color32| a.lerp_to_gamma(b, 0.35);
        let toward = if dark { rgb(0xFFFFFF) } else { rgb(0x000000) };
        for col in [
            &mut c.inline,
            &mut c.inline_sel,
            &mut c.inline_added,
            &mut c.inline_removed,
            &mut c.change_bar,
            &mut c.conflict_bar,
        ] {
            *col = more(*col, toward);
        }
        // Row colors further from the background they are drawn on.
        let bg = c.text_bg;
        let away = |col: Color32| {
            let ch =
                |a: u8, b: u8| (b as f32 + (a as f32 - b as f32) * 1.7).clamp(0.0, 255.0) as u8;
            Color32::from_rgb(
                ch(col.r(), bg.r()),
                ch(col.g(), bg.g()),
                ch(col.b(), bg.b()),
            )
        };
        for col in [
            &mut c.diff,
            &mut c.diff_sel,
            &mut c.added,
            &mut c.added_sel,
            &mut c.removed,
            &mut c.removed_sel,
            &mut c.missing,
            &mut c.missing_sel,
            &mut c.moved,
            &mut c.conflict,
            &mut c.conflict_sel,
        ] {
            *col = away(*col);
        }
        c.text = c.ui.text;
        c.gutter_text = c.ui.text_dim;
    }
    c
}

fn base_colors(dark: bool, ui: Palette) -> Colors {
    if dark {
        Colors {
            diff: rgb(0x223449),
            diff_sel: rgb(0x2D4966),
            added: rgb(0x233C2B),
            added_sel: rgb(0x2A5135),
            inline: rgb(0x315D85),
            inline_sel: rgb(0x4075A4),
            missing: rgb(0x26272C),
            missing_sel: rgb(0x31333A),
            moved: rgb(0x23304A),
            ignored: rgb(0x232428),
            conflict: rgb(0x47272A),
            conflict_sel: rgb(0x5C2C30),
            removed: rgb(0x40262A),
            removed_sel: rgb(0x542D32),
            inline_removed: rgb(0x7C3A40),
            inline_added: rgb(0x2F6E40),
            change_bar: rgb(0x6AA8E8),
            conflict_bar: rgb(0xF2706A),
            gutter_bg: rgb(0x1B1C20),
            gutter_text: rgb(0x5F646E),
            text: rgb(0xDCDFE4),
            text_bg: ui.surface,
            cursor_row: ui.accent,
            find_hit: rgb(0x2E5F96),
            folder_error: rgb(0x3D2427),
            location_bg: rgb(0x1B1C20),
            viewport: rgb(0xA9B6FF),
            table_grid: rgb(0x34363C),
            ui,
        }
    } else {
        Colors {
            diff: rgb(0xE1EFFF),
            diff_sel: rgb(0xC4DEFF),
            added: rgb(0xE6F5D4),
            added_sel: rgb(0xD1EDAD),
            inline: rgb(0xA5CCFF),
            inline_sel: rgb(0x85B8F4),
            missing: rgb(0xF2F3F6),
            missing_sel: rgb(0xE7E9EE),
            moved: rgb(0xE5EDFF),
            ignored: rgb(0xF7F8FA),
            conflict: rgb(0xFFE4E4),
            conflict_sel: rgb(0xFFCACA),
            removed: rgb(0xFDECEC),
            removed_sel: rgb(0xFBD9D9),
            inline_removed: rgb(0xF5B8B8),
            inline_added: rgb(0xB5E3A0),
            change_bar: rgb(0x619BE0),
            conflict_bar: rgb(0xE05252),
            gutter_bg: rgb(0xF8F9FB),
            gutter_text: rgb(0xA2A9B4),
            text: rgb(0x1D2128),
            text_bg: ui.surface,
            cursor_row: ui.accent,
            find_hit: rgb(0xBBD6FF),
            folder_error: rgb(0xFFEDED),
            location_bg: rgb(0xF4F5F7),
            viewport: rgb(0x4E62E0),
            table_grid: rgb(0xE6E8EC),
            ui,
        }
    }
}

/// Installs the visual style (colors, spacing, rounding, typography) for the given mode and
/// look; the look also applies to [`palette`] and [`colors`] from now on.
pub fn apply(ctx: &egui::Context, dark: bool, look: Look) {
    LOOK.store(look.bits(), std::sync::atomic::Ordering::Relaxed);
    // Pin egui's own theme so it never swaps in its stock style behind ours.
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let p = palette(dark);
    let mut style = (*ctx.global_style()).clone();

    style.text_styles = [
        (TextStyle::Small, FontId::proportional(size::CAPTION)),
        (TextStyle::Body, FontId::proportional(size::BODY)),
        (TextStyle::Button, FontId::proportional(size::BODY)),
        (TextStyle::Heading, semibold(size::HEADING)),
        (TextStyle::Monospace, FontId::monospace(13.5)),
    ]
    .into();

    let s = &mut style.spacing;
    let step = |n: i8| n as f32;
    s.item_spacing = egui::vec2(step(space::MD), step(space::SM));
    s.button_padding = egui::vec2(step(space::CONTROL), step(space::XS));
    s.interact_size = egui::vec2(28.0, 26.0);
    s.window_margin = Margin::same(space::XXL);
    s.menu_margin = Margin::same(space::SM);
    s.menu_spacing = step(space::XS);
    s.combo_height = 320.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.slider_rail_height = 4.0;
    s.scroll = egui::style::ScrollStyle::thin();

    if look.compact {
        s.item_spacing = egui::vec2(step(space::SM), 3.0);
        s.button_padding = egui::vec2(step(space::MD), step(space::XXS));
        s.interact_size = egui::vec2(24.0, 22.0);
        s.window_margin = Margin::same(14);
    }

    style.interaction.tooltip_delay = 0.35;
    style.animation_time = if look.reduce_motion { 0.0 } else { 1.0 / 12.0 };
    style.scroll_animation = if look.reduce_motion {
        egui::style::ScrollAnimation::none()
    } else {
        egui::style::ScrollAnimation::default()
    };

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
    v.window_corner_radius = CornerRadius::same(radius::XL);
    v.menu_corner_radius = CornerRadius::same(radius::LG);
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

    let control = CornerRadius::same(radius::MD);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.noninteractive.corner_radius = control;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.corner_radius = control;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.hovered.corner_radius = control;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.active.corner_radius = control;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control_hover;
    w.open.weak_bg_fill = p.control_hover;
    w.open.bg_stroke = Stroke::new(1.0_f32, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.open.corner_radius = control;
    w.open.expansion = 0.0;

    ctx.set_global_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_blind_and_high_contrast_looks_change_the_diff_colors() {
        for dark in [false, true] {
            let cb = colors_for(
                dark,
                Look {
                    color_blind: true,
                    ..Default::default()
                },
            );
            // Added lines lean blue and removed lines orange, rather than green and red.
            assert!(cb.added.b() > cb.added.g() && cb.added.b() > cb.added.r());
            assert!(cb.removed.r() > cb.removed.b() && cb.removed.g() > cb.removed.b());
            let plain = colors_for(dark, Look::default());
            assert!(plain.added.g() > plain.added.b());
            // The status colors drawn from the palette change too.
            let cb_palette = palette_for(
                dark,
                Look {
                    color_blind: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                (cb_palette.success, cb_palette.danger),
                (cb.ui.success, cb.ui.danger)
            );
            assert_ne!(cb_palette.success, plain.ui.success);
            // No two of the colors that mark lines look alike.
            let far = |a: Color32, b: Color32| {
                let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
                d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b()) > 24
            };
            let rows = [
                cb.diff,
                cb.added,
                cb.removed,
                cb.conflict,
                cb.moved,
                cb.missing,
            ];
            for (i, &a) in rows.iter().enumerate() {
                for &b in &rows[i + 1..] {
                    assert!(far(a, b), "{a:?} and {b:?} look alike");
                }
            }
            for hl in [cb.inline, cb.inline_added, cb.inline_removed] {
                assert!(far(cb.find_hit, hl), "search hits vanish on {hl:?}");
            }

            let hc = palette_for(
                dark,
                Look {
                    high_contrast: true,
                    ..Default::default()
                },
            );
            let base = palette_for(dark, Look::default());
            let contrast = |p: &Palette| (p.text.r() as i32 - p.surface.r() as i32).abs();
            assert!(contrast(&hc) > contrast(&base));
            let hc_colors = colors_for(
                dark,
                Look {
                    high_contrast: true,
                    ..Default::default()
                },
            );
            let off_bg = |c: &Colors, col: Color32| {
                let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
                d(col.r(), c.text_bg.r()) + d(col.g(), c.text_bg.g()) + d(col.b(), c.text_bg.b())
            };
            for (a, b) in [(hc_colors.diff, plain.diff), (hc_colors.added, plain.added)] {
                assert!(off_bg(&hc_colors, a) > off_bg(&plain, b));
            }
        }
        let compact = Look {
            compact: true,
            ..Default::default()
        };
        assert_eq!(Look::from_bits(compact.bits()), compact);
    }

    #[test]
    fn control_states_are_told_apart_in_every_look() {
        let dist = |a: Color32, b: Color32| {
            let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
            d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
        };
        for bits in 0..16 {
            for dark in [false, true] {
                let p = palette_for(dark, Look::from_bits(bits));
                // Rest, hover and pressed differ, for neutral and accent controls.
                assert!(dist(p.control, p.control_hover) > 10);
                assert!(dist(p.control_hover, p.control_active) > 10);
                assert!(dist(p.accent, p.accent_hover) > 20);
                assert!(dist(p.accent_hover, p.accent_pressed) > 20);
                // Disabled labels are dimmer than secondary text but still readable.
                assert!(dist(p.text_disabled, p.surface) < dist(p.text_dim, p.surface));
                assert!(dist(p.text_disabled, p.surface) > 120);
                // Sides keep distinct identity colors.
                assert!(dist(p.sides[0], p.sides[1]) > 60 && dist(p.sides[1], p.sides[2]) > 60);
            }
        }
    }
}
