//! Image compare view.

use crate::config::Config;
use crate::core::image_cmp::{self, ImageDiff};
use crate::i18n::{tr, trf};
use crate::ui::theme::Colors;
use crate::ui::widgets;
use egui::{
    pos2, vec2, Color32, ColorImage, Rect, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, Vec2,
};
use image::RgbaImage;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImgMode {
    SideBySide,
    Overlay,
    Difference,
}

pub struct ImageView {
    pub paths: Vec<PathBuf>,
    /// Shown instead of the file names in the pane headers.
    pub descriptions: Vec<Option<String>>,
    images: Vec<RgbaImage>,
    textures: Vec<Option<TextureHandle>>,
    diff_tex: Option<TextureHandle>,
    textures_dirty: bool,
    diff_dirty: bool,
    diff: ImageDiff,
    mode: ImgMode,
    zoom: f32,
    offset: Vec2,
    fit: bool,
    center: bool,
    show_blocks: bool,
    overlay_alpha: f32,
    threshold: u8,
    block: u32,
    cur: Option<usize>,
    pub modified: Vec<bool>,
    undo: Vec<(Vec<RgbaImage>, Vec<bool>)>,
    pub status: String,
    id: egui::Id,
}

impl ImageView {
    pub fn load(paths: Vec<PathBuf>, cfg: &Config) -> anyhow::Result<Self> {
        let mut images = Vec::new();
        for p in &paths {
            let img = image::open(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            images.push(img.to_rgba8());
        }
        let refs: Vec<&RgbaImage> = images.iter().collect();
        let diff = image_cmp::compare(&refs, cfg.image_threshold, cfg.image_block);
        let n = paths.len();
        Ok(ImageView {
            paths,
            descriptions: vec![None; n],
            images,
            textures: vec![None; n],
            diff_tex: None,
            textures_dirty: true,
            diff_dirty: true,
            diff,
            mode: ImgMode::SideBySide,
            zoom: 1.0,
            offset: Vec2::ZERO,
            fit: true,
            center: false,
            show_blocks: true,
            overlay_alpha: 0.5,
            threshold: cfg.image_threshold,
            block: cfg.image_block.max(1),
            cur: None,
            modified: vec![false; n],
            undo: Vec::new(),
            status: String::new(),
            id: egui::Id::new(("imgview", std::time::Instant::now())),
        })
    }

    pub fn n(&self) -> usize {
        self.images.len()
    }

    pub fn title(&self) -> String {
        self.paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = p
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if self.modified[i] {
                    format!("{name}*")
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(" ↔ ")
    }

    pub fn is_modified(&self) -> bool {
        self.modified.iter().any(|&m| m)
    }

    fn recompute(&mut self) {
        let refs: Vec<&RgbaImage> = self.images.iter().collect();
        self.diff = image_cmp::compare(&refs, self.threshold, self.block);
        self.diff_dirty = true;
        if self.cur.is_some_and(|c| c >= self.diff.regions.len()) {
            self.cur = None;
        }
    }

    pub fn next_diff(&mut self) {
        if self.diff.regions.is_empty() {
            return;
        }
        let i = self
            .cur
            .map(|c| (c + 1).min(self.diff.regions.len() - 1))
            .unwrap_or(0);
        self.cur = Some(i);
        self.center_on_current();
    }

    pub fn prev_diff(&mut self) {
        if self.diff.regions.is_empty() {
            return;
        }
        let i = self.cur.map(|c| c.saturating_sub(1)).unwrap_or(0);
        self.cur = Some(i);
        self.center_on_current();
    }

    pub fn first_diff(&mut self) {
        if !self.diff.regions.is_empty() {
            self.cur = Some(0);
            self.center_on_current();
        }
    }

    pub fn last_diff(&mut self) {
        if !self.diff.regions.is_empty() {
            self.cur = Some(self.diff.regions.len() - 1);
            self.center_on_current();
        }
    }

    fn center_on_current(&mut self) {
        self.fit = false;
        self.center = true;
    }

    pub fn copy_current(&mut self, to_right: bool) {
        let Some(i) = self.cur else { return };
        let n = self.n();
        let (from, to) = match (n, to_right) {
            (2, true) => (0, 1),
            (2, false) => (1, 0),
            (_, true) => (0, 1),
            (_, false) => (2, 1),
        };
        let [x, y, w, h] = self.diff.regions[i];
        self.undo.push((self.images.clone(), self.modified.clone()));
        let (src, dst) = if from < to {
            let (a, b) = self.images.split_at_mut(to);
            (&a[from], &mut b[0])
        } else {
            let (a, b) = self.images.split_at_mut(from);
            (&b[0], &mut a[to])
        };
        for yy in y..y + h {
            for xx in x..x + w {
                if xx < src.width() && yy < src.height() && xx < dst.width() && yy < dst.height() {
                    dst.put_pixel(xx, yy, *src.get_pixel(xx, yy));
                }
            }
        }
        self.modified[to] = true;
        self.textures_dirty = true;
        self.recompute();
    }

    pub fn undo(&mut self) {
        if let Some((imgs, m)) = self.undo.pop() {
            self.images = imgs;
            self.modified = m;
            self.textures_dirty = true;
            self.recompute();
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        for i in 0..self.n() {
            if self.modified[i] {
                self.images[i]
                    .save(&self.paths[i])
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                self.modified[i] = false;
                self.status = trf(
                    "msg.saved",
                    &[("file", self.paths[i].display().to_string())],
                );
            }
        }
        Ok(())
    }

    pub fn status_line(&self) -> String {
        let total = (self.diff.width as u64 * self.diff.height as u64).max(1);
        let pct = self.diff.diff_pixels as f64 * 100.0 / total as f64;
        let mut s = if self.diff.diff_pixels == 0 {
            tr("msg.images_identical").to_string()
        } else {
            trf(
                "msg.image_diff",
                &[
                    ("pixels", self.diff.diff_pixels.to_string()),
                    ("pct", format!("{pct:.3}")),
                    ("regions", self.diff.regions.len().to_string()),
                ],
            )
        };
        if let Some(c) = self.cur {
            s.push_str(&format!(
                "  |  {}",
                trf(
                    "msg.diff_of",
                    &[
                        ("n", (c + 1).to_string()),
                        ("total", self.diff.regions.len().to_string())
                    ]
                )
            ));
        }
        s.push_str(&format!(
            "  |  {}: {:.0}%",
            tr("label.zoom"),
            self.zoom * 100.0
        ));
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    fn ensure_textures(&mut self, ctx: &egui::Context) {
        if self.textures_dirty {
            self.textures_dirty = false;
            for (i, img) in self.images.iter().enumerate() {
                let ci = ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                );
                self.textures[i] = Some(ctx.load_texture(
                    format!("{:?}img{i}", self.id),
                    ci,
                    TextureOptions::NEAREST,
                ));
            }
        }
        if !self.diff_dirty {
            return;
        }
        self.diff_dirty = false;
        let d = &self.diff.diff_image;
        let ci = ColorImage::from_rgba_unmultiplied(
            [d.width() as usize, d.height() as usize],
            d.as_raw(),
        );
        self.diff_tex =
            Some(ctx.load_texture(format!("{:?}diff", self.id), ci, TextureOptions::NEAREST));
    }

    pub fn ui(&mut self, ui: &mut Ui, _cfg: &Config, c: &Colors) {
        self.ensure_textures(ui.ctx());
        let mut recompute = false;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            widgets::segmented(
                ui,
                &mut self.mode,
                &[
                    (ImgMode::SideBySide, tr("img.side_by_side")),
                    (ImgMode::Overlay, tr("img.overlay")),
                    (ImgMode::Difference, tr("img.difference")),
                ],
            );
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, "−", tr("cmd.zoom_out")).clicked() {
                self.zoom = (self.zoom / 1.25).max(0.02);
            }
            widgets::chip(
                ui,
                &format!("{:.0}%", self.zoom * 100.0),
                pal.text,
                pal.control,
            );
            if widgets::icon_button(ui, "+", tr("cmd.zoom_in")).clicked() {
                self.zoom = (self.zoom * 1.25).min(64.0);
            }
            if widgets::ghost_button(ui, &format!("⛶ {}", tr("img.fit"))).clicked() {
                self.fit = true;
            }
            if widgets::ghost_button(ui, "1:1").clicked() {
                self.zoom = 1.0;
                self.offset = Vec2::ZERO;
            }
            widgets::toolbar_sep(ui);
            widgets::toggle_button(
                ui,
                &mut self.show_blocks,
                &format!("▦ {}", tr("img.show_blocks")),
                tr("img.show_blocks"),
            );
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tr("img.threshold")).color(pal.text_dim));
            // Comparing every pixel is too slow to repeat on each frame of a drag.
            let settled = |r: egui::Response| (r.changed() && !r.dragged()) || r.drag_stopped();
            recompute |= settled(ui.add(egui::DragValue::new(&mut self.threshold).range(0..=255)));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tr("img.block_size")).color(pal.text_dim));
            recompute |= settled(ui.add(egui::DragValue::new(&mut self.block).range(1..=256)));
            if self.mode == ImgMode::Overlay {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(tr("img.alpha")).color(pal.text_dim));
                ui.add(egui::Slider::new(&mut self.overlay_alpha, 0.0..=1.0).show_value(false));
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, "↑", tr("cmd.prev_diff")).clicked() {
                self.prev_diff();
            }
            if widgets::icon_button(ui, "↓", tr("cmd.next_diff")).clicked() {
                self.next_diff();
            }
            if widgets::icon_button(ui, "→", tr("cmd.copy_right")).clicked() {
                self.copy_current(true);
            }
            if widgets::icon_button(ui, "←", tr("cmd.copy_left")).clicked() {
                self.copy_current(false);
            }
            if widgets::icon_button_enabled(ui, !self.undo.is_empty(), "↶", tr("cmd.undo"))
                .clicked()
            {
                self.undo();
            }
        });
        if recompute {
            self.recompute();
        }

        let avail = ui.available_rect_before_wrap();
        let panes = if self.mode == ImgMode::SideBySide {
            self.n()
        } else {
            1
        };
        let pane_w = avail.width() / panes as f32;
        let header_h = 36.0;
        let img_size = vec2(self.diff.width as f32, self.diff.height as f32);
        let view_size = vec2(pane_w - 4.0, avail.height() - header_h);
        if self.fit {
            self.zoom = (view_size.x / img_size.x.max(1.0))
                .min(view_size.y / img_size.y.max(1.0))
                .min(1.0);
            self.offset = (view_size - img_size * self.zoom) / 2.0;
            self.fit = false;
        }
        if self.center {
            self.center = false;
            if let Some(i) = self.cur {
                let [x, y, w, h] = self.diff.regions[i];
                let center = vec2(x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0);
                self.offset = view_size / 2.0 - center * self.zoom;
            }
        }

        let all_rect = Rect::from_min_max(pos2(avail.left(), avail.top() + header_h), avail.max);
        let resp = ui.interact(all_rect, self.id.with("drag"), Sense::click_and_drag());
        if resp.dragged() {
            self.offset += resp.drag_delta();
        }
        if let Some(hover) = resp.hover_pos() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                let local = hover
                    - pos2(
                        avail.left() + ((hover.x - avail.left()) / pane_w).floor() * pane_w,
                        avail.top() + header_h,
                    );
                let old = self.zoom;
                let new = (old * (1.0 + dy * 0.0015)).clamp(0.02, 64.0);
                self.offset = local - (local - self.offset) * (new / old);
                self.zoom = new;
            }
        }

        let dims: Vec<String> = self
            .images
            .iter()
            .map(|i| format!("{}×{}", i.width(), i.height()))
            .collect();
        let pal = widgets::pal(ui);
        let header_rect =
            Rect::from_min_max(avail.min, pos2(avail.right(), avail.top() + header_h));
        ui.painter().rect_filled(header_rect, 0.0, pal.surface_alt);
        ui.painter().hline(
            header_rect.x_range(),
            header_rect.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.border),
        );
        let checker = if ui.visuals().dark_mode {
            [
                Color32::from_rgb(0x26, 0x27, 0x2C),
                Color32::from_rgb(0x2E, 0x30, 0x35),
            ]
        } else {
            [
                Color32::from_rgb(0xF4, 0xF5, 0xF7),
                Color32::from_rgb(0xE8, 0xEA, 0xEE),
            ]
        };
        for p in 0..panes {
            let x0 = avail.left() + p as f32 * pane_w;
            let header =
                Rect::from_min_size(pos2(x0 + 8.0, avail.top()), vec2(pane_w - 16.0, header_h));
            let n = self.n();
            ui.scope_builder(egui::UiBuilder::new().max_rect(header), |ui| {
                ui.horizontal_centered(|ui| match self.mode {
                    ImgMode::SideBySide => {
                        widgets::side_chip(ui, p, n);
                        let path = match self.descriptions.get(p) {
                            Some(Some(d)) => d.clone(),
                            _ => self.paths[p].display().to_string(),
                        };
                        let mut name = match self.descriptions.get(p) {
                            Some(Some(d)) => d.clone(),
                            _ => self.paths[p]
                                .file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone()),
                        };
                        if self.modified[p] {
                            name.push_str("  ●");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            widgets::chip(ui, &dims[p], pal.text_dim, pal.control);
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(name)
                                                .font(crate::ui::theme::semibold(13.5)),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(path);
                                },
                            );
                        });
                    }
                    ImgMode::Overlay | ImgMode::Difference => {
                        let title = if self.mode == ImgMode::Overlay {
                            tr("img.overlay")
                        } else {
                            tr("img.difference")
                        };
                        ui.label(egui::RichText::new(title).font(crate::ui::theme::semibold(13.5)));
                        widgets::chip(ui, &dims.join(" / "), pal.text_dim, pal.control);
                    }
                });
            });
            let view = Rect::from_min_size(pos2(x0 + 2.0, avail.top() + header_h), view_size);
            let painter = ui.painter_at(view);
            painter.rect_filled(view, 0.0, c.missing);
            let img_rect =
                Rect::from_min_size(view.min + self.offset, img_size * self.zoom).intersect(view);
            let cell = 10.0;
            if img_rect.is_positive()
                && (img_rect.width() / cell) * (img_rect.height() / cell) < 20_000.0
            {
                let mut y = img_rect.top();
                let mut row = 0;
                while y < img_rect.bottom() {
                    let mut x = img_rect.left();
                    let mut col = row;
                    while x < img_rect.right() {
                        let r = Rect::from_min_max(
                            pos2(x, y),
                            pos2(
                                (x + cell).min(img_rect.right()),
                                (y + cell).min(img_rect.bottom()),
                            ),
                        );
                        painter.rect_filled(r, 0.0, checker[col % 2]);
                        x += cell;
                        col += 1;
                    }
                    y += cell;
                    row += 1;
                }
            }
            let origin = view.min + self.offset;
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let draw = |tex: &Option<TextureHandle>, img_w: u32, img_h: u32, tint: Color32| {
                if let Some(t) = tex {
                    let r =
                        Rect::from_min_size(origin, vec2(img_w as f32, img_h as f32) * self.zoom);
                    painter.image(t.id(), r, uv, tint);
                }
            };
            match self.mode {
                ImgMode::SideBySide => draw(
                    &self.textures[p],
                    self.images[p].width(),
                    self.images[p].height(),
                    Color32::WHITE,
                ),
                ImgMode::Overlay => {
                    draw(
                        &self.textures[0],
                        self.images[0].width(),
                        self.images[0].height(),
                        Color32::WHITE,
                    );
                    let last = self.n() - 1;
                    let a = (self.overlay_alpha * 255.0) as u8;
                    draw(
                        &self.textures[last],
                        self.images[last].width(),
                        self.images[last].height(),
                        Color32::from_white_alpha(a),
                    );
                }
                ImgMode::Difference => draw(
                    &self.diff_tex,
                    self.diff.diff_image.width(),
                    self.diff.diff_image.height(),
                    Color32::WHITE,
                ),
            }
            if self.show_blocks {
                let bs = self.diff.block_size as f32 * self.zoom;
                let (bw, bh) = (self.diff.blocks_x, self.diff.blocks_y);
                let span = |lo: f32, hi: f32, n: u32| {
                    let first = (lo / bs).floor().clamp(0.0, n as f32) as u32;
                    let last = (hi / bs).ceil().clamp(0.0, n as f32) as u32;
                    first..last
                };
                let fill = c.conflict_bar.gamma_multiply(0.22);
                for by in span(view.top() - origin.y, view.bottom() - origin.y, bh) {
                    let cols = span(view.left() - origin.x, view.right() - origin.x, bw);
                    let mut run: Option<u32> = None;
                    for bx in cols.start..=cols.end {
                        let set = bx < cols.end && self.diff.blocks[(by * bw + bx) as usize];
                        match (set, run) {
                            (true, None) => run = Some(bx),
                            (false, Some(start)) => {
                                let r = Rect::from_min_size(
                                    origin + vec2(start as f32 * bs, by as f32 * bs),
                                    vec2((bx - start) as f32 * bs, bs),
                                );
                                painter.rect_filled(r, 0.0, fill);
                                run = None;
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some(i) = self.cur {
                let [x, y, w, h] = self.diff.regions[i];
                let r = Rect::from_min_size(
                    origin + vec2(x as f32, y as f32) * self.zoom,
                    vec2(w as f32, h as f32) * self.zoom,
                );
                painter.rect_stroke(
                    r.expand(2.0),
                    0.0,
                    Stroke::new(2.0_f32, c.cursor_row),
                    StrokeKind::Outside,
                );
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if view.contains(pos) {
                        let px = (pos - origin) / self.zoom;
                        self.cur = self.diff.regions.iter().position(|[x, y, w, h]| {
                            px.x >= *x as f32
                                && px.y >= *y as f32
                                && px.x < (*x + *w) as f32
                                && px.y < (*y + *h) as f32
                        });
                    }
                }
            }
        }
        ui.allocate_rect(avail, Sense::hover());
    }
}
