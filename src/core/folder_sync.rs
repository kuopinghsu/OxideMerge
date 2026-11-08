//! Two-way folder synchronization: the destination folder made to match the source folder,
//! or only brought up to date from it. A plan is built from a finished folder compare and
//! does not change once built. It records what each path it touches held, as kind, size,
//! time and SHA-256, and applying it refuses a path changed since, even one edited with its
//! size and time put back. Content decides: a newer time alone never replaces a file.
//!
//! Applying is not atomic across files. Each file is copied to a temporary file beside its
//! destination, checked against the planned fingerprint and then renamed into place, and the
//! file it replaces or deletes is first copied into a recovery journal kept outside both
//! folders. When applying fails or is stopped, what was done is undone in reverse order;
//! when that fails too, or the program ends first, the journal stays and is offered for
//! recovery the next time.
//!
//! A file renamed or moved in the source, whose contents a file the plan deletes still has, is
//! moved in the destination rather than copied again: renamed to a temporary name recorded in
//! the journal first, then to its new name, so a rename is undone even after a crash, and a
//! rename only in case or Unicode form works on file systems that take both for one name.
//!
//! Symbolic links are never followed. They are left alone, unless the plan copies links: then a
//! link is synchronized as the link itself, with the text of its target as its contents, and a
//! link facing a file or folder of the same name is still left alone (Unix only). Other names
//! that may stand for another file on the destination's file system are left alone: those reaching an existing entry
//! under another case or Unicode form, and source names alike but for case or for accents on
//! Latin letters, composed or not.

use super::analysis;
use super::folder::{CmpStatus, DirItem};
use super::folder_merge::escapes;
use super::fsutil;
use super::problem::Problem;
use crate::i18n::{tr, trf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;

/// The version of the plan file format; newer plans are refused.
pub const PLAN_VERSION: u32 = 1;
/// 2 records renames and 3 symbolic links, which older versions would undo wrongly, so they
/// refuse it.
const JOURNAL_VERSION: u32 = 3;
const JOURNAL_FILE: &str = "journal.jsonl";
const HEADER_FILE: &str = "header.json";
/// Deleting at least this many items asks a second time.
const LARGE_DELETION: usize = 50;
/// For reading that is not to be stopped, as when undoing.
static NEVER: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// The destination ends up holding what the source holds: files only in the destination
    /// are deleted.
    Mirror,
    /// Files new or changed in the source are copied; nothing is deleted.
    Update,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SyncPolicy {
    pub mode: SyncMode,
    pub source: Side,
}

impl SyncPolicy {
    pub const ALL: [SyncPolicy; 4] = [
        SyncPolicy {
            mode: SyncMode::Mirror,
            source: Side::Left,
        },
        SyncPolicy {
            mode: SyncMode::Mirror,
            source: Side::Right,
        },
        SyncPolicy {
            mode: SyncMode::Update,
            source: Side::Left,
        },
        SyncPolicy {
            mode: SyncMode::Update,
            source: Side::Right,
        },
    ];

    pub fn source(self) -> usize {
        self.source.index()
    }

    pub fn destination(self) -> usize {
        1 - self.source.index()
    }

    pub fn key(self) -> &'static str {
        match (self.mode, self.source) {
            (SyncMode::Mirror, Side::Left) => "folder_sync.mirror_right",
            (SyncMode::Mirror, Side::Right) => "folder_sync.mirror_left",
            (SyncMode::Update, Side::Left) => "folder_sync.update_right",
            (SyncMode::Update, Side::Right) => "folder_sync.update_left",
        }
    }
}

/// What is at a path, without following a symbolic link there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Missing,
    File,
    Dir,
    Link,
    /// A device, pipe or socket.
    Other,
    /// There, but not to be read now.
    Unreadable,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CreateDir,
    Copy,
    Replace,
    Delete,
    /// Remove the destination folder once its listed contents are deleted, if it is empty.
    RemoveDir,
    /// Left alone by the policy.
    Skip,
    /// Left alone because it is not safe to decide.
    Conflict,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::CreateDir,
        Action::Copy,
        Action::Replace,
        Action::Delete,
        Action::RemoveDir,
        Action::Skip,
        Action::Conflict,
    ];

    pub fn changes(self) -> bool {
        !matches!(self, Action::Skip | Action::Conflict)
    }

    /// Whether it removes or overwrites something in the destination.
    pub fn destructive(self) -> bool {
        matches!(self, Action::Replace | Action::Delete | Action::RemoveDir)
    }

    pub fn key(self) -> &'static str {
        match self {
            Action::CreateDir => "folder_sync.action_create_dir",
            Action::Copy => "folder_sync.action_copy",
            Action::Replace => "folder_sync.action_replace",
            Action::Delete => "folder_sync.action_delete",
            Action::RemoveDir => "folder_sync.action_remove_dir",
            Action::Skip => "folder_sync.action_skip",
            Action::Conflict => "folder_sync.action_conflict",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    OnlyInSource,
    /// The contents differ.
    Changed,
    OnlyInDestination,
    Unreadable,
    Link,
    Special,
    /// A file on one side and a folder on the other.
    TypeDiffers,
    /// The name differs only in case or Unicode form from another, which a file system may
    /// take for the same name.
    NameClash,
    /// Not on disk as the folder compare found it.
    ChangedSinceCompare,
}

impl Reason {
    pub fn key(self) -> &'static str {
        match self {
            Reason::OnlyInSource => "folder_sync.reason_only_in_source",
            Reason::Changed => "folder_sync.reason_changed",
            Reason::OnlyInDestination => "folder_sync.reason_only_in_destination",
            Reason::Unreadable => "folder_sync.reason_unreadable",
            Reason::Link => "folder_sync.reason_link",
            Reason::Special => "folder_sync.reason_special",
            Reason::TypeDiffers => "folder_sync.reason_type_differs",
            Reason::NameClash => "folder_sync.reason_name_clash",
            Reason::ChangedSinceCompare => "folder_sync.reason_changed_since_compare",
        }
    }
}

/// What a path held when the plan was made.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Precondition {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Nanoseconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Precondition {
    fn of(kind: Kind) -> Precondition {
        Precondition {
            kind,
            size: None,
            modified: None,
            sha256: None,
        }
    }

    /// What is at `path` now; with `hash`, a regular file is fingerprinted too.
    fn read(path: &Path, hash: bool) -> Precondition {
        Precondition::read_until(path, hash, &NEVER)
    }

    /// As [`Precondition::read`]; a fingerprint cut short by `stop` makes it unreadable.
    fn read_until(path: &Path, hash: bool, stop: &AtomicBool) -> Precondition {
        let md = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Precondition::of(Kind::Missing)
            }
            Err(_) => return Precondition::of(Kind::Unreadable),
        };
        let kind = if md.file_type().is_symlink() {
            Kind::Link
        } else if md.is_dir() {
            Kind::Dir
        } else if md.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        if kind == Kind::Link {
            // The target is the link's contents; nothing is read through it.
            let Ok(target) = std::fs::read_link(path) else {
                return Precondition::of(Kind::Unreadable);
            };
            let sha256 = if hash {
                match link_hash(&target) {
                    Some(h) => Some(h),
                    None => return Precondition::of(Kind::Unreadable),
                }
            } else {
                None
            };
            return Precondition {
                kind,
                size: Some(target.as_os_str().len() as u64),
                modified: None,
                sha256,
            };
        }
        if kind != Kind::File {
            return Precondition::of(kind);
        }
        let mut p = Precondition {
            kind,
            size: Some(md.len()),
            modified: nanos(md.modified().ok()),
            sha256: None,
        };
        if hash {
            match hash_file(path, &md, stop) {
                Some(h) => p.sha256 = Some(h),
                None => return Precondition::of(Kind::Unreadable),
            }
        }
        p
    }

    /// Whether `now` holds what this did. Times are not compared: a file touched but not
    /// changed is still the planned one, and one edited with its time put back is not.
    fn matches(&self, now: &Precondition) -> bool {
        self.kind == now.kind
            && self.size == now.size
            && (self.sha256.is_none() || self.sha256 == now.sha256)
    }
}

/// The fingerprint of a symbolic link's target text, which no file's contents share. Only a
/// target written as UTF-8 is taken, so the journal can record it as it is.
fn link_hash(target: &Path) -> Option<String> {
    let text = target.to_str()?;
    let mut h = Sha256::new();
    h.update(b"symlink\0");
    h.update(text.as_bytes());
    Some(format!("{:x}", h.finalize()))
}

/// The target of the link `path`, refused unless it is still the planned `expected` one.
fn planned_link(path: &Path, expected: &str) -> std::io::Result<PathBuf> {
    let target = std::fs::read_link(path).map_err(|_| changed_since_preview())?;
    if link_hash(&target).as_deref() != Some(expected) {
        return Err(changed_since_preview());
    }
    Ok(target)
}

/// Makes a symbolic link to `target` at `at`, where nothing may be.
fn make_link(target: &Path, at: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                changed_since_preview()
            } else {
                e
            }
        })?;
        if let Some(dir) = at.parent() {
            let _ = sync_dir(dir);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (target, at);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// Puts a link to `target` in place of the link `at`, while that still has the fingerprint
/// `expected`: made beside it under a temporary name starting with `prefix`, then renamed over.
fn replace_link(target: &Path, at: &Path, expected: &str, prefix: &str) -> std::io::Result<()> {
    let dir = at
        .parent()
        .ok_or_else(|| std::io::Error::other(tr("folder_sync.err_not_file")))?;
    let tmp = (0..)
        .map(|k| dir.join(format!("{prefix}link-{k}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("a free name");
    make_link(target, &tmp)?;
    let still = std::fs::read_link(at)
        .ok()
        .and_then(|t| link_hash(&t))
        .is_some_and(|h| h == expected);
    if !still {
        let _ = std::fs::remove_file(&tmp);
        return Err(changed_since_preview());
    }
    if let Err(e) = std::fs::rename(&tmp, at) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = sync_dir(dir);
    Ok(())
}

fn nanos(t: Option<SystemTime>) -> Option<u64> {
    let d = t?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    u64::try_from(d.as_nanos()).ok()
}

/// Opens a file for reading without following a symbolic link at its name.
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT
        options.custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(tr("folder_sync.err_not_file")));
    }
    Ok(file)
}

/// Copies `from` into `to`, returning the SHA-256 of what was copied.
fn copy_hashing(
    from: &mut impl Read,
    to: &mut impl Write,
    stop: &AtomicBool,
) -> std::io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                tr("folder_sync.err_stopped"),
            ));
        }
        let count = from.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        to.write_all(&buffer[..count])?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// What tells a file from one written over it since: its size, time and, on Unix, inode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp(u64, Option<SystemTime>, u64);

impl Stamp {
    fn of(md: &std::fs::Metadata) -> Stamp {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(md);
        #[cfg(not(unix))]
        let inode = 0;
        Stamp(md.len(), md.modified().ok(), inode)
    }

    fn at(path: &Path) -> Option<Stamp> {
        std::fs::symlink_metadata(path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|md| Stamp::of(&md))
    }
}

/// Makes the names just created in `dir` last through a power failure.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Deletes the file `path`; on Windows also a read-only one.
fn remove_file(path: &Path) -> std::io::Result<()> {
    let original = std::fs::symlink_metadata(path).map(|md| md.permissions());
    let unlocked = cfg!(windows) && fsutil::make_writable(path);
    let removed = std::fs::remove_file(path);
    if let (true, Err(_), Ok(p)) = (unlocked, &removed, original) {
        let _ = std::fs::set_permissions(path, p);
    }
    removed
}

/// The name start of the temporary files a synchronization leaves beside its destinations,
/// so recovery can tell them from anything else.
fn temp_prefix(plan_id: &str) -> String {
    format!(".oxidemerge-sync-{}-", plan_id.get(..12).unwrap_or(plan_id))
}

/// The SHA-256 of the regular file `path` that `md` describes, unless it changes meanwhile.
fn hash_file(path: &Path, md: &std::fs::Metadata, stop: &AtomicBool) -> Option<String> {
    let mut file = open_nofollow(path).ok()?;
    let digest = copy_hashing(&mut file, &mut std::io::sink(), stop).ok()?;
    let after = std::fs::symlink_metadata(path).ok()?;
    (after.is_file() && after.len() == md.len() && after.modified().ok() == md.modified().ok())
        .then_some(digest)
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SyncOperation {
    /// Relative to the compared folders, with `/` between names.
    pub path: String,
    pub action: Action,
    pub reason: Reason,
    /// The bytes copied, replaced or deleted.
    pub bytes: u64,
    pub source: Precondition,
    pub destination: Precondition,
    /// For a copy, the path of a deleted destination file with the same contents; for that
    /// deletion, the path of the copy. Together they are a file renamed or moved in the
    /// source, applied by moving the destination file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanWarning {
    /// The filter or hidden-file option left items out, so a mirror does not delete them
    /// and the destination may keep files the source does not have.
    Excluded,
    Conflicts {
        count: usize,
    },
    LargeDeletion {
        count: usize,
    },
}

impl PlanWarning {
    pub fn text(&self) -> String {
        match self {
            PlanWarning::Excluded => tr("folder_sync.warn_excluded").to_string(),
            PlanWarning::Conflicts { count } => {
                trf("folder_sync.warn_conflicts", &[("n", count.to_string())])
            }
            PlanWarning::LargeDeletion { count } => trf(
                "folder_sync.warn_large_deletion",
                &[("n", count.to_string())],
            ),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Estimate {
    pub create_dirs: usize,
    pub copies: usize,
    pub replaces: usize,
    pub deletes: usize,
    pub skips: usize,
    pub conflicts: usize,
    /// Copies paired with a deletion of the same contents, counted in both as well.
    #[serde(default)]
    pub renames: usize,
    /// Bytes to copy, new files and replacements together.
    pub bytes: u64,
    pub deleted_bytes: u64,
    /// Files and folders in the destination when planned.
    pub destination_entries: usize,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SyncPlan {
    pub version: u32,
    /// A SHA-256 over the policy, the operations, the filter and the folders, so a plan file
    /// that was altered, or one made for folders that have changed since, is refused.
    pub id: String,
    pub policy: SyncPolicy,
    /// The filter of the compare, which decides what a mirror leaves alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_hidden: bool,
    /// A SHA-256 over the full paths of the two folders, so the plan is applied only to the
    /// folders it was made for without naming them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folders: String,
    /// Symbolic links are synchronized as links, never followed. Plans without it leave links
    /// alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub copy_links: bool,
    pub operations: Vec<SyncOperation>,
    pub warnings: Vec<PlanWarning>,
    pub estimate: Estimate,
}

impl SyncPlan {
    fn identity(&self) -> String {
        let base = (self.version, &self.policy, &self.operations, &self.warnings);
        let all = (base, &self.filter, self.skip_hidden, &self.folders);
        // Left out while off, so plans made before it keep their identity.
        if self.copy_links {
            analysis::identity(&(all, "copy_links"))
        } else {
            analysis::identity(&all)
        }
    }

    pub fn changes(&self) -> usize {
        self.operations
            .iter()
            .filter(|o| o.action.changes())
            .count()
    }

    pub fn deletions(&self) -> usize {
        self.estimate.deletes
    }

    pub fn count(&self, action: Action) -> usize {
        self.operations
            .iter()
            .filter(|o| o.action == action)
            .count()
    }

    /// Whether `fresh`, planned again, makes the same changes to files with the same
    /// contents. Times are left out, as applying leaves them out.
    pub fn agrees_with(&self, fresh: &SyncPlan) -> bool {
        let timeless = |p: &SyncPlan| {
            let mut ops = p.operations.clone();
            for o in &mut ops {
                (o.source.modified, o.destination.modified) = (None, None);
            }
            (
                p.policy,
                ops,
                p.warnings.clone(),
                (p.filter.clone(), p.skip_hidden),
            )
        };
        timeless(self) == timeless(fresh)
    }

    /// Whether the deletions are many enough to confirm a second time.
    pub fn large_deletion(&self) -> bool {
        let d = self.deletions();
        d >= LARGE_DELETION || (d >= 10 && d * 4 >= self.estimate.destination_entries)
    }

    /// The plan as JSON: paths relative to the folders, which are not named.
    pub fn to_json(&self) -> anyhow::Result<String> {
        analysis::json(self)
    }

    /// Reads a plan file, refusing a newer format or one whose id does not match it.
    pub fn from_json(text: &str) -> Result<SyncPlan, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("{}: {e}", tr("folder_sync.err_plan_file")))?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        match version {
            Some(v) if v > u64::from(PLAN_VERSION) => {
                return Err(trf(
                    "folder_sync.err_future_version",
                    &[("n", v.to_string())],
                ))
            }
            Some(_) => {}
            None => return Err(tr("folder_sync.err_plan_file").to_string()),
        }
        let plan: SyncPlan = serde_json::from_value(value)
            .map_err(|e| format!("{}: {e}", tr("folder_sync.err_plan_file")))?;
        if plan.id != plan.identity() {
            return Err(tr("folder_sync.err_plan_altered").to_string());
        }
        Ok(plan)
    }
}

/// What the folder compare the plan starts from covered.
#[derive(Clone, Default)]
pub struct Scope {
    /// The compare finished and listed every folder.
    pub complete: bool,
    /// The filter or hidden-file option may have left items out.
    pub excluded: bool,
    /// The filter as written, recorded in the plan.
    pub filter: String,
    pub skip_hidden: bool,
    /// Synchronize symbolic links as links rather than leave them alone.
    pub copy_links: bool,
}

/// Folders known in plans by a name rather than by where they are: copies of folders on a
/// server, fetched afresh to a new place each time, by their address there.
static FOLDER_NAMES: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

/// Has plans know the folder at `local` as `name`, so one made against a copy of a folder on a
/// server applies to the next copy of it.
pub fn name_folder(local: &Path, name: &str) {
    let real = std::fs::canonicalize(local).unwrap_or_else(|_| local.to_path_buf());
    if let Ok(mut names) = FOLDER_NAMES.lock() {
        names.retain(|(p, _)| *p != real);
        names.push((real, name.to_string()));
    }
}

/// The SHA-256 of the two folders' full paths (or names, see [`name_folder`]), as recorded in
/// a plan.
fn folders_digest(roots: &[PathBuf]) -> String {
    let names = FOLDER_NAMES.lock().map(|n| n.clone()).unwrap_or_default();
    let real: Vec<String> = roots
        .iter()
        .map(|r| {
            let real = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
            match names.iter().find(|(p, _)| *p == real) {
                Some((_, name)) => name.clone(),
                None => real.to_string_lossy().into_owned(),
            }
        })
        .collect();
    analysis::identity(&real)
}

/// Links each copy to the deletion of the same contents when no other copy or deletion has
/// them. Empty files are left alone, as their contents say nothing.
fn pair_renames(operations: &mut [SyncOperation]) {
    let mut by_hash: HashMap<&str, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (i, o) in operations
        .iter()
        .enumerate()
        .filter(|(_, o)| o.renamed.is_none())
    {
        let (side, copy) = match o.action {
            Action::Copy => (&o.source, true),
            Action::Delete => (&o.destination, false),
            _ => continue,
        };
        if side.kind != Kind::File {
            continue;
        }
        if let (Some(h), Some(1..)) = (&side.sha256, side.size) {
            let (copies, deletes) = by_hash.entry(h.as_str()).or_default();
            let list = if copy { copies } else { deletes };
            list.push(i);
        }
    }
    let pairs: Vec<(usize, usize)> = by_hash
        .into_values()
        .filter_map(|(c, d)| (c.len() == 1 && d.len() == 1).then(|| (c[0], d[0])))
        .collect();
    for (c, d) in pairs {
        operations[c].renamed = Some(operations[d].path.clone());
        operations[d].renamed = Some(operations[c].path.clone());
    }
}

/// Refuses folders that are the same, one inside the other, or one reached through another
/// name, so a synchronization never copies a folder into itself.
pub fn check_roots(roots: &[PathBuf]) -> Result<(), String> {
    if roots.len() != 2 {
        return Err(tr("folder_sync.err_two_folders").to_string());
    }
    let mut real = Vec::new();
    for r in roots {
        let md = std::fs::symlink_metadata(r);
        let c = std::fs::canonicalize(r);
        match (md, c) {
            (Ok(_), Ok(c)) if c.is_dir() => real.push(c),
            _ => {
                return Err(trf(
                    "folder_sync.err_root_unreadable",
                    &[("path", r.display().to_string())],
                ))
            }
        }
    }
    if real[0] == real[1] {
        return Err(tr("folder_sync.err_same_folder").to_string());
    }
    if real[0].starts_with(&real[1]) || real[1].starts_with(&real[0]) {
        return Err(tr("folder_sync.err_nested").to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(&real[0]), std::fs::metadata(&real[1])) {
            if a.dev() == b.dev() && a.ino() == b.ino() {
                return Err(tr("folder_sync.err_same_folder").to_string());
            }
        }
    }
    Ok(())
}

/// The names in each folder, to tell whether a path names its entry exactly or only reaches
/// it through a file system that ignores case or Unicode form.
#[derive(Default)]
struct Names(HashMap<PathBuf, Option<HashSet<OsString>>>);

impl Names {
    fn of(&mut self, dir: &Path) -> Option<&HashSet<OsString>> {
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_dir(dir)
                    .ok()
                    .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect())
            })
            .as_ref()
    }

    fn exact(&mut self, path: &Path) -> bool {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return true;
        };
        self.of(dir).is_none_or(|names| names.contains(name))
    }

    /// How the one entry in `path`'s folder whose name folds as `path`'s does is spelled, if
    /// exactly one does.
    fn spelling(&mut self, path: &Path) -> Option<String> {
        let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
        let key = fold(name);
        let mut alike = self
            .of(dir)?
            .iter()
            .filter_map(|n| n.to_str())
            .filter(|n| fold(n) == key);
        let one = alike.next()?.to_string();
        alike.next().is_none().then_some(one)
    }
}

