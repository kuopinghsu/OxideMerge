//! Shared bounds, stable identities and versioned opt-in analysis report output.
use anyhow::{bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_FILE: u64 = 16 << 20;
pub const MAX_TOTAL: u64 = 256 << 20;
pub const MAX_FILES: usize = 10_000;

pub const MAX_REPORT: usize = 32 << 20;
pub fn json<T: Serialize>(value: &T) -> Result<String> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + bytes.len() > MAX_REPORT {
                return Err(std::io::Error::other(
                    "Analysis report exceeds 32 MiB; narrow the scope",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut output, value)?;
    Ok(String::from_utf8(output.0)?)
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn identity<T: Serialize>(value: &T) -> String {
    digest(&serde_json::to_vec(value).expect("serializable analysis identity"))
}
pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Analysis cancelled");
    }
    Ok(())
}
pub fn read(path: &Path) -> Result<Vec<u8>> {
    read_checked(path, None)
}
pub fn read_in_root(path: &Path, root: &Path) -> Result<Vec<u8>> {
    read_checked(path, Some(root))
}
fn read_checked(path: &Path, root: Option<&Path>) -> Result<Vec<u8>> {
    let before = std::fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() || before.len() > MAX_FILE {
        bail!("Not a regular file or exceeds 16 MiB: {}", path.display());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize()?;
    if let Some(root) = root {
        if !parent.starts_with(root) {
            bail!("Analysis input escapes its selected root");
        }
    }
    let canonical = parent.join(
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("Missing input file name"))?,
    );
    let mut f = open_nofollow(&canonical)?;
    let opened = f.metadata()?;
    if !opened.is_file() || opened.file_type().is_symlink() || opened.len() > MAX_FILE {
        bail!("Analysis input changed type");
    }
    let mut bytes = Vec::new();
    f.by_ref().take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    let after = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev()
            || before.ino() != opened.ino()
            || opened.dev() != after.dev()
            || opened.ino() != after.ino()
        {
            bail!("Analysis input was replaced while reading");
        }
    }
    if after.file_type().is_symlink()
        || bytes.len() as u64 > MAX_FILE
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || opened.len() != after.len()
        || opened.modified().ok() != after.modified().ok()
        || path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?
            != parent
    {
        bail!("Input changed during analysis: {}", path.display());
    }
    Ok(bytes)
}
#[cfg(unix)]
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    // Open each canonical component relative to a held descriptor, so swapping an ancestor
    // for a symlink between containment validation and open cannot escape the selected root.
    let mut directory = std::fs::File::open("/")?;
    let parts: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();
    for (i, component) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(component.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: directory owns a live descriptor; name is NUL terminated; the returned
        // descriptor is uniquely owned by File and automatically closed on every path.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}
#[cfg(not(unix))]
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    options.open(path)
}
/// Separate report envelope; existing comparison XML/HTML schemas are unchanged.
pub fn report<T: Serialize>(value: &T, path: &Path, inputs: &[std::path::PathBuf]) -> Result<()> {
    let json = json(value)?;
    let escaped = super::xml_report::esc(&json);
    let output = match super::text::extension(path).as_str() {
        "html" | "htm" => format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>OxideMerge analysis</title><style>body{{font:15px system-ui;margin:2rem}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}</style><h1>OxideMerge analysis</h1><pre>{escaped}</pre></html>"),
        "xml" => format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><oxide-analysis version=\"1\"><json>{escaped}</json></oxide-analysis>"),
        _ => json,
    };
    write_report_text(path, &output, inputs)
}
pub fn write_report_text(path: &Path, output: &str, inputs: &[std::path::PathBuf]) -> Result<()> {
    if output.len() > MAX_REPORT {
        bail!("Analysis report exceeds 32 MiB; narrow the comparison");
    }
    let absolute = std::path::absolute(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination = parent.canonicalize()?.join(
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid report path"))?,
    );
    if inputs.iter().any(|p| {
        std::path::absolute(p).is_ok_and(|p| p == absolute)
            || p.canonicalize()
                .is_ok_and(|p| p == destination || (p.is_dir() && destination.starts_with(&p)))
            || (path.exists() && same_file::is_same_file(p, path).unwrap_or(false))
    }) {
        bail!("An analysis report cannot replace an input");
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        bail!("Report destination must be a regular file");
    }
    super::fsutil::write_atomic_report(&destination, output.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_cannot_replace_an_input_or_a_file_inside_an_input_root() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.json");
        std::fs::write(&input, b"original").unwrap();
        assert!(report(&"report", &input, std::slice::from_ref(&input)).is_err());
        assert!(report(&"report", &input, &[dir.path().into()]).is_err());
        let alias = dir.path().join("alias");
        std::fs::hard_link(&input, &alias).unwrap();
        assert!(report(&"report", &alias, std::slice::from_ref(&input)).is_err());
        assert_eq!(std::fs::read(input).unwrap(), b"original");
    }
}
