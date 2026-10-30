//! Mechanical merge verification, separate from semantic/heuristic advice.
//! Planning is read-only. Preview bytes are supplied by the existing merge implementation.
use super::{
    analysis,
    diff::{BlockKind, DiffOptions, DiffResult},
    text::{self, TextDoc},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Info,
    Warning,
    Error,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    NotRun,
    Passed,
    Warning,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationFinding {
    pub id: String,
    pub hunk_id: Option<String>,
    pub row: Option<usize>,
    pub severity: ValidationSeverity,
    pub mechanical: bool,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeAction {
    pub id: String,
    pub path: String,
    pub operation: String,
    pub origin: String,
    pub ranges: Vec<std::ops::Range<usize>>,
    pub bytes_affected: u64,
    pub expected_digest: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergePlan {
    pub version: u32,
    pub id: String,
    pub inputs: Vec<(String, String)>,
    pub rules: DiffOptions,
    pub policy: String,
    pub rollback: String,
    pub actions: Vec<MergeAction>,
    pub findings: Vec<ValidationFinding>,
    pub expected_digest: Option<String>,
    pub preview: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeValidationReport {
    pub version: u32,
    pub plan_id: String,
    pub status: ValidationStatus,
    pub findings: Vec<ValidationFinding>,
    pub output_digest: Option<String>,
    pub limitations: Vec<String>,
}
fn finding(
    id: &str,
    severity: ValidationSeverity,
    mechanical: bool,
    message: &str,
) -> ValidationFinding {
    ValidationFinding {
        id: id.into(),
        hunk_id: None,
        row: None,
        severity,
        mechanical,
        message: message.into(),
    }
}
pub fn text_plan(
    docs: &[TextDoc],
    options: &DiffOptions,
    preview: &TextDoc,
    diff: &DiffResult,
) -> MergePlan {
    let inputs: Vec<_> = docs
        .iter()
        .map(|d| {
            (d.title(), {
                let mut exact = d.clone();
                exact.preserve_eols = true;
                analysis::digest(&text::encode(&exact.text(), exact.encoding, exact.bom))
            })
        })
        .collect();
    let mut actions = Vec::new();
    let mut findings = Vec::new();
    let mut occurrences = std::collections::BTreeMap::new();
    for block in &diff.blocks {
        if block.ignored {
            continue;
        }
        let origin = match block.kind {
            BlockKind::LeftOnly => "left",
            BlockKind::RightOnly => "right",
            BlockKind::BothSame => "both",
            _ => "manual_review",
        };
        let chunks: Vec<_> = docs
            .iter()
            .zip(&block.ranges)
            .map(|(d, r)| &d.lines[r.clone()])
            .collect();
        let context: Vec<_> = docs
            .iter()
            .zip(&block.ranges)
            .map(|(doc, range)| {
                &doc.lines[range.start.saturating_sub(2)..(range.end + 2).min(doc.lines.len())]
            })
            .collect();
        let context_id = analysis::identity(&(
            inputs.iter().map(|i| &i.0).collect::<Vec<_>>(),
            &chunks,
            context,
        ));
        let occurrence = occurrences.entry(context_id.clone()).or_insert(0usize);
        let id = analysis::identity(&(&context_id, *occurrence));
        *occurrence += 1;
        let action = MergeAction {
            id: id.clone(),
            path: docs[1].title(),
            operation: if origin == "manual_review" {
                "conflict"
            } else {
                "merge"
            }
            .into(),
            origin: origin.into(),
            ranges: block.ranges.to_vec(),
            bytes_affected: chunks
                .iter()
                .flat_map(|c| c.iter())
                .map(|s| s.len() as u64)
                .sum(),
            expected_digest: None,
        };
        if origin == "manual_review" {
            let mut f = finding(
                "unresolved_conflict",
                ValidationSeverity::Error,
                true,
                "This block requires explicit manual resolution; preview retains the base.",
            );
            f.hunk_id = Some(id.clone());
            f.row = Some(block.first_row);
            findings.push(f);
        } else if block.ranges[0].is_empty() || block.ranges[2].is_empty() {
            let mut f = finding("deletion_review", ValidationSeverity::Warning, false, "Deletion is mechanically mergeable; check whether removing this content is intended.");
            f.hunk_id = Some(id.clone());
            f.row = Some(block.first_row);
            findings.push(f);
        }
        actions.push(action);
    }
    let bytes = text::encode(&preview.text(), preview.encoding, preview.bom);
    if text::decode(&bytes, Some(preview.encoding)).0 != preview.text() {
        findings.push(finding(
            "encoding_loss",
            ValidationSeverity::Error,
            true,
            "The preview cannot round-trip in its selected encoding.",
        ));
    }
    let mut plan = MergePlan { version: 1, id: String::new(), inputs, rules: options.clone(), policy: "Existing three-way merge; conflicting blocks retain base; ignored edits follow active rules".into(), rollback: "Preview writes nothing. Existing Save and folder merge confirmation/backup policy applies.".into(), actions, findings, expected_digest: Some(analysis::digest(&bytes)), preview: Some(preview.text()) };
    plan.id = analysis::identity(&plan);
    plan
}
pub fn validate(plan: &MergePlan, output: &TextDoc, actual: &[u8]) -> MergeValidationReport {
    let mut findings = plan.findings.clone();
    if output
        .lines
        .iter()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with("=======") || l.starts_with(">>>>>>>"))
    {
        findings.push(finding(
            "conflict_marker",
            ValidationSeverity::Error,
            true,
            "Conflict marker candidates remain in the result; inspect them manually.",
        ));
    }
    let digest = analysis::digest(actual);
    if plan.expected_digest.as_deref() != Some(&digest) {
        findings.push(finding("output_mismatch", ValidationSeverity::Error, true, "Result bytes differ from the planned preview, including encoding/BOM/EOL/final newline."));
    } else {
        findings.push(finding(
            "output_matches",
            ValidationSeverity::Info,
            true,
            "Result bytes match the planned preview exactly.",
        ));
    }
    let status = if findings
        .iter()
        .any(|f| f.severity == ValidationSeverity::Error)
    {
        ValidationStatus::Failed
    } else if findings
        .iter()
        .any(|f| f.severity == ValidationSeverity::Warning)
    {
        ValidationStatus::Warning
    } else {
        ValidationStatus::Passed
    };
    MergeValidationReport { version: 1, plan_id: plan.id.clone(), status, findings, output_digest: Some(digest), limitations: vec!["Mechanical checks do not establish semantic correctness or build/test success.".into(), "Conflicts and independent user edits require manual review; they are never auto-accepted.".into()] }
}
pub fn validate_file(plan: &MergePlan, path: &Path) -> anyhow::Result<MergeValidationReport> {
    let bytes = analysis::read(path)?;
    let doc = text::doc_from_bytes(Some(path.to_owned()), &bytes, None);
    let mut report = validate(plan, &doc, &bytes);
    for (side, (source, expected)) in plan.inputs.iter().enumerate() {
        if side == 1
            && std::path::absolute(Path::new(source)).ok() == std::path::absolute(path).ok()
        {
            continue;
        }
        match analysis::read(Path::new(source)) {
            Ok(bytes) if analysis::digest(&bytes) != *expected => {
                report.findings.push(finding(
                    "input_changed",
                    ValidationSeverity::Error,
                    true,
                    "An input changed since preview; re-preview before validating.",
                ));
                report.status = ValidationStatus::Failed;
            }
            Err(_) => {
                report.findings.push(finding("input_not_rechecked", ValidationSeverity::Warning, true, "An original input is unavailable; byte equality of the result does not verify source freshness."));
                if report.status == ValidationStatus::Passed {
                    report.status = ValidationStatus::Warning;
                }
            }
            _ => {}
        }
    }
    Ok(report)
}
pub fn folder_plan(
    items: &[super::folder::DirItem],
    roots: &[PathBuf],
    options: &DiffOptions,
) -> MergePlan {
    let raw = super::folder_merge::plan(items, roots);
    let mut plan = MergePlan { version: 1, id: String::new(), inputs: roots.iter().map(|p| (p.display().to_string(), String::new())).collect(), rules: options.clone(), policy: "Existing byte-exact folder merge; middle is destination; no files are written by this plan".into(), rollback: "Existing per-file atomic replacement and configured backups; multi-file operations are not transactional.".into(), actions: Vec::new(), findings: Vec::new(), expected_digest: None, preview: None,  };
    for (i, step) in raw.steps {
        use super::folder_merge::Step;
        let item = &items[i];
        let (operation, origin, bytes) = match step {
            Step::Copy(side) => (
                "copy",
                side.to_string(),
                analysis::read(&item.path(&roots[side])).ok(),
            ),
            Step::Merge(doc) => (
                "merge",
                "both".into(),
                Some(text::encode(&doc.text(), doc.encoding, doc.bom)),
            ),
            Step::Delete | Step::Prune => ("delete", "both".into(), None),
            Step::Conflict => ("conflict", "manual_review".into(), None),
        };
        let path = item.rel_path();
        if operation == "copy" && !item.is_dir && bytes.is_none() {
            plan.findings.push(finding("copy_not_fingerprinted", ValidationSeverity::Warning, true, "Copy content could not be fingerprinted within analysis limits; this is not a verified copy plan."));
        }
        plan.actions.push(MergeAction {
            id: analysis::identity(&(&path, &origin, &bytes)),
            path,
            operation: operation.into(),
            origin,
            ranges: Vec::new(),
            bytes_affected: bytes.as_ref().map_or(0, |b| b.len() as u64),
            expected_digest: bytes.as_ref().map(|b| analysis::digest(b)),
        });
    }
    if !raw.stale.is_empty() {
        plan.findings.push(finding(
            "stale_inputs",
            ValidationSeverity::Error,
            true,
            "Files changed since scan; refresh before merging.",
        ));
    }
    plan.id = analysis::identity(&plan);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_are_deterministic_and_validate_saved_bytes_not_only_lines() {
        let cfg = crate::config::Config::default();
        let docs = ["a\nLEFT\nc\nd\n", "a\nb\nc\nd\n", "a\nb\nc\nRIGHT\n"]
            .map(|t| TextDoc::from_text(None, t));
        let view = crate::ui::text_view::TextView::new(docs.to_vec(), &cfg);
        let a = view.preview_merge(&cfg).unwrap();
        let b = view.preview_merge(&cfg).unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
        assert_eq!(a.preview.as_deref(), Some("a\nLEFT\nc\nRIGHT\n"));
        let output = TextDoc::from_text(None, a.preview.as_ref().unwrap());
        assert_eq!(
            validate(&a, &output, output.text().as_bytes()).status,
            ValidationStatus::Passed
        );
        let corrupted = output.text().replace('\n', "\r\n");
        assert_eq!(
            validate(&a, &output, corrupted.as_bytes()).status,
            ValidationStatus::Failed
        );
    }
    #[test]
    fn conflicts_and_marker_candidates_can_never_pass_verification() {
        let cfg = crate::config::Config::default();
        let docs = ["left\n", "base\n", "right\n"].map(|t| TextDoc::from_text(None, t));
        let view = crate::ui::text_view::TextView::new(docs.to_vec(), &cfg);
        let plan = view.preview_merge(&cfg).unwrap();
        assert!(plan.actions.iter().any(|a| a.operation == "conflict"));
        let output = TextDoc::from_text(None, plan.preview.as_ref().unwrap());
        assert_eq!(
            validate(&plan, &output, output.text().as_bytes()).status,
            ValidationStatus::Failed
        );
        assert!(plan.findings.iter().any(|f| f.hunk_id.is_some()));
    }
}
