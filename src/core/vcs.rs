//! Files and folders as they were in a revision of the version control system that holds
//! them: Git (through `git.rs`), Subversion (the `svn` command) or Mercurial (`hg`).
//!
//! The working copy decides: the nearest folder above the path with a `.git`, `.svn` or `.hg`
//! entry. Git and Mercurial revisions are read from the local repository. Subversion reads
//! `BASE` from the working copy; other revisions, and the list of recent ones, come from the
//! repository server the working copy names, as `svn` itself would.

use super::git;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vcs {
    Git,
    Subversion,
    Mercurial,
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
            } else if dir.join(".hg").is_dir() {
                Some(Vcs::Mercurial)
            } else {
                None
            }
        })
    }

    /// The system of `path`, Git when it is in no working copy that is found by its folders
    /// (Git may still find one, through `GIT_DIR` for example).
    pub fn of(path: &Path) -> Vcs {
        Vcs::detect(path).unwrap_or(Vcs::Git)
    }

    /// The revision compared with when none is given: the last commit, or for Subversion the
    /// revision the working copy was updated to, or for Mercurial the working copy's parent.
    pub fn default_rev(self) -> &'static str {
        match self {
            Vcs::Git => "HEAD",
            Vcs::Subversion => "BASE",
            Vcs::Mercurial => ".",
        }
    }

    /// `rev` trimmed, or this system's default revision when it is empty.
    pub fn rev_or_default(self, rev: &str) -> &str {
        match rev.trim() {
            "" => self.default_rev(),
            r => r,
        }
    }

    /// Examples of revisions, for an empty revision field.
    pub fn rev_hint(self) -> &'static str {
        match self {
            Vcs::Git => "HEAD, HEAD~1, main, v1.0 …",
            Vcs::Subversion => "BASE, PREV, HEAD, 1234 …",
            Vcs::Mercurial => "., .^, tip, 42, v1.0 …",
        }
    }
}

fn svn(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git::run(svn_command(dir, args), None)
}

fn svn_command(dir: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = git::tool("svn", dir);
    utf8_locale(&mut cmd);
    cmd.arg("--non-interactive").args(args);
    cmd
}

/// `svn` converts paths and messages with the locale's character set, so without a UTF-8
/// locale (as when the program is started from a desktop) a path that is not ASCII fails and
/// messages come out as `{U+...}`. Gives it a UTF-8 character set, keeping the language.
fn utf8_locale(cmd: &mut std::process::Command) {
    if cfg!(windows) {
        return;
    }
    let Some(utf8) = utf8_ctype() else {
        return;
    };
    let get = |k: &str| std::env::var(k).ok();
    for (key, value) in utf8_locale_env(get("LC_ALL"), get("LC_CTYPE"), get("LANG"), utf8) {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
}

/// The locale variables to change, from `LC_ALL`, `LC_CTYPE` and `LANG` as they are set, so
/// that the character set is UTF-8 (`utf8`) and the language stays; `None` removes one.
fn utf8_locale_env(
    lc_all: Option<String>,
    lc_ctype: Option<String>,
    lang: Option<String>,
    utf8: &str,
) -> Vec<(&'static str, Option<String>)> {
    let set = |v: Option<String>| v.filter(|v| !v.is_empty());
    let is_utf8 = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.contains("utf-8") || v.contains("utf8")
    };
    match (set(lc_all), set(lc_ctype).or(set(lang))) {
        (Some(v), _) if is_utf8(&v) => Vec::new(),
        // LC_ALL would override LC_CTYPE: it goes, and its language stays for the messages
        (Some(v), _) => vec![
            ("LC_ALL", None),
            ("LANG", Some(v.clone())),
            ("LC_MESSAGES", Some(v)),
            ("LC_CTYPE", Some(utf8.to_string())),
        ],
        (None, Some(v)) if is_utf8(&v) => Vec::new(),
        (None, _) => vec![("LC_CTYPE", Some(utf8.to_string()))],
    }
}

/// A UTF-8 character set this system has: macOS takes `UTF-8` alone; elsewhere the first of
/// the usual names that `locale -a` lists, or any UTF-8 locale it lists. Looked up once.
fn utf8_ctype() -> Option<&'static str> {
    static FOUND: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            if cfg!(target_os = "macos") {
                return Some("UTF-8".into());
            }
            let out = std::process::Command::new("locale")
                .arg("-a")
                .output()
                .ok()?;
            let listed = String::from_utf8_lossy(&out.stdout).into_owned();
            pick_utf8_locale(listed.lines())
        })
        .as_deref()
}