/// Whether the entry `path` names is in its folder under exactly that name, not only reached
/// through a file system that ignores case or Unicode form. A folder that cannot be listed
/// does not say otherwise.
fn named_exactly(path: &Path) -> bool {
    Names::default().exact(path)
}

/// As [`Precondition::read_until`], but a name reaching an entry only under another case or
/// Unicode form is missing.
fn read_exact(path: &Path, hash: bool, stop: &AtomicBool) -> Precondition {
    if named_exactly(path) {
        Precondition::read_until(path, hash, stop)
    } else {
        Precondition::of(Kind::Missing)
    }
}

/// Whether `o` is half of a rename only in case or Unicode form, whose two names a file
/// system may take for one: its paths are then read only as spelled.
fn respelled(o: &SyncOperation) -> bool {
    o.renamed
        .as_deref()
        .is_some_and(|r| r != o.path && fold(r) == fold(&o.path))
}

/// `path` with its nearest existing folder resolved, as it will be once the rest is created.
fn real_path(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `name` without case or accents on Latin letters, composed or not: two names alike here
/// may be one name to a file system that ignores case or Unicode form. Some different names
/// fold alike too, which only leaves them alone.
fn fold(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .map(|c| match c {
            'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
            'ď' => 'd',
            'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
            'ĥ' => 'h',
            'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
            'ĵ' => 'j',
            'ķ' => 'k',
            'ĺ' | 'ļ' | 'ľ' => 'l',
            'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
            'ò'..='ö' | 'ō' | 'ŏ' | 'ő' => 'o',
            'ŕ' | 'ŗ' | 'ř' => 'r',
            'ś' | 'ŝ' | 'ş' | 'š' => 's',
            'ţ' | 'ť' => 't',
            'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
            'ŵ' => 'w',
            'ý' | 'ÿ' | 'ŷ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            c => c,
        })
        .collect()
}

fn under(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

/// Plans the synchronization of the two compared folders. Reads the files the plan would
/// touch to fingerprint them, and writes nothing.
pub fn plan(
    items: &[DirItem],
    roots: &[PathBuf],
    policy: SyncPolicy,
    scope: Scope,
    stop: &AtomicBool,
) -> Result<SyncPlan, String> {
    check_roots(roots)?;
    let unfinished = items
        .iter()
        .any(|it| matches!(it.status, CmpStatus::Pending | CmpStatus::NotCompared));
    if !scope.complete || unfinished {
        return Err(tr("folder_sync.err_incomplete").to_string());
    }
    let (s, d) = (policy.source(), policy.destination());
    let copy_links = scope.copy_links && cfg!(unix);
    // Two source names one destination may take for the same one.
    let mut folded: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate().filter(|(_, it)| it.exists[s]) {
        let key = (fold(&it.rel_dir), fold(&it.name));
        folded.entry(key).or_default().push(i);
    }
    let clashing: HashSet<usize> = folded
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    let mut order: Vec<(String, usize)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.rel_path(), i))
        .collect();
    order.sort();
    let mut names = Names::default();
    let mut cut: HashSet<usize> = HashSet::new();
    let mut operations = Vec::new();
    // Renames only in case or Unicode form planned so far, by folder and folded name.
    let mut respelt: HashSet<(String, String)> = HashSet::new();
    for (rel, i) in order {
        if stop.load(Ordering::Relaxed) {
            return Err(tr("folder_sync.err_stopped").to_string());
        }
        let it = &items[i];
        if it.parent.is_some_and(|p| cut.contains(&p)) {
            cut.insert(i);
            continue;
        }
        let (sp, dp) = (under(&roots[s], &rel), under(&roots[d], &rel));
        let (ks, kd) = (
            Precondition::read(&sp, false).kind,
            Precondition::read(&dp, false).kind,
        );
        // A file whose name the two sides spell differently only in case or Unicode form,
        // which a file system ignoring those takes for one name, with the same contents on
        // both: a mirror renames it. Planned once, as the copy and the deletion of a rename.
        if !it.is_dir
            && policy.mode == SyncMode::Mirror
            && (ks, kd) == (Kind::File, Kind::File)
            && !clashing.contains(&i)
        {
            if let (Some(a), Some(b)) = (names.spelling(&sp), names.spelling(&dp)) {
                if a != b {
                    if !respelt.insert((it.rel_dir.clone(), fold(&a))) {
                        continue;
                    }
                    let join = |n: &str| {
                        if it.rel_dir.is_empty() {
                            n.to_string()
                        } else {
                            format!("{}/{n}", it.rel_dir)
                        }
                    };
                    let (new, old) = (join(&a), join(&b));
                    let source = Precondition::read_until(&under(&roots[s], &new), true, stop);
                    let destination = Precondition::read_until(&under(&roots[d], &old), true, stop);
                    let same = source.sha256.is_some() && source.sha256 == destination.sha256;
                    let pair =
                        |path: &str, other: &str, action, reason, bytes, src, dst| SyncOperation {
                            path: path.to_string(),
                            action,
                            reason,
                            bytes,
                            source: src,
                            destination: dst,
                            renamed: same.then(|| other.to_string()),
                        };
                    if same {
                        let bytes = source.size.unwrap_or(0);
                        operations.push(pair(
                            &new,
                            &old,
                            Action::Copy,
                            Reason::OnlyInSource,
                            bytes,
                            source,
                            Precondition::of(Kind::Missing),
                        ));
                        operations.push(pair(
                            &old,
                            &new,
                            Action::Delete,
                            Reason::OnlyInDestination,
                            bytes,
                            Precondition::of(Kind::Missing),
                            destination,
                        ));
                    } else {
                        // Renamed and changed: left alone, as any other name clash.
                        operations.push(pair(
                            &rel,
                            "",
                            Action::Conflict,
                            Reason::NameClash,
                            0,
                            Precondition::of(Kind::File),
                            Precondition::of(Kind::File),
                        ));
                    }
                    continue;
                }
            }
        }
        let expected = if it.is_dir { Kind::Dir } else { Kind::File };
        let in_s = ks != Kind::Missing;
        let in_d = kd != Kind::Missing;
        // A link on one side with a link or nothing on the other, synchronized as a link. The
        // compare may have found it unreadable: it reads through the link, which this does not.
        let link = copy_links
            && matches!(
                (ks, kd),
                (Kind::Link, Kind::Link)
                    | (Kind::Link, Kind::Missing)
                    | (Kind::Missing, Kind::Link)
            );
        let conflict = if !link
            && (it.status == CmpStatus::Error || ks == Kind::Unreadable || kd == Kind::Unreadable)
        {
            Some(Reason::Unreadable)
        } else if !link && (ks == Kind::Link || kd == Kind::Link) {
            Some(Reason::Link)
        } else if ks == Kind::Other || kd == Kind::Other {
            Some(Reason::Special)
        } else if link && it.is_dir {
            // The compare, which does not follow links, saw a folder here: what it listed
            // inside would now be reached through the link.
            Some(Reason::ChangedSinceCompare)
        } else if !link && ((in_s && ks != expected) || (in_d && kd != expected)) {
            Some(Reason::TypeDiffers)
        } else if (in_s && !it.exists[s])
            || (in_d && !it.exists[d])
            || (in_s && !names.exact(&sp))
            || (in_d && !names.exact(&dp))
            || clashing.contains(&i)
        {
            Some(Reason::NameClash)
        } else if in_s != it.exists[s] || in_d != it.exists[d] {
            Some(Reason::ChangedSinceCompare)
        } else {
            None
        };
        let op = |action, reason, bytes, source, destination| SyncOperation {
            path: rel.clone(),
            action,
            reason,
            bytes,
            source,
            destination,
            renamed: None,
        };
        if let Some(reason) = conflict {
            cut.insert(i);
            operations.push(op(
                Action::Conflict,
                reason,
                0,
                Precondition::of(ks),
                Precondition::of(kd),
            ));
            continue;
        }
        let fingerprint = |p: &Path| Precondition::read_until(p, true, stop);
        if link {
            // Nothing is ever planned under a link.
            cut.insert(i);
        }
        let planned = if link {
            match (in_s, in_d) {
                (true, false) => Some((
                    Action::Copy,
                    Reason::OnlyInSource,
                    fingerprint(&sp),
                    Precondition::of(Kind::Missing),
                )),
                (true, true) => {
                    let (a, b) = (fingerprint(&sp), fingerprint(&dp));
                    (a.sha256 != b.sha256 || a.sha256.is_none()).then_some((
                        Action::Replace,
                        Reason::Changed,
                        a,
                        b,
                    ))
                }
                (false, true) if policy.mode == SyncMode::Update => Some((
                    Action::Skip,
                    Reason::OnlyInDestination,
                    Precondition::of(Kind::Missing),
                    Precondition::of(kd),
                )),
                (false, true) => Some((
                    Action::Delete,
                    Reason::OnlyInDestination,
                    Precondition::of(Kind::Missing),
                    fingerprint(&dp),
                )),
                (false, false) => None,
            }
        } else {
            match (in_s, in_d, it.is_dir) {
                (true, false, true) => Some((
                    Action::CreateDir,
                    Reason::OnlyInSource,
                    Precondition::of(Kind::Dir),
                    Precondition::of(Kind::Missing),
                )),
                (true, false, false) => Some((
                    Action::Copy,
                    Reason::OnlyInSource,
                    fingerprint(&sp),
                    Precondition::of(Kind::Missing),
                )),
                // The compare may call files the same that differ in ignored ways, such as line
                // endings: unless size and time agree too, the contents decide.
                (true, true, false)
                    if !(it.status == CmpStatus::Identical
                        && it.size[s] == it.size[d]
                        && it.mtime[s] == it.mtime[d]) =>
                {
                    let (a, b) = (fingerprint(&sp), fingerprint(&dp));
                    (a.sha256 != b.sha256 || a.kind != b.kind).then_some((
                        Action::Replace,
                        Reason::Changed,
                        a,
                        b,
                    ))
                }
                (false, true, _) if policy.mode == SyncMode::Update => {
                    cut.insert(i);
                    Some((
                        Action::Skip,
                        Reason::OnlyInDestination,
                        Precondition::of(Kind::Missing),
                        Precondition::of(kd),
                    ))
                }
                (false, true, true) => Some((
                    Action::RemoveDir,
                    Reason::OnlyInDestination,
                    Precondition::of(Kind::Missing),
                    Precondition::of(Kind::Dir),
                )),
                (false, true, false) => Some((
                    Action::Delete,
                    Reason::OnlyInDestination,
                    Precondition::of(Kind::Missing),
                    fingerprint(&dp),
                )),
                _ => None,
            }
        };
        let Some((action, reason, source, destination)) = planned else {
            continue;
        };
        if (action.changes() && source.kind == Kind::Unreadable)
            || destination.kind == Kind::Unreadable
        {
            cut.insert(i);
            operations.push(op(
                Action::Conflict,
                Reason::Unreadable,
                0,
                Precondition::of(source.kind),
                Precondition::of(destination.kind),
            ));
            continue;
        }
        let bytes = match action {
            _ if link => 0,
            Action::Copy | Action::Replace => source.size.unwrap_or(0),
            Action::Delete => destination.size.unwrap_or(0),
            _ => 0,
        };
        operations.push(op(action, reason, bytes, source, destination));
    }
    // The other spelling of a name planned as a rename above is the same file to the
    // destination: deleting it as well would count it twice, or delete the file left alone.
    operations.retain(|o| {
        let (dir, name) = o.path.rsplit_once('/').unwrap_or(("", &o.path));
        o.renamed.is_some()
            || o.action != Action::Delete
            || !respelt.contains(&(dir.to_string(), fold(name)))
    });
    operations.sort_by(|a, b| {
        let phase = |o: &SyncOperation| match o.action {
            Action::CreateDir => 0,
            Action::Copy | Action::Replace => 1,
            Action::Delete => 2,
            Action::RemoveDir => 3,
            Action::Skip | Action::Conflict => 4,
        };
        let deleting = matches!(a.action, Action::Delete | Action::RemoveDir);
        phase(a).cmp(&phase(b)).then_with(|| {
            // Deepest first, so a folder is emptied before it is removed.
            if deleting {
                b.path.cmp(&a.path)
            } else {
                a.path.cmp(&b.path)
            }
        })
    });
    pair_renames(&mut operations);
    let mut estimate = Estimate {
        destination_entries: items.iter().filter(|it| it.exists[d]).count(),
        ..Estimate::default()
    };
    for o in &operations {
        match o.action {
            Action::CreateDir => estimate.create_dirs += 1,
            Action::Copy => estimate.copies += 1,
            Action::Replace => estimate.replaces += 1,
            Action::Delete | Action::RemoveDir => estimate.deletes += 1,
            Action::Skip => estimate.skips += 1,
            Action::Conflict => estimate.conflicts += 1,
        }
        match o.action {
            Action::Copy | Action::Replace => estimate.bytes += o.bytes,
            Action::Delete => estimate.deleted_bytes += o.bytes,
            _ => {}
        }
        if o.action == Action::Copy && o.renamed.is_some() {
            estimate.renames += 1;
        }
    }
    let mut warnings = Vec::new();
    if scope.excluded && policy.mode == SyncMode::Mirror {
        warnings.push(PlanWarning::Excluded);
    }
    if estimate.conflicts > 0 {
        warnings.push(PlanWarning::Conflicts {
            count: estimate.conflicts,
        });
    }
    let mut plan = SyncPlan {
        version: PLAN_VERSION,
        id: String::new(),
        policy,
        filter: scope.filter,
        skip_hidden: scope.skip_hidden,
        folders: folders_digest(roots),
        copy_links,
        operations,
        warnings,
        estimate,
    };
    if plan.large_deletion() {
        plan.warnings.push(PlanWarning::LargeDeletion {
            count: plan.deletions(),
        });
    }
    plan.id = plan.identity();
    Ok(plan)
}

