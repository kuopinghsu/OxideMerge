//! Folder synchronization: a read-only preview of the plan, confirmation, applying it with
//! progress and a way to stop, and how it ended. Also offers to undo a synchronization that
//! did not finish.

use crate::core::folder::DirItem;
use crate::core::folder_sync::{
    self, Action, Kind, Precondition, RollbackOutcome, Scope, SyncOperation, SyncOutcome, SyncPlan,
    SyncPolicy, Unfinished,
};
use crate::core::report::fmt_size;
use crate::i18n::{tr, trf};
use crate::ui::{theme, widgets};
use egui::{vec2, Color32, Key, Margin, RichText, Ui};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

enum State {
    /// Earlier synchronizations of these folders to deal with first.
    Unfinished(Recovery),
    Planning {
        job: JoinHandle<Result<SyncPlan, String>>,
        stop: Arc<AtomicBool>,
    },
    Failed(String),
    Preview(Arc<SyncPlan>),
    /// Asked to apply; `second` once a large deletion asks again, `sure` when its box is ticked.
    Confirm {
        plan: Arc<SyncPlan>,
        second: bool,
        sure: bool,
    },
    Applying {
        total: usize,
        plan: Arc<SyncPlan>,
        job: JoinHandle<SyncOutcome>,
        stop: Arc<AtomicBool>,
        done: Arc<AtomicUsize>,
    },
    /// `rescan` when the folders may have changed since they were compared.
    Done {
        outcome: SyncOutcome,
        rescan: bool,
    },
}

pub struct SyncDialog {
    roots: Vec<PathBuf>,
    names: Vec<String>,
    items: Arc<Vec<DirItem>>,
    scope: Scope,
    /// The sides that may be changed, and so be the destination.
    writable: [bool; 2],
    policy: SyncPolicy,
    state: State,
    /// The kind of operation listed; all when `None`.
    shown: Option<Action>,
    /// Lists the renames instead: copies paired with a deletion of the same contents.
    renames: bool,
    /// The operation whose details are shown.
    picked: Option<usize>,
    message: Option<String>,
    id: egui::Id,
}

impl SyncDialog {
    pub fn new(
        items: Vec<DirItem>,
        roots: Vec<PathBuf>,
        names: Vec<String>,
        scope: Scope,
        writable: [bool; 2],
        id: egui::Id,
    ) -> SyncDialog {
        let policy = SyncPolicy::ALL
            .into_iter()
            .find(|p| writable[p.destination()]);
        let mut d = SyncDialog {
            roots,
            names,
            items: Arc::new(items),
            scope,
            writable,
            policy: policy.unwrap_or(SyncPolicy::ALL[0]),
            state: State::Failed(String::new()),
            shown: None,
            renames: false,
            picked: None,
            message: None,
            id,
        };
        let pending = Recovery::for_roots(&d.roots);
        if policy.is_none() {
            d.state = State::Failed(tr("folder_sync.err_read_only").to_string());
        } else if pending.list.is_empty() {
            d.replan();
        } else {
            d.state = State::Unfinished(pending);
        }
        d
    }

