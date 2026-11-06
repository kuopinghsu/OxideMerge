//! Archive support: archives are extracted to a temporary folder and compared as folders.
//! Zip based formats, tar (plain or compressed with gzip, bzip2 or xz) and 7z are supported.
//! Edits saved into the extracted copy are written back with [`repack`].

use super::fsutil;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

pub const ZIP_EXTS: &[&str] = &[
    "zip", "jar", "war", "ear", "apk", "aar", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub",
    "nupkg", "vsix", "whl", "xpi", "crx", "ipa", "appx", "msix",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    SevenZ,
}

/// File name endings of the tar and 7z formats; compound ones first.
const SUFFIXES: &[(&str, Kind)] = &[
    (".tar.gz", Kind::TarGz),
    (".tgz", Kind::TarGz),
    (".tar.bz2", Kind::TarBz2),
    (".tbz2", Kind::TarBz2),
    (".tbz", Kind::TarBz2),
    (".tar.xz", Kind::TarXz),
    (".txz", Kind::TarXz),
    (".tar", Kind::Tar),
    (".7z", Kind::SevenZ),
];

fn kind(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if let Some(&(_, k)) = SUFFIXES.iter().find(|(s, _)| name.ends_with(s)) {
        return Some(k);
    }
    ZIP_EXTS
        .contains(&super::text::extension(path).as_str())
        .then_some(Kind::Zip)
}

pub fn is_archive(path: &Path) -> bool {
    path.is_file() && kind(path).is_some()
}

/// The most an archive is unpacked to, so that a small one can't fill the disk.
const LIMITS: Limits = Limits {
    bytes: 8 << 30,
    entries: 500_000,
};

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    entries: u64,
}

/// What is left of the [`Limits`] while unpacking: `None` once a limit was passed.
struct Budget {
    bytes: Cell<Option<u64>>,
    entries: Cell<Option<u64>>,
    limits: Limits,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Budget {
            bytes: Cell::new(Some(limits.bytes)),
            entries: Cell::new(Some(limits.entries)),
            limits,
        }
    }

    fn take(left: &Cell<Option<u64>>, n: u64) -> std::io::Result<()> {
        let rest = left.get().and_then(|l| l.checked_sub(n));
        left.set(rest);
        match rest {
            Some(_) => Ok(()),
            None => Err(std::io::Error::other("archive too large")),
        }
    }

    fn entry(&self) -> std::io::Result<()> {
        Budget::take(&self.entries, 1)
    }

    fn passed(&self) -> bool {
        self.bytes.get().is_none() || self.entries.get().is_none()
    }

    /// The error for an archive that passed a limit; `e` otherwise.
    fn error(&self, path: &Path, e: anyhow::Error) -> anyhow::Error {
        if !self.passed() {
            return e;
        }
        anyhow::anyhow!(
            "{}: unpacks to more than {} GB or {} files",
            path.display(),
            self.limits.bytes >> 30,
            self.limits.entries
        )
    }
}

/// Reads or writes through `inner`, failing once the bytes of `budget` are used up.
struct Counted<'a, T> {
    inner: T,
    budget: &'a Budget,
}

impl<T: Read> Read for Counted<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        Budget::take(&self.budget.bytes, n as u64)?;
        Ok(n)
    }
}

impl<T: Write> Write for Counted<'_, T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Budget::take(&self.budget.bytes, buf.len() as u64)?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn extract_to_temp(path: &Path) -> Result<tempfile::TempDir> {
    extract_with(path, LIMITS)
}

fn extract_with(path: &Path, limits: Limits) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("oxidemerge-").tempdir()?;
    let budget = Budget::new(limits);
    extract(path, dir.path(), &budget).map_err(|e| budget.error(path, e))?;
    remove_links_outside(dir.path())?;
    Ok(dir)
}

