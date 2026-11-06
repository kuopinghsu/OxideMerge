//! Binary (hex) compare view.

use crate::config::Config;
use crate::core::binary;
use crate::i18n::{tr, trf};
use crate::ui::commands::Cmd;
use crate::ui::theme::Colors;
use crate::ui::widgets::{self, Icon, Mark, Outcome};
use egui::text::LayoutJob;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Key, Modifiers, Rect, Sense, Stroke, StrokeKind,
    TextFormat, Ui,
};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

/// One undo or redo step: the bytes it puts back in place of the `len` bytes at `at` of side
/// `side`, and the contents that leaves, to tell them from those saved.
struct Step {
    side: usize,
    at: usize,
    len: usize,
    bytes: Vec<u8>,
    state: u64,
}

/// The moved blocks being found on another thread, from buffers it shares with the view.
struct MovedScan {
    rx: std::sync::mpsc::Receiver<Option<Vec<binary::MovedBlock>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl MovedScan {
    /// Stops the scan and waits the moment it takes, so the buffers are the view's alone again.
    fn finish(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

impl Drop for BinaryView {
    /// A closed view stops its scan, which lets go of the buffers it shares soon after.
    fn drop(&mut self) {
        if let Some(job) = &self.moved_job {
            job.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

pub struct BinaryView {
    pub paths: Vec<PathBuf>,
    pub data: Vec<Arc<Vec<u8>>>,
    pub modified: Vec<bool>,
    pub read_only: Vec<bool>,
    pub diffs: Vec<Range<usize>>,
    pub moved: Vec<binary::MovedBlock>,
    detect_moved: bool,
    /// The moved-block setting last read from the settings.
    moved_setting: Option<bool>,
    /// Set when the moved-block checkbox was turned on or off, for the settings.
    pub moved_choice: Option<bool>,
    moved_dirty: bool,
    moved_due: Option<f64>,
    moved_job: Option<MovedScan>,
    moved_ranges: Vec<Vec<Range<usize>>>,
    pub cur: Option<usize>,
    pub cursor: Option<usize>,
    scroll: f32,
    scroll_to: Option<usize>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// A number for the contents of each side, new for each change, and the one saved.
    state: Vec<u64>,
    saved: Vec<u64>,
    last_state: u64,
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
    /// The side that typing edits, chosen by clicking one of its bytes.
    pub pane: Option<usize>,
    /// The cursor is in the text column rather than the hexadecimal one.
    pub ascii: bool,
    /// Typing inserts bytes instead of replacing the bytes at the cursor.
    pub insert: bool,
    /// Side and offset of a byte whose first hexadecimal digit was typed.
    nibble: Option<(usize, usize)>,
    /// Side and cursor offset where an edit continues the undo step of the previous one.
    run: Option<(usize, usize)>,
    page_rows: usize,
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
            data: data.into_iter().map(Arc::new).collect(),
            modified: vec![false; n],
            read_only: vec![false; n],
            diffs: Vec::new(),
            moved: Vec::new(),
            detect_moved: true,
            moved_setting: None,
            moved_choice: None,
            moved_dirty: true,
            moved_due: None,
            moved_job: None,
            moved_ranges: vec![Vec::new(); n],
            cur: None,
            cursor: None,
            scroll: 0.0,
            scroll_to: None,
            undo: Vec::new(),
            redo: Vec::new(),
            state: vec![0; n],
            saved: vec![0; n],
            last_state: 0,
            status: String::new(),
            bulk_edit: None,
            id: egui::Id::unique(("binview", std::time::Instant::now())),
            step_file: None,
            find: Default::default(),
            hit: None,
            goto: None,
            pane: None,
            ascii: false,
            insert: false,
            nibble: None,
            run: None,
            page_rows: 1,
            bpl: 16,
        };
        v.recompute();
        v.moved_due = Some(0.0);
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
        self.invalidate_moved();
        let refs: Vec<&[u8]> = self.data.iter().map(|d| d.as_slice()).collect();
        self.diffs = binary::diff_ranges(&refs);
        if let Some(c) = self.cur {
            if c >= self.diffs.len() {
                self.cur = None;
            }
        }
    }

    /// The differences again after bytes `lo..hi` of a side changed and its length did not:
    /// only there and in the differences touching them.
    fn recompute_bytes(&mut self, lo: usize, hi: usize) {
        self.invalidate_moved();
        let first = self.diffs.partition_point(|r| r.end < lo);
        let last = self.diffs.partition_point(|r| r.start <= hi);
        let touching = &self.diffs[first..last];
        let (lo, hi) = match (touching.first(), touching.last()) {
            (Some(a), Some(b)) => (a.start.min(lo), b.end.max(hi)),
            _ => (lo, hi),
        };
        let refs: Vec<&[u8]> = self
            .data
            .iter()
            .map(|d| &d[lo.min(d.len())..hi.min(d.len())])
            .collect();
        let fresh = binary::diff_ranges(&refs)
            .into_iter()
            .map(|r| r.start + lo..r.end + lo);
        self.diffs.splice(first..last, fresh);
        if let Some(c) = self.cur {
            if c >= self.diffs.len() {
                self.cur = None;
            }
        }
    }

    fn invalidate_moved(&mut self) {
        self.stop_moved_scan();
        self.moved.clear();
        for ranges in &mut self.moved_ranges {
            ranges.clear();
        }
        self.moved_dirty = true;
        self.moved_due = None;
    }

    fn start_moved_scan(&mut self, ctx: &egui::Context) {
        self.moved_dirty = false;
        self.moved_due = None;
        if !self.detect_moved || self.diffs.is_empty() {
            return;
        }
        // The worker shares the buffers; an edit stops it first (see `stop_moved_scan`), so no
        // buffer is ever copied for it.
        let data = self.data.clone();
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let refs: Vec<_> = data.iter().map(|d| d.as_slice()).collect();
            let found = binary::moved_blocks_until(&refs, &flag);
            drop(refs);
            drop(data);
            if tx.send(found).is_ok() {
                ctx.request_repaint();
            }
        });
        self.moved_job = Some(MovedScan { rx, stop, thread });
    }

    /// Stops a scan sharing the buffers, before they change.
    fn stop_moved_scan(&mut self) {
        if let Some(job) = self.moved_job.take() {
            job.finish();
        }
    }

    fn poll_moved_scan(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some(job) = &self.moved_job else { return };
        let result = job.rx.try_recv();
        if matches!(result, Err(TryRecvError::Empty)) {
            return;
        }
        self.moved_job = None;
        // Any edit or toggle since launch invalidates the snapshot.
        if self.moved_dirty {
            return;
        }
        if let Ok(Some(moved)) = result {
            self.apply_moved(moved);
        }
    }

    #[cfg(test)]
    pub(super) fn wait_for_moved_scan(&mut self) {
        if let Some(job) = self.moved_job.take() {
            let moved = job
                .rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            if let (false, Some(moved)) = (self.moved_dirty, moved) {
                self.apply_moved(moved);
            }
        }
    }

    #[cfg(test)]
    fn refresh_moved(&mut self) {
        self.moved_dirty = false;
        self.moved_due = None;
        let refs: Vec<_> = self.data.iter().map(|d| d.as_slice()).collect();
        let moved = if self.detect_moved {
            binary::moved_blocks(&refs)
        } else {
            Vec::new()
        };
        self.apply_moved(moved);
    }

    fn apply_moved(&mut self, moved: Vec<binary::MovedBlock>) {
        self.moved = moved;
        for ranges in &mut self.moved_ranges {
            ranges.clear();
        }
        for m in &self.moved {
            self.moved_ranges[m.from].push(m.source.clone());
            self.moved_ranges[m.to].push(m.target.clone());
        }
        for ranges in &mut self.moved_ranges {
            ranges.sort_unstable_by_key(|r| (r.start, r.end));
            let mut merged: Vec<Range<usize>> = Vec::new();
            for r in ranges.drain(..) {
                match merged.last_mut() {
                    Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                    _ => merged.push(r),
                }
            }
            *ranges = merged;
        }
    }

    fn is_moved(&self, pane: usize, offset: usize) -> bool {
        let ranges = &self.moved_ranges[pane];
        let i = ranges.partition_point(|r| r.end <= offset);
        ranges.get(i).is_some_and(|r| r.contains(&offset))
    }

    /// The corresponding byte in another pane, preserving the offset within the block.
    pub fn moved_partner(&self) -> Option<(usize, usize)> {
        let (pane, offset) = (self.pane?, self.cursor?);
        self.moved.iter().find_map(|m| {
            if pane == m.from && m.source.contains(&offset) {
                Some((m.to, m.target.start + offset - m.source.start))
            } else if pane == m.to && m.target.contains(&offset) {
                Some((m.from, m.source.start + offset - m.target.start))
            } else {
                None
            }
        })
    }

    pub fn go_to_moved_partner(&mut self) {
        if let Some((pane, offset)) = self.moved_partner() {
            self.pane = Some(pane);
            self.move_cursor(offset);
        }
    }

    /// Replaces bytes `range` of side `p` with `bytes`, as part of the last undo step.
    fn splice(&mut self, p: usize, range: Range<usize>, bytes: &[u8]) {
        self.stop_moved_scan();
        let data = Arc::make_mut(&mut self.data[p]);
        if let Some(step) = self.undo.last_mut().filter(|s| s.side == p) {
            if step.len == 0 && step.bytes.is_empty() {
                step.at = range.start;
            }
            // what the step covers grows to cover the range too
            let lo = step.at.min(range.start);
            let hi = (step.at + step.len).max(range.end);
            let mut old = Vec::with_capacity(hi - lo - step.len + step.bytes.len());
            old.extend_from_slice(&data[lo..step.at]);
            old.extend_from_slice(&step.bytes);
            old.extend_from_slice(&data[step.at + step.len..hi]);
            (step.at, step.len, step.bytes) = (lo, hi - lo - range.len() + bytes.len(), old);
        }
        let same = range.len() == bytes.len();
        data.splice(range.clone(), bytes.iter().copied());
        self.modified[p] = self.state[p] != self.saved[p];
        if same {
            self.recompute_bytes(range.start, range.end);
        } else {
            self.recompute();
        }
    }

    fn select(&mut self, i: usize) {
        self.cur = Some(i);
        self.cursor = Some(self.diffs[i].start);
        self.scroll_to = Some(self.diffs[i].start / self.bpl);
        self.nibble = None;
        self.run = None;
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
        self.nibble = None;
        self.run = None;
    }

    /// Moves the cursor to `off`, at most just past the last byte of the side being edited (or
    /// of the longest side), where typing adds bytes.
    pub fn move_cursor(&mut self, off: usize) {
        let end = match self.pane {
            Some(p) => self.data[p].len(),
            None => self.data.iter().map(|d| d.len()).max().unwrap_or(0),
        };
        let off = off.min(end);
        self.cursor = Some(off);
        self.cur = self.diffs.iter().position(|r| r.contains(&off));
        self.scroll_to = Some(off / self.bpl);
        self.hit = None;
        self.nibble = None;
        self.run = None;
    }

    /// Typing in the text column, where keys such as "?" are characters rather than commands.
    pub fn takes_text(&self) -> bool {
        self.ascii && self.pane.is_some()
    }

    /// The side and offset an edit changes, after starting a new undo step unless the edit
    /// continues the previous one; `None` when no side is chosen or it is read-only.
    fn edit_at(&mut self) -> Option<(usize, usize)> {
        let p = self.pane?;
        if self.read_only[p] {
            self.status = tr("msg.readonly").into();
            return None;
        }
        let off = self.cursor.unwrap_or(0).min(self.data[p].len());
        if self.run != Some((p, off)) {
            self.push_undo(p);
        }
        Some((p, off))
    }

    /// After an edit of side `p` that leaves the cursor at `off`.
    fn edited(&mut self, p: usize, off: usize) {
        self.cursor = Some(off);
        self.cur = self.diffs.iter().position(|r| r.contains(&off));
        self.scroll_to = Some(off / self.bpl);
        self.hit = None;
        self.run = Some((p, off));
    }

    /// Types `ch` at the cursor: in the hexadecimal column a digit, which sets the first and
    /// then the second half of a byte; in the text column a printable ASCII character.
    pub fn type_char(&mut self, ch: char) {
        if self.ascii {
            if ch.is_ascii_graphic() || ch == ' ' {
                self.write_bytes(&[ch as u8]);
            }
            return;
        }
        let Some(digit) = ch.to_digit(16).map(|d| d as u8) else {
            return;
        };
        let Some((p, off)) = self.edit_at() else {
            return;
        };
        let data = &self.data[p];
        if self.nibble == Some((p, off)) && off < data.len() {
            let byte = data[off] & 0xF0 | digit;
            self.splice(p, off..off + 1, &[byte]);
            self.nibble = None;
            self.edited(p, off + 1);
        } else {
            if self.insert || off == data.len() {
                self.splice(p, off..off, &[digit << 4]);
            } else {
                let byte = data[off] & 0x0F | digit << 4;
                self.splice(p, off..off + 1, &[byte]);
            }
            self.edited(p, off);
            self.nibble = Some((p, off));
        }
    }

    /// Writes `bytes` at the cursor, over the bytes there or, in insert mode, before them, and
    /// moves the cursor past them.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some((p, off)) = self.edit_at() else {
            return;
        };
        let end = if self.insert {
            off
        } else {
            (off + bytes.len()).min(self.data[p].len())
        };
        self.splice(p, off..end, bytes);
        self.nibble = None;
        self.edited(p, off + bytes.len());
    }

