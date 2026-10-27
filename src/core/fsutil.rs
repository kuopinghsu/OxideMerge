//! Saving files without leaving them half-written.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static BACKUP: AtomicBool = AtomicBool::new(false);

/// Whether saving over an existing file first keeps its previous contents as `<name>.bak`.
pub fn set_backup(on: bool) {
    BACKUP.store(on, Ordering::Relaxed);
}

pub fn backup_on() -> bool {
    BACKUP.load(Ordering::Relaxed)
}

fn writing(path: &Path) -> String {
    crate::i18n::trf("msg.cannot_write", &[("file", path.display().to_string())])
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Removes an earlier `<name>.bak` of `path`, which may be read-only like the file it copied.
fn clear_backup(path: &Path) -> Result<PathBuf> {
    let bak = backup_path(path);
    if let Ok(md) = std::fs::symlink_metadata(&bak) {
        let mut perm = md.permissions();
        if md.is_file() && perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&bak, perm);
        }
        if !md.is_dir() {
            std::fs::remove_file(&bak).with_context(|| writing(&bak))?;
        }
    }
    Ok(bak)
}

/// Copies `path` to `<name>.bak`, replacing an earlier one.
fn keep_backup(path: &Path) -> Result<()> {
    let bak = clear_backup(path)?;
    std::fs::copy(path, &bak).with_context(|| writing(&bak))?;
    Ok(())
}

/// A device, pipe or terminal (`/dev/stdout`), written straight to, as it can't be replaced.
fn is_special(path: &Path) -> bool {
    cfg!(unix) && path.starts_with("/dev/")
        || std::fs::metadata(path).is_ok_and(|m| !m.is_file() && !m.is_dir())
}

/// Writes `bytes` to a temporary file next to `path` and renames it over `path`, so a crash or
/// a full disk leaves either the old or the new contents. A symbolic link is followed, and the
/// file keeps its permissions.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_with(path, bytes, BACKUP.load(Ordering::Relaxed))
}

pub fn write_atomic_with(path: &Path, bytes: &[u8], backup: bool) -> Result<()> {
    if is_special(path) {
        return std::fs::write(path, bytes).with_context(|| writing(path));
    }
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let mut old = std::fs::metadata(&target).ok();
    if old.is_some() && backup {
        keep_backup(&target)?;
    }
    let mut placeholder = false;
    if old.is_none() {
        // The temporary file is private; a new file gets the permissions files are made with.
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .is_ok()
        {
            placeholder = true;
            old = std::fs::metadata(&target).ok();
        }
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let written = write_staged(&target, dir, bytes, old.as_ref());
    if written.is_err() && placeholder {
        let _ = std::fs::remove_file(&target);
    }
    written.with_context(|| writing(path))
}

/// Writes `bytes` to a temporary file in `dir`, then renames it over `target`. Writing straight
/// into `target` is left for when the rename route is impossible, never for a write that failed
/// (a full disk would then empty the file).
fn write_staged(
    target: &Path,
    dir: &Path,
    bytes: &[u8],
    old: Option<&std::fs::Metadata>,
) -> std::io::Result<()> {
    let mut tmp = match tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(dir)
    {
        Ok(tmp) => tmp,
        // A folder that may not hold new files; a full disk or quota would empty the file.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return std::fs::write(target, bytes)
        }
        Err(e) => return Err(e),
    };
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Some(m) = old {
        let _ = tmp.as_file().set_permissions(m.permissions());
    }
    match tmp.persist(target) {
        Ok(_) => Ok(()),
        // A file that cannot be replaced; the temporary file goes first, freeing its space.
        Err(e) => {
            drop(e);
            std::fs::write(target, bytes)
        }
    }
}

/// Copies the file `from` over `to` as [`write_atomic`] writes, so a crash or a full disk
/// leaves `to` whole. `to` gets the permissions and modification time of `from`; with
/// `backup`, the file it replaces is kept as `<name>.bak`. `to` itself is replaced, not a file a
/// symbolic link there points to.
pub fn copy_atomic(from: &Path, to: &Path, backup: bool) -> Result<()> {
    let copying = || format!("copy {} -> {}", from.display(), to.display());
    if backup && to.is_file() {
        keep_backup(to)?;
    }
    let dir = match to.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir).with_context(copying)?;
    let mut src = std::fs::File::open(from).with_context(copying)?;
    let md = src.metadata().with_context(copying)?;
    let (perm, modified) = (md.permissions(), md.modified().ok());
    let mut tmp = match tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(dir)
    {
        Ok(tmp) => tmp,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return copy_in_place(from, to).with_context(copying);
        }
        Err(e) => return Err(e).with_context(copying),
    };
    std::io::copy(&mut src, tmp.as_file_mut()).with_context(copying)?;
    if let Some(t) = modified {
        let _ = tmp.as_file().set_modified(t);
    }
    tmp.as_file().sync_all().with_context(copying)?;
    // Set once in place: a read-only temporary file could not be removed on Windows.
    match tmp.persist(to) {
        Ok(_) => {
            let _ = std::fs::set_permissions(to, perm);
            Ok(())
        }
        // A file that cannot be replaced (read-only on Windows); the temporary file goes first.
        Err(e) => {
            drop(e);
            copy_in_place(from, to).with_context(copying)
        }
    }
}

