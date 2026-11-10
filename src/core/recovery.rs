//! Recovery snapshots: with recovery turned on, a running OxideMerge keeps a note of its open
//! comparisons, and the text of every side edited but not saved, in a private folder of its
//! settings. A program that closes normally deletes its note; a note left behind is offered at
//! the next start to restore, look at first, or throw away. Restoring opens the tabs and puts
//! the unsaved text back in their sides, never in the files: it is saved only when asked.

use crate::config::SessionTab;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The format of a snapshot; a snapshot of a later one is left alone.
pub const VERSION: u32 = 1;
/// Unsaved text kept in one snapshot at most, all sides together; sides past it are counted
/// in [`Snapshot::left_out`] instead.
pub const MAX_TEXT: usize = 32 << 20;
/// Snapshots of other runs kept at most; the oldest past them are deleted.
pub const KEEP: usize = 5;
/// How often a running program writes its snapshot again, even unchanged.
pub const BEAT: Duration = Duration::from_secs(30);
/// A snapshot not written for this long belongs to a program that is no longer running.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(120);

const PREFIX: &str = "session-";
const SUFFIX: &str = ".json";

/// The size and modification time of a compared file when the snapshot was written; the file
/// is reported changed when either differs at the next start.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct Fingerprint {
    pub len: u64,
    /// Nanoseconds since 1970, when the file system tells.
    pub modified: Option<u128>,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Option<Fingerprint> {
        let m = std::fs::metadata(path).ok()?;
        let modified = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        Some(Fingerprint {
            len: m.len(),
            modified,
        })
    }
}

/// An open comparison: how to open it again, the files as they were, and the text of each
/// side that had unsaved edits.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct RecoveredTab {
    pub tab: SessionTab,
    /// For each path of [`SessionTab::paths`]: the file then, `None` when it could not be read.
    pub sources: Vec<Option<Fingerprint>>,
    /// For each side: its whole text when it had unsaved edits.
    pub unsaved: Vec<Option<String>>,
    /// The difference that was selected.
    pub block: Option<usize>,
    /// The side the caret was in and its line there (from 0).
    pub caret: Option<(usize, usize)>,
    /// For each side, its bookmarks: line (from 0) and name.
    pub bookmarks: Vec<Vec<(usize, String)>>,
}

impl RecoveredTab {
    /// Which of the compared files changed (or appeared, or went away) since the snapshot.
    pub fn changed_sources(&self) -> Vec<bool> {
        self.tab
            .paths
            .iter()
            .enumerate()
            .map(|(i, p)| self.sources.get(i).copied().flatten() != Fingerprint::of(Path::new(p)))
            .collect()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Snapshot {
    pub version: u32,
    /// When it was written, in local time, to show.
    pub saved: String,
    pub tabs: Vec<RecoveredTab>,
    /// The index in `tabs` of the tab that was in front.
    pub active: usize,
    /// Sides whose unsaved text was too much to keep (see [`MAX_TEXT`]).
    pub left_out: usize,
}

impl Snapshot {
    pub fn new(tabs: Vec<RecoveredTab>, active: usize, left_out: usize) -> Snapshot {
        Snapshot {
            version: VERSION,
            saved: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            tabs,
            active,
            left_out,
        }
    }

    pub fn unsaved_sides(&self) -> usize {
        self.tabs
            .iter()
            .map(|t| t.unsaved.iter().flatten().count())
            .sum()
    }
}

/// The folder of the snapshots, in the settings folder.
pub fn dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("recovery"))
}

/// The file name of this run's snapshot: its process and start time, so two programs running
/// at once never share one.
pub fn own_name() -> String {
    let start = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{PREFIX}{}-{start}{SUFFIX}", std::process::id())
}

fn is_snapshot(name: &str) -> bool {
    name.starts_with(PREFIX) && name.ends_with(SUFFIX)
}

/// Makes `dir` readable by its owner only, where the system has such permissions: the text of
/// unsaved edits is kept there.
fn make_private(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| dir.display().to_string())?;
    }
    Ok(())
}

