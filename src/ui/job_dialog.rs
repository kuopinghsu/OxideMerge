//! *File → Save as job…* and *File → Run job…*: a folder comparison saved as a job file
//! (`.oxjob`), and a job file run as `--job` runs it, with its results listed.
//!
//! Running goes through the same code as `--job`, so a job gives in the program what it gives on
//! the command line. A job file that synchronizes folders runs only after it says which folders
//! it changes and the user agrees, each time; that agreement stands for `--yes`.

use crate::config::Config;
use crate::core::job::{Job, JobFile, JobKind};
use crate::i18n::{tr, trf};
use crate::ui::widgets;
use egui::{Key, RichText};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// What the job saved from a folder comparison does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveKind {
    Compare,
    SyncPlan,
    Sync,
}

pub struct SaveJob {
    /// The comparison, as a compare job.
    template: Job,
    pub name: String,
    pub kind: SaveKind,
    /// The report (compare) or plan (sync plan) file, relative to the job file unless absolute.
    pub output: String,
    pub update: bool,
    /// Right to left rather than left to right.
    pub reverse: bool,
    pub relative: bool,
    pub allow_large_deletion: bool,
    /// Synchronize symbolic links as links.
    pub copy_links: bool,
    error: Option<String>,
}

/// What the save dialog asks of the app.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveStep {
    /// Pick the job file and call [`SaveJob::write`].
    Save,
    Close,
}

impl SaveJob {
    /// `template` is the comparison as a compare job; `stem` names the files it suggests.
    pub fn new(template: Job, stem: &str) -> SaveJob {
        SaveJob {
            template,
            name: stem.to_string(),
            kind: SaveKind::Compare,
            output: format!("{stem}.html"),
            update: false,
            reverse: false,
            relative: false,
            allow_large_deletion: false,
            copy_links: false,
            error: None,
        }
    }

    fn two_sides(&self) -> bool {
        self.template.base.is_none()
    }

    /// The job as set in the dialog, to be saved as `job_file`.
    pub fn job(&self, job_file: &Path) -> Result<Job, String> {
        let t = &self.template;
        if self.kind != SaveKind::Compare && !self.two_sides() {
            return Err(tr("job.two_folders").into());
        }
        let output = Some(self.output.trim().to_string()).filter(|o| !o.is_empty());
        let mut job = match self.kind {
            SaveKind::Compare => Job {
                kind: JobKind::Compare,
                report: output,
                ..t.clone()
            },
            SaveKind::SyncPlan | SaveKind::Sync => {
                let (source, destination) = if self.reverse {
                    (t.right.clone(), t.left.clone())
                } else {
                    (t.left.clone(), t.right.clone())
                };
                let sync = self.kind == SaveKind::Sync;
                Job {
                    kind: if sync {
                        JobKind::Sync
                    } else {
                        JobKind::SyncPlan
                    },
                    source,
                    destination,
                    mode: Some(if self.update { "update" } else { "mirror" }.into()),
                    apply: sync,
                    allow_large_deletion: sync && self.allow_large_deletion,
                    copy_links: self.copy_links,
                    report: if sync { None } else { output },
                    filter: t.filter.clone(),
                    method: t.method.clone(),
                    rules: t.rules.clone(),
                    structured: t.structured,
                    whitespace: t.whitespace.clone(),
                    ignore_case: t.ignore_case,
                    ignore_blank: t.ignore_blank,
                    ignore_eol: t.ignore_eol,
                    ignore_numbers: t.ignore_numbers,
                    ignore_comments: t.ignore_comments,
                    prettify: t.prettify,
                    numeric_tolerance: t.numeric_tolerance,
                    ..Default::default()
                }
            }
        };
        job.name = Some(self.name.trim().to_string()).filter(|n| !n.is_empty());
        if self.relative {
            job = job.relative_to(job_file).map_err(|e| e.to_string())?;
        }
        job.validate()?;
        Ok(job)
    }

