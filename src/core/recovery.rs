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
/// A snapshot without a lock file (see [`lock`]) not written for this long belongs to a
/// program that is no longer running.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(120);

const PREFIX: &str = "session-";
const SUFFIX: &str = ".json";
/// Beside a snapshot, the file its program holds locked for as long as it runs.
const LOCK: &str = ".lock";
/// What [`super::fsutil::write_atomic_with`] names a file being written.
const WRITING: &str = ".oxidemerge-";

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
            .paths()
            .iter()
            .enumerate()
            .map(|(i, p)| self.sources.get(i).copied().flatten() != Fingerprint::of(p))
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

/// The folder of the snapshots, beside the settings file (next to the program in portable
/// mode).
pub fn dir() -> Option<PathBuf> {
    crate::config::config_path().and_then(|p| p.parent().map(|d| d.join("recovery")))
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

fn lock_path(snapshot: &Path) -> PathBuf {
    let mut name = snapshot.as_os_str().to_owned();
    name.push(LOCK);
    PathBuf::from(name)
}

/// Locks the lock file of snapshot `name` in `dir`, for this program to hold as long as it
/// runs: while it is held, no other program takes the snapshot for one left behind, however
/// long ago it was written. `None` where it cannot be locked; age alone tells then.
pub fn lock(dir: &Path, name: &str) -> Option<std::fs::File> {
    make_private(dir).ok()?;
    let file = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(&dir.join(name)))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Whether a running program holds the lock of `snapshot`: `Some(true)` if so, `Some(false)`
/// if its lock file is there but no longer held, `None` if it has none.
fn held(snapshot: &Path) -> Option<bool> {
    let file = std::fs::File::options()
        .write(true)
        .open(lock_path(snapshot))
        .ok()?;
    Some(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ))
}

/// Fills in each tab's [`RecoveredTab::sources`] still to be read, off the window's thread:
/// on a network share reading a file's details can take a while.
fn fingerprint(snapshot: &mut Snapshot) {
    for t in &mut snapshot.tabs {
        if t.sources.is_empty() {
            t.sources = t.tab.paths().iter().map(|p| Fingerprint::of(p)).collect();
        }
    }
}

