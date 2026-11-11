//! Image compare view.

use crate::config::Config;
use crate::core::image_cmp::{self, ImageDiff};
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::theme::Colors;
use crate::ui::widgets::{self, Icon, Outcome};
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
    /// The first image left of a draggable divider, the last one right of it.
    Swipe,
    /// The first and last images in turn.
    Blink,
    Difference,
}

const BLINK_SECS: f64 = 0.6;
/// A block whose pixels are off by less than this in every channel is one the eye may not
/// tell apart in the two images: its mark blinks with them.
const FAINT: u8 = 24;
/// How strongly a pixel that differs is marked, and one of a block that differs faintly
/// while the blink view shows the last image.
const MARK: f32 = 0.4;
const FLASH: f32 = 0.65;

pub struct ImageView {
    pub paths: Vec<PathBuf>,
    /// Shown instead of the file names in the pane headers.
    pub descriptions: Vec<Option<String>>,
    images: Vec<RgbaImage>,
    textures: Vec<Option<TextureHandle>>,
    diff_tex: Option<TextureHandle>,
    /// The pixels that differ, white on nothing, to be drawn over the images in the color of
    /// the marks: those of blocks that differ plainly, and those of blocks that differ
    /// faintly. `None` where there are no such pixels.
    mask_tex: [Option<TextureHandle>; 2],
    textures_dirty: bool,
    diff_dirty: bool,
    diff: ImageDiff,
    mode: ImgMode,
    zoom: f32,
    offset: Vec2,
    fit: bool,
    center: bool,
    pub show_blocks: bool,
    overlay_alpha: f32,
    /// Divider position of the swipe mode, as a fraction of the view width.
    swipe: f32,
    threshold: u8,
    block: u32,
    cur: Option<usize>,
    pub modified: Vec<bool>,
    pub read_only: Vec<bool>,
    undo: Vec<(Vec<RgbaImage>, Vec<bool>)>,
    pub status: String,
    /// Set when difference navigation ran past the last (true) or first (false) difference,
    /// so the app can move on to the next or previous file of the folder compare.
    pub step_file: Option<bool>,
    /// The pixel under the pointer, which the status line tells the place and colors of.
    probe: Option<(u32, u32)>,
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
            mask_tex: [None, None],
            textures_dirty: true,
            diff_dirty: true,
            diff,
            mode: ImgMode::SideBySide,
            swipe: 0.5,
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
            read_only: vec![false; n],
            undo: Vec::new(),
            status: String::new(),
            id: egui::Id::unique(("imgview", std::time::Instant::now())),
            step_file: None,
            probe: None,
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

    /// Selects the next difference; false when the last one is already selected.
    pub fn next_diff(&mut self) -> bool {
        let i = self.cur.map_or(0, |c| c + 1);
        if i >= self.diff.regions.len() {
            return false;
        }
        self.cur = Some(i);
        self.center_on_current();
        true
    }

