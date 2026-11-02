//! Additive portable review state. Names are annotations, not authenticated identities.
use super::{analysis, diff::DiffOptions};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Unreviewed,
    InReview,
    Accepted,
    NeedsAction,
    Resolved,
}
impl State {
    pub const ALL: [Self; 5] = [
        Self::Unreviewed,
        Self::InReview,
        Self::Accepted,
        Self::NeedsAction,
        Self::Resolved,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Unreviewed => "workspace.unreviewed",
            Self::InReview => "workspace.in_review",
            Self::Accepted => "workspace.accepted",
            Self::NeedsAction => "workspace.needs_action",
            Self::Resolved => "workspace.resolved",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub current_hunk: Option<String>,
    pub id: String,
    pub fingerprint: String,
    pub state: State,
    pub reviewer: String,
    pub comment: String,
    pub rationale: String,
    pub tags: Vec<String>,
    pub stale: bool,
    pub hunks: BTreeMap<String, Box<Decision>>,
    /// What the compared files held, wherever they are: finds the decision again after a rename.
    pub content: String,
    /// The path the decision was made on, when it followed the content to another path.
    pub renamed_from: Option<String>,
    /// Several paths hold that content: the decision was copied to each, to be reviewed again.
    pub ambiguous_rename: bool,
}

/// Fingerprints of one compared path: with the path (`fingerprint`) and without (`content`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prints {
    pub fingerprint: String,
    pub content: String,
}
impl Prints {
    /// A path that exists but could not be read: its decision stays where it is, stale, and no
    /// decision moves to it.
    pub fn unreadable() -> Prints {
        Prints::default()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub policy: String,
    pub entries: BTreeMap<String, Decision>,
    pub current: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub policy: String,
    pub version: u32,
    pub total: usize,
    pub unreviewed: usize,
    pub in_review: usize,
    pub accepted: usize,
    pub needs_action: usize,
    pub resolved: usize,
    pub stale: usize,
    pub entries: BTreeMap<String, Decision>,
    pub provenance: String,
}
impl Workspace {
    pub fn bind_policy(
        &mut self,
        options: &DiffOptions,
        folder: &super::folder::FolderOptions,
        filter: &str,
    ) -> bool {
        let expected = policy_digest(options, folder, filter);
        if self.policy == expected {
            return false;
        }
        let stale = !self.entries.is_empty();
        for decision in self.entries.values_mut() {
            decision.stale = true;
            for hunk in decision.hunks.values_mut() {
                hunk.stale = true;
            }
        }
        self.policy = expected;
        stale
    }
    pub fn validate(&self) -> Result<()> {
        if self.policy.len() > 128 || self.entries.len() > analysis::MAX_FILES {
            bail!("Too many review decisions");
        }
        for (path, d) in &self.entries {
            if !super::review::safe_path(path) {
                bail!("Invalid review decision path");
            }
            validate_decision(d, 0)?;
        }
        if self
            .current
            .as_deref()
            .is_some_and(|p| !super::review::safe_path(p))
        {
            bail!("Invalid review cursor path");
        }
        Ok(())
    }
    pub fn summary(&self, paths: impl IntoIterator<Item = String>) -> Summary {
        let entries: BTreeMap<_, _> = paths
            .into_iter()
            .map(|p| {
                let d = self.entries.get(&p).cloned().unwrap_or_default();
                (p, d)
            })
            .collect();
        let mut s = Summary { policy: self.policy.clone(), version: 1, total: entries.len(), unreviewed: 0, in_review: 0, accepted: 0, needs_action: 0, resolved: 0, stale: 0, entries, provenance: "Captured review annotations; reviewer names are free-form, not authenticated. Stale decisions require renewed review.".into() };
        for d in s.entries.values() {
            if d.stale {
                s.stale += 1;
                s.needs_action += 1;
                continue;
            }
            match d.state {
                State::Unreviewed => s.unreviewed += 1,
                State::InReview => s.in_review += 1,
                State::Accepted => s.accepted += 1,
                State::NeedsAction => s.needs_action += 1,
                State::Resolved => s.resolved += 1,
            }
        }
        s
    }
    /// Rebase never transfers an accepted state to a different fingerprint automatically. A
    /// decision whose path is gone follows its content to the one new path holding exactly it;
    /// when several do, each gets a stale copy flagged for review instead. When several gone paths
    /// held that content, the first one's decision moves, flagged, and the others stay put.
    pub fn reconcile(&mut self, prints: &BTreeMap<String, Prints>) {
        let mut undecided: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
        for (path, p) in prints {
            if !self.entries.contains_key(path) && !p.content.is_empty() {
                undecided.entry(p.content.as_str()).or_default().push(path);
            }
        }
        let mut orphans: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, d) in &self.entries {
            if !prints.contains_key(path) && undecided.contains_key(d.content.as_str()) {
                orphans
                    .entry(d.content.clone())
                    .or_default()
                    .push(path.clone());
            }
        }
        for (content, olds) in orphans {
            let targets = &undecided[content.as_str()];
            let ambiguous = olds.len() > 1 || targets.len() > 1;
            let old = &olds[0];
            let Some(decision) = self.entries.remove(old) else {
                continue;
            };
            for &new in targets {
                let mut d = decision.clone();
                d.fingerprint = prints[new].fingerprint.clone();
                d.renamed_from = Some(old.clone());
                d.ambiguous_rename = ambiguous;
                if ambiguous {
                    d.stale = true;
                    for h in d.hunks.values_mut() {
                        h.stale = true;
                    }
                }
                if self.current.as_deref() == Some(old.as_str()) {
                    self.current = Some(new.clone());
                }
                self.entries.insert(new.clone(), d);
            }
        }
        for (path, d) in &mut self.entries {
            if prints.get(path).map(|p| &p.fingerprint) != Some(&d.fingerprint) {
                d.stale = true;
                for h in d.hunks.values_mut() {
                    h.stale = true;
                }
            }
        }
    }
}
fn validate_decision(d: &Decision, depth: usize) -> Result<()> {
    if depth > 1
        || d.hunks.len() > 1000
        || d.comment.len() > 16_384
        || d.rationale.len() > 16_384
        || d.reviewer.len() > 256
        || d.tags.len() > 32
        || d.tags.iter().any(|t| t.len() > 256)
        || d.id.len() > 128
        || d.fingerprint.len() > 128
        || d.current_hunk.as_ref().is_some_and(|id| id.len() > 128)
        || d.content.len() > 128
    {
        bail!("Review annotation limits exceeded");
    }
    if d.renamed_from
        .as_deref()
        .is_some_and(|p| !super::review::safe_path(p))
    {
        bail!("Invalid review decision path");
    }
    for h in d.hunks.values() {
        validate_decision(h, depth + 1)?;
    }
    Ok(())
}
pub fn policy_digest(
    options: &DiffOptions,
    folder: &super::folder::FolderOptions,
    filter: &str,
) -> String {
    analysis::identity(&(options, folder, filter))
}
pub fn prints(path: &str, roots: &[PathBuf], options: &DiffOptions) -> Result<Prints> {
    let mut sides = Vec::new();
    for root in roots {
        let file = root.join(path);
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => sides.push(Some(
                analysis::digest(&analysis::read_in_root(&file, &root.canonicalize()?)?),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => sides.push(None),
            _ => bail!("Review state supports regular files only: {path}"),
        }
    }
    Ok(prints_of_digests(path, &sides, options))
}
pub fn fingerprint_digests(path: &str, sides: &[Option<String>], options: &DiffOptions) -> String {
    let converter = super::unpacker::find(&options.unpackers, std::path::Path::new(path));
    analysis::identity(&(path, sides, options, converter))
}
pub fn prints_of_digests(path: &str, sides: &[Option<String>], options: &DiffOptions) -> Prints {
    let converter = super::unpacker::find(&options.unpackers, std::path::Path::new(path));
    Prints {
        fingerprint: fingerprint_digests(path, sides, options),
        content: analysis::identity(&(sides, options, converter.map(|c| &c.command))),
    }
}
/// Opt-in additive annotation: comparison tables and CSV columns retain their old shape.
pub fn annotate_report(
    mut content: String,
    format: super::report::ReportFormat,
    summary: &Summary,
) -> String {
    use super::report::ReportFormat;
    let json = serde_json::to_string_pretty(summary).expect("review summary serializes");
    let escaped = super::xml_report::esc(&json);
    match format {
        ReportFormat::Html | ReportFormat::Slideshow => {
            let section = format!("<aside class=\"review-summary\"><h2>Review workspace</h2><pre>{escaped}</pre></aside>");
            let at = content.rfind("</body>").unwrap_or(content.len());
            content.insert_str(at, &section);
        }
        ReportFormat::Xml => {
            let at = content.rfind("</comparison>").unwrap_or(content.len());
            content.insert_str(
                at,
                &format!("<review-summary version=\"1\"><json>{escaped}</json></review-summary>"),
            );
        }
        ReportFormat::Text => {
            content.push_str("\nReview workspace (format 1)\n");
            content.push_str(&json);
        }
        _ => {}
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_policy_changes_invalidate_decisions_and_converter_identity_is_scoped() {
        let mut options = DiffOptions::default();
        let folder = super::super::folder::FolderOptions::default();
        let mut workspace = Workspace::default();
        workspace.bind_policy(&options, &folder, "*");
        workspace.entries.insert(
            "a.rs".into(),
            Decision {
                state: State::Accepted,
                ..Default::default()
            },
        );
        assert!(!workspace.bind_policy(&options, &folder, "*"));
        let changed = super::super::folder::FolderOptions {
            method: super::super::folder::CompareMethod::Size,
            ..folder
        };
        assert!(workspace.bind_policy(&options, &changed, "*"));
        assert_eq!(workspace.summary(["a.rs".into()]).accepted, 0);
        let sides = [Some("content".into()), Some("other".into())];
        let plain = fingerprint_digests("a.rs", &sides, &options);
        options.unpackers.push(super::super::unpacker::Unpacker {
            enabled: true,
            pattern: "*.pdf".into(),
            command: "never executed ${file}".into(),
        });
        assert_eq!(plain, fingerprint_digests("a.rs", &sides, &options));
        let converted = fingerprint_digests("a.pdf", &sides, &options);
        options.unpackers.clear();
        assert_ne!(converted, fingerprint_digests("a.pdf", &sides, &options));
    }

    #[test]
    fn decisions_follow_renamed_content_and_ambiguous_renames_are_flagged() {
        let accepted = |content: &str| Decision {
            fingerprint: format!("old-{content}"),
            content: content.into(),
            state: State::Accepted,
            comment: "looked fine".into(),
            ..Default::default()
        };
        let prints = |entries: &[(&str, &str)]| -> BTreeMap<String, Prints> {
            entries
                .iter()
                .map(|(path, content)| {
                    let p = Prints {
                        fingerprint: format!("{path}-{content}"),
                        content: (*content).into(),
                    };
                    ((*path).into(), p)
                })
                .collect()
        };

        // One new path holds exactly the content: the decision moves there, still accepted.
        let mut w = Workspace {
            current: Some("old.txt".into()),
            ..Default::default()
        };
        w.entries.insert("old.txt".into(), accepted("C"));
        w.reconcile(&prints(&[("new.txt", "C"), ("other.txt", "D")]));
        let d = &w.entries["new.txt"];
        assert!(!w.entries.contains_key("old.txt"));
        assert_eq!(
            (d.state, d.stale, d.ambiguous_rename),
            (State::Accepted, false, false)
        );
        assert_eq!(d.renamed_from.as_deref(), Some("old.txt"));
        assert_eq!(d.fingerprint, "new.txt-C");
        assert_eq!(d.comment, "looked fine");
        assert_eq!(w.current.as_deref(), Some("new.txt"));
        assert_eq!(w.summary(["new.txt".into()]).accepted, 1);
        w.validate().unwrap();

        // Two paths hold it: each gets a stale copy to review again.
        let mut w = Workspace::default();
        w.entries.insert("old.txt".into(), accepted("C"));
        w.reconcile(&prints(&[("a/x.txt", "C"), ("b/x.txt", "C")]));
        for path in ["a/x.txt", "b/x.txt"] {
            let d = &w.entries[path];
            assert!(d.stale && d.ambiguous_rename, "{path}");
            assert_eq!(d.renamed_from.as_deref(), Some("old.txt"));
        }
        assert_eq!(w.summary(["a/x.txt".into(), "b/x.txt".into()]).accepted, 0);

        // A path that could not be read keeps its decision, stale; nothing moves to or from it.
        let mut w = Workspace::default();
        w.entries.insert("old.txt".into(), accepted("C"));
        let mut read = prints(&[("copy.txt", "C")]);
        read.insert("old.txt".into(), Prints::unreadable());
        read.insert("locked.txt".into(), Prints::unreadable());
        w.entries.insert("gone.txt".into(), Decision::default());
        w.reconcile(&read);
        assert!(w.entries["old.txt"].stale);
        assert!(!w.entries.contains_key("copy.txt"));
        assert!(!w.entries.contains_key("locked.txt"));

        // Two gone paths held the same content: neither claim is trusted.
        let mut w = Workspace::default();
        w.entries.insert("one.txt".into(), accepted("C"));
        w.entries.insert("two.txt".into(), accepted("C"));
        w.entries.get_mut("two.txt").unwrap().comment = "second opinion".into();
        w.reconcile(&prints(&[("moved.txt", "C")]));
        assert!(w.entries["moved.txt"].stale && w.entries["moved.txt"].ambiguous_rename);
        assert_eq!(
            w.entries["moved.txt"].renamed_from.as_deref(),
            Some("one.txt")
        );
        assert!(!w.entries.contains_key("one.txt"));
        let kept = &w.entries["two.txt"];
        assert!(kept.stale && kept.comment == "second opinion");

        // Renamed and edited, decided without content, or onto a path already decided: no move.
        let mut w = Workspace::default();
        w.entries.insert("edited.txt".into(), accepted("C"));
        w.entries.insert(
            "legacy.txt".into(),
            Decision {
                content: String::new(),
                ..accepted("L")
            },
        );
        w.entries.insert("gone.txt".into(), accepted("K"));
        w.entries.insert(
            "kept.txt".into(),
            Decision {
                fingerprint: "kept.txt-K".into(),
                ..accepted("K")
            },
        );
        w.reconcile(&prints(&[
            ("renamed.txt", "C2"),
            ("legacy2.txt", "L"),
            ("kept.txt", "K"),
        ]));
        assert!(w.entries["edited.txt"].stale && w.entries["legacy.txt"].stale);
        assert!(!w.entries.contains_key("renamed.txt") && !w.entries.contains_key("legacy2.txt"));
        assert!(w.entries["gone.txt"].stale);
        assert_eq!(w.entries["kept.txt"].renamed_from, None);
        assert!(!w.entries["kept.txt"].stale);

        w.entries.get_mut("kept.txt").unwrap().renamed_from = Some("../escape".into());
        assert!(w.validate().is_err());
    }

    #[test]
    fn content_prints_ignore_the_path_but_not_the_bytes_or_rules() {
        let sides = [Some("a".into()), Some("b".into())];
        let options = DiffOptions::default();
        let here = prints_of_digests("x/a.txt", &sides, &options);
        let there = prints_of_digests("y/b.txt", &sides, &options);
        assert_eq!(here.content, there.content);
        assert_ne!(here.fingerprint, there.fingerprint);
        let edited = prints_of_digests("x/a.txt", &[Some("a".into()), Some("c".into())], &options);
        assert_ne!(here.content, edited.content);
        let strict = DiffOptions {
            ignore_case: !options.ignore_case,
            ..options
        };
        assert_ne!(
            here.content,
            prints_of_digests("x/a.txt", &sides, &strict).content
        );
    }

    #[test]
    fn stale_decisions_are_never_silently_reaccepted() {
        let mut w = Workspace::default();
        w.entries.insert(
            "a.txt".into(),
            Decision {
                fingerprint: "old".into(),
                state: State::Accepted,
                ..Default::default()
            },
        );
        w.reconcile(&BTreeMap::from([(
            "a.txt".into(),
            Prints {
                fingerprint: "new".into(),
                content: "new".into(),
            },
        )]));
        let s = w.summary(["a.txt".into(), "b.txt".into()]);
        assert_eq!((s.total, s.accepted, s.unreviewed, s.stale), (2, 0, 1, 1));
        w.entries.get_mut("a.txt").unwrap().comment = "x".repeat(16_385);
        assert!(w.validate().is_err());
    }
}
