use crate::config::Config;
use crate::core::filter::{FileFilter, FltFilter};
use crate::core::folder::{CompareMethod, FolderOptions};
use crate::core::remote_compare::{self, Opened, Row, Side, Status};
use crate::i18n::{tr, trf};
use crate::ui::theme;
use crate::ui::widgets::{self, pal, Icon};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

enum Update {
    Listed(Vec<Row>),
    Verified(
        String,
        anyhow::Result<bool>,
        Vec<crate::core::remote::Fetched>,
    ),
    Finished(anyhow::Result<Option<Box<[Opened; 2]>>>),
}

struct Job {
    updates: mpsc::Receiver<Update>,
    stop: Arc<AtomicBool>,
    verifying: bool,
    /// What its fetches read from a server so far, and when it started.
    progress: Arc<crate::core::remote::Progress>,
    started: std::time::Instant,
}

impl Job {
    fn start(
        work: impl FnOnce(&AtomicBool, &mpsc::Sender<Update>) -> anyhow::Result<Option<[Opened; 2]>>
            + Send
            + 'static,
    ) -> Self {
        let (send, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let progress = Arc::<crate::core::remote::Progress>::default();
        let counted = progress.clone();
        let run = move || {
            let result = crate::core::remote::with_progress(&counted, || work(&cancel, &send));
            let _ = send.send(Update::Finished(result.map(|opened| opened.map(Box::new))));
        };
        #[cfg(test)]
        if crate::core::remote::fake::installed() {
            run();
            return Self {
                updates,
                stop,
                verifying: false,
                progress,
                started: std::time::Instant::now(),
            };
        }
        std::thread::spawn(run);
        Self {
            updates,
            stop,
            verifying: false,
            progress,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct OpenFiles {
    pub files: [Opened; 2],
    pub path: String,
    pub read_only: [bool; 2],
    pub rules: String,
}

pub struct RemoteFolder {
    sides: [Side; 2],
    labels: [String; 2],
    rows: Vec<Row>,
    selected: Option<usize>,
    verified: HashMap<String, Result<Status, String>>,
    verified_options: Option<crate::core::diff::DiffOptions>,
    /// The files a verification still has to read, and how many it started with: its progress.
    pending: HashSet<String>,
    verify_total: usize,
    /// How many files are in each state, for the summary over the list; `None` once the rows
    /// or what was verified changed.
    counts: Option<Vec<(Shown, usize)>>,
    /// The copies verifying fetched from the servers, by path, kept until the list is
    /// refreshed: opening a verified pair uses them instead of fetching it again. `kept_bytes`
    /// is their size together, which [`MAX_KEPT_BYTES`] bounds.
    kept: HashMap<String, Vec<crate::core::remote::Fetched>>,
    kept_bytes: u64,
    /// What the last verification found, said over the list once it is done.
    notice: Option<(String, bool)>,
    /// A row the list still has to scroll to: the first that differs, after a verification.
    scroll_to: Option<usize>,
    opening_path: Option<String>,
    /// How many bytes opening `opening_path` fetches from servers, if its sizes are known.
    opening_bytes: u64,
    job: Option<Job>,
    error: Option<String>,
    filter: String,
    options: FolderOptions,
    method: CompareMethod,
    read_only: [bool; 2],
    rules: String,
    resolved_rules: Option<crate::core::rules::RuleSet>,
}

impl RemoteFolder {
    pub(crate) fn new(
        sides: [Side; 2],
        filter: String,
        options: FolderOptions,
        read_only: [bool; 2],
        rules: String,
        named: &[FltFilter],
    ) -> Self {
        let labels = sides.each_ref().map(Side::label);
        let method = if options.method == CompareMethod::FullContents {
            CompareMethod::FullContents
        } else {
            CompareMethod::BinaryContents
        };
        let mut view = Self {
            sides,
            labels,
            rows: Vec::new(),
            selected: None,
            verified: HashMap::new(),
            verified_options: None,
            pending: HashSet::new(),
            verify_total: 0,
            counts: None,
            kept: HashMap::new(),
            kept_bytes: 0,
            notice: None,
            scroll_to: None,
            opening_path: None,
            opening_bytes: 0,
            job: None,
            error: None,
            filter,
            options,
            method,
            read_only,
            resolved_rules: None,
            rules,
        };
        view.refresh(named);
        view
    }

    pub fn title(&self) -> String {
        self.labels.join(" ↔ ")
    }

    pub fn status_line(&self) -> String {
        trf(
            "remote_compare.entries",
            &[("count", self.rows.len().to_string())],
        )
    }

    pub fn refresh(&mut self, named: &[FltFilter]) {
        self.resolved_rules = crate::core::rules::find(&self.rules);
        self.rows.clear();
        self.selected = None;
        self.verified.clear();
        self.verified_options = None;
        self.counts = None;
        // A refresh asks the servers afresh: nothing fetched before is used after it.
        self.kept.clear();
        self.kept_bytes = 0;
        let sides = self.sides.clone();
        let options = self.options.clone();
        let filter = FileFilter::resolve(&self.filter, named);
        self.start(move |stop, tx| {
            remote_compare::scan(&sides, &options, &filter, stop, |rows| {
                let _ = tx.send(Update::Listed(rows));
            })?;
            Ok(None)
        });
    }

    pub fn invalidate(&mut self, path: &str, named: &[FltFilter]) {
        if self.job.is_some() {
            // A scan or verification may have read the file before the save.
            self.refresh(named);
        } else {
            self.verified.remove(path);
            self.forget_kept(path);
            self.counts = None;
            if let Some(row) = self
                .rows
                .iter_mut()
                .find(|row| !row.directory && row.path == path)
            {
                row.status = Status::Unverified;
                row.sizes = [None; 2];
            }
        }
    }

    fn start(
        &mut self,
        work: impl FnOnce(&AtomicBool, &mpsc::Sender<Update>) -> anyhow::Result<Option<[Opened; 2]>>
            + Send
            + 'static,
    ) {
        self.error = None;
        self.notice = None;
        self.opening_path = None;
        self.pending.clear();
        self.verify_total = 0;
        self.job = Some(Job::start(work));
    }

    fn poll(&mut self) -> Option<[Opened; 2]> {
        let job = self.job.as_ref()?;
        loop {
            match job.updates.try_recv() {
                Ok(Update::Listed(rows)) => {
                    self.rows.extend(rows);
                    self.counts = None;
                }
                Ok(Update::Verified(path, result, kept)) => {
                    self.pending.remove(&path);
                    self.counts = None;
                    // Kept for opening the pair later, while there is room.
                    if let Some(old) = self.kept.remove(&path) {
                        self.kept_bytes = self.kept_bytes.saturating_sub(kept_size(&old));
                    }
                    let size = kept_size(&kept);
                    if !kept.is_empty() && self.kept_bytes + size <= MAX_KEPT_BYTES {
                        self.kept_bytes += size;
                        self.kept.insert(path.clone(), kept);
                    }
                    self.verified.insert(
                        path,
                        result
                            .map(|equal| {
                                if equal {
                                    Status::ContentMatch
                                } else {
                                    Status::ContentDifferent
                                }
                            })
                            .map_err(|error| format!("{error:#}")),
                    );
                }
                Ok(Update::Finished(result)) => {
                    let cancelled = job.stop.load(Ordering::Relaxed);
                    let verified = job.verifying;
                    self.job = None;
                    self.pending.clear();
                    if cancelled {
                        self.error = Some(tr("msg.sftp_cancelled").to_string());
                        return None;
                    }
                    // Files that could not be verified say why over the list, not only in a
                    // tooltip on each: the first of them, and how many more there are.
                    let mut failed: Vec<_> = self
                        .verified
                        .iter()
                        .filter_map(|(path, result)| Some((path, result.as_ref().err()?)))
                        .collect();
                    failed.sort();
                    if let Some((path, error)) = failed.first() {
                        let more = match failed.len() - 1 {
                            0 => String::new(),
                            n => format!("  (+{n})"),
                        };
                        self.error = Some(format!("{path}: {error}{more}"));
                    }
                    if verified && result.is_ok() {
                        self.verification_done();
                    }
                    return match result {
                        Ok(opened) => opened.map(|files| *files),
                        Err(error) => {
                            self.error = Some(format!("{error:#}"));
                            None
                        }
                    };
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.pending.clear();
                    self.error = Some(tr("status.error").to_string());
                    return None;
                }
                Err(mpsc::TryRecvError::Empty) => return None,
            }
        }
    }

    fn verify(&mut self, rows: Vec<Row>, cfg: &Config) {
        let sides = self.sides.clone();
        let mut options = self.options.clone();
        options.method = self.method;
        self.reload_rules(cfg);
        let diff = self.comparison_options(cfg);
        self.verified_options = (self.method == CompareMethod::FullContents).then(|| diff.clone());
        // Set once the job has started (which clears them), and before any of its results
        // is taken in, so the progress starts from all of them.
        let (pending, total) = (rows.iter().map(|r| r.path.clone()).collect(), rows.len());
        self.start(move |stop, tx| {
            remote_compare::verify(&sides, rows, &options, &diff, stop, |path, result, kept| {
                let _ = tx.send(Update::Verified(path, result, kept));
            })?;
            Ok(None)
        });
        self.job.as_mut().unwrap().verifying = true;
        self.pending = pending;
        self.verify_total = total;
    }

    /// Drops the copies kept of `path`, which is no longer as it was fetched.
    fn forget_kept(&mut self, path: &str) {
        if let Some(old) = self.kept.remove(path) {
            self.kept_bytes = self.kept_bytes.saturating_sub(kept_size(&old));
        }
    }

    /// Says what a verification found, over the list, and goes to the first file that
    /// differs when more than one was verified.
    fn verification_done(&mut self) {
        let counts = self.counts();
        let verified = |shown: &Shown| {
            matches!(
                shown,
                Shown::Status(Status::ContentDifferent | Status::ContentMatch) | Shown::Error
            )
        };
        let found: Vec<String> = counts
            .iter()
            .filter(|(shown, _)| verified(shown))
            .map(|(shown, count)| format!("{count} {}", tr(shown.key())))
            .collect();
        let differs = counts
            .iter()
            .any(|(shown, _)| *shown != Shown::Status(Status::ContentMatch) && verified(shown));
        if !found.is_empty() {
            self.notice = Some((found.join("  ·  "), differs));
        }
        if self.verify_total > 1 {
            let first = self.rows.iter().position(|row| {
                !row.directory && self.shown(row) == Shown::Status(Status::ContentDifferent)
            });
            if first.is_some() {
                self.selected = first;
                self.scroll_to = first;
            }
        }
    }

    /// What the list shows as the state of `row`: what verifying it found, or else what its
    /// listing says.
    fn shown(&self, row: &Row) -> Shown {
        match self.verified.get(&row.path).filter(|_| !row.directory) {
            Some(Ok(status)) => Shown::Status(*status),
            Some(Err(_)) => Shown::Error,
            None => Shown::Status(row.status),
        }
    }

    /// How many files are in each state, the ones that differ first; folders are not counted.
    fn counts(&mut self) -> Vec<(Shown, usize)> {
        if self.counts.is_none() {
            let mut counts: Vec<(Shown, usize)> = Shown::ORDER.iter().map(|s| (*s, 0)).collect();
            for row in self.rows.iter().filter(|r| !r.directory) {
                let shown = self.shown(row);
                if let Some(c) = counts.iter_mut().find(|c| c.0 == shown) {
                    c.1 += 1;
                }
            }
            counts.retain(|c| c.1 > 0);
            self.counts = Some(counts);
        }
        self.counts.clone().unwrap_or_default()
    }

    fn comparison_options(&self, cfg: &Config) -> crate::core::diff::DiffOptions {
        crate::core::rules::options_with(&cfg.comparison_options(), self.resolved_rules.as_ref())
    }

    pub fn reload_rules(&mut self, cfg: &Config) {
        self.resolved_rules = crate::core::rules::find(&self.rules);
        self.invalidate_old_verification(cfg);
    }

    fn invalidate_old_verification(&mut self, cfg: &Config) {
        if self
            .verified_options
            .as_ref()
            .is_some_and(|options| *options != self.comparison_options(cfg))
        {
            self.verified.clear();
            self.verified_options = None;
            self.counts = None;
            if self.job.as_ref().is_some_and(|job| job.verifying) {
                self.job = None;
                self.pending.clear();
            }
        }
    }

    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &Config,
        named: &[FltFilter],
    ) -> Option<OpenFiles> {
        self.invalidate_old_verification(cfg);
        let opened = self.poll().map(|files| OpenFiles {
            files,
            path: self.opening_path.take().expect("path of opened file pair"),
            read_only: self.read_only,
            rules: self.rules.clone(),
        });
        let mut open = false;
        let mut verify_selected = false;
        let mut verify_all = false;
        let mut refresh = false;
        let paired = |row: &Row| !row.directory && row.present == [true, true];
        let selected = self
            .selected
            .map(|index| &self.rows[index])
            .filter(|r| paired(r));
        let has_selected = selected.is_some();
        let p = pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;

        // The two folders compared, each named by its side.
        egui::Frame::new()
            .fill(p.surface)
            .inner_margin(egui::Margin::symmetric(theme::space::LG, theme::space::SM))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                for (side, label) in self.labels.iter().enumerate() {
                    ui.horizontal(|ui| {
                        widgets::side_chip(ui, side, 2);
                        ui.add(egui::Label::new(egui::RichText::new(label).monospace()).truncate());
                    });
                }
            });

        let busy = self.job.is_some();
        let any_paired = self.rows.iter().any(paired);
        let counts = self.counts();
        widgets::toolbar(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                refresh =
                    widgets::bar_button(ui, true, Some(Icon::Refresh), tr("cmd.refresh"), false)
                        .clicked();
                widgets::toolbar_sep(ui);
                ui.label(tr("label.filter"));
                ui.add_space(4.0);
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("*.*")
                        .desired_width(120.0),
                );
                refresh |=
                    filter.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.add_space(6.0);
                refresh |= ui
                    .checkbox(&mut self.options.recursive, tr("label.recursive"))
                    .changed();
                ui.add_space(6.0);
                let label = self.method.key();
                egui::ComboBox::from_id_salt("remote_contents_method")
                    .selected_text(tr(label))
                    .show_ui(ui, |ui| {
                        for method in [CompareMethod::FullContents, CompareMethod::BinaryContents] {
                            if ui
                                .selectable_value(&mut self.method, method, tr(method.key()))
                                .changed()
                            {
                                self.verified.clear();
                                self.verified_options = None;
                                self.counts = None;
                            }
                        }
                    });
                widgets::toolbar_sep(ui);
                open = widgets::bar_button(
                    ui,
                    has_selected,
                    Some(Icon::Compare),
                    tr("remote_compare.open"),
                    true,
                )
                .clicked();
                ui.add_space(4.0);
                verify_selected = widgets::bar_button(
                    ui,
                    has_selected,
                    Some(Icon::Check),
                    tr("remote_compare.verify_selected"),
                    false,
                )
                .clicked();
                verify_all = widgets::bar_button(
                    ui,
                    any_paired,
                    Some(Icon::Check),
                    tr("remote_compare.verify_all"),
                    false,
                )
                .clicked();
            });
        });

        // What is going on, in a strip of its own so that it cannot be missed: how far a
        // verification is, or that the folders are being listed or a file fetched.
        if let Some(job) = &self.job {
            egui::Frame::new()
                .fill(p.accent_soft)
                .inner_margin(egui::Margin::symmetric(theme::space::LG, theme::space::SM))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let seconds = format!("{} s", job.started.elapsed().as_secs());
                        let remote = self.sides.iter().find_map(|side| match side {
                            Side::Sftp(conn, _) => Some(conn),
                            Side::Local(_) => None,
                        });
                        let connecting =
                            remote.filter(|_| !job.progress.connected.load(Ordering::Relaxed));
                        if let Some(conn) = connecting {
                            // Nothing can be counted until the server is reached.
                            let server = crate::core::remote::label(conn, "/");
                            let server = server.trim_end_matches('/').to_string();
                            ui.label(trf("msg.remote_connecting", &[("server", server)]));
                        } else if self.verify_total > 0 {
                            let done =
                                self.verify_total - self.pending.len().min(self.verify_total);
                            ui.add(
                                egui::ProgressBar::new(done as f32 / self.verify_total as f32)
                                    .desired_width(260.0)
                                    .desired_height(14.0),
                            );
                            ui.label(progress_text(done, self.verify_total));
                        } else if let Some(path) = &self.opening_path {
                            let read = job.progress.bytes.load(Ordering::Relaxed);
                            if self.opening_bytes > 0 {
                                let part = read as f32 / self.opening_bytes as f32;
                                ui.add(
                                    egui::ProgressBar::new(part.min(1.0))
                                        .desired_width(260.0)
                                        .desired_height(14.0),
                                );
                            }
                            ui.label(format!(
                                "{}  {}",
                                trf("msg.fetching_remote", &[("name", path.clone())]),
                                bytes_text(read, self.opening_bytes)
                            ));
                        } else {
                            // The entries the servers have sent, which run ahead of the rows:
                            // a folder's rows come once both sides of it are listed.
                            let listed = job.progress.listed.load(Ordering::Relaxed) as usize;
                            ui.label(trf(
                                "remote_compare.entries",
                                &[("count", listed.max(self.rows.len()).to_string())],
                            ));
                        }
                        ui.weak(seconds);
                        ui.add_space(8.0);
                        if widgets::bar_button(ui, true, Some(Icon::Stop), tr("btn.stop"), false)
                            .clicked()
                        {
                            job.stop.store(true, Ordering::Relaxed);
                        }
                    });
                });
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
        if let Some(error) = &self.error {
            widgets::info_bar(ui, Icon::Warning, error, p.danger);
        }
        if let Some((found, differs)) = &self.notice {
            let (icon, color) = if *differs {
                (Icon::Warning, p.warning)
            } else {
                (Icon::Check, p.success)
            };
            widgets::info_bar(ui, icon, found, color);
        }

        // How many files are in each state, the ones that differ first.
        if !counts.is_empty() {
            egui::Frame::new()
                .fill(p.surface)
                .inner_margin(egui::Margin::symmetric(theme::space::LG, theme::space::SM))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                        for (shown, count) in &counts {
                            let (fg, bg, _) = shown.tone(ui);
                            widgets::chip(ui, &format!("{count}  {}", tr(shown.key())), fg, bg);
                        }
                    });
                });
        }

        // The list: a name, a state and the two sizes, each in a column of its own.
        let (header, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
        let cols = Columns::of(header);
        ui.painter().rect_filled(header, 0.0, p.surface_alt);
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            egui::Stroke::new(1.0_f32, p.border),
        );
        let caption = |x: f32, align: Align2, text: String| {
            ui.painter().text(
                pos2(x, header.center().y),
                align,
                text,
                theme::semibold(theme::size::SMALL),
                p.text_dim,
            );
        };
        caption(
            cols.name.min + 24.0,
            Align2::LEFT_CENTER,
            tr("col.name").into(),
        );
        caption(
            cols.status.min,
            Align2::LEFT_CENTER,
            tr("analysis.tree_status").into(),
        );
        for (side, col) in [cols.left, cols.right].into_iter().enumerate() {
            let side = crate::core::report::side_name(side, 2);
            caption(
                col.max,
                Align2::RIGHT_CENTER,
                format!("{side} {}", tr("col.size")),
            );
        }
        let mut list = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(index) = self.scroll_to.take() {
            list = list.vertical_scroll_offset((index as f32 - 2.0).max(0.0) * ROW_HEIGHT);
        }
        list.show_rows(ui, ROW_HEIGHT, self.rows.len(), |ui, range| {
            for index in range {
                let row = &self.rows[index];
                let shown = self.shown(row);
                let error = match self.verified.get(&row.path) {
                    Some(Err(error)) if !row.directory => Some(error.as_str()),
                    _ => None,
                };
                let waiting = self.pending.contains(&row.path);
                let response = remote_compare_row(
                    ui,
                    row,
                    shown,
                    waiting,
                    self.selected == Some(index),
                    index % 2 == 1,
                );
                if response.clicked() {
                    self.selected = Some(index);
                }
                open |= response.double_clicked() && self.job.is_none() && paired(row);
                if let Some(error) = error {
                    response.on_hover_text(error);
                }
            }
        });
        let row = self
            .selected
            .map(|index| &self.rows[index])
            .filter(|row| paired(row));
        let mut opened = opened;
        if refresh {
            self.refresh(named);
        } else if let (true, Some(row)) = (open, row) {
            let sides = self.sides.clone();
            let row = row.clone();
            let path = row.path.clone();
            // A pair a verification fetched opens from those copies, without another fetch.
            if let Some(kept) = self.kept.remove(&path) {
                self.kept_bytes = self.kept_bytes.saturating_sub(kept_size(&kept));
                match remote_compare::open_kept(&sides, &row, kept) {
                    Ok(files) => {
                        return Some(OpenFiles {
                            files,
                            path,
                            read_only: self.read_only,
                            rules: self.rules.clone(),
                        })
                    }
                    Err(error) => {
                        self.error = Some(format!("{error:#}"));
                        return opened.take();
                    }
                }
            }
            // What is fetched: each side on a server, at the size its listing gave.
            let bytes = (self.sides.iter().zip(row.sizes))
                .filter(|(side, _)| matches!(side, Side::Sftp(..)))
                .filter_map(|(_, size)| size)
                .sum();
            self.start(move |stop, _| remote_compare::open(&sides, &row, stop).map(Some));
            self.opening_path = Some(path);
            self.opening_bytes = bytes;
        } else if verify_all {
            // Files verified since the last refresh are not read again; *Verify Selected*
            // reads one again, and *Refresh* all of them.
            let rows: Vec<Row> = (self.rows.iter())
                .filter(|r| paired(r) && !matches!(self.verified.get(&r.path), Some(Ok(_))))
                .cloned()
                .collect();
            if rows.is_empty() {
                self.verify_total = 0;
                self.verification_done();
            } else {
                self.verify(rows, cfg);
            }
        } else if let (true, Some(row)) = (verify_selected, row) {
            self.verify(vec![row.clone()], cfg);
        }
        opened
    }
}