    fn replan(&mut self) {
        if let State::Planning { stop, .. } = &self.state {
            stop.store(true, Ordering::Relaxed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (items, roots, policy, scope) = (
            self.items.clone(),
            self.roots.clone(),
            self.policy,
            self.scope.clone(),
        );
        let flag = stop.clone();
        let job =
            std::thread::spawn(move || folder_sync::plan(&items, &roots, policy, scope, &flag));
        self.state = State::Planning { job, stop };
        self.picked = None;
    }

    /// Whether it is changing files: applying, or undoing an earlier synchronization.
    pub fn busy(&self) -> bool {
        match &self.state {
            State::Applying { .. } => true,
            State::Unfinished(r) => r.busy(),
            _ => false,
        }
    }

    /// Whether the preview is still being worked out.
    pub fn planning(&self) -> bool {
        matches!(self.state, State::Planning { .. })
    }

    /// Stops planning or applying, as when the view closes; applying then undoes its changes.
    pub fn stop(&self) {
        match &self.state {
            State::Planning { stop, .. } | State::Applying { stop, .. } => {
                stop.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let finished = match &self.state {
            State::Planning { job, .. } => job.is_finished(),
            State::Applying { job, .. } => job.is_finished(),
            _ => return,
        };
        if !finished {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Failed(String::new()));
        self.state = match state {
            State::Planning { job, .. } => match job.join() {
                Ok(Ok(plan)) => State::Preview(Arc::new(plan)),
                Ok(Err(e)) => State::Failed(e),
                Err(_) => State::Failed(tr("status.error").to_string()),
            },
            State::Applying { job, .. } => match job.join() {
                Ok(outcome) => State::Done {
                    rescan: !matches!(outcome, SyncOutcome::NotApplied(..)),
                    outcome,
                },
                // What it changed before it failed is not known, so the folders are read again.
                Err(_) => State::Done {
                    outcome: folder_sync::abandoned(
                        folder_sync::journal_root().as_deref(),
                        &self.roots,
                        tr("status.error").to_string(),
                    ),
                    rescan: true,
                },
            },
            other => other,
        };
    }

    fn start_apply(&mut self, plan: Arc<SyncPlan>) {
        let (stop, done) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
        );
        let roots = self.roots.clone();
        let (flag, count) = (stop.clone(), done.clone());
        let applied = plan.clone();
        let job = std::thread::spawn(move || match folder_sync::journal_root() {
            Some(journal) => folder_sync::apply(&applied, &roots, &journal, &flag, &count),
            None => SyncOutcome::NotApplied(
                crate::core::problem::Problem::WriteFailure,
                vec![tr("folder_sync.journal").to_string()],
            ),
        });
        self.state = State::Applying {
            total: plan.changes(),
            plan,
            job,
            stop,
            done,
        };
    }

    fn export(&mut self, plan: &SyncPlan) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("sync-plan.json")
            .save_file()
        else {
            return;
        };
        let written = plan
            .to_json()
            .and_then(|json| crate::core::fsutil::write_atomic_report(&path, json.as_bytes()));
        self.message = Some(match written {
            Ok(()) => trf("folder_sync.saved", &[("path", path.display().to_string())]),
            Err(e) => e.to_string(),
        });
    }

    /// Draws the dialog; `Some(rescan)` once it is closed, `rescan` when folders changed.
    pub fn show(&mut self, ui: &Ui) -> Option<bool> {
        self.poll(ui.ctx());
        let mut close = None;
        let mut action: Option<Step> = None;
        egui::Modal::new(self.id.with("sync")).show(ui.ctx(), |ui| {
            ui.set_min_width(560.0);
            ui.set_max_width(760.0);
            let pal = widgets::pal(ui);
            ui.heading(tr("folder_sync.title"));
            self.roots_line(ui);
            ui.add_space(6.0);
            match &mut self.state {
                State::Unfinished(recovery) => {
                    let dealt_with = recovery.ui(ui);
                    // Undoing changed files the compare listed, so its results are out of date.
                    if dealt_with && recovery.changed {
                        ui.label(RichText::new(tr("folder_sync.compare_again")).strong());
                    } else if dealt_with {
                        action = Some(Step::Replan);
                    }
                    let busy = recovery.busy();
                    widgets::dialog_footer(ui, |ui| {
                        let b = ui.add_enabled_ui(!busy, |ui| {
                            widgets::secondary_button(ui, tr("btn.close"))
                        });
                        if b.inner.clicked() {
                            close = Some(recovery.changed);
                        }
                    });
                }
                State::Planning { .. } => {
                    self.policy_picker(ui, &mut action, true);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("folder_sync.planning"));
                    });
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close = Some(false);
                        }
                    });
                }
                State::Failed(e) => {
                    let e = e.clone();
                    self.policy_picker(ui, &mut action, true);
                    ui.label(RichText::new(e).color(pal.danger));
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::secondary_button(ui, tr("btn.close")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close = Some(false);
                        }
                    });
                }
                State::Preview(plan) => {
                    let plan = plan.clone();
                    self.policy_picker(ui, &mut action, true);
                    self.preview(ui, &plan);
                    widgets::dialog_footer(ui, |ui| {
                        let apply = ui.add_enabled_ui(plan.changes() > 0, |ui| {
                            apply_button(ui, tr("folder_sync.apply"), plan.deletions() > 0)
                        });
                        if apply.inner.clicked() {
                            action = Some(Step::Confirm(plan.clone()));
                        }
                        if widgets::secondary_button(ui, tr("btn.close")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close = Some(false);
                        }
                        if widgets::secondary_button(ui, tr("folder_sync.export")).clicked() {
                            action = Some(Step::Export(plan.clone()));
                        }
                    });
                }
                State::Confirm { plan, second, sure } => {
                    let destination = &self.names[plan.policy.destination()];
                    let e = &plan.estimate;
                    if *second {
                        ui.label(
                            RichText::new(trf(
                                "folder_sync.confirm_large",
                                &[("n", e.deletes.to_string()), ("root", destination.clone())],
                            ))
                            .color(pal.danger)
                            .strong(),
                        );
                        ui.checkbox(
                            sure,
                            trf("folder_sync.confirm_tick", &[("n", e.deletes.to_string())]),
                        );
                    } else {
                        ui.label(
                            RichText::new(trf(
                                "folder_sync.confirm",
                                &[
                                    ("n", plan.changes().to_string()),
                                    ("root", destination.clone()),
                                ],
                            ))
                            .strong(),
                        );
                        ui.label(trf(
                            "folder_sync.confirm_detail",
                            &[
                                ("replaces", e.replaces.to_string()),
                                ("deletes", e.deletes.to_string()),
                            ],
                        ));
                    }
                    let ready = !*second || *sure;
                    let mut go = false;
                    let mut back = false;
                    widgets::dialog_footer(ui, |ui| {
                        let apply = ui.add_enabled_ui(ready, |ui| {
                            apply_button(ui, tr("folder_sync.apply_now"), e.deletes > 0)
                        });
                        go = apply.inner.clicked();
                        back = widgets::secondary_button(ui, tr("btn.cancel")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape));
                    });
                    if go && !*second && plan.large_deletion() {
                        *second = true;
                    } else if go {
                        action = Some(Step::Apply(plan.clone()));
                    } else if back {
                        action = Some(Step::Back(plan.clone()));
                    }
                }
                State::Applying {
                    total,
                    plan,
                    stop,
                    done,
                    ..
                } => {
                    let n = done.load(Ordering::Relaxed);
                    let stopping = stop.load(Ordering::Relaxed);
                    ui.label(trf(
                        "folder_sync.applying",
                        &[("done", n.to_string()), ("total", total.to_string())],
                    ));
                    // Operations are applied in plan order, so the next one is under way; after
                    // the last, the copies are checked once more.
                    let next = plan.operations.iter().filter(|o| o.action.changes()).nth(n);
                    if stopping || next.is_none() {
                        let key = if stopping {
                            "folder_sync.undoing"
                        } else {
                            "folder_sync.verifying"
                        };
                        ui.label(
                            RichText::new(tr(key))
                                .size(theme::size::DENSE)
                                .color(pal.text_dim),
                        );
                    } else if let Some(o) = next {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}  {}", tr(o.action.key()), o.path))
                                    .monospace()
                                    .size(theme::size::DENSE)
                                    .color(pal.text_dim),
                            )
                            .truncate(),
                        );
                    }
                    ui.add(
                        egui::ProgressBar::new(n as f32 / (*total).max(1) as f32)
                            .fill(pal.accent)
                            .desired_height(6.0),
                    );
                    widgets::dialog_footer(ui, |ui| {
                        let b = ui.add_enabled_ui(!stopping, |ui| {
                            widgets::secondary_button(ui, tr("btn.stop"))
                        });
                        if b.inner.clicked() {
                            stop.store(true, Ordering::Relaxed);
                        }
                    });
                }
                State::Done { outcome, rescan } => {
                    outcome_ui(ui, outcome);
                    let changed = *rescan;
                    widgets::dialog_footer(ui, |ui| {
                        if widgets::primary_button(ui, tr("btn.close")).clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close = Some(changed);
                        }
                    });
                }
            }
            if let Some(m) = &self.message {
                ui.label(
                    RichText::new(m)
                        .size(theme::size::SMALL)
                        .color(pal.text_dim),
                );
            }
        });
        match action {
            Some(Step::Replan) => self.replan(),
            Some(Step::Confirm(plan)) => {
                self.state = State::Confirm {
                    plan,
                    second: false,
                    sure: false,
                }
            }
            Some(Step::Back(plan)) => self.state = State::Preview(plan),
            Some(Step::Apply(plan)) => self.start_apply(plan),
            Some(Step::Export(plan)) => self.export(&plan),
            None => {}
        }
        if close.is_some() {
            self.stop();
        }
        close
    }

    fn roots_line(&self, ui: &mut Ui) {
        let pal = widgets::pal(ui);
        for (s, name) in self.names.iter().enumerate() {
            ui.horizontal(|ui| {
                widgets::side_chip(ui, s, 2);
                ui.add(
                    egui::Label::new(
                        RichText::new(name)
                            .size(theme::size::DENSE)
                            .color(pal.text_dim),
                    )
                    .truncate(),
                );
            });
        }
    }

    fn policy_picker(&mut self, ui: &mut Ui, action: &mut Option<Step>, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                for p in SyncPolicy::ALL {
                    let allowed = self.writable[p.destination()];
                    if ui
                        .add_enabled(
                            allowed,
                            egui::Button::selectable(self.policy == p, tr(p.key())),
                        )
                        .clicked()
                        && self.policy != p
                    {
                        self.policy = p;
                        *action = Some(Step::Replan);
                    }
                }
            });
            if cfg!(unix)
                && ui
                    .checkbox(&mut self.scope.copy_links, tr("folder_sync.copy_links"))
                    .on_hover_text(tr("folder_sync.copy_links_tip"))
                    .changed()
            {
                *action = Some(Step::Replan);
            }
        });
        let pal = widgets::pal(ui);
        egui::Frame::new()
            .fill(pal.surface_alt)
            .corner_radius(theme::radius::MD)
            .inner_margin(Margin::symmetric(theme::space::CONTROL, theme::space::SM))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(tr("folder_sync.preview_only"))
                        .color(pal.accent)
                        .strong(),
                );
            });
    }

    fn preview(&mut self, ui: &mut Ui, plan: &SyncPlan) {
        let pal = widgets::pal(ui);
        let e = &plan.estimate;
        if plan.changes() == 0 {
            ui.label(tr("folder_sync.nothing"));
        } else {
            ui.label(trf(
                "folder_sync.summary",
                &[
                    ("dirs", e.create_dirs.to_string()),
                    ("copies", e.copies.to_string()),
                    ("replaces", e.replaces.to_string()),
                    ("deletes", e.deletes.to_string()),
                    ("bytes", fmt_size(Some(e.bytes))),
                ],
            ));
            if e.renames > 0 {
                ui.label(trf(
                    "folder_sync.summary_renames",
                    &[("n", e.renames.to_string())],
                ));
            }
        }
        for w in &plan.warnings {
            ui.label(RichText::new(w.text()).color(pal.warning));
        }
        if plan.operations.is_empty() {
            return;
        }
        // a new plan may have nothing of the group shown
        if self.renames && e.renames == 0 {
            self.renames = false;
        }
        if self.shown.is_some_and(|a| plan.count(a) == 0) {
            self.shown = None;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(
                    self.shown.is_none() && !self.renames,
                    format!("{} {}", tr("folder_sync.all"), plan.operations.len()),
                )
                .clicked()
            {
                (self.shown, self.renames) = (None, false);
            }
            for a in Action::ALL {
                let n = plan.count(a);
                if n > 0
                    && ui
                        .selectable_label(
                            self.shown == Some(a) && !self.renames,
                            format!("{} {n}", tr(a.key())),
                        )
                        .clicked()
                {
                    (self.shown, self.renames) = (Some(a), false);
                }
            }
            if e.renames > 0
                && ui
                    .selectable_label(
                        self.renames,
                        format!("{} {}", tr("folder_sync.action_rename"), e.renames),
                    )
                    .on_hover_text(tr("tip.sync_renames"))
                    .clicked()
            {
                (self.shown, self.renames) = (None, true);
            }
        });
        let listed: Vec<usize> = (0..plan.operations.len())
            .filter(|&i| {
                let o = &plan.operations[i];
                if self.renames {
                    o.action == Action::Copy && o.renamed.is_some()
                } else {
                    self.shown.is_none_or(|a| o.action == a)
                }
            })
            .collect();
        if self.picked.is_some_and(|i| !listed.contains(&i)) {
            self.picked = None;
        }
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        egui::Frame::new()
            .fill(pal.surface_alt)
            .corner_radius(theme::radius::MD)
            .inner_margin(Margin::same(theme::space::SM))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show_rows(ui, row_h, listed.len(), |ui, range| {
                        for &i in &listed[range] {
                            let o = &plan.operations[i];
                            let r = ui.horizontal(|ui| {
                                ui.add_sized(
                                    vec2(110.0, row_h),
                                    egui::Label::new(
                                        RichText::new(if self.renames {
                                            tr("folder_sync.action_rename")
                                        } else {
                                            tr(o.action.key())
                                        })
                                        .color(action_color(o.action, &pal)),
                                    )
                                    .truncate(),
                                );
                                let size = if o.bytes > 0 {
                                    fmt_size(Some(o.bytes))
                                } else {
                                    String::new()
                                };
                                ui.add_sized(
                                    vec2(70.0, row_h),
                                    egui::Label::new(RichText::new(size).color(pal.text_dim)),
                                );
                                let path = match (o.action, &o.renamed) {
                                    (Action::Copy, Some(from)) => format!("{from} → {}", o.path),
                                    (_, Some(to)) => format!("{} → {to}", o.path),
                                    _ => o.path.clone(),
                                };
                                ui.selectable_label(
                                    self.picked == Some(i),
                                    RichText::new(path).monospace(),
                                )
                                .on_hover_text(tr(o.reason.key()))
                            });
                            if r.inner.clicked() {
                                self.picked = Some(i);
                            }
                        }
                    });
            });
        if let Some(o) = self.picked.and_then(|i| plan.operations.get(i)) {
            self.detail(ui, plan, o);
        }
    }

    fn detail(&self, ui: &mut Ui, plan: &SyncPlan, o: &SyncOperation) {
        let pal = widgets::pal(ui);
        ui.add_space(4.0);
        ui.label(RichText::new(&o.path).monospace().strong());
        ui.label(tr(o.reason.key()));
        if let Some(other) = &o.renamed {
            let (from, to) = if o.action == Action::Copy {
                (other.as_str(), o.path.as_str())
            } else {
                (o.path.as_str(), other.as_str())
            };
            ui.label(trf(
                "folder_sync.renamed_detail",
                &[("from", from.to_string()), ("to", to.to_string())],
            ));
        }
        let risk = if o.action.destructive() {
            (tr("folder_sync.risk_destructive"), pal.danger)
        } else if o.action.changes() {
            (tr("folder_sync.risk_adds"), pal.text_dim)
        } else {
            (tr("folder_sync.risk_none"), pal.text_dim)
        };
        ui.label(RichText::new(risk.0).color(risk.1));
        let (s, d) = (plan.policy.source(), plan.policy.destination());
        egui::Grid::new(self.id.with("sync_detail")).show(ui, |ui| {
            ui.label(trf(
                "folder_sync.source",
                &[("side", self.names[s].clone())],
            ));
            ui.label(describe(&o.source));
            ui.end_row();
            ui.label(trf(
                "folder_sync.destination",
                &[("side", self.names[d].clone())],
            ));
            ui.label(describe(&o.destination));
            ui.end_row();
        });
    }
}

