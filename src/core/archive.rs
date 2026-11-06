//! Archive support: archives are extracted to a temporary folder and compared as folders.
//! Zip based formats, tar (plain or compressed with gzip, bzip2 or xz) and 7z are supported.
//! Edits saved into the extracted copy are written back with [`repack`], which keeps every
//! entry left alone as it was.

use super::fsutil;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::collections::HashSet;
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

/// The size and modified time of an archive file, to notice a change made to it after it was
/// extracted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let md = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: md.len(),
        modified: md.modified().ok(),
    })
}

/// The entries an extraction wrote, by their paths in the archive.
type Written = std::sync::Arc<HashSet<PathBuf>>;

/// A folder [`extract_to_temp`] filled: the archive, how it was then, and the entries it
/// wrote, so that [`repack`] keeps every other entry as it was and refuses to write over an
/// archive changed since.
struct Extraction {
    dir: PathBuf,
    /// `dir` with every symbolic link on the way resolved.
    real: Option<PathBuf>,
    stamp: Option<Stamp>,
    written: Written,
}

static EXTRACTIONS: std::sync::Mutex<Vec<Extraction>> = std::sync::Mutex::new(Vec::new());

fn extractions() -> std::sync::MutexGuard<'static, Vec<Extraction>> {
    EXTRACTIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `path` is in a folder an archive was extracted to, where a backup copy of a saved
/// file would end up packed into the archive.
pub fn inside_extraction(path: &Path) -> bool {
    let all = extractions();
    if all.iter().any(|e| path.starts_with(&e.dir)) {
        return true;
    }
    let real = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.join(path.file_name().unwrap_or_default()));
    real.is_some_and(|real| {
        all.iter()
            .any(|e| e.real.as_ref().is_some_and(|d| real.starts_with(d)))
    })
}

/// The extraction recorded for the folder `dir`, if any.
fn find_extraction<'a>(all: &'a mut [Extraction], dir: &Path) -> Option<&'a mut Extraction> {
    let real = std::fs::canonicalize(dir).ok();
    all.iter_mut()
        .find(|e| e.dir == dir || real.is_some() && e.real == real)
}

pub fn extract_to_temp(path: &Path) -> Result<tempfile::TempDir> {
    extract_with(path, LIMITS)
}

fn extract_with(path: &Path, limits: Limits) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("oxidemerge-").tempdir()?;
    let budget = Budget::new(limits);
    let stamp = stamp(path);
    let written = std::cell::RefCell::new(HashSet::new());
    extract(path, dir.path(), &budget, &written).map_err(|e| budget.error(path, e))?;
    let mut written = written.into_inner();
    for gone in remove_links_outside(dir.path())? {
        written.remove(&gone);
    }
    let mut all = extractions();
    all.retain(|e| e.dir.exists());
    all.push(Extraction {
        dir: dir.path().to_path_buf(),
        real: std::fs::canonicalize(dir.path()).ok(),
        stamp,
        written: std::sync::Arc::new(written),
    });
    Ok(dir)
}

fn extract(
    path: &Path,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
) -> Result<()> {
    let open = || std::fs::File::open(path).with_context(|| format!("open {}", path.display()));
    let extract_tar = |input: &mut dyn Read| extract_tar(input, dir, budget, written);
    match kind(path).unwrap_or(Kind::Zip) {
        Kind::Zip => extract_zip(open()?, dir, budget, written)
            .with_context(|| format!("{} is not a zip archive", path.display())),
        Kind::Tar => extract_tar(&mut open()?),
        Kind::TarGz => extract_tar(&mut flate2::read::MultiGzDecoder::new(open()?)),
        Kind::TarBz2 => extract_tar(&mut bzip2::read::MultiBzDecoder::new(open()?)),
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
            extract_tar(&mut std::io::BufReader::new(tar))
        }
        Kind::SevenZ => sevenz_rust2::decompress_file_with_extract_fn(path, dir, |e, r, dest| {
            budget.entry()?;
            let mut r = Counted { inner: r, budget };
            let done = sevenz_rust2::default_entry_extract_fn(e, &mut r, dest)?;
            written.borrow_mut().insert(clean(Path::new(e.name())));
            Ok(done)
        })
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
    }
}

/// Removes the symbolic links under `root` that lead outside it, or nowhere, returning their
/// paths relative to `root`. Each link is resolved as the system resolves it, through the
/// other links on the way.
pub(super) fn remove_links_outside(root: &Path) -> Result<Vec<PathBuf>> {
    let real_root = std::fs::canonicalize(root)?;
    let mut dirs = vec![root.to_path_buf()];
    let mut removed = Vec::new();
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
                    if let Ok(rel) = path.strip_prefix(root) {
                        removed.push(rel.to_path_buf());
                    }
                }
            } else if md.is_dir() {
                dirs.push(path);
            }
        }
    }
    Ok(removed)
}

/// Finder metadata that macOS adds to archives it creates (`._name` and `__MACOSX/`).
fn apple_metadata(rel: &Path) -> bool {
    rel.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        c == "__MACOSX" || c.starts_with("._")
    })
}