fn pick_utf8_locale<'a>(listed: impl Iterator<Item = &'a str>) -> Option<String> {
    let listed: Vec<&str> = listed.map(str::trim).collect();
    let utf8 = |l: &&&str| {
        let l = l.to_ascii_lowercase();
        l.ends_with(".utf-8") || l.ends_with(".utf8")
    };
    ["C.UTF-8", "C.utf8", "en_US.UTF-8", "en_US.utf8"]
        .iter()
        .find(|want| listed.contains(want))
        .map(|s| s.to_string())
        .or_else(|| listed.iter().find(utf8).map(|s| s.to_string()))
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
    let rev = vcs.rev_or_default(rev);
    match vcs {
        Vcs::Git => no_working_copy(path, git::show_file(path, rev)),
        Vcs::Subversion => {
            let rev = svn_rev(rev)?;
            let (dir, target) = svn_target(path)?;
            svn(&dir, &["cat", "-r", &rev, "--", &target])
        }
        Vcs::Mercurial => {
            let rev = hg_rev(rev)?;
            let (root, rel) = hg_locate(path)?;
            let pattern = format!("path:{rel}");
            hg(&root, &["cat", "--decode", &rev, "--", &pattern])
        }
    }
}

/// Folder `path` as it was in revision `rev`, in a temporary folder. Returns the temporary
/// folder (deleted when dropped) and the copy of `path` inside it.
pub fn export_folder(path: &Path, rev: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let vcs = Vcs::of(path);
    let rev = vcs.rev_or_default(rev);
    match vcs {
        Vcs::Git => no_working_copy(path, git::export_folder(path, rev)),
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
            match svn(&dir, &["export", "-q", "-r", &rev, "--", &target, &dest]) {
                Ok(_) => {}
                // the folder was not there in that revision: it compares as empty, as in Git
                // and Mercurial (the codes are the same in every language)
                Err(e) if SVN_NOT_THERE.iter().any(|c| e.to_string().contains(c)) => {}
                Err(e) => return Err(e),
            }
            std::fs::create_dir_all(&copy)?;
            Ok((temp, copy))
        }
        Vcs::Mercurial => {
            let rev = hg_rev(rev)?;
            let (root, rel) = hg_locate(path)?;
            let temp = tempfile::Builder::new()
                .prefix("oxidemerge-hg-")
                .tempdir()?;
            let dest = temp.path().join("files");
            let mut args = vec![
                "--config",
                "ui.archivemeta=false",
                "archive",
                "-t",
                "files",
                &rev,
            ];
            let include = format!("path:{rel}");
            if !rel.is_empty() {
                args.extend(["-I", include.as_str()]);
            }
            let dest_arg = dest.display().to_string();
            args.extend(["--", dest_arg.as_str()]);
            match hg(&root, &args) {
                Ok(_) => {}
                // the folder had no files in that revision: it compares as empty
                Err(e) if e.to_string().contains("no files match") => {}
                Err(e) => return Err(e),
            }
            let copy = dest.join(&rel);
            std::fs::create_dir_all(&copy)?;
            Ok((temp, copy))
        }
    }
}

