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
#[derive(Default)]
pub struct Index {
    cache: BTreeMap<String, Vec<(usize, String)>>,
}
impl Index {
    pub fn analyze(
        &mut self,
        summary: &ReleaseSummary,
        target: &Capture,
        cancel: &AtomicBool,
    ) -> Result<ImpactReport> {
        let active: BTreeSet<_> = target.files.values().map(|f| f.digest.clone()).collect();
        self.cache.retain(|digest, _| active.contains(digest));
        for f in target.files.values() {
            analysis::check_cancel(cancel)?;
            if super::text::looks_binary(&f.bytes) || self.cache.contains_key(&f.digest) {
                continue;
            }
            let text = super::text::decode(&f.bytes, None).0;
            let lines: Vec<_> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| l.len() <= 4096)
                .map(|(i, line)| (i + 1, line.to_owned()))
                .collect();
            self.cache.insert(f.digest.clone(), lines);
        }
        let mut findings = Vec::new();
        for change in &summary.changes {
            analysis::check_cancel(cancel)?;
            if matches!(change.category, Category::Unchanged) {
                continue;
            }
            let mut needles = BTreeMap::new();
            needles.insert(change.path.clone(), "literal_path_reference");
            if let Some(name) = std::path::Path::new(&change.path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| n.len() >= 4)
            {
                needles.insert(name.into(), "filename_reference");
            }
            if let Some(semantic) = &change.semantic {
                for key in &semantic.changes {
                    // Top-level JSON keys only; nested/array/unsupported paths remain unassessed.
                    if let Some(raw) = key
                        .path
                        .strip_prefix("$[")
                        .and_then(|p| p.strip_suffix(']'))
                    {
                        if let Ok(key) = serde_json::from_str::<String>(raw) {
                            if key.len() >= 3 {
                                needles.insert(key, "config_key_occurrence");
                            }
                        }
                    }
                }
            }
            for (path, file) in &target.files {
                if path == &change.path {
                    continue;
                }
                let Some(lines) = self.cache.get(&file.digest) else {
                    continue;
                };
                for (line, source) in lines {
                    analysis::check_cancel(cancel)?;
                    for (needle, relation) in &needles {
                        // Require a delimited token, not arbitrary substring overlap.
                        let matched = source.match_indices(needle.as_str()).any(|(at, _)| {
                            let boundary = |c: char| {
                                !(c.is_alphanumeric()
                                    || c == '_'
                                    || c == '/'
                                    || c == '.'
                                    || c == '-')
                            };
                            source[..at].chars().next_back().is_none_or(boundary)
                                && source[at + needle.len()..]
                                    .chars()
                                    .next()
                                    .is_none_or(boundary)
                        });
                        if !matched {
                            continue;
                        }
                        if findings.len() >= 2000 {
                            bail!("Impact exceeds 2,000 findings; narrow the scope");
                        }
                        let rule_id = format!("literal-reference-v1:{relation}");
                        findings.push(ImpactFinding { id: analysis::identity(&(&change.id, path, line, &file.digest, needle)), origin_change_id: change.id.clone(), related_paths: vec![path.clone()], relation_type: (*relation).into(), evidence: Evidence { path: path.clone(), line: *line, excerpt: source.chars().take(240).collect(), digest: file.digest.clone() }, severity: "review".into(), confidence: "heuristic".into(), rule_id, limitations: "A delimited name/path occurrence is evidence of text only; it does not prove a dependency or runtime/build/test failure. Inspect or dismiss it manually.".into() });
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
            excluded: target.excluded.clone(),
        })
    }
}