fn extract_zip(
    file: std::fs::File,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
) -> Result<()> {
    let mut ar = zip::ZipArchive::new(file)?;
    for i in 0..ar.len() {
        budget.entry()?;
        let f = ar.by_index(i)?;
        let Some(rel) = f.enclosed_name().filter(|r| !apple_metadata(r)) else {
            continue;
        };
        let out = dir.join(&rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut Counted { inner: f, budget }, &mut w)?;
        }
        written.borrow_mut().insert(clean(&rel));
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
fn extract_tar(
    input: impl Read,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
) -> Result<()> {
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
        if entry.unpack_in(dir).unwrap_or(false) {
            if let Some(rel) = unpacked_at(&rel) {
                written.borrow_mut().insert(rel);
            }
        }
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
    let mut visited = HashSet::new();
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

/// Drops the `.` parts of a path stored in an archive, so it compares equal to the same path
/// found on disk: `./dir/a.txt`, as `tar -C dir .` names it, is `dir/a.txt`.
fn clean(rel: &Path) -> PathBuf {
    rel.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Adds the folders above `rel` to `implied`: an archive need not list them as entries.
fn add_parents(implied: &mut HashSet<PathBuf>, rel: &Path) {
    for p in rel.ancestors().skip(1) {
        if p.as_os_str().is_empty() || !implied.insert(p.to_path_buf()) {
            break;
        }
    }
}

/// Whether `e`, found on disk, is missing from an archive whose entries were `seen`.
fn is_new(e: &Entry, seen: &HashSet<PathBuf>, implied: &HashSet<PathBuf>) -> bool {
    let rel = e.rel();
    !seen.contains(rel) && !(matches!(e, Entry::Dir(_)) && implied.contains(rel))
}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Whether the regular file at `abs` holds exactly the `len` bytes `r` reads; `None` when
/// there is no regular file there any more. `r` is read to its end either way, as the next
/// entry of a stream starts after it.
fn same_file(r: &mut dyn Read, len: u64, abs: &Path) -> Result<Option<bool>> {
    let same = match std::fs::symlink_metadata(abs) {
        Ok(md) if md.is_file() => {
            let mut same = md.len() == len;
            if same {
                let mut f = std::fs::File::open(abs)?;
                let (mut a, mut b) = (vec![0u8; 64 << 10], vec![0u8; 64 << 10]);
                loop {
                    let n = read_full(r, &mut a)?;
                    let m = read_full(&mut f, &mut b)?;
                    if a[..n] != b[..m] {
                        same = false;
                        break;
                    }
                    if n == 0 {
                        break;
                    }
                }
            }
            Some(same)
        }
        _ => None,
    };
    std::io::copy(r, &mut std::io::sink())?;
    Ok(same)
}

/// Writes `e`, an entry found on disk under `dir` that the archive did not have, into `b`.
fn append_new<W: Write>(b: &mut tar::Builder<W>, dir: &Path, e: &Entry) -> Result<()> {
    match e {
        Entry::Dir(rel) => b
            .append_dir(rel, dir.join(rel))
            .with_context(|| format!("archive dir {}", rel.display())),
        Entry::File(rel) => b
            .append_path_with_name(dir.join(rel), rel)
            .with_context(|| format!("archive file {}", rel.display())),
        Entry::Symlink(rel, target) => {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            b.append_link(&mut h, rel, target)
                .with_context(|| format!("archive link {}", rel.display()))
        }
    }
}

/// Writes an entry with header `h` at `rel` into `b`: a link to `link`, or `data`.
fn append_entry<W: Write>(
    b: &mut tar::Builder<W>,
    h: &mut tar::Header,
    rel: &Path,
    link: Option<&Path>,
    data: impl Read,
) -> std::io::Result<()> {
    let kind = h.entry_type();
    match link {
        Some(target) if kind.is_symlink() || kind.is_hard_link() => b.append_link(h, rel, target),
        _ => b.append_data(h, rel, data),
    }
}

/// Extended (pax) header records of a tar entry, as key and value.
type Pax = Vec<(Vec<u8>, Vec<u8>)>;

/// The pax records `entry` came with: attributes a plain header has no room for, such as
/// extended attributes, times finer than a second and long owner names.
fn pax_records<R: Read>(entry: &mut tar::Entry<R>) -> std::io::Result<Pax> {
    if entry.header().entry_type().is_pax_global_extensions() {
        // Its records are its own data, copied with it.
        return Ok(Vec::new());
    }
    Ok(entry
        .pax_extensions()?
        .map(|records| {
            records
                .filter_map(Result::ok)
                .map(|r| (r.key_bytes().to_vec(), r.value_bytes().to_vec()))
                .collect()
        })
        .unwrap_or_default())
}

/// The records of `pax` still true of an entry written again by name: its path and size are
/// written anew, and so is its time when its contents `changed`.
fn rewritten(pax: &Pax, changed: bool) -> Pax {
    pax.iter()
        .filter(|(k, _)| {
            !matches!(k.as_slice(), b"path" | b"linkpath" | b"size") && !(changed && k == b"mtime")
        })
        .cloned()
        .collect()
}

/// Writes `records` as an extended header for the entry written next; nothing when empty.
fn append_pax<W: Write>(b: &mut tar::Builder<W>, records: &Pax) -> std::io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut data = Vec::new();
    for (k, v) in records {
        // `<length> <key>=<value>\n`, the length counting its own digits too.
        let rest = k.len() + v.len() + 3;
        let mut len = rest + 1;
        while rest + len.to_string().len() != len {
            len = rest + len.to_string().len();
        }
        data.extend_from_slice(format!("{len} ").as_bytes());
        data.extend_from_slice(k);
        data.push(b'=');
        data.extend_from_slice(v);
        data.push(b'\n');
    }
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::XHeader);
    h.set_path("PaxHeader")?;
    h.set_mode(0o644);
    h.set_size(data.len() as u64);
    h.set_cksum();
    b.append(&h, data.as_slice())
}

/// `h` as the header of a plain file: a GNU sparse file is written out whole, as its holes
/// were read back as zeros.
fn as_regular(h: &tar::Header) -> tar::Header {
    let mut out = tar::Header::new_gnu();
    // Name, mode, owner, size, time, type, link name, magic and owner names: what every tar
    // header holds, without the sparse map a GNU one adds after them.
    out.as_mut_bytes()[..345].copy_from_slice(&h.as_bytes()[..345]);
    out.set_entry_type(tar::EntryType::Regular);
    out
}

/// Where `tar` unpacks an entry named `name`, relative to the folder it unpacks to: without
/// its `.` parts or a leading `/`. `None` for a name going up with `..`, never unpacked, and
/// for the archive's own root (`./`).
fn unpacked_at(name: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in name.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::ParentDir => return None,
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// The name and link target of a tar entry exactly as stored, for one that can't be written
/// again by name (`tar::Builder` tidies `./a` and refuses `/a`).
struct RawName {
    path: Vec<u8>,
    link: Option<Vec<u8>>,
}

/// Writes an entry with header `h` and its pax records as it was, under the name it had.
fn append_raw<W: Write>(
    b: &mut tar::Builder<W>,
    h: &tar::Header,
    pax: &Pax,
    name: &RawName,
    data: impl Read,
) -> std::io::Result<()> {
    let mut records = pax.clone();
    let has = |key: &[u8]| pax.iter().any(|(k, _)| k == key);
    // A name too long for the header itself came in a GNU record, written here as pax.
    if name.path.len() > 100 && !has(b"path") {
        records.push((b"path".to_vec(), name.path.clone()));
    }
    if let Some(link) = name.link.as_ref().filter(|l| l.len() > 100) {
        if !has(b"linkpath") {
            records.push((b"linkpath".to_vec(), link.clone()));
        }
    }
    let mut h = h.clone();
    h.set_cksum();
    append_pax(b, &records)?;
    b.append(&h, data)
}

/// Writes an entry with header `h` at `rel` into `b`, with what still holds of its pax records:
/// under its stored `raw` name when it has one and its contents did not change.
#[allow(clippy::too_many_arguments)]
fn put<W: Write>(
    b: &mut tar::Builder<W>,
    h: &mut tar::Header,
    rel: &Path,
    link: Option<&Path>,
    pax: &Pax,
    changed: bool,
    raw: Option<&RawName>,
    data: impl Read,
) -> std::io::Result<()> {
    match raw {
        Some(raw) if !changed => append_raw(b, h, pax, raw, data),
        _ => {
            append_pax(b, &rewritten(pax, changed))?;
            append_entry(b, h, rel, link, data)
        }
    }
}

/// Whether the files at `a` and `b` hold the same bytes; false when either is not a file.
fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    let len = match std::fs::symlink_metadata(b) {
        Ok(md) if md.is_file() => md.len(),
        _ => return Ok(false),
    };
    Ok(same_file(&mut std::fs::File::open(b)?, len, a)? == Some(true))
}

/// Copies the tar stream `input` into `w`, taking the edits made to `dir` (see [`repack`]),
/// and returns `w` with whether anything changed.
fn copy_tar<W: Write>(
    input: impl Read,
    dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
    w: W,
) -> Result<(W, bool)> {
    let mut ar = tar::Archive::new(input);
    let mut b = tar::Builder::new(w);
    b.follow_symlinks(false);
    let mut seen = HashSet::new();
    let mut implied = HashSet::new();
    // Entries whose place on disk now holds something else (a link to elsewhere, a link
    // where a file was): written from disk at the end, as new ones are.
    let mut replaced = HashSet::new();
    let mut changed = false;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for entry in ar.entries()? {
        let mut entry = entry?;
        let mut h = entry.header().clone();
        let kind = h.entry_type();
        let pax = pax_records(&mut entry)?;
        let link = entry.link_name().ok().flatten().map(|l| l.into_owned());
        let stored = entry.path_bytes().into_owned();
        let rel = entry.path().ok().and_then(|p| unpacked_at(&p));
        // Left alone, an entry the builder would name differently keeps its stored name.
        let raw = rel
            .as_deref()
            .is_none_or(|r| r.as_os_str().as_encoded_bytes() != stored.as_slice())
            .then(|| RawName {
                path: stored,
                link: entry.link_name_bytes().map(|l| l.into_owned()),
            });
        if kind == tar::EntryType::GNUSparse {
            h = as_regular(&h);
            h.set_size(entry.size());
        }
        let Some(rel) = rel else {
            if let Some(raw) = &raw {
                append_raw(&mut b, &h, &pax, raw, &mut entry)?;
            }
            continue;
        };
        add_parents(&mut implied, &rel);
        let abs = dir.join(&rel);
        let keep = if !(seen.insert(rel.clone()) && !apple_metadata(&rel) && ours(&rel)) {
            true
        } else if kind.is_dir() {
            abs.is_dir()
        } else if kind.is_symlink() {
            std::fs::read_link(&abs).ok() == link
        } else if kind.is_hard_link() {
            // Kept as a link while it still holds what its target holds; an edit saved to
            // either one gave it a file of its own.
            let target = link.as_deref().map(|l| dir.join(clean(l)));
            match std::fs::symlink_metadata(&abs) {
                Ok(md) if md.is_file() => {
                    if target.is_some_and(|t| same_contents(&abs, &t).unwrap_or(false)) {
                        true
                    } else {
                        changed = true;
                        let mut h = as_regular(&h);
                        h.set_size(md.len());
                        h.set_mtime(now);
                        let f = std::fs::File::open(&abs)
                            .with_context(|| format!("open {}", rel.display()))?;
                        put(&mut b, &mut h, &rel, None, &pax, true, None, f)
                            .with_context(|| format!("archive file {}", rel.display()))?;
                        continue;
                    }
                }
                _ => false,
            }
        } else if kind.is_file()
            || kind == tar::EntryType::Continuous
            || kind == tar::EntryType::GNUSparse
        {
            let len = entry.size();
            match same_file(&mut entry, len, &abs)? {
                Some(same) => {
                    let f = std::fs::File::open(&abs)
                        .with_context(|| format!("open {}", rel.display()))?;
                    if !same {
                        changed = true;
                        h.set_size(f.metadata()?.len());
                        h.set_mtime(now);
                    }
                    put(&mut b, &mut h, &rel, None, &pax, !same, raw.as_ref(), f)
                        .with_context(|| format!("archive file {}", rel.display()))?;
                    continue;
                }
                None => false,
            }
        } else {
            true
        };
        if keep {
            let link = link.as_deref();
            put(
                &mut b,
                &mut h,
                &rel,
                link,
                &pax,
                false,
                raw.as_ref(),
                &mut entry,
            )
            .with_context(|| format!("archive {}", rel.display()))?;
        } else {
            changed = true;
            if std::fs::symlink_metadata(&abs).is_ok() {
                replaced.insert(rel);
            }
        }
    }
    for e in list_dir(dir, false)? {
        if is_new(&e, &seen, &implied) || replaced.contains(e.rel()) {
            changed = true;
            append_new(&mut b, dir, &e)?;
        }
    }
    Ok((b.into_inner().context("finish tar")?, changed))
}

/// A temporary file in `dir` for a replacement archive to be written to.
fn staging(dir: &Path) -> Result<tempfile::NamedTempFile> {
    Ok(tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(dir)?)
}

/// Packages signed as a whole: any change would leave them with a broken signature.
const SIGNED_EXTS: &[&str] = &["apk", "ipa", "appx", "msix", "crx", "xpi"];

/// Whether edits to an extraction of `path` can be written back with [`repack`]. Not for a
/// signed package, whose signature a change would break, nor for `.tar.xz`, which could only
/// be written back uncompressed here.
pub fn writable(path: &Path) -> bool {
    match kind(path) {
        None | Some(Kind::TarXz) => false,
        Some(Kind::Zip) => {
            !SIGNED_EXTS.contains(&super::text::extension(path).as_str()) && !zip_signed(path)
        }
        Some(_) => true,
    }
}

/// Whether the zip at `path` carries a signature: a signed jar (`META-INF/*.SF` and its key
/// block), a signed OpenDocument or EPUB file, a signed Office document or other Open
/// Packaging Conventions package (a VSIX), a signed NuGet package, or a signed wheel.
fn zip_signed(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(ar) = zip::ZipArchive::new(std::io::BufReader::new(file)) else {
        return false;
    };
    let signed = ar.file_names().any(|n| {
        let n = n.to_ascii_lowercase();
        let meta = n.strip_prefix("meta-inf/").unwrap_or("");
        n.starts_with("_xmlsignatures/")
            || n.starts_with("package/services/digital-signature/")
            || n == ".signature.p7s"
            || n.ends_with(".dist-info/record.jws")
            || n.ends_with(".dist-info/record.p7s")
            || [".sf", ".rsa", ".dsa", ".ec", "signatures.xml"]
                .iter()
                .any(|end| meta.ends_with(end))
    });
    signed
}

fn zip_now() -> zip::DateTime {
    use chrono::{Datelike, Timelike};
    let t = chrono::Local::now();
    zip::DateTime::from_date_and_time(
        t.year().try_into().unwrap_or(1980),
        t.month() as u8,
        t.day() as u8,
        t.hour() as u8,
        t.minute() as u8,
        t.second().min(58) as u8,
    )
    .unwrap_or_default()
}

/// The name of `rel` inside a zip, with `/` between its parts.
fn zip_name(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The permission bits of `md` for an archive entry.
fn mode_of(md: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        if md.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

/// Finds the extraction recorded for `dir`: how its archive was then, and which entries it
/// wrote (`None`, all of them, for a folder not extracted here).
fn recorded(dir: &Path) -> (Option<Stamp>, Option<Written>) {
    match find_extraction(&mut extractions(), dir) {
        Some(e) => (e.stamp, Some(e.written.clone())),
        None => (None, None),
    }
}

/// Notes that the archive behind `dir` was just written from it: it now holds every entry
/// found there, as it is now.
fn rerecord(dir: &Path, archive: &Path) {
    let on_disk: Vec<PathBuf> = list_dir(dir, false)
        .map(|v| v.iter().map(|e| e.rel().to_path_buf()).collect())
        .unwrap_or_default();
    if let Some(e) = find_extraction(&mut extractions(), dir) {
        e.stamp = stamp(archive);
        let mut written = (*e.written).clone();
        written.extend(on_disk);
        e.written = std::sync::Arc::new(written);
    }
}

/// Writes the edits made to `dir`, an extraction of the archive at `path`, back into it.
/// Entries left alone keep their bytes, times and order (in a zip, their compressed data
/// too), as do entries never extracted; changed files get the current time; removed entries
/// are left out, and new ones are added at the end. Nothing is written when nothing changed;
/// returns whether the archive was written.
///
/// The archive is written to a temporary file beside it, which then takes its place keeping
/// its permissions (through a symbolic link, to the file it leads to), so a failure or a
/// crash leaves the original untouched. Refused for an archive [`writable`] turns down, a
/// read-only one, and one changed since it was extracted.
pub fn repack(path: &Path, dir: &Path) -> Result<bool> {
    use crate::i18n::{tr, trf};
    let file = || ("file", path.display().to_string());
    if !writable(path) {
        anyhow::bail!(trf("msg.archive_not_writable", &[file()]));
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let md = std::fs::metadata(&target).with_context(|| format!("open {}", path.display()))?;
    if md.permissions().readonly() {
        anyhow::bail!(trf("msg.archive_read_only", &[file()]));
    }
    let (then, written) = recorded(dir);
    if then.is_some() && stamp(&target) != then {
        anyhow::bail!(trf("msg.archive_changed", &[file()]));
    }
    let ours = |rel: &Path| written.as_ref().is_none_or(|w| w.contains(rel));
    let staged = repack_into(&target, dir, &ours)
        .map_err(|e| anyhow::anyhow!("{}\n{e:#}", trf("msg.archive_write_failed", &[file()])))?;
    let Some(tmp) = staged else {
        return Ok(false);
    };
    // Writing it can take a while: checked again just before it takes the archive's place.
    if then.is_some() && stamp(&target) != then {
        anyhow::bail!(trf("msg.archive_changed", &[file()]));
    }
    tmp.as_file().set_permissions(md.permissions())?;
    tmp.as_file().sync_all()?;
    fsutil::persist(tmp, &target)
        .map_err(|e| anyhow::anyhow!("{}\n{e:#}", tr("msg.archive_replace_failed")))?;
    rerecord(dir, &target);
    Ok(true)
}

/// Writes the replacement for the archive at `path` (see [`repack`]), or `None` when it would
/// be the same.
fn repack_into(
    path: &Path,
    dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
) -> Result<Option<tempfile::NamedTempFile>> {
    let out_dir = parent_dir(path);
    let open = || -> Result<std::io::BufReader<std::fs::File>> {
        Ok(std::io::BufReader::new(std::fs::File::open(path)?))
    };
    let (tmp, changed) = match kind(path) {
        Some(Kind::Zip) => repack_zip(path, dir, out_dir, ours)?,
        Some(Kind::Tar) => copy_tar(open()?, dir, ours, staging(out_dir)?)?,
        Some(Kind::TarGz) => {
            let input = flate2::read::MultiGzDecoder::new(open()?);
            let enc = flate2::write::GzEncoder::new(staging(out_dir)?, Default::default());
            let (enc, changed) = copy_tar(input, dir, ours, enc)?;
            (enc.finish().context("finish gzip")?, changed)
        }
        Some(Kind::TarBz2) => {
            let input = bzip2::read::MultiBzDecoder::new(open()?);
            let enc = bzip2::write::BzEncoder::new(staging(out_dir)?, Default::default());
            let (enc, changed) = copy_tar(input, dir, ours, enc)?;
            (enc.finish().context("finish bzip2")?, changed)
        }
        Some(Kind::SevenZ) => {
            if !sevenz_changed(path, dir)? {
                return Ok(None);
            }
            (repack_7z(path, dir, out_dir)?, true)
        }
        Some(Kind::TarXz) | None => anyhow::bail!("{}: cannot be written", path.display()),
    };
    Ok(changed.then_some(tmp))
}

fn repack_zip(
    path: &Path,
    dir: &Path,
    out_dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
) -> Result<(tempfile::NamedTempFile, bool)> {
    use zip::{write::SimpleFileOptions, CompressionMethod};
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path)?))?;
    let mut tmp = staging(out_dir)?;
    let mut seen = HashSet::new();
    let mut implied = HashSet::new();
    let mut changed = false;
    let now = zip_now();
    {
        let mut w = zip::ZipWriter::new(std::io::BufWriter::new(tmp.as_file_mut()));
        w.set_raw_comment(ar.comment().into());
        for i in 0..ar.len() {
            let (name, rel, is_dir, method, mode, len) = {
                let f = ar.by_index_raw(i)?;
                let rel = f.enclosed_name().map(|r| clean(&r));
                (
                    f.name().to_owned(),
                    rel,
                    f.is_dir(),
                    f.compression(),
                    f.unix_mode(),
                    f.size(),
                )
            };
            if let Some(rel) = &rel {
                add_parents(&mut implied, rel);
            }
            let rel = rel.filter(|r| seen.insert(r.clone()) && !apple_metadata(r) && ours(r));
            let keep = match rel {
                None => true,
                Some(rel) if is_dir => dir.join(rel).is_dir(),
                Some(rel) => {
                    let abs = dir.join(&rel);
                    match same_file(&mut ar.by_index(i)?, len, &abs)? {
                        Some(true) => true,
                        Some(false) => {
                            changed = true;
                            let size = std::fs::metadata(&abs)?.len();
                            let method = match method {
                                CompressionMethod::Stored => CompressionMethod::Stored,
                                _ => CompressionMethod::Deflated,
                            };
                            let mut o = SimpleFileOptions::default()
                                .compression_method(method)
                                .last_modified_time(now)
                                .large_file(size > u32::MAX as u64);
                            if let Some(m) = mode {
                                o = o.unix_permissions(m & 0o777);
                            }
                            if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
                                let target = std::fs::read_to_string(&abs)?;
                                w.add_symlink(name, target, o)?;
                            } else {
                                w.start_file(name, o)?;
                                let mut f = std::fs::File::open(&abs)
                                    .with_context(|| format!("open {}", rel.display()))?;
                                std::io::copy(&mut f, &mut w)?;
                            }
                            continue;
                        }
                        None => false,
                    }
                }
            };
            if keep {
                w.raw_copy_file(ar.by_index_raw(i)?)?;
            } else {
                changed = true;
            }
        }
        for e in list_dir(dir, true)? {
            if !is_new(&e, &seen, &implied) {
                continue;
            }
            changed = true;
            let rel = e.rel();
            let o = SimpleFileOptions::default().last_modified_time(now);
            match e {
                Entry::Dir(_) => w.add_directory(zip_name(rel), o)?,
                Entry::File(_) => {
                    let mut f = std::fs::File::open(dir.join(rel))
                        .with_context(|| format!("open {}", rel.display()))?;
                    let md = f.metadata()?;
                    let o = o
                        .unix_permissions(mode_of(&md))
                        .large_file(md.len() > u32::MAX as u64);
                    w.start_file(zip_name(rel), o)?;
                    std::io::copy(&mut f, &mut w)?;
                }
                Entry::Symlink(..) => unreachable!("zip entries follow symlinks"),
            }
        }
        w.finish()
            .context("finish zip")?
            .into_inner()
            .map_err(|e| e.into_error())?;
    }
    Ok((tmp, changed))
}