enum Step {
    Replan,
    Confirm(Arc<SyncPlan>),
    Back(Arc<SyncPlan>),
    Apply(Arc<SyncPlan>),
    Export(Arc<SyncPlan>),
}

/// A filled button, red when it deletes, so applying never looks like previewing.
fn apply_button(ui: &mut Ui, label: &str, deletes: bool) -> egui::Response {
    let pal = widgets::pal(ui);
    let fill = if deletes { pal.danger } else { pal.accent };
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(fill)
            .min_size(vec2(120.0, 32.0)),
    )
}

fn action_color(a: Action, pal: &theme::Palette) -> Color32 {
    match a {
        Action::Delete | Action::RemoveDir => pal.danger,
        Action::Replace | Action::Conflict => pal.warning,
        Action::Skip => pal.text_dim,
        Action::CreateDir | Action::Copy => pal.success,
    }
}

fn describe(p: &Precondition) -> String {
    let what = match p.kind {
        Kind::Missing => tr("folder_sync.kind_missing").to_string(),
        Kind::File => trf("folder_sync.kind_file", &[("size", fmt_size(p.size))]),
        Kind::Dir => tr("folder_sync.kind_dir").to_string(),
        Kind::Link => tr("folder_sync.kind_link").to_string(),
        Kind::Other => tr("folder_sync.kind_other").to_string(),
        Kind::Unreadable => tr("folder_sync.kind_unreadable").to_string(),
    };
    match &p.sha256 {
        Some(h) => format!("{what} · SHA-256 {}", &h[..h.len().min(12)]),
        None => what,
    }
}

