//! Archive support: archives are extracted to a temporary folder and compared as folders.
//! Zip based formats, tar (plain or compressed with gzip, bzip2, xz or Zstandard), 7z and RAR
//! are supported, password-protected zip, 7z and RAR archives included. Edits saved into the
//! extracted copy are written back with [`repack`], which keeps every entry left alone as it
//! was; a RAR archive is only read, as nothing free writes one.

mod rar;

use super::fsutil;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

pub const ZIP_EXTS: &[&str] = &[
    "zip", "jar", "war", "ear", "apk", "aar", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub",
    "nupkg", "vsix", "whl", "xpi", "crx", "ipa", "appx", "msix", "xws",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    SevenZ,
    Rar,
}

/// File name endings of the tar, 7z and RAR formats; compound ones first.
const SUFFIXES: &[(&str, Kind)] = &[
    (".tar.gz", Kind::TarGz),
    (".tgz", Kind::TarGz),
    (".tar.bz2", Kind::TarBz2),
    (".tbz2", Kind::TarBz2),
    (".tbz", Kind::TarBz2),
    (".tar.xz", Kind::TarXz),
    (".txz", Kind::TarXz),
    (".tar.zst", Kind::TarZst),
    (".tzst", Kind::TarZst),
    (".tar", Kind::Tar),
    (".7z", Kind::SevenZ),
    (".rar", Kind::Rar),
];

/// The password of an encrypted archive, wiped from memory once dropped.
type Secret = Arc<Zeroizing<String>>;

/// An archive that can't be read without its password: none was given, or the one given is
/// wrong. Ask for it, [`give_password`], and try again.
#[derive(Debug)]
pub struct NeedsPassword {
    pub archive: PathBuf,
    pub wrong: bool,
}

impl std::fmt::Display for NeedsPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = if self.wrong {
            "msg.archive_password_wrong"
        } else {
            "msg.archive_password_needed"
        };
        let file = ("file", self.archive.display().to_string());
        f.write_str(&crate::i18n::trf(key, &[file]))
    }
}

impl std::error::Error for NeedsPassword {}

/// Passwords given for archives about to be extracted, kept until [`forget_password`]. An
/// extraction keeps its own, to write the archive back encrypted.
static PASSWORDS: std::sync::Mutex<Vec<(PathBuf, Secret)>> = std::sync::Mutex::new(Vec::new());

fn passwords() -> std::sync::MutexGuard<'static, Vec<(PathBuf, Secret)>> {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Gives the password to open the archive at `path` with, replacing any given before.
pub fn give_password(path: &Path, password: Zeroizing<String>) {
    let mut all = passwords();
    all.retain(|(p, _)| p != path);
    all.push((path.to_path_buf(), Arc::new(password)));
}

/// Drops the password given with [`give_password`] for `path`; extractions keep theirs.
pub fn forget_password(path: &Path) {
    passwords().retain(|(p, _)| p != path);
}

fn password_for(path: &Path) -> Option<Secret> {
    passwords()
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, s)| s.clone())
}

/// The error for the archive at `path` when it needs a password: `wrong` when one was tried.
fn needs_password(path: &Path, tried: Option<&Secret>) -> anyhow::Error {
    NeedsPassword {
        archive: path.to_path_buf(),
        wrong: tried.is_some(),
    }
    .into()
}

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
    archive: PathBuf,
    dir: PathBuf,
    /// `dir` with every symbolic link on the way resolved.
    real: Option<PathBuf>,
    stamp: Option<Stamp>,
    written: Written,
    /// What the archive was opened with, if it is encrypted.
    password: Option<Secret>,
}

static EXTRACTIONS: std::sync::Mutex<Vec<Extraction>> = std::sync::Mutex::new(Vec::new());

fn extractions() -> std::sync::MutexGuard<'static, Vec<Extraction>> {
    EXTRACTIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops what was kept of the extractions whose folder was removed, passwords included.
pub fn forget_removed() {
    extractions().retain(|e| e.dir.exists());
}

/// Whether `path` is in a folder an archive was extracted to, where a backup copy of a saved
/// file would end up packed into the archive.
pub fn inside_extraction(path: &Path) -> bool {
    extraction_of(path).is_some()
}

/// The archive, and the folder it was extracted to, of the extraction `path` is in.
fn extraction_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let all = extractions();
    let found = |e: &Extraction| (e.archive.clone(), e.dir.clone());
    if let Some(e) = all.iter().find(|e| path.starts_with(&e.dir)) {
        return Some(found(e));
    }
    let real = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.join(path.file_name().unwrap_or_default()))?;
    all.iter()
        .find(|e| e.real.as_ref().is_some_and(|d| real.starts_with(d)))
        .map(found)
}

/// The extraction recorded for the folder `dir`, if any.
fn find_extraction<'a>(all: &'a mut [Extraction], dir: &Path) -> Option<&'a mut Extraction> {
    let real = std::fs::canonicalize(dir).ok();
    all.iter_mut()
        .find(|e| e.dir == dir || real.is_some() && e.real == real)
}

/// Extracts the archive at `path` to a new temporary folder. An encrypted zip, 7z or RAR is
/// read with the password [`give_password`] gave for it, failing with [`NeedsPassword`] without
/// one or with a wrong one.
pub fn extract_to_temp(path: &Path) -> Result<tempfile::TempDir> {
    extract_with(path, LIMITS)
}

fn extract_with(path: &Path, limits: Limits) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("oxidemerge-").tempdir()?;
    let budget = Budget::new(limits);
    let stamp = stamp(path);
    let written = std::cell::RefCell::new(HashSet::new());
    let password = password_for(path);
    extract(path, dir.path(), &budget, &written, password.as_ref())
        .map_err(|e| budget.error(path, e))?;
    let mut written = written.into_inner();
    for gone in remove_links_outside(dir.path())? {
        written.remove(&gone);
    }
    forget_removed();
    extractions().push(Extraction {
        archive: path.to_path_buf(),
        dir: dir.path().to_path_buf(),
        real: std::fs::canonicalize(dir.path()).ok(),
        stamp,
        written: std::sync::Arc::new(written),
        password,
    });
    Ok(dir)
}

fn extract(
    path: &Path,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
    password: Option<&Secret>,
) -> Result<()> {
    let open = || std::fs::File::open(path).with_context(|| format!("open {}", path.display()));
    let extract_tar = |input: &mut dyn Read| extract_tar(input, dir, budget, written);
    match kind(path).unwrap_or(Kind::Zip) {
        Kind::Zip => extract_zip(path, open()?, dir, budget, written, password)
            .with_context(|| path.display().to_string()),
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
        Kind::TarZst => extract_tar(&mut zstd::stream::read::Decoder::new(open()?)?),
        Kind::SevenZ => sevenz_rust2::decompress_with_extract_fn_and_password(
            std::io::BufReader::new(open()?),
            dir,
            sevenz_password(password),
            |e, r, dest| {
                budget.entry()?;
                let mut r = Counted { inner: r, budget };
                let done = sevenz_rust2::default_entry_extract_fn(e, &mut r, dest)?;
                written.borrow_mut().insert(clean(Path::new(e.name())));
                Ok(done)
            },
        )
        .map_err(|e| sevenz_error(path, password, e)),
        Kind::Rar => extract_rar(path, dir, budget, written, password),
    }
}

