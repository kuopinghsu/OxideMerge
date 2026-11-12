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
                anyhow::ensure!(fs.kind(&path)? == Some(remote::EntryKind::File), "{path}");
                let mut meta = Metadata::from(fs.stamp(&path)?.with_context(|| path)?);
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

pub(crate) fn scan(
    sides: &[Side; 2],
    options: &FolderOptions,
    filter: &FileFilter,
    stop: &AtomicBool,
    mut send: impl FnMut(Vec<Row>),
) -> Result<()> {
    let sources = [sides[0].connect(stop)?, sides[1].connect(stop)?];
    let mut pending = vec![(String::new(), [Some(String::new()), Some(String::new())], 0)];
    let mut remaining = sides.each_ref().map(|side| match side {
        Side::Local(_) => usize::MAX,
        Side::Sftp(..) => remote::MAX_REMOTE_ENTRIES,
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
            let (listing, count) = source.list(path, options.skip_hidden, remaining[side], stop)?;
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

pub(crate) fn verify(
    sides: &[Side; 2],
    rows: Vec<Row>,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    stop: &AtomicBool,
    mut send: impl FnMut(String, Result<bool>),
) -> Result<()> {
    let sources = [sides[0].connect(stop)?, sides[1].connect(stop)?];
    let norm = Normalizer::new(dopts);
    for row in rows {
        check(stop)?;
        let result = verify_file(&sources, &row, fopts, dopts, &norm, stop);
        check(stop)?;
        send(row.path, result);
    }
    Ok(())
}

fn verify_file(
    sources: &[Source<'_>; 2],
    row: &Row,
    fopts: &FolderOptions,
    dopts: &DiffOptions,
    norm: &Normalizer,
    stop: &AtomicBool,
) -> Result<bool> {
    let left = row.paths[0].as_deref().context("Missing left path")?;
    let right = row.paths[1].as_deref().context("Missing right path")?;
    let before = [
        sources[0].file_metadata(left)?,
        sources[1].file_metadata(right)?,
    ];
    let same = if fopts.method == CompareMethod::FullContents {
        let mut fetched = Vec::new();
        let mut paths = Vec::new();
        for (source, rel) in sources.iter().zip([left, right]) {
            match source {
                Source::Local(root) => paths.push(root.join(rel)),
                Source::Sftp(fs, root) => {
                    let file = remote::fetch_from(
                        &**fs,
                        &remote_path(root, rel),
                        stop,
                        FetchMode::FileOnly,
                    )?;
                    paths.push(file.local.clone());
                    fetched.push(file);
                }
            }
        }
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
    Ok(same)
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
            |path, result| {
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

    #[test]
    fn binary_verification_stops_reading_after_the_first_different_chunk() {
        let mut left = std::io::Cursor::new(vec![0; 1024 * 1024]);
        let mut right = std::io::Cursor::new(vec![1; 1024 * 1024]);
        assert!(!equal_streams(&mut left, &mut right, &AtomicBool::new(false)).unwrap());
        assert_eq!(left.position(), 64 * 1024);
        assert_eq!(right.position(), left.position());
    }
}