/// Writes `snapshot` as `name` in `dir`, whole or not at all, then deletes the oldest snapshots
/// of other runs past [`KEEP`].
pub fn write(dir: &Path, name: &str, mut snapshot: Snapshot) -> Result<()> {
    make_private(dir)?;
    fingerprint(&mut snapshot);
    let text = serde_json::to_vec(&snapshot)?;
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

/// Writes `snapshot`, what is left of one left behind, back as `name` in `dir` to offer again:
/// beside a lock file no one holds, so the next start takes it for left behind at once rather
/// than [`ABANDONED_AFTER`] after this write.
pub fn keep(dir: &Path, name: &str, snapshot: Snapshot) -> Result<()> {
    write(dir, name, snapshot)?;
    drop(lock(dir, name));
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

/// Deletes the oldest snapshots of programs no longer running past [`KEEP`], and what crashed
/// programs left beside them: lock files without a snapshot, and files half written.
fn prune(dir: &Path, own: &str) {
    for (path, _) in others(dir, own)
        .into_iter()
        .filter(|(p, _)| held(p) != Some(true))
        .skip(KEEP)
    {
        forget(&path);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let path = e.path();
        let stale = match name.strip_suffix(LOCK) {
            Some(snapshot) => {
                is_snapshot(snapshot)
                    && !dir.join(snapshot).exists()
                    && held(&dir.join(snapshot)) == Some(false)
            }
            None => {
                name.starts_with(WRITING)
                    && e.metadata().and_then(|m| m.modified()).is_ok_and(|at| {
                        now.duration_since(at).unwrap_or_default() >= ABANDONED_AFTER
                    })
            }
        };
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A snapshot left behind by a program that did not close normally.
#[derive(Debug)]
pub struct Found {
    pub path: PathBuf,
    /// The snapshot, or why it cannot be used: unreadable, or made by a later version.
    pub snapshot: Result<Snapshot, String>,
}

/// The snapshots in `dir` of programs no longer running, other than `own`, newest first: whose
/// lock is no longer held or, without a lock file, not written for [`ABANDONED_AFTER`] before
/// `now`.
pub fn abandoned(dir: &Path, own: &str, now: SystemTime) -> Vec<Found> {
    others(dir, own)
        .into_iter()
        .filter(|(path, at)| match held(path) {
            Some(held) => !held,
            None => now.duration_since(*at).unwrap_or_default() >= ABANDONED_AFTER,
        })
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

/// Deletes this run's own snapshot while it runs, keeping its lock.
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Deletes a snapshot and its lock file: discarded, restored, or this run's own at a normal
/// exit, once its lock is let go.
pub fn forget(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(lock_path(path));
}

/// Deletes every snapshot of a program no longer running, for recovery turned off: unsaved
/// text is then not kept on disk at all.
pub fn forget_abandoned(dir: &Path, own: &str) {
    for found in abandoned(dir, own, SystemTime::now() + ABANDONED_AFTER) {
        forget(&found.path);
    }
    prune(dir, own);
}

/// Writes one program's snapshot on other threads, in the order asked: a write that would
/// finish after a later write, or after the snapshot was removed, is dropped.
#[derive(Default)]
pub struct Writer {
    asked: u64,
    done: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl Writer {
    pub fn write(&mut self, dir: PathBuf, name: String, snapshot: Snapshot) {
        std::thread::spawn(self.queue(dir, name, snapshot));
    }

    /// The write of `snapshot`, in its place in the order asked, to run when it will.
    fn queue(
        &mut self,
        dir: PathBuf,
        name: String,
        snapshot: Snapshot,
    ) -> impl FnOnce() + Send + 'static {
        self.asked += 1;
        let (at, done) = (self.asked, self.done.clone());
        move || {
            let mut done = done.lock().unwrap_or_else(|e| e.into_inner());
            if *done < at {
                let _ = write(&dir, &name, snapshot);
                *done = at;
            }
        }
    }

    /// Removes the snapshot at `path` once any write under way has finished, and keeps writes
    /// asked for before from bringing it back. With `lock`, its lock file goes too.
    pub fn remove(&mut self, path: &Path, lock: bool) {
        self.asked += 1;
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        *done = self.asked;
        if lock {
            forget(path);
        } else {
            remove(path);
        }
    }
}

/// Whether a side's unsaved text of about `len` bytes still fits in the [`MAX_TEXT`] a snapshot
/// keeps, `used` already; if so, it counts towards `used`. Asked before the text is made, so a
/// side too large is never copied.
pub fn within_budget(len: usize, used: &mut usize) -> bool {
    let fits = used.saturating_add(len) <= MAX_TEXT;
    if fits {
        *used += len;
    }
    fits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(paths: &[&Path], unsaved: Vec<Option<String>>) -> RecoveredTab {
        RecoveredTab {
            tab: SessionTab::new(
                &paths.iter().map(|p| p.to_path_buf()).collect::<Vec<_>>(),
                Some("text".into()),
                None,
            ),
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
        write(&dir, "session-1-1.json", snap.clone()).unwrap();
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
            write(dir, &name, Snapshot::new(Vec::new(), 0, 0)).unwrap();
            age(&dir.join(&name), Duration::from_secs(1000 - i as u64));
        }
        write(dir, "session-own-0.json", Snapshot::new(Vec::new(), 0, 0)).unwrap();
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
        assert!(within_budget(5, &mut used));
        assert!(!within_budget(13, &mut used));
        assert_eq!(used, MAX_TEXT - 5);
        assert!(within_budget(5, &mut used));
        assert!(!within_budget(usize::MAX, &mut used));
    }

    #[test]
    fn a_held_lock_keeps_a_snapshot_however_old_and_a_dropped_one_offers_it_at_once() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let name = "session-1-1.json";
        let held = lock(dir, name).unwrap();
        write(dir, name, Snapshot::new(Vec::new(), 0, 0)).unwrap();
        age(&dir.join(name), ABANDONED_AFTER * 2);
        assert!(abandoned(dir, "x", SystemTime::now()).is_empty());
        forget_abandoned(dir, "x");
        assert!(
            dir.join(name).exists(),
            "a running program's snapshot is kept"
        );

        write(dir, name, Snapshot::new(Vec::new(), 0, 0)).unwrap();
        drop(held);
        assert_eq!(abandoned(dir, "x", SystemTime::now()).len(), 1);
        forget(&dir.join(name));
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);

        keep(dir, "session-2-2.json", Snapshot::new(Vec::new(), 0, 0)).unwrap();
        assert_eq!(abandoned(dir, "x", SystemTime::now()).len(), 1);
    }

    #[test]
    fn the_writer_keeps_the_last_write_and_a_removal_beats_writes_under_way() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let path = dir.join("session-1-1.json");
        let mut w = Writer::default();
        let queue = |w: &mut Writer, left_out| {
            w.queue(
                dir.clone(),
                "session-1-1.json".into(),
                Snapshot::new(Vec::new(), 0, left_out),
            )
        };
        let (first, second) = (queue(&mut w, 1), queue(&mut w, 2));
        second();
        first();
        assert_eq!(read(&path).unwrap().left_out, 2, "the later write stays");

        let late = queue(&mut w, 3);
        w.remove(&path, false);
        late();
        assert!(
            !path.exists(),
            "a write asked before the removal never brings it back"
        );

        w.write(
            dir.clone(),
            "session-1-1.json".into(),
            Snapshot::new(Vec::new(), 0, 4),
        );
        let start = std::time::Instant::now();
        while *w.done.lock().unwrap() < w.asked {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            read(&path).unwrap().left_out,
            4,
            "written on another thread"
        );
    }

    #[test]
    fn pruning_removes_stale_lock_files_and_old_half_written_files() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let stale = dir.join("session-1-1.json.lock");
        std::fs::write(&stale, "").unwrap();
        let _running = lock(dir, "session-2-2.json").unwrap();
        let (old, new) = (dir.join(".oxidemerge-old"), dir.join(".oxidemerge-new"));
        std::fs::write(&old, "half").unwrap();
        std::fs::write(&new, "half").unwrap();
        age(&old, ABANDONED_AFTER * 2);
        write(dir, "session-own-0.json", Snapshot::new(Vec::new(), 0, 0)).unwrap();
        assert!(!stale.exists());
        assert!(dir.join("session-2-2.json.lock").exists(), "still held");
        assert!(!old.exists());
        assert!(new.exists(), "possibly still being written");
    }
}