fn extract(path: &Path, dir: &Path, budget: &Budget) -> Result<()> {
    let open = || std::fs::File::open(path).with_context(|| format!("open {}", path.display()));
    match kind(path).unwrap_or(Kind::Zip) {
        Kind::Zip => extract_zip(open()?, dir, budget)
            .with_context(|| format!("{} is not a zip archive", path.display())),
        Kind::Tar => extract_tar(open()?, dir, budget),
        Kind::TarGz => extract_tar(flate2::read::MultiGzDecoder::new(open()?), dir, budget),
        Kind::TarBz2 => extract_tar(bzip2::read::MultiBzDecoder::new(open()?), dir, budget),
        Kind::TarXz => {
            // Unpacked to a file first, as the tar inside may be larger than memory.
            let mut input = std::io::BufReader::new(open()?);
            let mut tar = tempfile::tempfile()?;
            let mut out = Counted {
                inner: std::io::BufWriter::new(&mut tar),
                budget,
            };
            lzma_rs::xz_decompress(&mut input, &mut out)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            out.flush()?;
            drop(out);
            tar.rewind()?;
            // The bytes were counted as they were unpacked.
            budget.bytes.set(Some(budget.limits.bytes));
            extract_tar(std::io::BufReader::new(tar), dir, budget)
        }
        Kind::SevenZ => sevenz_rust2::decompress_file_with_extract_fn(path, dir, |e, r, dest| {
            budget.entry()?;
            let mut r = Counted { inner: r, budget };
            sevenz_rust2::default_entry_extract_fn(e, &mut r, dest)
        })
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
    }
}

/// Removes the symbolic links under `root` that lead outside it, or nowhere. Each link is
/// resolved as the system resolves it, through the other links on the way.
pub(super) fn remove_links_outside(root: &Path) -> Result<()> {
    let real_root = std::fs::canonicalize(root)?;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let md = std::fs::symlink_metadata(&path)?;
            if md.file_type().is_symlink() {
                let inside = std::fs::canonicalize(&path).is_ok_and(|p| p.starts_with(&real_root));
                if !inside {
                    std::fs::remove_file(&path)
                        .or_else(|_| std::fs::remove_dir(&path))
                        .with_context(|| path.display().to_string())?;
                }
            } else if md.is_dir() {
                dirs.push(path);
            }
        }
    }
    Ok(())
}

/// Finder metadata that macOS adds to archives it creates (`._name` and `__MACOSX/`).
fn apple_metadata(rel: &Path) -> bool {
    rel.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        c == "__MACOSX" || c.starts_with("._")
    })
}

fn extract_zip(file: std::fs::File, dir: &Path, budget: &Budget) -> Result<()> {
    let mut ar = zip::ZipArchive::new(file)?;
    for i in 0..ar.len() {
        budget.entry()?;
        let f = ar.by_index(i)?;
        let Some(rel) = f.enclosed_name().filter(|r| !apple_metadata(r)) else {
            continue;
        };
        let out = dir.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut Counted { inner: f, budget }, &mut w)?;
        }
    }
    Ok(())
}

