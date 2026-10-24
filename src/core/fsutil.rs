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

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Writes `bytes` to a temporary file next to `path` and renames it over `path`, so a crash or
/// a full disk leaves either the old or the new contents. A symbolic link is followed, and the
/// file keeps its permissions.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let old = std::fs::metadata(&target).ok();
    if old.is_some() && BACKUP.load(Ordering::Relaxed) {
        let bak = backup_path(&target);
        std::fs::copy(&target, &bak).with_context(|| format!("writing {}", bak.display()))?;
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let staged = (|| -> std::io::Result<tempfile::NamedTempFile> {
        let mut tmp = tempfile::Builder::new()
            .prefix(".oxidemerge-")
            .tempfile_in(dir)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        if let Some(m) = &old {
            tmp.as_file().set_permissions(m.permissions())?;
        }
        Ok(tmp)
    })();
    match staged.map(|tmp| tmp.persist(&target)) {
        Ok(Ok(_)) => Ok(()),
        // A folder that cannot hold new files, or a file that cannot be replaced: write in place.
        _ => std::fs::write(&target, bytes).with_context(|| format!("writing {}", path.display())),
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

    #[test]
    fn keeps_a_backup_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.txt");
        std::fs::write(&file, "first").unwrap();
        set_backup(true);
        let saved = write_atomic(&file, b"second");
        set_backup(false);
        saved.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        assert_eq!(
            std::fs::read_to_string(backup_path(&file)).unwrap(),
            "first"
        );
    }
}