    /// Writes the job to `path`.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let job = self.job(path)?;
        crate::core::fsutil::write_atomic(path, JobFile::single(job).to_text().as_bytes())
            .map_err(|e| format!("{}: {e:#}", path.display()))
    }

    /// Shows why saving failed, in the dialog.
    pub fn failed(&mut self, e: String) {
        self.error = Some(e);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<SaveStep> {
        let mut step = None;
        let modal = egui::Modal::new(egui::Id::unique("save_job_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            let pal = widgets::pal(ui);
            if widgets::dialog_title(ui, &tr("dlg.save_job").replace("...", "")) {
                step = Some(SaveStep::Close);
            }
            let side = |s: &Option<String>| s.clone().unwrap_or_default();
            ui.label(RichText::new(side(&self.template.left)).monospace());
            if let Some(b) = &self.template.base {
                ui.label(RichText::new(b).monospace());
            }
            ui.label(RichText::new(side(&self.template.right)).monospace());
            ui.add_space(8.0);
            egui::Grid::new("save_job_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(tr("job.name"));
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(340.0));
                    ui.end_row();
                    ui.label(tr("job.kind"));
                    ui.vertical(|ui| {
                        let before = self.kind;
                        ui.radio_value(&mut self.kind, SaveKind::Compare, tr("job.kind_compare"));
                        ui.add_enabled_ui(self.two_sides(), |ui| {
                            ui.radio_value(
                                &mut self.kind,
                                SaveKind::SyncPlan,
                                tr("job.kind_sync_plan"),
                            );
                            ui.radio_value(&mut self.kind, SaveKind::Sync, tr("job.kind_sync"));
                        });
                        if before != self.kind {
                            self.suggest_output(before);
                        }
                    });
                    ui.end_row();
                    if self.kind != SaveKind::Sync {
                        ui.label(if self.kind == SaveKind::Compare {
                            tr("job.report")
                        } else {
                            tr("job.plan")
                        });
                        ui.vertical(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.output).desired_width(340.0),
                            );
                            ui.label(
                                RichText::new(if self.kind == SaveKind::Compare {
                                    tr("job.report_hint")
                                } else {
                                    tr("job.plan_hint")
                                })
                                .small()
                                .color(pal.text_dim),
                            );
                        });
                        ui.end_row();
                    }
                    if self.kind != SaveKind::Compare {
                        ui.label(tr("job.direction"));
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.reverse, false, tr("job.left_to_right"));
                            ui.radio_value(&mut self.reverse, true, tr("job.right_to_left"));
                        });
                        ui.end_row();
                        ui.label(tr("job.mode"));
                        ui.vertical(|ui| {
                            ui.radio_value(&mut self.update, false, tr("job.mirror"));
                            ui.radio_value(&mut self.update, true, tr("job.update"));
                        });
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            ui.checkbox(&mut self.relative, tr("job.relative"));
            if self.kind != SaveKind::Compare && cfg!(unix) {
                ui.checkbox(&mut self.copy_links, tr("folder_sync.copy_links"))
                    .on_hover_text(tr("folder_sync.copy_links_tip"));
            }
            if self.kind == SaveKind::Sync {
                if !self.update {
                    ui.checkbox(&mut self.allow_large_deletion, tr("job.large_deletion"));
                }
                ui.label(RichText::new(tr("job.sync_note")).color(pal.warning));
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(pal.danger));
            }
            widgets::dialog_footer(ui, |ui| {
                if widgets::primary_button(ui, &tr("btn.save").replace("...", "")).clicked() {
                    step = Some(SaveStep::Save);
                }
                if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape))
                {
                    step = Some(SaveStep::Close);
                }
            });
        });
        if modal.should_close() && step.is_none() {
            step = Some(SaveStep::Close);
        }
        step
    }

    /// Follows the kind with the output name, unless the user typed their own.
    fn suggest_output(&mut self, before: SaveKind) {
        let stem = self.name.trim();
        let suggested = |k: SaveKind| match k {
            SaveKind::Compare => format!("{stem}.html"),
            SaveKind::SyncPlan => format!("{stem}-plan.json"),
            SaveKind::Sync => String::new(),
        };
        if self.output.trim().is_empty() || self.output == suggested(before) {
            self.output = suggested(self.kind);
        }
    }
}

enum RunState {
    /// The file changes folders: waiting for the user to agree.
    Confirm,
    Running(Receiver<Result<crate::JobsReport, String>>),
    Done(crate::JobsReport),
    Failed(String),
}

pub struct RunJob {
    pub path: PathBuf,
    file: Option<JobFile>,
    state: RunState,
}

