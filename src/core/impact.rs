//! Conservative evidence-only references. This module never runs a build or rewrites inputs.
use super::{
    analysis,
    baseline::{Capture, Category, ReleaseSummary},
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line: usize,
    pub excerpt: String,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImpactFinding {
    pub id: String,
    pub origin_change_id: String,
    pub related_paths: Vec<String>,
    pub relation_type: String,
    pub evidence: Evidence,
    pub severity: String,
    pub confidence: String,
    pub rule_id: String,
    pub limitations: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImpactReport {
    pub version: u32,
    pub baseline_fingerprint: String,
    pub findings: Vec<ImpactFinding>,
    pub assessment: String,
    pub excluded: BTreeMap<String, String>,
}
struct CacheEntry {
    lines: Vec<(usize, String)>,
    footprint: usize,
    long_lines: bool,
}
#[derive(Default)]
pub struct Index {
    version: &'static str,
    cache: BTreeMap<String, CacheEntry>,
}
impl Index {
    pub fn analyze(
        &mut self,
        summary: &ReleaseSummary,
        target: &Capture,
        cancel: &AtomicBool,
    ) -> Result<ImpactReport> {
        let result = self.analyze_inner(summary, target, cancel);
        if result.is_err() {
            self.cache.clear();
        }
        result
    }
    fn analyze_inner(
        &mut self,
        summary: &ReleaseSummary,
        target: &Capture,
        cancel: &AtomicBool,
    ) -> Result<ImpactReport> {
        anyhow::ensure!(
            summary.right.fingerprint == target.metadata.fingerprint
                && summary.right.rules == target.metadata.rules
                && summary.right.filter == target.metadata.filter,
            "Impact baseline changed; regenerate the release summary first"
        );
        const VERSION: &str = "literal-lines-v1";
        if self.version != VERSION {
            self.cache.clear();
            self.version = VERSION;
        }
        let active: BTreeSet<_> = target.files.values().map(|f| f.digest.clone()).collect();
        self.cache.retain(|digest, _| active.contains(digest));
        let mut patterns: BTreeMap<String, Vec<(&super::baseline::ReleaseChange, &'static str)>> =
            BTreeMap::new();
        for change in &summary.changes {
            analysis::check_cancel(cancel)?;
            if matches!(change.category, Category::Unchanged) || change.ignored_by_text_rules {
                continue;
            }
            let mut local = BTreeMap::new();
            local.insert(change.path.clone(), "literal_path_reference");
            if let Some(name) = std::path::Path::new(&change.path)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| s.len() >= 4)
            {
                local.insert(name.into(), "filename_reference");
            }
            if let Some(semantic) = &change.semantic {
                for key in &semantic.changes {
                    if let Some(raw) = key
                        .path
                        .strip_prefix("$[")
                        .and_then(|p| p.strip_suffix(']'))
                    {
                        if let Ok(key) = serde_json::from_str::<String>(raw) {
                            if key.len() >= 3 {
                                local.insert(key, "config_key_occurrence");
                            }
                        }
                    }
                }
            }
            for (needle, relation) in local {
                patterns.entry(needle).or_default().push((change, relation));
            }
        }
        if patterns.keys().map(String::len).sum::<usize>() > 1 << 20 {
            bail!("Impact pattern budget exceeds 1 MiB; narrow the scope");
        }
        let mut excluded = target.excluded.clone();
        let mut findings = Vec::new();
        if !patterns.is_empty() {
            // Multi-pattern search keeps runtime linear in indexed text instead of rescanning
            // the entire project for each changed path. Literal boundary checks stay identical.
            let rules: Vec<_> = patterns.into_iter().collect();
            let matcher = aho_corasick::AhoCorasick::builder()
                .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
                .build(rules.iter().map(|(pattern, _)| pattern))?;
            if matcher.memory_usage() > 32 << 20 {
                bail!("Impact matcher exceeds 32 MiB; narrow the scope");
            }
            let by_digest: BTreeMap<_, _> = target
                .files
                .values()
                .map(|f| (f.digest.as_str(), f))
                .collect();
            let mut selected = BTreeSet::new();
            let mut unsupported = BTreeMap::new();
            let (mut line_count, mut footprint) = (0usize, 0usize);
            for (digest, file) in by_digest {
                analysis::check_cancel(cancel)?;
                let mut candidate = None;
                if !self.cache.contains_key(digest) {
                    if super::text::looks_binary(&file.bytes) {
                        unsupported.insert(
                            digest.to_owned(),
                            "Binary content was not indexed".to_string(),
                        );
                        continue;
                    }
                    let source = super::text::decode(&file.bytes, None).0;
                    if source.len() as u64 > analysis::MAX_FILE
                        || super::text::lines(&source).count() > 100_000
                    {
                        unsupported.insert(
                            digest.to_owned(),
                            "Decoded text exceeds 16 MiB or 100,000 lines; references not assessed"
                                .into(),
                        );
                        continue;
                    }
                    let long_lines = super::text::lines(&source).any(|(line, _)| line.len() > 4096);
                    let mut lines: Vec<_> = super::text::lines(&source)
                        .enumerate()
                        .filter(|(_, (line, _))| line.len() <= 4096)
                        .map(|(i, (line, _))| (i + 1, line.to_owned()))
                        .collect();
                    lines.shrink_to_fit();
                    let footprint = lines
                        .iter()
                        .map(|(_, line)| line.len() + std::mem::size_of::<(usize, String)>())
                        .sum();
                    candidate = Some(CacheEntry {
                        lines,
                        footprint,
                        long_lines,
                    });
                }
                let entry = self.cache.get(digest).or(candidate.as_ref()).unwrap();
                if line_count + entry.lines.len() > 1_000_000
                    || footprint + entry.footprint > analysis::MAX_TOTAL as usize
                {
                    unsupported.insert(
                        digest.to_owned(),
                        "Index exceeds 1,000,000 lines or 256 MiB; references not assessed".into(),
                    );
                    continue;
                }
                line_count += entry.lines.len();
                footprint += entry.footprint;
                if entry.long_lines {
                    unsupported.insert(
                        digest.to_owned(),
                        "Lines longer than 4 KiB were not assessed".into(),
                    );
                }
                selected.insert(digest.to_owned());
                if let Some(entry) = candidate {
                    self.cache.insert(digest.to_owned(), entry);
                }
            }
            self.cache.retain(|digest, _| selected.contains(digest));
            let mut match_steps = 0usize;
            for (path, file) in &target.files {
                analysis::check_cancel(cancel)?;
                if let Some(reason) = unsupported.get(&file.digest) {
                    excluded.insert(path.clone(), reason.clone());
                }
                let Some(entry) = self.cache.get(&file.digest) else {
                    continue;
                };
                for (line, source) in &entry.lines {
                    analysis::check_cancel(cancel)?;
                    let mut seen = BTreeSet::new();
                    for matched in matcher.find_overlapping_iter(source) {
                        match_steps += 1;
                        if match_steps.is_multiple_of(1024) {
                            analysis::check_cancel(cancel)?;
                        }
                        if match_steps > 10_000_000 {
                            bail!("Impact reference matching budget exceeded; narrow the scope");
                        }
                        let boundary =
                            |c: char| !(c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'));
                        if !source[..matched.start()]
                            .chars()
                            .next_back()
                            .is_none_or(|c| c == '/' || boundary(c))
                            || !source[matched.end()..].chars().next().is_none_or(boundary)
                        {
                            continue;
                        }
                        let (needle, origins) = &rules[matched.pattern().as_usize()];
                        for &(change, relation) in origins {
                            if path == &change.path
                                || !seen.insert((change.id.as_str(), relation, needle.as_str()))
                            {
                                continue;
                            }
                            if findings.len() >= 2000 {
                                bail!("Impact exceeds 2,000 findings; narrow the scope");
                            }
                            let rule_id = format!("literal-reference-v1:{relation}");
                            findings.push(ImpactFinding { id: analysis::identity(&(&change.id, path, line, &file.digest, needle, &rule_id)), origin_change_id: change.id.clone(), related_paths: vec![path.clone()], relation_type: relation.into(), evidence: Evidence { path: path.clone(), line: *line, excerpt: source[source[..matched.start()].char_indices().rev().nth(80).map_or(0, |(at,_)| at)..].chars().take(240).collect(), digest: file.digest.clone() }, severity: "review".into(), confidence: "heuristic".into(), rule_id, limitations: "A delimited name/path occurrence is evidence of text only; it does not prove a dependency or runtime/build/test failure. Inspect or dismiss it manually.".into() });
                        }
                    }
                }
            }
        }
        findings.sort_by(|a, b| {
            (
                &a.origin_change_id,
                &a.evidence.path,
                a.evidence.line,
                &a.rule_id,
            )
                .cmp(&(
                    &b.origin_change_id,
                    &b.evidence.path,
                    b.evidence.line,
                    &b.rule_id,
                ))
        });
        let assessment = if findings.is_empty() {
            "No supported references found; impact is not assessed"
        } else {
            "Potential references found; all findings require review"
        }
        .into();
        Ok(ImpactReport {
            version: 1,
            baseline_fingerprint: target.metadata.fingerprint.clone(),
            findings,
            assessment,
            excluded,
        })
    }
}
