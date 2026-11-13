//! Live SFTP folder comparisons. Listing and reading file contents are separate operations.
use super::diff::{DiffOptions, Normalizer};
use super::filter::FileFilter;
use super::folder::{self, CmpStatus, CompareMethod, DirItem, FolderOptions};
use super::remote::{self, FetchMode, Fetched, Remote, RemoteConnection};
use crate::i18n::tr;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub(crate) enum Side {
    Local(PathBuf),
    Sftp(Arc<RemoteConnection>, String),
}

impl Side {
    pub fn label(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Sftp(conn, path) => remote::label(conn, path),
        }
    }

    /// How many more connections may be made beside one to read this side over.
    fn more_connections(&self) -> usize {
        match self {
            Self::Local(_) => 0,
            Self::Sftp(conn, _) => remote::more_connections(conn),
        }
    }

    fn connect<'a>(&'a self, stop: &AtomicBool) -> Result<Source<'a>> {
        match self {
            Self::Local(path) => Ok(Source::Local(path)),
            Self::Sftp(conn, path) => Ok(Source::Sftp(remote::connect(conn, stop)?, path)),
        }
    }
}

enum Source<'a> {
    Local(&'a Path),
    Sftp(Box<dyn Remote>, &'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Metadata {
    size: Option<u64>,
    modified: Option<SystemTime>,
}

impl From<remote::RemoteStamp> for Metadata {
    fn from(stamp: remote::RemoteStamp) -> Self {
        Self {
            size: Some(stamp.size),
            modified: (stamp.modified != 0)
                .then(|| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(stamp.modified)))
                .flatten(),
        }
    }
}

struct Entry {
    name: String,
    directory: bool,
    metadata: Metadata,
}

type MatchedEntry = ([Option<String>; 2], [Option<Metadata>; 2]);

fn list_local(
    path: &Path,
    top: bool,
    hidden: bool,
    limit: usize,
    stop: &AtomicBool,
) -> Result<(Vec<Entry>, usize)> {
    let mut entries = Vec::new();
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        check(stop)?;
        count += 1;
        anyhow::ensure!(count <= limit, "{}", tr("remote_compare.limit"));
        let entry = entry?;
        if hidden && folder::is_hidden(&entry) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let ty = entry.file_type()?;
        if (top && ty.is_dir() && name.starts_with(folder::UNDO_PREFIX))
            || !(ty.is_dir() || ty.is_file())
        {
            continue;
        }
        let meta = entry.metadata()?;
        entries.push(Entry {
            name,
            directory: ty.is_dir(),
            metadata: Metadata {
                size: Some(meta.len()),
                modified: meta.modified().ok(),
            },
        });
    }
    Ok((entries, count))
}

impl Source<'_> {
    fn list(
        &self,
        rel: &str,
        hidden: bool,
        limit: usize,
        stop: &AtomicBool,
    ) -> Result<(Vec<Entry>, usize)> {
        match self {
            Self::Local(root) => {
                let path = root.join(rel);
                list_local(&path, rel.is_empty(), hidden, limit, stop)
                    .with_context(|| path.display().to_string())
            }
            Self::Sftp(fs, root) => {
                let listed = fs.list_dir_stamped(&remote_path(root, rel), limit, stop)?;
                let count = listed.len();
                let entries = listed
                    .into_iter()
                    .filter(|e| (e.is_dir || e.is_file) && !(hidden && e.name.starts_with('.')))
                    .map(|entry| Entry {
                        name: entry.name,
                        directory: entry.is_dir,
                        metadata: entry.stamp.map_or(
                            Metadata {
                                size: None,
                                modified: None,
                            },
                            Metadata::from,
                        ),
                    })
                    .collect();
                Ok((entries, count))
            }
        }
    }

    fn file_metadata(&self, rel: &str) -> Result<Metadata> {
        match self {
            Self::Local(root) => {
                let path = root.join(rel);
                let meta =
                    std::fs::symlink_metadata(&path).with_context(|| path.display().to_string())?;
                anyhow::ensure!(meta.is_file(), "{}", path.display());
                Ok(Metadata {
                    size: Some(meta.len()),
                    modified: meta.modified().ok(),
                })
            }
            Self::Sftp(fs, root) => {
                let path = remote_path(root, rel);
                let (kind, stamp) = fs.kind_stamped(&path)?;
                anyhow::ensure!(kind == Some(remote::EntryKind::File), "{path}");
                let stamp = match stamp {
                    Some(stamp) => stamp,
                    None => fs.stamp(&path)?.with_context(|| path)?,
                };
                let mut meta = Metadata::from(stamp);
                // SFTP stamps also use zero when the server omits size. Let the stream
                // settle an empty-file comparison instead of relying on an ambiguous stat.
                meta.size = meta.size.filter(|size| *size != 0);
                Ok(meta)
            }
        }
    }

    fn read(&self, rel: &str) -> Result<Box<dyn Read + '_>> {
        match self {
            Self::Local(root) => Ok(Box::new(std::fs::File::open(root.join(rel))?)),
            Self::Sftp(fs, root) => fs.open_read(&remote_path(root, rel)),
        }
    }
}

