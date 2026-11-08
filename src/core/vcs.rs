//! Files and folders as they were in a revision of the version control system that holds
//! them: Git (through `git.rs`), Subversion (the `svn` command) or Mercurial (`hg`).
//!
//! The working copy decides: the nearest folder above the path with a `.git`, `.svn` or `.hg`
//! entry. Git and Mercurial revisions are read from the local repository. Subversion reads
//! `BASE` from the working copy; other revisions, and the list of recent ones, come from the
//! repository server the working copy names, as `svn` itself would.
//!
//! Reading a revision honors the repository's own configuration, as Git's filters are honored.
//! The list of recent revisions, read while a path is being typed, does not: Mercurial is told
//! to skip the repository's `.hg/hgrc`, whose hooks and extensions could run commands.
//!
//! On Windows the `svn` and `hg` programs take arguments in the system ANSI code page, so a
//! name they cannot spell is never put on their command line. Subversion is started in the
//! folder instead, which it is told whole: a folder is `.` there, and a file is read through
//! its repository URL (ASCII, with the name percent-encoded). Mercurial is left on that code
//! page, which is as far as its own Windows build can see file names.

use super::{archive, git};
use anyhow::{bail, Context, Result};
use sha2::Digest;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetACP() -> u32;
    fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, buffer: u32) -> u32;
}

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

