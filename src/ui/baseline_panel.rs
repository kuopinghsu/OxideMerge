use super::analysis::Task;
use super::widgets::{self, Icon};
use crate::{
    core::{
        analysis,
        baseline::{self, BaselineSpec, Capture, ReleaseSummary},
        impact,
        text::{self, TextDoc},
    },
    i18n::{tr, trf},
};
use std::{collections::BTreeSet, sync::Arc};
pub enum CapturedComparison {
    Text(Vec<TextDoc>),
    Source { docs: Vec<TextDoc>, line: usize },
    Bytes(Vec<(String, Vec<u8>)>),
}
pub struct ResultSet {
    pub left: Arc<Capture>,
    pub right: Arc<Capture>,
    pub summary: ReleaseSummary,
    pub impact: Option<impact::ImpactReport>,
}

/// Summarizes `left` and `right`, with the impact of the changes when asked.
fn summarize(
    left: Arc<Capture>,
    right: Arc<Capture>,
    important: &[String],
    split: &BTreeSet<String>,
    index: Option<&std::sync::Mutex<impact::Index>>,
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<ResultSet> {
    let summary = baseline::compare_with(&left, &right, important, split, cancel)?;
    let impact = match index {
        Some(index) => Some(
            index
                .lock()
                .map_err(|_| anyhow::anyhow!("Impact index worker failed; reopen the comparison"))?
                .analyze(&summary, &left, &right, cancel)?,
        ),
        None => None,
    };
    Ok(ResultSet {
        left,
        right,
        summary,
        impact,
    })
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
    /// Categories shown in the change list, by name; empty shows all.
    categories: BTreeSet<&'static str>,
    priority_only: bool,
    /// Suspected renames the user split back into an added and a deleted file.
    split: BTreeSet<String>,
    policy_id: String,
    index: std::sync::Arc<std::sync::Mutex<impact::Index>>,
    initialized: bool,
    scope: String,
    /// Shows a link from each impact finding to the review workspace of the folder compare.
    pub review_links: bool,
    /// The evidence path of the finding whose review link was clicked, for the folder compare.
    pub review_request: Option<String>,
}
impl BaselinePanel {
    pub fn with_review_links() -> Self {
        BaselinePanel {
            review_links: true,
            ..Default::default()
        }
    }
    pub fn invalidate(&mut self) {
        self.job = None;
        self.result = None;
        self.error.clear();
    }
    pub fn ui(
        &mut self,
        ui: Option<&mut egui::Ui>,
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
        let ui = ui?;
        if let Some(task) = &self.export_job {
            task.status(ui);
        }
        let mut open_files = None;
        let mut resplit = None;
        let mut restore = false;
        {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(tr("baseline.filter"));
                changed |= ui
                    .add(egui::TextEdit::singleline(&mut self.scope).char_limit(1024))
                    .changed();
            });
            ui.small(tr("baseline.syntax"));
            for (side, spec) in self.specs.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    widgets::side_chip(ui, side, 2);
                    let label = tr(if side == 0 {
                        "baseline.left"
                    } else {
                        "baseline.right"
                    });
                    let response = ui.add(egui::TextEdit::singleline(spec).hint_text(label));
                    changed |= response.changed();
                    response.widget_info(|| {
                        let mut info =
                            egui::WidgetInfo::text_edit(true, spec.as_str(), spec.as_str(), "");
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
                self.split.clear();
                let specs = [
                    BaselineSpec::parse(&self.specs[0]),
                    BaselineSpec::parse(&self.specs[1]),
                ];
                match specs {
                    [Ok(a), Ok(b)] => {
                        let options = options.clone();
                        let filter = self.scope.clone();
                        let important = self.important_paths();
                        let index = self.impact_enabled.then(|| self.index.clone());
                        self.job = Some(Task::start(move |cancel, progress| {
                            let left = baseline::capture(&a, &options, &filter, cancel, progress)?;
                            let right = baseline::capture(&b, &options, &filter, cancel, progress)?;
                            summarize(
                                Arc::new(left),
                                Arc::new(right),
                                &important,
                                &BTreeSet::new(),
                                index.as_deref(),
                                cancel,
                            )
                        }));
                    }
                    _ => self.error = tr("baseline.invalid").into(),
                }
            }
            if let Some(job) = &mut self.job {
                if let Some(result) = job.poll() {
                    match result {
                        Ok(r) => {
                            let fresh = self.result.as_ref().is_none_or(|old| {
                                old.left.metadata.fingerprint != r.left.metadata.fingerprint
                                    || old.right.metadata.fingerprint
                                        != r.right.metadata.fingerprint
                            });
                            if fresh {
                                self.dismissed.clear();
                            }
                            self.result = Some(Arc::new(r));
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
                for (side, meta) in [&result.summary.left, &result.summary.right]
                    .into_iter()
                    .enumerate()
                {
                    ui.horizontal(|ui| {
                        widgets::side_chip(ui, side, 2);
                        ui.label(trf(
                            "baseline.captured_at",
                            &[("time", meta.captured_at.clone())],
                        ));
                    })
                    .response
                    .on_hover_text(format!(
                        "{} · {}",
                        meta.resolved_commit.as_deref().unwrap_or("SHA-256"),
                        meta.fingerprint
                    ));
                }
                let counts = &result.summary.counts;
                self.categories
                    .retain(|name| counts.get(*name).is_some_and(|&n| n > 0));
                ui.horizontal_wrapped(|ui| {
                    let mut all = self.categories.is_empty() && !self.priority_only;
                    let label = tr("analysis.all");
                    if widgets::toggle_icon_button(ui, &mut all, None, label, label).clicked() {
                        self.categories.clear();
                        self.priority_only = false;
                    }
                    for category in baseline::Category::ALL {
                        let name = category.name();
                        let Some(&n) = counts.get(name).filter(|&&n| n > 0) else {
                            continue;
                        };
                        let mut on = self.categories.contains(name);
                        let label = format!("{} {n}", category_label(&category));
                        if widgets::toggle_icon_button(ui, &mut on, None, &label, &label).clicked()
                            && !self.categories.remove(name)
                        {
                            self.categories.insert(name);
                        }
                    }
                    let n = counts.get("high_priority").copied().unwrap_or(0);
                    if n > 0 {
                        let label = format!("{} {n}", tr("baseline.high_priority"));
                        widgets::toggle_icon_button(
                            ui,
                            &mut self.priority_only,
                            Some(Icon::Warning),
                            &label,
                            &label,
                        );
                    }
                });
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
                                } else if ["html", "htm"].contains(&text::extension(&path).as_str())
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
                let priority: Vec<_> = result
                    .summary
                    .changes
                    .iter()
                    .filter(|c| c.high_priority())
                    .collect();
                if !priority.is_empty() && !self.priority_only {
                    ui.horizontal(|ui| {
                        widgets::small_icon(ui, Icon::Warning, widgets::pal(ui).warning);
                        ui.label(format!(
                            "{} ({})",
                            tr("baseline.high_priority"),
                            priority.len()
                        ));
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("release_priority")
                        .max_height(90.0)
                        .show(ui, |ui| {
                            for change in priority {
                                if ui.selectable_label(false, &change.path).clicked() {
                                    open_files = Some(open_change(result, change));
                                }
                                for flag in &change.risk_flags {
                                    ui.small(flag);
                                }
                            }
                        });
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .id_salt("release_rows")
                    .max_height(190.0)
                    .show(ui, |ui| {
                        for change in &result.summary.changes {
                            let name = change.category.name();
                            if (self.changes_only
                                && matches!(change.category, baseline::Category::Unchanged))
                                || (!self.categories.is_empty() && !self.categories.contains(name))
                                || (self.priority_only && !change.high_priority())
                            {
                                continue;
                            }
                            let label = format!(
                                "{} {}{}",
                                category_label(&change.category),
                                change.path,
                                if change.ignored_by_text_rules {
                                    tr("baseline.ignored")
                                } else {
                                    ""
                                },
                            );
                            ui.horizontal(|ui| {
                                if ui.selectable_label(false, label).clicked() {
                                    open_files = Some(open_change(result, change));
                                }
                                if change.high_priority() {
                                    let priority = tr("baseline.high_priority");
                                    let mark = widgets::small_icon(
                                        ui,
                                        Icon::Warning,
                                        widgets::pal(ui).warning,
                                    );
                                    mark.widget_info(|| {
                                        egui::WidgetInfo::labeled(egui::Role::Image, true, priority)
                                    });
                                    mark.on_hover_text(priority);
                                }
                            });
                            if let Some(old) = &change.old_path {
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    match change.similarity {
                                        Some(n) => {
                                            ui.small(trf(
                                                "baseline.renamed_from",
                                                &[("path", old.clone()), ("n", n.to_string())],
                                            ));
                                            if ui.small_button(tr("baseline.not_rename")).clicked()
                                            {
                                                resplit = Some(change.path.clone());
                                            }
                                        }
                                        None => {
                                            ui.small(trf(
                                                "baseline.moved_from",
                                                &[("path", old.clone())],
                                            ));
                                        }
                                    }
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
                if !self.split.is_empty()
                    && ui
                        .small_button(trf(
                            "baseline.restore_renames",
                            &[("n", self.split.len().to_string())],
                        ))
                        .clicked()
                {
                    restore = true;
                }
                if let Some(impact) = &result.impact {
                    ui.separator();
                    ui.label(tr("impact.title"));
                    ui.small(&impact.assessment);
                    for note in &impact.notes {
                        ui.colored_label(widgets::pal(ui).warning, note);
                    }
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
                                                tr(if finding.confidence == "exact" {
                                                    "impact.exact"
                                                } else {
                                                    "analysis.heuristic"
                                                }),
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
                                    if self.review_links
                                        && ui.small_button(tr("impact.review")).clicked()
                                    {
                                        self.review_request = Some(finding.evidence.path.clone());
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
        }
        let rerun = resplit.is_some() || restore;
        if let Some(path) = resplit {
            self.split.insert(path);
        }
        if restore {
            self.split.clear();
        }
        if let Some(result) = self.result.as_ref().filter(|_| rerun) {
            let (left, right) = (result.left.clone(), result.right.clone());
            let important = self.important_paths();
            let split = self.split.clone();
            let index = self.impact_enabled.then(|| self.index.clone());
            self.job = Some(Task::start(move |cancel, _| {
                summarize(left, right, &important, &split, index.as_deref(), cancel)
            }));
        }
        open_files
    }

    fn important_paths(&self) -> Vec<String> {
        self.important
            .split(';')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

fn category_label(category: &baseline::Category) -> &'static str {
    use baseline::Category::*;
    tr(match category {
        Added => "baseline.cat.added",
        Deleted => "baseline.cat.deleted",
        Modified => "baseline.cat.modified",
        RenamedModified => "baseline.cat.renamed_modified",
        Moved => "baseline.cat.moved",
        BinaryChanged => "baseline.cat.binary_changed",
        FileTypeChanged => "baseline.cat.file_type_changed",
        Unchanged => "baseline.cat.unchanged",
    })
}

/// The two captured sides of `change`, read-only, as text or as bytes.
fn open_change(result: &ResultSet, change: &baseline::ReleaseChange) -> CapturedComparison {
    let sides = [&result.left, &result.right].map(|capture| capture.as_ref());
    let keys = [
        change.old_path.as_deref().unwrap_or(&change.path),
        change.path.as_str(),
    ];
    let bytes: Vec<_> = sides
        .iter()
        .zip(keys)
        .map(|(capture, key)| {
            let file = capture.files.get(key).map(|f| f.bytes.clone());
            (key.to_string(), file.unwrap_or_default())
        })
        .collect();
    if bytes.iter().any(|(_, b)| text::looks_binary(b)) {
        return CapturedComparison::Bytes(bytes);
    }
    let docs = sides
        .iter()
        .zip(&bytes)
        .map(|(capture, (key, bytes))| {
            let mut doc = text::doc_from_bytes(None, bytes, None);
            doc.read_only = true;
            doc.description = Some(format!("{key} · {}", &capture.metadata.fingerprint[..12]));
            doc.syntax = Some(text::extension(std::path::Path::new(key)));
            doc
        })
        .collect();
    CapturedComparison::Text(docs)
}