/// How a synchronization ended.
#[derive(Debug)]
pub enum SyncOutcome {
    /// Nothing was changed: the plan no longer fits the folders, or they may not be synced.
    NotApplied(Problem, Vec<String>),
    /// Every planned change was made and checked. `kept` lists folders left in place
    /// because they still hold items the compare did not list.
    Applied { changes: usize, kept: Vec<String> },
    /// Stopped part way, by the user (`reason` is `None`) or a failure, and undone as far as
    /// `rollback` says.
    Stopped {
        reason: Option<String>,
        rollback: RollbackOutcome,
        /// What the failure was; `None` when the user stopped it.
        failure: Option<Problem>,
    },
}

impl SyncOutcome {
    /// What kept the synchronization from simply succeeding, in the shared taxonomy.
    pub fn problem(&self) -> Option<Problem> {
        match self {
            SyncOutcome::Applied { .. } => None,
            SyncOutcome::NotApplied(problem, _) => Some(*problem),
            SyncOutcome::Stopped { rollback, .. } if !rollback.complete() => {
                Some(Problem::RecoveryNeeded)
            }
            SyncOutcome::Stopped { reason: None, .. } => Some(Problem::Canceled),
            SyncOutcome::Stopped { failure, .. } => Some(failure.unwrap_or(Problem::WriteFailure)),
        }
    }
}

#[derive(Debug, Default)]
pub struct RollbackOutcome {
    pub restored: usize,
    /// Paths not put back, and why.
    pub failed: Vec<(String, String)>,
    /// The recovery journal, kept with its copies while something was not put back.
    pub journal: Option<PathBuf>,
}

impl RollbackOutcome {
    pub fn complete(&self) -> bool {
        self.failed.is_empty() && self.journal.is_none()
    }
}

/// Where recovery journals are kept: in the settings folder, never in a synced folder.
pub fn journal_root() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("sync-recovery"))
}

#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    plan: String,
    roots: [PathBuf; 2],
    destination: usize,
    started: u64,
    /// Where the journal is, which is not written in it.
    #[serde(skip)]
    dir: PathBuf,
}

/// What a destination path held before the synchronization changed it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Before {
    Missing,
    Dir,
    File {
        /// The copy in the journal, relative to it.
        backup: String,
        sha256: String,
    },
    /// Nothing was there; the destination file `from` (relative to the folder) was moved
    /// there, through `via` beside it, a temporary name of its own.
    Moved {
        from: String,
        via: String,
    },
    /// A symbolic link to `target`.
    Link {
        target: String,
        sha256: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: String,
    before: Before,
    /// The fingerprint of the file written there, so only that is undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written: Option<String>,
}

/// An append-only record, flushed to disk before each change it describes. Its file stays
/// locked while the synchronization runs or is undone, so another window does not take it
/// for one that did not finish.
struct Journal {
    dir: PathBuf,
    file: std::fs::File,
    header: Header,
    entries: Vec<Entry>,
}

impl Journal {
    fn create(root: &Path, plan: &SyncPlan, roots: &[PathBuf]) -> std::io::Result<Journal> {
        std::fs::create_dir_all(root)?;
        let started = nanos(Some(SystemTime::now())).unwrap_or(0) / 1_000_000;
        let dir = root.join(format!("{}-{started}", &plan.id[..12]));
        std::fs::create_dir(&dir)?;
        std::fs::create_dir(dir.join("files"))?;
        let header = Header {
            version: JOURNAL_VERSION,
            plan: plan.id.clone(),
            roots: [roots[0].clone(), roots[1].clone()],
            destination: plan.policy.destination(),
            started,
            dir: dir.clone(),
        };
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(dir.join(JOURNAL_FILE))?;
        file.lock()?;
        // Written last: a journal without its header is not offered for recovery.
        let json = serde_json::to_vec(&header).map_err(std::io::Error::other)?;
        let mut h = std::fs::File::create(dir.join(HEADER_FILE))?;
        h.write_all(&json)?;
        h.sync_all()?;
        sync_dir(&dir)?;
        sync_dir(root)?;
        Ok(Journal {
            dir,
            file,
            header,
            entries: Vec::new(),
        })
    }

    fn record(&mut self, entry: Entry) -> std::io::Result<()> {
        let line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
        self.file.write_all(format!("{line}\n").as_bytes())?;
        self.file.sync_data()?;
        self.entries.push(entry);
        Ok(())
    }

    /// Copies the destination file `path` into the journal, refusing it unless it is still
    /// the planned `expected` one. Returns what the file was as it was copied, to tell
    /// whether it is written to again before it is replaced or deleted.
    fn keep(
        &mut self,
        path: &Path,
        expected: &str,
        stop: &AtomicBool,
    ) -> std::io::Result<(Before, Stamp)> {
        let name = format!("files/{}", self.entries.len());
        let files = self.dir.join("files");
        let to = self.dir.join(&name);
        let mut src = open_nofollow(path)?;
        let md = src.metadata()?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".oxidemerge-")
            .tempfile_in(fsutil::long_path_for(&files, ".oxidemerge-".len() + 16))?;
        let sha256 = copy_hashing(&mut src, tmp.as_file_mut(), stop)?;
        let stamp = Stamp::of(&md);
        if sha256 != expected || Stamp::at(path) != Some(stamp) {
            return Err(changed_since_preview());
        }
        if let Ok(t) = md.modified() {
            let _ = tmp.as_file().set_modified(t);
        }
        let _ = tmp.as_file().set_permissions(md.permissions());
        tmp.as_file().sync_all()?;
        tmp.persist(fsutil::long_path(&to)).map_err(|e| e.error)?;
        sync_dir(&files)?;
        Ok((
            Before::File {
                backup: name,
                sha256,
            },
            stamp,
        ))
    }

    fn temp_prefix(&self) -> String {
        temp_prefix(&self.header.plan)
    }
}

/// A path that is no longer as the preview found it.
#[derive(Debug)]
struct Changed;

impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(tr("folder_sync.err_changed"))
    }
}

impl std::error::Error for Changed {}

fn changed_since_preview() -> std::io::Error {
    std::io::Error::other(Changed)
}

/// The problem a failed step is.
fn problem_of(e: &std::io::Error) -> Problem {
    if e.get_ref().is_some_and(|inner| inner.is::<Changed>()) {
        Problem::Stale
    } else {
        Problem::of_io(e, true)
    }
}

/// Where [`publish`] may put a file.
enum Place {
    /// Only where nothing is.
    New,
    /// Over the file there, only while it is still the one stamped, if one is.
    Over(Option<Stamp>),
}

/// Copies `from` to a temporary file beside `to` named with `prefix`, checks it against
/// `expected` and renames it into place. `to` gets the time and permissions of `from`.
fn publish(
    from: &Path,
    to: &Path,
    expected: &str,
    place: Place,
    prefix: &str,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let dir = to
        .parent()
        .ok_or_else(|| std::io::Error::other(tr("folder_sync.err_not_file")))?;
    let mut src = open_nofollow(from)?;
    let md = src.metadata()?;
    // Staged beside the file under its long-path name, so the rename below is not refused.
    // The temporary name is the prefix and the random characters `tempfile` adds to it, which
    // count towards the limit as much as the folder does.
    let mut tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(fsutil::long_path_for(dir, prefix.len() + 16))?;
    if copy_hashing(&mut src, tmp.as_file_mut(), stop)? != expected {
        return Err(changed_since_preview());
    }
    if let Ok(t) = md.modified() {
        let _ = tmp.as_file().set_modified(t);
    }
    tmp.as_file().sync_all()?;
    match place {
        Place::New => {
            tmp.persist_noclobber(fsutil::long_path(to))
                .map_err(|e| e.error)?;
        }
        Place::Over(stamp) => {
            if stamp.is_some_and(|s| Stamp::at(to) != Some(s)) {
                return Err(changed_since_preview());
            }
            let original = std::fs::symlink_metadata(to).map(|m| m.permissions());
            let unlocked = cfg!(windows) && fsutil::make_writable(to);
            if let Err(e) = fsutil::persist(tmp, to) {
                if let (true, Ok(p)) = (unlocked, original) {
                    let _ = std::fs::set_permissions(to, p);
                }
                return Err(e);
            }
        }
    }
    let _ = std::fs::set_permissions(to, md.permissions());
    // Some network file systems cannot flush a folder; the journal still has the backup.
    let _ = sync_dir(dir);
    Ok(())
}

/// A failure or crash injected by tests between the steps of applying and undoing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum Point {
    /// Before changing a path.
    Before,
    /// After a file was written, before it is checked.
    Written,
    /// Half way through moving a renamed file: at its temporary name.
    Moving,
    /// Before putting a path back.
    Rollback,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum Fault {
    None,
    Fail,
    /// Fail as a write to a full file system does.
    Full,
    /// End at once, as if the program ended, leaving the journal.
    Crash,
}

type Faults<'a> = &'a dyn Fn(Point, &str) -> Fault;

/// The program ending, injected by tests inside a step.
#[derive(Debug)]
struct Crash;

impl std::fmt::Display for Crash {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("crash")
    }
}

impl std::error::Error for Crash {}

fn injected() -> std::io::Error {
    std::io::Error::other("injected failure")
}

/// Applies `plan` to the folders it was made for, keeping a recovery journal under
/// `journal_root`. `done` counts the changes made; setting `stop` stops and undoes them.
pub fn apply(
    plan: &SyncPlan,
    roots: &[PathBuf],
    journal_root: &Path,
    stop: &AtomicBool,
    done: &AtomicUsize,
) -> SyncOutcome {
    apply_with(plan, roots, journal_root, stop, done, &|_, _| Fault::None)
}