/// Recent revisions that changed `path`, newest first: (revision, first line of the message).
/// Setting `cancel` stops a Subversion or Mercurial log that is still running (one waiting on a
/// server), and the list comes back empty.
pub fn recent_revisions(path: &Path, n: usize, cancel: &AtomicBool) -> Vec<(String, String)> {
    match Vcs::detect(path) {
        Some(Vcs::Subversion) => {
            let Ok((dir, target)) = svn_target(path) else {
                return Vec::new();
            };
            let limit = n.to_string();
            git::run_cancellable(
                svn_command(&dir, &["log", "-l", &limit, "--", &target]),
                cancel,
            )
            .map(|out| parse_svn_log(&String::from_utf8_lossy(&out)))
            .unwrap_or_default()
        }
        Some(Vcs::Mercurial) => {
            let Ok((root, rel)) = hg_locate(path) else {
                return Vec::new();
            };
            let limit = n.to_string();
            let pattern = format!("path:{rel}");
            let mut args = vec![
                "log",
                "-l",
                &limit,
                "--template",
                "{node|short}\\t{desc|firstline}\\n",
            ];
            if !rel.is_empty() {
                args.extend(["--", pattern.as_str()]);
            }
            git::run_cancellable(hg_command(&root, &args), cancel)
                .map(|out| {
                    String::from_utf8_lossy(&out)
                        .lines()
                        .filter_map(|l| l.split_once('\t'))
                        .map(|(h, s)| (h.to_string(), s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => git::recent_commits(path, n),
    }
}

/// `hg` with the plain, untranslated behavior meant for scripts, UTF-8 file names and no
/// prompts.
fn hg(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git::run(hg_command(dir, args), None)
}

fn hg_command(dir: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = git::tool("hg", dir);
    cmd.env("HGPLAIN", "1")
        .env("HGENCODING", "utf-8")
        .arg("-y")
        .args(args);
    cmd
}

/// The repository holding `path` and the path relative to its top folder, with `/`
/// separators (empty for the top folder itself).
fn hg_locate(path: &Path) -> Result<(PathBuf, String)> {
    git::locate_with(path, |dir| hg(dir, &["root"]))
}

/// A Mercurial revision as one `--rev=` argument. Mercurial takes negative numbers (`-1` is
/// the tip), and joined to the option a revision cannot be read as another option.
fn hg_rev(rev: &str) -> Result<String> {
    let rev = rev.trim();
    if rev.is_empty() {
        anyhow::bail!("invalid revision: {rev}");
    }
    Ok(format!("--rev={rev}"))
}

/// Error codes of `svn` for a path that is not in the revision asked for.
const SVN_NOT_THERE: &[&str] = &["E195012", "E160013", "E170000"];

/// A Git error for a path that no working copy was found for names every system looked for.
fn no_working_copy<T>(path: &Path, result: Result<T>) -> Result<T> {
    if result.is_ok() || !path.exists() || Vcs::detect(path).is_some() {
        return result;
    }
    result.with_context(|| {
        format!(
            "{} is not in a Git, Subversion or Mercurial working copy",
            path.display()
        )
    })
}

/// `rev` trimmed, or the default revision of the system holding `path` when it is empty: the
/// revision that `show_file` and `export_folder` compare with.
pub fn rev_or_default<'a>(path: &Path, rev: &'a str) -> &'a str {
    Vcs::of(path).rev_or_default(rev)
}

/// The revisions and subjects of `svn log` output: entries are a dashed line, a header
/// `r12 | author | date | 2 lines`, a blank line and that many lines of message. The count is
/// followed, so a message line that looks like a header is not taken for one.
fn parse_svn_log(log: &str) -> Vec<(String, String)> {
    const SEPARATOR: &str =
        "------------------------------------------------------------------------";
    let lines: Vec<&str> = log.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let fields: Vec<&str> = line.split(" | ").collect();
        let rev = fields[0];
        let is_header = fields.len() >= 4
            && rev.len() > 1
            && rev.starts_with('r')
            && rev[1..].bytes().all(|b| b.is_ascii_digit());
        if !is_header {
            continue;
        }
        // the blank line after the header, then the message
        i += 1;
        let count: Option<usize> = fields[fields.len() - 1]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok());
        let end = match count {
            Some(n) => (i + n).min(lines.len()),
            None => lines[i.min(lines.len())..]
                .iter()
                .position(|l| *l == SEPARATOR)
                .map_or(lines.len(), |p| i + p),
        };
        let message = &lines[i.min(end)..end];
        let subject = message.iter().find(|l| !l.trim().is_empty());
        out.push((
            rev.to_string(),
            subject.map_or("", |l| l.trim()).to_string(),
        ));
        i = end;
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
r3 | ann | 2026-11-04 10:00:00 +0000 (Wed, 04 Nov 2026) | 3 lines

Fix the parser
r9 | not | a | header
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
    fn svn_gets_a_utf8_character_set_and_keeps_its_language() {
        let s = |v: &str| Some(v.to_string());
        assert!(utf8_locale_env(s("de_DE.UTF-8"), None, None, "UTF-8").is_empty());
        assert!(utf8_locale_env(None, None, s("de_DE.utf8"), "UTF-8").is_empty());
        assert_eq!(
            utf8_locale_env(s("de_DE.ISO8859-1"), s("C"), None, "C.UTF-8"),
            vec![
                ("LC_ALL", None),
                ("LANG", s("de_DE.ISO8859-1")),
                ("LC_MESSAGES", s("de_DE.ISO8859-1")),
                ("LC_CTYPE", s("C.UTF-8")),
            ]
        );
        assert_eq!(
            utf8_locale_env(s(""), None, s("fr_FR"), "UTF-8"),
            vec![("LC_CTYPE", s("UTF-8"))]
        );
        assert_eq!(
            utf8_locale_env(None, None, None, "UTF-8"),
            vec![("LC_CTYPE", s("UTF-8"))]
        );
    }

    #[test]
    fn a_utf8_locale_is_picked_from_those_the_system_has() {
        let listed = ["C", "POSIX", "de_DE.utf8", "C.utf8"];
        assert_eq!(
            pick_utf8_locale(listed.into_iter()),
            Some("C.utf8".to_string())
        );
        let listed = ["C", "POSIX", "de_DE.utf8"];
        assert_eq!(
            pick_utf8_locale(listed.into_iter()),
            Some("de_DE.utf8".to_string())
        );
        assert_eq!(pick_utf8_locale(["C", "POSIX"].into_iter()), None);
    }

    #[test]
    fn mercurial_revisions_are_one_argument_and_may_be_negative() {
        assert_eq!(hg_rev("-1").unwrap(), "--rev=-1");
        assert_eq!(hg_rev(" .^ ").unwrap(), "--rev=.^");
        assert_eq!(hg_rev("--config=x").unwrap(), "--rev=--config=x");
        assert!(hg_rev(" ").is_err());
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
        std::fs::create_dir_all(d.path().join("hg/.hg")).unwrap();
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
        assert_eq!(Vcs::detect(&d.path().join("hg")), Some(Vcs::Mercurial));
        assert_eq!(Vcs::of(d.path()), Vcs::Git);
    }
}