/// The most bytes of fetched copies kept for opening verified pairs later.
const MAX_KEPT_BYTES: u64 = 512 << 20;

/// The size of the copies kept of one pair.
fn kept_size(kept: &[crate::core::remote::Fetched]) -> u64 {
    kept.iter()
        .filter_map(|f| std::fs::metadata(&f.local).ok())
        .map(|m| m.len())
        .sum()
}

/// How far a verification is: the files read, of those to read.
fn progress_text(done: usize, total: usize) -> String {
    format!("{done} / {total}")
}

/// How much of a fetch has come: the bytes read, of those to read when that is known.
fn bytes_text(read: u64, total: u64) -> String {
    let read = crate::core::report::fmt_size(Some(read));
    if total > 0 {
        format!("{read} / {}", crate::core::report::fmt_size(Some(total)))
    } else {
        read
    }
}

const HEADER_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 26.0;

/// Where the columns of the list are across `rect`, a row or the header: the name takes what
/// the state and the two sizes leave.
struct Columns {
    name: egui::Rangef,
    status: egui::Rangef,
    left: egui::Rangef,
    right: egui::Rangef,
}

impl Columns {
    fn of(rect: Rect) -> Self {
        const STATUS: f32 = 190.0;
        const SIZE: f32 = 96.0;
        let end = rect.right() - 12.0;
        let right = egui::Rangef::new(end - SIZE, end);
        let left = egui::Rangef::new(right.min - SIZE, right.min);
        let status = egui::Rangef::new(left.min - STATUS, left.min);
        let start = rect.left() + 12.0;
        let name = egui::Rangef::new(start, (status.min - 12.0).max(start + 60.0));
        Self {
            name,
            status,
            left,
            right,
        }
    }
}