fn outcome_ui(ui: &mut Ui, outcome: &SyncOutcome) {
    let pal = widgets::pal(ui);
    match outcome {
        SyncOutcome::Applied { changes, kept } => {
            ui.label(
                RichText::new(trf("folder_sync.done", &[("n", changes.to_string())]))
                    .color(pal.success),
            );
            if !kept.is_empty() {
                ui.label(trf("folder_sync.kept", &[("n", kept.len().to_string())]));
                list(ui, kept.iter().map(String::as_str));
            }
        }
        SyncOutcome::NotApplied(_, errors) => {
            ui.label(
                RichText::new(tr("folder_sync.not_applied"))
                    .color(pal.warning)
                    .strong(),
            );
            list(ui, errors.iter().map(String::as_str));
        }
        SyncOutcome::Stopped {
            reason, rollback, ..
        } if rollback.complete() => {
            let text = match reason {
                None => trf(
                    "folder_sync.stopped_undone",
                    &[("n", rollback.restored.to_string())],
                ),
                Some(r) => trf("folder_sync.failed_undone", &[("reason", r.clone())]),
            };
            ui.label(RichText::new(text).color(pal.warning));
        }
        SyncOutcome::Stopped {
            reason, rollback, ..
        } => {
            ui.label(
                RichText::new(tr("folder_sync.recovery_required"))
                    .color(pal.danger)
                    .strong(),
            );
            if let Some(r) = reason {
                ui.label(r);
            }
            let path = rollback
                .journal
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            ui.label(trf("folder_sync.recovery_detail", &[("path", path)]));
            let failed: Vec<String> = rollback
                .failed
                .iter()
                .map(|(p, e)| format!("{p}: {e}"))
                .collect();
            list(ui, failed.iter().map(String::as_str));
            ui.label(tr("folder_sync.recovery_next"));
        }
    }
}