    /// Pastes text as its bytes in the text column, or in the hexadecimal column the bytes it
    /// writes in hexadecimal (`DE AD BE EF`). A paste is one undo step.
    pub fn paste(&mut self, text: &str) {
        let bytes = if self.ascii {
            Some(text.as_bytes().to_vec())
        } else {
            binary::parse_hex(text)
        };
        match bytes {
            Some(bytes) => {
                self.run = None;
                self.write_bytes(&bytes);
                self.run = None;
            }
            None if !text.trim().is_empty() => self.status = tr("msg.paste_not_hex").into(),
            None => {}
        }
    }

    /// Deletes the byte at the cursor or, with `back`, the one before it.
    pub fn delete(&mut self, back: bool) {
        let Some(p) = self.pane else { return };
        let at = self.cursor.unwrap_or(0).min(self.data[p].len());
        let off = if back {
            match at.checked_sub(1) {
                Some(o) => o,
                None => return,
            }
        } else {
            at
        };
        if off >= self.data[p].len() || self.edit_at().is_none() {
            return;
        }
        self.splice(p, off..off + 1, &[]);
        self.nibble = None;
        self.edited(p, off);
    }

    /// Starts an undo step for a change of side `side`.
    fn push_undo(&mut self, side: usize) {
        self.undo.push(Step {
            side,
            at: 0,
            len: 0,
            bytes: Vec::new(),
            state: self.state[side],
        });
        self.last_state += 1;
        self.state[side] = self.last_state;
        self.redo.clear();
        self.run = None;
        self.nibble = None;
        if self.undo.len() > 20 {
            self.undo.remove(0);
        }
    }