/// The state the list shows for a file: its [`Status`], or that verifying it failed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    Status(Status),
    Error,
}

impl Shown {
    /// The states the summary counts, the ones that need a look first.
    const ORDER: [Shown; 8] = [
        Shown::Status(Status::ContentDifferent),
        Shown::Status(Status::MetadataDifferent),
        Shown::Error,
        Shown::Status(Status::LeftOnly),
        Shown::Status(Status::RightOnly),
        Shown::Status(Status::Unverified),
        Shown::Status(Status::ContentMatch),
        Shown::Status(Status::MetadataMatch),
    ];

    fn key(self) -> &'static str {
        match self {
            Shown::Status(status) => status_key(status),
            Shown::Error => "status.error",
        }
    }

    /// The colors of its text and of the pill behind it, and the icon before it: red for
    /// files that differ, amber for ones that may, green for ones read and found the same,
    /// the side's own color for a file only one side has, and grey for what is not known.
    fn tone(self, ui: &egui::Ui) -> (Color32, Color32, Option<Icon>) {
        let p = pal(ui);
        let (color, icon) = match self {
            Shown::Status(Status::ContentDifferent) => (p.danger, Some(Icon::Close)),
            Shown::Error => (p.danger, Some(Icon::Warning)),
            Shown::Status(Status::MetadataDifferent) => (p.warning, Some(Icon::Warning)),
            Shown::Status(Status::ContentMatch) => (p.success, Some(Icon::Check)),
            Shown::Status(Status::LeftOnly) => (widgets::side_color(ui, 0, 2), Some(Icon::Prev)),
            Shown::Status(Status::RightOnly) => (widgets::side_color(ui, 1, 2), Some(Icon::Next)),
            Shown::Status(Status::MetadataMatch | Status::Unverified | Status::Folder) => {
                return (p.text_dim, p.control, None);
            }
        };
        let soft = if ui.visuals().dark_mode { 0.24 } else { 0.14 };
        (color, color.gamma_multiply(soft), icon)
    }
}