fn svn(dir: &Path, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>> {
    git::run_cancellable(svn_command(dir, args), cancel)
}

fn svn_command(dir: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = git::tool("svn", dir);
    utf8_locale(&mut cmd);
    // ssh would ask for a password on the terminal the program was started from, if any
    if std::env::var_os("SVN_SSH").is_none() {
        cmd.env("SVN_SSH", "ssh -o BatchMode=yes");
    }
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

/// Where `svn` is asked to read `path`. The Windows build of `svn` keeps only ASCII arguments,
/// but its working folder is passed whole. An ASCII path is given as it is. A folder that is
/// not ASCII is `.` from inside it. A file that is not ASCII is read through its repository
/// URL, which `svn info` of its folder gives.
enum SvnTarget {
    Local { cwd: PathBuf, path: String },
    Remote(SvnNode),
}

/// A working-copy file `svn` cannot be given, read through its repository URL.
struct SvnNode {
    /// The folder holding the file.
    cwd: PathBuf,
    /// The file's name, as `svn` lists it in `cwd`.
    name: String,
    /// Peg-safe: a literal `@` in the path is `%40`.
    url: String,
    /// The revision the working copy has for this path (`BASE`).
    base: String,
    /// The revision that last changed it (`COMMITTED`).
    committed: String,
    /// The pristine copy of `BASE` for a file, when the working copy has one.
    pristine: Option<PathBuf>,
}

/// `true` when `path` can be an argument of the Windows `svn`: its canonical spelling is
/// ASCII. A failure to look at the path is reported by the local read, not hidden here.
fn svn_path_is_ascii(path: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    git::dunce_canonical(path)
        .ok()
        .and_then(|p| p.to_str().map(|s| s.is_ascii()))
        .unwrap_or(true)
}

fn svn_open(path: &Path, cancel: &AtomicBool) -> Result<SvnTarget> {
    let abs = git::dunce_canonical(path)?;
    if svn_path_is_ascii(&abs) {
        let cwd = abs.parent().context("no parent folder")?.to_path_buf();
        let local = abs
            .to_str()
            .with_context(|| format!("{}: the name is not valid UTF-8", abs.display()))?;
        return Ok(SvnTarget::Local {
            cwd,
            path: local.to_string(),
        });
    }
    if abs.is_dir() {
        return Ok(SvnTarget::Local {
            cwd: abs,
            path: ".".to_string(),
        });
    }
    Ok(SvnTarget::Remote(svn_node(&abs, cancel)?))
}

/// The node of a non-ASCII file `abs`: its URL and the revisions recorded in the working copy,
/// from `svn info` of `.` in the folder holding it.
fn svn_node(abs: &Path, cancel: &AtomicBool) -> Result<SvnNode> {
    let cwd = abs.parent().context("no parent folder")?.to_path_buf();
    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{}: the name is not valid UTF-8", abs.display()))?
        .to_string();
    let xml = svn(
        &cwd,
        &["info", "--xml", "--depth", "immediates", "--", ".@"],
        cancel,
    )?;
    let mut node =
        parse_svn_info(&tool_text(&xml), &name).with_context(|| abs.display().to_string())?;
    node.cwd = cwd;
    node.name = name;
    Ok(node)
}

/// `rev` as `svn cat` and `svn export` take it. `BASE`, `COMMITTED` and `PREV` are working-copy
/// words, so on a repository URL they become the number the working copy recorded.
fn svn_number(rev: &str, node: &SvnNode) -> Result<String> {
    let rev = svn_rev(rev)?;
    match rev.as_str() {
        "BASE" => Ok(node.base.clone()),
        "COMMITTED" => Ok(node.committed.clone()),
        "PREV" => previous_rev(&node.committed),
        _ => Ok(rev),
    }
}

fn previous_rev(committed: &str) -> Result<String> {
    let n: u64 = committed
        .parse()
        .with_context(|| format!("no previous revision ({committed})"))?;
    if n == 0 {
        bail!("no previous revision");
    }
    Ok((n - 1).to_string())
}

/// A repository URL that can take a peg: `svn` reads the last `@` as one, and prints a literal
/// `@` in a path unencoded.
fn peg_safe_url(raw: &str) -> Result<String> {
    let raw = raw.trim().replace('\\', "/");
    let prepared = encode_at_in_path(&raw);
    let url = url::Url::parse(&prepared).with_context(|| format!("svn url {raw}"))?;
    Ok(url.to_string())
}

fn encode_at_in_path(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return raw.replace('@', "%40");
    };
    if scheme == "file" {
        return raw.replace('@', "%40");
    }
    match rest.split_once('/') {
        Some((authority, path)) => {
            format!("{scheme}://{authority}/{}", path.replace('@', "%40"))
        }
        None => raw.to_string(),
    }
}

/// File or folder `path` as it was in revision `rev` (the default revision when `rev` is
/// empty), in a temporary folder. Returns the temporary folder (deleted when dropped) and the
/// copy of `path` inside it. Setting `cancel` stops the command reading it, one waiting on a
/// server for example, and fails.
pub fn copy_of(
    path: &Path,
    rev: &str,
    cancel: &AtomicBool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    if path.is_dir() {
        return export_folder(path, rev, cancel);
    }
    let temp = tempfile::Builder::new()
        .prefix("oxidemerge-rev-")
        .tempdir()?;
    let copy = temp.path().join(path.file_name().unwrap_or_default());
    save_file(path, rev, &copy, cancel)?;
    Ok((temp, copy))
}

/// Writes file `path` as it was in revision `rev` to the new file `dest`.
fn save_file(path: &Path, rev: &str, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let vcs = Vcs::of(path);
    let rev = vcs.rev_or_default(rev);
    match vcs {
        Vcs::Git => no_working_copy(path, git::save_file(path, rev, dest, cancel)),
        Vcs::Subversion => svn_save(path, rev, dest, cancel),
        Vcs::Mercurial => {
            let rev = hg_rev(rev)?;
            let (root, rel) = hg_locate(path, Repo::Trusted)?;
            let pattern = format!("path:{rel}");
            let cmd = hg_command(
                &root,
                &["cat", "--decode", &rev, "--", &pattern],
                Repo::Trusted,
            );
            git::run_to_file(cmd, dest, cancel)
        }
    }
}

/// The most a Subversion or Mercurial revision export may contain. See
/// [`bound_exported_folder`]; Git's own export is bounded as it is written instead (see
/// `git::MAX_EXPORT_BYTES`).
const MAX_EXPORTED_BYTES: u64 = 8 << 30;
/// The most files a Subversion or Mercurial revision export may contain. See
/// [`MAX_EXPORTED_BYTES`].
const MAX_EXPORTED_FILES: u64 = 500_000;

/// Refuses an export that turned out larger than [`MAX_EXPORTED_BYTES`] or
/// [`MAX_EXPORTED_FILES`], once `svn export` or `hg archive` has already written `dir`: unlike
/// Git's own tree, neither command reports what a revision contains before writing it, so this
/// is a backstop rather than a preventive check. The caller's temporary folder is still deleted
/// as usual once this returns an error, so a revision that got this large does not linger on
/// disk, even though - unlike the SFTP fetch or an archive extraction - it was briefly written
/// in full first. Call this after [`archive::remove_links_outside`], so a link escaping `dir`
/// is not read, and not counted, either.
fn bound_exported_folder(dir: &Path) -> Result<()> {
    bound_with_limits(dir, MAX_EXPORTED_BYTES, MAX_EXPORTED_FILES)
}

/// What [`bound_exported_folder`] checks `dir` against, with the limits as arguments so a test
/// can pass small ones without actually writing gigabytes or hundreds of thousands of files.
fn bound_with_limits(dir: &Path, max_bytes: u64, max_files: u64) -> Result<()> {
    let (mut bytes, mut files) = (0u64, 0u64);
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.context("reading the exported folder")?;
        if !entry.file_type().is_file() {
            continue;
        }
        files += 1;
        bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        if bytes > max_bytes || files > max_files {
            bail!(
                "stopped after {} GB or {} files; export a smaller revision or folder",
                max_bytes >> 30,
                max_files
            );
        }
    }
    Ok(())
}

/// Folder `path` as it was in revision `rev`, in a temporary folder. Returns the temporary
/// folder (deleted when dropped) and the copy of `path` inside it.
fn export_folder(
    path: &Path,
    rev: &str,
    cancel: &AtomicBool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let vcs = Vcs::of(path);
    let rev = vcs.rev_or_default(rev);
    match vcs {
        Vcs::Git => no_working_copy(path, git::export_folder(path, rev, cancel)),
        Vcs::Subversion => svn_export(path, rev, cancel),
        Vcs::Mercurial => {
            let rev = hg_rev(rev)?;
            let (root, rel) = hg_locate(path, Repo::Trusted)?;
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
            let dest_arg = argv_path(&dest).display().to_string();
            args.extend(["--", dest_arg.as_str()]);
            match git::run_cancellable(hg_command(&root, &args, Repo::Trusted), cancel) {
                Ok(_) => {}
                // the folder had no files in that revision: it compares as empty
                Err(e) if e.to_string().contains("no files match") => {}
                Err(e) => return Err(e),
            }
            let copy = dest.join(&rel);
            std::fs::create_dir_all(&copy)?;
            // Mercurial checks a tracked symlink out as a real one; one crafted to point
            // outside `dest` (the whole archive, of which `copy` may be only a subfolder)
            // could otherwise be opened and shown as if it were part of the revision.
            archive::remove_links_outside(&dest)?;
            bound_exported_folder(&dest)?;
            Ok((temp, copy))
        }
    }
}

