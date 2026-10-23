//! Binary (hex) compare view.

use crate::config::Config;
use crate::core::binary;
use crate::i18n::{tr, trf};
use crate::ui::theme::Colors;
use crate::ui::widgets::{self, Mark};
use egui::text::LayoutJob;
use egui::{pos2, vec2, Color32, FontId, Rect, Sense, Stroke, TextFormat, Ui};
use std::ops::Range;
use std::path::PathBuf;

pub struct BinaryView {
    pub paths: Vec<PathBuf>,
    pub data: Vec<Vec<u8>>,
    pub modified: Vec<bool>,
    pub read_only: Vec<bool>,
    pub diffs: Vec<Range<usize>>,
    pub cur: Option<usize>,
    pub cursor: Option<usize>,
    scroll: f32,
    scroll_to: Option<usize>,
    undo: Vec<(Vec<Vec<u8>>, Vec<bool>)>,
    redo: Vec<(Vec<Vec<u8>>, Vec<bool>)>,
    pub status: String,
    id: egui::Id,
    bpl: usize,
}

impl BinaryView {
    pub fn load(paths: Vec<PathBuf>) -> anyhow::Result<Self> {
        let mut data = Vec::new();
        for p in &paths {
            if p.exists() {
                data.push(std::fs::read(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?);
            } else {
                data.push(Vec::new());
            }
        }
        let n = paths.len();
        let mut v = BinaryView {
            paths,
            data,
            modified: vec![false; n],
            read_only: vec![false; n],
            diffs: Vec::new(),
            cur: None,
            cursor: None,
            scroll: 0.0,
            scroll_to: None,
            undo: Vec::new(),
            redo: Vec::new(),
            status: String::new(),
            id: egui::Id::new(("binview", std::time::Instant::now())),
            bpl: 16,
        };
        v.recompute();
        if !v.diffs.is_empty() {
            v.select(0);
        }
        Ok(v)
    }

    pub fn n(&self) -> usize {
        self.data.len()
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
        let refs: Vec<&[u8]> = self.data.iter().map(|d| d.as_slice()).collect();
        self.diffs = binary::diff_ranges(&refs);
        if let Some(c) = self.cur {
            if c >= self.diffs.len() {
                self.cur = None;
            }
        }
    }

    fn select(&mut self, i: usize) {
        self.cur = Some(i);
        self.cursor = Some(self.diffs[i].start);
        self.scroll_to = Some(self.diffs[i].start / self.bpl);
    }

    pub fn next_diff(&mut self) {
        let off = self.cursor.map(|c| c as i64).unwrap_or(-1);
        if let Some(i) = self.diffs.iter().position(|r| r.start as i64 > off) {
            self.select(i);
        }
    }

    pub fn prev_diff(&mut self) {
        let off = self.cursor.unwrap_or(usize::MAX);
        if let Some(i) = self.diffs.iter().rposition(|r| r.end <= off) {
            self.select(i);
        }
    }

    pub fn first_diff(&mut self) {
        if !self.diffs.is_empty() {
            self.select(0);
        }
    }

    pub fn last_diff(&mut self) {
        if !self.diffs.is_empty() {
            self.select(self.diffs.len() - 1);
        }
    }

    fn push_undo(&mut self) {
        self.undo.push((self.data.clone(), self.modified.clone()));
        self.redo.clear();
        if self.undo.len() > 20 {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self) {
        if let Some((d, m)) = self.undo.pop() {
            self.redo.push((
                std::mem::replace(&mut self.data, d),
                std::mem::replace(&mut self.modified, m),
            ));
            self.recompute();
        }
    }

    pub fn redo(&mut self) {
        if let Some((d, m)) = self.redo.pop() {
            self.undo.push((
                std::mem::replace(&mut self.data, d),
                std::mem::replace(&mut self.modified, m),
            ));
            self.recompute();
        }
    }

    pub fn copy_range(&mut self, i: usize, from: usize, to: usize) {
        if self.read_only[to] {
            self.status = tr("msg.readonly").into();
            return;
        }
        let r = self.diffs[i].clone();
        self.push_undo();
        let src_end = r.end.min(self.data[from].len());
        let src = self.data[from][r.start.min(src_end)..src_end].to_vec();
        let dst_end = r.end.min(self.data[to].len());
        let dst_start = r.start.min(dst_end);
        self.data[to].splice(dst_start..dst_end, src);
        self.modified[to] = true;
        self.recompute();
        let pos = r.start;
        if let Some(ni) = self.diffs.iter().position(|d| d.start >= pos) {
            self.select(ni);
        } else {
            self.cur = None;
        }
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
        self.copy_range(i, from, to);
    }

    pub fn copy_all(&mut self, to_right: bool) {
        let (from, to) = if to_right {
            (0, 1)
        } else {
            (self.n() - 1, self.n() - 2)
        };
        if self.read_only[to] {
            return;
        }
        self.push_undo();
        self.data[to] = self.data[from].clone();
        self.modified[to] = true;
        self.recompute();
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        for i in 0..self.n() {
            if self.modified[i] {
                std::fs::write(&self.paths[i], &self.data[i])?;
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
        let mut s = if self.diffs.is_empty() {
            tr("msg.binary_identical").to_string()
        } else {
            match self.cur {
                Some(c) => trf(
                    "msg.diff_of",
                    &[
                        ("n", (c + 1).to_string()),
                        ("total", self.diffs.len().to_string()),
                    ],
                ),
                None => trf("msg.diff_count", &[("total", self.diffs.len().to_string())]),
            }
        };
        let sizes: Vec<String> = self
            .data
            .iter()
            .map(|d| crate::core::report::fmt_size(Some(d.len() as u64)))
            .collect();
        s.push_str(&format!("  |  {}: {}", tr("col.size"), sizes.join(" / ")));
        if let Some(c) = self.cursor {
            s.push_str(&format!("  |  {} 0x{c:08X} ({c})", tr("label.offset")));
        }
        if !self.status.is_empty() {
            s.push_str("  |  ");
            s.push_str(&self.status);
        }
        s
    }

    pub fn ui(&mut self, ui: &mut Ui, cfg: &Config, c: &Colors) {
        if self.bpl != cfg.binary_bytes_per_line.clamp(4, 64) {
            self.bpl = cfg.binary_bytes_per_line.clamp(4, 64);
        }
        let bpl = self.bpl;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            if widgets::icon_button(ui, "⇈", tr("cmd.first_diff")).clicked() {
                self.first_diff();
            }
            if widgets::icon_button(ui, "↑", tr("cmd.prev_diff")).clicked() {
                self.prev_diff();
            }
            let counter = match self.cur {
                Some(c) => format!("{} / {}", c + 1, self.diffs.len()),
                None => format!("– / {}", self.diffs.len()),
            };
            widgets::chip(ui, &counter, pal.text, pal.control);
            if widgets::icon_button(ui, "↓", tr("cmd.next_diff")).clicked() {
                self.next_diff();
            }
            if widgets::icon_button(ui, "⇊", tr("cmd.last_diff")).clicked() {
                self.last_diff();
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, "→", tr("cmd.copy_right")).clicked() {
                self.copy_current(true);
            }
            if widgets::icon_button(ui, "←", tr("cmd.copy_left")).clicked() {
                self.copy_current(false);
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button_enabled(ui, !self.undo.is_empty(), "↶", tr("cmd.undo"))
                .clicked()
            {
                self.undo();
            }
            if widgets::icon_button_enabled(ui, !self.redo.is_empty(), "↷", tr("cmd.redo"))
                .clicked()
            {
                self.redo();
            }
        });
        let n = self.n();
        let avail = ui.available_rect_before_wrap();
        let header_h = 36.0;
        let loc_w = if cfg.show_location_pane {
            14.0 + 8.0 * n as f32
        } else {
            0.0
        };
        let vbar_w = 12.0;
        let left = avail.left() + loc_w;
        let right = avail.right() - vbar_w;
        let pane_w = (right - left) / n as f32;
        let top = avail.top() + header_h;
        let bottom = avail.bottom();
        let font = FontId::monospace(cfg.font_size);
        let row_h = ui.fonts(|f| f.row_height(&font)) + 2.0;
        let char_w = ui.fonts(|f| f.glyph_width(&font, '0'));
        let visible = (bottom - top) / row_h;
        let max_len = self.data.iter().map(|d| d.len()).max().unwrap_or(0);
        let total = max_len.div_ceil(bpl).max(1);

        let pal = widgets::pal(ui);
        let header_rect =
            Rect::from_min_max(avail.min, pos2(avail.right(), avail.top() + header_h));
        ui.painter().rect_filled(header_rect, 0.0, pal.surface_alt);
        ui.painter().hline(
            header_rect.x_range(),
            header_rect.bottom() - 0.5,
            Stroke::new(1.0_f32, pal.border),
        );
        for p in 0..n {
            let r = Rect::from_min_size(
                pos2(left + p as f32 * pane_w + 8.0, avail.top()),
                vec2(pane_w - 16.0, header_h),
            );
            let path = self.paths[p].display().to_string();
            let mut title = self.paths[p]
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            if self.modified[p] {
                title.push_str("  ●");
            }
            let size = crate::core::report::fmt_size(Some(self.data[p].len() as u64));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                ui.horizontal_centered(|ui| {
                    widgets::side_chip(ui, p, n);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let lock = if self.read_only[p] { "🔒" } else { "🔓" };
                        widgets::toggle_button(
                            ui,
                            &mut self.read_only[p],
                            lock,
                            tr("label.read_only"),
                        );
                        widgets::chip(ui, &size, pal.text_dim, pal.control);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(title)
                                        .font(crate::ui::theme::semibold(13.5)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(path);
                        });
                    });
                });
            });
        }

        let body = Rect::from_min_max(pos2(left, top), pos2(right, bottom));
        let resp = ui.interact(body, self.id.with("body"), Sense::click());
        if ui.rect_contains_pointer(body) {
            self.scroll -= ui.input(|i| i.smooth_scroll_delta.y) / row_h;
        }
        if let Some(line) = self.scroll_to.take() {
            let l = line as f32;
            if l < self.scroll || l > self.scroll + visible - 2.0 {
                self.scroll = l - (visible / 3.0).floor();
            }
        }
        let max_scroll = (total as f32 - visible + 1.0).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let first = self.scroll.floor() as usize;
        let last = (first + visible.ceil() as usize + 1).min(total);
        let hex_x = 10.0 * char_w + 4.0;

        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let line = (self.scroll + (pos.y - top) / row_h).floor() as usize;
                let p = (((pos.x - left) / pane_w).floor() as usize).min(n - 1);
                let x = pos.x - (left + p as f32 * pane_w) - hex_x;
                if x >= 0.0 {
                    let col = (x / (3.0 * char_w)).floor() as usize;
                    if col < bpl {
                        let off = line * bpl + col;
                        self.cursor = Some(off);
                        self.cur = self.diffs.iter().position(|r| r.contains(&off));
                    }
                }
            }
        }

        let refs: Vec<&[u8]> = self.data.iter().map(|d| d.as_slice()).collect();
        let cur_range = self.cur.map(|i| self.diffs[i].clone());
        for p in 0..n {
            let x0 = left + p as f32 * pane_w;
            let rect = Rect::from_min_max(pos2(x0, top), pos2(x0 + pane_w - 2.0, bottom));
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, c.text_bg);
            for line in first..last {
                let y = top + (line as f32 - self.scroll) * row_h;
                let mut job = LayoutJob::default();
                let fmt = |color: Color32, bg: Color32| TextFormat {
                    font_id: font.clone(),
                    color,
                    background: bg,
                    ..Default::default()
                };
                job.append(
                    &format!("{:08X}  ", line * bpl),
                    0.0,
                    fmt(c.gutter_text, Color32::TRANSPARENT),
                );
                let mut ascii = LayoutJob::default();
                for k in 0..bpl {
                    let off = line * bpl + k;
                    let byte = refs[p].get(off).copied();
                    let differs = off < max_len && binary::byte_differs(&refs, off);
                    let sel = cur_range.as_ref().is_some_and(|r| r.contains(&off));
                    let bg = match (byte, differs, sel) {
                        (None, _, _) if off < max_len => c.missing,
                        (_, true, true) => c.inline_sel,
                        (_, true, false) => c.inline,
                        _ => Color32::TRANSPARENT,
                    };
                    let bg = if self.cursor == Some(off) {
                        c.find_hit
                    } else {
                        bg
                    };
                    let hex = byte
                        .map(|b| format!("{b:02X}"))
                        .unwrap_or_else(|| "  ".into());
                    job.append(&hex, 0.0, fmt(c.text, bg));
                    job.append(" ", 0.0, fmt(c.text, Color32::TRANSPARENT));
                    let ch = match byte {
                        Some(b) if (0x20..0x7f).contains(&b) => b as char,
                        Some(_) => '.',
                        None => ' ',
                    };
                    ascii.append(&ch.to_string(), 0.0, fmt(c.text, bg));
                }
                job.append(" ", 0.0, fmt(c.text, Color32::TRANSPARENT));
                for s in ascii.sections.iter() {
                    let t = &ascii.text[s.byte_range.clone()];
                    job.append(t, 0.0, s.format.clone());
                }
                let galley = ui.fonts(|f| f.layout_job(job));
                painter.galley(pos2(x0 + 4.0, y + 1.0), galley, c.text);
            }
            ui.painter().line_segment(
                [
                    pos2(x0 + pane_w - 1.0, avail.top()),
                    pos2(x0 + pane_w - 1.0, bottom),
                ],
                Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
        }

        if cfg.show_location_pane {
            let loc = Rect::from_min_max(
                pos2(avail.left(), top),
                pos2(avail.left() + loc_w - 2.0, bottom),
            );
            let marks: Vec<Mark> = self
                .diffs
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let start = r.start / bpl;
                    let end = (r.end.saturating_sub(1)) / bpl;
                    let color = if self.cur == Some(i) {
                        c.ui.accent
                    } else {
                        c.change_bar
                    };
                    Mark {
                        start,
                        len: end - start + 1,
                        columns: [Some(color); 3],
                    }
                })
                .collect();
            if let Some(t) = widgets::location_pane(
                ui,
                self.id.with("loc"),
                loc,
                total,
                n,
                &marks,
                self.scroll,
                visible,
                c.location_bg,
                c.viewport,
            ) {
                self.scroll = (t - visible / 2.0).clamp(0.0, max_scroll);
            }
        }
        let vbar = Rect::from_min_max(pos2(right, top), pos2(avail.right(), bottom));
        widgets::vscrollbar(
            ui,
            self.id.with("vbar"),
            vbar,
            &mut self.scroll,
            total + 1,
            visible,
        );
        ui.allocate_rect(avail, Sense::hover());
    }
}