/// Whether a link at `rel` (a path in the archive) pointing to `target` stays in the archive,
/// judged by the names alone; [`remove_links_outside`] then checks where links really lead.
fn link_inside(rel: &Path, target: &Path, hard: bool) -> bool {
    let start = if hard {
        0
    } else {
        rel.components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .count()
            .saturating_sub(1)
    };
    let mut depth = start as isize;
    for c in target.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => depth -= 1,
            Component::RootDir | Component::Prefix(_) => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// Entries that cannot be created here (links on Windows, device files) are skipped, and so
/// are links to anything outside the archive; `unpack_in` also skips any that would land
/// outside `dir`.
fn extract_tar(input: impl Read, dir: &Path, budget: &Budget) -> Result<()> {
    let mut ar = tar::Archive::new(Counted {
        inner: input,
        budget,
    });
    for entry in ar.entries()? {
        budget.entry()?;
        let mut entry = entry?;
        let Ok(rel) = entry.path().map(|p| p.into_owned()) else {
            continue;
        };
        if apple_metadata(&rel) {
            continue;
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let inside = entry
                .link_name()
                .ok()
                .flatten()
                .is_some_and(|t| link_inside(&rel, &t, kind.is_hard_link()));
            if !inside {
                continue;
            }
        }
        let _ = entry.unpack_in(dir);
    }
    // A limit passed inside an entry fails only that entry.
    if budget.passed() {
        anyhow::bail!("archive too large");
    }
    Ok(())
}

/// One entry found while walking an extracted archive for [`repack`], relative to its root.
enum Entry {
    Dir(PathBuf),
    File(PathBuf),
    /// A symbolic link kept as a link (tar only): its target, as stored in the link itself.
    Symlink(PathBuf, PathBuf),
}

impl Entry {
    fn rel(&self) -> &Path {
        match self {
            Entry::Dir(p) | Entry::File(p) => p,
            Entry::Symlink(p, _) => p,
        }
    }
}

/// Lists every entry under `dir`, relative to it, in a stable (sorted) order. With
/// `follow_symlinks`, a symbolic link is read as whatever it points to (as zip has no
/// portable way to store one), but only when that real path stays inside `dir`: a symlink
/// added to the extracted copy by hand, pointing elsewhere (say, at `/etc/passwd`), is left
/// out rather than having its outside content embedded into the repacked archive. Without
/// `follow_symlinks`, a link is instead kept as an [`Entry::Symlink`] (tar supports these
/// directly), but only when it stays inside `dir` by name, as [`extract`] would have let it
/// through. Following a symlink never descends into the same real directory twice, so a
/// cycle made of symlinks cannot loop forever.
fn list_dir(dir: &Path, follow_symlinks: bool) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    let mut visited = std::collections::HashSet::new();
    let real_root = follow_symlinks
        .then(|| std::fs::canonicalize(dir).ok())
        .flatten();
    if let Some(root) = &real_root {
        visited.insert(root.clone());
    }
    while let Some(rel) = stack.pop() {
        let abs = dir.join(&rel);
        for entry in std::fs::read_dir(&abs).with_context(|| format!("read {}", abs.display()))? {
            let entry = entry?;
            let rel_child = rel.join(entry.file_name());
            let abs_child = dir.join(&rel_child);
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                if !follow_symlinks {
                    if let Ok(target) = std::fs::read_link(&abs_child) {
                        if link_inside(&rel_child, &target, false) {
                            out.push(Entry::Symlink(rel_child, target));
                        }
                    }
                    continue;
                }
                // Resolve where the link really leads before trusting it with anything
                // else: not inside `dir` at all, or (for a directory) a real path already
                // on the way down, which would otherwise recurse forever.
                let Ok(real) = std::fs::canonicalize(&abs_child) else {
                    continue;
                };
                if real_root
                    .as_ref()
                    .is_some_and(|root| !real.starts_with(root))
                {
                    continue;
                }
                if abs_child.is_dir() {
                    out.push(Entry::Dir(rel_child.clone()));
                    if visited.insert(real) {
                        stack.push(rel_child);
                    }
                } else {
                    out.push(Entry::File(rel_child));
                }
                continue;
            }
            if ft.is_dir() {
                out.push(Entry::Dir(rel_child.clone()));
                stack.push(rel_child);
            } else {
                out.push(Entry::File(rel_child));
            }
        }
    }
    out.sort_by(|a, b| a.rel().cmp(b.rel()));
    Ok(out)
}

/// The folder a replacement for `path` is staged in before it takes its place, so the
/// rename stays on the same file system.
fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Writes every entry of `dir` into `w` as a tar stream, returning it once finished.
fn write_tar<W: Write>(entries: &[Entry], dir: &Path, w: W) -> Result<W> {
    let mut b = tar::Builder::new(w);
    for e in entries {
        match e {
            Entry::Dir(rel) => {
                b.append_dir(rel, dir.join(rel))
                    .with_context(|| format!("archive dir {}", rel.display()))?;
            }
            Entry::File(rel) => {
                b.append_path_with_name(dir.join(rel), rel)
                    .with_context(|| format!("archive file {}", rel.display()))?;
            }
            Entry::Symlink(rel, target) => {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o777);
                b.append_link(&mut h, rel, target)
                    .with_context(|| format!("archive link {}", rel.display()))?;
            }
        }
    }
    b.into_inner().context("finish tar")
}