/// What the run dialog asks of the app.
#[derive(Debug, PartialEq, Eq)]
pub enum RunStep {
    Close,
    /// Run the file; `yes` lets its sync jobs change folders.
    Start {
        yes: bool,
    },
    Open(PathBuf),
    Compare(Vec<PathBuf>),
}

impl RunJob {
    /// Reads the job file at `path`. A file that changes no folder can be started at once.
    pub fn new(path: PathBuf) -> RunJob {
        let read = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
            .and_then(|t| JobFile::parse(&t));
        match read {
            Ok(file) => RunJob {
                path,
                file: Some(file),
                state: RunState::Confirm,
            },
            Err(e) => RunJob {
                state: RunState::Failed(format!("{}: {e}", path.display())),
                path,
                file: None,
            },
        }
    }

    /// Whether running the file needs the user's agreement first.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self.state, RunState::Confirm)
            && self.file.as_ref().is_some_and(JobFile::changes_folders)
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, RunState::Running(_))
    }

    /// Runs the file on another thread, from the settings `cfg`.
    pub fn start(&mut self, cfg: Config, yes: bool) {
        if self.file.is_none() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = self.path.clone();
        std::thread::spawn(move || {
            let r = crate::job_summary(&path, yes, &cfg).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
        self.state = RunState::Running(rx);
    }

    /// Picks up the result of a run, once there is one. Returns whether it just ended.
    pub fn poll(&mut self) -> bool {
        let RunState::Running(rx) = &self.state else {
            return false;
        };
        self.state = match rx.try_recv() {
            Ok(Ok(summary)) => RunState::Done(summary),
            Ok(Err(e)) => RunState::Failed(e),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => RunState::Failed(tr("status.error").into()),
        };
        true
    }

    /// The worst exit code of the run, once it has ended.
    #[cfg(test)]
    pub fn exit_code(&self) -> Option<u8> {
        match &self.state {
            RunState::Done(s) => Some(s.exit_code),
            RunState::Failed(_) => Some(2),
            _ => None,
        }
    }

    /// One line per job of a finished run: name, how it ended, and whether that is a problem.
    pub fn results(&self) -> Vec<(String, String, bool)> {
        let (RunState::Done(s), Some(file)) = (&self.state, &self.file) else {
            return Vec::new();
        };
        let mut rows: Vec<(String, String, bool)> = s
            .jobs
            .iter()
            .zip(&file.jobs)
            .map(|(r, job)| {
                let (text, bad) = outcome(r, job.kind);
                (r.name.clone(), text, bad)
            })
            .collect();
        rows.extend(
            s.not_run
                .iter()
                .map(|n| (n.clone(), tr("job.not_run").into(), true)),
        );
        rows
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<RunStep> {
        if self.poll() {
            ctx.request_repaint();
        }
        let mut step = None;
        let mut again = false;
        let dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        let modal = egui::Modal::new(egui::Id::unique("run_job_dialog")).show(ctx, |ui| {
            ui.set_min_width(520.0);
            ui.set_max_width(760.0);
            let pal = widgets::pal(ui);
            let running = self.is_running();
            if widgets::dialog_title(ui, &tr("dlg.run_job").replace("...", "")) && !running {
                step = Some(RunStep::Close);
            }
            ui.label(RichText::new(self.path.display().to_string()).monospace());
            ui.add_space(6.0);
            match &self.state {
                RunState::Confirm => {
                    let file = self.file.as_ref();
                    let syncs: Vec<&Job> = file
                        .map(|f| f.jobs.iter().filter(|j| j.kind == JobKind::Sync).collect())
                        .unwrap_or_default();
                    ui.label(RichText::new(tr("job.confirm")).color(pal.warning).strong());
                    for j in syncs {
                        let p = j.paths(&dir);
                        let at = |i: usize| {
                            p.get(i)
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        };
                        let mode = if j.mode.as_deref() == Some("update") {
                            tr("job.update")
                        } else {
                            tr("job.mirror")
                        };
                        ui.label(format!("• {}", j.label(0)));
                        ui.label(
                            RichText::new(format!("   {mode}\n   {} → {}", at(0), at(1)))
                                .monospace(),
                        );
                    }
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("job.run_changes")).clicked() {
                            step = Some(RunStep::Start { yes: true });
                        }
                        if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            step = Some(RunStep::Close);
                        }
                    });
                }
                RunState::Running(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("job.running"));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                }
                RunState::Failed(e) => {
                    ui.label(RichText::new(e).color(pal.danger));
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::secondary_button(ui, tr("btn.close")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            step = Some(RunStep::Close);
                        }
                    });
                }
                RunState::Done(_) => {
                    let rows = self.results();
                    let jobs = self
                        .file
                        .as_ref()
                        .map(|f| f.jobs.as_slice())
                        .unwrap_or_default();
                    egui::Grid::new("run_job_results")
                        .num_columns(3)
                        .striped(true)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.strong(tr("job.col_job"));
                            ui.strong(tr("job.col_result"));
                            ui.label("");
                            ui.end_row();
                            for (i, (name, text, bad)) in rows.iter().enumerate() {
                                ui.label(name);
                                let color = if *bad { pal.danger } else { pal.text };
                                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                                ui.horizontal(|ui| {
                                    let Some(job) = jobs.get(i) else { return };
                                    if let Some(out) = job.output(&dir).filter(|o| o.is_file()) {
                                        if ui.small_button(tr("job.open_output")).clicked() {
                                            step = Some(RunStep::Open(out));
                                        }
                                    }
                                    let paths = job.paths(&dir);
                                    if paths.len() >= 2
                                        && paths.iter().all(|p| p.exists())
                                        && ui.small_button(tr("job.open_compare")).clicked()
                                    {
                                        step = Some(RunStep::Compare(paths));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("btn.close")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            step = Some(RunStep::Close);
                        }
                        if widgets::secondary_button(ui, tr("job.run_again")).clicked() {
                            again = true;
                        }
                    });
                }
            }
        });
        if again {
            // A file that changes folders asks again before every run.
            if self.file.as_ref().is_some_and(JobFile::changes_folders) {
                self.state = RunState::Confirm;
            } else {
                step = Some(RunStep::Start { yes: false });
            }
        }
        if modal.should_close() && step.is_none() && !self.is_running() {
            step = Some(RunStep::Close);
        }
        step
    }
}