fn list<'a>(ui: &mut Ui, lines: impl Iterator<Item = &'a str>) {
    let pal = widgets::pal(ui);
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for l in lines.take(200) {
                ui.label(RichText::new(l).monospace().color(pal.text_dim));
            }
        });
}

/// Synchronizations that did not finish, offered to be undone or forgotten.
pub struct Recovery {
    list: Vec<Unfinished>,
    /// What happened to the last one dealt with.
    result: Option<(String, bool)>,
    /// Whether folders were changed by undoing.
    pub changed: bool,
    job: Option<Job>,
}

/// One being undone, or forgotten when `undo` is off, on another thread.
struct Job {
    at: usize,
    item: Unfinished,
    undo: bool,
    handle: JoinHandle<Result<RollbackOutcome, String>>,
}

impl Recovery {
    /// Those under the settings folder, all of them.
    pub fn all() -> Recovery {
        let list = folder_sync::journal_root()
            .map(|r| folder_sync::unfinished(&r))
            .unwrap_or_default();
        Recovery {
            list,
            result: None,
            changed: false,
            job: None,
        }
    }

    /// Those that changed one of `roots`.
    fn for_roots(roots: &[PathBuf]) -> Recovery {
        let same = |a: &Path, b: &Path| {
            std::fs::canonicalize(a)
                .ok()
                .is_some_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
        };
        let mut r = Recovery::all();
        r.list.retain(|u| {
            u.roots
                .iter()
                .any(|j| roots.iter().any(|root| same(j, root)))
        });
        r
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty() && self.result.is_none()
    }

