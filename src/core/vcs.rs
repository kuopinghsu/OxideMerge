//! Files and folders as they were in a revision of the version control system that holds
//! them: Git (through `git.rs`) or Subversion (through the `svn` command).
//!
//! The working copy decides: the nearest folder above the path with a `.git` or `.svn` entry.
//! Git revisions are read from the local repository. Subversion reads `BASE` from the working
//! copy; other revisions, and the list of recent ones, come from the repository server the
//! working copy names, as `svn` itself would.

use super::git;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vcs {
    Git,
    Subversion,
}

impl Vcs {
    /// The version control system of the working copy holding `path`, if any.
    pub fn detect(path: &Path) -> Option<Vcs> {
        let abs = git::dunce_canonical(path).ok()?;
        let start = if abs.is_dir() {
            abs.as_path()
        } else {
            abs.parent()?
        };
        start.ancestors().find_map(|dir| {
            if dir.join(".git").exists() {
                Some(Vcs::Git)
            } else if dir.join(".svn").is_dir() {
                Some(Vcs::Subversion)
            } else {
                None
            }
        })
    }

    /// The system of `path`, Git when it is in no working copy (so the error is Git's).
    pub fn of(path: &Path) -> Vcs {
        Vcs::detect(path).unwrap_or(Vcs::Git)
    }

    /// The revision compared with when none is given: the last commit, or for Subversion the
    /// revision the working copy was updated to.
    pub fn default_rev(self) -> &'static str {
        match self {
            Vcs::Git => "HEAD",
            Vcs::Subversion => "BASE",
        }
    }

    /// Examples of revisions, for an empty revision field.
    pub fn rev_hint(self) -> &'static str {
        match self {
            Vcs::Git => "HEAD, HEAD~1, main, v1.0 …",
            Vcs::Subversion => "BASE, PREV, HEAD, 1234 …",
        }
    }
}

fn svn(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = git::tool("svn", dir);
    utf8_locale(&mut cmd);
    cmd.arg("--non-interactive").args(args);
    git::run(cmd, None)
}

/// `svn` converts paths and messages with the locale's character set, so without a UTF-8
/// locale (as when the program is started from a desktop) a path that is not ASCII fails and
/// messages come out as `{U+...}`. Gives it a UTF-8 character set, keeping the language.
fn utf8_locale(cmd: &mut std::process::Command) {
    if cfg!(windows) {
        return;
    }
    let var = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k, v)));
    let is_utf8 = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.contains("utf-8") || v.contains("utf8")
    };
    let utf8 = if cfg!(target_os = "macos") {
        "UTF-8"
    } else {
        "C.UTF-8"
    };
    match var {
        Some((_, v)) if is_utf8(&v) => {}
        Some(("LC_ALL", _)) => {
            cmd.env("LC_ALL", utf8);
        }
        _ => {
            cmd.env("LC_CTYPE", utf8);
        }
    }
}

/// A Subversion revision as `svn` takes it: `r12`, as `svn log` prints it, becomes `12`.
fn svn_rev(rev: &str) -> Result<String> {
    let rev = git::check_rev(rev)?;
    Ok(match rev.strip_prefix('r') {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n.to_string(),
        _ => rev.to_string(),
    })
}

/// `path` for `svn`: absolute, and ending in `@` so an `@` in the name is not read as a
/// peg revision.
fn svn_target(path: &Path) -> Result<(PathBuf, String)> {
    let abs = git::dunce_canonical(path)?;
    let dir = abs.parent().context("no parent folder")?.to_path_buf();
    let target = format!("{}@", abs.display());
    Ok((dir, target))
}

/// The contents of file `path` in revision `rev`, or in the default revision when `rev` is
/// empty.
pub fn show_file(path: &Path, rev: &str) -> Result<Vec<u8>> {
    let vcs = Vcs::of(path);
    let rev = pick(vcs, rev);
    match vcs {
        Vcs::Git => git::show_file(path, rev),
        Vcs::Subversion => {
            let rev = svn_rev(rev)?;
            let (dir, target) = svn_target(path)?;
            svn(&dir, &["cat", "-r", &rev, "--", &target])
        }
    }
}