/// How a job ended, in words, and whether that is a problem.
fn outcome(r: &crate::JobReport, kind: JobKind) -> (String, bool) {
    if let Some(e) = &r.error {
        return (trf("job.failed", &[("error", e.clone())]), true);
    }
    if let Some(s) = &r.sync {
        let mut text = match s.status {
            "applied" => trf("job.applied", &[("n", s.changes.to_string())]),
            "not_applied" => tr("job.not_applied").into(),
            "rolled_back" => tr("job.rolled_back").into(),
            _ => tr("job.recovery").into(),
        };
        for m in &s.messages {
            text.push('\n');
            text.push_str(m);
        }
        return (text, s.status != "applied");
    }
    match (kind, r.exit_code) {
        (JobKind::Compare, 0) => (tr("job.identical").into(), false),
        (JobKind::Compare, 1) => (tr("job.different").into(), false),
        (JobKind::SyncPlan, 0 | 1) => (tr("job.planned").into(), false),
        (_, code) => (trf("job.exit_code", &[("code", code.to_string())]), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("left"), dir.path().join("right"));
        for (side, text) in [(&l, "one\n"), (&r, "two\n")] {
            std::fs::create_dir_all(side).unwrap();
            std::fs::write(side.join("a.txt"), text).unwrap();
            std::fs::write(side.join("same.txt"), "same\n").unwrap();
        }
        std::fs::write(l.join("only.txt"), "left only\n").unwrap();
        (dir, l, r)
    }

    fn template(l: &Path, r: &Path) -> Job {
        Job {
            left: Some(l.display().to_string()),
            right: Some(r.display().to_string()),
            recursive: true,
            filter: Some("*.txt".into()),
            method: Some("full".into()),
            ignore_case: true,
            ..Default::default()
        }
    }

    fn run(path: &Path, yes: bool) -> RunJob {
        let mut run = RunJob::new(path.to_path_buf());
        run.start(Config::default(), yes);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while run.is_running() {
            assert!(std::time::Instant::now() < until, "the job finishes");
            run.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run
    }

    #[test]
    fn a_saved_compare_job_runs_and_writes_its_report_beside_the_job_file() {
        let (dir, l, r) = folders();
        let mut d = SaveJob::new(template(&l, &r), "left_vs_right");
        d.relative = true;
        let file = dir.path().join("jobs").join("nightly.oxjob");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        d.write(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let saved = JobFile::parse(&text).unwrap();
        let job = &saved.jobs[0];
        assert_eq!(job.kind, JobKind::Compare);
        assert_eq!(job.left.as_deref(), Some("../left"), "{text}");
        assert_eq!(job.right.as_deref(), Some("../right"));
        assert_eq!(job.report.as_deref(), Some("left_vs_right.html"));
        assert_eq!(job.filter.as_deref(), Some("*.txt"));
        assert!(job.ignore_case && job.recursive);
        assert_eq!(job.name.as_deref(), Some("left_vs_right"));

        let run = run(&file, false);
        assert_eq!(run.exit_code(), Some(1), "the folders differ");
        let rows = run.results();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, tr("job.different"));
        assert!(!rows[0].2);
        let report = file.parent().unwrap().join("left_vs_right.html");
        assert!(std::fs::read_to_string(report)
            .unwrap()
            .contains("only.txt"));
    }

    #[test]
    fn a_saved_sync_job_swaps_sides_and_asks_before_it_changes_anything() {
        let (dir, l, r) = folders();
        let mut d = SaveJob::new(template(&l, &r), "sync");
        d.kind = SaveKind::Sync;
        d.reverse = true;
        d.update = true;
        let file = dir.path().join("sync.oxjob");
        d.write(&file).unwrap();
        let job = JobFile::parse(&std::fs::read_to_string(&file).unwrap())
            .unwrap()
            .jobs[0]
            .clone();
        assert_eq!(job.kind, JobKind::Sync);
        assert!(job.apply);
        assert_eq!(job.source, Some(r.display().to_string()));
        assert_eq!(job.destination, Some(l.display().to_string()));
        assert_eq!(job.mode.as_deref(), Some("update"));
        assert!(job.report.is_none() && job.left.is_none() && !job.recursive);

        let fresh = RunJob::new(file.clone());
        assert!(
            fresh.needs_confirmation(),
            "a job that changes folders asks first"
        );
        // Without the user's agreement the job refuses and nothing changes.
        let refused = run(&file, false);
        assert_eq!(refused.exit_code(), Some(2));
        assert!(refused.results()[0].2);
        assert_eq!(std::fs::read_to_string(l.join("a.txt")).unwrap(), "one\n");
    }

    #[test]
    fn a_sync_plan_job_writes_a_plan_and_changes_nothing() {
        let (dir, l, r) = folders();
        let mut d = SaveJob::new(template(&l, &r), "plan");
        d.kind = SaveKind::SyncPlan;
        d.suggest_output(SaveKind::Compare);
        assert_eq!(d.output, "plan-plan.json");
        let file = dir.path().join("plan.oxjob");
        d.write(&file).unwrap();
        let run = RunJob::new(file.clone());
        assert!(!run.needs_confirmation());
        let run = self::run(&file, false);
        assert!(!run.results()[0].2, "{:?}", run.results());
        assert!(dir.path().join("plan-plan.json").is_file());
        assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "two\n");
        assert!(!r.join("only.txt").exists());
    }

    #[test]
    fn a_job_file_that_cannot_be_read_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.oxjob");
        std::fs::write(&file, r#"{"version": 99, "jobs": []}"#).unwrap();
        let run = RunJob::new(file);
        assert!(!run.needs_confirmation());
        assert_eq!(run.exit_code(), Some(2));
        assert!(matches!(&run.state, RunState::Failed(e) if e.contains("newer")));
    }

    #[test]
    fn a_three_way_compare_saves_only_as_a_compare_job() {
        let (_dir, l, r) = folders();
        let mut t = template(&l, &r);
        t.base = Some(l.display().to_string());
        let mut d = SaveJob::new(t, "three");
        assert!(!d.two_sides());
        d.kind = SaveKind::Sync;
        assert!(
            d.job(Path::new("x.oxjob")).is_err(),
            "a sync needs a source"
        );
    }
}
