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
    #[serde(default)]
    pub source_fingerprints: Vec<std::collections::BTreeMap<String, Option<String>>>,
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
pub fn doc_fingerprint(doc: &TextDoc) -> String {
    let mut exact = doc.clone();
    exact.preserve_eols = true;
    analysis::digest(&text::encode(&exact.text(), exact.encoding, exact.bom))
}
pub fn text_plan(
    docs: &[TextDoc],
    options: &DiffOptions,
    preview: &TextDoc,
    diff: &DiffResult,
) -> MergePlan {
    let inputs: Vec<_> = docs
        .iter()
        .map(|d| (d.title(), doc_fingerprint(d)))
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
            BlockKind::BothSame | BlockKind::Compatible => "both",
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
        } else if (block.ranges[0].is_empty() || block.ranges[2].is_empty())
            && block.ranges.iter().any(|range| !range.is_empty())
        {
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
    let mut plan = MergePlan { version: 1, id: String::new(), inputs, rules: options.clone(), policy: "Existing three-way merge; conflicting blocks retain base; ignored edits follow active rules".into(), rollback: "Preview writes nothing. Existing Save and folder merge confirmation/backup policy applies.".into(), actions, findings, expected_digest: Some(analysis::digest(&bytes)), preview: Some(preview.text()), source_fingerprints: Vec::new() };
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
    folder_plan_control(
        items,
        roots,
        options,
        &std::sync::atomic::AtomicBool::new(false),
        &std::sync::atomic::AtomicUsize::new(0),
    )
}
pub fn folder_plan_control(
    items: &[super::folder::DirItem],
    roots: &[PathBuf],
    options: &DiffOptions,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &std::sync::atomic::AtomicUsize,
) -> MergePlan {
    let mut plan = MergePlan { version: 1, id: String::new(), inputs: roots.iter().map(|p| (p.display().to_string(), String::new())).collect(), rules: options.clone(), policy: "Existing byte-exact folder merge; middle is destination; no files are written by this plan".into(), rollback: "Existing per-file atomic replacement and configured backups; multi-file operations are not transactional.".into(), actions: Vec::new(), findings: Vec::new(), expected_digest: None, preview: None, source_fingerprints: Vec::new() };
    if roots.len() != 3 {
        plan.findings.push(finding(
            "invalid_sides",
            ValidationSeverity::Error,
            true,
            "Folder merge analysis requires three roots",
        ));
        plan.id = analysis::identity(&plan);
        return plan;
    }
    if items.len() > analysis::MAX_FILES
        || items
            .iter()
            .filter(|i| !i.is_dir)
            .flat_map(|i| i.size)
            .flatten()
            .any(|size| size > analysis::MAX_FILE)
        || items.iter().flat_map(|i| i.size).flatten().sum::<u64>() > analysis::MAX_TOTAL * 3
    {
        plan.findings.push(finding(
            "analysis_limit",
            ValidationSeverity::Error,
            true,
            "Folder merge analysis exceeds its entry/byte limits; narrow the selected scope.",
        ));
        plan.id = analysis::identity(&plan);
        return plan;
    }
    let fingerprints = folder_fingerprints(items, roots, cancel, progress);
    match &fingerprints {
        Ok(f) => plan.source_fingerprints = f.clone(),
        Err(e) => {
            plan.findings.push(finding(
                "input_not_fingerprinted",
                ValidationSeverity::Error,
                true,
                &e.to_string(),
            ));
            plan.id = analysis::identity(&plan);
            return plan;
        }
    }
    let raw = super::folder_merge::plan(items, roots);
    let mut planned = std::collections::BTreeSet::new();
    for (i, step) in raw.steps {
        planned.insert(i);

        use super::folder_merge::Step;
        let item = &items[i];
        let (operation, origin, bytes) = match step {
            Step::Copy(side) => (
                if item.is_dir {
                    "create_directory"
                } else {
                    "copy"
                },
                side.to_string(),
                analysis::read(&item.path(&roots[side])).ok(),
            ),
            Step::Merge(doc) => (
                "merge",
                "both".into(),
                Some(text::encode(&doc.text(), doc.encoding, doc.bom)),
            ),
            Step::Delete => ("delete", "both".into(), None),
            Step::Prune => ("prune_if_empty", "both".into(), None),
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
    for (i, item) in items
        .iter()
        .enumerate()
        .filter(|(i, item)| !planned.contains(i) && !item.is_dir && item.exists[1])
    {
        let _ = i;
        let path = item.rel_path();
        let digest = analysis::read_in_root(
            &item.path(&roots[1]),
            &roots[1].canonicalize().unwrap_or_else(|_| roots[1].clone()),
        )
        .ok()
        .map(|bytes| analysis::digest(&bytes));
        if digest.is_none() {
            plan.findings.push(finding(
                "retained_not_fingerprinted",
                ValidationSeverity::Warning,
                true,
                "An unchanged destination could not be fingerprinted and cannot be verified.",
            ));
        }
        plan.actions.push(MergeAction {
            id: analysis::identity(&(&path, &digest)),
            path,
            operation: "retain".into(),
            origin: "base".into(),
            ranges: Vec::new(),
            bytes_affected: item.size[1].unwrap_or(0),
            expected_digest: digest,
        });
    }
    if folder_fingerprints(items, roots, cancel, progress)
        .ok()
        .as_ref()
        != Some(&plan.source_fingerprints)
    {
        plan.findings.push(finding(
            "inputs_changed_during_preview",
            ValidationSeverity::Error,
            true,
            "Inputs changed while planning; compare again before merging",
        ));
    }
    plan.actions
        .sort_by(|a, b| (&a.path, &a.operation).cmp(&(&b.path, &b.operation)));
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

fn folder_fingerprints(
    items: &[super::folder::DirItem],
    roots: &[PathBuf],
    cancel: &std::sync::atomic::AtomicBool,
    progress: &std::sync::atomic::AtomicUsize,
) -> anyhow::Result<Vec<std::collections::BTreeMap<String, Option<String>>>> {
    roots
        .iter()
        .enumerate()
        .map(|(side, root)| {
            let canonical = root.canonicalize()?;
            items
                .iter()
                .filter(|item| !item.is_dir)
                .map(|item| {
                    analysis::check_cancel(cancel)?;
                    progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let digest = if item.exists[side] {
                        Some(analysis::digest(&analysis::read_in_root(
                            &item.path(root),
                            &canonical,
                        )?))
                    } else {
                        anyhow::ensure!(matches!(std::fs::symlink_metadata(item.path(root)), Err(ref e) if e.kind() == std::io::ErrorKind::NotFound), "A previously missing source appeared or changed type");
                        None
                    };
                    Ok((item.rel_path(), digest))
                })
                .collect()
        })
        .collect()
}
/// Re-read every scoped planned/retained output. Extra excluded paths are not certified.
pub fn validate_folder(
    plan: &MergePlan,
    destination: &Path,
) -> anyhow::Result<MergeValidationReport> {
    validate_folder_control(
        plan,
        destination,
        &std::sync::atomic::AtomicBool::new(false),
        &std::sync::atomic::AtomicUsize::new(0),
    )
}
pub fn validate_folder_control(
    plan: &MergePlan,
    destination: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &std::sync::atomic::AtomicUsize,
) -> anyhow::Result<MergeValidationReport> {
    let root = destination.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "Folder merge result must be a directory");
    let mut findings = plan.findings.clone();
    let mut output = std::collections::BTreeMap::new();
    for (side, fingerprints) in plan.source_fingerprints.iter().enumerate() {
        let Some((input, _)) = plan.inputs.get(side) else {
            continue;
        };
        let input = Path::new(input);
        if side == 1 && input.canonicalize().ok().as_ref() == Some(&root) {
            continue;
        }
        let source_root = input.canonicalize()?;
        for (rel, expected) in fingerprints {
            analysis::check_cancel(cancel)?;
            progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            anyhow::ensure!(
                super::review::safe_path(rel),
                "Unsafe folder fingerprint path"
            );
            let path = source_root.join(rel);
            let matched = match expected {
                Some(expected) => analysis::read_in_root(&path, &source_root)
                    .is_ok_and(|bytes| analysis::digest(&bytes) == *expected),
                None => {
                    matches!(std::fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
                }
            };
            if !matched {
                findings.push(finding(
                    "source_changed",
                    ValidationSeverity::Error,
                    true,
                    &format!("Source changed since preview: side {} {rel}", side + 1),
                ));
            }
        }
    }

    for action in &plan.actions {
        analysis::check_cancel(cancel)?;
        progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !super::review::safe_path(&action.path) {
            anyhow::bail!("Non-portable/unsafe merge validation path");
        }
        let path = root.join(&action.path);
        let result: anyhow::Result<Option<String>> = (|| {
            let metadata = std::fs::symlink_metadata(&path);
            match action.operation.as_str() {
                "conflict" => {
                    anyhow::bail!("Unresolved planned conflict requires explicit manual review")
                }
                "delete" => {
                    anyhow::ensure!(
                        matches!(metadata, Err(ref e) if e.kind() == std::io::ErrorKind::NotFound),
                        "Planned deletion is still present"
                    );
                    Ok(None)
                }
                "create_directory" => {
                    let m = metadata?;
                    anyhow::ensure!(
                        m.is_dir()
                            && !m.file_type().is_symlink()
                            && path.canonicalize()?.starts_with(&root),
                        "Planned directory is missing or unsafe"
                    );
                    Ok(None)
                }
                "prune_if_empty" => {
                    match metadata {
                        Ok(m) => {
                            anyhow::ensure!(
                                m.is_dir()
                                    && !m.file_type().is_symlink()
                                    && path.canonicalize()?.starts_with(&root)
                                    && std::fs::read_dir(&path)?.next().is_some(),
                                "An empty directory should have been pruned"
                            );
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                    };
                    Ok(None)
                }
                _ => {
                    let bytes = analysis::read_in_root(&path, &root)?;
                    let digest = analysis::digest(&bytes);
                    anyhow::ensure!(
                        action.expected_digest.as_deref() == Some(&digest),
                        "Saved output bytes differ from preview or could not be fingerprinted"
                    );
                    Ok(Some(digest))
                }
            }
        })();
        match result {
            Ok(digest) => {
                output.insert(action.path.clone(), digest);
            }
            Err(error) => {
                let mut f = finding(
                    "folder_output_mismatch",
                    ValidationSeverity::Error,
                    true,
                    &format!("{}: {error:#}", action.path),
                );
                f.hunk_id = Some(action.id.clone());
                findings.push(f);
            }
        }
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
    Ok(MergeValidationReport { version: 1, plan_id: plan.id.clone(), status, findings, output_digest: Some(analysis::identity(&output)), limitations: vec!["Only captured/scoped planned and retained paths were checked; excluded or additional paths are not certified.".into(), "No claim about semantic correctness, build/test success, or multi-file transactional writes.".into()] })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_plan_verifies_retained_deleted_created_and_merged_outputs_and_source_drift() {
        use crate::core::{
            filter::FileFilter,
            folder::{FolderOptions, Progress, Scanner},
        };
        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["left", "base", "right"]
            .iter()
            .map(|p| temp.path().join(p))
            .collect();
        for root in &roots {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("keep.txt"), "keep\n").unwrap();
        }
        for (side, body) in [
            "LEFT\ntwo\nthree\n",
            "one\ntwo\nthree\n",
            "one\ntwo\nRIGHT\n",
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(roots[side].join("a.txt"), body).unwrap();
        }
        std::fs::write(roots[1].join("deleted.txt"), "delete\n").unwrap();
        std::fs::write(roots[2].join("new.txt"), "new\n").unwrap();
        let options = DiffOptions::default();
        let folder = FolderOptions {
            recursive: true,
            ..Default::default()
        };
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &folder,
            dopts: &options,
            progress: &Progress::default(),
        }
        .run();
        let destination = temp.path().join("result");
        let plan = folder_plan(&items, &roots, &options);
        assert!(
            !destination.exists(),
            "planning must create no output directory"
        );
        assert!(plan.actions.iter().any(|a| a.operation == "retain"));
        assert_eq!(
            std::fs::read_to_string(roots[1].join("deleted.txt")).unwrap(),
            "delete\n"
        );
        std::fs::create_dir(&destination).unwrap();
        for (name, body) in [
            ("a.txt", "LEFT\ntwo\nRIGHT\n"),
            ("keep.txt", "keep\n"),
            ("new.txt", "new\n"),
        ] {
            std::fs::write(destination.join(name), body).unwrap();
        }
        assert_eq!(
            validate_folder(&plan, &destination).unwrap().status,
            ValidationStatus::Passed
        );
        std::fs::write(destination.join("keep.txt"), "CORRUPTED\n").unwrap();
        assert_eq!(
            validate_folder(&plan, &destination).unwrap().status,
            ValidationStatus::Failed
        );
        std::fs::write(destination.join("keep.txt"), "keep\n").unwrap();
        let stamp = std::fs::metadata(roots[2].join("new.txt"))
            .unwrap()
            .modified()
            .unwrap();
        std::fs::write(roots[2].join("new.txt"), "BAD\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(roots[2].join("new.txt"))
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let report = validate_folder(&plan, &destination).unwrap();
        assert_eq!(report.status, ValidationStatus::Failed);
        assert!(report.findings.iter().any(|f| f.id == "source_changed"));
        assert!(validate_folder_control(
            &plan,
            &destination,
            &std::sync::atomic::AtomicBool::new(true),
            &std::sync::atomic::AtomicUsize::new(0)
        )
        .is_err());
    }

    #[test]
    fn preview_preserves_bom_crlf_legacy_encoding_and_no_final_newline() {
        for encoding in [
            text::TextEncoding::Utf8,
            text::TextEncoding::Other(encoding_rs::WINDOWS_1252),
        ] {
            let bom = encoding == text::TextEncoding::Utf8;
            let strings = [
                "café LEFT\r\ncommon\r\nlast",
                "café base\r\ncommon\r\nlast",
                "café base\r\ncommon\r\nRIGHT",
            ];
            let docs: Vec<_> = strings
                .into_iter()
                .map(|s| {
                    let bytes = text::encode(s, encoding, bom);
                    text::doc_from_bytes(None, &bytes, Some(encoding))
                })
                .collect();
            let cfg = crate::config::Config::default();
            let view = crate::ui::text_view::TextView::new(docs, &cfg);
            let plan = view.preview_merge(&cfg).unwrap();
            let expected = "café LEFT\r\ncommon\r\nRIGHT";
            let bytes = text::encode(expected, encoding, bom);
            assert_eq!(plan.expected_digest, Some(analysis::digest(&bytes)));
            let output = text::doc_from_bytes(None, &bytes, Some(encoding));
            assert_eq!(
                validate(&plan, &output, &bytes).status,
                ValidationStatus::Passed
            );
        }
    }
    #[test]
    fn fixed_seed_independent_merge_edits_are_not_lost() {
        let mut seed = 0x5eed_u64;
        let cfg = crate::config::Config::default();
        for _ in 0..64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = (seed as usize % 5) + 1;
            let base: Vec<String> = (0..16).map(|i| format!("line {i}")).collect();
            let mut left = base.clone();
            let mut right = base.clone();
            left[at] = format!("left {seed}");
            right[at + 8] = format!("right {seed}");
            let docs = [&left, &base, &right]
                .map(|l| TextDoc::from_text(None, &l.join("\n")))
                .to_vec();
            let view = crate::ui::text_view::TextView::new(docs, &cfg);
            let plan = view.preview_merge(&cfg).unwrap();
            let mut expected = base;
            expected[at] = left[at].clone();
            expected[at + 8] = right[at + 8].clone();
            assert_eq!(plan.preview, Some(expected.join("\n")));
            assert!(plan
                .findings
                .iter()
                .all(|f| f.severity != ValidationSeverity::Error));
        }
    }

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