/// One row of the list, as wide as the list: an icon and the path, cut short if it is too
/// long, with the folders it is in dimmed; its state in a colored pill, with a spinner while
/// a verification has yet to read it; and the size on each side.
fn remote_compare_row(
    ui: &mut egui::Ui,
    row: &Row,
    shown: Shown,
    waiting: bool,
    selected: bool,
    striped: bool,
) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &row.path));
    let cols = Columns::of(rect);
    let status_text = tr(shown.key());
    // The state is read out, and found by its words, as a label of its own.
    let status_rect = Rect::from_x_y_ranges(cols.status, rect.y_range());
    ui.interact(status_rect, resp.id.with("status"), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, status_text));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let fill = if selected {
        p.accent_soft
    } else if resp.hovered() {
        p.control_hover
    } else if striped {
        p.surface_alt.gamma_multiply(0.5)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let mid = rect.center().y;

    let (icon, tint) = if row.directory {
        (Icon::Folder, p.folder)
    } else {
        (Icon::File, p.text_dim)
    };
    let icon_rect = Rect::from_center_size(pos2(cols.name.min + 8.0, mid), egui::Vec2::splat(16.0));
    crate::ui::icons::paint(ui.painter(), icon_rect, icon, tint);
    let font = FontId::proportional(theme::size::DENSE);
    let (folder, name) = match row.path.rfind('/') {
        Some(i) => row.path.split_at(i + 1),
        None => ("", row.path.as_str()),
    };
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    job.append(folder, 0.0, format(p.text_dim));
    job.append(name, 0.0, format(p.text));
    let left = cols.name.min + 24.0;
    let galley = egui::WidgetText::from(job).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (cols.name.max - left).max(24.0),
        font.clone(),
    );
    ui.painter()
        .galley(pos2(left, mid - galley.size().y / 2.0), galley, p.text);

    let (fg, bg, mark) = shown.tone(ui);
    let text = ui.painter().layout_no_wrap(
        status_text.to_string(),
        theme::semibold(theme::size::SMALL),
        fg,
    );
    let lead = if waiting || mark.is_some() { 16.0 } else { 0.0 };
    let pill = Rect::from_min_size(
        pos2(cols.status.min, mid - 10.0),
        vec2(
            (text.size().x + lead + 18.0).min(cols.status.span() - 8.0),
            20.0,
        ),
    );
    ui.painter().rect_filled(pill, 10.0, bg);
    let mark_rect = Rect::from_center_size(pos2(pill.left() + 15.0, mid), egui::Vec2::splat(11.0));
    if waiting {
        egui::Spinner::new().color(fg).paint_at(ui, mark_rect);
    } else if let Some(mark) = mark {
        crate::ui::icons::paint(ui.painter(), mark_rect, mark, fg);
    }
    ui.painter().galley(
        pos2(pill.left() + 9.0 + lead, mid - text.size().y / 2.0),
        text,
        fg,
    );

    if !row.directory {
        for (col, bytes) in [cols.left, cols.right].into_iter().zip(row.sizes) {
            ui.painter().text(
                pos2(col.max, mid),
                Align2::RIGHT_CENTER,
                size(bytes),
                FontId::proportional(theme::size::SMALL),
                p.text_dim,
            );
        }
    }
    resp
}

