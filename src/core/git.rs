//! Files and folders as they were in a Git revision, read with the `git` command.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

fn command(dir: &Path) -> Command {
    tool("git", dir)
}

/// A command for `program` run in `dir`, with no console window on Windows.
pub(super) fn tool(program: &str, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
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
pub(super) fn run(mut cmd: Command, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{program} could not be started"))?;
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
    finish(&program, out.status, out.stdout, &out.stderr)
}

/// Runs `cmd` like `run` with no input, stopping it (and failing) as soon as `cancel` is set, so
/// a command waiting on a server that does not answer can be given up.
pub(super) fn run_cancellable(cmd: Command, cancel: &AtomicBool) -> Result<Vec<u8>> {
    run_until(cmd, None, cancel)
}

/// Runs `cmd` like `run`, writing `input` to it, and stopping it as soon as `cancel` is set.
fn run_until(mut cmd: Command, input: Option<Vec<u8>>, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{program} could not be started"))?;
    if let (Some(mut stdin), Some(data)) = (child.stdin.take(), input) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = wait(&mut child, &program, cancel)?;
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    finish(&program, status, stdout, &stderr)
}

/// Runs `cmd` with what it prints going straight into a new file `dest`, so a large file of a
/// revision is never held in memory. Stops it as soon as `cancel` is set.
pub(super) fn run_to_file(mut cmd: Command, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let file = std::fs::File::create_new(dest).with_context(|| dest.display().to_string())?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{program} could not be started"))?;
    let stderr = drain(child.stderr.take());
    let status = wait(&mut child, &program, cancel)?;
    let stderr = stderr.join().unwrap_or_default();
    finish(&program, status, Vec::new(), &stderr).map(drop)
}

/// Reads `pipe` to its end on another thread, so a full pipe cannot stop the command. On cancel
/// the thread is left to end with the pipe, which a process the command started may still hold.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Waits for `child` to end, killing it (and failing) as soon as `cancel` is set.
fn wait(
    child: &mut std::process::Child,
    program: &str,
    cancel: &AtomicBool,
) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} was stopped");
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// What `program` printed when it succeeded, or the first line of its error output.
fn finish(
    program: &str,
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: &[u8],
) -> Result<Vec<u8>> {
    if !status.success() {
        let err = match program {
            // their messages are in the ANSI code page on Windows; Git's are UTF-8
            "svn" | "hg" => super::vcs::tool_text(stderr),
            _ => String::from_utf8_lossy(stderr).into_owned(),
        };
        let first = err.lines().find(|l| !l.trim().is_empty());
        bail!(
            "{}",
            first.map_or(format!("{program} failed"), |l| l.trim().to_string())
        );
    }
    Ok(stdout)
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = command(dir);
    cmd.args(args);
    run(cmd, None)
}

/// The repository holding `path` and the path relative to its top folder, with `/`
/// separators (empty for the top folder itself).
pub fn locate(path: &Path) -> Result<(PathBuf, String)> {
    locate_with(path, |dir| git(dir, &["rev-parse", "--show-toplevel"]))
}