/// Folder `path` as it was in revision `rev`, in a temporary folder. Returns the temporary
/// folder (deleted when dropped) and the copy of `path` inside it.
pub fn export_folder(path: &Path, rev: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let vcs = Vcs::of(path);
    let rev = pick(vcs, rev);
    match vcs {
        Vcs::Git => git::export_folder(path, rev),
        Vcs::Subversion => {
            let rev = svn_rev(rev)?;
            let (dir, target) = svn_target(path)?;
            let temp = tempfile::Builder::new()
                .prefix("oxidemerge-svn-")
                .tempdir()?;
            let name = path
                .canonicalize()?
                .file_name()
                .map(|n| n.to_owned())
                .unwrap_or_else(|| "export".into());
            let copy = temp.path().join(name);
            // the destination is read for a peg revision too
            let dest = format!("{}@", copy.display());
            svn(&dir, &["export", "-q", "-r", &rev, "--", &target, &dest])?;
            std::fs::create_dir_all(&copy)?;
            Ok((temp, copy))
        }
    }
}

/// Recent revisions that changed `path`, newest first: (revision, first line of the message).
pub fn recent_revisions(path: &Path, n: usize) -> Vec<(String, String)> {
    match Vcs::detect(path) {
        Some(Vcs::Subversion) => {
            let Ok((dir, target)) = svn_target(path) else {
                return Vec::new();
            };
            let limit = n.to_string();
            svn(&dir, &["log", "-l", &limit, "--", &target])
                .map(|out| parse_svn_log(&String::from_utf8_lossy(&out)))
                .unwrap_or_default()
        }
        _ => git::recent_commits(path, n),
    }
}

fn pick(vcs: Vcs, rev: &str) -> &str {
    match rev.trim() {
        "" => vcs.default_rev(),
        r => r,
    }
}

/// The revisions and subjects of `svn log` output: entries are a dashed line, a header
/// `r12 | author | date | 1 line`, a blank line and the message.
fn parse_svn_log(log: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = log.lines();
    while let Some(line) = lines.next() {
        let Some((rev, rest)) = line.split_once(" | ") else {
            continue;
        };
        let is_header = rev.len() > 1
            && rev.starts_with('r')
            && rev[1..].bytes().all(|b| b.is_ascii_digit())
            && rest.contains(" | ");
        if !is_header {
            continue;
        }
        let subject = lines
            .by_ref()
            .take_while(|l| !l.starts_with("-----"))
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default();
        out.push((rev.to_string(), subject.trim().to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svn_log_entries_give_their_revision_and_first_message_line() {
        let log = "\
------------------------------------------------------------------------
r3 | ann | 2026-11-04 10:00:00 +0000 (Wed, 04 Nov 2026) | 2 lines

Fix the parser
and its tests
------------------------------------------------------------------------
r2 | bob | 2026-11-03 09:00:00 +0000 (Tue, 03 Nov 2026) | 1 line


------------------------------------------------------------------------
r1 | ann | 2026-11-02 08:00:00 +0000 (Mon, 02 Nov 2026) | 1 line

First | with a bar
------------------------------------------------------------------------
";
        assert_eq!(
            parse_svn_log(log),
            vec![
                ("r3".into(), "Fix the parser".into()),
                ("r2".into(), String::new()),
                ("r1".into(), "First | with a bar".into()),
            ]
        );
    }

    #[test]
    fn svn_revisions_lose_the_r_of_svn_log_and_refuse_options() {
        assert_eq!(svn_rev("r12").unwrap(), "12");
        assert_eq!(svn_rev(" 12 ").unwrap(), "12");
        assert_eq!(svn_rev("PREV").unwrap(), "PREV");
        assert_eq!(svn_rev("{2026-11-01}").unwrap(), "{2026-11-01}");
        assert!(svn_rev("-r5").is_err());
        assert!(svn_rev("").is_err());
    }

    #[test]
    fn the_nearest_working_copy_decides_and_none_is_git() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("wc/.svn")).unwrap();
        std::fs::create_dir_all(d.path().join("wc/sub/repo/.git")).unwrap();
        std::fs::write(d.path().join("wc/a.txt"), "a").unwrap();
        std::fs::write(d.path().join("wc/sub/repo/b.txt"), "b").unwrap();
        assert_eq!(
            Vcs::detect(&d.path().join("wc/a.txt")),
            Some(Vcs::Subversion)
        );
        assert_eq!(Vcs::detect(&d.path().join("wc/sub")), Some(Vcs::Subversion));
        assert_eq!(
            Vcs::detect(&d.path().join("wc/sub/repo/b.txt")),
            Some(Vcs::Git)
        );
        assert_eq!(Vcs::of(d.path()), Vcs::Git);
    }
}