/// Refuses a plan whose new files, or whose journal copies of the files it replaces and
/// deletes, do not fit in the free space of their file systems. When the journal is on the
/// destination's file system, each file it keeps a copy of is replaced or deleted straight
/// after, so only the largest of them is there twice at once.
fn check_space(plan: &SyncPlan, roots: &[PathBuf], journal_root: &Path) -> Result<(), String> {
    let (s, d) = (plan.policy.source(), plan.policy.destination());
    let destination = roots[d].as_path();
    // A rename that moves the file it deletes into the copy's place writes and keeps nothing;
    // one that is copied and deleted instead counts as both.
    let renames: HashMap<&str, &SyncOperation> = plan
        .operations
        .iter()
        .filter(|o| o.action == Action::Copy)
        .filter_map(|o| Some((o.renamed.as_deref()?, o)))
        .collect();
    let moved = |o: &SyncOperation| match (o.action, o.renamed.as_deref()) {
        (Action::Copy, Some(old)) => plan
            .operations
            .iter()
            .find(|p| p.action == Action::Delete && p.path == old)
            .is_some_and(|old| moves(o, old, roots, s, d)),
        (Action::Delete, Some(_)) => renames
            .get(o.path.as_str())
            .is_some_and(|copy| moves(copy, o, roots, s, d)),
        _ => false,
    };
    let changes = || {
        plan.operations
            .iter()
            .filter(move |o| o.action.changes() && !moved(o))
    };
    let written: u64 = changes()
        .filter(|o| matches!(o.action, Action::Copy | Action::Replace))
        .map(|o| o.bytes)
        .sum();
    let kept = || {
        changes()
            .filter(|o| matches!(o.action, Action::Replace | Action::Delete))
            .filter_map(|o| o.destination.size)
    };
    let needs = if same_volume(destination, journal_root) {
        vec![(destination, written + kept().max().unwrap_or(0))]
    } else {
        vec![(destination, written), (journal_root, kept().sum())]
    };
    for (path, need) in needs {
        match free_space(path) {
            Some(free) if need > free => {
                return Err(trf(
                    "folder_sync.err_no_space",
                    &[
                        ("path", path.display().to_string()),
                        ("need", mib(need)),
                        ("free", mib(free)),
                    ],
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Whether the rename made of `copy` and `old`, its deletion, moves the file rather than
/// copying and deleting it: only within one file system, and since a move keeps the file's
/// time and permissions, only when the source has the same ones, unless only the spelling of
/// the name changed.
fn moves(copy: &SyncOperation, old: &SyncOperation, roots: &[PathBuf], s: usize, d: usize) -> bool {
    let (sp, dp) = (under(&roots[s], &copy.path), under(&roots[d], &copy.path));
    let odp = under(&roots[d], &old.path);
    let one_volume = match (odp.parent(), dp.parent()) {
        (Some(a), Some(b)) => same_volume(a, b),
        _ => false,
    };
    one_volume && (respelled(copy) || same_time_and_mode(&sp, &odp))
}

/// Whether files `a` and `b` have the same modification time and permissions. Times
/// within 2 seconds count as the same: FAT and some network shares round them.
fn same_time_and_mode(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    let close = match (a.modified(), b.modified()) {
        (Ok(x), Ok(y)) => {
            let gap = x.duration_since(y).or_else(|_| y.duration_since(x));
            gap.is_ok_and(|g| g <= std::time::Duration::from_secs(2))
        }
        _ => false,
    };
    close && a.permissions() == b.permissions()
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_volume(_: &Path, _: &Path) -> bool {
    true
}

#[cfg(test)]
thread_local! {
    /// The free space tests pretend every file system has.
    static FREE_SPACE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Bytes the user may still write on the file system holding `path`, when it can be told.
fn free_space(path: &Path) -> Option<u64> {
    #[cfg(test)]
    if let Some(free) = FREE_SPACE.with(|f| f.get()) {
        return Some(free);
    }
    free_space_of(path)
}

#[cfg(unix)]
fn free_space_of(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Not known here; a full disk then fails a write, which is undone like any other failure.
#[cfg(not(unix))]
fn free_space_of(_: &Path) -> Option<u64> {
    None
}

/// Every check that may refuse the plan before anything is changed.
fn preflight(
    plan: &SyncPlan,
    roots: &[PathBuf],
    journal_root: &Path,
    stop: &AtomicBool,
) -> Result<(), (Problem, Vec<String>)> {
    let one = |p: Problem| move |s: String| (p, vec![s]);
    if plan.version != PLAN_VERSION {
        return Err(one(Problem::Unsupported)(trf(
            "folder_sync.err_future_version",
            &[("n", plan.version.to_string())],
        )));
    }
    if plan.id != plan.identity() {
        return Err(one(Problem::Stale)(
            tr("folder_sync.err_plan_altered").to_string(),
        ));
    }
    check_roots(roots).map_err(one(Problem::Unsupported))?;
    if plan.folders != folders_digest(roots) {
        return Err(one(Problem::Stale)(
            tr("folder_sync.err_other_folders").to_string(),
        ));
    }
    // Checked before it is created, so a refused journal leaves nothing in a synced folder.
    let journal = real_path(journal_root);
    for r in roots {
        let r = std::fs::canonicalize(r).map_err(|e| one(Problem::ReadFailure)(e.to_string()))?;
        if journal.starts_with(&r) || r.starts_with(&journal) {
            return Err(one(Problem::Unsupported)(
                tr("folder_sync.err_journal_inside").to_string(),
            ));
        }
    }
    std::fs::create_dir_all(journal_root).map_err(|e| one(Problem::WriteFailure)(e.to_string()))?;
    check_space(plan, roots, journal_root).map_err(one(Problem::WriteFailure))?;
    let pending = journals(journal_root);
    let same = |a: &Path, b: &Path| {
        std::fs::canonicalize(a)
            .ok()
            .is_some_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
    };
    if pending
        .iter()
        .any(|u| u.roots.iter().any(|j| roots.iter().any(|r| same(j, r))))
    {
        return Err(one(Problem::RecoveryNeeded)(
            tr("folder_sync.err_unfinished").to_string(),
        ));
    }
    let (s, d) = (plan.policy.source(), plan.policy.destination());
    let mut errors = Vec::new();
    // The first thing found wrong, which is the one the errors below name first.
    let mut problem = None;
    let mut found = |p: Problem| problem = problem.or(Some(p));
    for o in plan.operations.iter().filter(|o| o.action.changes()) {
        if stop.load(Ordering::Relaxed) {
            return Err(one(Problem::Canceled)(
                tr("folder_sync.err_stopped").to_string(),
            ));
        }
        let (sp, dp) = (under(&roots[s], &o.path), under(&roots[d], &o.path));
        if escapes(&roots[d], &o.path, &dp) {
            errors.push(trf("folder_sync.err_outside", &[("path", o.path.clone())]));
            found(Problem::Unsupported);
            continue;
        }
        let hash = |p: &Precondition| p.sha256.is_some();
        let exact = respelled(o);
        let read = |p: &Path, planned: &Precondition| {
            if exact {
                read_exact(p, hash(planned), stop)
            } else {
                Precondition::read_until(p, hash(planned), stop)
            }
        };
        let now = (read(&sp, &o.source), read(&dp, &o.destination));
        if stop.load(Ordering::Relaxed) {
            return Err(one(Problem::Canceled)(
                tr("folder_sync.err_stopped").to_string(),
            ));
        }
        // A path that cannot be read now has not been shown to have changed: say which it is,
        // since a file another program holds open is put right in another way.
        let shut = |planned: &Precondition, now: &Precondition| {
            now.kind == Kind::Unreadable && planned.kind != Kind::Unreadable
        };
        if shut(&o.source, &now.0) || shut(&o.destination, &now.1) {
            errors.push(trf(
                "folder_sync.err_unreadable",
                &[("path", o.path.clone())],
            ));
            found(Problem::ReadFailure);
            continue;
        }
        if !o.source.matches(&now.0) || !o.destination.matches(&now.1) {
            errors.push(trf("folder_sync.err_stale", &[("path", o.path.clone())]));
            found(Problem::Stale);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err((problem.unwrap_or(Problem::Stale), errors))
    }
}

fn apply_with(
    plan: &SyncPlan,
    roots: &[PathBuf],
    journal_root: &Path,
    stop: &AtomicBool,
    done: &AtomicUsize,
    fault: Faults,
) -> SyncOutcome {
    if let Err((problem, errors)) = preflight(plan, roots, journal_root, stop) {
        return SyncOutcome::NotApplied(problem, errors);
    }
    // Recovery may run from another working folder.
    let absolute = |p: &Path| std::path::absolute(p).map_err(|e| vec![e.to_string()]);
    let (roots, journal_root) = match (
        roots
            .iter()
            .map(|r| absolute(r))
            .collect::<Result<Vec<_>, _>>(),
        absolute(journal_root),
    ) {
        (Ok(r), Ok(j)) => (r, j),
        (Err(e), _) | (_, Err(e)) => return SyncOutcome::NotApplied(Problem::ReadFailure, e),
    };
    let roots = &roots[..];
    let mut journal = match Journal::create(&journal_root, plan, roots) {
        Ok(j) => j,
        Err(e) => {
            return SyncOutcome::NotApplied(
                Problem::WriteFailure,
                vec![format!("{}: {e}", tr("folder_sync.journal"))],
            )
        }
    };
    let (s, d) = (plan.policy.source(), plan.policy.destination());
    let mut kept = Vec::new();
    let mut failure: Option<Option<String>> = None;
    let mut problem = None;
    let fail = |path: &str, e: std::io::Error, problem: &mut Option<Problem>| {
        *problem = Some(problem_of(&e));
        Some(Some(format!("{path}: {e}")))
    };
    // The deletions renames are made from, by path, and those already moved away.
    let sources: HashMap<&str, &SyncOperation> = plan
        .operations
        .iter()
        .filter(|o| o.action == Action::Delete && o.renamed.is_some())
        .map(|o| (o.path.as_str(), o))
        .collect();
    let mut moved: HashSet<&str> = HashSet::new();
    for o in plan.operations.iter().filter(|o| o.action.changes()) {
        if stop.load(Ordering::Relaxed) {
            failure = Some(None);
            break;
        }
        if o.action == Action::Delete && moved.contains(o.path.as_str()) {
            done.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        match fault(Point::Before, &o.path) {
            Fault::None => {}
            Fault::Fail => {
                failure = fail(&o.path, injected(), &mut problem);
                break;
            }
            Fault::Full => {
                failure = fail(
                    &o.path,
                    std::io::ErrorKind::StorageFull.into(),
                    &mut problem,
                );
                break;
            }
            Fault::Crash => return crashed(journal),
        }
        let (sp, dp) = (under(&roots[s], &o.path), under(&roots[d], &o.path));
        if escapes(&roots[d], &o.path, &dp) {
            problem = Some(Problem::Unsupported);
            failure = Some(Some(trf(
                "folder_sync.err_outside",
                &[("path", o.path.clone())],
            )));
            break;
        }
        let from = (o.action == Action::Copy)
            .then(|| o.renamed.as_deref().and_then(|r| sources.get(r)))
            .flatten()
            .filter(|old| moves(o, old, roots, s, d));
        let result = match from {
            Some(old) => {
                let (osp, odp) = (under(&roots[s], &old.path), under(&roots[d], &old.path));
                // The old name is checked now too: a folder above it may have become a link
                // since the plan was checked.
                if escapes(&roots[d], &old.path, &odp) {
                    problem = Some(Problem::Unsupported);
                    failure = Some(Some(trf(
                        "folder_sync.err_outside",
                        &[("path", old.path.clone())],
                    )));
                    break;
                }
                let r = step_move(&mut journal, o, old, (&sp, &dp, &osp, &odp), fault, stop);
                if r.is_ok() {
                    moved.insert(old.path.as_str());
                }
                r
            }
            None => step(&mut journal, o, (&sp, &dp), &mut kept, stop),
        };
        if let Err(e) = result {
            if e.get_ref().is_some_and(|inner| inner.is::<Crash>()) {
                return crashed(journal);
            }
            failure = if stop.load(Ordering::Relaxed) {
                Some(None)
            } else {
                fail(&o.path, e, &mut problem)
            };
            break;
        }
        done.fetch_add(1, Ordering::Relaxed);
        match fault(Point::Written, &o.path) {
            Fault::None => {}
            Fault::Fail => {
                failure = fail(&o.path, injected(), &mut problem);
                break;
            }
            Fault::Full => {
                failure = fail(
                    &o.path,
                    std::io::ErrorKind::StorageFull.into(),
                    &mut problem,
                );
                break;
            }
            Fault::Crash => return crashed(journal),
        }
    }
    if failure.is_none() {
        // Check what was written once more, now that all of it is in place.
        for o in &plan.operations {
            if !matches!(o.action, Action::Copy | Action::Replace) {
                continue;
            }
            let now = Precondition::read(&under(&roots[d], &o.path), true);
            if now.sha256 != o.source.sha256 {
                problem = Some(Problem::WriteFailure);
                failure = Some(Some(trf(
                    "folder_sync.err_verify",
                    &[("path", o.path.clone())],
                )));
                break;
            }
        }
    }
    let Journal {
        dir,
        file,
        header,
        entries,
    } = journal;
    match failure {
        None => {
            let _ = remove_journal(&dir, file);
            SyncOutcome::Applied {
                changes: done.load(Ordering::Relaxed),
                kept,
            }
        }
        Some(reason) => {
            let mut rollback = roll_back(&header, &entries, fault);
            settle(&dir, file, &mut rollback);
            SyncOutcome::Stopped {
                failure: problem.filter(|_| reason.is_some()),
                reason,
                rollback,
            }
        }
    }
}

fn crashed(journal: Journal) -> SyncOutcome {
    SyncOutcome::Stopped {
        reason: Some("crash".into()),
        failure: Some(Problem::RecoveryNeeded),
        rollback: RollbackOutcome {
            journal: Some(journal.dir),
            ..RollbackOutcome::default()
        },
    }
}

/// Makes one planned change, rechecking what is at both paths first.
fn step(
    journal: &mut Journal,
    o: &SyncOperation,
    (sp, dp): (&Path, &Path),
    kept: &mut Vec<String>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let unchanged = |p: &Path, planned: &Precondition| {
        planned.matches(&Precondition::read_until(
            p,
            planned.sha256.is_some() && matches!(planned.kind, Kind::File | Kind::Link),
            stop,
        ))
    };
    let entry = |before, written| Entry {
        path: o.path.clone(),
        before,
        written,
    };
    let prefix = journal.temp_prefix();
    match o.action {
        Action::CreateDir => {
            if !unchanged(dp, &o.destination) {
                return Err(changed_since_preview());
            }
            journal.record(entry(Before::Missing, None))?;
            std::fs::create_dir(dp)
        }
        Action::Copy if o.source.kind == Kind::Link => {
            let expected = o.source.sha256.clone().unwrap_or_default();
            let target = planned_link(sp, &expected)?;
            if !unchanged(dp, &o.destination) {
                return Err(changed_since_preview());
            }
            journal.record(entry(Before::Missing, Some(expected)))?;
            make_link(&target, dp)
        }
        Action::Replace if o.source.kind == Kind::Link => {
            let expected = o.source.sha256.clone().unwrap_or_default();
            let target = planned_link(sp, &expected)?;
            let old = o.destination.sha256.clone().unwrap_or_default();
            let before = keep_link(dp, &old)?;
            journal.record(entry(before, Some(expected)))?;
            replace_link(&target, dp, &old, &prefix)
        }
        Action::Delete if o.destination.kind == Kind::Link => {
            if Precondition::read(sp, false).kind != Kind::Missing {
                return Err(changed_since_preview());
            }
            let old = o.destination.sha256.clone().unwrap_or_default();
            let before = keep_link(dp, &old)?;
            journal.record(entry(before, None))?;
            planned_link(dp, &old)?;
            remove_file(dp)
        }
        Action::Copy => {
            let expected = o.source.sha256.clone().unwrap_or_default();
            if !unchanged(dp, &o.destination) {
                return Err(changed_since_preview());
            }
            journal.record(entry(Before::Missing, Some(expected.clone())))?;
            publish(sp, dp, &expected, Place::New, &prefix, stop)
        }
        Action::Replace => {
            let expected = o.source.sha256.clone().unwrap_or_default();
            let (before, stamp) =
                journal.keep(dp, o.destination.sha256.as_deref().unwrap_or(""), stop)?;
            journal.record(entry(before, Some(expected.clone())))?;
            publish(sp, dp, &expected, Place::Over(Some(stamp)), &prefix, stop)
        }
        Action::Delete => {
            // Only what the source verifiably no longer has is deleted.
            if Precondition::read(sp, false).kind != Kind::Missing {
                return Err(changed_since_preview());
            }
            let (before, stamp) =
                journal.keep(dp, o.destination.sha256.as_deref().unwrap_or(""), stop)?;
            journal.record(entry(before, None))?;
            if Stamp::at(dp) != Some(stamp) {
                return Err(changed_since_preview());
            }
            remove_file(dp)
        }
        Action::RemoveDir => {
            if Precondition::read(sp, false).kind != Kind::Missing || !unchanged(dp, &o.destination)
            {
                return Err(changed_since_preview());
            }
            journal.record(entry(Before::Dir, None))?;
            if std::fs::remove_dir(dp).is_err() {
                kept.push(o.path.clone());
            }
            Ok(())
        }
        Action::Skip | Action::Conflict => Ok(()),
    }
}

/// What the destination link `path` was, for the journal, refused unless it is still the
/// planned `expected` one.
fn keep_link(path: &Path, expected: &str) -> std::io::Result<Before> {
    let target = planned_link(path, expected)?;
    Ok(Before::Link {
        target: target.to_str().unwrap_or_default().to_string(),
        sha256: expected.to_string(),
    })
}

/// A name for a file being moved into `dir`, not yet taken, which no scan of a temporary file
/// takes for a leftover copy: recovery finds it through the journal.
fn move_name(dir: &Path, plan_id: &str, n: usize) -> PathBuf {
    let base = format!(
        ".oxidemerge-move-{}-{n}",
        plan_id.get(..12).unwrap_or(plan_id)
    );
    (0..)
        .map(|k| {
            dir.join(if k == 0 {
                base.clone()
            } else {
                format!("{base}-{k}")
            })
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("a free name")
}

/// Renames `from` to `to`, refusing when `to` names anything, also under another case or
/// Unicode form.
fn move_new(from: &Path, to: &Path) -> std::io::Result<()> {
    // Something put there after the checks: changed since the preview.
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(changed_since_preview());
    }
    std::fs::rename(fsutil::long_path(from), fsutil::long_path(to))?;
    for dir in [from.parent(), to.parent()].into_iter().flatten() {
        let _ = sync_dir(dir);
    }
    Ok(())
}

/// Makes the copy `o` by moving the destination file that `old`, its rename, deletes, after
/// checking all four paths are as planned. `(sp, dp)` are the copy's paths and `(osp, odp)`
/// the deletion's.
fn step_move(
    journal: &mut Journal,
    o: &SyncOperation,
    old: &SyncOperation,
    (sp, dp, osp, odp): (&Path, &Path, &Path, &Path),
    fault: Faults,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let exact = respelled(o);
    let read = |p: &Path, hash: bool| {
        if exact {
            read_exact(p, hash, stop)
        } else {
            Precondition::read_until(p, hash, stop)
        }
    };
    let expected = o.source.sha256.clone().unwrap_or_default();
    let stamp = Stamp::at(odp);
    let there = read(odp, true);
    if !o.source.matches(&read(sp, true))
        || read(dp, false).kind != Kind::Missing
        || read(osp, false).kind != Kind::Missing
        || !old.destination.matches(&there)
        || there.sha256.as_deref() != Some(expected.as_str())
        || stamp.is_none()
    {
        return Err(changed_since_preview());
    }
    let dir = dp
        .parent()
        .ok_or_else(|| std::io::Error::other(tr("folder_sync.err_not_file")))?;
    let via = move_name(dir, &journal.header.plan, journal.entries.len());
    let via_name = via
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    journal.record(Entry {
        path: o.path.clone(),
        before: Before::Moved {
            from: old.path.clone(),
            via: via_name,
        },
        written: Some(expected),
    })?;
    if Stamp::at(odp) != stamp {
        return Err(changed_since_preview());
    }
    move_new(odp, &via)?;
    match fault(Point::Moving, &o.path) {
        Fault::None => {}
        Fault::Crash => return Err(std::io::Error::other(Crash)),
        _ => return Err(injected()),
    }
    move_new(&via, dp)
}

/// Puts back what `entries` changed, last first. A path changed again since is left as it is
/// and reported.
fn roll_back(header: &Header, entries: &[Entry], fault: Faults) -> RollbackOutcome {
    let root = &header.roots[header.destination];
    let prefix = temp_prefix(&header.plan);
    let mut out = RollbackOutcome::default();
    let mut folders = std::collections::BTreeSet::new();
    for e in entries.iter().rev() {
        let target = under(root, &e.path);
        let result = if fault(Point::Rollback, &e.path) != Fault::None {
            Err(injected())
        } else if escapes(root, &e.path, &target) {
            Err(std::io::Error::other(trf(
                "folder_sync.err_outside",
                &[("path", e.path.clone())],
            )))
        } else {
            folders.extend(target.parent().map(Path::to_path_buf));
            undo(&header.dir, e, &target, &prefix)
        };
        match result {
            Ok(true) => out.restored += 1,
            Ok(false) => {}
            Err(err) => out.failed.push((e.path.clone(), err.to_string())),
        }
    }
    // Left by copying when the program ended part way.
    for dir in folders {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in rd.flatten() {
            let ours = item
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix));
            if ours
                && item
                    .file_type()
                    .is_ok_and(|t| t.is_file() || t.is_symlink())
            {
                let _ = remove_file(&item.path());
            }
        }
    }
    out
}

/// Removes the journal once everything is put back, else keeps it with its copies.
fn settle(dir: &Path, file: std::fs::File, out: &mut RollbackOutcome) {
    if out.failed.is_empty() {
        let _ = remove_journal(dir, file);
        out.journal = None;
    } else {
        out.journal = Some(dir.to_path_buf());
    }
}

/// Removes the journal in `dir` whose locked file is `file`: its header first, so another
/// window never offers it for recovery once the lock is let go.
fn remove_journal(dir: &Path, file: std::fs::File) -> std::io::Result<()> {
    std::fs::remove_file(dir.join(HEADER_FILE))?;
    drop(file);
    std::fs::remove_dir_all(dir)
}

/// Puts back one path; whether it had to be changed.
fn undo(dir: &Path, e: &Entry, target: &Path, prefix: &str) -> std::io::Result<bool> {
    let stamp = Stamp::at(target);
    let now = Precondition::read(target, true);
    let not_ours = || std::io::Error::other(tr("folder_sync.err_changed_after"));
    match &e.before {
        Before::Missing => match now.kind {
            Kind::Missing => Ok(false),
            // Only a folder this created; one where a file was copied is someone else's.
            Kind::Dir if e.written.is_none() => std::fs::remove_dir(target).map(|()| true),
            Kind::File if now.sha256.is_some() && now.sha256 == e.written => {
                if Stamp::at(target) != stamp {
                    return Err(not_ours());
                }
                remove_file(target).map(|()| true)
            }
            Kind::Link if now.sha256.is_some() && now.sha256 == e.written => {
                remove_file(target).map(|()| true)
            }
            _ => Err(not_ours()),
        },
        Before::Link { target: to, sha256 } => {
            if link_hash(Path::new(to)).as_ref() != Some(sha256) {
                return Err(not_ours());
            }
            match now.kind {
                Kind::Link if now.sha256.as_ref() == Some(sha256) => Ok(false),
                Kind::Missing => make_link(Path::new(to), target).map(|()| true),
                Kind::Link if now.sha256.is_some() && now.sha256 == e.written => {
                    let written = e.written.clone().unwrap_or_default();
                    replace_link(Path::new(to), target, &written, prefix).map(|()| true)
                }
                _ => Err(not_ours()),
            }
        }
        Before::Dir => match now.kind {
            Kind::Dir => Ok(false),
            Kind::Missing => std::fs::create_dir_all(target).map(|()| true),
            _ => Err(not_ours()),
        },
        Before::Moved { from, via } => {
            let root = target
                .ancestors()
                .nth(e.path.split('/').count())
                .unwrap_or(target);
            let from_rel = from;
            let from = under(root, from_rel);
            // Read from a file: only a name of the kind this writes, beside the target.
            if escapes(root, from_rel, &from)
                || !via.starts_with(".oxidemerge-move-")
                || via.contains(['/', '\\'])
                || via.contains("..")
            {
                return Err(std::io::Error::other(trf(
                    "folder_sync.err_outside",
                    &[("path", from_rel.clone())],
                )));
            }
            let via = target.with_file_name(via);
            let ours = |p: &Precondition| p.kind == Kind::File && p.sha256 == e.written;
            // Where the move got to: at the new name, at the temporary one, or not started.
            if named_exactly(target) && ours(&now) {
                if read_exact(&from, false, &NEVER).kind != Kind::Missing
                    || Stamp::at(target) != stamp
                {
                    return Err(not_ours());
                }
                move_new(target, &via)?;
                move_new(&via, &from).map(|()| true)
            } else if ours(&Precondition::read(&via, true)) {
                if read_exact(&from, false, &NEVER).kind != Kind::Missing {
                    return Err(not_ours());
                }
                move_new(&via, &from).map(|()| true)
            } else if ours(&read_exact(&from, true, &NEVER)) {
                Ok(false)
            } else {
                Err(not_ours())
            }
        }
        Before::File { backup, sha256 } => {
            if now.sha256.as_ref() == Some(sha256) {
                return Ok(false);
            }
            let place = match now.kind {
                Kind::Missing => Place::New,
                _ if now.sha256.is_some() && now.sha256 == e.written => Place::Over(stamp),
                _ => return Err(not_ours()),
            };
            publish(&dir.join(backup), target, sha256, place, prefix, &NEVER).map(|()| true)
        }
    }
}

/// A synchronization that did not finish: the program ended while applying it, or undoing it
/// failed.
#[derive(Clone, Debug)]
pub struct Unfinished {
    pub dir: PathBuf,
    pub roots: [PathBuf; 2],
    pub destination: usize,
    /// The changes recorded, some of which may not have been made.
    pub changes: usize,
}

fn read_header(dir: &Path) -> std::io::Result<Header> {
    let mut header: Header = serde_json::from_slice(&std::fs::read(dir.join(HEADER_FILE))?)
        .map_err(std::io::Error::other)?;
    if header.roots.iter().any(|r| !r.is_absolute()) || header.destination > 1 {
        return Err(std::io::Error::other(tr("folder_sync.journal")));
    }
    header.dir = dir.to_path_buf();
    Ok(header)
}

/// Removes a journal the program ended in before it recorded anything, so before it
/// changed anything.
fn remove_if_empty(dir: &Path) {
    let old = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() > 60));
    if !old {
        return;
    }
    let empty = match claim(dir) {
        Ok(Some(file)) => file.metadata().is_ok_and(|m| m.len() == 0),
        Ok(None) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    if empty
        && dir
            .join("files")
            .read_dir()
            .is_ok_and(|mut f| f.next().is_none())
    {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The changes recorded in a journal file. A last line cut short by the end of the program is
/// not a change that was made.
fn read_entries(file: &std::fs::File) -> Vec<Entry> {
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .map_while(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Every journal under `journal_root`, also those in use.
fn journals(journal_root: &Path) -> Vec<Header> {
    let Ok(rd) = std::fs::read_dir(journal_root) else {
        return Vec::new();
    };
    let mut found: Vec<Header> = rd
        .filter_map(Result::ok)
        .filter_map(|e| match read_header(&e.path()) {
            Ok(h) => Some(h),
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    remove_if_empty(&e.path());
                }
                None
            }
        })
        .collect();
    found.sort_by_key(|h| h.started);
    found
}

/// Opens and locks the journal file of `dir`; `None` while a synchronization uses it.
fn claim(dir: &Path) -> std::io::Result<Option<std::fs::File>> {
    claim_all(&[dir]).pop().unwrap_or(Ok(None))
}

/// [`claim`]s the journal files of `dirs` together, so the tries for those in use take no
/// longer than for one.
fn claim_all(dirs: &[&Path]) -> Vec<std::io::Result<Option<std::fs::File>>> {
    let open = |dir: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(dir.join(JOURNAL_FILE))
    };
    let mut files: Vec<std::io::Result<(std::fs::File, bool)>> =
        dirs.iter().map(|d| open(d).map(|f| (f, false))).collect();
    // A process being started copies every open file until it runs its program, and with it
    // the lock of a synchronization that has just let it go: a few short tries outlast that.
    for tries in 1..=10 {
        let mut waiting = false;
        for f in &mut files {
            let Ok((file, locked)) = f else {
                continue;
            };
            if *locked {
                continue;
            }
            match file.try_lock() {
                Ok(()) => *locked = true,
                Err(std::fs::TryLockError::WouldBlock) => waiting = true,
                Err(std::fs::TryLockError::Error(e)) => *f = Err(e),
            }
        }
        if !waiting {
            break;
        }
        if tries < 10 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    files
        .into_iter()
        .map(|f| f.map(|(file, locked)| locked.then_some(file)))
        .collect()
}

/// The unfinished synchronizations recorded under `journal_root`, oldest first.
pub fn unfinished(journal_root: &Path) -> Vec<Unfinished> {
    let found = journals(journal_root);
    let dirs: Vec<&Path> = found.iter().map(|h| h.dir.as_path()).collect();
    let files = claim_all(&dirs);
    found
        .into_iter()
        .zip(files)
        .filter_map(|(h, file)| {
            let file = file.ok()??;
            // The header was read before the lock was taken. A synchronization that finishes in
            // between removes its header and only then lets the lock go, so the header is read
            // again now that it is held: without this, one that has just succeeded would be
            // offered for undoing, and undoing it would take back correct work.
            read_header(&h.dir).ok()?;
            Some(Unfinished {
                changes: read_entries(&file).len(),
                dir: h.dir,
                roots: h.roots,
                destination: h.destination,
            })
        })
        .collect()
}

/// What became of a synchronization of `roots` that ended without saying, as when the work
/// failed in a way nothing caught. A journal it left behind holds what it changed, to be undone
/// from there; without one, nothing was started or everything was finished.
pub fn abandoned(journal_root: Option<&Path>, roots: &[PathBuf], why: String) -> SyncOutcome {
    let same = |a: &Path, b: &Path| {
        std::fs::canonicalize(a)
            .ok()
            .is_some_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
    };
    let left = journal_root
        .map(unfinished)
        .unwrap_or_default()
        .into_iter()
        .find(|u| u.roots.iter().any(|j| roots.iter().any(|r| same(j, r))));
    match left {
        Some(u) => SyncOutcome::Stopped {
            reason: Some(why),
            failure: Some(Problem::RecoveryNeeded),
            rollback: RollbackOutcome {
                journal: Some(u.dir),
                ..RollbackOutcome::default()
            },
        },
        None => SyncOutcome::NotApplied(Problem::WriteFailure, vec![why]),
    }
}

/// Undoes an unfinished synchronization from its journal.
pub fn recover(u: &Unfinished) -> RollbackOutcome {
    let failed = |e: String| RollbackOutcome {
        failed: vec![(u.dir.display().to_string(), e)],
        journal: Some(u.dir.clone()),
        ..RollbackOutcome::default()
    };
    // The lock first, then the header, so a synchronization that finished since this one was
    // listed cannot be undone: it removes its header before letting the lock go, and a journal
    // without a header is not one that did not finish.
    let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    let file = match claim(&u.dir) {
        Ok(Some(f)) => f,
        Ok(None) => return failed(tr("folder_sync.err_in_use").to_string()),
        Err(e) if gone(&e) => return RollbackOutcome::default(),
        Err(e) => return failed(e.to_string()),
    };
    let header = match read_header(&u.dir) {
        Ok(h) if h.version > JOURNAL_VERSION => {
            return failed(tr("folder_sync.err_newer_journal").to_string())
        }
        Ok(h) => h,
        Err(e) if gone(&e) => return RollbackOutcome::default(),
        Err(e) => return failed(e.to_string()),
    };
    let mut out = roll_back(&header, &read_entries(&file), &|_, _| Fault::None);
    settle(&u.dir, file, &mut out);
    out
}

/// Forgets an unfinished synchronization, leaving the folders as they are now.
pub fn discard(u: &Unfinished) -> std::io::Result<()> {
    match claim(&u.dir) {
        Ok(Some(file)) => remove_journal(&u.dir, file),
        Ok(None) => Err(std::io::Error::other(tr("folder_sync.err_in_use"))),
        // Already gone, which is what this asks for.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Mirrors `roots[0]` to `roots[1]` until the first file is written, then ends as if the
/// program had, leaving its journal under `journal_root`: for tests elsewhere of what finds
/// and undoes such a journal.
#[cfg(test)]
pub(crate) fn mirror_then_crash(roots: &[PathBuf], journal_root: &Path) -> SyncOutcome {
    use crate::core::diff::DiffOptions;
    use crate::core::filter::FileFilter;
    use crate::core::folder::{FolderOptions, Progress, Scanner};
    let items = Scanner {
        roots,
        filter: &FileFilter::parse_mask("*.*"),
        fopts: &FolderOptions::default(),
        dopts: &DiffOptions::default(),
        progress: &Progress::default(),
    }
    .run();
    let policy = SyncPolicy {
        mode: SyncMode::Mirror,
        source: Side::Left,
    };
    let scope = Scope {
        complete: true,
        excluded: false,
        filter: String::new(),
        skip_hidden: false,
        copy_links: false,
    };
    let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
    let plan = plan(&items, roots, policy, scope, &stop).unwrap();
    apply_with(&plan, roots, journal_root, &stop, &done, &|p, _| match p {
        Point::Written => Fault::Crash,
        _ => Fault::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::DiffOptions;
    use crate::core::filter::FileFilter;
    use crate::core::folder::{FolderOptions, Progress, Scanner};
    use std::collections::BTreeMap;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn scan_with(roots: &[PathBuf], filter: &str) -> Vec<DirItem> {
        Scanner {
            roots,
            filter: &FileFilter::parse_mask(filter),
            fopts: &FolderOptions::default(),
            dopts: &DiffOptions::default(),
            progress: &Progress::default(),
        }
        .run()
    }

    fn scan(roots: &[PathBuf]) -> Vec<DirItem> {
        scan_with(roots, "*.*")
    }

    struct Folders {
        _dirs: Vec<tempfile::TempDir>,
        roots: Vec<PathBuf>,
        journal: PathBuf,
    }

    fn folders() -> Folders {
        let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let roots = dirs[..2].iter().map(|d| d.path().to_path_buf()).collect();
        let journal = dirs[2].path().join("journal");
        Folders {
            _dirs: dirs,
            roots,
            journal,
        }
    }

    /// Left: a.txt, d/b.txt, same.txt. Right: an older a.txt, same.txt, old.txt, gone/c.txt.
    fn sample() -> Folders {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "a.txt", "new a\n");
        write(l, "d/b.txt", "b\n");
        write(l, "same.txt", "same\n");
        write(r, "a.txt", "old a\n");
        write(r, "same.txt", "same\n");
        write(r, "old.txt", "old\n");
        write(r, "gone/c.txt", "c\n");
        f
    }

    const MIRROR: SyncPolicy = SyncPolicy {
        mode: SyncMode::Mirror,
        source: Side::Left,
    };
    const UPDATE: SyncPolicy = SyncPolicy {
        mode: SyncMode::Update,
        source: Side::Left,
    };
    const ALL: Scope = Scope {
        complete: true,
        excluded: false,
        filter: String::new(),
        skip_hidden: false,
        copy_links: false,
    };

    fn make(f: &Folders, policy: SyncPolicy) -> SyncPlan {
        plan(
            &scan(&f.roots),
            &f.roots,
            policy,
            ALL,
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn run(f: &Folders, plan: &SyncPlan, faults: Faults) -> SyncOutcome {
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        apply_with(plan, &f.roots, &f.journal, &stop, &done, faults)
    }

    /// Every path under `root` with its contents (folders as `/`) and time.
    fn tree(root: &Path) -> BTreeMap<String, (String, Option<SystemTime>)> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (String, Option<SystemTime>)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let md = std::fs::symlink_metadata(&p).unwrap();
                if md.is_dir() {
                    out.insert(rel, ("/".into(), None));
                    walk(root, &p, out);
                } else {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    out.insert(rel, (text, md.modified().ok()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn ops(plan: &SyncPlan) -> Vec<(Action, &str)> {
        plan.operations
            .iter()
            .map(|o| (o.action, o.path.as_str()))
            .collect()
    }

    #[test]
    fn mirroring_makes_the_destination_match_and_deletes_last() {
        let f = sample();
        let left = tree(&f.roots[0]);
        let plan = make(&f, MIRROR);
        assert_eq!(
            ops(&plan),
            [
                (Action::CreateDir, "d"),
                (Action::Replace, "a.txt"),
                (Action::Copy, "d/b.txt"),
                (Action::Delete, "old.txt"),
                (Action::Delete, "gone/c.txt"),
                (Action::RemoveDir, "gone"),
            ]
        );
        assert_eq!(plan.estimate.deletes, 3);
        assert_eq!(plan.estimate.bytes, 8);
        let SyncOutcome::Applied { changes, kept } = run(&f, &plan, &|_, _| Fault::None) else {
            panic!("not applied");
        };
        assert_eq!((changes, kept.len()), (6, 0));
        assert_eq!(tree(&f.roots[0]), left);
        let strip = |t: BTreeMap<String, (String, Option<SystemTime>)>| {
            t.into_iter().map(|(k, (v, _))| (k, v)).collect::<Vec<_>>()
        };
        assert_eq!(strip(tree(&f.roots[1])), strip(left.clone()));
        assert_eq!(
            std::fs::metadata(f.roots[1].join("a.txt"))
                .unwrap()
                .modified()
                .ok(),
            left["a.txt"].1
        );
        assert!(unfinished(&f.journal).is_empty());
        assert_eq!(std::fs::read_dir(&f.journal).unwrap().count(), 0);
    }

    #[test]
    fn updating_copies_and_replaces_but_never_deletes() {
        let f = sample();
        let plan = make(&f, UPDATE);
        assert_eq!(
            ops(&plan),
            [
                (Action::CreateDir, "d"),
                (Action::Replace, "a.txt"),
                (Action::Copy, "d/b.txt"),
                (Action::Skip, "gone"),
                (Action::Skip, "old.txt"),
            ]
        );
        assert!(matches!(
            run(&f, &plan, &|_, _| Fault::None),
            SyncOutcome::Applied { changes: 3, .. }
        ));
        assert!(f.roots[1].join("old.txt").exists());
        assert!(f.roots[1].join("gone/c.txt").exists());
    }

    #[test]
    fn planning_and_exporting_change_neither_folder() {
        let f = sample();
        let before = (tree(&f.roots[0]), tree(&f.roots[1]));
        let plan = make(&f, MIRROR);
        let json = plan.to_json().unwrap();
        assert_eq!((tree(&f.roots[0]), tree(&f.roots[1])), before);
        assert!(!f.journal.exists());
        for root in &f.roots {
            assert!(!json.contains(root.to_str().unwrap()));
        }
        let back = SyncPlan::from_json(&json).unwrap();
        assert_eq!(back, plan);
        assert_eq!(
            make(&f, MIRROR).id,
            plan.id,
            "the same folders plan the same id"
        );
    }

    #[test]
    fn a_plan_file_altered_or_from_a_newer_version_is_refused() {
        let f = sample();
        let json = make(&f, MIRROR).to_json().unwrap();
        let altered = json.replace("\"old.txt\"", "\"same.txt\"");
        assert_eq!(
            SyncPlan::from_json(&altered).unwrap_err(),
            tr("folder_sync.err_plan_altered")
        );
        let newer = json.replacen("\"version\": 1", "\"version\": 2", 1);
        assert_ne!(newer, json);
        assert!(SyncPlan::from_json(&newer).unwrap_err().contains('2'));
    }

    #[test]
    fn a_file_renamed_in_the_source_shows_as_a_rename_and_is_moved() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "docs/new name.txt", "moved\n");
        write(r, "old name.txt", "moved\n");
        // Contents found twice say nothing of which file moved, nor do empty ones.
        write(l, "twin1.txt", "twin\n");
        write(l, "twin2.txt", "twin\n");
        write(r, "twin.txt", "twin\n");
        write(l, "empty-new.txt", "");
        write(r, "empty-old.txt", "");
        let p = make(&f, MIRROR);
        let renamed: Vec<_> = p
            .operations
            .iter()
            .filter_map(|o| Some((o.action, o.path.as_str(), o.renamed.as_deref()?)))
            .collect();
        assert_eq!(
            renamed,
            [
                (Action::Copy, "docs/new name.txt", "old name.txt"),
                (Action::Delete, "old name.txt", "docs/new name.txt"),
            ]
        );
        let e = &p.estimate;
        assert_eq!((e.renames, e.copies, e.deletes), (1, 4, 3));
        assert_eq!(SyncPlan::from_json(&p.to_json().unwrap()).unwrap(), p);
        assert!(matches!(
            run(&f, &p, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        assert!(!r.join("old name.txt").exists());
        assert_eq!(
            std::fs::read_to_string(r.join("docs/new name.txt")).unwrap(),
            "moved\n"
        );
    }

    #[test]
    fn a_renamed_file_with_another_time_is_copied_so_it_gets_the_sources_time() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "new.txt", "moved\n");
        write(r, "old.txt", "moved\n");
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(r.join("old.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let p = make(&f, MIRROR);
        assert_eq!(p.estimate.renames, 1);
        let find = |a| p.operations.iter().find(|o| o.action == a).unwrap();
        let (copy, delete) = (find(Action::Copy), find(Action::Delete));
        let (s, d) = (p.policy.source(), p.policy.destination());
        assert!(!moves(copy, delete, &f.roots, s, d));
        assert!(matches!(
            run(&f, &p, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        assert!(!r.join("old.txt").exists());
        let time = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(time(r.join("new.txt")), time(l.join("new.txt")));
    }

    /// The names under `root` that a synchronization leaves only while it runs.
    fn leftovers(root: &Path) -> Vec<String> {
        tree(root)
            .into_keys()
            .filter(|k| k.contains(".oxidemerge-"))
            .collect()
    }

    #[test]
    fn a_renamed_file_is_moved_in_the_destination_rather_than_copied() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "new/name.txt", "moved\n");
        write(r, "old.txt", "moved\n");
        let p = make(&f, MIRROR);
        assert_eq!(p.estimate.renames, 1);
        #[cfg(unix)]
        let inode =
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(r.join("old.txt")).unwrap());
        // Nothing is kept in the journal for a file that is only moved.
        let kept = std::cell::Cell::new(0);
        let outcome = run(&f, &p, &|point, _| {
            if point == Point::Written {
                kept.set(
                    std::fs::read_dir(&f.journal)
                        .unwrap()
                        .flatten()
                        .map(|j| std::fs::read_dir(j.path().join("files")).unwrap().count())
                        .sum(),
                );
            }
            Fault::None
        });
        assert!(
            matches!(outcome, SyncOutcome::Applied { changes: 3, .. }),
            "{outcome:?}"
        );
        assert_eq!(kept.get(), 0);
        assert!(!r.join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(r.join("new/name.txt")).unwrap(),
            "moved\n"
        );
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(
                &std::fs::metadata(r.join("new/name.txt")).unwrap()
            ),
            inode
        );
        assert!(leftovers(r).is_empty(), "{:?}", leftovers(r));
        assert!(unfinished(&f.journal).is_empty());
    }

    #[test]
    fn a_rename_that_fails_part_way_or_after_is_moved_back() {
        for (point, path) in [
            (Point::Moving, "b.txt"),
            (Point::Written, "b.txt"),
            (Point::Before, "x.txt"),
        ] {
            let f = folders();
            let (l, r) = (&f.roots[0], &f.roots[1]);
            write(l, "b.txt", "same\n");
            write(l, "x.txt", "new\n");
            write(r, "a.txt", "same\n");
            let before = tree(r);
            let p = make(&f, MIRROR);
            let outcome = run(&f, &p, &|at, at_path| {
                if at == point && at_path == path {
                    Fault::Fail
                } else {
                    Fault::None
                }
            });
            let SyncOutcome::Stopped { rollback, .. } = outcome else {
                panic!("not stopped at {point:?}");
            };
            assert!(rollback.complete(), "{point:?}: {rollback:?}");
            assert_eq!(tree(r), before, "{point:?}");
            assert!(unfinished(&f.journal).is_empty());
        }
    }

    #[test]
    fn a_rename_the_program_ended_in_is_undone_on_recovery() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "b.txt", "same\n");
        write(r, "a.txt", "same\n");
        let before = tree(r);
        let p = make(&f, MIRROR);
        let outcome = run(&f, &p, &|at, _| {
            if at == Point::Moving {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(outcome, SyncOutcome::Stopped { .. }));
        // The file is at its temporary name, which only the journal knows.
        assert!(!r.join("a.txt").exists() && !r.join("b.txt").exists());
        assert_eq!(leftovers(r).len(), 1);
        let left = unfinished(&f.journal);
        assert_eq!(left.len(), 1);
        assert!(recover(&left[0]).complete());
        assert_eq!(tree(r), before);
    }

    #[test]
    fn a_journal_naming_a_place_outside_the_folder_is_not_followed() {
        let f = folders();
        let r = &f.roots[1];
        write(r, "b.txt", "x");
        let header = Header {
            version: JOURNAL_VERSION,
            plan: "0".repeat(64),
            roots: [f.roots[0].clone(), r.clone()],
            destination: 1,
            started: 0,
            dir: f.journal.clone(),
        };
        let entry = |from: &str, via: &str| Entry {
            path: "b.txt".into(),
            before: Before::Moved {
                from: from.into(),
                via: via.into(),
            },
            written: None,
        };
        for e in [
            entry("../outside.txt", ".oxidemerge-move-1"),
            entry("a.txt", "../x"),
        ] {
            let out = roll_back(&header, &[e], &|_, _| Fault::None);
            assert_eq!(out.failed.len(), 1);
        }
        assert_eq!(std::fs::read_to_string(r.join("b.txt")).unwrap(), "x");
    }

    /// Whether names that differ in case reach the same entry under `dir`.
    fn ignores_case(dir: &Path) -> bool {
        write(dir, "probe.txt", "");
        let same = dir.join("PROBE.TXT").exists();
        std::fs::remove_file(dir.join("probe.txt")).unwrap();
        same
    }

    #[test]
    fn a_rename_only_in_case_is_made_even_where_case_is_ignored() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(l, "README.md", "readme\n");
        write(r, "readme.md", "readme\n");
        let p = make(&f, MIRROR);
        let renamed: Vec<_> = p
            .operations
            .iter()
            .filter_map(|o| Some((o.action, o.path.as_str(), o.renamed.as_deref()?)))
            .collect();
        assert_eq!(
            renamed,
            [
                (Action::Copy, "README.md", "readme.md"),
                (Action::Delete, "readme.md", "README.md"),
            ],
            "{:?}",
            ops(&p)
        );
        assert_eq!(p.operations.len(), 2, "{:?}", ops(&p));
        assert_eq!(p.estimate.deletes, 1);
        // Undone when it fails after the move.
        let outcome = run(&f, &p, &|at, _| {
            if at == Point::Written {
                Fault::Fail
            } else {
                Fault::None
            }
        });
        assert!(matches!(&outcome, SyncOutcome::Stopped { rollback, .. } if rollback.complete()));
        let names = |root: &Path| -> Vec<String> {
            std::fs::read_dir(root)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        };
        assert_eq!(names(r), ["readme.md"]);
        assert!(matches!(
            run(&f, &make(&f, MIRROR), &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        assert_eq!(names(r), ["README.md"]);
        assert_eq!(
            std::fs::read_to_string(r.join("README.md")).unwrap(),
            "readme\n"
        );
    }

    #[test]
    fn a_rename_only_in_unicode_form_is_made_too() {
        let f = folders();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        let (composed, decomposed) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
        write(l, composed, "menu\n");
        write(r, decomposed, "menu\n");
        let p = make(&f, MIRROR);
        assert_eq!(p.estimate.renames, 1, "{:?}", ops(&p));
        assert!(matches!(
            run(&f, &p, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        let names: Vec<String> = std::fs::read_dir(r)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [composed]);
    }

    #[test]
    fn a_rename_only_in_case_with_new_contents_is_left_alone() {
        let f = folders();
        write(&f.roots[0], "Notes.txt", "new\n");
        write(&f.roots[1], "notes.txt", "old\n");
        let p = make(&f, MIRROR);
        if ignores_case(&f.roots[1]) {
            assert!(
                p.operations
                    .iter()
                    .all(|o| o.action == Action::Conflict && o.reason == Reason::NameClash),
                "{:?}",
                ops(&p)
            );
        } else {
            // Two names to a file system that keeps case apart.
            assert_eq!(
                ops(&p),
                [(Action::Copy, "Notes.txt"), (Action::Delete, "notes.txt")]
            );
        }
    }

    #[test]
    fn the_sample_folders_plan_every_kind_of_change() {
        let f = folders();
        for (root, side) in f.roots.iter().zip(["source", "destination"]) {
            crate::core::folder::copy_recursive(&Path::new("samples/sync").join(side), root)
                .unwrap();
        }
        let p = make(&f, MIRROR);
        assert_eq!(
            ops(&p),
            [
                (Action::CreateDir, "reports"),
                (Action::Replace, "README.txt"),
                (Action::Copy, "new.txt"),
                (Action::Copy, "reports/2026-q3.txt"),
                (Action::Delete, "q3-report.txt"),
                (Action::Delete, "obsolete.txt"),
            ]
        );
        assert_eq!(p.operations[3].renamed.as_deref(), Some("q3-report.txt"));
    }

    #[test]
    fn a_plan_records_its_filter_and_is_applied_only_to_its_folders() {
        let f = sample();
        let p = make(&f, MIRROR);
        assert!(!p.folders.is_empty());
        let json = p.to_json().unwrap();
        for root in &f.roots {
            assert!(!json.contains(root.to_str().unwrap()));
        }
        let filtered = plan(
            &scan(&f.roots),
            &f.roots,
            MIRROR,
            Scope {
                filter: "*.txt".into(),
                ..ALL
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(filtered.filter, "*.txt");
        assert_ne!(filtered.id, p.id);
        assert!(!p.agrees_with(&filtered));
        let elsewhere = sample();
        let before = tree(&elsewhere.roots[1]);
        let SyncOutcome::NotApplied(_, errors) = run(&elsewhere, &p, &|_, _| Fault::None) else {
            panic!("applied to other folders");
        };
        assert_eq!(errors, [tr("folder_sync.err_other_folders")]);
        assert_eq!(tree(&elsewhere.roots[1]), before);
        // A plan without the folders it was made for is applied to none.
        let mut bare = p.clone();
        bare.folders.clear();
        bare.id = bare.identity();
        let json = bare.to_json().unwrap();
        assert!(!json.contains("\"folders\""));
        assert_eq!(SyncPlan::from_json(&json).unwrap(), bare);
        let SyncOutcome::NotApplied(_, errors) = run(&f, &bare, &|_, _| Fault::None) else {
            panic!("applied without its folders");
        };
        assert_eq!(errors, [tr("folder_sync.err_other_folders")]);
    }

    #[test]
    fn a_plan_for_folders_on_a_server_applies_to_a_new_copy_of_them() {
        let names = [
            "sftp://ann@example.com/srv/src",
            "sftp://ann@example.com/srv/dst",
        ];
        let named = |f: &Folders, which: &[usize]| {
            for &i in which {
                name_folder(&f.roots[i], names[i]);
            }
        };
        let f = sample();
        named(&f, &[0, 1]);
        let p = make(&f, MIRROR);
        let half = sample();
        named(&half, &[1]);
        let SyncOutcome::NotApplied(_, errors) = run(&half, &p, &|_, _| Fault::None) else {
            panic!("applied to a folder of another name");
        };
        assert_eq!(errors, [tr("folder_sync.err_other_folders")]);
        let copy = sample();
        named(&copy, &[0, 1]);
        assert!(matches!(
            run(&copy, &p, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        let dst = &copy.roots[1];
        assert_eq!(
            std::fs::read_to_string(dst.join("a.txt")).unwrap(),
            "new a\n"
        );
        assert!(!dst.join("old.txt").exists());
    }

    #[test]
    fn a_destination_edited_after_the_preview_with_its_time_restored_is_refused() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let a = f.roots[1].join("a.txt");
        let time = std::fs::metadata(&a).unwrap().modified().unwrap();
        std::fs::write(&a, "OLD A\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(time)
            .unwrap();
        let before = (tree(&f.roots[0]), tree(&f.roots[1]));
        let SyncOutcome::NotApplied(_, errors) = run(&f, &plan, &|_, _| Fault::None) else {
            panic!("applied a stale plan");
        };
        assert_eq!(
            errors,
            [trf("folder_sync.err_stale", &[("path", "a.txt".into())])]
        );
        assert_eq!((tree(&f.roots[0]), tree(&f.roots[1])), before);
        assert!(unfinished(&f.journal).is_empty());
    }

    #[test]
    fn a_source_file_changed_after_the_preview_is_refused() {
        let f = sample();
        let plan = make(&f, MIRROR);
        write(&f.roots[0], "d/b.txt", "B\n");
        assert!(matches!(
            run(&f, &plan, &|_, _| Fault::None),
            SyncOutcome::NotApplied(_, _)
        ));
        assert!(!f.roots[1].join("d").exists());
    }

    #[test]
    fn a_failure_part_way_undoes_every_change_in_reverse() {
        for (point, at) in [
            (Point::Before, "old.txt"),
            (Point::Written, "a.txt"),
            (Point::Before, "gone"),
        ] {
            let f = sample();
            let before = (tree(&f.roots[0]), tree(&f.roots[1]));
            let plan = make(&f, MIRROR);
            let outcome = run(&f, &plan, &|p, path| {
                if p == point && path == at {
                    Fault::Fail
                } else {
                    Fault::None
                }
            });
            let SyncOutcome::Stopped {
                reason: Some(reason),
                rollback,
                ..
            } = outcome
            else {
                panic!("not stopped at {at}");
            };
            assert!(reason.starts_with(at), "{reason}");
            assert!(rollback.complete(), "{:?}", rollback.failed);
            assert_eq!((tree(&f.roots[0]), tree(&f.roots[1])), before, "at {at}");
            assert!(unfinished(&f.journal).is_empty());
        }
    }

    #[test]
    fn too_little_free_space_changes_nothing() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        FREE_SPACE.with(|s| s.set(Some(3)));
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        FREE_SPACE.with(|s| s.set(None));
        assert_eq!(outcome.problem(), Some(Problem::WriteFailure));
        let SyncOutcome::NotApplied(_, errors) = outcome else {
            panic!("applied without room");
        };
        assert!(errors[0].contains("MiB"), "{errors:?}");
        assert_eq!(tree(&f.roots[1]), before);
        assert!(unfinished(&f.journal).is_empty());
        FREE_SPACE.with(|s| s.set(Some(1 << 30)));
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        FREE_SPACE.with(|s| s.set(None));
        assert!(matches!(outcome, SyncOutcome::Applied { .. }));
    }

    /// A file the journal keeps a copy of is replaced or deleted straight after, so on one file
    /// system only the largest of them is there twice at once.
    #[test]
    fn the_space_needed_on_one_file_system_counts_only_the_largest_kept_file_twice() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let destination = &f.roots[plan.policy.destination()];
        std::fs::create_dir_all(&f.journal).unwrap();
        assert!(same_volume(destination, &f.journal));
        let changes = || {
            plan.operations
                .iter()
                .filter(|o| o.action.changes() && o.renamed.is_none())
        };
        let written: u64 = changes()
            .filter(|o| matches!(o.action, Action::Copy | Action::Replace))
            .map(|o| o.bytes)
            .sum();
        let kept: Vec<u64> = changes()
            .filter(|o| matches!(o.action, Action::Replace | Action::Delete))
            .filter_map(|o| o.destination.size)
            .collect();
        assert!(kept.len() > 1, "{kept:?}");
        let need = written + kept.iter().max().unwrap();
        FREE_SPACE.with(|s| s.set(Some(need)));
        let fits = check_space(&plan, &f.roots, &f.journal);
        FREE_SPACE.with(|s| s.set(Some(need - 1)));
        let short = check_space(&plan, &f.roots, &f.journal);
        FREE_SPACE.with(|s| s.set(None));
        assert!(fits.is_ok(), "{fits:?}");
        assert!(short.is_err());
    }

    /// When the work ends without saying how, as when it fails in a way nothing catches, the
    /// journal it left says whether anything is to be undone.
    #[test]
    fn a_synchronization_that_ended_without_an_outcome_is_reported_from_its_journal() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let why = || "failed".to_string();
        let outcome = abandoned(Some(&f.journal), &f.roots, why());
        assert!(
            matches!(outcome, SyncOutcome::NotApplied(..)),
            "{outcome:?}"
        );
        run(&f, &plan, &|p, path| {
            if p == Point::Written && path == "a.txt" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        let outcome = abandoned(Some(&f.journal), &f.roots, why());
        assert_eq!(outcome.problem(), Some(Problem::RecoveryNeeded));
        let SyncOutcome::Stopped { rollback, .. } = outcome else {
            panic!("not reported as needing recovery");
        };
        assert_eq!(
            rollback.journal,
            Some(unfinished(&f.journal)[0].dir.clone())
        );
    }

    #[test]
    fn a_file_system_filling_up_part_way_is_undone() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Before && path == "old.txt" {
                Fault::Full
            } else {
                Fault::None
            }
        });
        assert_eq!(outcome.problem(), Some(Problem::WriteFailure));
        let SyncOutcome::Stopped {
            reason: Some(reason),
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped");
        };
        assert!(reason.starts_with("old.txt"), "{reason}");
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    #[test]
    fn stopping_undoes_what_was_done() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let outcome = apply_with(&plan, &f.roots, &f.journal, &stop, &done, &|p, path| {
            if p == Point::Written && path == "d/b.txt" {
                stop.store(true, Ordering::Relaxed);
            }
            Fault::None
        });
        let SyncOutcome::Stopped {
            reason: None,
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped");
        };
        assert_eq!(rollback.restored, 3);
        assert_eq!(tree(&f.roots[1]), before);
    }

    #[test]
    fn a_synchronization_cut_short_is_found_and_undone_later() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Written && path == "old.txt" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(outcome, SyncOutcome::Stopped { .. }));
        assert!(!f.roots[1].join("old.txt").exists());
        let pending = unfinished(&f.journal);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].roots[1], f.roots[1]);
        assert_eq!(pending[0].changes, 4);
        // No other synchronization of these folders until it is dealt with.
        let SyncOutcome::NotApplied(_, errors) = run(&f, &make(&f, UPDATE), &|_, _| Fault::None)
        else {
            panic!("applied over an unfinished synchronization");
        };
        assert_eq!(errors, [tr("folder_sync.err_unfinished")]);
        let rollback = recover(&pending[0]);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
        assert!(unfinished(&f.journal).is_empty());
    }

    /// A window looking for synchronizations to undo must never find one that is running:
    /// offering it would let someone put back what has just been correctly copied. Looked for
    /// from another thread throughout, since the journal is made, written and removed in steps.
    #[test]
    fn a_synchronization_under_way_is_never_offered_for_recovery() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let watching = AtomicBool::new(true);
        let found = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while watching.load(Ordering::Relaxed) {
                    for u in unfinished(&f.journal) {
                        found.lock().unwrap().push((u.dir.clone(), u.changes));
                    }
                }
            });
            let outcome = run(&f, &plan, &|_, _| Fault::None);
            watching.store(false, Ordering::Relaxed);
            assert!(
                matches!(outcome, SyncOutcome::Applied { .. }),
                "{outcome:?}"
            );
        });
        let offered = found.into_inner().unwrap();
        assert!(offered.is_empty(), "offered while it ran: {offered:?}");
        assert!(unfinished(&f.journal).is_empty());
    }

    #[test]
    fn a_synchronization_still_running_is_not_offered_for_recovery() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let seen = std::cell::Cell::new(None);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Written && path == "a.txt" {
                seen.set(Some((
                    unfinished(&f.journal).len(),
                    journals(&f.journal).len(),
                )));
            }
            Fault::None
        });
        assert!(matches!(outcome, SyncOutcome::Applied { .. }));
        assert_eq!(seen.get(), Some((0, 1)));
    }

    #[test]
    fn recovery_removes_temporary_files_left_by_a_crash_and_nothing_else() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Written && path == "a.txt" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(outcome, SyncOutcome::Stopped { .. }));
        let left = format!("{}abc123", temp_prefix(&plan.id));
        write(&f.roots[1], &left, "half copied");
        write(&f.roots[1], ".oxidemerge-mine", "someone else's");
        let rollback = recover(&unfinished(&f.journal)[0]);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        let mut expected = before;
        expected.insert(
            ".oxidemerge-mine".into(),
            tree(&f.roots[1])[".oxidemerge-mine"].clone(),
        );
        assert_eq!(tree(&f.roots[1]), expected);
    }

    #[test]
    fn a_folder_made_where_a_file_was_copied_is_not_removed_by_recovery() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Written && path == "d/b.txt" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(outcome, SyncOutcome::Stopped { .. }));
        let copied = f.roots[1].join("d/b.txt");
        std::fs::remove_file(&copied).unwrap();
        std::fs::create_dir(&copied).unwrap();
        let rollback = recover(&unfinished(&f.journal)[0]);
        assert_eq!(rollback.failed.len(), 2, "{:?}", rollback.failed);
        assert!(copied.is_dir());
        assert!(rollback.journal.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_names_the_folders_in_full_when_given_relative_ones() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative: Vec<PathBuf> = f
            .roots
            .iter()
            .map(|r| Path::new(&up).join(r.strip_prefix("/").unwrap()))
            .collect();
        assert!(relative.iter().all(|r| r.is_relative() && r.is_dir()));
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let outcome = apply_with(&plan, &relative, &f.journal, &stop, &done, &|p, _| {
            if p == Point::Written {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(outcome, SyncOutcome::Stopped { .. }));
        let pending = unfinished(&f.journal);
        assert!(pending[0].roots.iter().all(|r| r.is_absolute()));
        assert!(recover(&pending[0]).complete());

        // A journal naming a folder relatively is not trusted.
        let dir = f.journal.join("relative");
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join(JOURNAL_FILE), "").unwrap();
        let header = r#"{"version":1,"plan":"x","roots":["a","b"],"destination":1,"started":0}"#;
        std::fs::write(dir.join(HEADER_FILE), header).unwrap();
        assert!(unfinished(&f.journal).is_empty());
    }

    #[test]
    fn journals_of_a_newer_version_are_listed_but_not_undone_and_empty_ones_go() {
        let f = folders();
        let newer = f.journal.join("newer");
        std::fs::create_dir_all(newer.join("files")).unwrap();
        std::fs::write(newer.join(JOURNAL_FILE), "").unwrap();
        let header = serde_json::json!({
            "version": JOURNAL_VERSION + 1,
            "plan": "x",
            "roots": f.roots,
            "destination": 1,
            "started": 0,
        });
        std::fs::write(newer.join(HEADER_FILE), header.to_string()).unwrap();
        // The program ended before writing the header of this one.
        let empty = f.journal.join("empty");
        std::fs::create_dir_all(empty.join("files")).unwrap();
        std::fs::write(empty.join(JOURNAL_FILE), "").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        #[cfg(not(windows))]
        let folder = std::fs::File::open(&empty);
        // Windows opens a folder only with FILE_FLAG_BACKUP_SEMANTICS, and its times only
        // for writing.
        #[cfg(windows)]
        let folder = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(0x0200_0000)
                .open(&empty)
        };
        folder.unwrap().set_modified(old).unwrap();

        let pending = unfinished(&f.journal);
        assert_eq!(pending.len(), 1);
        let rollback = recover(&pending[0]);
        assert_eq!(rollback.failed[0].1, tr("folder_sync.err_newer_journal"));
        assert!(newer.exists());
        assert!(!empty.exists());
    }

    #[test]
    fn a_plan_made_again_after_a_file_was_only_touched_agrees() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(f.roots[0].join("a.txt"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let fresh = make(&f, MIRROR);
        assert_ne!(plan.id, fresh.id);
        assert!(plan.agrees_with(&fresh));
        write(&f.roots[0], "a.txt", "edited");
        assert!(!plan.agrees_with(&make(&f, MIRROR)));
    }

    #[test]
    fn names_alike_but_for_case_or_accents_fold_alike() {
        assert_eq!(fold("Résumé.TXT"), fold("re\u{301}sume\u{301}.txt"));
        assert_eq!(fold("Ångström"), fold("angstrom"));
        assert_ne!(fold("a.txt"), fold("b.txt"));
        assert_eq!(fold("名前.txt"), "名前.txt");
    }

    #[test]
    fn a_rollback_that_fails_keeps_the_journal_and_its_copies() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| match (p, path) {
            (Point::Before, "gone") => Fault::Fail,
            (Point::Rollback, "old.txt") => Fault::Fail,
            _ => Fault::None,
        });
        let SyncOutcome::Stopped { rollback, .. } = outcome else {
            panic!("not stopped");
        };
        assert!(!rollback.complete());
        assert_eq!(rollback.failed.len(), 1);
        assert_eq!(rollback.failed[0].0, "old.txt");
        let journal = rollback.journal.unwrap();
        let kept: Vec<String> = std::fs::read_dir(journal.join("files"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert!(kept.contains(&"old\n".to_string()));
        assert_eq!(unfinished(&f.journal).len(), 1);
        // Undone the rest: a.txt is back.
        assert_eq!(
            std::fs::read_to_string(f.roots[1].join("a.txt")).unwrap(),
            "old a\n"
        );
        let again = recover(&unfinished(&f.journal)[0]);
        assert!(again.complete());
        assert_eq!(
            std::fs::read_to_string(f.roots[1].join("old.txt")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn a_path_changed_again_during_rollback_is_left_and_reported() {
        let f = sample();
        let plan = make(&f, MIRROR);
        let a = f.roots[1].join("a.txt");
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Before && path == "old.txt" {
                std::fs::write(&a, "edited meanwhile\n").unwrap();
                Fault::Fail
            } else {
                Fault::None
            }
        });
        let SyncOutcome::Stopped { rollback, .. } = outcome else {
            panic!("not stopped");
        };
        assert_eq!(rollback.failed.len(), 1);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "edited meanwhile\n");
    }

    #[test]
    fn folders_inside_each_other_or_the_same_are_refused() {
        let f = folders();
        let inner = f.roots[0].join("sub");
        std::fs::create_dir(&inner).unwrap();
        let nested = [f.roots[0].clone(), inner.clone()];
        assert_eq!(
            check_roots(&nested).unwrap_err(),
            tr("folder_sync.err_nested")
        );
        let same = [f.roots[0].clone(), f.roots[0].join("sub/..")];
        assert_eq!(
            check_roots(&same).unwrap_err(),
            tr("folder_sync.err_same_folder")
        );
        #[cfg(unix)]
        {
            let alias = f.roots[1].join("alias");
            std::os::unix::fs::symlink(&f.roots[0], &alias).unwrap();
            let aliased = [f.roots[0].clone(), alias];
            assert_eq!(
                check_roots(&aliased).unwrap_err(),
                tr("folder_sync.err_same_folder")
            );
        }
        let journal_inside = folders();
        write(&journal_inside.roots[0], "a.txt", "a");
        let plan = make(&journal_inside, MIRROR);
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let inside = journal_inside.roots[1].join("journal");
        let SyncOutcome::NotApplied(_, errors) =
            apply(&plan, &journal_inside.roots, &inside, &stop, &done)
        else {
            panic!("journal inside a synced folder");
        };
        assert_eq!(errors, [tr("folder_sync.err_journal_inside")]);
        assert!(!inside.exists(), "a refused journal was created");
    }

    #[test]
    fn an_unfinished_compare_is_not_planned() {
        let f = sample();
        let items = scan(&f.roots);
        let stop = AtomicBool::new(false);
        let partial = Scope::default();
        assert_eq!(
            plan(&items, &f.roots, MIRROR, partial, &stop).unwrap_err(),
            tr("folder_sync.err_incomplete")
        );
        let mut pending = items.clone();
        let i = pending.iter().position(|it| it.name == "a.txt").unwrap();
        pending[i].status = CmpStatus::Pending;
        assert!(plan(&pending, &f.roots, MIRROR, ALL, &stop).is_err());
    }

    #[test]
    fn a_file_on_one_side_and_a_folder_on_the_other_is_left_alone() {
        let f = folders();
        write(&f.roots[0], "x", "file");
        write(&f.roots[1], "x/inside.txt", "keep");
        let plan = make(&f, MIRROR);
        assert!(plan
            .operations
            .iter()
            .all(|o| o.action == Action::Conflict && o.reason == Reason::TypeDiffers));
        assert!(matches!(
            run(&f, &plan, &|_, _| Fault::None),
            SyncOutcome::Applied { changes: 0, .. }
        ));
        assert!(f.roots[1].join("x/inside.txt").is_file());
    }

    #[test]
    fn names_a_file_system_may_take_for_the_same_are_left_alone() {
        let f = folders();
        write(&f.roots[0], "Read.txt", "new");
        write(&f.roots[1], "read.txt", "old");
        if !f.roots[1].join("READ.TXT").exists() {
            // A case-sensitive file system: two names in the source that fold together.
            write(&f.roots[0], "read.txt", "other");
        }
        let plan = make(&f, MIRROR);
        let clashes: Vec<&str> = plan
            .operations
            .iter()
            .filter(|o| o.reason == Reason::NameClash)
            .map(|o| o.path.as_str())
            .collect();
        assert!(!clashes.is_empty(), "{:?}", ops(&plan));
        assert!(plan
            .operations
            .iter()
            .all(|o| !(o.action.destructive() && clashes.contains(&o.path.as_str()))));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_never_followed_copied_or_deleted() {
        let f = folders();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.txt", "outside");
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), f.roots[1].join("l.txt"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), f.roots[1].join("ldir")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), f.roots[0].join("s.txt"))
            .unwrap();
        let plan = make(&f, MIRROR);
        assert!(!plan.operations.is_empty());
        assert!(plan
            .operations
            .iter()
            .all(|o| o.action == Action::Conflict && o.reason == Reason::Link));
        run(&f, &plan, &|_, _| Fault::None);
        assert!(std::fs::symlink_metadata(f.roots[1].join("l.txt")).is_ok());
        assert!(!f.roots[1].join("s.txt").exists());
        assert_eq!(read_to(outside.path().join("secret.txt")), "outside");
    }

    #[cfg(unix)]
    fn linked(f: &Folders, policy: SyncPolicy) -> SyncPlan {
        let scope = Scope {
            copy_links: true,
            ..ALL
        };
        plan(
            &scan(&f.roots),
            &f.roots,
            policy,
            scope,
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    /// Left: new.lnk, dir.lnk (to a folder outside), moved.lnk, clash (a link), dangling.lnk.
    /// Right: moved.lnk to another target, extra.lnk, clash (a file).
    #[cfg(unix)]
    fn links(outside: &Path) -> Folders {
        let f = folders();
        let link = |to: &Path, at: &Path| std::os::unix::fs::symlink(to, at).unwrap();
        let (l, r) = (&f.roots[0], &f.roots[1]);
        write(outside, "secret.txt", "outside");
        write(l, "a.txt", "a\n");
        link(Path::new("a.txt"), &l.join("new.lnk"));
        link(outside, &l.join("dir.lnk"));
        link(Path::new("a.txt"), &l.join("moved.lnk"));
        link(Path::new("a.txt"), &l.join("clash"));
        link(Path::new("nowhere/at/all"), &l.join("dangling.lnk"));
        write(r, "a.txt", "a\n");
        link(&outside.join("secret.txt"), &r.join("moved.lnk"));
        link(&outside.join("secret.txt"), &r.join("extra.lnk"));
        write(r, "clash", "a file");
        f
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_became_a_link_has_nothing_planned_through_it() {
        let f = folders();
        let r = &f.roots[1];
        write(r, "current/app.js", "app\n");
        let items = scan(&f.roots);
        // After the compare, the folder is replaced by a link to a folder with the same file.
        write(r, ".releases/v3/app.js", "app\n");
        std::fs::remove_dir_all(r.join("current")).unwrap();
        std::os::unix::fs::symlink(".releases/v3", r.join("current")).unwrap();
        let scope = Scope {
            copy_links: true,
            ..ALL
        };
        let plan = plan(&items, &f.roots, MIRROR, scope, &AtomicBool::new(false)).unwrap();
        assert!(
            plan.operations
                .iter()
                .all(|o| !o.path.starts_with("current/")),
            "{:?}",
            ops(&plan)
        );
        let current = plan
            .operations
            .iter()
            .find(|o| o.path == "current")
            .unwrap();
        assert_eq!(
            (current.action, current.reason),
            (Action::Conflict, Reason::ChangedSinceCompare)
        );
        run(&f, &plan, &|_, _| Fault::None);
        assert_eq!(read_to(r.join(".releases/v3/app.js")), "app\n");
    }

    #[cfg(unix)]
    fn target(p: PathBuf) -> String {
        std::fs::read_link(p).unwrap().display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn copied_links_are_made_replaced_and_deleted_as_links_and_never_followed() {
        let outside = tempfile::tempdir().unwrap();
        let f = links(outside.path());
        let plan = linked(&f, MIRROR);
        assert!(plan.copy_links);
        let mut got = ops(&plan);
        got.sort();
        assert_eq!(
            got,
            [
                (Action::Copy, "dangling.lnk"),
                (Action::Copy, "dir.lnk"),
                (Action::Copy, "new.lnk"),
                (Action::Replace, "moved.lnk"),
                (Action::Delete, "extra.lnk"),
                (Action::Conflict, "clash"),
            ]
        );
        assert_eq!(plan.estimate.bytes, 0, "a link copies no file");
        assert_eq!(plan.estimate.renames, 0, "links are not paired as renames");
        let SyncOutcome::Applied { changes, .. } = run(&f, &plan, &|_, _| Fault::None) else {
            panic!("not applied");
        };
        assert_eq!(changes, 5);
        let r = &f.roots[1];
        assert_eq!(target(r.join("new.lnk")), "a.txt");
        assert_eq!(target(r.join("moved.lnk")), "a.txt");
        assert_eq!(target(r.join("dangling.lnk")), "nowhere/at/all");
        assert_eq!(
            target(r.join("dir.lnk")),
            outside.path().display().to_string()
        );
        assert!(std::fs::symlink_metadata(r.join("extra.lnk")).is_err());
        assert_eq!(read_to(r.join("clash")), "a file");
        // What the links point at is never read, written or deleted.
        assert_eq!(read_to(outside.path().join("secret.txt")), "outside");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        assert!(unfinished(&f.journal).is_empty());
        // Planned again, nothing is left to do but the conflict.
        assert_eq!(ops(&linked(&f, MIRROR)), [(Action::Conflict, "clash")]);
    }

    #[cfg(unix)]
    #[test]
    fn an_update_keeps_links_only_the_destination_has() {
        let outside = tempfile::tempdir().unwrap();
        let f = links(outside.path());
        let plan = linked(&f, UPDATE);
        assert!(ops(&plan).contains(&(Action::Skip, "extra.lnk")));
        run(&f, &plan, &|_, _| Fault::None);
        assert!(std::fs::symlink_metadata(f.roots[1].join("extra.lnk")).is_ok());
        assert_eq!(target(f.roots[1].join("new.lnk")), "a.txt");
    }

    #[cfg(unix)]
    #[test]
    fn link_changes_are_undone_when_applying_fails_or_the_program_ends() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt").display().to_string();
        for crash in [false, true] {
            let f = links(outside.path());
            let plan = linked(&f, MIRROR);
            let last = plan
                .operations
                .iter()
                .rfind(|o| o.action.changes())
                .unwrap()
                .path
                .clone();
            let outcome = run(&f, &plan, &|p, path| match (p, path == last) {
                (Point::Written, true) if crash => Fault::Crash,
                (Point::Written, true) => Fault::Fail,
                _ => Fault::None,
            });
            let SyncOutcome::Stopped { rollback, .. } = outcome else {
                panic!("not stopped");
            };
            if crash {
                let pending = unfinished(&f.journal);
                assert_eq!(pending.len(), 1);
                assert!(recover(&pending[0]).complete());
            } else {
                assert!(rollback.complete(), "{:?}", rollback.failed);
            }
            let r = &f.roots[1];
            assert_eq!(target(r.join("moved.lnk")), secret, "crash {crash}");
            assert_eq!(target(r.join("extra.lnk")), secret);
            for gone in ["new.lnk", "dir.lnk", "dangling.lnk"] {
                assert!(std::fs::symlink_metadata(r.join(gone)).is_err(), "{gone}");
            }
            let names: Vec<String> = std::fs::read_dir(r)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                names.iter().all(|n| !n.starts_with(".oxidemerge")),
                "{names:?}"
            );
            assert!(unfinished(&f.journal).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_changed_after_the_preview_is_not_overwritten() {
        let outside = tempfile::tempdir().unwrap();
        let f = links(outside.path());
        let plan = linked(&f, MIRROR);
        let moved = f.roots[1].join("moved.lnk");
        std::fs::remove_file(&moved).unwrap();
        std::os::unix::fs::symlink("elsewhere", &moved).unwrap();
        assert!(!matches!(
            run(&f, &plan, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        assert_eq!(target(moved), "elsewhere");
        assert!(std::fs::symlink_metadata(f.roots[1].join("new.lnk")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn copying_links_is_part_of_what_a_plan_file_is() {
        let outside = tempfile::tempdir().unwrap();
        let f = links(outside.path());
        let without = make(&f, MIRROR);
        assert!(!without.to_json().unwrap().contains("copy_links"));
        let with = linked(&f, MIRROR);
        let mut altered = with.clone();
        altered.copy_links = false;
        assert_ne!(
            altered.identity(),
            with.id,
            "switching it off is an alteration"
        );
        let back = SyncPlan::from_json(&with.to_json().unwrap()).unwrap();
        assert!(back.copy_links);
        assert_eq!(back.id, back.identity());
    }

    #[cfg(unix)]
    fn read_to(p: PathBuf) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn a_mirror_with_items_filtered_out_warns_and_keeps_their_folders() {
        let f = folders();
        write(&f.roots[1], "logs/app.log", "kept");
        write(&f.roots[1], "logs/note.txt", "deleted");
        let items = scan_with(&f.roots, "!*.log");
        let scope = Scope {
            complete: true,
            excluded: FileFilter::parse_mask("!*.log").excludes_anything(),
            filter: "!*.log".into(),
            skip_hidden: false,
            copy_links: false,
        };
        let plan = plan(&items, &f.roots, MIRROR, scope, &AtomicBool::new(false)).unwrap();
        assert!(plan.warnings.contains(&PlanWarning::Excluded));
        let SyncOutcome::Applied { kept, .. } = run(&f, &plan, &|_, _| Fault::None) else {
            panic!("not applied");
        };
        assert_eq!(kept, ["logs"]);
        assert!(f.roots[1].join("logs/app.log").exists());
        assert!(!f.roots[1].join("logs/note.txt").exists());
        assert!(!FileFilter::parse_mask("*.*").excludes_anything());
    }

    #[test]
    fn many_deletions_are_flagged_for_a_second_confirmation() {
        let f = folders();
        for i in 0..LARGE_DELETION {
            write(&f.roots[1], &format!("f{i}.txt"), "x");
        }
        let plan = make(&f, MIRROR);
        assert!(plan.large_deletion());
        assert!(plan.warnings.contains(&PlanWarning::LargeDeletion {
            count: LARGE_DELETION
        }));
        assert!(!make(&f, UPDATE).large_deletion());
    }

    #[test]
    fn identical_contents_under_a_newer_time_are_not_replaced() {
        let f = folders();
        write(&f.roots[0], "a.txt", "same");
        write(&f.roots[1], "a.txt", "same");
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(f.roots[1].join("a.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let items = Scanner {
            roots: &f.roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions {
                method: crate::core::folder::CompareMethod::Date,
                ..FolderOptions::default()
            },
            dopts: &DiffOptions::default(),
            progress: &Progress::default(),
        }
        .run();
        assert_eq!(items[0].status, CmpStatus::Different);
        let plan = plan(&items, &f.roots, MIRROR, ALL, &AtomicBool::new(false)).unwrap();
        assert!(plan.operations.is_empty(), "{:?}", ops(&plan));
    }

    /// Makes `dir` read-only; false, with it made writable again, where that does not stop
    /// writing, as for the superuser.
    #[cfg(unix)]
    fn make_read_only(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dir.join(".probe");
        if std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(probe);
            make_writable(dir);
            return false;
        }
        true
    }

    /// Undoes [`make_read_only`], unless `dir` is gone.
    #[cfg(unix)]
    fn make_writable(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_that_cannot_be_written_stops_before_the_file_it_keeps_is_replaced() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let locked = std::cell::RefCell::new(None);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Before && path == "a.txt" {
                let dir = std::fs::read_dir(&f.journal).unwrap().next().unwrap();
                let files = dir.unwrap().path().join("files");
                if make_read_only(&files) {
                    *locked.borrow_mut() = Some(files);
                }
            }
            Fault::None
        });
        if let Some(files) = locked.borrow().as_ref() {
            make_writable(files);
        } else {
            return;
        }
        let SyncOutcome::Stopped {
            reason: Some(reason),
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped: {outcome:?}");
        };
        assert!(reason.starts_with("a.txt: "), "{reason}");
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
        assert!(unfinished(&f.journal).is_empty());
    }

    #[test]
    fn stopping_while_a_file_is_copied_leaves_no_part_of_it() {
        let f = sample();
        write(&f.roots[0], "d/b.txt", &"b".repeat(1 << 20));
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let outcome = apply_with(&plan, &f.roots, &f.journal, &stop, &done, &|p, path| {
            if p == Point::Before && path == "d/b.txt" {
                stop.store(true, Ordering::Relaxed);
            }
            Fault::None
        });
        let SyncOutcome::Stopped {
            reason: None,
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped: {outcome:?}");
        };
        assert_eq!(done.load(Ordering::Relaxed), 2);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    #[test]
    fn a_copy_stops_between_its_chunks() {
        /// Asks to stop once it has given the first chunk.
        struct Stopping<'a> {
            data: &'a [u8],
            stop: &'a AtomicBool,
        }
        impl Read for Stopping<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.data.len().min(buf.len());
                buf[..n].copy_from_slice(&self.data[..n]);
                self.data = &self.data[n..];
                self.stop.store(true, Ordering::Relaxed);
                Ok(n)
            }
        }
        let stop = AtomicBool::new(false);
        let data = vec![7u8; 1 << 20];
        let mut out = Vec::new();
        let mut from = Stopping {
            data: &data,
            stop: &stop,
        };
        let e = copy_hashing(&mut from, &mut out, &stop).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(out.len(), 64 * 1024);
    }

    #[test]
    fn stopping_again_during_the_rollback_does_not_cut_it_short() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let outcome = apply_with(&plan, &f.roots, &f.journal, &stop, &done, &|p, path| {
            match (p, path) {
                (Point::Written, "old.txt") | (Point::Rollback, _) => {
                    stop.store(true, Ordering::Relaxed)
                }
                _ => {}
            }
            Fault::None
        });
        let SyncOutcome::Stopped {
            reason: None,
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped: {outcome:?}");
        };
        assert_eq!(rollback.restored, 4);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    /// Set for a copy of the test program that applies a plan and is killed part way.
    const CHILD: &str = "OXIDEMERGE_TEST_SYNC_CHILD";

    #[test]
    fn a_synchronization_whose_process_was_killed_is_undone_later() {
        let name =
            "core::folder_sync::tests::a_synchronization_whose_process_was_killed_is_undone_later";
        if let Ok(dirs) = std::env::var(CHILD) {
            let dirs: Vec<PathBuf> = dirs.split('\n').map(PathBuf::from).collect();
            let (roots, journal, ready) = (&dirs[..2], &dirs[2], &dirs[3]);
            let p = plan(&scan(roots), roots, MIRROR, ALL, &AtomicBool::new(false)).unwrap();
            let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
            apply_with(&p, roots, journal, &stop, &done, &|pt, path| {
                if pt == Point::Written && path == "d/b.txt" {
                    std::fs::write(ready, "").unwrap();
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
                Fault::None
            });
            unreachable!("killed part way");
        }
        let f = sample();
        let before = tree(&f.roots[1]);
        let ready = f.journal.with_extension("ready");
        let dirs = [&f.roots[0], &f.roots[1], &f.journal, &ready]
            .map(|p| p.to_string_lossy().into_owned())
            .join("\n");
        /// Killed when dropped, so a failed check leaves no copy running.
        struct Killed(std::process::Child);
        impl Drop for Killed {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Killed(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--test-threads=1"])
                .env(CHILD, dirs)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let start = std::time::Instant::now();
        while !ready.exists() {
            assert!(
                start.elapsed().as_secs() < 60 && child.0.try_wait().unwrap().is_none(),
                "the copy did not reach the point to be killed at"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(child);
        assert_ne!(tree(&f.roots[1]), before);
        // the lock of a killed process may be released a little later on some systems
        let start = std::time::Instant::now();
        let pending = loop {
            let pending = unfinished(&f.journal);
            if !pending.is_empty() || start.elapsed().as_secs() >= 10 {
                break pending;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(pending.len(), 1);
        let rollback = recover(&pending[0]);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
        assert!(unfinished(&f.journal).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_loops_are_left_alone() {
        let f = sample();
        let link = std::os::unix::fs::symlink;
        link(".", f.roots[0].join("self")).unwrap();
        link("l2", f.roots[0].join("l1")).unwrap();
        link("l1", f.roots[0].join("l2")).unwrap();
        link("..", f.roots[1].join("gone/up")).unwrap();
        let plan = make(&f, MIRROR);
        let mut links: Vec<&str> = plan
            .operations
            .iter()
            .filter(|o| o.reason == Reason::Link)
            .map(|o| o.path.as_str())
            .collect();
        links.sort_unstable();
        assert_eq!(links, ["gone/up", "l1", "l2", "self"]);
        let SyncOutcome::Applied { kept, .. } = run(&f, &plan, &|_, _| Fault::None) else {
            panic!("not applied");
        };
        assert_eq!(kept, ["gone"]);
        assert!(std::fs::symlink_metadata(f.roots[1].join("gone/up")).is_ok());
        assert!(!f.roots[1].join("l1").exists());
    }

    #[test]
    fn a_file_hard_linked_into_the_destination_keeps_its_other_name_as_it_was() {
        let f = sample();
        let outside = tempfile::tempdir().unwrap();
        for (name, text) in [("a.txt", "old a\n"), ("old.txt", "old\n")] {
            let other = outside.path().join(name);
            std::fs::write(&other, text).unwrap();
            std::fs::remove_file(f.roots[1].join(name)).unwrap();
            std::fs::hard_link(&other, f.roots[1].join(name)).unwrap();
        }
        let plan = make(&f, MIRROR);
        assert!(matches!(
            run(&f, &plan, &|_, _| Fault::None),
            SyncOutcome::Applied { .. }
        ));
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(f.roots[1].join("a.txt")), "new a\n");
        assert!(!f.roots[1].join("old.txt").exists());
        assert_eq!(read(outside.path().join("a.txt")), "old a\n");
        assert_eq!(read(outside.path().join("old.txt")), "old\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_destination_folder_stops_and_undoes_the_rest() {
        let f = sample();
        write(&f.roots[0], "ro/x.txt", "new x\n");
        write(&f.roots[1], "ro/x.txt", "old x\n");
        let ro = f.roots[1].join("ro");
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        if !make_read_only(&ro) {
            return;
        }
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        make_writable(&ro);
        let SyncOutcome::Stopped {
            reason: Some(reason),
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped: {outcome:?}");
        };
        assert!(reason.starts_with("ro/x.txt: "), "{reason}");
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    #[test]
    fn a_source_file_changed_while_it_is_copied_is_refused_and_undone() {
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|p, path| {
            if p == Point::Before && path == "d/b.txt" {
                write(&f.roots[0], "d/b.txt", "changed\n");
            }
            Fault::None
        });
        let SyncOutcome::Stopped {
            reason: Some(reason),
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped: {outcome:?}");
        };
        assert_eq!(
            reason,
            format!("d/b.txt: {}", tr("folder_sync.err_changed"))
        );
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    /// Makes `link` a junction (a directory reparse point) to `target`; whether the system
    /// allowed one, which needs no privilege but may be refused on a file system without them.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
            && std::fs::symlink_metadata(link).is_ok()
    }

    /// A junction is a reparse point, not a folder of its own: a synchronization must leave it
    /// where it is rather than walk into it, copy what it points at, or delete it.
    #[cfg(windows)]
    #[test]
    fn junctions_are_never_followed_copied_or_deleted() {
        let f = folders();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.txt", "outside");
        if !junction(&f.roots[1].join("jdir"), outside.path())
            || !junction(&f.roots[0].join("sdir"), outside.path())
        {
            return;
        }
        let plan = make(&f, MIRROR);
        assert!(!plan.operations.is_empty());
        assert!(
            plan.operations
                .iter()
                .all(|o| o.action == Action::Conflict && o.reason == Reason::Link),
            "{:?}",
            ops(&plan)
        );
        run(&f, &plan, &|_, _| Fault::None);
        assert!(
            std::fs::symlink_metadata(f.roots[1].join("jdir")).is_ok(),
            "the junction in the destination was deleted"
        );
        assert!(
            std::fs::symlink_metadata(f.roots[0].join("sdir")).is_ok(),
            "the junction in the source was changed"
        );
        assert!(
            std::fs::symlink_metadata(f.roots[1].join("sdir")).is_err(),
            "the junction was copied"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "outside",
            "what the junction points at was changed"
        );
    }

    /// Windows refuses to read, replace or delete a file another program holds open without
    /// sharing. Such a file is reported as one that cannot be read, not as one that changed,
    /// and nothing is touched: the checks before the first change find it.
    #[cfg(windows)]
    #[test]
    fn a_destination_file_held_open_is_refused_before_anything_changes() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(f.roots[1].join("old.txt"))
            .unwrap();
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        drop(held);
        assert_eq!(outcome.problem(), Some(Problem::ReadFailure));
        let SyncOutcome::NotApplied(_, errors) = outcome else {
            panic!("a file that cannot be read did not stop the synchronization");
        };
        assert_eq!(
            errors,
            [trf(
                "folder_sync.err_unreadable",
                &[("path", "old.txt".into())]
            )]
        );
        assert_eq!(tree(&f.roots[1]), before);
        assert!(unfinished(&f.journal).is_empty());
    }

    /// The same file held open only once the changes are under way: the step that reaches it
    /// fails, and what was done before it is undone.
    #[cfg(windows)]
    #[test]
    fn a_destination_file_held_open_part_way_stops_and_undoes_the_rest() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = sample();
        let before = tree(&f.roots[1]);
        let plan = make(&f, MIRROR);
        let held = std::cell::RefCell::new(None);
        // old.txt is deleted last, so the copies before it have to be undone
        let outcome = run(&f, &plan, &|point, path| {
            if point == Point::Before && path == "old.txt" {
                *held.borrow_mut() = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(f.roots[1].join("old.txt"))
                    .ok();
            }
            Fault::None
        });
        let opened = held.borrow_mut().take().is_some();
        assert!(opened, "the file could not be held open");
        assert_eq!(outcome.problem(), Some(Problem::WriteFailure));
        let SyncOutcome::Stopped {
            reason: Some(reason),
            rollback,
            ..
        } = outcome
        else {
            panic!("not stopped");
        };
        assert!(reason.starts_with("old.txt: "), "{reason}");
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
    }

    /// A folder just under the old limit still cannot hold a file: the name of the temporary
    /// copy counts towards it too. This band is what a length check on the folder alone would
    /// let through.
    #[cfg(windows)]
    #[test]
    fn a_folder_just_under_the_old_windows_limit_can_still_be_copied_into() {
        let f = folders();
        // Long enough that the folder itself fits but a file made in it does not.
        let room = 233usize.saturating_sub(f.roots[0].as_os_str().len() + 1);
        let rel = format!("{}/f.txt", "d".repeat(room.max(1)));
        write(
            &f.roots[0],
            &rel,
            "deep
",
        );
        let full = f.roots[0].join(&rel);
        let folder = full.parent().unwrap().as_os_str().len();
        assert!(
            (224..260).contains(&folder),
            "{folder} characters is not in the band"
        );
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        assert!(
            matches!(outcome, SyncOutcome::Applied { .. }),
            "a folder near the limit was not copied into: {outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(f.roots[1].join(&rel)).unwrap(),
            "deep
"
        );
    }

    /// Paths longer than the old 260-character Windows limit are copied and deleted like any
    /// other, since every path goes to the system in its extended form.
    #[cfg(windows)]
    #[test]
    fn a_path_past_the_old_windows_limit_is_copied_then_deleted() {
        let f = folders();
        let deep: Vec<String> = (0..10)
            .map(|i| format!("a-directory-with-a-rather-long-name-{i:02}"))
            .collect();
        let rel = format!("{}/file.txt", deep.join("/"));
        write(
            &f.roots[0],
            &rel,
            "deep
",
        );
        let full = f.roots[0].join(&rel);
        assert!(
            full.as_os_str().len() > 260,
            "{} characters is not past the limit",
            full.as_os_str().len()
        );
        let plan = make(&f, MIRROR);
        let outcome = run(&f, &plan, &|_, _| Fault::None);
        assert!(
            matches!(outcome, SyncOutcome::Applied { .. }),
            "the long path was not copied: {outcome:?}"
        );
        let copied = f.roots[1].join(&rel);
        assert_eq!(
            std::fs::read_to_string(&copied).unwrap(),
            "deep
"
        );

        std::fs::remove_file(&full).unwrap();
        let plan = make(&f, MIRROR);
        assert!(
            matches!(
                run(&f, &plan, &|_, _| Fault::None),
                SyncOutcome::Applied { .. }
            ),
            "the long path was not deleted"
        );
        assert!(!copied.exists(), "the long path is still there");
    }
}
