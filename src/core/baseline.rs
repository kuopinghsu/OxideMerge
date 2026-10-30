//! Bounded immutable baseline captures and reproducible release summaries.
use super::{
    analysis,
    diff::{self, DiffOptions},
    filter::FileFilter,
    semantic, text,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselineSpec {
    Folder { path: PathBuf },
    GitRef { path: PathBuf, revision: String },
    ReviewSnapshot { path: PathBuf, side: usize },
}
impl BaselineSpec {
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(value) = value.strip_prefix("git:") {
            let (rev, path) = value.split_once(':').context("Use git:REV:PATH")?;
            return Ok(Self::GitRef {
                path: path.into(),
                revision: rev.into(),
            });
        }
        if let Some(value) = value.strip_prefix("review:") {
            let (side, path) = value
                .split_once(':')
                .context("Use review:SIDE:PATH (zero-based side)")?;
            return Ok(Self::ReviewSnapshot {
                path: path.into(),
                side: side.parse()?,
            });
        }
        Ok(Self::Folder {
            path: value.strip_prefix("folder:").unwrap_or(value).into(),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaselineMetadata {
    pub spec: BaselineSpec,
    pub resolved_commit: Option<String>,
    pub fingerprint: String,
    pub filter: String,
    pub rules: DiffOptions,
    pub working_tree: String,
}
#[derive(Clone)]
pub struct CapturedFile {
    pub bytes: Vec<u8>,
    pub digest: String,
}
pub struct Capture {
    pub metadata: BaselineMetadata,
    pub files: BTreeMap<String, CapturedFile>,
    pub directories: BTreeSet<String>,
    pub excluded: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Added,
    Deleted,
    Modified,
    Unchanged,
    Moved,
    BinaryChanged,
    FileTypeChanged,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseChange {
    pub id: String,
    pub path: String,
    pub old_path: Option<String>,
    pub category: Category,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub ignored_by_text_rules: bool,
    pub semantic: Option<semantic::SemanticReport>,
    pub risk_flags: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseSummary {
    pub version: u32,
    pub left: BaselineMetadata,
    pub right: BaselineMetadata,
    pub changes: Vec<ReleaseChange>,
    pub counts: BTreeMap<String, usize>,
    pub excluded: Vec<(String, String, String)>,
    pub limitations: Vec<String>,
}
pub fn capture(
    spec: &BaselineSpec,
    options: &DiffOptions,
    filter: &str,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> Result<Capture> {
    analysis::check_cancel(cancel)?;
    let mut git_temp = None;
    let mut review_temp = None;
    let mut resolved_commit = None;
    let path = match spec {
        BaselineSpec::Folder { path } => path.clone(),
        BaselineSpec::GitRef { path, revision } => {
            let id = super::git::resolve_commit(path, revision)?;
            let (temp, copy) = super::git::export_folder(path, &id)?;
            resolved_commit = Some(id);
            git_temp = Some(temp);
            copy
        }
        BaselineSpec::ReviewSnapshot { path, side } => {
            let review = super::review::load(path)?;
            let root = review
                .roots
                .get(*side)
                .context("Snapshot side is out of range")?
                .clone();
            review_temp = Some(review);
            root
        }
    };
    let root = path.canonicalize()?;
    if !root.is_dir() {
        bail!("Baseline must be a folder");
    }
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut excluded = BTreeMap::new();
    let mut total = 0;
    let mask = FileFilter::resolve(filter, &super::filter::load_all_filters());
    let mut walk = walkdir::WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = walk.next() {
        analysis::check_cancel(cancel)?;
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                excluded.insert(
                    e.path()
                        .and_then(|p| p.strip_prefix(&root).ok())
                        .map_or_else(String::new, |p| p.to_string_lossy().replace('\\', "/")),
                    e.to_string(),
                );
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&root)?
            .to_str()
            .context("Non-Unicode baseline path is unsupported")?
            .replace('\\', "/");
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_dir() {
            directories.insert(rel.clone());
            if name == ".git" || !mask.include_dir(&rel, &name) {
                excluded.insert(rel, "Directory excluded by scope".into());
                walk.skip_current_dir();
            }
            continue;
        }
        if files.len() + excluded.len() >= analysis::MAX_FILES {
            bail!("Baseline exceeds 10,000 entries; narrow the filter");
        }
        if !mask.include_file(&rel, &name) {
            excluded.insert(rel, "File excluded by scope".into());
            continue;
        }
        if entry.file_type().is_symlink() || !entry.file_type().is_file() {
            excluded.insert(rel, "Link or special file; not assessed".into());
            continue;
        }
        let bytes = match analysis::read(entry.path()) {
            Ok(b) => b,
            Err(e) => {
                excluded.insert(rel, e.to_string());
                continue;
            }
        };
        total += bytes.len() as u64;
        if total > analysis::MAX_TOTAL {
            bail!("Baseline exceeds 256 MiB; narrow the filter");
        }
        progress.fetch_add(1, Ordering::Relaxed);
        files.insert(
            rel,
            CapturedFile {
                digest: analysis::digest(&bytes),
                bytes,
            },
        );
    }
    let fingerprint = analysis::identity(
        &files
            .iter()
            .map(|(p, f)| (p, &f.digest))
            .collect::<Vec<_>>(),
    );
    let _keep_alive = (git_temp, review_temp);
    Ok(Capture {
        metadata: BaselineMetadata {
            spec: spec.clone(),
            resolved_commit,
            fingerprint,
            filter: filter.into(),
            rules: options.clone(),
            working_tree: if matches!(spec, BaselineSpec::GitRef { .. }) {
                "Excluded: committed tree only"
            } else {
                "Captured file bytes; unreadable/excluded paths listed separately"
            }
            .into(),
        },
        files,
        directories,
        excluded,
    })
}
fn excluded(exclusions: &BTreeMap<String, String>, mut path: &str) -> bool {
    if exclusions.contains_key("") {
        return true;
    }
    loop {
        if exclusions.contains_key(path) {
            return true;
        }
        match path.rsplit_once('/') {
            Some((parent, _)) => path = parent,
            None => return false,
        }
    }
}
pub fn compare(
    left: &Capture,
    right: &Capture,
    important: &[String],
    cancel: &AtomicBool,
) -> Result<ReleaseSummary> {
    let mut changes = Vec::new();
    let mut removed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut added: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, f) in &left.files {
        if !right.files.contains_key(path) && !excluded(&right.excluded, path) {
            removed
                .entry(f.digest.clone())
                .or_default()
                .push(path.clone());
        }
    }
    for (path, f) in &right.files {
        if !left.files.contains_key(path) && !excluded(&left.excluded, path) {
            added
                .entry(f.digest.clone())
                .or_default()
                .push(path.clone());
        }
    }
    let mut moved = BTreeMap::new();
    let mut consumed = BTreeSet::new();
    for (digest, old) in &removed {
        if let Some(new) = added.get(digest).filter(|n| n.len() == 1 && old.len() == 1) {
            moved.insert(new[0].clone(), old[0].clone());
            consumed.insert(old[0].clone());
        }
    }
    for path in left
        .files
        .keys()
        .chain(right.files.keys())
        .collect::<BTreeSet<_>>()
    {
        analysis::check_cancel(cancel)?;
        if consumed.contains(path)
            || excluded(&left.excluded, path)
            || excluded(&right.excluded, path)
        {
            continue;
        }
        let old_path = moved.get(path).cloned();
        let a = left.files.get(old_path.as_ref().unwrap_or(path));
        let b = right.files.get(path);
        let mut semantic = None;
        let mut ignored = false;
        let category = match (a, b) {
            (None, Some(_)) if left.directories.contains(path) => Category::FileTypeChanged,
            (Some(_), None) if right.directories.contains(path) => Category::FileTypeChanged,
            (None, Some(_)) => Category::Added,
            (Some(_), None) => Category::Deleted,
            (Some(a), Some(b)) if a.digest == b.digest => {
                if old_path.is_some() {
                    Category::Moved
                } else {
                    Category::Unchanged
                }
            }
            (Some(a), Some(b)) => {
                let binary_a = text::looks_binary(&a.bytes);
                let binary_b = text::looks_binary(&b.bytes);
                if binary_a != binary_b {
                    Category::FileTypeChanged
                } else if binary_a {
                    Category::BinaryChanged
                } else {
                    let da = text::doc_from_bytes(Some(PathBuf::from(path)), &a.bytes, None);
                    let db = text::doc_from_bytes(Some(PathBuf::from(path)), &b.bytes, None);
                    let mut d = diff::compute(&[&da.lines, &db.lines], &left.metadata.rules);
                    diff::include_doc_formats(&mut d, &[&da, &db], &left.metadata.rules);
                    ignored = d.blocks.iter().all(|b| b.ignored);
                    let format = match text::extension(Path::new(path)).as_str() {
                        "json" => Some(semantic::Format::Json),
                        "xml" => Some(semantic::Format::Xml),
                        _ => None,
                    };
                    if let Some(f) = format {
                        semantic = Some(semantic::compare(&da.text(), &db.text(), f));
                    }
                    Category::Modified
                }
            }
            _ => continue,
        };
        let risk_flags = if !matches!(category, Category::Unchanged)
            && important.iter().any(|p| path.contains(p))
        {
            vec!["User important-path rule matched; compatibility has not been validated".into()]
        } else {
            Vec::new()
        };
        let before_digest = a.map(|f| f.digest.clone());
        let after_digest = b.map(|f| f.digest.clone());
        changes.push(ReleaseChange {
            id: analysis::identity(&(path, &before_digest, &after_digest)),
            path: path.clone(),
            old_path,
            category,
            before_digest,
            after_digest,
            ignored_by_text_rules: ignored,
            semantic,
            risk_flags,
        });
    }
    let mut counts = BTreeMap::new();
    for c in &changes {
        *counts
            .entry(
                serde_json::to_value(&c.category)?
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
            .or_default() += 1;
    }
    let excluded = [("left", &left.excluded), ("right", &right.excluded)]
        .into_iter()
        .flat_map(|(side, e)| {
            e.iter()
                .map(move |(p, reason)| (side.into(), p.clone(), reason.clone()))
        })
        .collect();
    Ok(ReleaseSummary { version: 1, left: left.metadata.clone(), right: right.metadata.clone(), changes, counts, excluded, limitations: vec!["Exact unique-content moves only; edited/ambiguous renames stay added/deleted.".into(), "Heuristic priority flags do not validate release compatibility; source references are relative to captured baseline paths.".into(), "Files vs directories and excluded ancestors require manual review; unreadable paths are never counted identical.".into()] })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_exact_moves_exclusions_and_incremental_impact_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        std::fs::write(a.join("old.txt"), "same bytes").unwrap();
        std::fs::write(b.join("new.txt"), "same bytes").unwrap();
        std::fs::write(a.join("config.json"), r#"{"timeout":30}"#).unwrap();
        std::fs::write(b.join("config.json"), r#"{"timeout":60}"#).unwrap();
        std::fs::write(b.join("consumer.txt"), "read timeout from config.json\n").unwrap();
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        let opts = DiffOptions::default();
        let left = capture(
            &BaselineSpec::Folder { path: a },
            &opts,
            "*",
            &cancel,
            &progress,
        )
        .unwrap();
        let right = capture(
            &BaselineSpec::Folder { path: b.clone() },
            &opts,
            "*",
            &cancel,
            &progress,
        )
        .unwrap();
        let summary = compare(&left, &right, &["config".into()], &cancel).unwrap();
        assert_eq!(summary.counts.get("moved"), Some(&1));
        assert_eq!(
            summary
                .changes
                .iter()
                .find(|c| c.path == "new.txt")
                .unwrap()
                .old_path
                .as_deref(),
            Some("old.txt")
        );
        let mut index = crate::core::impact::Index::default();
        let report = index.analyze(&summary, &right, &cancel).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.evidence.path == "consumer.txt" && f.confidence == "heuristic"));
        std::fs::write(b.join("consumer.txt"), "unrelated\n").unwrap();
        let updated = capture(
            &BaselineSpec::Folder { path: b },
            &opts,
            "*",
            &cancel,
            &progress,
        )
        .unwrap();
        let incremental = index.analyze(&summary, &updated, &cancel).unwrap();
        let fresh = crate::core::impact::Index::default()
            .analyze(&summary, &updated, &cancel)
            .unwrap();
        assert_eq!(
            serde_json::to_value(incremental).unwrap(),
            serde_json::to_value(fresh).unwrap()
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(compare(&left, &right, &[], &cancel).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_inputs_are_excluded_and_never_counted_identical() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("secret");
        std::fs::write(&outside, "secret").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(outside, root.join("escape.txt")).unwrap();
        let c = capture(
            &BaselineSpec::Folder { path: root },
            &DiffOptions::default(),
            "*",
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
        assert!(c.files.is_empty());
        assert!(c.excluded.contains_key("escape.txt"));
    }
}