/// Rewrites the archive at `path` from the current contents of `dir`, an extraction of it
/// that may have been edited. The write goes to a temporary file in the same folder as
/// `path`, which then takes its place, so a failure or a crash leaves the original archive
/// untouched. Called after saving a change made to a file opened from an archive compare.
pub fn repack(path: &Path, dir: &Path) -> Result<()> {
    let out_dir = parent_dir(path);
    let writing = || format!("write {}", path.display());
    let Some(k) = kind(path) else {
        anyhow::bail!("{}: not a recognized archive", path.display());
    };
    match k {
        Kind::Zip => repack_zip(path, dir, out_dir).with_context(writing),
        Kind::Tar => {
            let entries = list_dir(dir, false)?;
            let tmp = tempfile::Builder::new()
                .prefix(".oxidemerge-")
                .tempfile_in(out_dir)?;
            let tmp = write_tar(&entries, dir, tmp)?;
            finish(tmp, path)
        }
        Kind::TarGz => {
            let entries = list_dir(dir, false)?;
            let tmp = tempfile::Builder::new()
                .prefix(".oxidemerge-")
                .tempfile_in(out_dir)?;
            let enc = flate2::write::GzEncoder::new(tmp, flate2::Compression::default());
            let enc = write_tar(&entries, dir, enc)?;
            finish(enc.finish().context("finish gzip")?, path)
        }
        Kind::TarBz2 => {
            let entries = list_dir(dir, false)?;
            let tmp = tempfile::Builder::new()
                .prefix(".oxidemerge-")
                .tempfile_in(out_dir)?;
            let enc = bzip2::write::BzEncoder::new(tmp, bzip2::Compression::default());
            let enc = write_tar(&entries, dir, enc)?;
            finish(enc.finish().context("finish bzip2")?, path)
        }
        Kind::TarXz => {
            let entries = list_dir(dir, false)?;
            // Built uncompressed first, as `lzma_rs` compresses a whole stream at once.
            let mut inner = tempfile::tempfile()?;
            write_tar(&entries, dir, &mut inner)?;
            inner.rewind()?;
            let tmp = tempfile::Builder::new()
                .prefix(".oxidemerge-")
                .tempfile_in(out_dir)?;
            let mut tmp = tmp;
            lzma_rs::xz_compress(&mut std::io::BufReader::new(inner), tmp.as_file_mut())
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            finish(tmp, path)
        }
        Kind::SevenZ => repack_7z(path, dir, out_dir).with_context(writing),
    }
}

/// Syncs and persists the staged replacement `tmp` over `path`.
fn finish(tmp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    tmp.as_file().sync_all()?;
    fsutil::persist(tmp, path).with_context(|| format!("write {}", path.display()))
}

fn repack_zip(path: &Path, dir: &Path, out_dir: &Path) -> Result<()> {
    let entries = list_dir(dir, true)?;
    let tmp = tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(out_dir)?;
    let mut tmp = tmp;
    {
        let mut w = zip::ZipWriter::new(tmp.as_file_mut());
        for e in &entries {
            match e {
                Entry::Dir(rel) => {
                    w.add_directory_from_path(rel, zip::write::SimpleFileOptions::default())?;
                }
                Entry::File(rel) => {
                    w.start_file_from_path(rel, zip::write::SimpleFileOptions::default())?;
                    let mut f = std::fs::File::open(dir.join(rel))
                        .with_context(|| format!("open {}", rel.display()))?;
                    std::io::copy(&mut f, &mut w)?;
                }
                Entry::Symlink(..) => unreachable!("zip entries follow symlinks"),
            }
        }
        w.finish().context("finish zip")?;
    }
    finish(tmp, path)
}

