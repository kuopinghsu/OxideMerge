//! Files and folders as they were in a Git revision, read with the `git` command.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().context("git could not be started")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().next().unwrap_or("git failed").trim());
    }
    Ok(out.stdout)
}

/// The repository holding `path` and the path relative to its top folder, with `/`
/// separators (empty for the top folder itself).
pub fn locate(path: &Path) -> Result<(PathBuf, String)> {
    let abs = dunce_canonical(path)?;
    let dir = if abs.is_dir() {
        abs.clone()
    } else {
        abs.parent().context("no parent folder")?.to_path_buf()
    };
    let top = git(&dir, &["rev-parse", "--show-toplevel"])?;
    let root = dunce_canonical(Path::new(String::from_utf8_lossy(&top).trim()))?;
    let rel = abs
        .strip_prefix(&root)
        .with_context(|| format!("{} is not in {}", abs.display(), root.display()))?;
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok((root, rel))
}

/// Canonical path without the `\\?\` prefix Windows adds, which git does not print.
fn dunce_canonical(path: &Path) -> Result<PathBuf> {
    let p = std::fs::canonicalize(path).with_context(|| path.display().to_string())?;
    #[cfg(windows)]
    if let Some(s) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        if !s.starts_with("UNC\\") {
            return Ok(PathBuf::from(s));
        }
    }
    Ok(p)
}

/// The contents of file `path` in revision `rev` (a commit, branch, tag, `HEAD~2`, ...).
pub fn show_file(path: &Path, rev: &str) -> Result<Vec<u8>> {
    let (root, rel) = locate(path)?;
    git(&root, &["show", &format!("{rev}:{rel}")])
}

/// Folder `path` as it was in revision `rev`, extracted to a temporary folder. Returns the
/// temporary folder (deleted when dropped) and the extracted copy of `path` inside it.
pub fn export_folder(path: &Path, rev: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let (root, rel) = locate(path)?;
    let mut args = vec!["archive", "--format=tar", rev];
    if !rel.is_empty() {
        args.extend(["--", rel.as_str()]);
    }
    let tar = git(&root, &args)?;
    let dir = tempfile::Builder::new()
        .prefix("oxidemerge-git-")
        .tempdir()?;
    super::archive::extract_tar(tar.as_slice(), dir.path())?;
    let copy = dir.path().join(&rel);
    std::fs::create_dir_all(&copy)?;
    Ok((dir, copy))
}

/// Recent commits touching `path`, newest first: (short hash, subject).
pub fn recent_commits(path: &Path, n: usize) -> Vec<(String, String)> {
    let Ok((root, rel)) = locate(path) else {
        return Vec::new();
    };
    let count = format!("-n{n}");
    let mut args = vec!["log", count.as_str(), "--format=%h%x09%s"];
    if !rel.is_empty() {
        args.extend(["--", rel.as_str()]);
    }
    git(&root, &args)
        .map(|out| {
            String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(h, s)| (h.to_string(), s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_files_and_folders_of_a_revision() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| git(repo, args).unwrap();
        if git(repo, &["init", "-q"]).is_err() {
            return; // git is not installed
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(repo.join("src/a.txt"), "two\n").unwrap();

        let file = repo.join("src/a.txt");
        assert_eq!(locate(&file).unwrap().1, "src/a.txt");
        assert_eq!(show_file(&file, "HEAD").unwrap(), b"one\n");
        let (_tmp, copy) = export_folder(&repo.join("src"), "HEAD").unwrap();
        assert_eq!(std::fs::read(copy.join("a.txt")).unwrap(), b"one\n");
        let commits = recent_commits(&file, 5);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].1, "first");
        assert!(show_file(&file, "nosuchrev").is_err());
    }
}