    /// Whether one is being undone or forgotten now.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn start(&mut self, at: usize, undo: bool) {
        let item = self.list.remove(at);
        let u = item.clone();
        let handle = std::thread::spawn(move || {
            if undo {
                Ok(folder_sync::recover(&u))
            } else {
                folder_sync::discard(&u)
                    .map(|()| RollbackOutcome::default())
                    .map_err(|e| e.to_string())
            }
        });
        self.result = None;
        self.job = Some(Job {
            at,
            item,
            undo,
            handle,
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = self.job.take_if(|j| j.handle.is_finished()) else {
            if self.job.is_some() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        };
        let result = job
            .handle
            .join()
            .unwrap_or_else(|_| Err(tr("status.error").to_string()));
        let mut put_back = || {
            self.list
                .insert(job.at.min(self.list.len()), job.item.clone())
        };
        self.result = Some(match result {
            Ok(r) if job.undo => {
                self.changed |= r.restored > 0;
                if r.complete() {
                    (tr("folder_sync.recovered").to_string(), true)
                } else {
                    put_back();
                    let path = r
                        .journal
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let mut text = format!(
                        "{} {}",
                        tr("folder_sync.recovery_required"),
                        trf("folder_sync.recovery_detail", &[("path", path)])
                    );
                    for (p, e) in &r.failed {
                        text.push_str(&format!("\n{p}: {e}"));
                    }
                    text.push_str(&format!("\n{}", tr("folder_sync.recovery_next")));
                    (text, false)
                }
            }
            Ok(_) => (tr("folder_sync.discarded").to_string(), true),
            Err(e) => {
                put_back();
                (e, false)
            }
        });
    }

    /// Lists them with their choices; whether all are dealt with.
    pub fn ui(&mut self, ui: &mut Ui) -> bool {
        self.poll(ui.ctx());
        let pal = widgets::pal(ui);
        if let Some((text, ok)) = &self.result {
            ui.label(RichText::new(text).color(if *ok { pal.success } else { pal.danger }));
        }
        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr(if job.undo {
                    "folder_sync.undoing"
                } else {
                    "folder_sync.forgetting"
                }));
            });
            return false;
        }
        let mut chosen: Option<(usize, bool)> = None;
        for (i, u) in self.list.iter().enumerate() {
            ui.separator();
            ui.label(RichText::new(tr("folder_sync.unfinished_title")).strong());
            ui.label(trf(
                "folder_sync.unfinished",
                &[
                    ("left", u.roots[0].display().to_string()),
                    ("right", u.roots[1].display().to_string()),
                    ("n", u.changes.to_string()),
                    ("root", u.roots[u.destination].display().to_string()),
                ],
            ));
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, tr("folder_sync.recover")).clicked() {
                    chosen = Some((i, true));
                }
                if widgets::secondary_button(ui, tr("folder_sync.discard")).clicked() {
                    chosen = Some((i, false));
                }
            });
        }
        if let Some((i, undo)) = chosen {
            self.start(i, undo);
            return false;
        }
        self.list.is_empty()
    }

    /// Shown by itself, as at startup; whether to keep showing it.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("sync_recovery")).show(ctx, |ui| {
            ui.set_min_width(480.0);
            ui.set_max_width(640.0);
            ui.heading(tr("folder_sync.title"));
            self.ui(ui);
            widgets::dialog_footer(ui, |ui| {
                let label = if self.list.is_empty() {
                    tr("btn.close")
                } else {
                    tr("folder_sync.later")
                };
                let busy = self.busy();
                let b = ui.add_enabled_ui(!busy, |ui| widgets::secondary_button(ui, label));
                if b.inner.clicked() || (!busy && ui.input(|i| i.key_pressed(Key::Escape))) {
                    keep = false;
                }
            });
        });
        keep
    }
}
