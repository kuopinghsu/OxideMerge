//! Archive support: archives are extracted to a temporary folder and compared as folders.
//! Zip based formats, tar (plain or compressed with gzip, bzip2 or xz) and 7z are supported.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

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

pub fn extract_to_temp(path: &Path) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("oxidemerge-").tempdir()?;
    let open = || std::fs::File::open(path).with_context(|| format!("open {}", path.display()));
    match kind(path).unwrap_or(Kind::Zip) {
        Kind::Zip => extract_zip(open()?, dir.path())
            .with_context(|| format!("{} is not a zip archive", path.display()))?,
        Kind::Tar => extract_tar(open()?, dir.path())?,
        Kind::TarGz => extract_tar(flate2::read::MultiGzDecoder::new(open()?), dir.path())?,
        Kind::TarBz2 => extract_tar(bzip2::read::MultiBzDecoder::new(open()?), dir.path())?,
        Kind::TarXz => {
            let mut input = std::io::BufReader::new(open()?);
            let mut tar = Vec::new();
            lzma_rs::xz_decompress(&mut input, &mut tar)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            extract_tar(tar.as_slice(), dir.path())?
        }
        Kind::SevenZ => sevenz_rust2::decompress_file(path, dir.path())
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
    }
    Ok(dir)
}

/// Finder metadata that macOS adds to archives it creates (`._name` and `__MACOSX/`).
fn apple_metadata(rel: &Path) -> bool {
    rel.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        c == "__MACOSX" || c.starts_with("._")
    })
}

fn extract_zip(file: std::fs::File, dir: &Path) -> Result<()> {
    let mut ar = zip::ZipArchive::new(file)?;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i)?;
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
            std::io::copy(&mut f, &mut w)?;
        }
    }
    Ok(())
}

/// Entries that cannot be created here (links on Windows, device files) are skipped;
/// `unpack_in` also skips any that would land outside `dir`.
fn extract_tar(input: impl Read, dir: &Path) -> Result<()> {
    let mut ar = tar::Archive::new(input);
    for entry in ar.entries()? {
        let mut entry = entry?;
        if entry.path().is_ok_and(|p| apple_metadata(&p)) {
            continue;
        }
        let _ = entry.unpack_in(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
