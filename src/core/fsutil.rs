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
        // Only Windows keeps a read-only file from being deleted.
        if cfg!(windows) {
            make_writable(&bak);
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
/// Files kept under `/dev/` (`/dev/shm`) are not.
fn is_special(path: &Path) -> bool {
    let in_dev = cfg!(unix)
        && path.starts_with("/dev/")
        && !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    in_dev || std::fs::metadata(path).is_ok_and(|m| !m.is_file() && !m.is_dir())
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

/// Writes `bytes` to a temporary file in `dir`, then renames it over `target`.
/// If staging or replacement fails, return the error without writing into the original.
fn write_staged(
    target: &Path,
    dir: &Path,
    bytes: &[u8],
    old: Option<&std::fs::Metadata>,
) -> std::io::Result<()> {
    let mut tmp = tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    let perm = old.map(std::fs::Metadata::permissions);
    // Windows can neither remove a read-only temporary file nor rename over a read-only file,
    // so there a read-only file is made writable for the rename and read-only again after it.
    let unlocked = if cfg!(unix) {
        if let Some(p) = &perm {
            let _ = tmp.as_file().set_permissions(p.clone());
        }
        false
    } else {
        perm.as_ref().is_some_and(|p| p.readonly()) && make_writable(target)
    };
    let written = tmp.persist(target).map(|_| ()).map_err(|e| e.error);
    if unlocked {
        if let Some(p) = perm {
            let _ = std::fs::set_permissions(target, p);
        }
    }
    written
}

/// Clears the read-only flag of the file `path`; whether it was set and is now cleared.
fn make_writable(path: &Path) -> bool {
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let mut perm = md.permissions();
    if !md.is_file() || !perm.readonly() {
        return false;
    }
    // For the owner only: clearing read-only on Unix would let everyone write.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perm.set_mode(perm.mode() | 0o200);
    }
    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
    }
    std::fs::set_permissions(path, perm).is_ok()
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
    let mut tmp = tempfile::Builder::new()
        .prefix(".oxidemerge-")
        .tempfile_in(dir)
        .with_context(copying)?;
    std::io::copy(&mut src, tmp.as_file_mut()).with_context(copying)?;
    if let Some(t) = modified {
        let _ = tmp.as_file().set_modified(t);
    }
    tmp.as_file().sync_all().with_context(copying)?;
    // Set once in place: a read-only temporary file could not be removed on Windows, which
    // can't rename over a read-only file either, so that is made writable for the rename.
    let old = std::fs::symlink_metadata(to).map(|m| m.permissions());
    let unlocked = cfg!(windows) && make_writable(to);
    match tmp.persist(to) {
        Ok(_) => {
            let _ = std::fs::set_permissions(to, perm);
            Ok(())
        }
        Err(e) => {
            if let (true, Ok(p)) = (unlocked, old) {
                let _ = std::fs::set_permissions(to, p);
            }
            Err(e.error).with_context(copying)
        }
    }
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
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "third");
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
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
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().starts_with(".oxidemerge-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[cfg(unix)]
    #[test]
    fn devices_are_written_in_place() {
        write_atomic_with(Path::new("/dev/null"), b"x", true).unwrap();
        assert!(!Path::new("/dev/null.bak").exists());
        assert!(is_special(Path::new("/dev/stdout")));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_files_become_writable_for_their_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ro.txt");
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(make_writable(&file));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
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
        // A read-only file is replaced, not written into: a link to it keeps the old content.
        std::fs::write(&to, "old").unwrap();
        let alias = dir.path().join("alias.txt");
        std::fs::hard_link(&to, &alias).unwrap();
        let mut perm = std::fs::metadata(&to).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&to, perm).unwrap();
        copy_atomic(&from, &to, false).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&alias).unwrap(), "old");
        make_writable(&alias);
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

    #[cfg(unix)]
    #[test]
    fn staging_and_replacement_failures_preserve_existing_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("protected");
        std::fs::create_dir(&protected).unwrap();
        let target = protected.join("target.txt");
        let source = dir.path().join("source.txt");
        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&source, b"replacement").unwrap();
        let old = std::fs::metadata(&target).unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Privileged users can bypass directory permissions, so they cannot exercise this case.
        let probe = protected.join("probe");
        if std::fs::write(&probe, b"").is_ok() {
            std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        // The file is writable, but its directory prevents staging and replacement.
        let staging = write_atomic_with(&target, b"replacement", false);
        let after_staging = std::fs::read(&target).unwrap();
        let copying = copy_atomic(&source, &target, false);
        let after_copying = std::fs::read(&target).unwrap();
        // Stage outside the protected directory to reach the failed rename independently.
        let replacing = write_staged(&target, dir.path(), b"replacement", Some(&old));
        let after_replacing = std::fs::read(&target).unwrap();
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(staging.is_err());
        assert_eq!(after_staging, b"original");
        assert!(copying.is_err());
        assert_eq!(after_copying, b"original");
        assert!(replacing.is_err());
        assert_eq!(after_replacing, b"original");
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".oxidemerge-")));
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