/// Recent revisions that changed `path`, newest first: (revision, first line of the message).
/// Setting `cancel` stops a log that is still running (one waiting on a server, or walking a
/// long history), and the list comes back empty.
pub fn recent_revisions(path: &Path, n: usize, cancel: &AtomicBool) -> Vec<(String, String)> {
    match Vcs::detect(path) {
        Some(Vcs::Subversion) => {
            let Ok(target) = svn_open(path, cancel) else {
                return Vec::new();
            };
            let (cwd, pegged) = match &target {
                SvnTarget::Local { cwd, path } => (cwd.as_path(), format!("{path}@")),
                SvnTarget::Remote(node) => {
                    (node.cwd.as_path(), format!("{}@{}", node.url, node.base))
                }
            };
            let limit = n.to_string();
            git::run_cancellable(
                svn_command(cwd, &["log", "--xml", "-l", &limit, "--", &pegged]),
                cancel,
            )
            .map(|out| parse_svn_log_xml(&tool_text(&out)))
            .unwrap_or_default()
        }
        Some(Vcs::Mercurial) => {
            let Ok((root, rel)) = hg_locate(path, Repo::Untrusted) else {
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
            git::run_cancellable(hg_command(&root, &args, Repo::Untrusted), cancel)
                .map(|out| {
                    tool_text(&out)
                        .lines()
                        .filter_map(|l| l.split_once('\t'))
                        .map(|(h, s)| (h.to_string(), s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => git::recent_commits(path, n, cancel),
    }
}

/// Whether Mercurial may read the repository's own `.hg/hgrc`: only when the user asked for a
/// revision, never for what is read on its own while a path is typed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Repo {
    Trusted,
    Untrusted,
}

/// `hg` with the plain, untranslated behavior meant for scripts, and no prompts.
///
/// On Windows Mercurial decodes the command line in the ANSI code page and then as
/// `HGENCODING`. Forcing UTF-8 there rejects every name the code page can spell, and a name
/// the code page cannot spell is one Mercurial itself cannot open. The system encoding is
/// used, even when the environment sets another; what `hg` prints is decoded from it when it
/// is not UTF-8.
fn hg_command(dir: &Path, args: &[&str], repo: Repo) -> std::process::Command {
    let mut cmd = git::tool("hg", dir);
    cmd.env("HGPLAIN", "1");
    if cfg!(windows) {
        cmd.env_remove("HGENCODING");
    } else {
        cmd.env("HGENCODING", "utf-8");
    }
    if repo == Repo::Untrusted {
        cmd.env("HGRCSKIPREPO", "1");
    }
    cmd.arg("-y").args(args);
    cmd
}

/// The repository holding `path` and the path relative to its top folder, with `/`
/// separators (empty for the top folder itself).
fn hg_locate(path: &Path, repo: Repo) -> Result<(PathBuf, String)> {
    git::locate_with(path, |dir| {
        let out = git::run(hg_command(dir, &["root"], repo), None)?;
        Ok(tool_text(&out).into_bytes())
    })
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
const SVN_NOT_THERE: &[&str] = &["E195012", "E160013"];
/// The error code of `svn` for a URL that does not exist: a path not in the revision, or a
/// repository the server no longer has.
const SVN_NO_URL: &str = "E170000";

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

/// Writes file `path` as Subversion has it in `rev`.
fn svn_save(path: &Path, rev: &str, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    match svn_open(path, cancel)? {
        SvnTarget::Local { cwd, path } => {
            let rev = svn_rev(rev)?;
            let target = format!("{path}@");
            let cmd = svn_command(&cwd, &["cat", "-r", &rev, "--", &target]);
            git::run_to_file(cmd, dest, cancel)
        }
        SvnTarget::Remote(node) => {
            let rev = svn_number(rev, &node)?;
            // BASE is the pristine file in the working copy, so it is read with the server
            // unreachable, translated as `svn cat` would. Keywords need the repository.
            if rev == node.base {
                if let Some(src) = &node.pristine {
                    let props = svn_props(&node, cancel).unwrap_or_default();
                    if props.keywords.is_none() {
                        let bytes =
                            std::fs::read(src).with_context(|| src.display().to_string())?;
                        let bytes = translate_eol(bytes, props.eol_style.as_deref());
                        std::fs::write(dest, bytes).with_context(|| dest.display().to_string())?;
                        return Ok(());
                    }
                }
            }
            // Pegged at BASE, as a working-copy path is, so the file is followed across renames.
            let target = format!("{}@{}", node.url, node.base);
            let cmd = svn_command(&node.cwd, &["cat", "-r", &rev, "--", &target]);
            git::run_to_file(cmd, dest, cancel)
        }
    }
}

/// The properties of a file that change what `svn cat` prints.
#[derive(Default, Debug, PartialEq)]
struct SvnProps {
    keywords: Option<String>,
    eol_style: Option<String>,
}

fn svn_props(node: &SvnNode, cancel: &AtomicBool) -> Result<SvnProps> {
    let xml = svn(
        &node.cwd,
        &[
            "proplist",
            "--xml",
            "-v",
            "--depth",
            "immediates",
            "--",
            ".@",
        ],
        cancel,
    )?;
    Ok(parse_svn_props(&tool_text(&xml), &node.name))
}

/// `svn:keywords` and `svn:eol-style` of `name` in `svn proplist --xml -v` output.
fn parse_svn_props(xml: &str, name: &str) -> SvnProps {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
    reader.config_mut().expand_empty_elements = true;
    let mut props = SvnProps::default();
    let mut in_target = false;
    let mut property: Option<String> = None;
    let mut value = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                "target" => {
                    in_target = svn_entry_matches(&xml_attr(&e, "path").unwrap_or_default(), name)
                }
                "property" if in_target => {
                    property = xml_attr(&e, "name");
                    value.clear();
                }
                _ => {}
            },
            Ok(Event::Text(t)) if property.is_some() => value.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) if property.is_some() => {
                if let Some(s) = quick_xml::escape::resolve_xml_entity(&r) {
                    value.push_str(s);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                "property" => match property.take().as_deref() {
                    Some("svn:keywords") => props.keywords = Some(value.trim().to_string()),
                    Some("svn:eol-style") => props.eol_style = Some(value.trim().to_string()),
                    _ => {}
                },
                "target" => in_target = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    props
}

/// A pristine file, stored with `\n` line ends when it has `svn:eol-style`, with the line ends
/// `svn cat` gives it.
fn translate_eol(bytes: Vec<u8>, eol_style: Option<&str>) -> Vec<u8> {
    let eol: &[u8] = match eol_style {
        Some("native") if cfg!(windows) => b"\r\n",
        Some("CRLF") => b"\r\n",
        Some("CR") => b"\r",
        _ => return bytes,
    };
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    for &b in &bytes {
        if b == b'\n' {
            out.extend_from_slice(eol);
        } else {
            out.push(b);
        }
    }
    out
}

/// Folder `path` as Subversion has it in `rev`, in a temporary folder.
fn svn_export(path: &Path, rev: &str, cancel: &AtomicBool) -> Result<(tempfile::TempDir, PathBuf)> {
    let SvnTarget::Local { cwd, path: source } = svn_open(path, cancel)? else {
        bail!("{} is not a folder", path.display());
    };
    let temp = tempfile::Builder::new()
        .prefix("oxidemerge-svn-")
        .tempdir()?;
    let name = path
        .canonicalize()?
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| "export".into());
    let copy = temp.path().join(&name);
    // `svn` is given an ASCII destination, then the folder is renamed to its own name.
    let staging = if copy.to_str().is_some_and(|s| s.is_ascii()) {
        copy.clone()
    } else {
        temp.path().join("export")
    };
    let dest = format!("{}@", argv_path(&staging).display());
    let rev = svn_rev(rev)?;
    let pegged = format!("{source}@");
    let export = &["export", "-q", "-r", &rev, "--", &pegged, &dest];
    match svn(&cwd, export, cancel) {
        Ok(_) => {}
        // the folder was not there in that revision: it compares as empty, as in Git
        // and Mercurial (the codes are the same in every language)
        Err(e) if SVN_NOT_THERE.iter().any(|c| e.to_string().contains(c)) => {}
        // also a repository the server no longer has: the folder is missing only if
        // the folder holding it is there
        Err(e) if e.to_string().contains(SVN_NO_URL) => {
            let parent = svn_parent(&source).map_err(|_| anyhow::anyhow!("{e}"))?;
            let info = &["info", "-r", &rev, "--depth", "empty", "--", &parent];
            if cancel.load(std::sync::atomic::Ordering::Relaxed) || svn(&cwd, info, cancel).is_err()
            {
                return Err(e);
            }
        }
        Err(e) => return Err(e),
    }
    if staging != copy && staging.exists() {
        std::fs::rename(&staging, &copy)?;
    }
    std::fs::create_dir_all(&copy)?;
    // Subversion's svn:special property checks a symlink out as a real one; one
    // crafted to point outside `copy` could otherwise be opened and shown as if it
    // were part of the revision.
    archive::remove_links_outside(&copy)?;
    bound_exported_folder(&copy)?;
    Ok((temp, copy))
}

/// The folder that holds the exported path, as an `svn info` target, so a missing path can be
/// told from a repository the server no longer has.
fn svn_parent(source: &str) -> Result<String> {
    if source == "." {
        return Ok("..@".to_string());
    }
    let parent = Path::new(source).parent().context("no parent folder")?;
    Ok(format!("{}@", parent.display()))
}

/// The entry of `svn info --xml .` whose path is `name`, a file in that folder.
fn parse_svn_info(xml: &str, name: &str) -> Result<SvnNode> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut matched = false;
    let mut found = false;
    let mut text = String::new();
    let mut url = String::new();
    let mut base = String::new();
    let mut committed = String::new();
    let mut kind = String::new();
    let mut checksum = String::new();
    let mut wcroot = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                text.clear();
                let element = e.name().as_ref().to_string();
                if element == "entry" {
                    let path = xml_attr(&e, "path").unwrap_or_default();
                    matched = svn_entry_matches(&path, name);
                    if matched {
                        found = true;
                        base = xml_attr(&e, "revision").unwrap_or_default();
                        kind = xml_attr(&e, "kind").unwrap_or_default();
                        url.clear();
                        committed.clear();
                        checksum.clear();
                        wcroot.clear();
                    }
                } else if matched && element == "commit" {
                    if let Some(rev) = xml_attr(&e, "revision") {
                        committed = rev;
                    }
                }
            }
            Ok(Event::Text(t)) if matched => text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) if matched => {
                if let Some(s) = quick_xml::escape::resolve_xml_entity(&r) {
                    text.push_str(s);
                }
            }
            Ok(Event::End(e)) => {
                let element = e.name().as_ref().to_string();
                if matched {
                    let value = std::mem::take(&mut text);
                    let value = value.trim();
                    match element.as_str() {
                        "url" => url = value.to_string(),
                        "checksum" => checksum = value.to_string(),
                        "wcroot-abspath" => wcroot = value.to_string(),
                        "entry" => matched = false,
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("svn info: {e}"),
            _ => {}
        }
    }
    if !found || url.is_empty() || base.is_empty() {
        bail!("not in the working copy");
    }
    if committed.is_empty() {
        committed = base.clone();
    }
    let pristine = (kind == "file")
        .then(|| pristine_path(Path::new(&wcroot), &checksum))
        .flatten();
    Ok(SvnNode {
        cwd: PathBuf::new(),
        name: String::new(),
        url: peg_safe_url(&url)?,
        base,
        committed,
        pristine,
    })
}

/// `svn` lists the children of `.` by their names alone, so only the file itself matches, never
/// one of the same name in a folder below.
fn svn_entry_matches(entry: &str, name: &str) -> bool {
    entry == name
}

/// `.svn/pristine/ab/<checksum>.svn-base` for a file's base, when that file is in the working
/// copy. `checksum` is the hex `svn info` prints, with or without a `$sha1$` prefix.
fn pristine_path(wcroot: &Path, checksum: &str) -> Option<PathBuf> {
    let hex = checksum.trim().rsplit('$').next()?.trim();
    if hex.len() < 2 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let path = wcroot
        .join(".svn")
        .join("pristine")
        .join(&hex[..2])
        .join(format!("{hex}.svn-base"));
    path.is_file().then_some(path)
}

/// The revisions and subjects of `svn log --xml`: `r` and the revision number, then the first
/// line of the message that is not blank. An empty message stays empty.
fn parse_svn_log_xml(xml: &str) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = true;
    let mut out = Vec::new();
    let mut rev = String::new();
    let mut in_msg = false;
    let mut msg = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                if name == "logentry" {
                    rev = xml_attr(&e, "revision").unwrap_or_default();
                    msg.clear();
                } else if name == "msg" {
                    in_msg = true;
                    msg.clear();
                }
            }
            Ok(Event::Text(t)) if in_msg => msg.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) if in_msg => {
                if let Some(s) = quick_xml::escape::resolve_xml_entity(&r) {
                    msg.push_str(s);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                "msg" => in_msg = false,
                "logentry" => {
                    let subject = msg
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .map(|line| line.trim())
                        .unwrap_or("");
                    if !rev.is_empty() {
                        out.push((format!("r{rev}"), subject.to_string()));
                    }
                    rev.clear();
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

fn xml_attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        if a.key.as_ref() != name {
            return None;
        }
        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .map(|v| v.into_owned())
    })
}

/// What `svn`, `hg` or `git` printed. UTF-8 is kept (Subversion's XML is UTF-8 even on
/// Windows). Anything else is the ANSI code page, which is how Windows Mercurial prints a name
/// and Windows Subversion its messages.
pub(super) fn tool_text(bytes: &[u8]) -> String {
    #[cfg(windows)]
    let ansi = ansi_encoding();
    #[cfg(not(windows))]
    let ansi = encoding_rs::UTF_8;
    decode_tool_text(bytes, ansi)
}

fn decode_tool_text(bytes: &[u8], ansi: &'static encoding_rs::Encoding) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    ansi.decode(bytes).0.into_owned()
}

#[cfg(windows)]
fn ansi_encoding() -> &'static encoding_rs::Encoding {
    // The code page `hg` uses for arguments and output: GetACP, not the console's.
    match unsafe { GetACP() } {
        65001 => encoding_rs::UTF_8,
        950 | 951 => encoding_rs::BIG5,
        936 => encoding_rs::GBK,
        932 => encoding_rs::SHIFT_JIS,
        949 => encoding_rs::EUC_KR,
        874 => encoding_rs::WINDOWS_874,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        _ => encoding_rs::UTF_8,
    }
}

