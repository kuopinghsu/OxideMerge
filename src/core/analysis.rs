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
    // Tree callers walk canonical roots without following links. Refuse a link leaf.
    let before = std::fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() || before.len() > MAX_FILE {
        bail!("Not a regular file or exceeds 16 MiB: {}", path.display());
    }
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
        options.custom_flags(0x00200000);
    }
    let mut f = options.open(path)?;
    let opened = f.metadata()?;
    let mut bytes = Vec::new();
    f.by_ref().take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    let after = std::fs::symlink_metadata(path)?;
    if after.file_type().is_symlink()
        || bytes.len() as u64 > MAX_FILE
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || opened.len() != after.len()
        || opened.modified().ok() != after.modified().ok()
    {
        bail!("Input changed during analysis: {}", path.display());
    }
    Ok(bytes)
}
/// Separate report envelope; existing comparison XML/HTML schemas are unchanged.
pub fn report<T: Serialize>(value: &T, path: &Path, inputs: &[std::path::PathBuf]) -> Result<()> {
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
    let json = serde_json::to_string_pretty(value)?;
    let escaped = super::xml_report::esc(&json);
    let output = match super::text::extension(path).as_str() {
        "html" | "htm" => format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>OxideMerge analysis</title><style>body{{font:15px system-ui;margin:2rem}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}</style><h1>OxideMerge analysis</h1><pre>{escaped}</pre></html>"),
        "xml" => format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><oxide-analysis version=\"1\"><json>{escaped}</json></oxide-analysis>"),
        _ => json,
    };
    super::fsutil::write_atomic_with(path, output.as_bytes(), false)
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