/// Writes `snapshot` as `name` in `dir`, whole or not at all, then deletes the oldest snapshots
/// of other runs past [`KEEP`].
pub fn write(dir: &Path, name: &str, snapshot: &Snapshot) -> Result<()> {
    make_private(dir)?;
    let text = serde_json::to_vec(snapshot)?;
    let path = dir.join(name);
    super::fsutil::write_atomic_with(&path, &text, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    prune(dir, name);
    Ok(())
}

/// The snapshots in `dir` other than `own`, newest first, with when they were last written.
fn others(dir: &Path, own: &str) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            is_snapshot(&name) && name != own
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file()
                .then(|| (e.path(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
    found
}

fn prune(dir: &Path, own: &str) {
    for (path, _) in others(dir, own).into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(path);
    }
}

/// A snapshot left behind by a program that did not close normally.
#[derive(Debug)]
pub struct Found {
    pub path: PathBuf,
    /// The snapshot, or why it cannot be used: unreadable, or made by a later version.
    pub snapshot: Result<Snapshot, String>,
}

/// The snapshots in `dir` that no running program writes any more (none written for
/// [`ABANDONED_AFTER`] before `now`), other than `own`, newest first.
pub fn abandoned(dir: &Path, own: &str, now: SystemTime) -> Vec<Found> {
    others(dir, own)
        .into_iter()
        .filter(|(_, at)| now.duration_since(*at).unwrap_or_default() >= ABANDONED_AFTER)
        .map(|(path, _)| {
            let snapshot = read(&path);
            Found { path, snapshot }
        })
        .collect()
}

fn read(path: &Path) -> Result<Snapshot, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let version = value.get("version").and_then(serde_json::Value::as_u64);
    match version {
        Some(v) if v > u64::from(VERSION) => Err(format!(
            "made by a later version of OxideMerge (format {v})"
        )),
        Some(_) => serde_json::from_value(value).map_err(|e| e.to_string()),
        None => Err("not a recovery snapshot".into()),
    }
}

/// Deletes a snapshot: discarded, restored, or this run's own at a normal exit.
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Keeps the unsaved text of each side while the total stays within [`MAX_TEXT`]; returns the
/// text kept for each side and how many sides were left out.
pub fn within_budget(sides: Vec<Option<String>>, used: &mut usize) -> (Vec<Option<String>>, usize) {
    let mut left_out = 0;
    let kept = sides
        .into_iter()
        .map(|s| {
            s.and_then(|text| {
                if *used + text.len() > MAX_TEXT {
                    left_out += 1;
                    None
                } else {
                    *used += text.len();
                    Some(text)
                }
            })
        })
        .collect();
    (kept, left_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(paths: &[&Path], unsaved: Vec<Option<String>>) -> RecoveredTab {
        RecoveredTab {
            tab: SessionTab {
                paths: paths.iter().map(|p| p.display().to_string()).collect(),
                kind: Some("text".into()),
                rules: None,
            },
            sources: paths.iter().map(|p| Fingerprint::of(p)).collect(),
            unsaved,
            block: Some(1),
            ..Default::default()
        }
    }

    fn age(path: &Path, by: Duration) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn a_snapshot_left_behind_is_found_and_its_changed_files_are_flagged() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.txt"), d.path().join("b.txt"));
        std::fs::write(&a, "one\n").unwrap();
        std::fs::write(&b, "two\n").unwrap();
        let dir = d.path().join("recovery");
        let snap = Snapshot::new(
            vec![tab(&[&a, &b], vec![None, Some("edited\n".into())])],
            0,
            0,
        );
        write(&dir, "session-1-1.json", &snap).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("session-1-1.json")), 0o600);
        }

        // Still being written by a running program: not offered, and never its own.
        assert!(abandoned(&dir, "session-2-2.json", SystemTime::now()).is_empty());
        age(&dir.join("session-1-1.json"), ABANDONED_AFTER * 2);
        assert!(abandoned(&dir, "session-1-1.json", SystemTime::now()).is_empty());
        let found = abandoned(&dir, "session-2-2.json", SystemTime::now());
        assert_eq!(found.len(), 1);
        let back = found[0].snapshot.as_ref().unwrap();
        assert_eq!(back.tabs, snap.tabs);
        assert_eq!(back.unsaved_sides(), 1);
        assert_eq!(back.tabs[0].changed_sources(), [false, false]);

        // A file changed since, by size, and one gone.
        std::fs::write(&b, "two and more\n").unwrap();
        assert_eq!(back.tabs[0].changed_sources(), [false, true]);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(back.tabs[0].changed_sources(), [true, true]);
        // Restoring never wrote to them: b is as the test left it.
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two and more\n");

        remove(&found[0].path);
        assert!(abandoned(&dir, "x", SystemTime::now()).is_empty());
    }

    #[test]
    fn later_formats_and_damaged_files_are_reported_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        std::fs::write(
            dir.join("session-1-1.json"),
            r#"{"version": 99, "tabs": []}"#,
        )
        .unwrap();
        std::fs::write(dir.join("session-2-2.json"), "{not json").unwrap();
        std::fs::write(dir.join("session-3-3.json"), r#"{"version": 1}"#).unwrap();
        std::fs::write(dir.join("notes.txt"), "not ours").unwrap();
        for f in ["session-1-1.json", "session-2-2.json", "session-3-3.json"] {
            age(&dir.join(f), ABANDONED_AFTER * 2);
        }
        let found = abandoned(dir, "own", SystemTime::now());
        assert_eq!(found.len(), 3);
        let by = |n: &str| {
            found
                .iter()
                .find(|f| f.path.ends_with(n))
                .unwrap()
                .snapshot
                .as_ref()
        };
        assert!(by("session-1-1.json")
            .unwrap_err()
            .contains("later version"));
        assert!(by("session-2-2.json").is_err());
        assert_eq!(by("session-3-3.json").unwrap().tabs, []);
        // Nothing was deleted by looking.
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 4);
    }

    #[test]
    fn old_snapshots_past_the_cap_are_deleted_and_unsaved_text_is_capped() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        for i in 0..KEEP + 3 {
            let name = format!("session-{i}-{i}.json");
            write(dir, &name, &Snapshot::new(Vec::new(), 0, 0)).unwrap();
            age(&dir.join(&name), Duration::from_secs(1000 - i as u64));
        }
        write(dir, "session-own-0.json", &Snapshot::new(Vec::new(), 0, 0)).unwrap();
        let left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), KEEP + 1, "{left:?}");
        assert!(left.contains(&"session-own-0.json".to_string()));
        // The newest of the others are kept.
        assert!(left.contains(&format!("session-{}-{}.json", KEEP + 2, KEEP + 2)));
        assert!(!left.contains(&"session-0-0.json".to_string()));

        let mut used = MAX_TEXT - 10;
        let (kept, out) = within_budget(
            vec![Some("12345".into()), None, Some("too much text".into())],
            &mut used,
        );
        assert_eq!(kept, [Some("12345".into()), None, None]);
        assert_eq!(out, 1);
    }
}