fn remote_path(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_string()
    } else {
        remote::join(root, rel)
    }
}

fn check(stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        bail!("{}", tr("msg.sftp_cancelled"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Folder,
    LeftOnly,
    RightOnly,
    MetadataMatch,
    MetadataDifferent,
    Unverified,
    ContentMatch,
    ContentDifferent,
}

#[derive(Clone)]
pub(crate) struct Row {
    pub path: String,
    paths: [Option<String>; 2],
    pub directory: bool,
    pub status: Status,
    pub present: [bool; 2],
    pub sizes: [Option<u64>; 2],
}

fn status(entries: &[Option<Metadata>; 2], directory: bool, tolerance: u64) -> Status {
    match entries {
        [Some(_), None] => Status::LeftOnly,
        [None, Some(_)] => Status::RightOnly,
        [Some(_), Some(_)] if directory => Status::Folder,
        [Some(a), Some(b)] => {
            if a.size.zip(b.size).is_some_and(|(a, b)| a != b) {
                return Status::MetadataDifferent;
            }
            let within_tolerance = a.modified.zip(b.modified).map(|(a, b)| {
                a.duration_since(b).unwrap_or_else(|e| e.duration())
                    <= Duration::from_secs(tolerance)
            });
            match (a.size.zip(b.size), within_tolerance) {
                (_, Some(false)) => Status::MetadataDifferent,
                (Some(_), Some(true)) => Status::MetadataMatch,
                _ => Status::Unverified,
            }
        }
        [None, None] => unreachable!("a row comes from a directory entry"),
    }
}

/// How many more connections to make to both sides, to read them over several at once: as
/// many as the server that takes the fewest does.
fn more_connections(sides: &[Side; 2]) -> usize {
    sides
        .iter()
        .map(Side::more_connections)
        .filter(|&more| more > 0)
        .min()
        .unwrap_or(0)
}

/// A folder's listing, as [`Source::list`] gives it.
type Listing = Result<(Vec<Entry>, usize)>;

/// The folders of servers a scan comes to next, listed ahead of it over more connections: a
/// server takes each listing in a few round trips, one after another on a connection.
#[derive(Default)]
struct Ahead {
    state: std::sync::Mutex<AheadState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct AheadState {
    /// The side, path and entry limit of each folder still to list. The last is the one the
    /// scan comes to next.
    wanted: Vec<(usize, String, usize)>,
    /// The folders being listed.
    listing: std::collections::HashSet<(usize, String)>,
    listed: std::collections::HashMap<(usize, String), Listing>,
    /// Whether the connections that list ahead were asked for.
    connecting: bool,
    /// Set once the scan is over.
    over: bool,
}

/// How many folders must be waiting before more connections are made to list them ahead.
const WORTH_LISTING_AHEAD: usize = 8;
/// The most listings kept for the scan to come to.
const MAX_LISTED_AHEAD: usize = 64;

impl Ahead {
    fn state(&self) -> std::sync::MutexGuard<'_, AheadState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Notes the folders the scan comes to next, the first of them last. Returns whether
    /// more connections are now worth making, which it says once.
    fn want(&self, folders: impl Iterator<Item = (usize, String, usize)>) -> bool {
        let mut state = self.state();
        state.wanted.extend(folders);
        let connect = !state.connecting && state.wanted.len() >= WORTH_LISTING_AHEAD;
        state.connecting |= connect;
        drop(state);
        self.changed.notify_all();
        connect
    }

    /// The listing of `path` on `side` when it was read ahead, waiting for one being read;
    /// `None` when the scan is to list it itself.
    fn take(&self, side: usize, path: &str) -> Option<Listing> {
        let key = (side, path.to_owned());
        let mut state = self.state();
        loop {
            if let Some(listed) = state.listed.remove(&key) {
                drop(state);
                self.changed.notify_all();
                return Some(listed);
            }
            if !state.listing.contains(&key) {
                let wanted = &mut state.wanted;
                if let Some(at) = wanted
                    .iter()
                    .rposition(|(s, p, _)| (*s, p) == (side, &key.1))
                {
                    wanted.remove(at);
                }
                return None;
            }
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Lists wanted folders over `sources`, connections of its own, until the scan is over.
    fn list(&self, sources: &[Source<'_>; 2], hidden: bool, stop: &AtomicBool) {
        let mut state = self.state();
        loop {
            if state.over {
                return;
            }
            if state.listed.len() < MAX_LISTED_AHEAD {
                if let Some((side, path, limit)) = state.wanted.pop() {
                    state.listing.insert((side, path.clone()));
                    drop(state);
                    let listed = sources[side].list(&path, hidden, limit, stop);
                    state = self.state();
                    let key = (side, path);
                    state.listing.remove(&key);
                    state.listed.insert(key, listed);
                    self.changed.notify_all();
                    continue;
                }
            }
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finish(&self) {
        self.state().over = true;
        self.changed.notify_all();
    }
}

pub(crate) fn scan(
    sides: &[Side; 2],
    options: &FolderOptions,
    filter: &FileFilter,
    stop: &AtomicBool,
    send: impl FnMut(Vec<Row>),
) -> Result<()> {
    let sources = [sides[0].connect(stop)?, sides[1].connect(stop)?];
    // The sides whose folders more connections can list ahead.
    let ahead_of = sides.each_ref().map(|side| side.more_connections() > 0);
    let more = more_connections(sides);
    let ahead = Ahead::default();
    // Set with the scan over, so that a connection still being made then is given up.
    let over = AtomicBool::new(false);
    let progress = remote::progress_here();
    std::thread::scope(|scope| {
        let (ahead, over, progress) = (&ahead, &over, &progress);
        let connect = || {
            for _ in 0..more {
                scope.spawn(move || {
                    // One that cannot be made is done without: the scan lists it all.
                    let connect = || anyhow::Ok([sides[0].connect(over)?, sides[1].connect(over)?]);
                    if let Ok(sources) = remote::quietly(connect) {
                        remote::with_progress_of(progress, || {
                            ahead.list(&sources, options.skip_hidden, stop)
                        });
                    }
                });
            }
        };
        let done = scan_over(
            &sources, ahead_of, ahead, &connect, options, filter, stop, send,
        );
        over.store(true, Ordering::Relaxed);
        ahead.finish();
        done
    })
}

/// [`scan`], over the connections `sources`. The folders of the sides `ahead_of` names are
/// listed ahead through `ahead`, once `connect` made the connections for it.
#[allow(clippy::too_many_arguments)]
fn scan_over(
    sources: &[Source<'_>; 2],
    ahead_of: [bool; 2],
    ahead: &Ahead,
    connect: &dyn Fn(),
    options: &FolderOptions,
    filter: &FileFilter,
    stop: &AtomicBool,
    mut send: impl FnMut(Vec<Row>),
) -> Result<()> {
    let mut pending = vec![(String::new(), [Some(String::new()), Some(String::new())], 0)];
    let mut remaining = sources.each_ref().map(|source| match source {
        Source::Local(_) => usize::MAX,
        Source::Sftp(..) => remote::MAX_REMOTE_ENTRIES,
    });
    while let Some((rel, paths, depth)) = pending.pop() {
        check(stop)?;
        let mut entries = BTreeMap::<_, BTreeMap<String, [Option<Metadata>; 2]>>::new();
        for (side, source) in sources.iter().enumerate() {
            let Some(path) = &paths[side] else { continue };
            anyhow::ensure!(
                !matches!(source, Source::Sftp(..)) || depth <= remote::MAX_REMOTE_DEPTH,
                "{}",
                tr("remote_compare.depth")
            );
            let (listing, count) = match ahead.take(side, path) {
                Some(listed) => listed?,
                None => source.list(path, options.skip_hidden, remaining[side], stop)?,
            };
            remaining[side] = remaining[side]
                .checked_sub(count)
                .context(tr("remote_compare.limit"))?;
            for entry in listing {
                // Never turn a server-supplied path component into a local path prefix.
                if remote::local_name_ok(&entry.name) {
                    entries
                        .entry(folder::name_key(&entry.name, entry.directory))
                        .or_default()
                        .entry(entry.name)
                        .or_default()[side] = Some(entry.metadata);
                }
            }
        }
        let mut rows = Vec::new();
        let children_start = pending.len();
        for ((file, _), spellings) in entries {
            for (names, entries) in pair_spellings(spellings) {
                let name = names.iter().flatten().next().expect("listed entry");
                let path = relative_child(&rel, name);
                let keep = if file {
                    filter.include_file(&path, name)
                } else {
                    filter.include_dir(&path, name)
                };
                if !keep {
                    continue;
                }
                let present = entries.map(|e| e.is_some());
                let child_paths = std::array::from_fn(|side| {
                    Some(relative_child(
                        paths[side].as_deref()?,
                        names[side].as_deref()?,
                    ))
                });
                if !file
                    && options.recursive
                    && (present == [true, true] || options.include_unique_subfolders)
                {
                    pending.push((path.clone(), child_paths.clone(), depth + 1));
                }
                rows.push(Row {
                    path,
                    paths: child_paths,
                    directory: !file,
                    status: status(&entries, !file, options.time_tolerance_secs),
                    present,
                    sizes: entries.map(|e| if file { e.and_then(|m| m.size) } else { None }),
                });
            }
        }
        pending[children_start..].reverse(); // visit this directory's children in name order
        let next = pending[children_start..].iter().flat_map(|(_, paths, _)| {
            (0..2)
                .filter(|&side| ahead_of[side])
                .filter_map(|side| Some((side, paths[side].clone()?, remaining[side])))
                .rev()
        });
        if ahead.want(next) {
            connect();
        }
        send(rows);
    }
    Ok(())
}

fn relative_child(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn pair_spellings(spellings: BTreeMap<String, [Option<Metadata>; 2]>) -> Vec<MatchedEntry> {
    let mut exact: Vec<_> = spellings
        .into_iter()
        .map(|(name, entries)| (entries.map(|entry| entry.map(|_| name.clone())), entries))
        .collect();
    // Match different spellings only when there is exactly one candidate on each side.
    if let [left, right] = exact.as_mut_slice() {
        for (a, b) in [(0, 1), (1, 0)] {
            if left.1[a].is_some()
                && left.1[b].is_none()
                && right.1[a].is_none()
                && right.1[b].is_some()
            {
                left.0[b] = right.0[b].take();
                left.1[b] = right.1[b].take();
                exact.pop();
                break;
            }
        }
    }
    exact
}

pub(crate) struct Opened {
    pub path: PathBuf,
    pub fetched: Option<(RemoteConnection, String, Fetched)>,
}

pub(crate) fn open(sides: &[Side; 2], row: &Row, stop: &AtomicBool) -> Result<[Opened; 2]> {
    fn one(side: &Side, path: &str, stop: &AtomicBool) -> Result<Opened> {
        check(stop)?;
        match side {
            Side::Local(root) => {
                let path = root.join(path);
                anyhow::ensure!(
                    std::fs::symlink_metadata(&path)?.is_file(),
                    "{}",
                    path.display()
                );
                Ok(Opened {
                    path,
                    fetched: None,
                })
            }
            Side::Sftp(conn, root) => {
                let remote = remote_path(root, path);
                let fetched = remote::fetch_with(conn, &remote, stop, FetchMode::FileOnly)?;
                Ok(Opened {
                    path: fetched.local.clone(),
                    fetched: Some(((**conn).clone(), remote, fetched)),
                })
            }
        }
    }
    Ok([
        one(
            &sides[0],
            row.paths[0].as_deref().context("Missing left path")?,
            stop,
        )?,
        one(
            &sides[1],
            row.paths[1].as_deref().context("Missing right path")?,
            stop,
        )?,
    ])
}

/// Opens the pair of `row` from `kept`, the copies a verification fetched of it (one for each
/// side on a server, in order), without fetching them again.
pub(crate) fn open_kept(sides: &[Side; 2], row: &Row, kept: Vec<Fetched>) -> Result<[Opened; 2]> {
    let mut kept = kept.into_iter();
    let mut one = |side: usize| -> Result<Opened> {
        let path = row.paths[side].as_deref().context("Missing path")?;
        match &sides[side] {
            Side::Local(root) => {
                let path = root.join(path);
                anyhow::ensure!(
                    std::fs::symlink_metadata(&path)?.is_file(),
                    "{}",
                    path.display()
                );
                Ok(Opened {
                    path,
                    fetched: None,
                })
            }
            Side::Sftp(conn, root) => {
                let fetched = kept.next().context("Missing fetched file")?;
                remote::register_fetch(&fetched);
                Ok(Opened {
                    path: fetched.local.clone(),
                    fetched: Some(((**conn).clone(), remote_path(root, path), fetched)),
                })
            }
        }
    };
    Ok([one(0)?, one(1)?])
}

/// The largest file a byte-for-byte verification fetches whole, to keep for opening later;
/// larger ones are compared as they stream, stopping at the first difference.
const MAX_KEPT_FILE: u64 = 32 << 20;

/// Verifies each of `rows`, giving `send` its path, whether both sides hold the same, and the
/// copies fetched of it from servers (one for each side on one, in order; none when it was
/// compared as it streamed), which open it later without another fetch.
pub(crate) fn verify(
    sides: &[Side; 2],
    rows: Vec<Row>,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    stop: &AtomicBool,
    mut send: impl FnMut(String, Result<bool>, Vec<Fetched>),
) -> Result<()> {
    let sources = [sides[0].connect(stop)?, sides[1].connect(stop)?];
    let norm = Normalizer::new(dopts);
    let mut send = |path: String, result: Result<(bool, Vec<Fetched>)>| match result {
        Ok((same, kept)) => send(path, Ok(same), kept),
        Err(error) => send(path, Err(error), Vec::new()),
    };
    // Many files are verified over several connections at once: each takes a server a few
    // round trips, one after another on a connection.
    let more = match rows.len() >= remote::WORTH_CONNECTING {
        true => more_connections(sides),
        false => 0,
    };
    let together = more > 0;
    let rows = std::sync::Mutex::new(rows.into_iter());
    let next = || rows.lock().unwrap_or_else(|e| e.into_inner()).next();
    // Set once the first connection has verified all there was left for it, so that one
    // still being made then is given up.
    let over = AtomicBool::new(false);
    let progress = remote::progress_here();
    let (done, results) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..more {
            let (next, over, progress, done) = (&next, &over, &progress, done.clone());
            scope.spawn(move || {
                // One that cannot be made is done without: the first verifies them all.
                let connect = || anyhow::Ok([sides[0].connect(over)?, sides[1].connect(over)?]);
                let Ok(sources) = remote::quietly(connect) else {
                    return;
                };
                let norm = Normalizer::new(dopts);
                remote::with_progress_of(progress, || {
                    while let Some(row) = next() {
                        let result = verify_file(&sources, &row, fopts, dopts, &norm, stop);
                        if stop.load(Ordering::Relaxed) || done.send((row.path, result)).is_err() {
                            return;
                        }
                    }
                });
            });
        }
        drop(done);
        let first = (|| {
            while let Some(row) = next() {
                check(stop)?;
                let result = verify_file(&sources, &row, fopts, dopts, &norm, stop);
                check(stop)?;
                send(row.path, result);
                for (path, result) in results.try_iter() {
                    send(path, result);
                }
            }
            Ok(())
        })();
        over.store(true, Ordering::Relaxed);
        // What the other connections were verifying meanwhile.
        for (path, result) in results.iter() {
            if first.is_ok() && !stop.load(Ordering::Relaxed) {
                send(path, result);
            }
        }
        // Stopped, the other connections may have left files unverified.
        first.and_then(|()| if together { check(stop) } else { Ok(()) })
    })
}

/// The two files of a pair as local paths, fetching the ones on servers; with the fetches.
fn fetch_pair(
    sources: &[Source<'_>; 2],
    rels: [&str; 2],
    stop: &AtomicBool,
) -> Result<(Vec<PathBuf>, Vec<Fetched>)> {
    let mut fetched = Vec::new();
    let mut paths = Vec::new();
    for (source, rel) in sources.iter().zip(rels) {
        match source {
            Source::Local(root) => paths.push(root.join(rel)),
            Source::Sftp(fs, root) => {
                let file =
                    remote::fetch_from(&**fs, &remote_path(root, rel), stop, FetchMode::FileOnly)?;
                paths.push(file.local.clone());
                fetched.push(file);
            }
        }
    }
    Ok((paths, fetched))
}

fn verify_file(
    sources: &[Source<'_>; 2],
    row: &Row,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    norm: &Normalizer,
    stop: &AtomicBool,
) -> Result<(bool, Vec<Fetched>)> {
    let left = row.paths[0].as_deref().context("Missing left path")?;
    let right = row.paths[1].as_deref().context("Missing right path")?;
    let before = [
        sources[0].file_metadata(left)?,
        sources[1].file_metadata(right)?,
    ];
    let mut kept = Vec::new();
    let remote = sources.iter().any(|s| matches!(s, Source::Sftp(..)));
    let same = if fopts.method == CompareMethod::FullContents {
        let (paths, fetched) = fetch_pair(sources, [left, right], stop)?;
        kept = fetched;
        let name = row.path.rsplit('/').next().unwrap_or(&row.path);
        let item = DirItem {
            rel_dir: String::new(),
            name: name.to_string(),
            is_dir: false,
            exists: [true, true, false],
            size: [
                Some(std::fs::metadata(&paths[0])?.len()),
                Some(std::fs::metadata(&paths[1])?.len()),
                None,
            ],
            mtime: [before[0].modified, before[1].modified, None],
            status: CmpStatus::Pending,
            diff_side: None,
            binary: false,
            error: None,
            depth: 0,
            parent: None,
            children: Vec::new(),
        };
        folder::compare_files_with_cancel(&paths, &item, fopts, dopts, norm, Some(stop))?.0
            == CmpStatus::Identical
    } else if before[0]
        .size
        .zip(before[1].size)
        .is_some_and(|(a, b)| a != b)
    {
        false
    } else if remote
        && before
            .iter()
            .all(|m| m.size.is_some_and(|size| size <= MAX_KEPT_FILE))
    {
        // Fetched whole, so that opening the pair later needs no second fetch.
        let (paths, fetched) = fetch_pair(sources, [left, right], stop)?;
        kept = fetched;
        equal_streams(
            &mut std::fs::File::open(&paths[0])?,
            &mut std::fs::File::open(&paths[1])?,
            stop,
        )?
    } else {
        equal_streams(
            &mut sources[0].read(left)?,
            &mut sources[1].read(right)?,
            stop,
        )?
    };
    let after = [
        sources[0].file_metadata(left)?,
        sources[1].file_metadata(right)?,
    ];
    anyhow::ensure!(before == after, "{}", tr("remote_compare.changed"));
    Ok((same, kept))
}

fn equal_streams(a: &mut dyn Read, b: &mut dyn Read, stop: &AtomicBool) -> Result<bool> {
    let mut left = vec![0; 64 * 1024];
    let mut right = vec![0; left.len()];
    loop {
        check(stop)?;
        let a = folder::read_full(a, &mut left)?;
        let b = folder::read_full(b, &mut right)?;
        if a != b || left[..a] != right[..b] {
            return Ok(false);
        }
        if a < left.len() {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Arc<RemoteConnection> {
        Arc::new(RemoteConnection {
            protocol: remote::Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            user: "test".into(),
            auth: remote::Auth::Agent,
            options: Default::default(),
        })
    }

    #[test]
    fn matching_sftp_metadata_can_hide_a_byte_difference() {
        let right = if cfg!(any(windows, target_os = "macos")) {
            "A.txt"
        } else {
            "a.txt"
        };
        let right = format!("/right/{right}");
        remote::fake::install(
            remote::fake::Server::new()
                .dir("/left")
                .dir("/right")
                .file("/left/a.txt", "left")
                .file(&right, "else")
                .dir("/left/excluded")
                .file("/left/excluded/hidden.txt", "skip"),
        );
        let conn = connection();
        let sides = [
            Side::Sftp(conn.clone(), "/left".into()),
            Side::Sftp(conn, "/right".into()),
        ];
        let options = FolderOptions {
            method: CompareMethod::BinaryContents,
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let mut rows = Vec::new();
        scan(
            &sides,
            &options,
            &FileFilter::parse_mask("!excluded/"),
            &cancel,
            |batch| rows.extend(batch),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, Status::MetadataMatch);
        assert!(remote::fake::opened_paths().is_empty());

        let mut results = Vec::new();
        verify(
            &sides,
            rows,
            &options,
            &DiffOptions::default(),
            &cancel,
            |path, result, _| {
                results.push((path, result.unwrap()));
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].1);
        assert_eq!(
            remote::fake::opened_paths(),
            ["/left/a.txt", right.as_str()]
        );
        remote::fake::uninstall();
    }

    #[test]
    fn an_ambiguous_case_match_keeps_exact_pairs_and_other_spellings_separate() {
        let meta = Some(Metadata {
            size: Some(1),
            modified: None,
        });
        let rows = pair_spellings(BTreeMap::from([
            ("README.txt".to_owned(), [meta, meta]),
            ("Readme.txt".to_owned(), [None, meta]),
        ]));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].0,
            [Some("README.txt".into()), Some("README.txt".into())]
        );
        assert_eq!(rows[1].0, [None, Some("Readme.txt".into())]);
    }

    #[test]
    fn a_local_only_subtree_can_exceed_the_sftp_depth_limit() {
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().to_path_buf();
        for _ in 0..=remote::MAX_REMOTE_DEPTH {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("a.txt"), "local").unwrap();
        remote::fake::install(remote::fake::Server::new().dir("/remote"));
        let sides = [
            Side::Local(root.path().to_path_buf()),
            Side::Sftp(connection(), "/remote".into()),
        ];
        let mut rows = Vec::new();
        scan(
            &sides,
            &FolderOptions::default(),
            &FileFilter::parse_mask(""),
            &AtomicBool::new(false),
            |batch| rows.extend(batch),
        )
        .unwrap();
        assert!(rows
            .iter()
            .any(|row| row.path.ends_with("/a.txt") && row.status == Status::LeftOnly));
        remote::fake::uninstall();
    }

    /// Manual benchmark of a folder compare against a real server (see `docs/benchmarks.md`):
    /// `OXIDEMERGE_BENCH_REMOTE` is the folder's URL, `OXIDEMERGE_BENCH_LOCAL` the local
    /// folder to compare it with, and `OXIDEMERGE_REMOTE_PASSWORD` the password. The server
    /// is trusted as it is found, in the tests' own list of known hosts. Ignored, as the
    /// suite has no server.
    #[test]
    #[ignore]
    fn bench_remote_folder_compare() {
        use crate::core::remote_url;
        let (Ok(url), Ok(local)) = (
            std::env::var("OXIDEMERGE_BENCH_REMOTE"),
            std::env::var("OXIDEMERGE_BENCH_LOCAL"),
        ) else {
            eprintln!("set OXIDEMERGE_BENCH_REMOTE and OXIDEMERGE_BENCH_LOCAL");
            return;
        };
        let runs: usize = std::env::var("OXIDEMERGE_BENCH_RUNS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(3);
        let url = remote_url::parse(&url).unwrap();
        let creds = remote_url::Credentials::from_env(None, None);
        let conn = Arc::new(remote_url::connection(&url, &creds).unwrap());
        let never = AtomicBool::new(false);
        if let Err(e) = remote::connect(&conn, &never) {
            remote::trust_host(&e.downcast::<remote::UnknownHost>().unwrap()).unwrap();
        }
        let sides = [
            Side::Sftp(conn.clone(), url.path.clone()),
            Side::Local(PathBuf::from(local)),
        ];
        // Each step is timed signing in anew, and again taking the connections the run
        // before it left.
        let median = |name: &str, work: &mut dyn FnMut() -> String| {
            let mut note = String::new();
            let [anew, kept] = [true, false].map(|anew| {
                let mut times = Vec::new();
                for _ in 0..runs {
                    if anew {
                        remote::close_idle();
                    }
                    let start = std::time::Instant::now();
                    note = work();
                    times.push(start.elapsed());
                }
                times.sort();
                times[runs / 2].as_millis()
            });
            eprintln!("{name:<8} {anew:>7} ms  {kept:>7} ms kept  {note}");
        };
        median("connect", &mut || {
            remote::connect(&conn, &never).unwrap();
            String::new()
        });
        let options = FolderOptions {
            method: CompareMethod::BinaryContents,
            ..Default::default()
        };
        let filter = FileFilter::parse_mask("");
        let mut rows = Vec::new();
        median("scan", &mut || {
            rows.clear();
            scan(&sides, &options, &filter, &never, |batch| {
                rows.extend(batch)
            })
            .unwrap();
            format!("{} rows", rows.len())
        });
        let files: Vec<Row> = rows
            .iter()
            .filter(|r| !r.directory && r.present == [true, true])
            .cloned()
            .collect();
        median("verify", &mut || {
            let (mut same, mut failed) = (0, 0);
            verify(
                &sides,
                files.clone(),
                &options,
                &DiffOptions::default(),
                &never,
                |_, result, _| match result {
                    Ok(true) => same += 1,
                    Ok(false) => {}
                    Err(_) => failed += 1,
                },
            )
            .unwrap();
            format!("{} files, {same} same, {failed} failed", files.len())
        });
        median("fetch", &mut || {
            let fetched = remote::fetch(&conn, &url.path, &never).unwrap();
            let manifest = fetched.manifest.as_ref().unwrap();
            let bytes: u64 = manifest.files.values().map(|(stamp, _)| stamp.size).sum();
            format!("{} files, {bytes} bytes", manifest.files.len())
        });
    }

    #[test]
    fn a_folder_listed_ahead_is_the_one_the_scan_would_list_itself() {
        let root = tempfile::tempdir().unwrap();
        for file in ["a/x.txt", "a/y.txt", "b/z.txt"] {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "text").unwrap();
        }
        let never = AtomicBool::new(false);
        let ahead = Ahead::default();
        // One nobody asked for is the scan's own to list.
        assert!(ahead.take(0, "a").is_none());
        // The scan comes to `a` first; too few to connect for.
        let folders = [(0, "b".to_owned(), 9), (0, "a".to_owned(), 9)];
        assert!(!ahead.want(folders.into_iter()));
        // One the scan comes to before it was read is its own to list too.
        assert!(ahead.take(0, "a").is_none());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let sources = [Source::Local(root.path()), Source::Local(root.path())];
                ahead.list(&sources, false, &never);
            });
            while ahead.state().listed.is_empty() {
                std::thread::yield_now();
            }
            let (entries, count) = ahead.take(0, "b").unwrap().unwrap();
            assert_eq!(count, 1);
            assert_eq!(entries[0].name, "z.txt");
            ahead.finish();
        });
        assert!(ahead.take(0, "b").is_none());

        let many = (0..WORTH_LISTING_AHEAD).map(|i| (1, format!("d{i}"), 9));
        assert!(ahead.want(many.clone()));
        // Said once: the connections are being made.
        assert!(!ahead.want(many));
    }

    #[test]
    fn binary_verification_stops_reading_after_the_first_different_chunk() {
        let mut left = std::io::Cursor::new(vec![0; 1024 * 1024]);
        let mut right = std::io::Cursor::new(vec![1; 1024 * 1024]);
        assert!(!equal_streams(&mut left, &mut right, &AtomicBool::new(false)).unwrap());
        assert_eq!(left.position(), 64 * 1024);
        assert_eq!(right.position(), left.position());
    }
}
