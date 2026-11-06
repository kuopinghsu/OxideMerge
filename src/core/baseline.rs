//! Bounded immutable baseline captures and reproducible release summaries.
use super::{analysis, diff::DiffOptions, filter::FileFilter, semantic, text};
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
    pub fn path(&self) -> &Path {
        match self {
            Self::Folder { path }
            | Self::GitRef { path, .. }
            | Self::ReviewSnapshot { path, .. } => path,
        }
    }
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
    /// When the bytes were read (UTC, RFC 3339); not part of the fingerprint.
    #[serde(default)]
    pub captured_at: String,
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
    RenamedModified,
    BinaryChanged,
    FileTypeChanged,
}
impl Category {
    pub const ALL: [Category; 8] = [
        Category::Added,
        Category::Deleted,
        Category::Modified,
        Category::RenamedModified,
        Category::Moved,
        Category::BinaryChanged,
        Category::FileTypeChanged,
        Category::Unchanged,
    ];
    /// The JSON name, also used as the count key.
    pub fn name(&self) -> &'static str {
        match self {
            Category::Added => "added",
            Category::Deleted => "deleted",
            Category::Modified => "modified",
            Category::Unchanged => "unchanged",
            Category::Moved => "moved",
            Category::RenamedModified => "renamed_modified",
            Category::BinaryChanged => "binary_changed",
            Category::FileTypeChanged => "file_type_changed",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Category::Added => "Added",
            Category::Deleted => "Deleted",
            Category::Modified => "Modified",
            Category::Unchanged => "Unchanged",
            Category::Moved => "Moved",
            Category::RenamedModified => "Renamed with edits",
            Category::BinaryChanged => "Binary changed",
            Category::FileTypeChanged => "File type changed",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseChange {
    pub id: String,
    pub path: String,
    pub old_path: Option<String>,
    /// Percent of matching lines, for a suspected rename with edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<u8>,
    pub category: Category,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub ignored_by_text_rules: bool,
    pub conflict_marker_candidates: bool,
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
    /// Added paths the user said are not renames: they stay added, their sources deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split_renames: Vec<String>,
}
impl ReleaseChange {
    pub fn high_priority(&self) -> bool {
        !self.risk_flags.is_empty()
    }
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
            let (temp, copy) = super::git::export_folder(path, &id, cancel)?;
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
    anyhow::ensure!(
        filter.len() <= 4096 && !filter.contains('\0'),
        "Baseline scope expression exceeds 4 KiB or contains NUL"
    );
    let named = super::filter::load_all_filters();
    if let Some(name) = filter.strip_prefix(super::filter::FLT_PREFIX) {
        anyhow::ensure!(
            named.iter().any(|f| f.name == name.trim()),
            "Unknown named scope filter: {name}"
        );
    }
    let mask = FileFilter::resolve(filter, &named);
    let mut walk = walkdir::WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    let mut observed = 0usize;
    while let Some(entry) = walk.next() {
        observed += 1;
        if observed > analysis::MAX_FILES + 1 {
            bail!(
                "Baseline exceeds 10,000 observed entries; narrow the root or exclude directories"
            );
        }
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
        let bytes = match analysis::read_in_root(entry.path(), &root) {
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
    let fingerprint = analysis::identity(&(
        files
            .iter()
            .map(|(path, file)| (path, &file.digest))
            .collect::<Vec<_>>(),
        &directories,
        &excluded,
        options,
        filter,
    ));
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
            captured_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        files,
        directories,
        excluded,
    })
}
pub fn is_scope_exclusion(reason: &str) -> bool {
    matches!(
        reason,
        "File excluded by scope" | "Directory excluded by scope"
    )
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
/// Smallest share of matching lines for a suspected rename with edits, in percent.
const RENAME_SIMILARITY: usize = 50;
/// Files larger than this are never rename candidates.
const RENAME_MAX_BYTES: usize = 256 << 10;
/// More candidate pairs than this skips the similarity search.
const RENAME_MAX_PAIRS: usize = 250_000;
/// Fewest non-blank lines a suspected rename shares, so that two tiny files with one line
/// in common are not paired.
const RENAME_MIN_SHARED: usize = 3;

/// The lines of a text file, counted.
fn line_counts(bytes: &[u8]) -> Option<(std::collections::HashMap<&str, usize>, usize)> {
    if bytes.len() > RENAME_MAX_BYTES || text::looks_binary(bytes) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut counts = std::collections::HashMap::new();
    let mut total = 0;
    for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        *counts.entry(line).or_insert(0) += 1;
        total += 1;
    }
    (total > 0).then_some((counts, total))
}

/// Added path → (deleted path, percentage of matching lines).
type Renames = BTreeMap<String, (String, u8)>;

/// Pairs deleted and added text files that share most of their lines, by extension. A pair is
/// kept only when it is the clear best for both files; ties leave both files unpaired.
fn similar_renames(
    removed: &[(&String, &CapturedFile)],
    added: &[(&String, &CapturedFile)],
    cancel: &AtomicBool,
) -> Result<(Renames, bool)> {
    let mut found = BTreeMap::new();
    if removed.is_empty() || added.is_empty() {
        return Ok((found, false));
    }
    if removed.len().saturating_mul(added.len()) > RENAME_MAX_PAIRS {
        return Ok((found, true));
    }
    let ext = |p: &str| text::extension(Path::new(p));
    let old: Vec<_> = removed
        .iter()
        .filter_map(|(p, f)| line_counts(&f.bytes).map(|c| (p.as_str(), ext(p), c)))
        .collect();
    let new: Vec<_> = added
        .iter()
        .filter_map(|(p, f)| line_counts(&f.bytes).map(|c| (p.as_str(), ext(p), c)))
        .collect();
    let mut pairs = Vec::new();
    for (o, oext, (ocounts, ototal)) in &old {
        analysis::check_cancel(cancel)?;
        for (n, next, (ncounts, ntotal)) in &new {
            if oext != next || ototal.min(ntotal) * 200 < RENAME_SIMILARITY * (ototal + ntotal) {
                continue;
            }
            let common: usize = ocounts
                .iter()
                .map(|(line, c)| (*c).min(ncounts.get(line).copied().unwrap_or(0)))
                .sum();
            let score = common * 200 / (ototal + ntotal);
            if score >= RENAME_SIMILARITY && common >= RENAME_MIN_SHARED {
                pairs.push((score, *o, *n));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(b.2)));
    let mut used = BTreeSet::new();
    for (i, &(score, o, n)) in pairs.iter().enumerate() {
        if used.contains(&("old", o)) || used.contains(&("new", n)) {
            continue;
        }
        let tied = pairs.iter().enumerate().any(|(j, &(s, oo, nn))| {
            j != i
                && s == score
                && (oo == o || nn == n)
                && !used.contains(&("old", oo))
                && !used.contains(&("new", nn))
        });
        used.insert(("old", o));
        used.insert(("new", n));
        if !tied {
            found.insert(n.to_owned(), (o.to_owned(), score as u8));
        }
    }
    Ok((found, false))
}

#[cfg(test)]
pub fn compare(
    left: &Capture,
    right: &Capture,
    important: &[String],
    cancel: &AtomicBool,
) -> Result<ReleaseSummary> {
    compare_with(left, right, important, &BTreeSet::new(), cancel)
}

/// Compares two captures; `split` lists added paths never to be paired as renames with edits.
pub fn compare_with(
    left: &Capture,
    right: &Capture,
    important: &[String],
    split: &BTreeSet<String>,
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
    fn unpaired<'a>(
        side: &BTreeMap<String, Vec<String>>,
        capture: &'a Capture,
        skip: &dyn Fn(&str) -> bool,
    ) -> Vec<(&'a String, &'a CapturedFile)> {
        side.values()
            .flatten()
            .filter(|p| !skip(p))
            .filter_map(|p| capture.files.get_key_value(p))
            .collect()
    }
    let (similar, skipped) = similar_renames(
        &unpaired(&removed, left, &|p| consumed.contains(p)),
        &unpaired(&added, right, &|p| {
            moved.contains_key(p) || split.contains(p)
        }),
        cancel,
    )?;
    let mut similarity = BTreeMap::new();
    for (new, (old, score)) in similar {
        consumed.insert(old.clone());
        moved.insert(new.clone(), old);
        similarity.insert(new, score);
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
        let conflict_marker_candidates = b.is_some_and(|file| {
            !text::looks_binary(&file.bytes)
                && text::decode(&file.bytes, None)
                    .0
                    .lines()
                    .any(|line| line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>"))
        });
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
                    ignored = super::folder::captured_bytes_equal(
                        &a.bytes,
                        &b.bytes,
                        &left.metadata.rules,
                        Path::new(path),
                    );
                    let format = semantic::Format::of_extension(&text::extension(Path::new(path)));
                    if let Some(f) = format {
                        semantic = Some(semantic::compare(&da.text(), &db.text(), f));
                    }
                    if old_path.is_some() {
                        Category::RenamedModified
                    } else {
                        Category::Modified
                    }
                }
            }
            _ => continue,
        };
        let mut risk_flags = if !matches!(category, Category::Unchanged)
            && important
                .iter()
                .any(|p| path.contains(p) || old_path.as_ref().is_some_and(|o| o.contains(p)))
        {
            vec!["User important-path rule matched; compatibility has not been validated".into()]
        } else {
            Vec::new()
        };
        if conflict_marker_candidates {
            risk_flags.push(
                "Conflict marker candidates remain; inspect the captured source manually".into(),
            );
        }
        let before_digest = a.map(|f| f.digest.clone());
        let after_digest = b.map(|f| f.digest.clone());
        changes.push(ReleaseChange {
            id: analysis::identity(&(path, &before_digest, &after_digest)),
            similarity: similarity.get(path).copied(),
            path: path.clone(),
            old_path,
            category,
            before_digest,
            after_digest,
            ignored_by_text_rules: ignored,
            conflict_marker_candidates,
            semantic,
            risk_flags,
        });
    }
    let mut counts = BTreeMap::new();
    for c in &changes {
        *counts.entry(c.category.name().to_owned()).or_default() += 1;
    }
    counts.insert(
        "conflict_marker_candidates".into(),
        changes
            .iter()
            .filter(|c| c.conflict_marker_candidates)
            .count(),
    );
    counts.insert(
        "high_priority".into(),
        changes.iter().filter(|c| c.high_priority()).count(),
    );
    let mut limitations = vec![
        "Exact unique-content moves are moves. Renames with edits are suspected from shared lines (at least 50% and 3 lines, same extension, text up to 256 KiB, a clear best match only) and can be split back into added and deleted files.".to_owned(),
        "Heuristic priority flags do not validate release compatibility; source references are relative to captured baseline paths.".into(),
        "Files vs directories and excluded ancestors require manual review; unreadable paths are never counted identical.".into(),
    ];
    if skipped {
        limitations.push(
            "Too many added and deleted files to look for renames with edits; they stay added/deleted."
                .into(),
        );
    }
    let excluded = [("left", &left.excluded), ("right", &right.excluded)]
        .into_iter()
        .flat_map(|(side, e)| {
            e.iter()
                .map(move |(p, reason)| (side.into(), p.clone(), reason.clone()))
        })
        .collect();
    Ok(ReleaseSummary {
        version: 1,
        left: left.metadata.clone(),
        right: right.metadata.clone(),
        changes,
        counts,
        excluded,
        limitations,
        split_renames: split.iter().cloned().collect(),
    })
}

