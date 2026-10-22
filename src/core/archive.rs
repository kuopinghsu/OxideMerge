//! Archive support: zip based archives are extracted to a temporary folder and compared as folders.

use anyhow::{Context, Result};
use std::path::Path;

pub const ARCHIVE_EXTS: &[&str] = &[
    "zip", "jar", "war", "ear", "apk", "aar", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "nupkg",
    "vsix", "whl", "xpi", "crx", "ipa", "appx", "msix",
];

pub fn is_archive(path: &Path) -> bool {
    path.is_file() && ARCHIVE_EXTS.contains(&super::text::extension(path).as_str())
}

pub fn extract_to_temp(path: &Path) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("oxidemerge-").tempdir()?;
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut ar = zip::ZipArchive::new(file).with_context(|| format!("{} is not a zip archive", path.display()))?;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dir.path().join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut f, &mut w)?;
        }
    }
    Ok(dir)
}
