//! Files and folders as they were in a Git revision, read with the `git` command.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs `cmd`, writing `input` to it, and returns what it printed.
fn run(mut cmd: Command, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("git could not be started")?;
    // written on another thread so a full output pipe cannot block the input
    let writer = child.stdin.take().zip(input).map(|(mut stdin, data)| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        })
    });
    let out = child.wait_with_output()?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().next().unwrap_or("git failed").trim());
    }
    Ok(out.stdout)
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = command(dir);
    cmd.args(args);
    run(cmd, None)
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

/// A revision is passed to git as an argument, so it must not look like an option.
fn check_rev(rev: &str) -> Result<&str> {
    let rev = rev.trim();
    if rev.is_empty() || rev.starts_with('-') {
        bail!("invalid revision: {rev}");
    }
    Ok(rev)
}

/// The contents of file `path` in revision `rev` (a commit, branch, tag, `HEAD~2`, ...), with
/// the line endings and filters a checkout would apply, like the working copy it is compared to.
pub fn show_file(path: &Path, rev: &str) -> Result<Vec<u8>> {
    let rev = check_rev(rev)?;
    let (root, rel) = locate(path)?;
    git(&root, &["cat-file", "--filters", &format!("{rev}:{rel}")])
}

/// Folder `path` as it was in revision `rev`, extracted to a temporary folder. Returns the
/// temporary folder (deleted when dropped) and the extracted copy of `path` inside it.
pub fn export_folder(path: &Path, rev: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let rev = check_rev(rev)?;
    let (root, rel) = locate(path)?;
    let dir = tempfile::Builder::new()
        .prefix("oxidemerge-git-")
        .tempdir()?;
    checkout_into(&root, rev, &rel, dir.path())?;
    let copy = dir.path().join(&rel);
    std::fs::create_dir_all(&copy)?;
    Ok((dir, copy))
}

/// Writes folder `rel` (empty for all) of revision `rev` of the repository at `root` into
/// `dest` as a checkout would, with its line endings and filters such as Git LFS, through a
/// separate index so the repository's own index and working copy are left alone. Submodules
/// are filled in from their checked-out repositories when the recorded commit is there.
fn checkout_into(root: &Path, rev: &str, rel: &str, dest: &Path) -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    let with_index = |args: &[&str]| {
        let mut cmd = command(root);
        cmd.env("GIT_INDEX_FILE", &index)
            .arg("--literal-pathspecs")
            .args(args);
        cmd
    };
    run(with_index(&["read-tree", rev]), None)?;
    let mut ls = vec!["ls-files", "--stage", "-z"];
    if !rel.is_empty() {
        ls.extend(["--", rel]);
    }
    let listing = run(with_index(&ls), None)?;
    let mut files = Vec::new();
    let mut submodules = Vec::new();
    // entries are "mode hash stage<TAB>path"
    for entry in listing.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let (meta, path) = (&entry[..tab], &entry[tab + 1..]);
        if meta.starts_with(b"160000 ") {
            let hash = meta.split(|&b| b == b' ').nth(1).unwrap_or_default();
            submodules.push((
                String::from_utf8_lossy(path).into_owned(),
                String::from_utf8_lossy(hash).into_owned(),
            ));
        } else {
            files.extend_from_slice(path);
            files.push(0);
        }
    }
    if !files.is_empty() {
        let prefix = format!("--prefix={}/", dest.display());
        let checkout = |skip_lfs: bool| {
            let mut cmd = with_index(&["checkout-index", "--stdin", "-z", "-f", &prefix]);
            if skip_lfs {
                // LFS files of old revisions may not have been downloaded: show their pointers
                cmd.env("GIT_LFS_SKIP_SMUDGE", "1");
            }
            run(cmd, Some(files.clone()))
        };
        checkout(false).or_else(|_| checkout(true))?;
    }
    for (path, hash) in submodules {
        let target = dest.join(&path);
        std::fs::create_dir_all(&target)?;
        let work = root.join(&path);
        if work.join(".git").exists() {
            let _ = checkout_into(&work, &hash, "", &target);
        }
    }
    Ok(())
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
        run(&["config", "core.autocrlf", "false"]);
        std::fs::create_dir(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.txt"), "one\n").unwrap();
        std::fs::write(repo.join("src/skip.txt"), "kept\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "src/skip.txt export-ignore\n").unwrap();
        std::fs::create_dir(repo.join("s[1]")).unwrap();
        std::fs::write(repo.join("s[1]/b.txt"), "b\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(repo.join("src/a.txt"), "two\n").unwrap();

        let file = repo.join("src/a.txt");
        assert_eq!(locate(&file).unwrap().1, "src/a.txt");
        // Files and folders get the line endings of a checkout, as Windows' autocrlf does.
        for (autocrlf, expected) in [("false", &b"one\n"[..]), ("true", b"one\r\n")] {
            run(&["config", "core.autocrlf", autocrlf]);
            assert_eq!(show_file(&file, "HEAD").unwrap(), expected);
            let (_tmp, copy) = export_folder(&repo.join("src"), "HEAD").unwrap();
            assert_eq!(std::fs::read(copy.join("a.txt")).unwrap(), expected);
            // unlike `git archive`, export attributes do not leave files out
            assert!(copy.join("skip.txt").is_file());
        }
        let (_tmp, copy) = export_folder(&repo.join("s[1]"), "HEAD").unwrap();
        assert!(copy.join("b.txt").is_file());
        let commits = recent_commits(&file, 5);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].1, "first");
        assert!(show_file(&file, "nosuchrev").is_err());
        assert!(export_folder(repo, "--output=x").is_err());
        assert!(!repo.join("x").exists());

        // a submodule is filled in at its recorded commit
        let sub_dir = tempfile::tempdir().unwrap();
        let sub = sub_dir.path();
        let in_sub = |args: &[&str]| git(sub, args).unwrap();
        in_sub(&["init", "-q"]);
        in_sub(&["config", "user.email", "t@example.com"]);
        in_sub(&["config", "user.name", "T"]);
        in_sub(&["config", "commit.gpgsign", "false"]);
        std::fs::write(sub.join("m.txt"), "recorded\n").unwrap();
        in_sub(&["add", "."]);
        in_sub(&["commit", "-q", "-m", "sub"]);
        let added = git(
            repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &sub.display().to_string(),
                "module",
            ],
        );
        if added.is_ok() {
            // the clone `submodule add` made, which the export reads from
            git(&repo.join("module"), &["config", "core.autocrlf", "false"]).unwrap();
            run(&["commit", "-q", "-m", "module"]);
            std::fs::write(repo.join("module/m.txt"), "edited\n").unwrap();
            let (_tmp, copy) = export_folder(repo, "HEAD").unwrap();
            assert_eq!(
                std::fs::read_to_string(copy.join("module/m.txt")).unwrap(),
                "recorded\n"
            );
        }
    }
}