fn repack_7z(path: &Path, dir: &Path, out_dir: &Path) -> Result<()> {
    let entries = list_dir(dir, true)?;
    let tmp = tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(out_dir)?;
    let mut tmp = tmp;
    {
        let mut w = sevenz_rust2::ArchiveWriter::new(tmp.as_file_mut())
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        // DEFLATE rather than the default LZMA2, as this build only enables the codecs it
        // reads archives with (see the `sevenz-rust2` dependency features).
        w.set_content_methods(vec![sevenz_rust2::EncoderMethod::DEFLATE.into()]);
        for e in &entries {
            let rel = e.rel();
            let name = rel.to_string_lossy().replace('\\', "/");
            match e {
                Entry::Dir(_) => {
                    let entry = sevenz_rust2::ArchiveEntry::from_path(dir.join(rel), name);
                    w.push_archive_entry::<&[u8]>(entry, None)
                        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                }
                Entry::File(_) => {
                    let entry = sevenz_rust2::ArchiveEntry::from_path(dir.join(rel), name);
                    let f = std::fs::File::open(dir.join(rel))
                        .with_context(|| format!("open {}", rel.display()))?;
                    w.push_archive_entry(entry, Some(f))
                        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                }
                Entry::Symlink(..) => unreachable!("7z entries follow symlinks"),
            }
        }
        w.finish()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    }
    finish(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extracts `archive`, edits the files as a user might, repacks it, then extracts the
    /// result again and checks the edits came through, for every supported format.
    #[test]
    fn repack_round_trips_every_format() {
        let work = tempfile::tempdir().unwrap();

        let zip_path = work.path().join("a.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zw.start_file("keep.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"keep\n").unwrap();
        zw.start_file("change.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"old\n").unwrap();
        zw.start_file("remove.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"gone\n").unwrap();
        zw.add_directory("sub/", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.finish().unwrap();

        let tar_path = work.path().join("a.tar");
        std::fs::write(&tar_path, tar_bytes()).unwrap();

        let tgz_path = work.path().join("a.tar.gz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes()).unwrap();
        std::fs::write(&tgz_path, gz.finish().unwrap()).unwrap();

        let tbz_path = work.path().join("a.tar.bz2");
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bz.write_all(&tar_bytes()).unwrap();
        std::fs::write(&tbz_path, bz.finish().unwrap()).unwrap();

        let txz_path = work.path().join("a.tar.xz");
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar_bytes().as_slice(), &mut xz).unwrap();
        std::fs::write(&txz_path, xz).unwrap();

        // `tar_bytes` makes a `dir/a.txt`; build the same tree on disk for the 7z source.
        let sevenz_path = work.path().join("a.7z");
        let src = work.path().join("sevenz_src");
        std::fs::create_dir_all(src.join("dir")).unwrap();
        std::fs::write(src.join("dir/a.txt"), "hello\n").unwrap();
        sevenz_rust2::compress_to_path(&src, &sevenz_path).unwrap();

        for (archive, rel) in [
            (&zip_path, "change.txt"),
            (&tar_path, "dir/a.txt"),
            (&tgz_path, "dir/a.txt"),
            (&tbz_path, "dir/a.txt"),
            (&txz_path, "dir/a.txt"),
            (&sevenz_path, "dir/a.txt"),
        ] {
            let extracted = extract_to_temp(archive).unwrap();
            let target = extracted.path().join(rel);
            std::fs::write(&target, "new\n").unwrap();
            std::fs::write(extracted.path().join("added.txt"), "added\n").unwrap();

            super::repack(archive, extracted.path())
                .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));

            let after = extract_to_temp(archive).unwrap();
            let changed = std::fs::read_to_string(after.path().join(rel)).unwrap();
            assert_eq!(changed, "new\n", "{}", archive.display());
            let added = std::fs::read_to_string(after.path().join("added.txt")).unwrap();
            assert_eq!(added, "added\n", "{}", archive.display());
        }

        // Zip alone was given an unrelated file and an empty directory to drop and keep.
        let extracted = extract_to_temp(&zip_path).unwrap();
        assert!(extracted.path().join("keep.txt").exists());
        std::fs::remove_file(extracted.path().join("remove.txt")).unwrap();
        super::repack(&zip_path, extracted.path()).unwrap();
        let after = extract_to_temp(&zip_path).unwrap();
        assert!(after.path().join("keep.txt").exists());
        assert!(!after.path().join("remove.txt").exists());
        assert!(after.path().join("sub").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn repack_keeps_symlinks_in_tar_but_not_zip() {
        let work = tempfile::tempdir().unwrap();
        let tar_path = work.path().join("a.tar");
        std::fs::write(&tar_path, tar_bytes()).unwrap();
        let extracted = extract_to_temp(&tar_path).unwrap();
        std::os::unix::fs::symlink("a.txt", extracted.path().join("dir/link.txt")).unwrap();
        super::repack(&tar_path, extracted.path()).unwrap();
        let after = extract_to_temp(&tar_path).unwrap();
        let link = after.path().join("dir/link.txt");
        assert!(
            std::fs::symlink_metadata(&link).unwrap().is_symlink(),
            "tar should keep the symbolic link"
        );

        let zip_path = work.path().join("a.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zw.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"hi\n").unwrap();
        zw.finish().unwrap();
        let extracted = extract_to_temp(&zip_path).unwrap();
        std::os::unix::fs::symlink("a.txt", extracted.path().join("link.txt")).unwrap();
        super::repack(&zip_path, extracted.path()).unwrap();
        let after = extract_to_temp(&zip_path).unwrap();
        let link = after.path().join("link.txt");
        // Zip has no portable symbolic link of its own here, so the link was followed and
        // stored as the regular file it points to.
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "hi\n");
    }

    /// A symlink added to the extracted copy by hand, pointing outside it, is left out of
    /// the repacked tar rather than written into the archive: `extract` would never have let
    /// such a link through in the first place (see `link_inside`, `remove_links_outside`).
    #[cfg(unix)]
    #[test]
    fn repack_drops_a_tar_symlink_pointing_outside_the_archive() {
        let work = tempfile::tempdir().unwrap();
        let tar_path = work.path().join("a.tar");
        std::fs::write(&tar_path, tar_bytes()).unwrap();
        let extracted = extract_to_temp(&tar_path).unwrap();
        std::os::unix::fs::symlink("../../../etc/passwd", extracted.path().join("escape.txt"))
            .unwrap();
        super::repack(&tar_path, extracted.path()).unwrap();
        let after = extract_to_temp(&tar_path).unwrap();
        assert!(!after.path().join("escape.txt").exists());
    }

    /// Following symlinks to repack a zip or 7z must not loop forever on a cycle of
    /// directory symlinks (`a` -> `b`, `b` -> `a`).
    #[cfg(unix)]
    #[test]
    fn repack_follows_symlinks_without_looping_on_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/a.txt"), "hi\n").unwrap();
        // "." inside `real` resolves to `real` itself: following `real/loop` walks back
        // into `real`, then into `real/loop` again, forever, unless that is detected.
        std::os::unix::fs::symlink(".", dir.path().join("real/loop")).unwrap();
        let entries = list_dir(dir.path(), true).unwrap();
        // The cycle is cut off rather than walked forever; the real file is still found.
        assert!(entries.iter().any(|e| e.rel() == Path::new("real/a.txt")));
    }

    /// Following symlinks to repack a zip or 7z must not read through one pointing outside
    /// the extracted copy: that would embed a file (or a whole directory) from elsewhere on
    /// disk into the archive, under the attacker's chosen name.
    #[cfg(unix)]
    #[test]
    fn repack_does_not_follow_a_symlink_outside_the_extracted_copy() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        std::fs::create_dir_all(outside.path().join("secret_dir")).unwrap();
        std::fs::write(outside.path().join("secret_dir/x.txt"), "x\n").unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("escape_file.txt"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret_dir"),
            dir.path().join("escape_dir"),
        )
        .unwrap();

        let entries = list_dir(dir.path(), true).unwrap();
        assert!(entries.iter().any(|e| e.rel() == Path::new("a.txt")));
        assert!(!entries
            .iter()
            .any(|e| e.rel().starts_with("escape_file.txt")));
        assert!(!entries.iter().any(|e| e.rel().starts_with("escape_dir")));
    }

    fn tar_bytes() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let data = b"hello\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "dir/a.txt", &data[..]).unwrap();
        b.append_data(&mut h, "dir/._a.txt", &data[..]).unwrap();
        b.into_inner().unwrap()
    }

    #[test]
    fn recognizes_and_extracts_tar_archives() {
        assert_eq!(kind(Path::new("x.TAR.GZ")), Some(Kind::TarGz));
        assert_eq!(kind(Path::new("x.tbz2")), Some(Kind::TarBz2));
        assert_eq!(kind(Path::new("x.docx")), Some(Kind::Zip));
        assert_eq!(kind(Path::new("x.gz")), None);

        let dir = tempfile::tempdir().unwrap();
        let tgz = dir.path().join("a.tgz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes()).unwrap();
        std::fs::write(&tgz, gz.finish().unwrap()).unwrap();
        let tbz = dir.path().join("a.tar.bz2");
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(&mut bz, &tar_bytes()).unwrap();
        std::fs::write(&tbz, bz.finish().unwrap()).unwrap();
        let txz = dir.path().join("a.tar.xz");
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar_bytes().as_slice(), &mut xz).unwrap();
        std::fs::write(&txz, xz).unwrap();

        for archive in [tgz, tbz, txz] {
            assert!(is_archive(&archive));
            let out = extract_to_temp(&archive).unwrap();
            let text = std::fs::read_to_string(out.path().join("dir/a.txt")).unwrap();
            assert_eq!(text, "hello\n", "{}", archive.display());
            assert!(!out.path().join("dir/._a.txt").exists());
        }
    }

    #[test]
    fn archives_that_unpack_to_too_much_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = dir.path().join("a.tgz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes()).unwrap();
        std::fs::write(&tgz, gz.finish().unwrap()).unwrap();
        let txz = dir.path().join("a.tar.xz");
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar_bytes().as_slice(), &mut xz).unwrap();
        std::fs::write(&txz, xz).unwrap();
        let zip = dir.path().join("a.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        for name in ["big.txt", "other.txt"] {
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(&[b'0'; 10_000]).unwrap();
        }
        w.finish().unwrap();

        let small = Limits {
            bytes: 1000,
            entries: 100,
        };
        let few = Limits {
            bytes: 1 << 20,
            entries: 1,
        };
        let roomy = Limits {
            bytes: 1 << 20,
            entries: 100,
        };
        for archive in [&tgz, &txz, &zip] {
            for limits in [small, few] {
                let err = format!("{:#}", extract_with(archive, limits).unwrap_err());
                assert!(err.contains("unpacks to more than"), "{err}");
            }
            assert!(
                extract_with(archive, roomy).is_ok(),
                "{}",
                archive.display()
            );
        }
    }

    #[test]
    fn links_out_of_the_archive_are_left_out() {
        let p = Path::new;
        assert!(link_inside(p("a/b/link"), p("../c.txt"), false));
        assert!(link_inside(p("a/link"), p("./b/../c"), false));
        assert!(!link_inside(p("a/link"), p("../../etc"), false));
        assert!(!link_inside(p("link"), p("/etc/passwd"), false));
        assert!(link_inside(p("a/b/hard"), p("a/c.txt"), true));
        assert!(!link_inside(p("a/hard"), p("../x"), true));

        #[cfg(unix)]
        {
            let mut b = tar::Builder::new(Vec::new());
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "dir/a.txt", &b"hi\n"[..]).unwrap();
            // `s2` looks inside by its name, but leads out through the link `a/s1`
            let links = [
                ("dir/ok", "a.txt"),
                ("dir/home", "/"),
                ("dir/up", "../.."),
                ("./dir/dot_up", "../.."),
                ("a/s1", ".."),
                ("s2", "a/s1/.."),
            ];
            for (name, target) in links {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                b.append_link(&mut h, name, target).unwrap();
            }
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("links.tar");
            std::fs::write(&archive, b.into_inner().unwrap()).unwrap();
            let out = extract_to_temp(&archive).unwrap();
            let read = |n: &str| std::fs::read_to_string(out.path().join(n)).ok();
            assert_eq!(read("dir/ok").as_deref(), Some("hi\n"));
            assert!(std::fs::symlink_metadata(out.path().join("dir/home")).is_err());
            for gone in ["dir/up", "dir/dot_up", "s2"] {
                assert!(
                    std::fs::symlink_metadata(out.path().join(gone)).is_err(),
                    "{gone}"
                );
            }
            assert!(out.path().join("a/s1").is_dir());
        }
    }
}