    /// Puts back the side of `step`, returning the step that brings the current bytes back.
    fn swap_step(&mut self, step: Step) -> Step {
        let (s, at, len) = (step.side, step.at, step.bytes.len());
        let same = len == step.len;
        self.stop_moved_scan();
        let removed = Arc::make_mut(&mut self.data[s])
            .splice(at..at + step.len, step.bytes)
            .collect();
        let back = Step {
            side: s,
            at,
            len,
            bytes: removed,
            state: std::mem::replace(&mut self.state[s], step.state),
        };
        self.modified[s] = self.state[s] != self.saved[s];
        self.run = None;
        self.nibble = None;
        if same {
            self.recompute_bytes(at, at + len);
        } else {
            self.recompute();
        }
        back
    }

    pub fn undo(&mut self) {
        if let Some(step) = self.undo.pop() {
            let back = self.swap_step(step);
            self.redo.push(back);
        }
    }

    pub fn redo(&mut self) {
        if let Some(step) = self.redo.pop() {
            let back = self.swap_step(step);
            self.undo.push(back);
        }
    }

    pub fn copy_range(&mut self, i: usize, from: usize, to: usize) {
        if self.read_only[to] {
            self.status = tr("msg.readonly").into();
            return;
        }
        let r = self.diffs[i].clone();
        self.push_undo(to);
        let src_end = r.end.min(self.data[from].len());
        let src = self.data[from][r.start.min(src_end)..src_end].to_vec();
        let dst_end = r.end.min(self.data[to].len());
        let dst_start = r.start.min(dst_end);
        self.splice(to, dst_start..dst_end, &src);
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
        self.push_undo(to);
        let src = self.data[from].clone();
        self.splice(to, 0..self.data[to].len(), &src);
        self.bulk_edit = Some(trf("msg.copied_blocks", &[("n", n.to_string())]));
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        self.run = None;
        self.nibble = None;
        for i in 0..self.n() {
            if self.modified[i] {
                anyhow::ensure!(!self.read_only[i], "{}", tr("msg.readonly"));
                crate::core::fsutil::write_atomic(&self.paths[i], &self.data[i])?;
                self.saved[i] = self.state[i];
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
        if self.moved_setting != Some(cfg.binary_moved_blocks) {
            self.moved_setting = Some(cfg.binary_moved_blocks);
            if self.detect_moved != cfg.binary_moved_blocks {
                self.detect_moved = cfg.binary_moved_blocks;
                self.invalidate_moved();
            }
        }
        let bpl = self.bpl;
        let active = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Cut
                        | egui::Event::Key { pressed: true, .. }
                )
            })
        });
        self.handle_keys(ui);
        // Edits clear old annotations immediately. Wait briefly before finding new matches,
        // so typing does not scan the complete buffers at every keystroke.
        self.poll_moved_scan();
        if self.moved_dirty {
            let now = ui.input(|i| i.time);
            let due = *self.moved_due.get_or_insert(now + 0.15);
            if active {
                self.moved_due = Some(now + 0.15);
            } else if now >= due && self.moved_job.is_none() {
                self.start_moved_scan(ui.ctx());
            }
            if let Some(due) = self.moved_due.filter(|&due| due > now) {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(
                        (due - now).max(0.0),
                    ));
            }
        }
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            let key = widgets::with_key;
            if widgets::icon_button(ui, Icon::First, &Cmd::FirstDiff.tip()).clicked() {
                self.first_diff();
            }
            if widgets::icon_button_hint(ui, Icon::Prev, &Cmd::PrevDiff.tip(), tr("tip.nav_diff"))
                .clicked()
                && !self.prev_diff()
            {
                self.step_file = Some(false);
            }
            let counter = match self.cur {
                Some(c) => format!("{} / {}", c + 1, self.diffs.len()),
                None => format!("– / {}", self.diffs.len()),
            };
            widgets::chip(ui, &counter, pal.text, pal.control);
            if widgets::icon_button_hint(ui, Icon::Next, &Cmd::NextDiff.tip(), tr("tip.nav_diff"))
                .clicked()
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
            if widgets::toggle_button(
                ui,
                &mut self.insert,
                tr("label.insert_mode"),
                &key(tr("tip.insert_mode"), "Insert"),
            )
            .clicked()
            {
                self.nibble = None;
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
        widgets::toolbar(ui, |ui| {
            if ui
                .checkbox(&mut self.detect_moved, tr("binary.moved_blocks"))
                .on_hover_text(tr("binary.moved_hint"))
                .changed()
            {
                self.moved_choice = Some(self.detect_moved);
                self.invalidate_moved();
                self.moved_due = Some(0.0);
                ui.ctx().request_repaint();
            }
            if self.detect_moved {
                ui.add_space(6.0);
                let pal = widgets::pal(ui);
                widgets::chip(ui, &self.moved.len().to_string(), pal.text, pal.control)
                    .on_hover_text(trf(
                        "binary.moved_count",
                        &[("n", self.moved.len().to_string())],
                    ));
                widgets::toolbar_sep(ui);
                if ui
                    .add_enabled(
                        self.moved_partner().is_some(),
                        egui::Button::new(tr("binary.moved_partner")),
                    )
                    .clicked()
                {
                    self.go_to_moved_partner();
                    ui.memory_mut(|m| m.request_focus(self.id.with("body")));
                }
                if let Some((p, at)) = self.moved_partner() {
                    ui.label(trf(
                        "binary.moved_at",
                        &[
                            ("side", crate::core::report::side_name(p, self.n()).into()),
                            ("offset", format!("{at:08X}")),
                        ],
                    ));
                }
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
        let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let visible = (bottom - top) / row_h;
        self.page_rows = (visible.floor() as usize).saturating_sub(1).max(1);
        let max_len = self.data.iter().map(|d| d.len()).max().unwrap_or(0);
        // The cursor can stand just past the last byte, where typing adds one.
        let end = max_len.max(self.cursor.map_or(0, |c| c + 1));
        let total = end.div_ceil(bpl).max(1);

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
                                egui::Label::new(egui::RichText::new(title).font(
                                    crate::ui::theme::semibold(crate::ui::theme::size::CONTROL),
                                ))
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
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Document, true, tr("help.binary_compare"))
        });
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
        // Character columns of a line: the offset, then each byte as "HH ", a space and the text.
        let hex_col = 10.0;
        let text_col = hex_col + 3.0 * bpl as f32 + 1.0;

        if resp.clicked() {
            resp.request_focus();
            if let Some(pos) = resp.interact_pointer_pos() {
                let line = (self.scroll + (pos.y - top) / row_h).floor() as usize;
                let p = (((pos.x - left) / pane_w).floor() as usize).min(n - 1);
                let col = (pos.x - (left + p as f32 * pane_w) - 4.0) / char_w;
                let hit = if (hex_col..text_col - 1.0).contains(&col) {
                    Some((((col - hex_col) / 3.0) as usize, false))
                } else if (text_col..text_col + bpl as f32).contains(&col) {
                    Some(((col - text_col) as usize, true))
                } else {
                    None
                };
                if let Some((k, ascii)) = hit {
                    self.pane = Some(p);
                    self.ascii = ascii;
                    self.move_cursor(line * bpl + k);
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
                        (Some(_), _, _) if self.is_moved(p, off) => c.moved,
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
                    let t = &ascii.text[s.byte_range.start.0..s.byte_range.end.0];
                    job.append(t, 0.0, s.format.clone());
                }
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                painter.galley(pos2(x0 + 4.0, y + 1.0), galley, c.text);
            }
            if let (Some(off), true) = (self.cursor, self.pane == Some(p)) {
                let line = off / bpl;
                if (first..last).contains(&line) {
                    let k = (off % bpl) as f32;
                    let (col, width) = if self.ascii {
                        (text_col + k, 1.0)
                    } else if self.nibble == Some((p, off)) {
                        (hex_col + 3.0 * k + 1.0, 1.0)
                    } else {
                        (hex_col + 3.0 * k, 2.0)
                    };
                    let y = top + (line as f32 - self.scroll) * row_h;
                    let r = Rect::from_min_size(
                        pos2(x0 + 4.0 + col * char_w, y + 1.0),
                        vec2(width * char_w, row_h - 2.0),
                    );
                    painter.rect_stroke(
                        r.expand(1.0),
                        2.0,
                        Stroke::new(1.5_f32, c.ui.accent),
                        StrokeKind::Inside,
                    );
                }
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
            let mut marks: Vec<Mark> = self
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
            for (p, ranges) in self.moved_ranges.iter().enumerate() {
                for r in ranges {
                    let mut columns = [None; 3];
                    columns[p] = Some(c.moved);
                    marks.push(Mark {
                        start: r.start / bpl,
                        len: (r.end - 1) / bpl - r.start / bpl + 1,
                        columns,
                    });
                }
            }
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

    /// Cursor keys, and typing, deleting and pasting bytes on the side being edited.
    fn handle_keys(&mut self, ui: &Ui) {
        let ctx = ui.ctx();
        if widgets::typing(ctx)
            // Toolbar controls own their keyboard input: Space on the moved-block checkbox
            // must never also overwrite a byte in the ASCII column.
            || ctx.memory(|m| m.focused()).is_some_and(|id| id != self.id.with("body"))
            || self.goto.is_some()
            || widgets::modal_open(ctx)
            || egui::Popup::is_any_open(ctx)
        {
            return;
        }
        let bpl = self.bpl;
        let page = bpl * self.page_rows;
        let end = match self.pane {
            Some(p) => self.data[p].len(),
            None => self.data.iter().map(|d| d.len()).max().unwrap_or(0),
        };
        let at = self.cursor.unwrap_or(0).min(end);
        let line_start = at - at % bpl;
        let mut target = None;
        let (mut insert, mut delete) = (false, None);
        let mut typed = String::new();
        let mut pasted = None;
        ui.input_mut(|i| {
            for (modifiers, key, to) in [
                (Modifiers::COMMAND, Key::Home, 0),
                (Modifiers::COMMAND, Key::End, end),
                (Modifiers::NONE, Key::ArrowLeft, at.saturating_sub(1)),
                (Modifiers::NONE, Key::ArrowRight, at + 1),
                (
                    Modifiers::NONE,
                    Key::ArrowUp,
                    at.checked_sub(bpl).unwrap_or(at),
                ),
                (
                    Modifiers::NONE,
                    Key::ArrowDown,
                    if at + bpl <= end { at + bpl } else { at },
                ),
                (Modifiers::NONE, Key::PageUp, at.saturating_sub(page)),
                (Modifiers::NONE, Key::PageDown, at + page),
                (Modifiers::NONE, Key::Home, line_start),
                (Modifiers::NONE, Key::End, line_start + bpl - 1),
            ] {
                if i.consume_key(modifiers, key) {
                    target = Some(to);
                }
            }
            insert = i.consume_key(Modifiers::NONE, Key::Insert);
            if i.consume_key(Modifiers::NONE, Key::Delete) {
                delete = Some(false);
            }
            if i.consume_key(Modifiers::NONE, Key::Backspace) {
                delete = Some(true);
            }
            for e in &i.events {
                match e {
                    egui::Event::Text(t) => typed.push_str(t),
                    egui::Event::Paste(t) => pasted = Some(t.clone()),
                    _ => {}
                }
            }
        });
        if let Some(to) = target {
            self.move_cursor(to);
        }
        if insert {
            self.insert = !self.insert;
            self.nibble = None;
        }
        if let Some(back) = delete {
            self.delete(back);
        }
        if self.pane.is_some() {
            for ch in typed.chars() {
                self.type_char(ch);
            }
            if let Some(text) = pasted {
                self.paste(&text);
            }
        }
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
                // within the side being edited, or just past its end where typing adds bytes
                Some(off) if self.pane.is_some() => self.move_cursor(off),
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

    fn moved_view() -> (tempfile::TempDir, BinaryView) {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c): (Vec<u8>, Vec<u8>, Vec<u8>) =
            ((0..64).collect(), (64..128).collect(), (128..192).collect());
        let paths = vec![dir.path().join("a.bin"), dir.path().join("b.bin")];
        std::fs::write(&paths[0], [a.clone(), b.clone(), c.clone()].concat()).unwrap();
        std::fs::write(&paths[1], [a, c, b].concat()).unwrap();
        let mut view = BinaryView::load(paths).unwrap();
        view.refresh_moved();
        (dir, view)
    }

    #[test]
    fn moved_blocks_navigate_without_editing_and_refresh_after_edits_and_undo() {
        let (_dir, mut v) = moved_view();
        let original = v.data.clone();
        let differences = v.diffs.clone();
        assert_eq!(v.moved.len(), 2);
        v.pane = Some(0);
        v.move_cursor(69);
        assert!(v.is_moved(0, 69));
        assert_eq!(v.moved_partner(), Some((1, 133)));
        v.read_only[1] = true;
        v.go_to_moved_partner();
        assert_eq!((v.pane, v.cursor), (Some(1), Some(133)));
        v.go_to_moved_partner();
        assert_eq!((v.pane, v.cursor), (Some(0), Some(69)));
        assert_eq!(v.data, original);
        assert_eq!(v.diffs, differences);
        assert!(v.undo.is_empty() && !v.is_modified());
        v.move_cursor(64);
        v.write_bytes(&[250; 64]);
        assert!(
            v.moved.is_empty(),
            "an old correspondence is never shown after an edit"
        );
        v.refresh_moved();
        assert!(!v.is_moved(0, 69));
        v.move_cursor(69);
        assert_eq!(v.moved_partner(), None);
        v.undo();
        v.refresh_moved();
        assert_eq!(v.data, original);
        assert_eq!(v.moved.len(), 2);
        v.redo();
        v.refresh_moved();
        assert!(!v.is_moved(0, 69));
        v.undo();
        v.refresh_moved();
        v.detect_moved = false;
        v.invalidate_moved();
        v.refresh_moved();
        assert!(v.moved.is_empty());
        assert!(!v.is_moved(0, 69));
        assert_eq!(v.diffs, differences);
        v.detect_moved = true;
        v.invalidate_moved();
        v.refresh_moved();
        v.pane = Some(1);
        v.move_cursor(133);
        let moves = v.moved.clone();
        v.write_bytes(&[0]);
        assert_eq!(v.data, original, "read-only side refuses writes");
        assert_eq!(v.moved, moves);
        v.read_only[1] = false;
        v.copy_all(true);
        v.refresh_moved();
        assert!(v.diffs.is_empty() && v.moved.is_empty());
        v.undo();
        v.refresh_moved();
        assert_eq!(v.data, original);
        assert_eq!(v.moved.len(), 2);
    }

    /// The runs of `samples/*/bin/firmware.bin` the two sides share: the right side's range
    /// and how far the left side's copy is from it.
    const FIRMWARE_RUNS: [(Range<usize>, isize); 18] = [
        (0x20..0x3f, 1),
        (0x41..0x100, -1),
        (0x105..0x200, -1),
        (0x200..0x380, 4),
        (0x383..0x3b5, 1),
        (0x3b7..0x3e8, 2),
        (0x3e9..0x4fd, 4),
        (0x4fd..0x5fd, 8),
        (0x604..0x66f, 1),
        (0x66f..0x692, 3),
        (0x697..0x6b5, 2),
        (0x6b5..0x800, 4),
        (0x805..0x822, 5),
        (0x823..0x838, 5),
        (0x838..0xef0, 7),
        (0xef0..0xf48, 17),
        (0xf49..0xf76, 17),
        (0xf78..0xff0, 21),
    ];

    fn firmware_paths() -> [std::path::PathBuf; 2] {
        ["left", "right"].map(|side| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("samples/{side}/bin/firmware.bin"))
        })
    }

    /// A view of `left` and `right` written to a temporary folder.
    fn view_of(left: &[u8], right: &[u8]) -> (tempfile::TempDir, BinaryView) {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![dir.path().join("left.bin"), dir.path().join("right.bin")];
        std::fs::write(&paths[0], left).unwrap();
        std::fs::write(&paths[1], right).unwrap();
        (dir, BinaryView::load(paths).unwrap())
    }

    /// Checks that each left byte of the runs, `extra` bytes further on, has its right byte as
    /// its moved partner; runs that line up again are only compared.
    fn assert_runs(v: &mut BinaryView, runs: &[(Range<usize>, isize)], extra: isize) {
        v.pane = Some(0);
        for (right_range, shift) in runs {
            for right in right_range.clone() {
                let left = right.checked_add_signed(shift + extra).unwrap();
                assert_eq!(v.data[0][left], v.data[1][right]);
                if shift + extra != 0 {
                    v.move_cursor(left);
                    assert_eq!(v.moved_partner(), Some((1, right)), "left {left:#x}");
                }
            }
        }
    }

    #[test]
    fn firmware_sample_leading_insertion_recovers_header_and_repeated_runs() {
        let mut v = BinaryView::load(firmware_paths().into()).unwrap();
        v.refresh_moved();
        let baseline_moves = v.moved.clone();
        v.pane = Some(0);
        v.move_cursor(0);
        v.insert = true;
        v.write_bytes(&[0xa5]);
        v.refresh_moved();
        assert_eq!(v.data[0][0], 0xa5);
        assert_ne!(v.data[0][0], v.data[1][0]);
        assert!(!v.is_moved(0, 0));
        assert_eq!(&v.data[0][1..0x21], &v.data[1][..0x20]);
        for offset in 1..0x21 {
            assert!(v.is_moved(0, offset));
            assert!(v.is_moved(1, offset - 1));
            v.move_cursor(offset);
            assert_eq!(v.moved_partner(), Some((1, offset - 1)));
        }
        assert!(!v.is_moved(0, 0x21), "the byte only the left side has");
        assert_runs(&mut v, &FIRMWARE_RUNS, 1);
        v.undo();
        v.refresh_moved();
        assert_eq!(v.moved, baseline_moves);
    }

    #[test]
    fn firmware_sample_balanced_edits_detect_local_shift() {
        let base = include_bytes!("../../samples/right/bin/firmware.bin");
        for deleted in [1, 2] {
            let (_dir, mut v) = view_of(base, base);
            v.pane = Some(0);
            v.insert = true;
            v.move_cursor(0x20);
            v.write_bytes(&[0xa5]);
            v.move_cursor(0x40);
            for _ in 0..deleted {
                v.delete(false);
            }
            assert_eq!(v.data[0].len() + deleted - 1, v.data[1].len());
            v.refresh_moved();
            for left in 0x21..0x40 {
                assert_eq!(v.data[0][left], v.data[1][left - 1]);
                assert!(v.is_moved(0, left));
                v.move_cursor(left);
                assert_eq!(v.moved_partner(), Some((1, left - 1)));
            }
            assert!(!v.is_moved(0, 0x20));
            for removed in 0x3f..0x3f + deleted {
                assert!(!v.is_moved(1, removed));
            }
            v.move_cursor(0x40);
            assert_eq!(v.moved_partner(), (deleted == 2).then_some((1, 0x41)));
        }
    }

    #[test]
    fn firmware_opposite_pane_edits_keep_short_match() {
        let shift = 2;
        let mut v = BinaryView::load(firmware_paths().into()).unwrap();
        let removed = v.data[1][0x2f];
        v.pane = Some(0);
        v.insert = true;
        v.move_cursor(0x20);
        v.write_bytes(&[0xa6]);
        v.pane = Some(1);
        v.move_cursor(0x2f);
        v.delete(false);
        v.refresh_moved();
        for right in 0x20..0x2f {
            let left = right + shift;
            assert_eq!(v.data[0][left], v.data[1][right]);
            v.move_cursor(right);
            assert_eq!(v.moved_partner(), Some((0, left)), "{right:#x}");
        }
        assert!(!v.is_moved(0, 0x20));
        assert_eq!(v.data[0][0x2f + shift], removed);
        assert!(!v.is_moved(0, 0x2f + shift));
    }

    #[test]
    fn firmware_sample_deletion_keeps_later_runs_aligned() {
        for prepend in [false, true] {
            let mut v = BinaryView::load(firmware_paths().into()).unwrap();
            v.pane = Some(0);
            if prepend {
                v.insert = true;
                v.move_cursor(0);
                v.write_bytes(&[0xa5]);
            }
            v.move_cursor(0x120);
            v.delete(false);
            v.delete(false);
            let removed = 2 - isize::from(prepend);
            assert_eq!(v.data[0].len() as isize, 4101 - removed);
            v.refresh_moved();
            assert_runs(&mut v, &FIRMWARE_RUNS[3..], -removed);
            // The ten bytes only the left side has stay unmatched.
            let only_left = 0xef7 - removed as usize..0xf01 - removed as usize;
            assert!(only_left.into_iter().all(|at| !v.is_moved(0, at)));
        }
    }

    #[test]
    fn firmware_sample_insertion_detects_shifted_bytes() {
        let left = include_bytes!("../../samples/left/bin/firmware.bin");
        let right = include_bytes!("../../samples/right/bin/firmware.bin");
        // Inside the header both sides have at the same offsets.
        let at = 0x10;
        for baseline in [&left[..], &right[..]] {
            let (_dir, mut v) = view_of(baseline, right);
            v.refresh_moved();
            let baseline_moves = v.moved.clone();
            v.pane = Some(1);
            v.move_cursor(at);
            v.insert = true;
            v.write_bytes(&[0xa5]);
            assert_eq!(v.data[1].len(), right.len() + 1);
            assert_eq!(v.data[1][at], 0xa5);
            assert_eq!(&v.data[1][at + 1..], &right[at..]);
            v.refresh_moved();
            let m = v.moved.iter().find(|m| m.source.contains(&at)).unwrap();
            assert_eq!(m.target.start, m.source.start + 1);
            assert_eq!(v.data[0][m.source.clone()], v.data[1][m.target.clone()]);
            v.move_cursor(at + 1);
            assert_eq!(v.moved_partner(), Some((0, at)));
            v.go_to_moved_partner();
            assert_eq!((v.pane, v.cursor), (Some(0), Some(at)));
            v.undo();
            v.refresh_moved();
            assert_eq!(v.data[1].as_slice(), right);
            assert_eq!(v.moved, baseline_moves);
            assert_eq!(std::fs::read(&v.paths[1]).unwrap(), right);
        }
    }

    #[test]
    fn complex_firmware_fixture_tracks_each_shift_and_leaves_edits_unmatched() {
        let mut v = BinaryView::load(firmware_paths().into()).unwrap();
        v.refresh_moved();
        let found: Vec<_> = v
            .moved
            .iter()
            .map(|m| (m.source.clone(), m.target.clone()))
            .collect();
        let expected: Vec<_> = FIRMWARE_RUNS
            .iter()
            .map(|(right, shift)| {
                let left = right.start.checked_add_signed(*shift).unwrap()
                    ..right.end.checked_add_signed(*shift).unwrap();
                (left, right.clone())
            })
            .collect();
        assert_eq!(found, expected);
        assert_runs(&mut v, &FIRMWARE_RUNS, 0);
        for unmatched in [
            0x20..0x21,
            0xff..0x104,
            0x1ff..0x204,
            0x501..0x505,
            0xef7..0xf01,
            0xf87..0xf8d,
        ] {
            assert!(unmatched.into_iter().all(|at| !v.is_moved(0, at)));
        }
    }

    #[test]
    fn an_edit_stops_the_moved_scan_and_changes_the_bytes_in_place() {
        let (_dir, mut v) = moved_view();
        let ctx = egui::Context::default();
        let before = Arc::as_ptr(&v.data[0]);
        v.invalidate_moved();
        v.start_moved_scan(&ctx);
        assert!(v.moved_job.is_some());
        v.pane = Some(0);
        v.move_cursor(64);
        v.write_bytes(&[250]);
        assert_eq!(v.data[0][64], 250);
        assert!(v.moved_job.is_none(), "the scan is stopped");
        assert_eq!(Arc::as_ptr(&v.data[0]), before, "the side was not copied");
        assert_eq!(Arc::strong_count(&v.data[0]), 1);
        assert!(v.moved_dirty && v.moved.is_empty());
    }

    #[test]
    fn a_stopped_scan_gives_nothing_and_a_closed_view_stops_its_scan() {
        let stop = std::sync::atomic::AtomicBool::new(true);
        let a: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut b = a.clone();
        b.rotate_left(4096);
        assert!(binary::moved_blocks_until(&[&a, &b], &stop).is_none());

        let (_dir, mut v) = moved_view();
        v.invalidate_moved();
        v.start_moved_scan(&egui::Context::default());
        let flag = v.moved_job.as_ref().unwrap().stop.clone();
        drop(v);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn moved_blocks_wait_for_idle_after_binary_typing() {
        let (_dir, mut v) = moved_view();
        let ctx = egui::Context::default();
        let cfg = Config::default();
        let colors = crate::ui::theme::colors(false);
        let frame = |v: &mut BinaryView, time, events| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 820.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| v.ui(ui, &cfg, &colors));
                },
            );
            out.textures_delta.clear();
        };
        v.pane = Some(0);
        v.move_cursor(64);
        v.write_bytes(&[250]);
        frame(&mut v, 0.0, vec![]);
        assert!(v.moved_dirty && v.moved.is_empty());
        frame(
            &mut v,
            0.1,
            vec![egui::Event::Key {
                key: Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        frame(&mut v, 0.2, vec![]);
        assert!(v.moved_dirty && v.moved.is_empty());
        frame(&mut v, 0.3, vec![]);
        v.wait_for_moved_scan();
        assert!(!v.moved_dirty && !v.moved.is_empty());
        for m in &v.moved {
            assert_eq!(
                v.data[m.from][m.source.clone()],
                v.data[m.to][m.target.clone()]
            );
        }
    }

    #[test]
    fn the_moved_block_setting_is_followed_when_it_changes() {
        let (_dir, mut v) = moved_view();
        let ctx = egui::Context::default();
        let mut cfg = Config {
            binary_moved_blocks: false,
            ..Config::default()
        };
        let colors = crate::ui::theme::colors(false);
        let frame = |v: &mut BinaryView, cfg: &Config, time| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| v.ui(ui, cfg, &colors));
                },
            );
            out.textures_delta.clear();
        };
        frame(&mut v, &cfg, 0.0);
        frame(&mut v, &cfg, 1.0);
        assert!(v.moved_job.is_none() && v.moved.is_empty());
        assert_eq!(v.moved_choice, None);
        cfg.binary_moved_blocks = true;
        frame(&mut v, &cfg, 2.0);
        frame(&mut v, &cfg, 3.0);
        v.wait_for_moved_scan();
        assert!(!v.moved.is_empty());
    }

    #[test]
    fn edits_keep_the_differences_of_a_full_compare_and_undo_to_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a.bin", "b.bin"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        let base: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&paths[0], &base).unwrap();
        std::fs::write(&paths[1], &base[..4000]).unwrap();
        let mut v = BinaryView::load(paths).unwrap();
        v.pane = Some(1);
        let mut seed = 12345u64;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        let full = |v: &BinaryView| {
            let refs: Vec<&[u8]> = v.data.iter().map(|d| d.as_slice()).collect();
            binary::diff_ranges(&refs)
        };
        let mut history = Vec::new();
        for round in 0..400 {
            let (before, state) = (v.data[1].clone(), v.last_state);
            v.move_cursor(next(v.data[1].len() + 1));
            v.insert = next(4) == 0;
            v.ascii = false;
            let digit = |n: usize| char::from_digit(n as u32, 16).unwrap();
            match next(4) {
                0 => v.type_char(digit(next(16))),
                // a byte typed digit by digit, and bytes written on, are one step each
                1 => {
                    v.type_char(digit(next(16)));
                    v.type_char(digit(next(16)));
                }
                2 => {
                    v.write_bytes(&[next(256) as u8; 3]);
                    v.write_bytes(&[next(256) as u8; 2]);
                }
                _ => v.delete(next(2) == 0),
            }
            assert_eq!(v.diffs, full(&v), "round {round}");
            if v.last_state != state {
                history.push(before);
            }
        }
        let last = v.data[1].clone();
        let steps = v.undo.len();
        for k in 0..steps {
            v.undo();
            assert_eq!(v.data[1], history[history.len() - 1 - k], "undo {k}");
            assert_eq!(v.diffs, full(&v), "undo {k}");
        }
        for _ in 0..steps {
            v.redo();
        }
        assert_eq!(v.data[1], last);
        assert_eq!(v.diffs, full(&v));
    }

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
        assert_eq!(v.data[1].as_slice(), b"bbb");
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

    #[test]
    fn typing_digits_and_text_edits_the_chosen_side() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a.bin", "b.bin"]
            .iter()
            .map(|n| dir.path().join(n))
            .collect();
        std::fs::write(&paths[0], [0x00, 0x11, 0x22]).unwrap();
        std::fs::write(&paths[1], [0x00, 0x11, 0x22]).unwrap();
        let mut v = BinaryView::load(paths.clone()).unwrap();
        v.type_char('a');
        assert!(!v.is_modified(), "no side is chosen before a click");

        v.pane = Some(1);
        v.move_cursor(1);
        for ch in "abc".chars() {
            v.type_char(ch);
        }
        assert_eq!(v.data[1].as_slice(), [0x00, 0xAB, 0xC2]);
        assert_eq!(v.cursor, Some(2), "half a byte typed keeps the cursor");
        for ch in "dxef".chars() {
            v.type_char(ch);
        }
        assert_eq!(
            v.data[1].as_slice(),
            [0x00, 0xAB, 0xCD, 0xEF],
            "typed past the end adds bytes"
        );
        assert_eq!(v.diffs, [Range { start: 1, end: 4 }]);
        v.undo();
        assert_eq!(
            v.data[1].as_slice(),
            [0x00, 0x11, 0x22],
            "one typing run is one undo step"
        );
        assert!(!v.is_modified());
        v.redo();

        v.insert = true;
        v.ascii = true;
        v.move_cursor(0);
        for ch in "OKé".chars() {
            v.type_char(ch);
        }
        assert_eq!(v.data[1].as_slice(), [b'O', b'K', 0x00, 0xAB, 0xCD, 0xEF]);
        v.delete(true);
        v.delete(false);
        assert_eq!(v.data[1].as_slice(), [b'O', 0xAB, 0xCD, 0xEF]);

        v.insert = false;
        v.ascii = false;
        v.move_cursor(1);
        v.paste("DE AD BE EF");
        assert_eq!(v.data[1].as_slice(), [b'O', 0xDE, 0xAD, 0xBE, 0xEF]);
        v.undo();
        assert_eq!(
            v.data[1].as_slice(),
            [b'O', 0xAB, 0xCD, 0xEF],
            "a paste is its own undo step"
        );

        v.paste("123");
        v.paste("hello");
        assert_eq!(v.data[1].as_slice(), [b'O', 0xAB, 0xCD, 0xEF]);
        assert_eq!(v.status, tr("msg.paste_not_hex"));

        v.move_cursor(1);
        v.type_char('1');
        v.type_char('2');
        v.move_cursor(3);
        v.move_cursor(2);
        v.type_char('3');
        v.type_char('4');
        assert_eq!(v.data[1].as_slice(), [b'O', 0x12, 0x34, 0xEF]);
        v.undo();
        assert_eq!(
            v.data[1].as_slice(),
            [b'O', 0x12, 0xCD, 0xEF],
            "typing after moving the cursor is a new undo step"
        );

        v.move_cursor(1);
        v.type_char('9');
        v.copy_all(false);
        assert_eq!(v.data[0], v.data[1]);
        v.type_char('8');
        assert_eq!(v.data[1][1], 0x82, "a copy ends a half-typed byte");
        v.undo();
        v.undo();
        assert_eq!(
            v.data[1].as_slice(),
            [b'O', 0x92, 0xCD, 0xEF],
            "undoing a change of one side leaves the other"
        );
        assert_eq!(v.data[0].as_slice(), [0x00, 0x11, 0x22]);
        assert!(!v.modified[0]);
        v.undo();
        assert_eq!(v.data[1].as_slice(), [b'O', 0x12, 0xCD, 0xEF]);

        v.read_only[0] = true;
        v.pane = Some(0);
        v.move_cursor(0);
        v.type_char('f');
        assert_eq!(v.data[0].as_slice(), [0x00, 0x11, 0x22]);
        assert_eq!(v.status, tr("msg.readonly"));
        v.save().unwrap();
        assert_eq!(std::fs::read(&paths[1]).unwrap(), [b'O', 0x12, 0xCD, 0xEF]);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), [0x00, 0x11, 0x22]);
    }
}
