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
//! Symbolic links are never followed or copied, and names that may stand for another file
//! on the destination's file system are left alone: those reaching an existing entry under
//! another case or Unicode form, and source names alike but for case or for accents on Latin
//! letters, composed or not.

use super::analysis;
use super::folder::{CmpStatus, DirItem};
use super::folder_merge::escapes;
use super::fsutil;
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
const JOURNAL_VERSION: u32 = 1;
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
    /// source, still applied as a copy and a deletion.
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
    pub operations: Vec<SyncOperation>,
    pub warnings: Vec<PlanWarning>,
    pub estimate: Estimate,
}

impl SyncPlan {
    fn identity(&self) -> String {
        let base = (self.version, &self.policy, &self.operations, &self.warnings);
        analysis::identity(&(base, &self.filter, self.skip_hidden, &self.folders))
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
}

/// The SHA-256 of the two folders' full paths, as recorded in a plan.
fn folders_digest(roots: &[PathBuf]) -> String {
    let real: Vec<String> = roots
        .iter()
        .map(|r| {
            std::fs::canonicalize(r)
                .unwrap_or_else(|_| r.clone())
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    analysis::identity(&real)
}

/// Links each copy to the deletion of the same contents when no other copy or deletion has
/// them. Empty files are left alone, as their contents say nothing.
fn pair_renames(operations: &mut [SyncOperation]) {
    let mut by_hash: HashMap<&str, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (i, o) in operations.iter().enumerate() {
        let (side, copy) = match o.action {
            Action::Copy => (&o.source, true),
            Action::Delete => (&o.destination, false),
            _ => continue,
        };
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
    fn exact(&mut self, path: &Path) -> bool {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return true;
        };
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_dir(dir)
                    .ok()
                    .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect())
            })
            .as_ref()
            .is_none_or(|names| names.contains(name))
    }
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
        let expected = if it.is_dir { Kind::Dir } else { Kind::File };
        let in_s = ks != Kind::Missing;
        let in_d = kd != Kind::Missing;
        let conflict =
            if it.status == CmpStatus::Error || ks == Kind::Unreadable || kd == Kind::Unreadable {
                Some(Reason::Unreadable)
            } else if ks == Kind::Link || kd == Kind::Link {
                Some(Reason::Link)
            } else if ks == Kind::Other || kd == Kind::Other {
                Some(Reason::Special)
            } else if (in_s && ks != expected) || (in_d && kd != expected) {
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
        let planned = match (in_s, in_d, it.is_dir) {
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
            Action::Copy | Action::Replace => source.size.unwrap_or(0),
            Action::Delete => destination.size.unwrap_or(0),
            _ => 0,
        };
        operations.push(op(action, reason, bytes, source, destination));
    }
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
    NotApplied(Vec<String>),
    /// Every planned change was made and checked. `kept` lists folders left in place
    /// because they still hold items the compare did not list.
    Applied { changes: usize, kept: Vec<String> },
    /// Stopped part way, by the user (`reason` is `None`) or a failure, and undone as far as
    /// `rollback` says.
    Stopped {
        reason: Option<String>,
        rollback: RollbackOutcome,
    },
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
            .tempfile_in(&files)?;
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
        tmp.persist(&to).map_err(|e| e.error)?;
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

fn changed_since_preview() -> std::io::Error {
    std::io::Error::other(tr("folder_sync.err_changed"))
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
    let mut tmp = tempfile::Builder::new().prefix(prefix).tempfile_in(dir)?;
    if copy_hashing(&mut src, tmp.as_file_mut(), stop)? != expected {
        return Err(changed_since_preview());
    }
    if let Ok(t) = md.modified() {
        let _ = tmp.as_file().set_modified(t);
    }
    tmp.as_file().sync_all()?;
    match place {
        Place::New => {
            tmp.persist_noclobber(to).map_err(|e| e.error)?;
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
    /// Before putting a path back.
    Rollback,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum Fault {
    None,
    Fail,
    /// End at once, as if the program ended, leaving the journal.
    Crash,
}

type Faults<'a> = &'a dyn Fn(Point, &str) -> Fault;

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

/// Every check that may refuse the plan before anything is changed.
fn preflight(
    plan: &SyncPlan,
    roots: &[PathBuf],
    journal_root: &Path,
    stop: &AtomicBool,
) -> Result<(), Vec<String>> {
    let one = |s: String| vec![s];
    if plan.version != PLAN_VERSION {
        return Err(one(trf(
            "folder_sync.err_future_version",
            &[("n", plan.version.to_string())],
        )));
    }
    if plan.id != plan.identity() {
        return Err(one(tr("folder_sync.err_plan_altered").to_string()));
    }
    check_roots(roots).map_err(one)?;
    if plan.folders != folders_digest(roots) {
        return Err(one(tr("folder_sync.err_other_folders").to_string()));
    }
    // Checked before it is created, so a refused journal leaves nothing in a synced folder.
    let journal = real_path(journal_root);
    for r in roots {
        let r = std::fs::canonicalize(r).map_err(|e| one(e.to_string()))?;
        if journal.starts_with(&r) || r.starts_with(&journal) {
            return Err(one(tr("folder_sync.err_journal_inside").to_string()));
        }
    }
    std::fs::create_dir_all(journal_root).map_err(|e| one(e.to_string()))?;
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
        return Err(one(tr("folder_sync.err_unfinished").to_string()));
    }
    let (s, d) = (plan.policy.source(), plan.policy.destination());
    let mut errors = Vec::new();
    for o in plan.operations.iter().filter(|o| o.action.changes()) {
        if stop.load(Ordering::Relaxed) {
            return Err(one(tr("folder_sync.err_stopped").to_string()));
        }
        let (sp, dp) = (under(&roots[s], &o.path), under(&roots[d], &o.path));
        if escapes(&roots[d], &o.path, &dp) {
            errors.push(trf("folder_sync.err_outside", &[("path", o.path.clone())]));
            continue;
        }
        let hash = |p: &Precondition| p.sha256.is_some();
        let read =
            |p: &Path, planned: &Precondition| Precondition::read_until(p, hash(planned), stop);
        if !o.source.matches(&read(&sp, &o.source))
            || !o.destination.matches(&read(&dp, &o.destination))
        {
            if stop.load(Ordering::Relaxed) {
                return Err(one(tr("folder_sync.err_stopped").to_string()));
            }
            errors.push(trf("folder_sync.err_stale", &[("path", o.path.clone())]));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
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
    if let Err(errors) = preflight(plan, roots, journal_root, stop) {
        return SyncOutcome::NotApplied(errors);
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
        (Err(e), _) | (_, Err(e)) => return SyncOutcome::NotApplied(e),
    };
    let roots = &roots[..];
    let mut journal = match Journal::create(&journal_root, plan, roots) {
        Ok(j) => j,
        Err(e) => {
            return SyncOutcome::NotApplied(vec![format!("{}: {e}", tr("folder_sync.journal"))])
        }
    };
    let (s, d) = (plan.policy.source(), plan.policy.destination());
    let mut kept = Vec::new();
    let mut failure: Option<Option<String>> = None;
    let fail = |path: &str, e: std::io::Error| Some(Some(format!("{path}: {e}")));
    for o in plan.operations.iter().filter(|o| o.action.changes()) {
        if stop.load(Ordering::Relaxed) {
            failure = Some(None);
            break;
        }
        match fault(Point::Before, &o.path) {
            Fault::None => {}
            Fault::Fail => {
                failure = fail(&o.path, injected());
                break;
            }
            Fault::Crash => return crashed(journal),
        }
        let (sp, dp) = (under(&roots[s], &o.path), under(&roots[d], &o.path));
        if escapes(&roots[d], &o.path, &dp) {
            failure = Some(Some(trf(
                "folder_sync.err_outside",
                &[("path", o.path.clone())],
            )));
            break;
        }
        let result = step(&mut journal, o, (&sp, &dp), &mut kept, stop);
        if let Err(e) = result {
            failure = if stop.load(Ordering::Relaxed) {
                Some(None)
            } else {
                fail(&o.path, e)
            };
            break;
        }
        done.fetch_add(1, Ordering::Relaxed);
        match fault(Point::Written, &o.path) {
            Fault::None => {}
            Fault::Fail => {
                failure = fail(&o.path, injected());
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
            SyncOutcome::Stopped { reason, rollback }
        }
    }
}

fn crashed(journal: Journal) -> SyncOutcome {
    SyncOutcome::Stopped {
        reason: Some("crash".into()),
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
            planned.sha256.is_some() && planned.kind == Kind::File,
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
            if ours && item.file_type().is_ok_and(|t| t.is_file()) {
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
            _ => Err(not_ours()),
        },
        Before::Dir => match now.kind {
            Kind::Dir => Ok(false),
            Kind::Missing => std::fs::create_dir_all(target).map(|()| true),
            _ => Err(not_ours()),
        },
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
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(dir.join(JOURNAL_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// The unfinished synchronizations recorded under `journal_root`, oldest first.
pub fn unfinished(journal_root: &Path) -> Vec<Unfinished> {
    journals(journal_root)
        .into_iter()
        .filter_map(|h| {
            let file = claim(&h.dir).ok()??;
            Some(Unfinished {
                changes: read_entries(&file).len(),
                dir: h.dir,
                roots: h.roots,
                destination: h.destination,
            })
        })
        .collect()
}

/// Undoes an unfinished synchronization from its journal.
pub fn recover(u: &Unfinished) -> RollbackOutcome {
    let failed = |e: String| RollbackOutcome {
        failed: vec![(u.dir.display().to_string(), e)],
        journal: Some(u.dir.clone()),
        ..RollbackOutcome::default()
    };
    let header = match read_header(&u.dir) {
        Ok(h) if h.version > JOURNAL_VERSION => {
            return failed(tr("folder_sync.err_newer_journal").to_string())
        }
        Ok(h) => h,
        Err(e) => return failed(e.to_string()),
    };
    let file = match claim(&u.dir) {
        Ok(Some(f)) => f,
        Ok(None) => return failed(tr("folder_sync.err_in_use").to_string()),
        Err(e) => return failed(e.to_string()),
    };
    let mut out = roll_back(&header, &read_entries(&file), &|_, _| Fault::None);
    settle(&u.dir, file, &mut out);
    out
}

/// Forgets an unfinished synchronization, leaving the folders as they are now.
pub fn discard(u: &Unfinished) -> std::io::Result<()> {
    match claim(&u.dir)? {
        Some(file) => remove_journal(&u.dir, file),
        None => Err(std::io::Error::other(tr("folder_sync.err_in_use"))),
    }
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
    fn a_file_renamed_in_the_source_shows_as_a_rename_and_is_copied_then_deleted() {
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
        let SyncOutcome::NotApplied(errors) = run(&elsewhere, &p, &|_, _| Fault::None) else {
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
        let SyncOutcome::NotApplied(errors) = run(&f, &bare, &|_, _| Fault::None) else {
            panic!("applied without its folders");
        };
        assert_eq!(errors, [tr("folder_sync.err_other_folders")]);
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
        let SyncOutcome::NotApplied(errors) = run(&f, &plan, &|_, _| Fault::None) else {
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
            SyncOutcome::NotApplied(_)
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
        let SyncOutcome::NotApplied(errors) = run(&f, &make(&f, UPDATE), &|_, _| Fault::None)
        else {
            panic!("applied over an unfinished synchronization");
        };
        assert_eq!(errors, [tr("folder_sync.err_unfinished")]);
        let rollback = recover(&pending[0]);
        assert!(rollback.complete(), "{:?}", rollback.failed);
        assert_eq!(tree(&f.roots[1]), before);
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
        std::fs::File::open(&empty)
            .unwrap()
            .set_modified(old)
            .unwrap();

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
        let SyncOutcome::NotApplied(errors) =
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
}