/// A path the Windows `svn` and `hg` can be given. A short (8.3) name is used when the real
/// one is not ASCII and Windows still has one.
fn argv_path(path: &Path) -> PathBuf {
    if !cfg!(windows) || path.to_str().is_some_and(|text| text.is_ascii()) {
        return path.to_path_buf();
    }
    short_ascii_path(path).unwrap_or_else(|| path.to_path_buf())
}

#[cfg(windows)]
fn short_ascii_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    buf.truncate(n as usize);
    let short = PathBuf::from(std::ffi::OsString::from_wide(&buf));
    short
        .to_str()
        .is_some_and(|text| text.is_ascii())
        .then_some(short)
}

#[cfg(not(windows))]
fn short_ascii_path(_path: &Path) -> Option<PathBuf> {
    None
}

/// A merge started by a version-control tool that passes only files, in its own order.
pub struct ExternalMerge {
    /// Left, middle and right, in the order the panes show them.
    pub paths: Vec<PathBuf>,
    pub labels: Vec<String>,
    /// Which of those panes cannot be edited.
    pub read_only: Vec<bool>,
    /// Where the merged result is saved.
    pub output: PathBuf,
}

/// Subversion's `merge-tool-cmd` passes base, theirs, mine, the merged result and the working
/// copy, the last two being the same file. Perforce's `P4MERGE` passes base, theirs, yours and
/// the result, which may not exist yet; all but yours are p4's temporary files. Recognized only
/// when the command line has no other option, so a compare that sets one is left as it was
/// written. Other files, such as a shell pattern that matched four or five, are not a merge:
/// saving would overwrite the last of them.
pub fn external_merge(paths: &[PathBuf]) -> Option<ExternalMerge> {
    let file = |p: &Path| p.is_file();
    match paths {
        [base, theirs, mine, merged, wc]
            if file(base)
                && file(theirs)
                && file(mine)
                && same_file(merged, wc)
                && distinct(&[base, theirs, mine, merged]) =>
        {
            Some(ExternalMerge {
                paths: vec![mine.clone(), base.clone(), theirs.clone()],
                labels: vec!["Mine".into(), "Base".into(), "Theirs".into()],
                read_only: vec![true, false, true],
                output: merged.clone(),
            })
        }
        [base, theirs, yours, result]
            if file(base)
                && file(theirs)
                && file(yours)
                && !result.is_dir()
                && [base, theirs, result].iter().all(|p| p4_temp_name(p))
                && distinct(&[base, theirs, yours, result]) =>
        {
            Some(ExternalMerge {
                paths: vec![yours.clone(), base.clone(), theirs.clone()],
                labels: vec!["Yours".into(), "Base".into(), "Theirs".into()],
                read_only: vec![true, false, true],
                output: result.clone(),
            })
        }
        _ => None,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (git::dunce_canonical(a), git::dunce_canonical(b)) {
        (Ok(a), Ok(b)) => a.is_file() && a == b,
        _ => false,
    }
}

/// No two of `paths` name the same file. A path that does not exist yet is taken as written.
fn distinct(paths: &[&PathBuf]) -> bool {
    let keys: Vec<PathBuf> = paths
        .iter()
        .map(|p| git::dunce_canonical(p).unwrap_or_else(|_| p.to_path_buf()))
        .collect();
    keys.iter()
        .enumerate()
        .all(|(i, a)| keys[i + 1..].iter().all(|b| a != b))
}

/// A name p4 gives the files of a merge: `t1234t5678t90.tmp` on Windows, `tmp.1234.…` on
/// macOS and Linux.
fn p4_temp_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(stem) = name.strip_suffix(".tmp") {
        let mut parts = stem.split('t');
        return parts.next() == Some("") && parts.clone().count() == 3 && parts.all(digits);
    }
    name.strip_prefix("tmp.")
        .and_then(|rest| rest.split('.').next())
        .is_some_and(digits)
}

