use super::analysis::Task;
use crate::{
    core::{
        analysis,
        baseline::{self, BaselineSpec, Capture, ReleaseSummary},
        impact,
        text::{self, TextDoc},
    },
    i18n::tr,
};
use std::collections::BTreeSet;
pub enum CapturedComparison {
    Text(Vec<TextDoc>),
    Source { docs: Vec<TextDoc>, line: usize },
    Bytes(Vec<(String, Vec<u8>)>),
}
pub struct ResultSet {
    pub left: Capture,
    pub right: Capture,
    pub summary: ReleaseSummary,
    pub impact: Option<impact::ImpactReport>,
}
#[derive(Default)]
pub struct BaselinePanel {
    specs: [String; 2],
    pub open: bool,
    important: String,
    impact_enabled: bool,
    job: Option<Task<ResultSet>>,
    result: Option<std::sync::Arc<ResultSet>>,
    export_job: Option<Task<()>>,
    error: String,
    dismissed: BTreeSet<String>,
    changes_only: bool,
    policy_id: String,
    index: std::sync::Arc<std::sync::Mutex<impact::Index>>,
    initialized: bool,
    scope: String,
    id: Option<egui::Id>,
}
impl BaselinePanel {
    pub fn invalidate(&mut self) {
        self.job = None;
        self.result = None;
        self.error.clear();
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        roots: &[std::path::PathBuf],
        options: &crate::core::diff::DiffOptions,
        filter: &str,
    ) -> Option<CapturedComparison> {
        if !self.initialized {
            for (i, root) in roots.iter().take(2).enumerate() {
                self.specs[i] = format!("folder:{}", root.display());
            }
            self.scope = filter.into();
            self.initialized = true;
        }
        let policy = analysis::identity(&(options, &self.scope, &options.unpackers));
        if self.policy_id != policy {
            self.invalidate();
            self.policy_id = policy;
        }
        if let Some(task) = &mut self.export_job {
            if let Some(result) = task.poll() {
                if let Err(e) = result {
                    self.error = e.to_string();
                }
                self.export_job = None;
            }
        }
        if let Some(task) = &self.export_job {
            task.status(ui);
        }
        let mut open_files = None;
        let id = *self
            .id
            .get_or_insert_with(|| egui::Id::new(("baseline", std::time::Instant::now())));
        let response = egui::CollapsingHeader::new(tr("baseline.title"))
            .id_salt(id)
            .default_open(self.open)
            .show(ui, |ui| {
                let mut changed = false;
                ui.horizontal(|ui| { ui.label(tr("baseline.filter")); changed |= ui.add(egui::TextEdit::singleline(&mut self.scope).char_limit(1024)).changed(); });
                ui.small(tr("baseline.syntax"));
                for (side, spec) in self.specs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        super::widgets::side_chip(ui, side, 2);
                        let label = tr(if side == 0 {
                            "baseline.left"
                        } else {
                            "baseline.right"
                        });
                        let response = ui.add(egui::TextEdit::singleline(spec).hint_text(label));
                        changed |= response.changed();
                        response.widget_info(|| {
                            let mut info =
                                egui::WidgetInfo::text_edit(true, spec.as_str(), spec.as_str());
                            info.label = Some(label.into());
                            info
                        });
                        if ui.button(tr("baseline.folder")).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                *spec = format!("folder:{}", p.display());
                                changed = true;
                            }
                        }
                        if ui.button(tr("baseline.snapshot")).clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("OxideMerge review", &["oxmreview"])
                                .pick_file()
                            {
                                *spec = format!("review:0:{}", p.display());
                                changed = true;
                            }
                        }
                    });
                }
                changed |= ui
                    .checkbox(&mut self.impact_enabled, tr("impact.title"))
                    .changed();
                ui.horizontal(|ui| {
                    ui.label(tr("baseline.important"));
                    changed |= ui.text_edit_singleline(&mut self.important).changed();
                });
                if changed {
                    self.job = None;
                    self.result = None;
                    self.error.clear();
                }
                if ui.button(tr("baseline.compare")).clicked() {
                    self.result = None;
                    self.error.clear();
                    let specs = [
                        BaselineSpec::parse(&self.specs[0]),
                        BaselineSpec::parse(&self.specs[1]),
                    ];
                    match specs {
                        [Ok(a), Ok(b)] => {
                            let options = options.clone();
                            let filter = self.scope.clone();
                            let important: Vec<_> = self
                                .important
                                .split(';')
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .collect();
                            let impact_enabled = self.impact_enabled;
                            let index = self.index.clone();
                            self.job = Some(Task::start(move |cancel, progress| {
                                let left =
                                    baseline::capture(&a, &options, &filter, cancel, progress)?;
                                let right =
                                    baseline::capture(&b, &options, &filter, cancel, progress)?;
                                let summary = baseline::compare(&left, &right, &important, cancel)?;
                                let impact = if impact_enabled {
                                    Some(
                                        index.lock().map_err(|_| anyhow::anyhow!("Impact index worker failed; reopen the comparison"))?.analyze(&summary, &right, cancel)?,
                                    )
                                } else {
                                    None
                                };
                                Ok(ResultSet {
                                    left,
                                    right,
                                    summary,
                                    impact,
                                })
                            }));
                        }
                        _ => self.error = tr("baseline.invalid").into(),
                    }
                }
                if let Some(job) = &mut self.job {
                    if let Some(result) = job.poll() {
                        match result {
                            Ok(r) => {
                                self.result = Some(std::sync::Arc::new(r));
                                self.dismissed.clear();
                            }
                            Err(e) => self.error = e.to_string(),
                        };
                        self.job = None;
                    }
                }
                if let Some(job) = &self.job {
                    job.status(ui);
                }
                if !self.error.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
                if let Some(result) = &self.result {
                    ui.small(tr("baseline.captured"));
                    for meta in [&result.summary.left, &result.summary.right] {
                        ui.monospace(format!(
                            "{} · {}",
                            meta.resolved_commit.as_deref().unwrap_or("SHA-256"),
                            meta.fingerprint
                        ));
                    }
                    ui.label(format!("{:?}", result.summary.counts));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.changes_only, tr("baseline.changes_only"));
                        if ui.button(tr("analysis.export")).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("release.json")
                                .save_file()
                            {
                                let inputs: Vec<_> = roots
                                    .iter()
                                    .cloned()
                                    .chain([
                                        result.summary.left.spec.path().to_owned(),
                                        result.summary.right.spec.path().to_owned(),
                                    ])
                                    .collect();
                                let result = result.clone();
                                let dismissed: Vec<_> = self.dismissed.iter().cloned().collect();
                                self.export_job = Some(Task::start(move |cancel, _| {
                                    analysis::check_cancel(cancel)?;
                                    if let Some(impact) = &result.impact {
                                        let mut report = serde_json::to_value(impact)?;
                                        report.as_object_mut().unwrap().insert(
                                            "dismissed_findings".into(),
                                            serde_json::to_value(dismissed)?,
                                        );
                                        analysis::check_cancel(cancel)?;
                                        analysis::report(&report, &path, &inputs)
                                    } else if ["html", "htm"]
                                        .contains(&text::extension(&path).as_str())
                                    {
                                        let html = baseline::html_report(
                                            &result.summary,
                                            &result.left,
                                            &result.right,
                                        )?;
                                        analysis::check_cancel(cancel)?;
                                        analysis::write_report_text(&path, &html, &inputs)
                                    } else {
                                        analysis::report(&result.summary, &path, &inputs)
                                    }
                                }));
                            }
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("release_rows")
                        .max_height(190.0)
                        .show(ui, |ui| {
                            for change in &result.summary.changes {
                                if self.changes_only
                                    && matches!(change.category, baseline::Category::Unchanged)
                                {
                                    continue;
                                }
                                if ui
                                    .selectable_label(
                                        false,
                                        format!(
                                            "{:?} {}{}{}",
                                            change.category,
                                            change.path,
                                            if change.ignored_by_text_rules {
                                                tr("baseline.ignored")
                                            } else {
                                                ""
                                            },
                                            if change.risk_flags.is_empty() {
                                                ""
                                            } else {
                                                " ⚠"
                                            }
                                        ),
                                    )
                                    .clicked()
                                {
                                    let mut docs = Vec::new();
                                    for (side, capture) in
                                        [&result.left, &result.right].into_iter().enumerate()
                                    {
                                        let key = if side == 0 {
                                            change.old_path.as_deref().unwrap_or(&change.path)
                                        } else {
                                            &change.path
                                        };
                                        let bytes = capture
                                            .files
                                            .get(key)
                                            .map(|f| f.bytes.as_slice())
                                            .unwrap_or(&[]);
                                        let mut doc = text::doc_from_bytes(None, bytes, None);
                                        doc.read_only = true;
                                        doc.description = Some(format!(
                                            "{key} · {}",
                                            &capture.metadata.fingerprint[..12]
                                        ));
                                        doc.syntax =
                                            Some(text::extension(std::path::Path::new(key)));
                                        docs.push(doc);
                                    }
                                    let bytes: Vec<_> = [&result.left, &result.right]
                                        .iter()
                                        .enumerate()
                                        .map(|(side, capture)| {
                                            let key = if side == 0 {
                                                change.old_path.as_deref().unwrap_or(&change.path)
                                            } else {
                                                &change.path
                                            };
                                            (
                                                key.to_string(),
                                                capture
                                                    .files
                                                    .get(key)
                                                    .map(|f| f.bytes.clone())
                                                    .unwrap_or_default(),
                                            )
                                        })
                                        .collect();
                                    open_files =
                                        Some(if bytes.iter().any(|(_, b)| text::looks_binary(b)) {
                                            CapturedComparison::Bytes(bytes)
                                        } else {
                                            CapturedComparison::Text(docs)
                                        });
                                }
                            }
                            for (side, path, error) in &result.summary.excluded {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    format!("{side}: {path}: {error}"),
                                );
                            }
                        });
                    if let Some(impact) = &result.impact {
                        ui.separator();
                        ui.label(tr("impact.title"));
                        ui.small(&impact.assessment);
                        egui::ScrollArea::vertical()
                            .id_salt("impact_rows")
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for finding in &impact.findings {
                                    if self.dismissed.contains(&finding.id) {
                                        continue;
                                    }
                                    ui.horizontal(|ui| {
                                        if ui
                                            .selectable_label(
                                                false,
                                                format!(
                                                    "{} · {}:{}",
                                                    tr("analysis.heuristic"),
                                                    finding.evidence.path,
                                                    finding.evidence.line
                                                ),
                                            )
                                            .clicked()
                                        {
                                            if let Some(file) =
                                                result.right.files.get(&finding.evidence.path)
                                            {
                                                let mut d =
                                                    text::doc_from_bytes(None, &file.bytes, None);
                                                d.read_only = true;
                                                d.description = Some(format!(
                                                    "{}:{}",
                                                    finding.evidence.path, finding.evidence.line
                                                ));
                                                open_files = Some(CapturedComparison::Source {
                                                    docs: vec![d.clone(), d],
                                                    line: finding.evidence.line,
                                                });
                                            }
                                        }
                                        if ui.small_button(tr("impact.dismiss")).clicked() {
                                            self.dismissed.insert(finding.id.clone());
                                        }
                                    });
                                    ui.monospace(&finding.evidence.excerpt);
                                    ui.small(&finding.limitations);
                                }
                            });
                    }
                }
            });
        self.open = response.body_returned.is_some();
        open_files
    }
}