fn size(size: Option<u64>) -> String {
    size.map_or_else(
        || "—".into(),
        |size| crate::core::report::fmt_size(Some(size)),
    )
}

fn status_key(status: Status) -> &'static str {
    match status {
        Status::Folder => "remote_compare.folder",
        Status::LeftOnly => "remote_compare.left_only",
        Status::RightOnly => "remote_compare.right_only",
        Status::MetadataMatch => "remote_compare.metadata_match",
        Status::MetadataDifferent => "remote_compare.metadata_different",
        Status::Unverified => "remote_compare.unverified",
        Status::ContentMatch => "remote_compare.content_match",
        Status::ContentDifferent => "remote_compare.content_different",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_worded_as_done_of_total() {
        assert_eq!(progress_text(3, 120), "3 / 120");
        assert_eq!(bytes_text(0, 0), size(Some(0)));
        assert_eq!(
            bytes_text(1024, 4096),
            format!("{} / {}", size(Some(1024)), size(Some(4096)))
        );
    }
}

#[cfg(test)]
mod threaded_tests {
    use super::*;

    /// Runs frames of `view` until its job is done.
    fn settle(ctx: &egui::Context, view: &mut RemoteFolder, cfg: &Config) {
        for _ in 0..2000 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                view.ui(ui, cfg, &[]);
            });
            out.textures_delta.clear();
            if view.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("still busy");
    }

    #[test]
    fn verifying_all_on_another_thread_marks_every_paired_file() {
        let dir = tempfile::tempdir().unwrap();
        for (side, b) in [("l", "same"), ("r", "same")] {
            std::fs::create_dir_all(dir.path().join(side).join("sub")).unwrap();
            std::fs::write(dir.path().join(side).join("a.txt"), side).unwrap();
            std::fs::write(dir.path().join(side).join("sub/b.txt"), b).unwrap();
        }
        std::fs::write(dir.path().join("l/only.txt"), "x").unwrap();
        let cfg = Config::default();
        let ctx = egui::Context::default();
        let sides = [
            Side::Local(dir.path().join("l")),
            Side::Local(dir.path().join("r")),
        ];
        let mut options = cfg.folder.clone();
        options.recursive = true;
        let mut view = RemoteFolder::new(
            sides,
            "*.*".into(),
            options,
            [false, false],
            String::new(),
            &[],
        );
        settle(&ctx, &mut view, &cfg);
        assert_eq!(view.error, None);
        let paired: Vec<Row> = view
            .rows
            .iter()
            .filter(|r| !r.directory && r.present == [true, true])
            .cloned()
            .collect();
        assert_eq!(paired.len(), 2);
        view.verify(paired, &cfg);
        assert_eq!((view.verify_total, view.pending.len()), (2, 2));
        settle(&ctx, &mut view, &cfg);
        assert_eq!(view.error, None);
        assert!(view.pending.is_empty());
        assert_eq!(
            view.verified.get("a.txt"),
            Some(&Ok(Status::ContentDifferent))
        );
        assert_eq!(
            view.verified.get("sub/b.txt"),
            Some(&Ok(Status::ContentMatch))
        );
    }
}
