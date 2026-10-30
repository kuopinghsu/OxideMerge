//! About dialog, opened from the Help menu or the logo in the header, and the card and title
//! strip it shares with the Help & Shortcuts dialog.

use crate::i18n::{tr, trf};
use crate::ui::{theme, widgets};
use egui::{Color32, CornerRadius, Margin, RichText, Sense, Shadow, Stroke, Vec2};

const RADIUS: u8 = 10;

const FEATURES: [&str; 4] = [
    "about.feature_compare",
    "about.feature_merge",
    "about.feature_options",
    "about.feature_platforms",
];

/// Draws the dialog. Returns true once it should close.
pub fn show(ctx: &egui::Context) -> bool {
    let mut close = false;
    let response = egui::Modal::new(egui::Id::new("about_dialog"))
        .frame(dialog_frame(ctx))
        .show(ctx, |ui| {
            ui.set_width(600.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            title_bar(ui, tr("menu.about"), &mut close);
            egui::Frame::new()
                .inner_margin(Margin {
                    left: 24,
                    right: 28,
                    top: 22,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        widgets::logo(ui, 120.0);
                        ui.add_space(24.0);
                        ui.vertical(body);
                    });
                });
        });
    close || response.should_close()
}

/// Card behind the About and Help & Shortcuts dialogs.
pub fn dialog_frame(ctx: &egui::Context) -> egui::Frame {
    let dark = ctx.style().visuals.dark_mode;
    let p = theme::palette(dark);
    egui::Frame::new()
        .fill(p.surface)
        .corner_radius(CornerRadius::same(RADIUS))
        .stroke(Stroke::new(1.0_f32, p.border))
        .shadow(Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 90 } else { 45 }),
        })
}

/// Window-style strip across the top of a dialog with the logo, `title`, and a close button that
/// sets `close`.
pub fn title_bar(ui: &mut egui::Ui, title: &str, close: &mut bool) {
    let p = widgets::pal(ui);
    let corners = CornerRadius {
        nw: RADIUS,
        ne: RADIUS,
        sw: 0,
        se: 0,
    };
    egui::Frame::new()
        .fill(p.chrome)
        .corner_radius(corners)
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::logo(ui, 18.0);
                ui.label(RichText::new(title).size(13.0).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui).on_hover_text(tr("btn.close")).clicked() {
                        *close = true;
                    }
                });
            });
        });
}

fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let p = widgets::pal(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(6), p.control_hover);
    }
    let arm = 5.0;
    let color = if response.hovered() {
        p.text
    } else {
        p.text_dim
    };
    let stroke = Stroke::new(1.4_f32, color);
    let c = rect.center();
    painter.line_segment([c + Vec2::new(-arm, -arm), c + Vec2::new(arm, arm)], stroke);
    painter.line_segment([c + Vec2::new(arm, -arm), c + Vec2::new(-arm, arm)], stroke);
    response
}

fn body(ui: &mut egui::Ui) {
    let p = widgets::pal(ui);
    let build = env!("OXIDEMERGE_BUILD_ID");
    let mut title = egui::text::LayoutJob::default();
    let parts = [
        RichText::new(format!("OxideMerge {}  ", env!("CARGO_PKG_VERSION")))
            .font(theme::semibold(26.0))
            .color(p.text),
        RichText::new(format!(
            "({})",
            trf("about.build", &[("date", build.to_string())])
        ))
        .size(16.0)
        .color(p.text_dim),
    ];
    for part in parts {
        part.append_to(
            &mut title,
            ui.style(),
            egui::FontSelection::Default,
            egui::Align::BOTTOM,
        );
    }
    ui.label(title);

    heading(ui, tr("app.tagline"));
    text(ui, tr("app.about"));

    heading(ui, tr("about.features"));
    for feature in FEATURES {
        text(ui, &format!("-  {}", tr(feature)));
    }

    heading(ui, tr("about.credits"));
    text(ui, tr("about.built_with"));
    link(ui, tr("about.winmerge"), "https://winmerge.org/");
    text(ui, tr("about.thanks"));

    heading(ui, tr("about.license"));
    let repository = env!("CARGO_PKG_REPOSITORY");
    link(ui, repository.trim_start_matches("https://"), repository);
    let date = env!("OXIDEMERGE_BUILD_DATE");
    let year = date.get(..4).unwrap_or(date);
    text(
        ui,
        &trf(
            "about.copyright",
            &[
                ("year", year.to_string()),
                ("license", env!("CARGO_PKG_LICENSE").to_string()),
            ],
        ),
    );
}

fn heading(ui: &mut egui::Ui, title: &str) {
    let color = widgets::pal(ui).text;
    ui.add_space(12.0);
    ui.label(
        RichText::new(title)
            .font(theme::semibold(17.0))
            .color(color),
    );
    ui.add_space(1.0);
}

fn text(ui: &mut egui::Ui, line: &str) {
    let color = widgets::pal(ui).text;
    ui.add(egui::Label::new(RichText::new(line).size(13.5).color(color)).wrap());
}

fn link(ui: &mut egui::Ui, label: &str, url: &str) {
    let color = ui.visuals().hyperlink_color;
    ui.hyperlink_to(RichText::new(label).size(13.5).color(color), url);
}