/// A self-contained, linked release report. Detail comparisons reuse the existing report
/// renderer in script-disabled frames and are bounded independently of captured input size.
pub fn html_report(summary: &ReleaseSummary, left: &Capture, right: &Capture) -> Result<String> {
    use super::xml_report::esc;
    use std::fmt::Write;
    let json = serde_json::to_string_pretty(summary)?;
    let mut out = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>OxideMerge release summary</title><style>body{font:15px system-ui;margin:2rem}pre{white-space:pre-wrap;overflow-wrap:anywhere}iframe{width:100%;height:32rem;border:1px solid #aaa}section{margin-block:2rem}</style><h1>Release summary</h1><p>Frozen captured baselines. Priority and conflict-marker flags require review; compatibility has not been validated.</p>".to_string();
    for (side, meta) in [("Left", &summary.left), ("Right", &summary.right)] {
        write!(
            out,
            "<p>{side}: {} · captured {} · {}</p>",
            esc(&meta.spec.path().display().to_string()),
            esc(&meta.captured_at),
            esc(meta.resolved_commit.as_deref().unwrap_or(&meta.fingerprint))
        )?;
    }
    out.push_str("<ul class=\"counts\">");
    for category in Category::ALL {
        if let Some(n) = summary.counts.get(category.name()).filter(|&&n| n > 0) {
            write!(out, "<li>{}: {n}</li>", category.label())?;
        }
    }
    out.push_str("</ul>");
    let priority: Vec<_> = summary
        .changes
        .iter()
        .filter(|c| c.high_priority())
        .collect();
    if !priority.is_empty() {
        write!(out, "<h2>High priority ({})</h2><ul>", priority.len())?;
        for change in priority {
            write!(
                out,
                "<li><a href=\"#change-{}\">{}</a>: {}</li>",
                change.id,
                esc(&change.path),
                esc(&change.risk_flags.join(" "))
            )?;
        }
        out.push_str("</ul>");
    }
    write!(
        out,
        "<details><summary>Baseline metadata and versioned JSON</summary><pre>{}</pre></details><nav><ul>",
        esc(&json)
    )?;
    for change in &summary.changes {
        write!(
            out,
            "<li><a href=\"#change-{}\">{}: {}</a></li>",
            change.id,
            change.category.label(),
            esc(&change.path)
        )?;
    }
    out.push_str("</ul></nav>");
    let mut detailed = 0;
    for change in &summary.changes {
        write!(
            out,
            "<section id=\"change-{}\"><h2>{}: {}</h2>",
            change.id,
            change.category.label(),
            esc(&change.path)
        )?;
        match (&change.old_path, change.similarity) {
            (Some(old), Some(score)) => write!(
                out,
                "<p>Suspected rename from {} ({score}% of lines match)</p>",
                esc(old)
            )?,
            (Some(old), None) => write!(out, "<p>Exact content move from {}</p>", esc(old))?,
            _ => {}
        }
        for warning in &change.risk_flags {
            write!(out, "<p>{}</p>", esc(warning))?;
        }
        let a = left
            .files
            .get(change.old_path.as_deref().unwrap_or(&change.path));
        let b = right.files.get(&change.path);
        let bytes = [
            a.map_or(&[][..], |f| f.bytes.as_slice()),
            b.map_or(&[][..], |f| f.bytes.as_slice()),
        ];
        if !matches!(change.category, Category::Unchanged)
            && detailed < 100
            && out.len() < 12 << 20
            && bytes
                .iter()
                .all(|b| b.len() <= 128 << 10 && !text::looks_binary(b))
        {
            let docs: Vec<_> = bytes
                .iter()
                .map(|b| text::doc_from_bytes(None, b, None))
                .collect();
            let refs: Vec<_> = docs.iter().map(|d| d.lines.as_slice()).collect();
            let mut diff = super::diff::compute_with(
                &refs,
                &summary.left.rules,
                super::comments::for_path(Path::new(&change.path)),
            );
            super::diff::include_doc_formats(
                &mut diff,
                &docs.iter().collect::<Vec<_>>(),
                &summary.left.rules,
            );
            let html = super::report::text_html(
                &[
                    format!(
                        "Left: {}",
                        change.old_path.as_deref().unwrap_or(&change.path)
                    ),
                    format!("Right: {}", change.path),
                ],
                &refs,
                &diff,
                &summary.left.rules,
                &[],
                "Captured baseline comparison options",
            );
            let escaped = esc(&html);
            if out.len() + escaped.len() <= 16 << 20 {
                write!(
                    out,
                    "<iframe sandbox loading=\"lazy\" title=\"{}\" srcdoc=\"{}\"></iframe>",
                    esc(&change.path),
                    escaped
                )?;
                detailed += 1;
            } else {
                out.push_str("<p>Detail omitted: HTML preview budget exceeded. Use the captured comparison in the app.</p>");
            }
        } else {
            out.push_str("<p>Detail omitted: unchanged, binary or beyond the bounded HTML preview budget. Full classifications and digests are in the JSON above.</p>");
        }
        out.push_str("</section>");
    }
    out.push_str("</html>");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignored_text_changes_use_the_same_full_content_policy_as_folders() {
        let root = tempfile::tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        for p in [&left, &right] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(left.join("a.rs"), "let x = 1; // old\n").unwrap();
        std::fs::write(right.join("a.rs"), "let x = 1; // new\n").unwrap();
        let options = DiffOptions {
            ignore_comments: true,
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        let a = capture(
            &BaselineSpec::Folder { path: left },
            &options,
            "*",
            &cancel,
            &progress,
        )
        .unwrap();
        let b = capture(
            &BaselineSpec::Folder { path: right },
            &options,
            "*",
            &cancel,
            &progress,
        )
        .unwrap();
        let report = compare(&a, &b, &[], &cancel).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(report.changes[0].ignored_by_text_rules);
        assert!(crate::core::impact::Index::default()
            .analyze(&report, &a, &b, &cancel)
            .unwrap()
            .findings
            .is_empty());
    }

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
        let html = html_report(&summary, &left, &right).unwrap();
        assert!(html.contains("href=\"#change-"));
        assert!(html.contains("<iframe sandbox"));
        assert!(html.contains("srcdoc=\"&lt;!DOCTYPE") || html.contains("srcdoc=\"&lt;!doctype"));

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
        let report = index.analyze(&summary, &left, &right, &cancel).unwrap();
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
        assert!(index.analyze(&summary, &left, &updated, &cancel).is_err());
        let updated_summary = compare(&left, &updated, &["config".into()], &cancel).unwrap();
        let incremental = index
            .analyze(&updated_summary, &left, &updated, &cancel)
            .unwrap();
        let fresh = crate::core::impact::Index::default()
            .analyze(&updated_summary, &left, &updated, &cancel)
            .unwrap();
        assert_eq!(
            serde_json::to_value(incremental).unwrap(),
            serde_json::to_value(fresh).unwrap()
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(compare(&left, &right, &[], &cancel).is_err());
    }
    #[test]
    fn renames_with_edits_are_suspected_only_on_a_clear_match_and_can_be_split() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        let lines = |tag: &str, n: usize| {
            (0..n)
                .map(|i| format!("{tag} line {i}\n"))
                .collect::<String>()
        };
        let body = lines("main", 10);
        std::fs::write(a.join("old.rs"), &body).unwrap();
        std::fs::write(b.join("new.rs"), body.replace("main line 3", "edited")).unwrap();
        let shared = lines("dup", 10);
        std::fs::write(a.join("dup.txt"), &shared).unwrap();
        std::fs::write(b.join("copy1.txt"), shared.replace("dup line 1", "x")).unwrap();
        std::fs::write(b.join("copy2.txt"), shared.replace("dup line 2", "y")).unwrap();
        std::fs::write(a.join("notes.md"), lines("notes", 6)).unwrap();
        std::fs::write(b.join("notes.txt"), lines("notes", 6) + "more\n").unwrap();
        std::fs::write(a.join("gone.cfg"), lines("gone", 8)).unwrap();
        std::fs::write(b.join("fresh.cfg"), lines("fresh", 8)).unwrap();
        // one line in common is too little, however short the files
        std::fs::write(a.join("tiny.ini"), "x=1\n").unwrap();
        std::fs::write(b.join("small.ini"), "x=1\ny=2\n").unwrap();
        let cancel = AtomicBool::new(false);
        let take = |root: &PathBuf| {
            capture(
                &BaselineSpec::Folder { path: root.clone() },
                &DiffOptions::default(),
                "*",
                &cancel,
                &AtomicUsize::new(0),
            )
            .unwrap()
        };
        let (left, right) = (take(&a), take(&b));
        assert!(chrono::DateTime::parse_from_rfc3339(&left.metadata.captured_at).is_ok());
        assert_eq!(take(&a).metadata.fingerprint, left.metadata.fingerprint);
        let summary = compare(&left, &right, &["new".into()], &cancel).unwrap();
        let find = |s: &ReleaseSummary, p: &str| {
            s.changes
                .iter()
                .find(|c| c.path == p)
                .map(|c| (c.category.name(), c.old_path.clone(), c.similarity))
        };
        assert_eq!(
            find(&summary, "new.rs"),
            Some(("renamed_modified", Some("old.rs".into()), Some(90)))
        );
        assert_eq!(find(&summary, "old.rs"), None);
        for (path, category) in [
            ("dup.txt", "deleted"),
            ("copy1.txt", "added"),
            ("copy2.txt", "added"),
            ("notes.md", "deleted"),
            ("notes.txt", "added"),
            ("gone.cfg", "deleted"),
            ("fresh.cfg", "added"),
            ("tiny.ini", "deleted"),
            ("small.ini", "added"),
        ] {
            assert_eq!(find(&summary, path), Some((category, None, None)), "{path}");
        }
        assert_eq!(summary.counts["renamed_modified"], 1);
        assert_eq!(summary.counts["high_priority"], 1);
        let html = html_report(&summary, &left, &right).unwrap();
        assert!(html.contains("Renamed with edits: new.rs"));
        assert!(html.contains("Suspected rename from old.rs (90% of lines match)"));
        assert!(html.contains("High priority (1)"));
        assert!(html.contains("<style>body{font:") && !html.contains("{{"));
        assert!(html.contains(&left.metadata.captured_at));
        let by_old = compare(&left, &right, &["old.rs".into()], &cancel).unwrap();
        assert_eq!(by_old.counts["high_priority"], 1);

        let split = BTreeSet::from(["new.rs".to_owned()]);
        let summary = compare_with(&left, &right, &[], &split, &cancel).unwrap();
        assert_eq!(find(&summary, "new.rs"), Some(("added", None, None)));
        assert_eq!(find(&summary, "old.rs"), Some(("deleted", None, None)));
        assert_eq!(summary.split_renames, ["new.rs"]);
        assert!(!summary.counts.contains_key("renamed_modified"));
    }

    fn take(spec: BaselineSpec) -> Result<Capture> {
        capture(
            &spec,
            &DiffOptions::default(),
            "*",
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
    }

    fn category_of(s: &ReleaseSummary, path: &str) -> Option<(&'static str, Option<String>)> {
        s.changes
            .iter()
            .find(|c| c.path == path)
            .map(|c| (c.category.name(), c.old_path.clone()))
    }

    #[test]
    fn unicode_and_case_only_renames_are_moves_or_renames_with_edits() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            std::fs::create_dir_all(root.join("docs")).unwrap();
        }
        let body = (0..10).map(|i| format!("line {i}\n")).collect::<String>();
        std::fs::write(a.join("résumé.txt"), "cv\n").unwrap();
        std::fs::write(b.join("docs/履歷.txt"), "cv\n").unwrap();
        std::fs::write(a.join("Readme.md"), "read me\n").unwrap();
        std::fs::write(b.join("README.md"), "read me\n").unwrap();
        std::fs::write(a.join("データ.rs"), &body).unwrap();
        std::fs::write(b.join("資料.rs"), body.replace("line 4", "edited")).unwrap();
        std::fs::write(a.join("Lib.rs"), body.replace("line", "lib")).unwrap();
        std::fs::write(b.join("lib.rs"), body.replace("line", "lib") + "more\n").unwrap();
        // the same name composed and decomposed
        std::fs::write(a.join("caf\u{e9}.txt"), "coffee\n").unwrap();
        std::fs::write(b.join("cafe\u{301}.txt"), "coffee\n").unwrap();

        let left = take(BaselineSpec::Folder { path: a }).unwrap();
        let right = take(BaselineSpec::Folder { path: b }).unwrap();
        let summary = compare(&left, &right, &[], &AtomicBool::new(false)).unwrap();
        for (path, expected) in [
            ("docs/履歷.txt", ("moved", Some("résumé.txt"))),
            ("README.md", ("moved", Some("Readme.md"))),
            ("資料.rs", ("renamed_modified", Some("データ.rs"))),
            ("lib.rs", ("renamed_modified", Some("Lib.rs"))),
        ] {
            let expected = (expected.0, expected.1.map(str::to_owned));
            assert_eq!(category_of(&summary, path), Some(expected), "{path}");
        }
        for old in ["résumé.txt", "Readme.md", "データ.rs", "Lib.rs"] {
            assert_eq!(category_of(&summary, old), None, "{old}");
        }
        // A file system may store either form; the file is never added and deleted.
        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        let renamed = category_of(&summary, decomposed)
            .or_else(|| category_of(&summary, composed))
            .unwrap();
        assert!(
            renamed == ("moved", Some(composed.into())) || renamed == ("unchanged", None),
            "{renamed:?}"
        );
        assert_eq!(summary.counts.get("added"), None);
        assert_eq!(summary.counts.get("deleted"), None);
    }

    #[test]
    fn a_review_snapshot_compares_with_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let review = dir.path().join("release.oxmreview");
        std::fs::write(
            &review,
            include_bytes!("../../tests/fixtures/review-0.9.6.oxmreview"),
        )
        .unwrap();
        let spec = BaselineSpec::parse(&format!("review:1:{}", review.display())).unwrap();
        assert!(matches!(spec, BaselineSpec::ReviewSnapshot { side: 1, .. }));
        let folder = dir.path().join("work");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        std::fs::write(folder.join("same.txt"), "same\n").unwrap();
        std::fs::write(folder.join("a.txt"), "ONE\nfour\n").unwrap();
        std::fs::write(folder.join("sub/C.txt"), "cafe\n").unwrap();
        std::fs::write(folder.join("new.txt"), "new\n").unwrap();

        let snapshot = take(spec.clone()).unwrap();
        assert_eq!(
            snapshot.files.keys().collect::<Vec<_>>(),
            ["a.txt", "same.txt", "sub/c.txt"]
        );
        assert!(snapshot.directories.contains("empty"));
        // the review is opened in a new temporary folder each time
        assert_eq!(
            take(spec).unwrap().metadata.fingerprint,
            snapshot.metadata.fingerprint
        );
        let work = take(BaselineSpec::Folder { path: folder }).unwrap();
        let summary = compare(&snapshot, &work, &[], &AtomicBool::new(false)).unwrap();
        assert_eq!(category_of(&summary, "same.txt"), Some(("unchanged", None)));
        assert_eq!(category_of(&summary, "a.txt"), Some(("modified", None)));
        assert_eq!(
            category_of(&summary, "sub/C.txt"),
            Some(("moved", Some("sub/c.txt".into())))
        );
        assert_eq!(category_of(&summary, "new.txt"), Some(("added", None)));
        assert_eq!(summary.changes.len(), 4);
        assert!(matches!(
            summary.left.spec,
            BaselineSpec::ReviewSnapshot { side: 1, .. }
        ));

        let error = take(BaselineSpec::ReviewSnapshot {
            path: review,
            side: 2,
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("out of range"), "{error:#}");
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