/// Whether `name`, the name of an archive's entry, stays in the archive: no step of it
/// leads up or to a root.
fn name_inside(name: &Path) -> bool {
    name.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Extracts the RAR archive at `path`, of any version of the format (see [`rar`]). The
/// files are written here from the data the UnRAR library unpacks, so a symbolic link in
/// the archive is left out, and an entry stored as a copy of another is copied from it. A
/// set of volumes is turned down, whichever of them `path` is.
fn extract_rar(
    path: &Path,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
    password: Option<&Secret>,
) -> Result<()> {
    // Only a RAR 5 archive can tell a wrong password; an older one decrypts to data that
    // does not check out, which is taken for one where the data is `encrypted`.
    let failed = |e: rar::Error, encrypted: bool| match e {
        rar::Error::NeedsPassword => needs_password(path, password),
        rar::Error::BadData if encrypted && password.is_some() => needs_password(path, password),
        rar::Error::TooLarge => {
            budget.bytes.set(None);
            anyhow::anyhow!("archive too large")
        }
        e => anyhow::anyhow!("{}: {e}", path.display()),
    };
    let mut ar =
        rar::Reader::open(path, password.map(|p| p.as_str())).map_err(|e| failed(e, false))?;
    let names_encrypted = ar.names_encrypted();
    while let Some(entry) = ar.next().map_err(|e| failed(e, names_encrypted))? {
        budget.entry()?;
        let encrypted = entry.encrypted || names_encrypted;
        // A volume with encrypted names may only show as one by a file going on in the next.
        if entry.split {
            return Err(failed(rar::Error::Volumes, false));
        }
        // A name leading out of the archive is not extracted, as for a zip.
        let rel = clean(&entry.name);
        if !name_inside(&entry.name) || apple_metadata(&rel) || rel.as_os_str().is_empty() {
            ar.skip().map_err(|e| failed(e, encrypted))?;
            continue;
        }
        let out = dir.join(&rel);
        if entry.dir {
            ar.skip().map_err(|e| failed(e, encrypted))?;
            std::fs::create_dir_all(&out)?;
        } else if let Some(link) = &entry.link {
            ar.skip().map_err(|e| failed(e, encrypted))?;
            // Only what was extracted before, a regular file, can be the one to copy.
            let from = match link {
                rar::Link::SameAs(name) if name_inside(name) => clean(name),
                _ => continue,
            };
            if !written.borrow().contains(&from) || !dir.join(&from).is_file() {
                continue;
            }
            Budget::take(&budget.bytes, std::fs::metadata(dir.join(&from))?.len())?;
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(dir.join(&from), &out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let file = std::fs::File::create(&out)?;
            // The header's size is only what the archive says: what is written counts.
            let limit = budget.bytes.get().unwrap_or(0);
            let n = ar.extract(file, limit).map_err(|e| failed(e, encrypted))?;
            Budget::take(&budget.bytes, n)?;
            if let Some(time) = entry.modified {
                let file = std::fs::File::options().write(true).open(&out)?;
                // Not every file system keeps a time: the file is extracted all the same.
                let _ = file.set_modified(time);
            }
        }
        written.borrow_mut().insert(rel);
    }
    Ok(())
}

fn sevenz_password(password: Option<&Secret>) -> sevenz_rust2::Password {
    password.map_or_else(sevenz_rust2::Password::empty, |p| p.as_str().into())
}

/// The error for a 7z archive that could not be read: [`NeedsPassword`] when a password was
/// missing or, judging by encrypted data that would not decode, wrong.
fn sevenz_error(path: &Path, password: Option<&Secret>, e: sevenz_rust2::Error) -> anyhow::Error {
    use sevenz_rust2::Error as E;
    match e {
        E::PasswordRequired => needs_password(path, password),
        E::MaybeBadPassword(_) => needs_password(path, password),
        // A wrong key decrypts to garbage, which fails however it happens to.
        E::FileOpen(..) | E::MaxMemLimited { .. } => anyhow::anyhow!("{}: {e}", path.display()),
        _ if password.is_some() && sevenz_encrypted(path) => needs_password(path, password),
        e => anyhow::anyhow!("{}: {e}", path.display()),
    }
}

/// Whether the 7z archive at `path` holds encrypted data, or encrypted names.
fn sevenz_encrypted(path: &Path) -> bool {
    use sevenz_rust2::{Archive, EncoderMethod, Password};
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    match Archive::read(&mut std::io::BufReader::new(file), &Password::empty()) {
        Err(sevenz_rust2::Error::PasswordRequired) => true,
        Err(_) => false,
        Ok(ar) => ar.blocks.iter().any(|b| {
            b.coders
                .iter()
                .any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
        }),
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
    path: &Path,
    file: std::fs::File,
    dir: &Path,
    budget: &Budget,
    written: &std::cell::RefCell<HashSet<PathBuf>>,
    password: Option<&Secret>,
) -> Result<()> {
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    for i in 0..ar.len() {
        budget.entry()?;
        let (rel, is_dir, encrypted) = {
            let f = ar.by_index_raw(i)?;
            (f.enclosed_name(), f.is_dir(), f.encrypted())
        };
        let Some(rel) = rel.filter(|r| !apple_metadata(r)) else {
            continue;
        };
        let out = dir.join(&rel);
        if is_dir {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let f = zip_entry(&mut ar, i, path, password)?;
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut Counted { inner: f, budget }, &mut w)
                .map_err(|e| zip_read_error(path, password, encrypted, e))?;
        }
        written.borrow_mut().insert(clean(&rel));
    }
    Ok(())
}

/// Entry `i` of `ar` (in the archive at `path`), decompressed and, when it is encrypted,
/// decrypted with `password`.
fn zip_entry<'a, R: Read + Seek>(
    ar: &'a mut zip::ZipArchive<R>,
    i: usize,
    path: &Path,
    password: Option<&Secret>,
) -> Result<zip::read::ZipFile<'a, R>> {
    if !ar.by_index_raw(i)?.encrypted() {
        return Ok(ar.by_index(i)?);
    }
    let Some(p) = password else {
        return Err(needs_password(path, None));
    };
    match ar.by_index_decrypt(i, p.as_bytes()) {
        Ok(f) => Ok(f),
        Err(zip::result::ZipError::InvalidPassword) => Err(needs_password(path, password)),
        Err(e) => Err(e.into()),
    }
}