/// [`std::fs::copy`] over a file that may be read-only, or over a symbolic link.
fn copy_in_place(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(to) {
        if md.file_type().is_symlink() {
            std::fs::remove_file(to)?;
        }
        let mut perm = md.permissions();
        if md.is_file() && perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(to, perm);
        }
    }
    std::fs::copy(from, to).map(|_| ())
}

/// Deletes a file, or with backups on, renames it to `<name>.bak`.
pub fn remove(path: &Path) -> Result<()> {
    remove_with(path, BACKUP.load(Ordering::Relaxed))
}

fn remove_with(path: &Path, backup: bool) -> Result<()> {
    if backup {
        let bak = clear_backup(path)?;
        std::fs::rename(path, &bak).with_context(|| writing(&bak))
    } else {
        std::fs::remove_file(path).with_context(|| {
            crate::i18n::trf("msg.cannot_delete", &[("file", path.display().to_string())])
        })
    }
}

/// Deletes `dir` and the folders holding it while they are empty, up to `root` (kept).
pub fn remove_empty_dirs(dir: &Path, root: &Path) {
    let mut dir = dir;
    while dir != root && dir.starts_with(root) && std::fs::remove_dir(dir).is_ok() {
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_contents_keeps_permissions_and_follows_links() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        write_atomic(&file, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640);
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            write_atomic(&link, b"via link").unwrap();
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "via link");
        }
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert!(names
            .iter()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".oxidemerge-")));

        write_atomic(&dir.path().join("new.txt"), b"fresh").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "fresh"
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_files_are_not_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new.txt");
        write_atomic(&file, b"x").unwrap();
        let usual = {
            let probe = dir.path().join("probe");
            std::fs::write(&probe, "").unwrap();
            std::fs::metadata(probe).unwrap().permissions().mode() & 0o777
        };
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, usual);
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
    }

    #[test]
    fn keeps_a_backup_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.txt");
        std::fs::write(&file, "first").unwrap();
        write_atomic_with(&file, b"second", true).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            "first"
        );
        // a read-only file leaves a read-only backup, which the next backup replaces
        let mut perm = std::fs::metadata(&file).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&file, perm).unwrap();
        write_atomic_with(&file, b"third", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            "second"
        );
        let source = dir.path().join("source.txt");
        std::fs::write(&source, "fourth").unwrap();
        let mut perm = std::fs::metadata(&source).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&source, perm).unwrap();
        for _ in 0..2 {
            copy_atomic(&source, &file, true).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fourth");
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            "fourth"
        );
        remove_with(&file, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            "fourth"
        );
    }

    #[cfg(unix)]
    #[test]
    fn devices_are_written_in_place() {
        write_atomic_with(Path::new("/dev/null"), b"x", true).unwrap();
        assert!(!Path::new("/dev/null.bak").exists());
    }

    #[test]
    fn copies_whole_files_with_their_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from.txt"), dir.path().join("sub/to.txt"));
        std::fs::write(&from, "new").unwrap();
        let old_time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        std::fs::File::options()
            .write(true)
            .open(&from)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
        copy_atomic(&from, &to, true).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&to).unwrap().modified().unwrap(),
            old_time
        );
        assert!(!backup_path(&to).exists());
        std::fs::write(&to, "old").unwrap();
        let mut perm = std::fs::metadata(&to).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&to, perm).unwrap();
        copy_atomic(&from, &to, true).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(backup_path(&to)).unwrap(), "old");
        assert!(!std::fs::metadata(&to).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            let elsewhere = dir.path().join("elsewhere.txt");
            std::fs::write(&elsewhere, "untouched").unwrap();
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
            copy_atomic(&from, &link, false).unwrap();
            assert!(!std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
            assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "untouched");
        }
        assert!(copy_atomic(&dir.path().join("missing"), &to, false).is_err());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
    }

    #[test]
    fn removes_a_file_or_keeps_it_as_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("c.txt");
        std::fs::write(&file, "gone").unwrap();
        remove_with(&file, false).unwrap();
        assert!(!file.exists() && !backup_path(&file).exists());
        std::fs::write(&file, "kept").unwrap();
        remove_with(&file, true).unwrap();
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(backup_path(&file)).unwrap(), "kept");
        assert!(remove_with(&file, false).is_err());
    }

    #[test]
    fn removes_emptied_folders_up_to_the_root() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.path().join("a/keep.txt"), "x").unwrap();
        remove_empty_dirs(&deep, root.path());
        assert!(!root.path().join("a/b").exists());
        assert!(root.path().join("a/keep.txt").exists());
        std::fs::remove_file(root.path().join("a/keep.txt")).unwrap();
        remove_empty_dirs(&root.path().join("a"), root.path());
        assert!(!root.path().join("a").exists() && root.path().exists());
        let outside = tempfile::tempdir().unwrap();
        remove_empty_dirs(outside.path(), root.path());
        assert!(outside.path().exists());
    }
}
