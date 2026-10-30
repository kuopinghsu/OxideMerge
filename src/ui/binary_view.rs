//! Binary (hex) compare view.

use crate::config::Config;
use crate::core::binary;
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::theme::Colors;
use crate::ui::widgets::{self, Icon, Mark, Outcome};
use egui::text::LayoutJob;
use egui::{pos2, vec2, Align2, Color32, FontId, Key, Rect, Sense, Stroke, TextFormat, Ui};
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
    /// Message about an edit that changed many differences at once, for an undo toast.
    pub bulk_edit: Option<String>,
    /// Set when difference navigation ran past the last (true) or first (false) difference,
    /// so the app can move on to the next or previous file of the folder compare.
    pub step_file: Option<bool>,
    pub find: crate::ui::text_view::FindState,
    /// The bytes found last, highlighted.
    hit: Option<Range<usize>>,
    /// Text of the open "Go to offset" dialog.
    pub goto: Option<String>,
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
            bulk_edit: None,
            id: egui::Id::new(("binview", std::time::Instant::now())),
            step_file: None,
            find: Default::default(),
            hit: None,
            goto: None,
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

    /// Selects the next difference; false when there is none after the cursor.
    pub fn next_diff(&mut self) -> bool {
        let off = self.cursor.map(|c| c as i64).unwrap_or(-1);
        let found = self.diffs.iter().position(|r| r.start as i64 > off);
        if let Some(i) = found {
            self.select(i);
        }
        found.is_some()
    }

    /// Selects the previous difference; false when there is none before the cursor.
    pub fn prev_diff(&mut self) -> bool {
        let off = self.cursor.unwrap_or(usize::MAX);
        let found = self.diffs.iter().rposition(|r| r.end <= off);
        if let Some(i) = found {
            self.select(i);
        }
        found.is_some()
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

    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
    }

    /// Finds the hex bytes or text of the find bar in any pane, after or before the cursor,
    /// wrapping around at the end.
    pub fn find_next(&mut self, forward: bool) {
        let Some(needle) = binary::parse_pattern(&self.find.query) else {
            return;
        };
        let search = |from: Option<usize>| {
            let hits = self
                .data
                .iter()
                .filter_map(|d| binary::find(d, &needle, from, forward));
            if forward {
                hits.min()
            } else {
                hits.max()
            }
        };
        match search(self.cursor).or_else(|| search(None)) {
            Some(off) => {
                self.go_to(off);
                self.hit = Some(off..off + needle.len());
                self.status.clear();
            }
            None => self.status = trf("msg.not_found", &[("text", self.find.query.clone())]),
        }
    }

    pub fn go_to(&mut self, off: usize) {
        let max = self.data.iter().map(|d| d.len()).max().unwrap_or(0);
        let off = off.min(max.saturating_sub(1));
        self.cursor = Some(off);
        self.cur = self.diffs.iter().position(|r| r.contains(&off));
        self.scroll_to = Some(off / self.bpl);
        self.hit = None;
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
        if self.read_only[to] || self.diffs.is_empty() {
            return;
        }
        let n = self.diffs.len();
        self.push_undo();
        self.data[to] = self.data[from].clone();
        self.modified[to] = true;
        self.recompute();
        self.bulk_edit = Some(trf("msg.copied_blocks", &[("n", n.to_string())]));
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        for i in 0..self.n() {
            if self.modified[i] {
                anyhow::ensure!(!self.read_only[i], "{}", tr("msg.readonly"));
                crate::core::fsutil::write_atomic(&self.paths[i], &self.data[i])?;
                for (data, modified) in self.undo.iter_mut().chain(self.redo.iter_mut()) {
                    modified[i] = data[i] != self.data[i];
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
        if self.diffs.is_empty() {
            Outcome::Identical
        } else {
            Outcome::Different
        }
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
            let key = widgets::with_key;
            if widgets::icon_button(ui, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
                self.first_diff();
            }
            if widgets::icon_button(ui, Icon::Prev, &Cmd::PrevDiff.tip()).clicked()
                && !self.prev_diff()
            {
                self.step_file = Some(false);
            }
            let counter = match self.cur {
                Some(c) => format!("{} / {}", c + 1, self.diffs.len()),
                None => format!("– / {}", self.diffs.len()),
            };
            widgets::chip(ui, &counter, pal.text, pal.control);
            if widgets::icon_button(ui, Icon::Next, &Cmd::NextDiff.tip()).clicked()
                && !self.next_diff()
            {
                self.step_file = Some(true);
            }
            if widgets::icon_button(ui, Icon::Last, &Cmd::LastDiff.tip()).clicked() {
                self.last_diff();
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, Icon::CopyRight, &Cmd::CopyRight.tip()).clicked() {
                self.copy_current(true);
            }
            if widgets::icon_button(ui, Icon::CopyLeft, &Cmd::CopyLeft.tip()).clicked() {
                self.copy_current(false);
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button_enabled(ui, !self.undo.is_empty(), Icon::Undo, &Cmd::Undo.tip())
                .clicked()
            {
                self.undo();
            }
            if widgets::icon_button_enabled(ui, !self.redo.is_empty(), Icon::Redo, &Cmd::Redo.tip())
                .clicked()
            {
                self.redo();
            }
            widgets::toolbar_sep(ui);
            if widgets::icon_button(ui, Icon::Search, &Cmd::Find.tip()).clicked() {
                self.open_find();
            }
            if widgets::icon_button(
                ui,
                Icon::Method,
                &key(tr("cmd.goto_offset"), Cmd::GotoLine.key()),
            )
            .clicked()
            {
                self.goto = Some(String::new());
            }
        });
        if self.find.open {
            self.find_bar(ui);
        }
        if self.outcome() == Outcome::Identical {
            let color = widgets::pal(ui).success;
            widgets::info_bar(ui, Icon::Check, tr("msg.binary_identical"), color);
        }
        let n = self.n();
        let avail = ui.available_rect_before_wrap();
        let header_h = 36.0;
        let loc_w = if cfg.show_location_pane {
            14.0 + 8.0 * n as f32
        } else {
            0.0
        };
        let vbar_w = 12.0;
        let left = avail.left();
        let right = avail.right() - vbar_w - loc_w;
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
                        let lock = if self.read_only[p] {
                            Icon::Lock
                        } else {
                            Icon::Unlock
                        };
                        widgets::toggle_icon_button(
                            ui,
                            &mut self.read_only[p],
                            Some(lock),
                            "",
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
                        self.hit = None;
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
                    let found = self.hit.as_ref().is_some_and(|h| h.contains(&off));
                    let bg = if self.cursor == Some(off) || found {
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
            let loc = Rect::from_min_max(pos2(right, top), pos2(right + loc_w - 2.0, bottom));
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
        let vbar = Rect::from_min_max(pos2(right + loc_w, top), pos2(avail.right(), bottom));
        widgets::vscrollbar(
            ui,
            self.id.with("vbar"),
            vbar,
            &mut self.scroll,
            total + 1,
            visible,
        );
        ui.allocate_rect(avail, Sense::hover());
        self.goto_dialog(ui, max_len);
    }

    fn find_bar(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let dim = widgets::pal(ui).text_dim;
            ui.add_space(4.0);
            widgets::inline_icon(ui, Icon::Search, dim);
            ui.add_space(4.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.find.query)
                    .hint_text(tr("label.find_bytes"))
                    .margin(vec2(8.0, 4.0))
                    .desired_width(280.0),
            );
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let back = ui.input(|i| i.modifiers.shift);
                self.find_next(!back);
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(Key::Escape))
                && self.goto.is_none()
                && !widgets::modal_open(ui.ctx())
            {
                self.find.open = false;
                self.hit = None;
            }
            let key = widgets::with_key;
            if widgets::icon_button(ui, Icon::Prev, &Cmd::FindPrev.tip()).clicked() {
                self.find_next(false);
            }
            if widgets::icon_button(ui, Icon::Next, &Cmd::FindNext.tip()).clicked() {
                self.find_next(true);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::Close, &key(tr("btn.close"), "Esc")).clicked() {
                    self.find.open = false;
                    self.hit = None;
                }
            });
        });
    }

    fn goto_dialog(&mut self, ui: &mut Ui, max_len: usize) {
        let Some(mut g) = self.goto.take() else {
            return;
        };
        let mut open = !ui.input(|i| i.key_pressed(Key::Escape));
        let mut go = false;
        egui::Window::new(tr("cmd.goto_offset"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("label.offset"));
                    let hint = format!("0 – 0x{:X}", max_len.saturating_sub(1));
                    let r = ui.add(egui::TextEdit::singleline(&mut g).hint_text(hint));
                    r.request_focus();
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        go = true;
                    }
                    if ui.button(tr("btn.ok")).clicked() {
                        go = true;
                    }
                });
            });
        if go {
            match binary::parse_offset(&g) {
                Some(off) => self.go_to(off),
                None => self.status = trf("msg.not_found", &[("text", g)]),
            }
        } else if open {
            self.goto = Some(g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_remain_relative_to_the_latest_save() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a.bin", "b.bin"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        std::fs::write(&paths[0], b"aaa").unwrap();
        std::fs::write(&paths[1], b"bbb").unwrap();
        let mut v = BinaryView::load(paths.clone()).unwrap();
        v.copy_all(true);
        v.save().unwrap();
        v.undo();
        assert!(v.is_modified());
        assert_eq!(v.data[1], b"bbb");
        v.redo();
        assert!(!v.is_modified());
        v.undo();
        v.save().unwrap();
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"bbb");
        v.redo();
        assert!(v.is_modified());
        v.read_only[1] = true;
        assert!(v.save().is_err());
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"bbb");
    }
}