/// Whether `dir`, an extraction of the 7z archive at `path`, differs from it.
fn sevenz_changed(path: &Path, dir: &Path) -> Result<bool> {
    let mut seen = HashSet::new();
    let mut implied = HashSet::new();
    let mut changed = false;
    sevenz_rust2::decompress_file_with_extract_fn(path, dir, |e, r, _| {
        let rel = clean(Path::new(e.name()));
        add_parents(&mut implied, &rel);
        let abs = dir.join(&rel);
        if e.is_directory() {
            changed |= !abs.is_dir();
        } else {
            let same = same_file(r, e.size(), &abs)
                .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
            changed |= same != Some(true);
        }
        seen.insert(rel);
        Ok(true)
    })
    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(changed
        || list_dir(dir, true)?
            .iter()
            .any(|e| is_new(e, &seen, &implied)))
}

fn repack_7z(path: &Path, dir: &Path, out_dir: &Path) -> Result<tempfile::NamedTempFile> {
    let entries = list_dir(dir, true)?;
    let mut tmp = staging(out_dir)?;
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
    Ok(tmp)
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
            (&sevenz_path, "dir/a.txt"),
        ] {
            let extracted = extract_to_temp(archive).unwrap();
            let target = extracted.path().join(rel);
            std::fs::write(&target, "new\n").unwrap();
            std::fs::write(extracted.path().join("added.txt"), "added\n").unwrap();

            let written = super::repack(archive, extracted.path())
                .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
            assert!(written, "{}", archive.display());

            let after = extract_to_temp(archive).unwrap();
            let changed = std::fs::read_to_string(after.path().join(rel)).unwrap();
            assert_eq!(changed, "new\n", "{}", archive.display());
            let added = std::fs::read_to_string(after.path().join("added.txt")).unwrap();
            assert_eq!(added, "added\n", "{}", archive.display());
        }

        // A .tar.xz could only be written back uncompressed, so it is left read-only.
        assert!(!writable(&txz_path));
        let extracted = extract_to_temp(&txz_path).unwrap();
        std::fs::write(extracted.path().join("dir/a.txt"), "new\n").unwrap();
        assert!(super::repack(&txz_path, extracted.path()).is_err());

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

    /// Entries left alone keep their order, times, compression and bytes, so an EPUB's
    /// `mimetype` stays first and stored; an edited entry keeps its place and method; and an
    /// archive whose contents did not change is not written at all.
    #[test]
    fn repack_keeps_untouched_zip_entries_as_they_were() {
        use zip::{write::SimpleFileOptions, CompressionMethod, DateTime};
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("book.epub");
        let old = DateTime::from_date_and_time(2001, 2, 3, 4, 5, 6).unwrap();
        let stored = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(old);
        let deflated = stored.compression_method(CompressionMethod::Deflated);
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        w.start_file("mimetype", stored).unwrap();
        w.write_all(b"application/epub+zip").unwrap();
        w.start_file("z.xhtml", deflated).unwrap();
        w.write_all(b"<p>last letter first</p>").unwrap();
        w.start_file("a.css", stored).unwrap();
        w.write_all(b"p {}").unwrap();
        w.set_comment("kept");
        w.finish().unwrap();
        let before = std::fs::read(&path).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        std::fs::write(extracted.path().join("a.css"), "p { color: red }").unwrap();
        std::fs::write(extracted.path().join("new.txt"), "new").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let mut ar = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let names: Vec<&str> = ar.file_names().collect();
        assert_eq!(names.len(), 4);
        assert_eq!(ar.comment(), b"kept");
        let entry = |ar: &mut zip::ZipArchive<std::fs::File>, i: usize| {
            let f = ar.by_index(i).unwrap();
            (f.name().to_owned(), f.compression(), f.last_modified())
        };
        let (name, method, time) = entry(&mut ar, 0);
        assert_eq!(
            (name.as_str(), method, time),
            ("mimetype", CompressionMethod::Stored, Some(old))
        );
        let (name, method, time) = entry(&mut ar, 1);
        assert_eq!(
            (name.as_str(), method, time),
            ("z.xhtml", CompressionMethod::Deflated, Some(old))
        );
        let (name, method, time) = entry(&mut ar, 2);
        assert_eq!(
            (name.as_str(), method),
            ("a.css", CompressionMethod::Stored)
        );
        assert_ne!(time, Some(old));
        assert_eq!(entry(&mut ar, 3).0, "new.txt");
    }

    /// A tar entry left alone keeps its time; an edited one gets the current time.
    #[test]
    fn repack_keeps_untouched_tar_entries_as_they_were() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        for name in ["keep.txt", "edit.txt"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o640);
            h.set_mtime(1_000_000_000);
            h.set_cksum();
            b.append_data(&mut h, name, &b"old"[..]).unwrap();
        }
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(extracted.path().join("edit.txt"), "new!").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let mut ar = tar::Archive::new(std::fs::File::open(&path).unwrap());
        let got: Vec<(String, u64, u32)> = ar
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let h = e.header();
                let name = e.path().unwrap().display().to_string();
                (name, h.mtime().unwrap(), h.mode().unwrap())
            })
            .collect();
        assert_eq!(got[0], ("keep.txt".to_string(), 1_000_000_000, 0o640));
        assert_eq!(got[1].0, "edit.txt");
        assert_ne!(got[1].1, 1_000_000_000);
        assert_eq!(got[1].2, 0o640);
    }

    /// An unchanged 7z is not rewritten either.
    #[test]
    fn repack_leaves_an_unchanged_7z_alone() {
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("src");
        std::fs::create_dir_all(src.join("dir")).unwrap();
        std::fs::write(src.join("dir/a.txt"), "hello\n").unwrap();
        let path = work.path().join("a.7z");
        sevenz_rust2::compress_to_path(&src, &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// Edits are not written over an archive that changed on disk since it was extracted,
    /// nor into a read-only one.
    #[test]
    fn repack_refuses_an_archive_changed_since_or_read_only() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        std::fs::write(&path, tar_bytes()).unwrap();
        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("dir/a.txt"), "mine\n").unwrap();

        let mut theirs = tar_bytes();
        theirs.extend_from_slice(&[0; 512]);
        std::fs::write(&path, &theirs).unwrap();
        assert!(repack(&path, extracted.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), theirs);

        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("dir/a.txt"), "mine\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        assert!(repack(&path, extracted.path()).is_err());
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(repack(&path, extracted.path()).unwrap());
    }

    /// A `.bak` made beside a saved file would be packed into the archive with it.
    #[test]
    fn no_backups_are_made_inside_an_extracted_archive() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        std::fs::write(&path, tar_bytes()).unwrap();
        let extracted = extract_to_temp(&path).unwrap();
        let inside = extracted.path().join("dir/a.txt");
        assert!(!fsutil::backs_up(&inside, true));
        assert!(fsutil::backs_up(&path, true));
        assert!(!fsutil::backs_up(&path, false));
    }

    /// Signed packages are opened read-only: a change would break the signature.
    #[test]
    fn signed_packages_are_not_writable() {
        let work = tempfile::tempdir().unwrap();
        let zip_with = |name: &str, entries: &[&str]| {
            let path = work.path().join(name);
            let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for e in entries {
                w.start_file(*e, zip::write::SimpleFileOptions::default())
                    .unwrap();
            }
            w.finish().unwrap();
            path
        };
        assert!(writable(&zip_with("plain.jar", &["a.class"])));
        assert!(!writable(&zip_with(
            "signed.jar",
            &[
                "META-INF/MANIFEST.MF",
                "META-INF/CERT.SF",
                "META-INF/CERT.RSA",
                "a.class"
            ]
        )));
        assert!(!writable(&zip_with("app.apk", &["classes.dex"])));
        assert!(!writable(&zip_with(
            "signed.odt",
            &["mimetype", "META-INF/documentsignatures.xml"]
        )));
        assert!(writable(&zip_with("plain.docx", &["word/document.xml"])));
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
        assert!(!tar_names(&tar_path).contains(&"escape.txt".to_string()));
    }

    /// The names of the entries of the tar at `path`, as stored.
    fn tar_names(path: &Path) -> Vec<String> {
        let mut ar = tar::Archive::new(std::fs::File::open(path).unwrap());
        ar.entries()
            .unwrap()
            .map(|e| String::from_utf8_lossy(&e.unwrap().path_bytes()).into_owned())
            .collect()
    }

    /// A header for `name` stored as it is, which `tar::Builder` would tidy (`./a`) or refuse
    /// (`/a`).
    fn named_header(name: &str, kind: tar::EntryType, size: u64) -> tar::Header {
        let mut h = tar::Header::new_gnu();
        h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(kind);
        h.set_size(size);
        h.set_mode(0o644);
        h.set_cksum();
        h
    }

    /// `tar -C dir -cf x.tar .` names every entry `./...`, starting with `./` itself.
    #[test]
    fn repack_takes_edits_to_a_tar_naming_its_entries_from_dot() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        b.append(&named_header("./", tar::EntryType::Directory, 0), &[][..])
            .unwrap();
        b.append(
            &named_header("./a.txt", tar::EntryType::Regular, 4),
            &b"old\n"[..],
        )
        .unwrap();
        b.append(
            &named_header("./keep.txt", tar::EntryType::Regular, 5),
            &b"keep\n"[..],
        )
        .unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        std::fs::write(extracted.path().join("a.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());
        let after = extract_to_temp(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(after.path().join("a.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(after.path().join("keep.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(tar_names(&path), ["./", "a.txt", "./keep.txt"]);
    }

    /// A link pointed elsewhere, or put where a file was, is written as it now is.
    #[cfg(unix)]
    #[test]
    fn repack_writes_a_tar_link_that_changed() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        for name in ["v2/x", "v3/x", "file.txt"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            b.append_data(&mut h, name, &b"x"[..]).unwrap();
        }
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "current", "v2").unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        let current = extracted.path().join("current");
        std::fs::remove_file(&current).unwrap();
        std::os::unix::fs::symlink("v3", &current).unwrap();
        let file = extracted.path().join("file.txt");
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("v2/x", &file).unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let after = extract_to_temp(&path).unwrap();
        let target = |name: &str| std::fs::read_link(after.path().join(name)).ok();
        assert_eq!(target("current"), Some(PathBuf::from("v3")));
        assert_eq!(target("file.txt"), Some(PathBuf::from("v2/x")));
    }

    /// A hard link stays one while it holds what its target does; an edit saved to it (as a
    /// new file, the way files are saved) is written as a file of its own.
    #[cfg(unix)]
    #[test]
    fn repack_takes_an_edit_to_a_tar_hard_link() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(4);
        h.set_mode(0o644);
        b.append_data(&mut h, "a.txt", &b"old\n"[..]).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        b.append_link(&mut h, "b.txt", "a.txt").unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        let linked = extracted.path().join("b.txt");
        std::fs::remove_file(&linked).unwrap();
        std::fs::write(&linked, "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let after = extract_to_temp(&path).unwrap();
        let read = |name: &str| std::fs::read_to_string(after.path().join(name)).unwrap();
        assert_eq!(
            (read("a.txt"), read("b.txt")),
            ("old\n".into(), "new\n".into())
        );
    }

    /// A GNU sparse file is written out whole (its holes as zeros) rather than with a header
    /// that no longer fits its data, which would leave every entry after it unreadable.
    #[test]
    fn repack_keeps_a_tar_readable_past_a_sparse_file() {
        fn octal(field: &mut [u8], value: u64) {
            let digits = format!("{value:0w$o}", w = field.len() - 1);
            field[..digits.len()].copy_from_slice(digits.as_bytes());
        }
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut h = named_header("hole.bin", tar::EntryType::GNUSparse, 3);
        let gnu = h.as_gnu_mut().unwrap();
        octal(&mut gnu.sparse[0].offset, 1024);
        octal(&mut gnu.sparse[0].numbytes, 3);
        octal(&mut gnu.realsize, 1027);
        h.set_cksum();
        let mut b = tar::Builder::new(Vec::new());
        b.append(&h, &b"abc"[..]).unwrap();
        b.append(
            &named_header("edit.txt", tar::EntryType::Regular, 4),
            &b"old\n"[..],
        )
        .unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("edit.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let after = extract_to_temp(&path).unwrap();
        let mut hole = vec![0u8; 1024];
        hole.extend_from_slice(b"abc");
        assert_eq!(std::fs::read(after.path().join("hole.bin")).unwrap(), hole);
        assert_eq!(
            std::fs::read_to_string(after.path().join("edit.txt")).unwrap(),
            "new\n"
        );
    }

    /// Entries left alone keep their pax records (extended attributes, say), and one stored
    /// from the root, never extracted, is kept rather than failing the whole repack.
    #[test]
    fn repack_keeps_pax_records_and_entries_it_never_extracted() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        let xattr = (b"SCHILY.xattr.user.k".to_vec(), b"v".to_vec());
        append_pax(&mut b, &vec![xattr.clone()]).unwrap();
        b.append(
            &named_header("keep.txt", tar::EntryType::Regular, 5),
            &b"keep\n"[..],
        )
        .unwrap();
        b.append(
            &named_header("/abs.txt", tar::EntryType::Regular, 4),
            &b"abs\n"[..],
        )
        .unwrap();
        b.append(
            &named_header("edit.txt", tar::EntryType::Regular, 4),
            &b"old\n"[..],
        )
        .unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("edit.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        assert_eq!(tar_names(&path), ["keep.txt", "/abs.txt", "edit.txt"]);
        let mut ar = tar::Archive::new(std::fs::File::open(&path).unwrap());
        let mut first = ar.entries().unwrap().next().unwrap().unwrap();
        let records = pax_records(&mut first).unwrap();
        assert_eq!(records, vec![xattr]);
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