    /// Selects the previous difference; false when the first one is already selected.
    pub fn prev_diff(&mut self) -> bool {
        let i = match self.cur {
            Some(0) => return false,
            Some(c) => c - 1,
            None => match self.diff.regions.len() {
                0 => return false,
                n => n - 1,
            },
        };
        self.cur = Some(i);
        self.center_on_current();
        true
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
        if self.read_only[to] {
            self.status = tr("msg.readonly").into();
            return;
        }
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
                anyhow::ensure!(!self.read_only[i], "{}", tr("msg.readonly"));
                let format = image::ImageFormat::from_path(&self.paths[i])?;
                let mut bytes = std::io::Cursor::new(Vec::new());
                self.images[i].write_to(&mut bytes, format)?;
                crate::core::fsutil::write_atomic(&self.paths[i], bytes.get_ref())?;
                // Undo snapshots must describe dirtiness relative to this save, not the
                // disk contents that existed when the snapshot was taken.
                for (images, modified) in &mut self.undo {
                    modified[i] = images[i] != self.images[i];
                }
                self.modified[i] = false;
                self.status = trf(
                    "msg.saved",
                    &[("file", self.paths[i].display().to_string())],
                );
            }
        }
        Ok(())
    }

    pub fn outcome(&self) -> Outcome {
        if self.diff.diff_pixels == 0 {
            Outcome::Identical
        } else {
            Outcome::Different
        }
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

    /// The status line for the status bar: with the pixel under the pointer, its place, its
    /// color in each image and the most these are apart in a channel. Not part of
    /// [`Self::status_line`], which is also what a run without a user reports and must not
    /// depend on where the pointer happens to be.
    pub fn status_bar_line(&self) -> String {
        let mut s = self.status_line();
        let Some((x, y)) = self.probe else {
            return s;
        };
        // The same place in every image: they are shown at one zoom and offset. These are the
        // colors of the images whatever the view draws there, a blend or the difference.
        let pixels: Vec<Option<[u8; 4]>> = self
            .images
            .iter()
            .map(|img| img.get_pixel_checked(x, y).map(|p| p.0))
            .collect();
        let colors: Vec<String> = pixels
            .iter()
            .map(|p| match p {
                Some([r, g, b, 255]) => format!("RGB({r}, {g}, {b})"),
                Some([r, g, b, a]) => format!("RGBA({r}, {g}, {b}, {a})"),
                // A smaller image has nothing there.
                None => "–".to_string(),
            })
            .collect();
        s.push_str(&format!("  |  X: {x}, Y: {y}  |  {}", colors.join("  ↔  ")));
        if let Some(Some(first)) = pixels.first() {
            let apart = pixels[1..]
                .iter()
                .flatten()
                .flat_map(|p| (0..4).map(|i| p[i].abs_diff(first[i])))
                .max();
            if let Some(apart) = apart {
                s.push_str(&format!("  |  Δ {apart}"));
            }
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
        let (w, h) = (self.diff.width as usize, self.diff.height as usize);
        let (bs, bw) = (self.diff.block_size as usize, self.diff.blocks_x as usize);
        for (class, tex) in self.mask_tex.iter_mut().enumerate() {
            let mut any = false;
            let pixels: Vec<Color32> = (self.diff.mask.iter().enumerate())
                .map(|(i, &differs)| {
                    let block = (i / w / bs) * bw + (i % w) / bs;
                    if differs && is_faint(self.diff.strength[block]) == (class == 1) {
                        any = true;
                        Color32::WHITE
                    } else {
                        Color32::TRANSPARENT
                    }
                })
                .collect();
            *tex = any.then(|| {
                ctx.load_texture(
                    format!("{:?}mask{class}", self.id),
                    ColorImage::new([w, h], pixels),
                    TextureOptions::NEAREST,
                )
            });
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, _cfg: &Config, c: &Colors) {
        self.ensure_textures(ui.ctx());
        let mut recompute = false;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            let compact = ui.available_width() < 1180.0;
            widgets::segmented(
                ui,
                &mut self.mode,
                &[
                    (ImgMode::SideBySide, tr("img.side_by_side")),
                    (ImgMode::Overlay, tr("img.overlay")),
                    (ImgMode::Swipe, tr("img.swipe")),
                    (ImgMode::Blink, tr("img.blink")),
                    (ImgMode::Difference, tr("img.difference")),
                ],
            );
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, Icon::ZoomOut, tr("cmd.zoom_out")).clicked() {
                self.zoom = (self.zoom / 1.25).max(0.02);
            }
            widgets::chip(
                ui,
                &format!("{:.0}%", self.zoom * 100.0),
                pal.text,
                pal.control,
            );
            if widgets::icon_button(ui, Icon::ZoomIn, tr("cmd.zoom_in")).clicked() {
                self.zoom = (self.zoom * 1.25).min(64.0);
            }
            let fit = if compact {
                widgets::icon_button(ui, Icon::Fit, tr("img.fit"))
            } else {
                widgets::ghost_icon_button(ui, Icon::Fit, tr("img.fit"))
            };
            if fit.clicked() {
                self.fit = true;
            }
            if widgets::ghost_button(ui, "1:1").clicked() {
                self.zoom = 1.0;
                self.offset = Vec2::ZERO;
            }
            widgets::toolbar_sep(ui);
            widgets::toggle_icon_button(
                ui,
                &mut self.show_blocks,
                Some(Icon::Table),
                if compact { "" } else { tr("img.show_blocks") },
                tr("img.show_blocks"),
            );
            // Comparing every pixel is too slow to repeat on each frame of a drag.
            let settled = |r: egui::Response| (r.changed() && !r.dragged()) || r.drag_stopped();
            let caption = |ui: &mut Ui, label: &str| {
                ui.add_space(6.0);
                if !compact {
                    ui.label(egui::RichText::new(label).color(pal.text_dim));
                }
            };
            caption(ui, tr("img.threshold"));
            let r = ui.add(egui::DragValue::new(&mut self.threshold).range(0..=255));
            recompute |= settled(r.on_hover_text(tr("img.threshold")));
            caption(ui, tr("img.block_size"));
            let r = ui.add(egui::DragValue::new(&mut self.block).range(1..=256));
            recompute |= settled(r.on_hover_text(tr("img.block_size")));
            if self.mode == ImgMode::Overlay {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(tr("img.alpha")).color(pal.text_dim));
                ui.add(egui::Slider::new(&mut self.overlay_alpha, 0.0..=1.0).show_value(false));
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button_hint(ui, Icon::Prev, &Cmd::PrevDiff.tip(), tr("tip.nav_diff"))
                .clicked()
                && !self.prev_diff()
            {
                self.step_file = Some(false);
            }
            if widgets::icon_button_hint(ui, Icon::Next, &Cmd::NextDiff.tip(), tr("tip.nav_diff"))
                .clicked()
                && !self.next_diff()
            {
                self.step_file = Some(true);
            }
            if widgets::icon_button(ui, Icon::CopyRight, &Cmd::CopyRight.tip()).clicked() {
                self.copy_current(true);
            }
            if widgets::icon_button(ui, Icon::CopyLeft, &Cmd::CopyLeft.tip()).clicked() {
                self.copy_current(false);
            }
            if widgets::icon_button_enabled(ui, !self.undo.is_empty(), Icon::Undo, &Cmd::Undo.tip())
                .clicked()
            {
                self.undo();
            }
        });
        if self.outcome() == Outcome::Identical {
            let color = widgets::pal(ui).success;
            widgets::info_bar(ui, Icon::Check, tr("msg.images_identical"), color);
        }
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

        // The pane a place across the view is in, and where a pane shows its image.
        let pane_at =
            |x: f32| (((x - avail.left()) / pane_w).floor().max(0.0) as usize).min(panes - 1);
        let view_of = |pane: usize| {
            Rect::from_min_size(
                pos2(
                    avail.left() + pane as f32 * pane_w + 2.0,
                    avail.top() + header_h,
                ),
                view_size,
            )
        };
        let all_rect = Rect::from_min_max(pos2(avail.left(), avail.top() + header_h), avail.max);
        let resp = ui.interact(all_rect, self.id.with("drag"), Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Image, true, tr("help.image_compare"))
        });
        if resp.dragged() {
            self.offset += resp.drag_delta();
        }
        if let Some(hover) = resp.hover_pos() {
            // Trackpads scroll in points and pan; mouse wheels scroll in lines and zoom.
            // Pinching and Ctrl/Cmd+scroll arrive as zoom_delta.
            let (mut factor, pan) = ui.input(|i| {
                let mut factor = i.zoom_delta();
                let mut pan = Vec2::ZERO;
                for e in &i.events {
                    let egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } = e
                    else {
                        continue;
                    };
                    if modifiers.command || modifiers.ctrl {
                        continue;
                    }
                    match unit {
                        egui::MouseWheelUnit::Point => pan += *delta,
                        _ if modifiers.shift => pan.x += delta.y * 40.0,
                        _ => factor *= 1.1_f32.powf(delta.y),
                    }
                }
                (factor, pan)
            });
            self.offset += pan;
            if factor != 1.0 {
                let local = hover - view_of(pane_at(hover.x)).min;
                let old = self.zoom;
                let new = (old * factor).clamp(0.02, 64.0);
                factor = new / old;
                self.offset = local - (local - self.offset) * factor;
                self.zoom = new;
            }
        }

        // The pixel under the pointer, for the status bar: none while the images are dragged
        // about, nor under the handle of the swipe view, which is not a place in an image.
        let probe = resp
            .hover_pos()
            .filter(|_| !resp.dragged())
            .and_then(|hover| {
                let view = view_of(pane_at(hover.x));
                let handle = view.left() + view.width() * self.swipe;
                if self.mode == ImgMode::Swipe && (hover.x - handle).abs() <= 8.0 {
                    return None;
                }
                let at = (hover - view.min - self.offset) / self.zoom;
                let inside = view.contains(hover)
                    && (0.0..img_size.x).contains(&at.x)
                    && (0.0..img_size.y).contains(&at.y);
                inside.then_some((at.x as u32, at.y as u32))
            });
        if probe != self.probe {
            self.probe = probe;
            // The status bar was drawn before this view. A pointer on the move draws the next
            // frame itself; this one is for the pixel it comes to rest on.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
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
        let checker = pal.checker;
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
                                        egui::Label::new(egui::RichText::new(name).font(
                                            crate::ui::theme::semibold(
                                                crate::ui::theme::size::CONTROL,
                                            ),
                                        ))
                                        .truncate(),
                                    )
                                    .on_hover_text(path);
                                },
                            );
                        });
                    }
                    mode => {
                        let title = match mode {
                            ImgMode::Overlay => tr("img.overlay"),
                            ImgMode::Swipe => tr("img.swipe"),
                            ImgMode::Blink => tr("img.blink"),
                            _ => tr("img.difference"),
                        };
                        ui.label(
                            egui::RichText::new(title)
                                .font(crate::ui::theme::semibold(crate::ui::theme::size::CONTROL)),
                        );
                        widgets::chip(ui, &dims.join(" / "), pal.text_dim, pal.control);
                        if mode == ImgMode::Blink {
                            widgets::side_chip(ui, self.blink_side(ui), n);
                        }
                    }
                });
            });
            let view = view_of(p);
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
                ImgMode::Swipe => {
                    let last = self.n() - 1;
                    let split = view.left() + view.width() * self.swipe;
                    let handle = Rect::from_center_size(
                        pos2(split, view.center().y),
                        vec2(16.0, view.height()),
                    );
                    let h = ui
                        .interact(handle, self.id.with("swipe"), Sense::drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                    if let Some(pos) = h.interact_pointer_pos().filter(|_| h.dragged()) {
                        self.swipe = ((pos.x - view.left()) / view.width()).clamp(0.0, 1.0);
                    }
                    let split = view.left() + view.width() * self.swipe;
                    for (i, clip) in [
                        (0, Rect::from_min_max(view.min, pos2(split, view.bottom()))),
                        (last, Rect::from_min_max(pos2(split, view.top()), view.max)),
                    ] {
                        if let Some(t) = &self.textures[i] {
                            let size = vec2(
                                self.images[i].width() as f32,
                                self.images[i].height() as f32,
                            );
                            let r = Rect::from_min_size(origin, size * self.zoom);
                            painter
                                .with_clip_rect(clip)
                                .image(t.id(), r, uv, Color32::WHITE);
                        }
                    }
                    let stroke = if h.hovered() || h.dragged() {
                        Stroke::new(3.0_f32, c.ui.accent)
                    } else {
                        Stroke::new(2.0_f32, Color32::WHITE)
                    };
                    painter.vline(
                        split,
                        view.y_range(),
                        Stroke::new(4.0_f32, Color32::BLACK.gamma_multiply(0.35)),
                    );
                    painter.vline(split, view.y_range(), stroke);
                    painter.circle(
                        pos2(split, view.center().y),
                        9.0,
                        c.ui.accent,
                        Stroke::new(2.0_f32, Color32::WHITE),
                    );
                }
                ImgMode::Blink => {
                    let i = self.blink_side(ui);
                    draw(
                        &self.textures[i],
                        self.images[i].width(),
                        self.images[i].height(),
                        Color32::WHITE,
                    );
                    let t = ui.input(|i| i.time);
                    let left = BLINK_SECS - t % BLINK_SECS;
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(left));
                }
                ImgMode::Difference => draw(
                    &self.diff_tex,
                    self.diff.diff_image.width(),
                    self.diff.diff_image.height(),
                    Color32::WHITE,
                ),
            }
            // While the images blink, the marks of what differs faintly blink with them, on the
            // last image only and stronger: a pixel one shade off looks the same in both
            // images, and under a mark that stayed it could not be told to differ at all. What
            // differs plainly is seen to blink by itself, under the mark it always has.
            let blink = (self.mode == ImgMode::Blink).then(|| self.blink_side(ui) != 0);
            let whole = Rect::from_min_size(origin, img_size * self.zoom);
            for (faint, strength) in block_marks(self.show_blocks, blink) {
                let tint = c.conflict_bar.gamma_multiply(strength);
                for (class, tex) in self.mask_tex.iter().enumerate() {
                    if let (Some(t), true) = (tex, faint.is_none_or(|f| f == (class == 1))) {
                        painter.image(t.id(), whole, uv, tint);
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

    /// The image shown by the blink mode now: the first one or the last one.
    fn blink_side(&self, ui: &Ui) -> usize {
        if ((ui.input(|i| i.time) / BLINK_SECS) as u64).is_multiple_of(2) {
            0
        } else {
            self.n() - 1
        }
    }
}

/// Whether a block whose first and last image are `strength` apart at most differs so little
/// between them that the eye may not see it change.
fn is_faint(strength: u8) -> bool {
    (1..FAINT).contains(&strength)
}

/// The marks drawn over the pixels that differ: those of which blocks, by whether the blocks
/// differ faintly (`None` for all of them), and how strongly. `blink` is whether the blink view shows the
/// last image now, and `None` in the other views.
fn block_marks(shown: bool, blink: Option<bool>) -> Vec<(Option<bool>, f32)> {
    match (shown, blink) {
        (false, _) => Vec::new(),
        (true, None) => vec![(None, MARK)],
        (true, Some(false)) => vec![(Some(false), MARK)],
        (true, Some(true)) => vec![(Some(false), MARK), (Some(true), FLASH)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In the blink view the marks of blocks that differ faintly are there with the last
    /// image only, so they blink; every other mark stays, as in the other views.
    #[test]
    fn faint_blocks_blink_with_the_images() {
        assert!(block_marks(false, Some(true)).is_empty());
        assert_eq!(block_marks(true, None), [(None, MARK)]);
        assert_eq!(block_marks(true, Some(false)), [(Some(false), MARK)]);
        assert_eq!(
            block_marks(true, Some(true)),
            [(Some(false), MARK), (Some(true), FLASH)]
        );
        // A shade apart is faint; the same, or far apart, is not.
        assert!(is_faint(1) && is_faint(FAINT - 1));
        assert!(!is_faint(0) && !is_faint(FAINT) && !is_faint(255));
    }

    #[test]
    fn the_status_line_tells_the_pixel_under_the_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let pixels = [
            (2, image::Rgba([10, 20, 30, 255])),
            (1, image::Rgba([1, 2, 3, 128])),
        ];
        let paths: Vec<_> = pixels
            .iter()
            .enumerate()
            .map(|(i, (width, color))| {
                let path = dir.path().join(format!("{i}.png"));
                RgbaImage::from_pixel(*width, 1, *color)
                    .save(&path)
                    .unwrap();
                path
            })
            .collect();
        let mut v = ImageView::load(paths, &Config::default()).unwrap();
        assert_eq!(v.status_bar_line(), v.status_line());
        v.probe = Some((0, 0));
        let status = v.status_bar_line();
        assert!(
            status.ends_with("X: 0, Y: 0  |  RGB(10, 20, 30)  ↔  RGBA(1, 2, 3, 128)  |  Δ 127"),
            "{status}"
        );
        // What a run without a user reports does not depend on the pointer.
        assert!(!v.status_line().contains("X:"), "{}", v.status_line());
        // The second image is one pixel wide: it has nothing at the second.
        v.probe = Some((1, 0));
        let status = v.status_bar_line();
        assert!(
            status.ends_with("X: 1, Y: 0  |  RGB(10, 20, 30)  ↔  –"),
            "{status}"
        );
    }

    #[test]
    fn image_locks_and_undo_after_save_protect_disk_contents() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a.png", "b.png"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        for (i, path) in paths.iter().enumerate() {
            RgbaImage::from_pixel(1, 1, image::Rgba([i as u8 * 255, 0, 0, 255]))
                .save(path)
                .unwrap();
        }
        let original = std::fs::read(&paths[1]).unwrap();
        let mut v = ImageView::load(paths.clone(), &Config::default()).unwrap();
        v.first_diff();
        v.read_only[1] = true;
        v.copy_current(true);
        assert!(!v.is_modified());
        v.save().unwrap();
        assert_eq!(std::fs::read(&paths[1]).unwrap(), original);
        v.read_only[1] = false;
        v.copy_current(true);
        v.save().unwrap();
        v.undo();
        assert!(v.is_modified());
        v.read_only[1] = true;
        assert!(v.save().is_err());
        v.read_only[1] = false;
        v.save().unwrap();
        assert_eq!(
            image::open(&paths[1]).unwrap().to_rgba8().get_pixel(0, 0).0,
            [255, 0, 0, 255]
        );
    }
}