/// The top folder that `top` prints when run in the folder of `path`, and the path relative
/// to it with `/` separators (empty for the top folder itself).
pub(super) fn locate_with(
    path: &Path,
    top: impl FnOnce(&Path) -> Result<Vec<u8>>,
) -> Result<(PathBuf, String)> {
    let abs = dunce_canonical(path)?;
    let dir = if abs.is_dir() {
        abs.clone()
    } else {
        abs.parent().context("no parent folder")?.to_path_buf()
    };
    let top = top(&dir)?;
    let root = dunce_canonical(Path::new(String::from_utf8_lossy(&top).trim()))?;
    let rel = abs
        .strip_prefix(&root)
        .with_context(|| format!("{} is not in {}", abs.display(), root.display()))?;
    // A name changed to fit UTF-8 would name another file, or none, in the revision.
    let rel = rel
        .components()
        .map(|c| {
            c.as_os_str()
                .to_str()
                .with_context(|| format!("{}: the name is not valid UTF-8", abs.display()))
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    Ok((root, rel))
}

/// Canonical path without the `\\?\` prefix Windows adds, which git does not print.
pub(super) fn dunce_canonical(path: &Path) -> Result<PathBuf> {
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
pub(super) fn check_rev(rev: &str) -> Result<&str> {
    let rev = rev.trim();
    if rev.is_empty() || rev.starts_with('-') {
        bail!("invalid revision: {rev}");
    }
    Ok(rev)
}

/// Writes file `path` as it was in revision `rev` (a commit, branch, tag, `HEAD~2`, ...) to the
/// new file `dest`, with the line endings and filters a checkout would apply, like the working
/// copy it is compared to.
pub fn save_file(path: &Path, rev: &str, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let rev = check_rev(rev)?;
    let (root, rel) = locate(path)?;
    let mut cmd = command(&root);
    cmd.args(["cat-file", "--filters", &format!("{rev}:{rel}")]);
    run_to_file(cmd, dest, cancel)
}

/// Folder `path` as it was in revision `rev`, extracted to a temporary folder. Returns the
/// temporary folder (deleted when dropped) and the extracted copy of `path` inside it.
pub fn export_folder(
    path: &Path,
    rev: &str,
    cancel: &AtomicBool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let rev = check_rev(rev)?;
    let (root, rel) = locate(path)?;
    let dir = tempfile::Builder::new()
        .prefix("oxidemerge-git-")
        .tempdir()?;
    let mut budget = ExportBudget::new(MAX_EXPORT_BYTES, MAX_EXPORT_FILES);
    checkout_into(&root, rev, &rel, dir.path(), cancel, &mut budget)?;
    // A tracked symlink (including one from a submodule) is checked out as a real symlink; one
    // crafted to point outside the temporary folder could otherwise be opened and shown as if
    // it were part of the revision, the same risk `archive::remove_links_outside` already
    // guards extracting an archive against.
    super::archive::remove_links_outside(dir.path())?;
    let copy = dir.path().join(&rel);
    std::fs::create_dir_all(&copy)?;
    Ok((dir, copy))
}

/// The most a whole revision export may contain in total, across its folder and every
/// submodule within it, checked before anything is written: the same bound `archive.rs` puts
/// on unpacking an archive, since both materialize untrusted bulk content - a compact tree or
/// entry table that could reference far more data than a user meant to export - to a temporary
/// folder.
const MAX_EXPORT_BYTES: u64 = 8 << 30;
/// The most files a whole revision export may contain in total. See [`MAX_EXPORT_BYTES`].
const MAX_EXPORT_FILES: usize = 500_000;

/// What is left of [`MAX_EXPORT_BYTES`]/[`MAX_EXPORT_FILES`] while [`checkout_into`] recurses
/// into submodules: shared across every level so the limit bounds the whole export, not each
/// submodule again on its own. The limits are fields, not just constants, so a test can hit
/// them without actually checking out gigabytes or hundreds of thousands of files.
struct ExportBudget {
    bytes: u64,
    files: usize,
    max_bytes: u64,
    max_files: usize,
}

impl ExportBudget {
    fn new(max_bytes: u64, max_files: usize) -> Self {
        Self {
            bytes: 0,
            files: 0,
            max_bytes,
            max_files,
        }
    }

    fn take_files(&mut self, n: usize) -> Result<()> {
        self.files += n;
        if self.files > self.max_files {
            bail!(
                "stopped after {} files; export a smaller revision or folder",
                self.max_files
            );
        }
        Ok(())
    }

    fn take_bytes(&mut self, n: u64) -> Result<()> {
        self.bytes += n;
        if self.bytes > self.max_bytes {
            bail!(
                "stopped after {} GB; export a smaller revision or folder",
                self.max_bytes >> 30
            );
        }
        Ok(())
    }
}

/// Writes folder `rel` (empty for all) of revision `rev` of the repository at `root` into
/// `dest` as a checkout would, with its line endings and filters such as Git LFS, through a
/// separate index so the repository's own index and working copy are left alone. Submodules
/// are filled in from their checked-out repositories when the recorded commit is there, drawing
/// on the same `budget` as the folder itself.
fn checkout_into(
    root: &Path,
    rev: &str,
    rel: &str,
    dest: &Path,
    cancel: &AtomicBool,
    budget: &mut ExportBudget,
) -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    let with_index = |args: &[&str]| {
        let mut cmd = command(root);
        cmd.env("GIT_INDEX_FILE", &index)
            .arg("--literal-pathspecs")
            .args(args);
        cmd
    };
    run_until(with_index(&["read-tree", rev]), None, cancel)?;
    let mut ls = vec!["ls-files", "--stage", "-z"];
    if !rel.is_empty() {
        ls.extend(["--", rel]);
    }
    let listing = run_until(with_index(&ls), None, cancel)?;
    let mut files = Vec::new();
    let mut hashes = Vec::new();
    let mut submodules = Vec::new();
    let mut file_count: usize = 0;
    // entries are "mode hash stage<TAB>path"
    for entry in listing.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let (meta, path) = (&entry[..tab], &entry[tab + 1..]);
        let hash = meta.split(|&b| b == b' ').nth(1).unwrap_or_default();
        // A submodule entry counts towards the budget too, even though its own content is
        // only bounded once it is recursed into below: otherwise a tree of countless submodule
        // entries, each needing only an empty folder here, would not be bounded at all.
        file_count += 1;
        if meta.starts_with(b"160000 ") {
            submodules.push((
                String::from_utf8_lossy(path).into_owned(),
                String::from_utf8_lossy(hash).into_owned(),
            ));
        } else {
            hashes.extend_from_slice(hash);
            hashes.push(b'\n');
            files.extend_from_slice(path);
            files.push(0);
        }
    }
    budget.take_files(file_count)?;
    if !hashes.is_empty() {
        // sizes come back one per line, in the order the hashes were sent, before anything
        // is written to `dest`: a mistaken or huge revision is refused rather than filling
        // the disk.
        let sizes = run_until(
            with_index(&["cat-file", "--batch-check=%(objectsize)"]),
            Some(hashes),
            cancel,
        )?;
        let total: u64 = String::from_utf8_lossy(&sizes)
            .lines()
            .filter_map(|l| l.trim().parse::<u64>().ok())
            .sum();
        budget.take_bytes(total)?;
    }
    if !files.is_empty() {
        let prefix = format!("--prefix={}/", dest.display());
        let checkout = |skip_lfs: bool| {
            let mut cmd = with_index(&["checkout-index", "--stdin", "-z", "-f", &prefix]);
            if skip_lfs {
                // LFS files of old revisions may not have been downloaded: show their pointers
                cmd.env("GIT_LFS_SKIP_SMUDGE", "1");
            }
            run_until(cmd, Some(files.clone()), cancel)
        };
        checkout(false).or_else(|e| {
            if cancel.load(Ordering::Relaxed) {
                Err(e)
            } else {
                checkout(true)
            }
        })?;
    }
    for (path, hash) in submodules {
        let target = dest.join(&path);
        std::fs::create_dir_all(&target)?;
        let work = root.join(&path);
        if work.join(".git").exists() {
            let _ = checkout_into(&work, &hash, "", &target, cancel, budget);
        }
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("git was stopped");
    }
    Ok(())
}

/// Recent commits touching `path`, newest first: (short hash, subject). Setting `cancel` stops
/// a log still running (one walking a long history), and the list comes back empty.
pub fn recent_commits(path: &Path, n: usize, cancel: &AtomicBool) -> Vec<(String, String)> {
    let Ok((root, rel)) = locate(path) else {
        return Vec::new();
    };
    let count = format!("-n{n}");
    let mut args = vec!["log", count.as_str(), "--format=%h%x09%s"];
    if !rel.is_empty() {
        args.extend(["--", rel.as_str()]);
    }
    let mut cmd = command(&root);
    cmd.args(&args);
    run_cancellable(cmd, cancel)
        .map(|out| {
            String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(h, s)| (h.to_string(), s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a user revision once; callers retain this immutable commit in baseline reports.
pub fn resolve_commit(path: &Path, revision: &str) -> Result<String> {
    let revision = check_rev(revision)?;
    let (root, _) = locate(path)?;
    let bytes = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    let id = String::from_utf8(bytes)?.trim().to_owned();
    if ![40, 64].contains(&id.len()) || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid resolved Git commit");
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_command_is_stopped_at_once() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep 30"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.current_dir(std::env::temp_dir());
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            flag.store(true, Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        let err = run_cancellable(cmd, &cancel).unwrap_err();
        assert!(err.to_string().contains("was stopped"), "{err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));

        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "echo" });
        if cfg!(windows) {
            cmd.args(["/c", "echo", "done"]);
        } else {
            cmd.arg("done");
        }
        let out = run_cancellable(cmd, &AtomicBool::new(false)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "done");
    }

    #[test]
    fn a_submodule_entry_counts_towards_the_file_budget_too() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| git(repo, args).unwrap();
        let cancel = AtomicBool::new(false);
        if git(repo, &["init", "-q"]).is_err() {
            return; // git is not installed
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        // A gitlink entry (mode 160000) needs no blob of its own, so it can be added as a tree
        // entry directly, without a real submodule repository: cheaper, and just as good for
        // checking that it is still counted.
        run(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ]);
        run(&["commit", "-q", "-m", "first"]);

        let out = tempfile::tempdir().unwrap();
        let mut budget = ExportBudget::new(u64::MAX, 0);
        let err = checkout_into(repo, "HEAD", "", out.path(), &cancel, &mut budget).unwrap_err();
        assert!(err.to_string().contains("0 files"), "{err}");
    }

    #[test]
    fn an_export_past_the_file_or_byte_budget_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| git(repo, args).unwrap();
        let cancel = AtomicBool::new(false);
        if git(repo, &["init", "-q"]).is_err() {
            return; // git is not installed
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "0123456789").unwrap();
        std::fs::write(repo.join("b.txt"), "0123456789").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);

        // a byte budget too small for even one of the two 10-byte files
        let out = tempfile::tempdir().unwrap();
        let mut budget = ExportBudget::new(5, usize::MAX);
        let err = checkout_into(repo, "HEAD", "", out.path(), &cancel, &mut budget).unwrap_err();
        assert!(err.to_string().contains("GB"), "{err}");
        assert!(
            std::fs::read_dir(out.path()).unwrap().next().is_none(),
            "nothing should have been written once the budget was passed"
        );

        // a file-count budget too small for the two files
        let out = tempfile::tempdir().unwrap();
        let mut budget = ExportBudget::new(u64::MAX, 1);
        let err = checkout_into(repo, "HEAD", "", out.path(), &cancel, &mut budget).unwrap_err();
        assert!(err.to_string().contains("1 files"), "{err}");
        assert!(std::fs::read_dir(out.path()).unwrap().next().is_none());

        // budgets large enough for both still succeed
        let out = tempfile::tempdir().unwrap();
        let mut budget = ExportBudget::new(u64::MAX, usize::MAX);
        checkout_into(repo, "HEAD", "", out.path(), &cancel, &mut budget).unwrap();
        assert!(out.path().join("a.txt").is_file());
        assert!(out.path().join("b.txt").is_file());
    }

    /// Two submodules, each small enough on its own, must not add up to more than the budget
    /// allows in total: otherwise the number of submodules, not the limit, would decide how
    /// much a revision export could fill the disk with.
    #[test]
    fn two_submodules_share_one_budget_rather_than_getting_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |cwd: &Path, args: &[&str]| git(cwd, args).unwrap();
        let cancel = AtomicBool::new(false);
        if git(repo, &["init", "-q"]).is_err() {
            return; // git is not installed
        }
        run(repo, &["config", "user.email", "t@example.com"]);
        run(repo, &["config", "user.name", "T"]);
        run(repo, &["config", "commit.gpgsign", "false"]);

        let make_submodule = |name: &str, content: &str| {
            let sub = dir.path().join(name);
            std::fs::create_dir_all(&sub).unwrap();
            run(&sub, &["init", "-q"]);
            run(&sub, &["config", "user.email", "t@example.com"]);
            run(&sub, &["config", "user.name", "T"]);
            run(&sub, &["config", "commit.gpgsign", "false"]);
            std::fs::write(sub.join("f.txt"), content).unwrap();
            run(&sub, &["add", "."]);
            run(&sub, &["commit", "-q", "-m", "first"]);
            sub
        };
        let a = make_submodule("a", "0123456789");
        let b = make_submodule("b", "0123456789");
        for (name, sub) in [("a", &a), ("b", &b)] {
            let added = git(
                repo,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    &sub.display().to_string(),
                    name,
                ],
            );
            if added.is_err() {
                return; // local-path submodules refused by this Git's configuration
            }
        }
        run(repo, &["commit", "-q", "-m", "add submodules"]);

        // The top-level folder has 3 entries of its own (`.gitmodules` plus the two gitlinks),
        // and each submodule's own `f.txt` is one more: a budget of 4 files fits the top level
        // plus one submodule, but not both submodules together.
        let out = tempfile::tempdir().unwrap();
        let mut budget = ExportBudget::new(u64::MAX, 4);
        checkout_into(repo, "HEAD", "", out.path(), &cancel, &mut budget).unwrap();
        assert!(
            out.path().join("a/f.txt").is_file(),
            "the first submodule should still fit"
        );
        assert!(
            !out.path().join("b/f.txt").exists(),
            "the second submodule should not get a budget of its own"
        );
    }

    /// A tracked symlink pointing outside the repository is checked out as a real symlink by
    /// `git checkout-index`: left alone, opening it would read whatever it points to, elsewhere
    /// on disk, as if that were part of the revision. `export_folder` must drop it, the same
    /// way `archive::remove_links_outside` already protects an archive extraction.
    #[cfg(unix)]
    #[test]
    fn export_folder_drops_a_symlink_pointing_outside_it() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| git(repo, args).unwrap();
        let never = AtomicBool::new(false);
        if git(repo, &["init", "-q"]).is_err() {
            return; // git is not installed
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), repo.join("escape.txt"))
            .unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);

        let (_tmp, copy) = export_folder(repo, "HEAD", &never).unwrap();
        assert!(copy.join("a.txt").is_file());
        assert!(
            !copy.join("escape.txt").exists(),
            "the escaping link should have been dropped"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_refused_rather_than_changed() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let odd = d.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir(&odd).unwrap();
        let top = d.path().display().to_string().into_bytes();
        let err = locate_with(&odd, |_| Ok(top)).unwrap_err();
        assert!(err.to_string().contains("UTF-8"), "{err}");
    }

    #[test]
    fn reads_files_and_folders_of_a_revision() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| git(repo, args).unwrap();
        let never = AtomicBool::new(false);
        let show_file = |path: &Path, rev: &str| -> Result<Vec<u8>> {
            let out = tempfile::tempdir().unwrap();
            let dest = out.path().join("f");
            save_file(path, rev, &dest, &never)?;
            Ok(std::fs::read(dest)?)
        };
        let export_folder = |path: &Path, rev: &str| export_folder(path, rev, &never);
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
        let commits = recent_commits(&file, 5, &never);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].1, "first");
        assert!(show_file(&file, "nosuchrev").is_err());
        assert!(export_folder(repo, "--output=x").is_err());
        assert!(!repo.join("x").exists());
        let stopped = AtomicBool::new(true);
        assert!(super::export_folder(repo, "HEAD", &stopped).is_err());
        assert!(recent_commits(&file, 5, &stopped).is_empty());

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