/// SHA-256 of a file, or `None` when it is not there. A missing result is distinct from an
/// empty one, so creating it counts as a save.
pub fn file_digest(path: &Path) -> Option<[u8; 32]> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svn_log_entries_give_their_revision_and_first_message_line() {
        let log = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<log>
<logentry revision=\"3\">
<msg>Fix the parser
r9 | not | a | header</msg>
</logentry>
<logentry revision=\"2\">
<msg></msg>
</logentry>
<logentry revision=\"1\">
<msg>First | with a bar</msg>
</logentry>
</log>
";
        assert_eq!(
            parse_svn_log_xml(log),
            vec![
                ("r3".into(), "Fix the parser".into()),
                ("r2".into(), String::new()),
                ("r1".into(), "First | with a bar".into()),
            ]
        );
    }

    #[test]
    fn a_repository_url_keeps_a_peg_and_a_name_svn_cannot_spell() {
        assert_eq!(
            peg_safe_url("file:///C:/repo/a%20@b/é.txt").unwrap(),
            "file:///C:/repo/a%20%40b/%C3%A9.txt"
        );
        assert_eq!(
            peg_safe_url("svn+ssh://user@host/a@b").unwrap(),
            "svn+ssh://user@host/a%40b"
        );
    }

    #[test]
    fn svn_info_of_a_child_gives_its_url_and_pristine_base() {
        let dir = tempfile::tempdir().unwrap();
        let wc = dir.path();
        let hex = "6fcf9dfbd479ed82697fee719b9f8c610a11ff2a";
        let pristine = wc
            .join(".svn")
            .join("pristine")
            .join("6f")
            .join(format!("{hex}.svn-base"));
        std::fs::create_dir_all(pristine.parent().unwrap()).unwrap();
        std::fs::write(&pristine, "x\n").unwrap();
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<info>
<entry path="." revision="0" kind="dir">
<url>file:///C:/repo/a%20@b</url>
<commit revision="0"/>
</entry>
<entry path="é.txt" revision="1" kind="file">
<url>file:///C:/repo/a%20@b/é.txt</url>
<wc-info>
<wcroot-abspath>{}</wcroot-abspath>
<checksum>{hex}</checksum>
</wc-info>
<commit revision="1"/>
</entry>
<entry path="sub\é.txt" revision="7" kind="file">
<url>file:///C:/repo/a%20@b/sub/é.txt</url>
<commit revision="7"/>
</entry>
</info>"#,
            wc.display()
        );
        // Only the file itself, never one of the same name in a folder below.
        let node = parse_svn_info(&xml, "é.txt").unwrap();
        assert_eq!(node.base, "1");
        assert_eq!(node.committed, "1");
        assert_eq!(node.url, "file:///C:/repo/a%20%40b/%C3%A9.txt");
        assert_eq!(node.pristine.as_deref(), Some(pristine.as_path()));
        assert_eq!(previous_rev("1").unwrap(), "0");
        assert!(previous_rev("0").is_err());
    }

    #[test]
    fn a_pristine_base_gets_the_line_ends_svn_cat_gives_it_and_keywords_are_seen() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<properties>
