use crate::config::Config;
use crate::core::filter::{FileFilter, FltFilter};
use crate::core::folder::{CompareMethod, FolderOptions};
use crate::core::remote_compare::{self, Opened, Row, Side, Status};
use crate::i18n::{tr, trf};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

enum Update {
    Listed(Vec<Row>),
    Verified(String, anyhow::Result<bool>),
    Finished(anyhow::Result<Option<Box<[Opened; 2]>>>),
}

struct Job {
    updates: mpsc::Receiver<Update>,
    stop: Arc<AtomicBool>,
    verifying: bool,
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
        let run = move || {
            let result = work(&cancel, &send);
            let _ = send.send(Update::Finished(result.map(|opened| opened.map(Box::new))));
        };
        #[cfg(test)]
        if crate::core::remote::fake::installed() {
            run();
            return Self {
                updates,
                stop,
                verifying: false,
            };
        }
        std::thread::spawn(run);
        Self {
            updates,
            stop,
            verifying: false,
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
    opening_path: Option<String>,
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
            opening_path: None,
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
        self.opening_path = None;
        self.job = Some(Job::start(work));
    }

    fn poll(&mut self) -> Option<[Opened; 2]> {
        let job = self.job.as_ref()?;
        loop {
            match job.updates.try_recv() {
                Ok(Update::Listed(rows)) => self.rows.extend(rows),
                Ok(Update::Verified(path, result)) => {
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
                    self.job = None;
                    if cancelled {
                        self.error = Some(tr("msg.sftp_cancelled").to_string());
                        return None;
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
        self.start(move |stop, tx| {
            remote_compare::verify(&sides, rows, &options, &diff, stop, |path, result| {
                let _ = tx.send(Update::Verified(path, result));
            })?;
            Ok(None)
        });
        self.job.as_mut().unwrap().verifying = true;
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
            if self.job.as_ref().is_some_and(|job| job.verifying) {
                self.job = None;
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
        ui.add_space(8.0);
        for label in &self.labels {
            ui.label(label);
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(self.job.is_none(), |ui| {
                refresh = ui.button(tr("cmd.refresh")).clicked();
                ui.label(tr("label.filter"));
                let filter =
                    ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(110.0));
                refresh |=
                    filter.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                refresh |= ui
                    .checkbox(&mut self.options.recursive, tr("label.recursive"))
                    .changed();
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
                            }
                        }
                    });
                open = ui
                    .add_enabled(
                        selected.is_some(),
                        egui::Button::new(tr("remote_compare.open")),
                    )
                    .clicked();
                verify_selected = ui
                    .add_enabled(
                        selected.is_some(),
                        egui::Button::new(tr("remote_compare.verify_selected")),
                    )
                    .clicked();
                verify_all = ui
                    .add_enabled(
                        self.rows.iter().any(paired),
                        egui::Button::new(tr("remote_compare.verify_all")),
                    )
                    .clicked();
            });
            if let Some(job) = &self.job {
                ui.spinner();
                if ui.button(tr("btn.stop")).clicked() {
                    job.stop.store(true, Ordering::Relaxed);
                }
                ui.ctx().request_repaint_after(Duration::from_millis(60));
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        let height = ui.spacing().interact_size.y;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show_rows(ui, height, self.rows.len(), |ui, range| {
                for index in range {
                    let row = &self.rows[index];
                    let result = self.verified.get(&row.path).filter(|_| !row.directory);
                    let status = match result {
                        Some(Ok(status)) => tr(status_key(*status)),
                        Some(Err(_)) => tr("status.error"),
                        None => tr(status_key(row.status)),
                    };
                    ui.horizontal(|ui| {
                        ui.set_min_height(height);
                        let response = ui.selectable_label(self.selected == Some(index), &row.path);
                        if response.clicked() {
                            self.selected = Some(index);
                        }
                        open |= response.double_clicked() && self.job.is_none() && paired(row);
                        let badge = ui.weak(status);
                        if let Some(Err(error)) = result {
                            badge.on_hover_text(error);
                        }
                        if !row.directory {
                            ui.weak(format!("{} ↔ {}", size(row.sizes[0]), size(row.sizes[1])));
                        }
                    });
                }
            });
        let row = self
            .selected
            .map(|index| &self.rows[index])
            .filter(|row| paired(row));
        if refresh {
            self.refresh(named);
        } else if let (true, Some(row)) = (open, row) {
            let sides = self.sides.clone();
            let row = row.clone();
            let path = row.path.clone();
            self.start(move |stop, _| remote_compare::open(&sides, &row, stop).map(Some));
            self.opening_path = Some(path);
        } else if verify_all {
            self.verify(
                self.rows.iter().filter(|r| paired(r)).cloned().collect(),
                cfg,
            );
        } else if let (true, Some(row)) = (verify_selected, row) {
            self.verify(vec![row.clone()], cfg);
        }
        opened
    }
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
