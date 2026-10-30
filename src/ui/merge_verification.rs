//! Read-only folder merge verification; the existing merge confirmation remains the write path.
use super::analysis::Task;
use crate::{
    core::{
        analysis,
        diff::DiffOptions,
        folder::DirItem,
        merge_validation::{self as mv, MergePlan, MergeValidationReport, ValidationStatus},
    },
    i18n::tr,
};
use std::path::PathBuf;
#[derive(Default)]
pub struct FolderVerification {
    plan: Option<Box<MergePlan>>,
    report: Option<Box<MergeValidationReport>>,
    planning: Option<Task<MergePlan>>,
    validating: Option<Task<MergeValidationReport>>,
    error: String,
    filter: String,
}
impl FolderVerification {
    pub fn clear_validation(&mut self) {
        self.report = None;
        self.planning = None;
        self.validating = None;
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        items: &[DirItem],
        roots: &[PathBuf],
        options: &DiffOptions,
        busy: bool,
        filter: &str,
    ) -> Option<String> {
        if self.filter != filter {
            self.plan = None;
            self.clear_validation();
            self.filter = filter.into();
        }
        if roots.len() != 3 {
            return None;
        }
        if self.plan.as_ref().is_some_and(|plan| {
            plan.rules != *options
                || plan
                    .inputs
                    .iter()
                    .map(|(p, _)| p)
                    .ne(roots.iter().map(|p| p.to_str().unwrap_or_default()))
        }) {
            self.plan = None;
            self.clear_validation();
        }
        if let Some(task) = &mut self.planning {
            if let Some(result) = task.poll() {
                match result {
                    Ok(plan) => self.plan = Some(Box::new(plan)),
                    Err(e) => self.error = e.to_string(),
                };
                self.planning = None;
            }
        }
        if let Some(task) = &mut self.validating {
            if let Some(result) = task.poll() {
                match result {
                    Ok(report) => self.report = Some(Box::new(report)),
                    Err(e) => self.error = e.to_string(),
                };
                self.validating = None;
            }
        }
        let mut navigate = None;
        egui::CollapsingHeader::new(tr("analysis.merge_verification"))
            .id_salt("folder_merge_verification")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("analysis.preview_merge")))
                        .clicked()
                    {
                        self.plan = None;
                        self.report = None;
                        self.error.clear();
                        let items = items.to_vec();
                        let roots = roots.to_vec();
                        let options = options.clone();
                        self.planning = Some(Task::start(move |cancel, progress| {
                            analysis::check_cancel(cancel)?;
                            let plan =
                                mv::folder_plan_control(&items, &roots, &options, cancel, progress);
                            analysis::check_cancel(cancel)?;
                            Ok(plan)
                        }));
                    }
                    if ui
                        .add_enabled(
                            !busy && self.plan.is_some(),
                            egui::Button::new(tr("analysis.validate_result")),
                        )
                        .clicked()
                    {
                        let plan = self.plan.as_ref().unwrap().clone();
                        let root = roots[1].clone();
                        self.report = None;
                        self.error.clear();
                        self.validating = Some(Task::start(move |cancel, progress| {
                            analysis::check_cancel(cancel)?;
                            let report =
                                mv::validate_folder_control(&plan, &root, cancel, progress)?;
                            analysis::check_cancel(cancel)?;
                            Ok(report)
                        }));
                    }
                    if ui
                        .add_enabled(
                            self.plan.is_some(),
                            egui::Button::new(tr("analysis.export")),
                        )
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("folder-merge-plan.json")
                            .save_file()
                        {
                            let result = match (&self.report, &self.plan) {
                                (Some(report), _) => analysis::report(report, &path, roots),
                                (_, Some(plan)) => analysis::report(plan, &path, roots),
                                _ => Ok(()),
                            };
                            if let Err(e) = result {
                                self.error = e.to_string();
                            }
                        }
                    }
                });
                ui.small(tr("analysis.mechanical_only"));
                if let Some(task) = &self.planning {
                    task.status(ui);
                }
                if let Some(task) = &self.validating {
                    task.status(ui);
                }
                if !self.error.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
                if let Some(plan) = &self.plan {
                    let status = self
                        .report
                        .as_ref()
                        .map_or(ValidationStatus::NotRun, |r| r.status.clone());
                    ui.label(tr(match status {
                        ValidationStatus::NotRun => "analysis.not_run",
                        ValidationStatus::Passed => "analysis.passed",
                        ValidationStatus::Warning => "analysis.warning",
                        ValidationStatus::Failed => "analysis.failed",
                    }));
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for action in &plan.actions {
                                if ui
                                    .selectable_label(
                                        false,
                                        format!("{}: {}", action.operation, action.path),
                                    )
                                    .clicked()
                                {
                                    navigate = Some(action.path.clone());
                                }
                            }
                            for f in self.report.as_ref().map_or(&plan.findings, |r| &r.findings) {
                                if ui
                                    .selectable_label(
                                        false,
                                        format!("{:?}: {}", f.severity, f.message),
                                    )
                                    .clicked()
                                {
                                    navigate = f.hunk_id.as_ref().and_then(|id| {
                                        plan.actions
                                            .iter()
                                            .find(|a| &a.id == id)
                                            .map(|a| a.path.clone())
                                    });
                                }
                            }
                        });
                }
            });
        navigate
    }
}