<target path="other.txt">
<property name="svn:keywords">Id</property>
</target>
<target path="é.txt">
<property name="svn:eol-style">CRLF</property>
<property name="svn:mime-type">text/plain</property>
</target>
</properties>"#;
        let props = parse_svn_props(xml, "é.txt");
        assert_eq!(props.keywords, None);
        assert_eq!(props.eol_style.as_deref(), Some("CRLF"));
        assert_eq!(
            parse_svn_props(xml, "other.txt").keywords.as_deref(),
            Some("Id")
        );
        assert_eq!(parse_svn_props(xml, "missing.txt"), SvnProps::default());

        let lf = b"a\nb\n".to_vec();
        assert_eq!(translate_eol(lf.clone(), None), lf);
        assert_eq!(translate_eol(lf.clone(), Some("LF")), lf);
        assert_eq!(translate_eol(lf.clone(), Some("CRLF")), b"a\r\nb\r\n");
        assert_eq!(translate_eol(lf.clone(), Some("CR")), b"a\rb\r");
        let native: &[u8] = if cfg!(windows) {
            b"a\r\nb\r\n"
        } else {
            b"a\nb\n"
        };
        assert_eq!(translate_eol(lf, Some("native")), native);
    }

    #[test]
    fn only_the_names_p4_gives_merge_files_count_as_p4_temporary_files() {
        for name in [
            "t11292t37660t86.tmp",
            "t1t2t3.tmp",
            "tmp.63484.0x20d2d0f40.90",
        ] {
            assert!(p4_temp_name(Path::new(name)), "{name}");
        }
        for name in [
            "report.tmp",
            "t1t2.tmp",
            "t1t2t3t4.tmp",
            "tat2t3.tmp",
            "T1T2T3.TMP",
            "tmp.txt",
            "tmp..1",
        ] {
            assert!(!p4_temp_name(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn tool_text_keeps_utf8_and_decodes_the_ansi_code_page() {
        assert_eq!(
            decode_tool_text("修正".as_bytes(), encoding_rs::BIG5),
            "修正"
        );
        let (bytes, _, _) = encoding_rs::BIG5.encode("修正");
        assert!(std::str::from_utf8(&bytes).is_err());
        assert_eq!(decode_tool_text(&bytes, encoding_rs::BIG5), "修正");
    }

    #[test]
    fn external_merge_reads_subversion_and_perforce_orders() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, name).unwrap();
            path
        };
        // As `svn update --accept launch` passes them: the merged file is the working file.
        let base = file("g.txt.r1");
        let theirs = file("g.txt.r2");
        let mine = file("g.txt.mine");
        let merged = file("g.txt");
        let merge = external_merge(&[
            base.clone(),
            theirs.clone(),
            mine.clone(),
            merged.clone(),
            merged.clone(),
        ])
        .unwrap();
        assert_eq!(
            merge.paths,
            vec![mine.clone(), base.clone(), theirs.clone()]
        );
        assert_eq!(merge.labels, ["Mine", "Base", "Theirs"]);
        assert_eq!(merge.read_only, [true, false, true]);
        assert_eq!(merge.output, merged);
        // Five different files, as `oxidemerge *.txt` could pass, are not a merge.
        let other = file("other.txt");
        assert!(external_merge(&[base, theirs, mine, merged.clone(), other.clone()]).is_none());

        // As `p4 resolve` passes them: p4's temporary files around the workspace file.
        let p4_base = file("t11292t37660t86.tmp");
        let p4_theirs = file("tmp.63484.0x20d2d0f40.90");
        let yours = file("f.txt");
        let result = dir.path().join("t11292t37660t158.tmp");
        let merge = external_merge(&[
            p4_base.clone(),
            p4_theirs.clone(),
            yours.clone(),
            result.clone(),
        ])
        .unwrap();
        assert_eq!(merge.paths, vec![yours.clone(), p4_base.clone(), p4_theirs]);
        assert_eq!(merge.labels, ["Yours", "Base", "Theirs"]);
        assert_eq!(merge.output, result);
        assert!(file_digest(&merge.output).is_none());
        std::fs::write(&merge.output, b"").unwrap();
        assert!(file_digest(&merge.output).is_some());
        // Four ordinary files are not, and neither is a result that is one of the inputs.
        assert!(
            external_merge(&[merged.clone(), other.clone(), yours.clone(), file("x.txt")])
                .is_none()
        );
        assert!(external_merge(&[p4_base.clone(), result.clone(), yours, p4_base]).is_none());

        let dirs: Vec<_> = (0..4)
            .map(|i| {
                let path = dir.path().join(format!("dir{i}"));
                std::fs::create_dir(&path).unwrap();
                path
            })
            .collect();
        assert!(external_merge(&dirs).is_none());
    }

    #[test]
    fn a_folder_past_the_byte_or_file_budget_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "0123456789").unwrap();
        std::fs::write(dir.path().join("b.txt"), "0123456789").unwrap();

        let err = bound_with_limits(dir.path(), 5, u64::MAX).unwrap_err();
        assert!(err.to_string().contains("GB"), "{err}");

        let err = bound_with_limits(dir.path(), u64::MAX, 1).unwrap_err();
        assert!(err.to_string().contains("1 files"), "{err}");

        bound_with_limits(dir.path(), u64::MAX, u64::MAX).unwrap();
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
    fn the_revision_list_runs_no_hook_of_a_mercurial_repository() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let init = std::process::Command::new("hg")
            .args(["init", "-q"])
            .current_dir(&repo)
            .env("HGRCPATH", "")
            .output();
        if !init.is_ok_and(|o| o.status.success()) {
            eprintln!("Mercurial is not installed; skipped");
            return;
        }
        let marker = d.path().join("ran");
        let hook = if cfg!(windows) {
            format!("type nul > \"{}\"", marker.display())
        } else {
            format!("touch '{}'", marker.display())
        };
        std::fs::write(
            repo.join(".hg/hgrc"),
            format!("[hooks]\npre-root = {hook}\npre-log = {hook}\n"),
        )
        .unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        let never = AtomicBool::new(false);
        assert!(recent_revisions(&repo.join("a.txt"), 5, &never).is_empty());
        assert!(!marker.exists(), "a hook of the repository ran");
    }

    /// Mercurial checks a tracked symlink out as a real one when archiving a revision: left
    /// alone, one pointing outside the exported copy would be opened and read as if it were
    /// part of the revision. `export_folder` must drop it (see `archive::remove_links_outside`).
    #[cfg(unix)]
    #[test]
    fn mercurial_export_drops_a_symlink_pointing_outside_it() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();

        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let hg = |args: &[&str]| {
            std::process::Command::new("hg")
                .args(args)
                .current_dir(&repo)
                .env("HGRCPATH", "")
                .output()
        };
        if !hg(&["init", "-q"]).is_ok_and(|o| o.status.success()) {
            eprintln!("Mercurial is not installed; skipped");
            return;
        }
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), repo.join("escape.txt"))
            .unwrap();
        hg(&["add", "a.txt", "escape.txt"]).unwrap();
        hg(&["commit", "-q", "-m", "first", "-u", "T <t@example.com>"]).unwrap();

        let never = AtomicBool::new(false);
        let (_tmp, copy) = export_folder(&repo, "0", &never).unwrap();
        assert!(copy.join("a.txt").is_file());
        assert!(
            !copy.join("escape.txt").exists(),
            "the escaping link should have been dropped"
        );
    }

    /// Subversion checks a symlink out as a real one (`svn:special`) when exporting a
    /// revision: left alone, one pointing outside the exported copy would be opened and read
    /// as if it were part of the revision. `export_folder` must drop it (see
    /// `archive::remove_links_outside`).
    #[cfg(unix)]
    #[test]
    fn svn_export_drops_a_symlink_pointing_outside_it() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();

        let d = tempfile::tempdir().unwrap();
        let run = |cwd: &Path, program: &str, args: &[&str]| {
            std::process::Command::new(program)
                .args(args)
                .current_dir(cwd)
                .stdin(std::process::Stdio::null())
                .output()
        };
        let repo = d.path().join("repo");
        if run(d.path(), "svnadmin", &["create", repo.to_str().unwrap()]).is_err() {
            eprintln!("Subversion is not installed; skipped");
            return;
        }
        let url = format!(
            "file:///{}",
            repo.to_str()
                .unwrap()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let wc = d.path().join("wc");
        let ok = |cwd: &Path, args: &[&str]| {
            let out = run(cwd, "svn", args).unwrap();
            assert!(
                out.status.success(),
                "svn {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        ok(d.path(), &["checkout", "-q", &url, wc.to_str().unwrap()]);
        std::fs::write(wc.join("a.txt"), "a\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), wc.join("escape.txt"))
            .unwrap();
        ok(&wc, &["add", "-q", "a.txt", "escape.txt"]);
        ok(&wc, &["commit", "-q", "-m", "first"]);

        let never = AtomicBool::new(false);
        let (_tmp, copy) = export_folder(&wc, "1", &never).unwrap();
        assert!(copy.join("a.txt").is_file());
        assert!(
            !copy.join("escape.txt").exists(),
            "the escaping link should have been dropped"
        );
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
