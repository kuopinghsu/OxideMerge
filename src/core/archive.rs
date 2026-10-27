//! Archive support: archives are extracted to a temporary folder and compared as folders.
//! Zip based formats, tar (plain or compressed with gzip, bzip2 or xz) and 7z are supported.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Component, Path};

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
    remove_links_outside(dir.path())?;
    Ok(dir)
}

/// Removes the symbolic links under `root` that lead outside it, or nowhere. Each link is
/// resolved as the system resolves it, through the other links on the way.
fn remove_links_outside(root: &Path) -> Result<()> {
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
fn extract_tar(input: impl Read, dir: &Path) -> Result<()> {
    let mut ar = tar::Archive::new(input);
    for entry in ar.entries()? {
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