/// The error for reading an entry of the zip at `path` that failed: for an encrypted one,
/// data that does not check out means the password was wrong (a wrong one passes the first
/// check one time in 256 for the old zip encryption).
fn zip_read_error(
    path: &Path,
    password: Option<&Secret>,
    encrypted: bool,
    e: std::io::Error,
) -> anyhow::Error {
    if encrypted && e.kind() == std::io::ErrorKind::InvalidData {
        needs_password(path, password)
    } else {
        e.into()
    }
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
        if entry.header().entry_type() == tar::EntryType::GNUSparse {
            // Its holes take no room in the archive but are written out as zeros.
            let stored = entry.header().entry_size().unwrap_or(0);
            Budget::take(&budget.bytes, entry.size().saturating_sub(stored))?;
        }
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
        Some(target) if kind.is_symlink() || kind.is_hard_link() => {
            // A link holds no data, whatever size its header gave.
            h.set_size(0);
            b.append_link(h, rel, target)
        }
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
            !matches!(k.as_slice(), b"path" | b"linkpath" | b"size")
                && !(changed
                    && (k == b"mtime" || k == b"SCHILY.realsize" || k.starts_with(b"GNU.sparse.")))
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

/// For each place a tar stream unpacks to, the index of the last entry unpacked there: the
/// one left on disk, as `tar` writes an entry over an earlier one of the same name.
fn last_entries(input: impl Read) -> Result<HashMap<PathBuf, usize>> {
    let mut last = HashMap::new();
    for (i, entry) in tar::Archive::new(input).entries()?.enumerate() {
        if let Some(rel) = entry?.path().ok().and_then(|p| unpacked_at(&p)) {
            last.insert(rel, i);
        }
    }
    Ok(last)
}

/// Copies the tar stream `open` reads into `w`, taking the edits made to `dir` (see
/// [`repack`]), and returns `w` with whether anything changed. Of entries unpacked to the same
/// place, only the last is kept, as only that one was extracted.
fn copy_tar<W: Write>(
    open: &dyn Fn() -> Result<Box<dyn Read>>,
    dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
    w: W,
) -> Result<(W, bool)> {
    let last = last_entries(open()?)?;
    let mut ar = tar::Archive::new(open()?);
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
    for (i, entry) in ar.entries()?.enumerate() {
        let mut entry = entry?;
        let mut h = entry.header().clone();
        let kind = h.entry_type();
        let pax = pax_records(&mut entry)?;
        let link = entry.link_name().ok().flatten().map(|l| l.into_owned());
        let stored = entry.path_bytes().into_owned();
        // Unpacked as a folder, as old tars wrote one.
        let old_dir = h.as_ustar().is_none() && stored.ends_with(b"/");
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
        if last.get(&rel) != Some(&i) {
            continue;
        }
        add_parents(&mut implied, &rel);
        seen.insert(rel.clone());
        let abs = dir.join(&rel);
        let keep = if apple_metadata(&rel) || !ours(&rel) {
            true
        } else if kind.is_dir() || old_dir {
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
/// signed package, whose signature a change would break, nor for a RAR archive, which can
/// only be read.
pub fn writable(path: &Path) -> bool {
    match kind(path) {
        None | Some(Kind::Rar) => false,
        Some(Kind::Zip) => {
            !SIGNED_EXTS.contains(&super::text::extension(path).as_str()) && !zip_signed(path)
        }
        Some(_) => true,
    }
}

/// Whether edits to an extraction of `path` reach a file of the user's with [`repack_all`]:
/// `path` is [`writable`], and so is every extracted archive it is nested in.
pub fn editable(path: &Path) -> bool {
    writable(path) && extraction_of(path).is_none_or(|(outer, _)| editable(&outer))
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
    let signed = ar.file_names().filter_map(Result::ok).any(|n| {
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

/// Finds the extraction recorded for `dir`: how its archive was then, which entries it wrote
/// (`None`, all of them, for a folder not extracted here), and the archive's password.
fn recorded(dir: &Path) -> (Option<Stamp>, Option<Written>, Option<Secret>) {
    match find_extraction(&mut extractions(), dir) {
        Some(e) => (e.stamp, Some(e.written.clone()), e.password.clone()),
        None => (None, None, None),
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

/// Whether [`repack_all`] of `path` may take long: it, or an archive it sits in an extraction
/// of, is compressed with xz or 7z's LZMA, which can take minutes for a large one.
pub fn slow_to_repack(path: &Path) -> bool {
    let mut path = path.to_path_buf();
    loop {
        if matches!(kind(&path), Some(Kind::TarXz | Kind::SevenZ)) {
            return true;
        }
        match extraction_of(&path) {
            Some((outer, _)) => path = outer,
            None => return false,
        }
    }
}

/// [`repack`]s `path` from `dir`, then each archive it sits in an extraction of, from the
/// inside out, so that an edit saved into an archive nested in another reaches the outermost
/// one. Returns the archives written. The outer ones are repacked even when `path` came out
/// the same, as an earlier save may have failed before reaching them.
pub fn repack_all(path: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let (mut path, mut dir) = (path.to_path_buf(), dir.to_path_buf());
    loop {
        if repack(&path, &dir)? {
            written.push(path.clone());
        }
        match extraction_of(&path) {
            Some((outer, outer_dir)) => (path, dir) = (outer, outer_dir),
            None => return Ok(written),
        }
    }
}

/// Writes the edits made to `dir`, an extraction of the archive at `path`, back into it.
/// Entries left alone keep their bytes, times and order (in a zip, their compressed data
/// too, unless encrypted with ZipCrypto or compressed with LZMA; in a 7z, the compressed
/// blocks holding only such entries), as do entries never extracted; changed files get the
/// current time; removed entries are left out, and new ones are added at the end. An
/// encrypted archive stays encrypted, with the password it was extracted with. Nothing is
/// written when nothing changed; returns whether the archive was written.
///
/// The archive is written to a temporary file beside it, which then takes its place keeping
/// its permissions (through a symbolic link, to the file it leads to), so a failure or a
/// crash leaves the original untouched. Refused for an archive [`writable`] turns down, a
/// read-only one, and one changed since it was extracted.
pub fn repack(path: &Path, dir: &Path) -> Result<bool> {
    use crate::i18n::{tr, trf};
    let file = || ("file", path.display().to_string());
    if kind(path) == Some(Kind::Rar) {
        anyhow::bail!(trf("msg.archive_rar_not_writable", &[file()]));
    }
    if !writable(path) {
        anyhow::bail!(trf("msg.archive_not_writable", &[file()]));
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let md = std::fs::metadata(&target).with_context(|| format!("open {}", path.display()))?;
    if md.permissions().readonly() {
        anyhow::bail!(trf("msg.archive_read_only", &[file()]));
    }
    let (then, written, password) = recorded(dir);
    if then.is_some() && stamp(&target) != then {
        anyhow::bail!(trf("msg.archive_changed", &[file()]));
    }
    let password = password.or_else(|| password_for(path));
    let ours = |rel: &Path| written.as_ref().is_none_or(|w| w.contains(rel));
    let staged = repack_into(&target, dir, &ours, password.as_ref()).map_err(|e| {
        if e.is::<NeedsPassword>() {
            return e;
        }
        anyhow::anyhow!("{}\n{e:#}", trf("msg.archive_write_failed", &[file()]))
    })?;
    let Some(tmp) = staged else {
        return Ok(false);
    };
    tmp.as_file().set_permissions(md.permissions())?;
    tmp.as_file().sync_all()?;
    // Writing it can take a while: checked again just before it takes the archive's place.
    if then.is_some() && stamp(&target) != then {
        anyhow::bail!(trf("msg.archive_changed", &[file()]));
    }
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
    password: Option<&Secret>,
) -> Result<Option<tempfile::NamedTempFile>> {
    let out_dir = parent_dir(path);
    let open = || -> Result<std::io::BufReader<std::fs::File>> {
        Ok(std::io::BufReader::new(std::fs::File::open(path)?))
    };
    let (tmp, changed) = match kind(path) {
        Some(Kind::Zip) => repack_zip(path, dir, out_dir, ours, password)?,
        Some(Kind::Tar) => copy_tar(
            &|| Ok(Box::new(open()?) as Box<dyn Read>),
            dir,
            ours,
            staging(out_dir)?,
        )?,
        Some(Kind::TarGz) => {
            let input =
                || Ok(Box::new(flate2::read::MultiGzDecoder::new(open()?)) as Box<dyn Read>);
            let enc = flate2::write::GzEncoder::new(staging(out_dir)?, Default::default());
            let (enc, changed) = copy_tar(&input, dir, ours, enc)?;
            (enc.finish().context("finish gzip")?, changed)
        }
        Some(Kind::TarBz2) => {
            let input = || Ok(Box::new(bzip2::read::MultiBzDecoder::new(open()?)) as Box<dyn Read>);
            let enc = bzip2::write::BzEncoder::new(staging(out_dir)?, Default::default());
            let (enc, changed) = copy_tar(&input, dir, ours, enc)?;
            (enc.finish().context("finish bzip2")?, changed)
        }
        Some(Kind::TarXz) => {
            // Unpacked to a file, as when it was extracted, and packed again into another,
            // so that the slow xz compression only runs when something changed.
            let mut tar = tempfile::tempfile()?;
            {
                let mut out = std::io::BufWriter::new(&mut tar);
                lzma_rs::xz_decompress(&mut open()?, &mut out)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                out.flush()?;
            }
            let input = || -> Result<Box<dyn Read>> {
                let mut f = tar.try_clone()?;
                f.rewind()?;
                Ok(Box::new(std::io::BufReader::new(f)))
            };
            let out = std::io::BufWriter::new(tempfile::tempfile()?);
            let (out, changed) = copy_tar(&input, dir, ours, out)?;
            if !changed {
                return Ok(None);
            }
            let mut plain = out.into_inner().map_err(|e| e.into_error())?;
            plain.rewind()?;
            let mut enc = XzWriter::new(staging(out_dir)?, XZ_PRESET)?;
            std::io::copy(&mut plain, &mut enc)?;
            (enc.finish().context("finish xz")?, true)
        }
        Some(Kind::TarZst) => {
            let input =
                || Ok(Box::new(zstd::stream::read::Decoder::new(open()?)?) as Box<dyn Read>);
            let enc = zstd::stream::write::Encoder::new(staging(out_dir)?, 0)?;
            let (enc, changed) = copy_tar(&input, dir, ours, enc)?;
            (enc.finish().context("finish zstd")?, changed)
        }
        Some(Kind::SevenZ) => return repack_7z(path, dir, out_dir, ours, password),
        Some(Kind::Rar) | None => anyhow::bail!("{}: cannot be written", path.display()),
    };
    Ok(changed.then_some(tmp))
}

/// How an entry of a zip is encrypted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ZipLock {
    Aes(zip::AesMode),
    /// The original zip encryption ("ZipCrypto"), weak but read by every unzip tool.
    Classic,
}

/// How entry `f` is encrypted, if it is.
fn zip_lock<R: Read>(f: &zip::read::ZipFile<'_, R>) -> Option<ZipLock> {
    use zip::HasZipMetadata;
    f.encrypted().then(|| {
        f.get_metadata()
            .aes_settings()
            .map_or(ZipLock::Classic, |(mode, _)| ZipLock::Aes(mode))
    })
}

/// `o`, encrypting with `password` as `lock` says, if it says to.
fn locked<'k>(
    o: zip::write::SimpleFileOptions,
    lock: Option<ZipLock>,
    password: Option<&'k Secret>,
) -> Result<zip::write::FileOptions<'k, 'static, ()>> {
    use zip::unstable::write::FileOptionsExt;
    let Some(lock) = lock else {
        return Ok(o);
    };
    let password = password.context("no password for an encrypted entry")?;
    Ok(match lock {
        ZipLock::Aes(mode) => o.with_aes_encryption(mode, password.as_str()),
        ZipLock::Classic => o.with_deprecated_encryption(password.as_bytes())?,
    })
}

/// The method a changed entry compressed with `method` is written with: the same, unless
/// it can only be read (Deflate64, LZMA and older ones), when it is compressed with Deflate.
fn rewritten_method(method: zip::CompressionMethod) -> zip::CompressionMethod {
    use zip::CompressionMethod as M;
    match method {
        M::Stored | M::Deflated | M::Bzip2 | M::Zstd | M::Xz | M::Ppmd => method,
        _ => M::Deflated,
    }
}

fn repack_zip(
    path: &Path,
    dir: &Path,
    out_dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
    password: Option<&Secret>,
) -> Result<(tempfile::NamedTempFile, bool)> {
    use zip::write::SimpleFileOptions;
    let mut ar = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path)?))?;
    // New files are encrypted as the archive's first encrypted entry is.
    let mut new_lock = None;
    for i in 0..ar.len() {
        new_lock = zip_lock(&ar.by_index_raw(i)?);
        if new_lock.is_some() {
            break;
        }
    }
    if new_lock.is_some() && password.is_none() {
        return Err(needs_password(path, None));
    }
    let mut tmp = staging(out_dir)?;
    let mut seen = HashSet::new();
    let mut implied = HashSet::new();
    let mut changed = false;
    let now = zip_now();
    {
        let mut w = zip::ZipWriter::new(std::io::BufWriter::new(tmp.as_file_mut()));
        w.set_raw_comment(ar.comment().into())?;
        for i in 0..ar.len() {
            let (name, rel, is_dir, method, mode, len, lock) = {
                let f = ar.by_index_raw(i)?;
                let rel = f.enclosed_name().map(|r| clean(&r));
                (
                    f.name().map(|n| n.into_owned()).unwrap_or_default(),
                    rel,
                    f.is_dir(),
                    f.compression(),
                    f.unix_mode(),
                    f.size(),
                    zip_lock(&f),
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
                    let same = {
                        let mut f = zip_entry(&mut ar, i, path, password)?;
                        same_file(&mut f, len, &abs)?
                    };
                    match same {
                        Some(true) => true,
                        Some(false) => {
                            changed = true;
                            let size = std::fs::metadata(&abs)?.len();
                            let mut o = SimpleFileOptions::default()
                                .compression_method(rewritten_method(method))
                                .last_modified_time(now)
                                .large_file(size > u32::MAX as u64);
                            if let Some(m) = mode {
                                o = o.unix_permissions(m & 0o777);
                            }
                            let o = locked(o, lock, password)?;
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
            let classic = lock == Some(ZipLock::Classic);
            if keep && (classic || method == zip::CompressionMethod::Lzma) && !is_dir {
                // A raw copy would drop the flag saying its data is encrypted, or for LZMA
                // that it ends with a marker, which other tools then refuse.
                let mut f = zip_entry(&mut ar, i, path, password)?;
                let o = f.options().compression_method(rewritten_method(method));
                w.start_file(name, locked(o, lock, password)?)?;
                std::io::copy(&mut f, &mut w)
                    .map_err(|e| zip_read_error(path, password, true, e))?;
            } else if keep {
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
                    let o = locked(o, new_lock, password)?;
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

/// What became of an entry of a 7z archive in its extracted copy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    Same,
    /// Its file now holds something else.
    Changed,
    /// Removed, or written over in the copy by a later entry of the same name.
    Gone,
}

/// The data of an entry packed into a 7z block: a file, opened only once it is read, so that
/// a block of many entries keeps one open at a time.
enum Source {
    Path(PathBuf),
    Open(std::fs::File),
    Done,
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Source::Path(path) = self {
            let f = std::fs::File::open(&*path)?;
            *self = Source::Open(f);
        }
        let Source::Open(f) = self else {
            return Ok(0);
        };
        let n = f.read(buf)?;
        if n == 0 && !buf.is_empty() {
            *self = Source::Done;
        }
        Ok(n)
    }
}

/// A 7z entry for the file or folder at `abs`, added as `name`, with its times and its
/// attributes: its Unix permissions too, above the Windows attributes, as 7-Zip stores them.
fn new_7z_entry(abs: &Path, name: String) -> sevenz_rust2::ArchiveEntry {
    const READ_ONLY: u32 = 0x1;
    const DIRECTORY: u32 = 0x10;
    const ARCHIVE: u32 = 0x20;
    let mut e = sevenz_rust2::ArchiveEntry::from_path(abs, name);
    if let Ok(md) = std::fs::metadata(abs) {
        let mut attrs = if md.is_dir() { DIRECTORY } else { ARCHIVE };
        if md.permissions().readonly() {
            attrs |= READ_ONLY;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            const UNIX_EXTENSION: u32 = 0x8000;
            attrs |= UNIX_EXTENSION | ((md.mode() & 0xFFFF) << 16);
        }
        e.windows_attributes = attrs;
        e.has_windows_attributes = true;
    }
    e
}

/// Writes the replacement for the 7z archive at `path` (see [`repack`]), or `None` when it
/// would be the same. A block whose entries are all as they were is copied without decoding
/// it; any other is compressed again from what is left of it, with LZMA2 unless it was
/// stored. Entries keep their times and attributes, but a changed file gets the current time.
fn repack_7z(
    path: &Path,
    dir: &Path,
    out_dir: &Path,
    ours: &dyn Fn(&Path) -> bool,
    secret: Option<&Secret>,
) -> Result<Option<tempfile::NamedTempFile>> {
    use sevenz_rust2::encoder_options::AesEncoderOptions;
    use sevenz_rust2::{
        Archive, ArchiveWriter, BlockDecoder, EncoderConfiguration, EncoderMethod, NtTime,
        Password, SourceReader,
    };
    let fail = |e: sevenz_rust2::Error| sevenz_error(path, secret, e);
    let write_fail = |e: sevenz_rust2::Error| anyhow::anyhow!("{}: {e}", path.display());
    let password = sevenz_password(secret);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    // Names that were encrypted stay so.
    let header_locked = matches!(
        Archive::read(
            &mut std::io::BufReader::new(std::fs::File::open(path)?),
            &Password::empty()
        ),
        Err(sevenz_rust2::Error::PasswordRequired)
    );
    let mut src = std::io::BufReader::new(std::fs::File::open(path)?);
    let ar = Archive::read(&mut src, &password).map_err(fail)?;
    let block_locked = |b: usize| {
        ar.blocks[b]
            .coders
            .iter()
            .any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
    };
    // New entries are encrypted when anything in the archive is.
    let any_locked = header_locked || (0..ar.blocks.len()).any(block_locked);
    if any_locked && secret.is_none() {
        return Err(needs_password(path, None));
    }
    let rels: Vec<PathBuf> = ar
        .files
        .iter()
        .map(|e| clean(Path::new(e.name())))
        .collect();
    let last: HashMap<&PathBuf, usize> = rels.iter().enumerate().map(|(i, r)| (r, i)).collect();
    let seen: HashSet<PathBuf> = rels.iter().cloned().collect();
    let mut implied = HashSet::new();
    for rel in &rels {
        add_parents(&mut implied, rel);
    }
    // Whether the extraction wrote the entry, so that it can be told from what is there now.
    let compared = |i: usize| !ar.files[i].is_anti_item() && ours(&rels[i]);
    let mut fate: Vec<Fate> = (0..ar.files.len())
        .map(|i| {
            let e = &ar.files[i];
            let abs = dir.join(&rels[i]);
            if !compared(i) {
                Fate::Same
            } else if last[&rels[i]] != i {
                Fate::Gone
            } else if e.is_directory() {
                if abs.is_dir() {
                    Fate::Same
                } else {
                    Fate::Gone
                }
            } else if e.has_stream() {
                // Told by its data, below.
                Fate::Same
            } else {
                match std::fs::symlink_metadata(&abs) {
                    Ok(md) if md.is_file() && md.len() == 0 => Fate::Same,
                    Ok(md) if md.is_file() => Fate::Changed,
                    _ => Fate::Gone,
                }
            }
        })
        .collect();

    // The entries with data in each block, in the order they were packed.
    let mut blocks: Vec<Vec<usize>> = vec![Vec::new(); ar.blocks.len()];
    for (i, b) in ar.stream_map.file_block_index.iter().enumerate() {
        if let Some(members) = b.and_then(|b| blocks.get_mut(b)) {
            if ar.files[i].has_stream() {
                members.push(i);
            }
        }
    }
    for (b, members) in blocks.iter().enumerate() {
        let decoder = BlockDecoder::new(threads, b, &ar, &password, &mut src);
        // `BlockDecoder` reads a block's entries as a run of the files, from its first one.
        let start = ar.stream_map.block_first_file_index[b];
        if decoder.entry_count() != members.len()
            || members.iter().enumerate().any(|(k, &i)| i != start + k)
        {
            anyhow::bail!(
                "{}: a block's entries are not stored together",
                path.display()
            );
        }
        if !members
            .iter()
            .any(|&i| compared(i) && fate[i] == Fate::Same)
        {
            continue;
        }
        let mut next = start;
        decoder
            .for_each_entries(&mut |e, r| {
                let i = next;
                next += 1;
                if compared(i) && fate[i] == Fate::Same {
                    let same = same_file(r, e.size(), &dir.join(&rels[i]))
                        .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
                    fate[i] = match same {
                        Some(true) => Fate::Same,
                        Some(false) => Fate::Changed,
                        None => Fate::Gone,
                    };
                } else {
                    std::io::copy(r, &mut std::io::sink())?;
                }
                Ok(true)
            })
            .map_err(fail)?;
    }

    // Whatever is now where a removed entry was (a folder where a file was, say) is added.
    let replaced: HashSet<PathBuf> = (0..ar.files.len())
        .filter(|&i| fate[i] == Fate::Gone && last[&rels[i]] == i)
        .filter(|&i| std::fs::symlink_metadata(dir.join(&rels[i])).is_ok())
        .map(|i| rels[i].clone())
        .collect();
    let added: Vec<Entry> = list_dir(dir, true)?
        .into_iter()
        .filter(|e| is_new(e, &seen, &implied) || replaced.contains(e.rel()))
        .collect();
    if added.is_empty() && fate.iter().all(|f| *f == Fate::Same) {
        return Ok(None);
    }

    let now = NtTime::now();
    let entry = |i: usize| {
        let mut e = ar.files[i].clone();
        if fate[i] == Fate::Changed {
            e.last_modified_date = now;
            e.has_last_modified_date = true;
        }
        e
    };
    let methods = |stored: bool, lock: bool| {
        let method = if stored {
            EncoderMethod::COPY
        } else {
            EncoderMethod::LZMA2
        };
        let mut methods = Vec::new();
        if lock {
            methods.push(AesEncoderOptions::new(password.clone()).into());
        }
        methods.push(EncoderConfiguration::new(method));
        methods
    };
    let mut tmp = staging(out_dir)?;
    {
        let mut w = ArchiveWriter::new(tmp.as_file_mut()).map_err(write_fail)?;
        w.set_encrypt_header(header_locked);
        // Entries without data, written after the blocks.
        let mut empty = Vec::new();
        // Files to pack together into a new block, with where to read them from.
        let mut fresh = Vec::new();
        for (b, members) in blocks.iter().enumerate() {
            if members.iter().all(|&i| fate[i] == Fate::Same) {
                w.push_raw_block(&ar, b, &mut src, |e| e.name().to_owned())
                    .map_err(write_fail)?;
                continue;
            }
            // An entry never extracted is not in the copy: it is taken from the block.
            let mut copies = HashMap::new();
            if members.iter().any(|&i| !compared(i)) {
                let mut next = ar.stream_map.block_first_file_index[b];
                BlockDecoder::new(threads, b, &ar, &password, &mut src)
                    .for_each_entries(&mut |_, r| {
                        let i = next;
                        next += 1;
                        if compared(i) {
                            std::io::copy(r, &mut std::io::sink())?;
                        } else {
                            let mut f = tempfile::tempfile()?;
                            std::io::copy(r, &mut f)?;
                            f.rewind()?;
                            copies.insert(i, f);
                        }
                        Ok(true)
                    })
                    .map_err(fail)?;
            }
            let mut entries = Vec::new();
            let mut sources = Vec::new();
            for &i in members {
                let source = match copies.remove(&i) {
                    Some(f) => Source::Open(f),
                    None if fate[i] == Fate::Gone => continue,
                    None => {
                        let abs = dir.join(&rels[i]);
                        if std::fs::metadata(&abs)?.len() == 0 {
                            empty.push(entry(i));
                            continue;
                        }
                        Source::Path(abs)
                    }
                };
                entries.push(entry(i));
                sources.push(SourceReader::new(source));
            }
            if !entries.is_empty() {
                let stored = ar.blocks[b].coders.iter().all(|c| {
                    let id = c.encoder_method_id();
                    id == EncoderMethod::ID_COPY || id == EncoderMethod::ID_AES256_SHA256
                });
                w.set_content_methods(methods(stored, block_locked(b)));
                w.push_archive_entries(entries, sources)
                    .map_err(write_fail)?;
            }
        }
        for (i, e) in ar.files.iter().enumerate() {
            match fate[i] {
                _ if e.has_stream() => {}
                Fate::Gone => {}
                Fate::Same => empty.push(entry(i)),
                // An empty file that was given contents.
                Fate::Changed => fresh.push((entry(i), dir.join(&rels[i]))),
            }
        }
        for e in &added {
            let abs = dir.join(e.rel());
            let new = new_7z_entry(&abs, zip_name(e.rel()));
            match e {
                Entry::File(_) if std::fs::metadata(&abs)?.len() > 0 => fresh.push((new, abs)),
                _ => empty.push(new),
            }
        }
        for e in empty {
            w.push_archive_entry::<&[u8]>(e, None).map_err(write_fail)?;
        }
        if !fresh.is_empty() {
            let (entries, sources): (Vec<_>, Vec<_>) = fresh
                .into_iter()
                .map(|(mut e, abs)| {
                    // Packing entries together does not mark them as having data itself.
                    e.has_stream = true;
                    (e, SourceReader::new(Source::Path(abs)))
                })
                .unzip();
            w.set_content_methods(methods(false, any_locked));
            w.push_archive_entries(entries, sources)
                .map_err(write_fail)?;
        }
        // The header is encrypted with the methods set last.
        if header_locked {
            w.set_content_methods(methods(false, true));
        }
        w.finish()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    }
    Ok(Some(tmp))
}

/// The preset `.tar.xz` files are written with: `xz`'s own default.
const XZ_PRESET: u32 = 6;
const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0];
/// Stream flags: a CRC32 check.
const XZ_FLAGS: [u8; 2] = [0, 1];
/// The size of the block header [`XzWriter`] writes.
const XZ_BLOCK_HEADER: u64 = 12;

/// Writes an xz file holding one block of LZMA2 data, checked with CRC32.
struct XzWriter<W: Write> {
    lzma2: lzma_rust2::Lzma2Writer<ByteCount<W>>,
    crc: flate2::Crc,
    len: u64,
}

/// Counts the bytes written through it.
struct ByteCount<W> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for ByteCount<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn crc32(data: &[u8]) -> [u8; 4] {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum().to_le_bytes()
}

/// Appends `n` to `out` as an xz variable-length integer.
fn xz_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

impl<W: Write> XzWriter<W> {
    fn new(mut inner: W, preset: u32) -> std::io::Result<Self> {
        let options = lzma_rust2::Lzma2Options::with_preset(preset);
        // The LZMA2 property: the smallest dictionary size it can name that is big enough.
        let dict = u64::from(options.lzma_options.dict_size);
        let dict_prop = (0..40u8)
            .find(|&p| (2 | u64::from(p & 1)) << (p / 2 + 11) >= dict)
            .unwrap_or(40);
        let mut head = XZ_MAGIC.to_vec();
        head.extend(XZ_FLAGS);
        head.extend(crc32(&XZ_FLAGS));
        // The block header: its size in fours less one, no flags, the LZMA2 filter with its
        // one property byte, padding to a multiple of four, and its CRC32.
        let block = [
            (XZ_BLOCK_HEADER / 4 - 1) as u8,
            0,
            0x21,
            1,
            dict_prop,
            0,
            0,
            0,
        ];
        head.extend(block);
        head.extend(crc32(&block));
        inner.write_all(&head)?;
        Ok(XzWriter {
            lzma2: lzma_rust2::Lzma2Writer::new(ByteCount { inner, written: 0 }, options),
            crc: flate2::Crc::new(),
            len: 0,
        })
    }

    fn finish(self) -> std::io::Result<W> {
        let ByteCount { mut inner, written } = self.lzma2.finish()?;
        // The block's padding to a multiple of four, and its check.
        let mut tail = vec![0; ((4 - written % 4) % 4) as usize];
        tail.extend(self.crc.sum().to_le_bytes());
        // The index of the one block: its size less the padding, and the size of its data.
        let mut index = vec![0];
        xz_varint(&mut index, 1);
        xz_varint(&mut index, XZ_BLOCK_HEADER + written + 4);
        xz_varint(&mut index, self.len);
        index.resize(index.len().next_multiple_of(4), 0);
        let index_crc = crc32(&index);
        // The footer: the size of the index in fours less one, and the stream flags again.
        let mut footer = ((index.len() as u32 + 4) / 4 - 1).to_le_bytes().to_vec();
        footer.extend(XZ_FLAGS);
        tail.extend(index);
        tail.extend(index_crc);
        tail.extend(crc32(&footer));
        tail.extend(footer);
        tail.extend(b"YZ");
        inner.write_all(&tail)?;
        Ok(inner)
    }
}

impl<W: Write> Write for XzWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.lzma2.write(buf)?;
        self.crc.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Not the encoder's own flush, which would end its chunk early.
        self.lzma2.inner_mut().flush()
    }
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

        let tzst_path = work.path().join("a.tar.zst");
        std::fs::write(&tzst_path, zstd::encode_all(&tar_bytes()[..], 0).unwrap()).unwrap();

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
            (&tzst_path, "dir/a.txt"),
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
        w.set_comment("kept").unwrap();
        w.finish().unwrap();
        let before = std::fs::read(&path).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        std::fs::write(extracted.path().join("a.css"), "p { color: red }").unwrap();
        std::fs::write(extracted.path().join("new.txt"), "new").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let mut ar = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(ar.len(), 4);
        assert_eq!(ar.comment(), b"kept");
        let entry = |ar: &mut zip::ZipArchive<std::fs::File>, i: usize| {
            let f = ar.by_index(i).unwrap();
            (
                f.name().unwrap().into_owned(),
                f.compression(),
                f.last_modified(),
            )
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

    /// Zip entries compressed with any method are read, and an edited one is written back
    /// with its method, or with Deflate when that method can only be read.
    #[test]
    fn repack_keeps_the_compression_method_of_every_zip_entry() {
        use zip::{write::SimpleFileOptions, CompressionMethod as M};
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.zip");
        let methods = [M::Stored, M::Deflated, M::Bzip2, M::Zstd, M::Xz, M::Ppmd];
        let name = |m: M| format!("{m:?}.txt");
        let text = |m: M| format!("{m:?} ").repeat(100);
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for m in methods {
            let o = SimpleFileOptions::default().compression_method(m);
            w.start_file(name(m), o).unwrap();
            w.write_all(text(m).as_bytes()).unwrap();
        }
        w.finish().unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        for m in methods {
            let file = extracted.path().join(name(m));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text(m));
            std::fs::write(&file, format!("{m:?} edited")).unwrap();
        }
        assert!(repack(&path, extracted.path()).unwrap());

        let mut ar = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        for m in methods {
            let mut f = ar.by_name(&name(m)).unwrap();
            assert_eq!(f.compression(), m);
            let mut got = String::new();
            f.read_to_string(&mut got).unwrap();
            assert_eq!(got, format!("{m:?} edited"));
        }
        assert_eq!(rewritten_method(M::Lzma), M::Deflated);
        assert_eq!(rewritten_method(M::Deflate64), M::Deflated);
    }

    /// Whether `e` asks for the password of `archive`, and if so whether one was tried.
    fn asks_password(e: anyhow::Error, archive: &Path) -> bool {
        let e = e
            .downcast::<NeedsPassword>()
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(e.archive, archive);
        e.wrong
    }

    /// A password-protected zip asks for its password, telling a wrong one apart, and is
    /// written back encrypted as it was: edited and new entries with the password it was
    /// opened with, untouched AES ones copied as they were.
    #[test]
    fn password_protected_zips_open_with_their_password_and_stay_encrypted() {
        use zip::{write::SimpleFileOptions, AesMode};
        type Zip = zip::ZipArchive<std::fs::File>;
        for lock in [
            ZipLock::Aes(AesMode::Aes256),
            ZipLock::Aes(AesMode::Aes128),
            ZipLock::Classic,
        ] {
            let work = tempfile::tempdir().unwrap();
            let path = work.path().join("secret.zip");
            let secret: Secret = Arc::new(Zeroizing::new("open sesame".to_string()));
            let o = SimpleFileOptions::default();
            let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for name in ["keep.txt", "edit.txt"] {
                w.start_file(name, locked(o, Some(lock), Some(&secret)).unwrap())
                    .unwrap();
                w.write_all(format!("{name} before").as_bytes()).unwrap();
            }
            w.start_file("plain.txt", o).unwrap();
            w.write_all(b"plain").unwrap();
            w.finish().unwrap();

            let err = extract_to_temp(&path).unwrap_err();
            assert!(!asks_password(err, &path), "{lock:?}");
            give_password(&path, Zeroizing::new("open says me".into()));
            let err = extract_to_temp(&path).unwrap_err();
            assert!(asks_password(err, &path), "{lock:?}");
            give_password(&path, Zeroizing::new(secret.as_str().into()));
            let extracted = extract_to_temp(&path).unwrap();
            let x = extracted.path();
            let read = |name: &str| std::fs::read_to_string(x.join(name)).unwrap();
            assert_eq!(read("keep.txt"), "keep.txt before");
            assert_eq!(read("plain.txt"), "plain");
            std::fs::write(x.join("edit.txt"), "edit.txt after").unwrap();
            std::fs::write(x.join("new.txt"), "new").unwrap();

            let open = || Zip::new(std::fs::File::open(&path).unwrap()).unwrap();
            let raw = |ar: &mut Zip, name: &str| {
                let mut f = ar.by_index_raw(ar.index_for_name(name).unwrap()).unwrap();
                let lock = zip_lock(&f);
                let mut bytes = Vec::new();
                f.read_to_end(&mut bytes).unwrap();
                (lock, bytes)
            };
            let kept = raw(&mut open(), "keep.txt");
            assert!(repack(&path, x).unwrap());

            let mut ar = open();
            if lock != ZipLock::Classic {
                assert_eq!(raw(&mut ar, "keep.txt"), kept, "{lock:?}");
            }
            assert_eq!(raw(&mut ar, "plain.txt").0, None);
            for (name, want) in [
                ("keep.txt", "keep.txt before"),
                ("edit.txt", "edit.txt after"),
                ("new.txt", "new"),
            ] {
                assert_eq!(raw(&mut ar, name).0, Some(lock), "{name}");
                let i = ar.index_for_name(name).unwrap();
                let mut got = String::new();
                ar.by_index_decrypt(i, secret.as_bytes())
                    .unwrap()
                    .read_to_string(&mut got)
                    .unwrap();
                assert_eq!(got, want, "{lock:?}");
            }
        }
    }

    /// An encrypted 7z, with its names encrypted too or not, asks for its password and is
    /// written back encrypted as it was, the block left alone copied as it was.
    #[test]
    fn password_protected_7z_opens_with_its_password_and_stays_encrypted() {
        use sevenz_rust2::encoder_options::AesEncoderOptions;
        use sevenz_rust2::{
            Archive, ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod,
        };
        let password = "open sesame";
        for names_locked in [false, true] {
            let work = tempfile::tempdir().unwrap();
            let path = work.path().join("secret.7z");
            let mut w = ArchiveWriter::create(&path).unwrap();
            w.set_content_methods(vec![
                AesEncoderOptions::new(password.into()).into(),
                EncoderConfiguration::new(EncoderMethod::LZMA2),
            ]);
            w.set_encrypt_header(names_locked);
            for name in ["keep.txt", "edit.txt"] {
                let text = format!("{name} before");
                w.push_archive_entry(ArchiveEntry::new_file(name), Some(text.as_bytes()))
                    .unwrap();
            }
            w.finish().unwrap();
            let read =
                |pw: &str| Archive::read(&mut std::fs::File::open(&path).unwrap(), &pw.into());
            let kept_block = |ar: &Archive| {
                let i = ar
                    .files
                    .iter()
                    .position(|e| e.name() == "keep.txt")
                    .unwrap();
                let b = ar.stream_map.file_block_index[i].unwrap();
                // The coders hold the AES salt and IV, new each time data is encrypted.
                (ar.blocks[b].coders.clone(), ar.blocks[b].crc)
            };
            let kept = kept_block(&read(password).unwrap());

            let err = extract_to_temp(&path).unwrap_err();
            assert!(!asks_password(err, &path), "{names_locked}");
            give_password(&path, Zeroizing::new("open says me".into()));
            let err = extract_to_temp(&path).unwrap_err();
            assert!(asks_password(err, &path), "{names_locked}");
            give_password(&path, Zeroizing::new(password.into()));
            let extracted = extract_to_temp(&path).unwrap();
            let x = extracted.path();
            assert_eq!(
                std::fs::read_to_string(x.join("keep.txt")).unwrap(),
                "keep.txt before"
            );
            std::fs::write(x.join("edit.txt"), "edit.txt after").unwrap();
            std::fs::write(x.join("new.txt"), "new").unwrap();
            assert!(repack(&path, x).unwrap());

            match read("") {
                Err(sevenz_rust2::Error::PasswordRequired) => assert!(names_locked),
                Ok(_) => assert!(!names_locked),
                Err(e) => panic!("{e}"),
            }
            let ar = read(password).unwrap();
            assert_eq!(ar.blocks.len(), 3);
            for b in &ar.blocks {
                let ids: Vec<_> = b.coders.iter().map(|c| c.encoder_method_id()).collect();
                assert!(
                    ids.contains(&EncoderMethod::ID_AES256_SHA256),
                    "{names_locked}"
                );
            }
            assert_eq!(kept_block(&ar), kept);
            let after = extract_to_temp(&path).unwrap();
            let read = |name: &str| std::fs::read_to_string(after.path().join(name)).unwrap();
            assert_eq!(read("keep.txt"), "keep.txt before");
            assert_eq!(read("edit.txt"), "edit.txt after");
            assert_eq!(read("new.txt"), "new");
        }
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

    /// A RAR archive kept for the tests: `v4` ones are in the format of RAR 4 and older,
    /// `v5` ones in the one of RAR 5 and later (see `tests/fixtures/rar/README.md`).
    fn rar_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
        let work = tempfile::tempdir().unwrap();
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rar")
            .join(name);
        let path = work.path().join(name);
        std::fs::copy(from, &path).unwrap();
        (work, path)
    }

    /// The tree every `v5` fixture holds, compressed: files, a folder with a file and an
    /// empty folder.
    fn assert_rar_tree(dir: &Path) {
        let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap();
        assert_eq!(read("dir/a.txt"), "hello\n");
        assert_eq!(read("keep.txt"), "keep\n");
        let big = read("big.txt");
        assert_eq!(big.lines().count(), 300);
        assert!(big.ends_with("line 300 of a file long enough to be compressed\n"));
        assert!(dir.join("sub").is_dir());
    }

    /// A RAR archive opens as a folder, in the format of RAR 5 (solid or not) and in the
    /// older one.
    #[test]
    fn rar_archives_are_extracted() {
        for name in ["v5.rar", "v5-solid.rar"] {
            let (_work, path) = rar_fixture(name);
            assert!(is_archive(&path), "{name}");
            let extracted = extract_to_temp(&path).unwrap();
            assert_rar_tree(extracted.path());
        }
        let (_work, path) = rar_fixture("v4.rar");
        assert!(is_archive(&path));
        let extracted = extract_to_temp(&path).unwrap();
        let version = extracted.path().join("VERSION");
        assert_eq!(std::fs::read_to_string(&version).unwrap(), "unrar-0.4.0");
        // A file keeps the time the archive has for it: this one is from 2015.
        let modified = std::fs::metadata(&version).unwrap().modified().unwrap();
        let year_2020 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
        assert!(modified < year_2020, "{modified:?}");
        // The extension is what makes it one, in any case.
        let upper = path.with_file_name("V4.RAR");
        std::fs::rename(&path, &upper).unwrap();
        assert!(is_archive(&upper));
        assert!(extract_to_temp(&upper).is_ok());
    }

    /// An encrypted RAR archive, with its names encrypted too or not, asks for its password
    /// and opens with it, in both formats.
    #[test]
    fn password_protected_rar_opens_with_its_password() {
        let needs = |path: &Path| {
            let e = extract_to_temp(path).unwrap_err();
            let e = e.downcast_ref::<NeedsPassword>().map(|n| n.wrong);
            e.unwrap_or_else(|| panic!("{}: not a password error", path.display()))
        };
        for name in ["v5-password.rar", "v5-names.rar"] {
            let (_work, path) = rar_fixture(name);
            assert!(!needs(&path), "{name}: none given yet");
            give_password(&path, Zeroizing::new("wrong".into()));
            assert!(needs(&path), "{name}: a wrong one");
            give_password(&path, Zeroizing::new("secret".into()));
            let extracted = extract_to_temp(&path).unwrap();
            assert_rar_tree(extracted.path());
            forget_password(&path);
        }
        // The older format can't tell a wrong password: it shows in the data.
        let (_work, path) = rar_fixture("v4-password.rar");
        assert!(!needs(&path));
        give_password(&path, Zeroizing::new("wrong".into()));
        assert!(needs(&path));
        give_password(&path, Zeroizing::new("unrar".into()));
        let extracted = extract_to_temp(&path).unwrap();
        let read = std::fs::read_to_string(extracted.path().join(".gitignore")).unwrap();
        assert_eq!(read, "target\nCargo.lock\n");
        forget_password(&path);
        // A password need not be ASCII.
        let (_work, path) = rar_fixture("v5-unicode-password.rar");
        assert!(!needs(&path));
        give_password(&path, Zeroizing::new("pass".into()));
        assert!(needs(&path));
        give_password(&path, Zeroizing::new("p\u{e4}ss\u{5bc6}".into()));
        let extracted = extract_to_temp(&path).unwrap();
        let read = std::fs::read_to_string(extracted.path().join("keep.txt")).unwrap();
        assert_eq!(read, "keep\n");
        forget_password(&path);
    }

    /// A file stored once for several entries with the same contents comes out as each of
    /// them; a link to a folder is left out, so that nothing is ever written through one.
    #[test]
    fn rar_copies_are_extracted_and_links_are_not() {
        let (_work, path) = rar_fixture("v5-copies.rar");
        let extracted = extract_to_temp(&path).unwrap();
        let read = |rel: &str| std::fs::read_to_string(extracted.path().join(rel)).unwrap();
        assert_eq!(read("one.txt").lines().count(), 4000);
        assert_eq!(read("one.txt"), read("two.txt"));
        assert_eq!(read("other.txt"), "other\n");

        let (_work, path) = rar_fixture("v5-link.rar");
        let extracted = extract_to_temp(&path).unwrap();
        let real = extracted.path().join("real/a.txt");
        assert_eq!(std::fs::read_to_string(real).unwrap(), "hello\n");
        assert!(std::fs::symlink_metadata(extracted.path().join("link")).is_err());
    }

    /// A RAR archive is only read: it is not offered for editing, and writing an edited
    /// copy back is refused, leaving the archive as it was.
    #[test]
    fn rar_archives_are_not_written_back() {
        let (_work, path) = rar_fixture("v5.rar");
        let before = std::fs::read(&path).unwrap();
        assert!(!writable(&path));
        assert!(!editable(&path));
        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("keep.txt"), "changed\n").unwrap();
        let e = super::repack(&path, extracted.path()).unwrap_err();
        let key = "msg.archive_rar_not_writable";
        let file = ("file", path.display().to_string());
        assert_eq!(e.to_string(), crate::i18n::trf(key, &[file]));
        assert!(super::repack_all(&path, extracted.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// A volume of a RAR archive in several is turned down, whatever it is named, before
    /// anything of it is read.
    #[test]
    fn rar_volumes_are_refused() {
        let (_work, path) = rar_fixture("volume.part1.rar");
        let renamed = path.with_file_name("plain.rar");
        std::fs::copy(&path, &renamed).unwrap();
        for volume in [&path, &renamed] {
            let e = extract_to_temp(volume).unwrap_err();
            assert!(e.to_string().contains("several volumes"), "{e:#}");
        }
        // So is one whose names are encrypted, once its password is given.
        let (_work, path) = rar_fixture("volume-names.part1.rar");
        let renamed = path.with_file_name("plain.rar");
        std::fs::copy(&path, &renamed).unwrap();
        let e = extract_to_temp(&renamed).unwrap_err();
        assert!(e.is::<NeedsPassword>(), "{e:#}");
        give_password(&renamed, Zeroizing::new("secret".into()));
        let e = extract_to_temp(&renamed).unwrap_err();
        assert!(e.to_string().contains("several volumes"), "{e:#}");
        forget_password(&renamed);
    }

    /// A file that is not a RAR archive, or a cut-off one, fails without a password prompt;
    /// the limits on what an archive unpacks to hold for a RAR too.
    #[test]
    fn broken_and_oversized_rar_archives_fail() {
        let (work, path) = rar_fixture("v5.rar");
        let bytes = std::fs::read(&path).unwrap();
        let not_rar = work.path().join("text.rar");
        std::fs::write(&not_rar, "not an archive\n").unwrap();
        let cut = work.path().join("cut.rar");
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        for broken in [&not_rar, &cut] {
            let e = extract_to_temp(broken).unwrap_err();
            assert!(!e.is::<NeedsPassword>(), "{e:#}");
            // Nor with a password kept for it: the damage is not taken for a wrong one.
            give_password(broken, Zeroizing::new("secret".into()));
            let e = extract_to_temp(broken).unwrap_err();
            assert!(!e.is::<NeedsPassword>(), "{e:#}");
            forget_password(broken);
        }
        let small = Limits {
            bytes: 1000,
            entries: 100,
        };
        let e = extract_with(&path, small).unwrap_err();
        assert!(e.to_string().contains("unpacks to more than"), "{e:#}");
        let few = Limits {
            bytes: 1 << 20,
            entries: 2,
        };
        let e = extract_with(&path, few).unwrap_err();
        assert!(e.to_string().contains("unpacks to more than"), "{e:#}");
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

    /// A 7z block whose entries were left alone is copied as it was, compression and all;
    /// any other is packed again from what is left of it. Entries keep their times and
    /// attributes, but a changed file gets the current time.
    #[test]
    fn repack_copies_untouched_7z_blocks_as_they_were() {
        use sevenz_rust2::{
            Archive, ArchiveEntry, ArchiveWriter, EncoderMethod, NtTime, SourceReader,
        };
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.7z");
        let old = NtTime::from(130_000_000_000_000_000u64);
        let attrs = 0x8000 | (0o100640 << 16);
        let entry = |name: &str| {
            let mut e = ArchiveEntry::new_file(name);
            e.last_modified_date = old;
            e.has_last_modified_date = true;
            e.windows_attributes = attrs;
            e.has_windows_attributes = true;
            e
        };
        let mut w = ArchiveWriter::create(&path).unwrap();
        w.set_content_methods(vec![EncoderMethod::DEFLATE.into()]);
        w.push_archive_entry(entry("keep.txt"), Some(&b"keep\n"[..]))
            .unwrap();
        w.push_archive_entry(entry("edit.txt"), Some(&b"old\n"[..]))
            .unwrap();
        let solid = ["s/a.txt", "s/b.txt", "s/c.txt"];
        w.push_archive_entries(
            solid.iter().map(|n| entry(n)).collect(),
            solid
                .iter()
                .map(|n| SourceReader::new(n.as_bytes()))
                .collect(),
        )
        .unwrap();
        w.push_archive_entry::<&[u8]>(entry("empty.txt"), None)
            .unwrap();
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("d"), None)
            .unwrap();
        w.finish().unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        let x = extracted.path();
        assert!(!repack(&path, x).unwrap());
        std::fs::write(x.join("edit.txt"), "new\n").unwrap();
        std::fs::remove_file(x.join("s/b.txt")).unwrap();
        std::fs::write(x.join("empty.txt"), "grown\n").unwrap();
        std::fs::write(x.join("added.txt"), "added\n").unwrap();
        assert!(repack(&path, x).unwrap());

        let ar = Archive::open(&path).unwrap();
        let find = |name: &str| {
            let i = ar.files.iter().position(|e| e.name() == name);
            i.unwrap_or_else(|| panic!("{name} is missing"))
        };
        let method = |name: &str| {
            let b = ar.stream_map.file_block_index[find(name)].unwrap();
            ar.blocks[b].coders[0].encoder_method_id().to_vec()
        };
        let time = |name: &str| u64::from(ar.files[find(name)].last_modified_date());
        assert_eq!(method("keep.txt"), EncoderMethod::ID_DEFLATE);
        assert_eq!(method("edit.txt"), EncoderMethod::ID_LZMA2);
        assert_eq!(method("s/a.txt"), EncoderMethod::ID_LZMA2);
        assert_eq!(
            ar.stream_map.file_block_index[find("s/a.txt")],
            ar.stream_map.file_block_index[find("s/c.txt")]
        );
        assert!(!ar.files.iter().any(|e| e.name() == "s/b.txt"));
        assert_eq!(time("keep.txt"), u64::from(old));
        assert_eq!(time("s/a.txt"), u64::from(old));
        assert_ne!(time("edit.txt"), u64::from(old));
        assert_ne!(time("empty.txt"), u64::from(old));
        for name in ["keep.txt", "edit.txt", "s/c.txt", "empty.txt"] {
            assert_eq!(ar.files[find(name)].windows_attributes(), attrs, "{name}");
        }
        assert!(ar.files[find("d")].is_directory());
        find("added.txt");

        let after = extract_to_temp(&path).unwrap();
        let read = |rel: &str| std::fs::read_to_string(after.path().join(rel)).unwrap();
        assert_eq!(read("keep.txt"), "keep\n");
        assert_eq!(read("edit.txt"), "new\n");
        assert_eq!(read("s/a.txt"), "s/a.txt");
        assert_eq!(read("s/c.txt"), "s/c.txt");
        assert_eq!(read("empty.txt"), "grown\n");
        assert_eq!(read("added.txt"), "added\n");
        assert!(!after.path().join("s/b.txt").exists());
    }

    /// A `.tar.xz` is written back compressed, as one xz stream another decoder reads.
    #[test]
    fn repack_writes_a_compressed_tar_xz() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar.xz");
        let text = "the same line again\n".repeat(20_000);
        let mut b = tar::Builder::new(Vec::new());
        for name in ["one.txt", "two.txt"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(text.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, text.as_bytes()).unwrap();
        }
        let tar = b.into_inner().unwrap();
        // lzma-rs writes the data as it is, uncompressed.
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar.as_slice(), &mut xz).unwrap();
        std::fs::write(&path, &xz).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!repack(&path, extracted.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), xz);
        std::fs::write(extracted.path().join("two.txt"), "edited\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        let written = std::fs::read(&path).unwrap();
        assert!(written.len() < tar.len() / 50, "{} bytes", written.len());
        let mut out = Vec::new();
        lzma_rs::xz_decompress(&mut written.as_slice(), &mut out).unwrap();
        let mut got = Vec::new();
        for e in tar::Archive::new(out.as_slice()).entries().unwrap() {
            let mut e = e.unwrap();
            let mut s = String::new();
            e.read_to_string(&mut s).unwrap();
            got.push((e.path().unwrap().display().to_string(), s));
        }
        assert_eq!(
            got,
            [
                ("one.txt".to_string(), text.clone()),
                ("two.txt".to_string(), "edited\n".to_string())
            ]
        );
    }

    /// Saving an archive nested in an extracted one writes the outer archive too; one nested
    /// in an archive that can't be written is not editable.
    #[test]
    fn repack_all_writes_the_archives_a_nested_one_is_in() {
        let work = tempfile::tempdir().unwrap();
        let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        inner
            .start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        inner.write_all(b"old\n").unwrap();
        let inner = inner.finish().unwrap().into_inner();
        let outer = work.path().join("outer.tar.gz");
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_size(inner.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "inner.zip", inner.as_slice())
            .unwrap();
        std::fs::write(&outer, b.into_inner().unwrap().finish().unwrap()).unwrap();

        let outer_copy = extract_to_temp(&outer).unwrap();
        let nested = outer_copy.path().join("inner.zip");
        assert!(editable(&nested));
        let inner_copy = extract_to_temp(&nested).unwrap();
        std::fs::write(inner_copy.path().join("a.txt"), "new\n").unwrap();
        assert_eq!(
            repack_all(&nested, inner_copy.path()).unwrap(),
            [nested.clone(), outer.clone()]
        );
        assert!(repack_all(&nested, inner_copy.path()).unwrap().is_empty());
        let outer_after = extract_to_temp(&outer).unwrap();
        let inner_after = extract_to_temp(&outer_after.path().join("inner.zip")).unwrap();
        assert_eq!(
            std::fs::read_to_string(inner_after.path().join("a.txt")).unwrap(),
            "new\n"
        );

        let apk = work.path().join("app.apk");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&apk).unwrap());
        zw.start_file("inner.zip", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(&inner).unwrap();
        zw.finish().unwrap();
        let apk_copy = extract_to_temp(&apk).unwrap();
        let nested = apk_copy.path().join("inner.zip");
        assert!(writable(&nested));
        assert!(!editable(&nested));
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
        // Copied unchanged, it comes out byte for byte as it was.
        let open = || Ok(Box::new(std::fs::File::open(&path)?) as Box<dyn Read>);
        let (copy, changed) = copy_tar(&open, extracted.path(), &|_| true, Vec::new()).unwrap();
        assert!(!changed);
        assert_eq!(copy, before);

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

    /// The header of a GNU sparse file `name` holding 3 bytes of data at `offset`, after a hole.
    fn sparse_header(name: &str, offset: u64) -> tar::Header {
        fn octal(field: &mut [u8], value: u64) {
            let digits = format!("{value:0w$o}", w = field.len() - 1);
            field[..digits.len()].copy_from_slice(digits.as_bytes());
        }
        let mut h = named_header(name, tar::EntryType::GNUSparse, 3);
        let gnu = h.as_gnu_mut().unwrap();
        octal(&mut gnu.sparse[0].offset, offset);
        octal(&mut gnu.sparse[0].numbytes, 3);
        octal(&mut gnu.realsize, offset + 3);
        h.set_cksum();
        h
    }

    /// A GNU sparse file is written out whole (its holes as zeros) rather than with a header
    /// that no longer fits its data, which would leave every entry after it unreadable.
    #[test]
    fn repack_keeps_a_tar_readable_past_a_sparse_file() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let h = sparse_header("hole.bin", 1024);
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

    /// The holes of a sparse file count against the limit on what an archive unpacks to, as
    /// they are written out.
    #[test]
    fn a_sparse_file_counts_whole_against_the_limit() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        b.append(&sparse_header("hole.bin", 2 << 20), &b"abc"[..])
            .unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();
        let limits = |bytes| Limits {
            bytes,
            entries: 100,
        };
        let err = format!("{:#}", extract_with(&path, limits(1 << 20)).unwrap_err());
        assert!(err.contains("unpacks to more than"), "{err}");
        let roomy = extract_with(&path, limits(4 << 20)).unwrap();
        let len = std::fs::metadata(roomy.path().join("hole.bin"))
            .unwrap()
            .len();
        assert_eq!(len, (2 << 20) + 3);
    }

    /// `tar` unpacks an entry over an earlier one of the same name, so only the last one is
    /// compared with the extracted copy and kept; earlier ones would bring an old version back.
    #[test]
    fn repack_keeps_only_the_last_tar_entry_of_a_name() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in [("a.txt", "v1\n"), ("b.txt", "b\n"), ("./a.txt", "v2\n")] {
            let h = named_header(name, tar::EntryType::Regular, data.len() as u64);
            b.append(&h, data.as_bytes()).unwrap();
        }
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();
        let read = |dir: &Path, name: &str| std::fs::read_to_string(dir.join(name)).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert_eq!(read(extracted.path(), "a.txt"), "v2\n");
        assert!(!repack(&path, extracted.path()).unwrap());
        std::fs::write(extracted.path().join("b.txt"), "B\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());
        assert_eq!(tar_names(&path), ["b.txt", "./a.txt"]);
        let after = extract_to_temp(&path).unwrap();
        assert_eq!(read(after.path(), "a.txt"), "v2\n");

        std::fs::write(after.path().join("a.txt"), "new\n").unwrap();
        assert!(repack(&path, after.path()).unwrap());
        let again = extract_to_temp(&path).unwrap();
        assert_eq!(read(again.path(), "a.txt"), "new\n");
        assert_eq!(read(again.path(), "b.txt"), "B\n");
    }

    /// An old tar marks a folder only by the `/` ending its name, as `tar` unpacks it.
    #[test]
    fn repack_leaves_a_folder_of_an_old_tar_alone() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        for (name, data, mode) in [("d/", "", 0o755), ("d/x.txt", "x\n", 0o644)] {
            let mut h = tar::Header::new_old();
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            b.append(&h, data.as_bytes()).unwrap();
        }
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(extracted.path().join("d").is_dir());
        assert!(!repack(&path, extracted.path()).unwrap());
        std::fs::write(extracted.path().join("d/x.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());
        assert_eq!(tar_names(&path), ["d/", "d/x.txt"]);
    }

    /// A link whose header gave it a size, written again by name, is written with none: it
    /// holds no data, and a size would leave every entry after it unreadable.
    #[cfg(unix)]
    #[test]
    fn repack_writes_a_tar_link_without_data() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("a.tar");
        let mut b = tar::Builder::new(Vec::new());
        let file = named_header("a.txt", tar::EntryType::Regular, 2);
        b.append(&file, &b"a\n"[..]).unwrap();
        let mut link = named_header("link", tar::EntryType::Symlink, 4);
        link.set_link_name("a.txt").unwrap();
        link.set_cksum();
        b.append(&link, &b"junk"[..]).unwrap();
        let edit = named_header("edit.txt", tar::EntryType::Regular, 4);
        b.append(&edit, &b"old\n"[..]).unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        std::fs::write(extracted.path().join("edit.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());
        let after = extract_to_temp(&path).unwrap();
        assert_eq!(
            std::fs::read_link(after.path().join("link")).unwrap(),
            PathBuf::from("a.txt")
        );
        assert_eq!(
            std::fs::read_to_string(after.path().join("edit.txt")).unwrap(),
            "new\n"
        );
    }

    /// The sparse map of a pax sparse file describes the data it was stored with: dropped,
    /// with its time, once that data changed.
    #[test]
    fn pax_records_of_a_changed_entry_drop_what_no_longer_holds() {
        let record = |k: &str| (k.as_bytes().to_vec(), b"1".to_vec());
        let pax = vec![
            record("GNU.sparse.map"),
            record("SCHILY.realsize"),
            record("mtime"),
            record("SCHILY.xattr.user.k"),
            record("size"),
        ];
        assert_eq!(rewritten(&pax, true), vec![record("SCHILY.xattr.user.k")]);
        assert_eq!(rewritten(&pax, false), pax[..4].to_vec());
    }

    /// Entries left alone keep their pax records (extended attributes, say), and one stored
    /// from the root or going up with `..`, never extracted, is kept rather than failing the
    /// whole repack.
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
            &named_header("../up.txt", tar::EntryType::Regular, 3),
            &b"up\n"[..],
        )
        .unwrap();
        b.append(
            &named_header("edit.txt", tar::EntryType::Regular, 4),
            &b"old\n"[..],
        )
        .unwrap();
        std::fs::write(&path, b.into_inner().unwrap()).unwrap();

        let extracted = extract_to_temp(&path).unwrap();
        assert!(!work.path().join("up.txt").exists());
        std::fs::write(extracted.path().join("edit.txt"), "new\n").unwrap();
        assert!(repack(&path, extracted.path()).unwrap());

        assert_eq!(
            tar_names(&path),
            ["keep.txt", "/abs.txt", "../up.txt", "edit.txt"]
        );
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
