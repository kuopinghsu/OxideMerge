//! Saving a file to a server through a staged copy, so that a failed upload never leaves the
//! file there partly written: the new bytes go to a temporary file beside it, which is checked
//! against what was sent and only then takes the original's place.
//!
//! An SFTP server (protocol version 3, as OpenSSH's) has no rename that replaces a file, so
//! the original is first renamed aside and then the staged copy renamed into its place; the
//! file is briefly absent in between, but never partial. A server that can rename over a file
//! does it in one step. Each rename either happens or does not; if the connection is lost in
//! the middle, the error names the files left on the server (see [`Uncertain`]).
//!
//! A replacement is a new file, so it only takes the original's place when it can be given the
//! same owner, group and permissions: otherwise (a file owned by someone else, a folder the
//! user may not create files in, a server that does not tell owners) the caller writes the file
//! in place instead. Access-control lists and hard links cannot be seen through SFTP version 3,
//! so a replacement loses them; [`super::remote::ConnOptions::in_place`] is for such servers.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};

use super::remote::{check, RemoteStamp};
use crate::i18n::trf;

/// How a file is on the server, as far as a staged upload needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    pub stamp: RemoteStamp,
    pub is_file: bool,
    /// Owner and group, when the server tells.
    pub owner: Option<(u32, u32)>,
    /// Permission bits (`0o7777`), when the server tells.
    pub perm: Option<u32>,
}

/// The file operations a staged upload is made of. A seam, so the order of the steps and what
/// happens when any one of them fails can be tested without a server (see the tests).
pub(crate) trait Stage {
    /// How `path` is now, following a symbolic link; `None` when nothing is there.
    fn meta(&self, path: &str) -> Result<Option<Meta>>;
    /// Creates `path`, empty, with permissions `perm` (less the server's umask). Fails when
    /// something is there already.
    fn create(&self, path: &str, perm: u32) -> Result<()>;
    /// Writes what `from` reads to `path`, made by [`Stage::create`], flushing it to disk where
    /// the server can. Setting `cancel` stops it. Returns how many bytes were written.
    fn write(&self, path: &str, from: &mut dyn Read, cancel: &AtomicBool) -> Result<u64>;
    fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<()>;
    fn set_perm(&self, path: &str, perm: u32) -> Result<()>;
    fn read(&self, path: &str) -> Result<Box<dyn Read + '_>>;
    /// Renames `from` to `to`. With `replace`, an existing `to` may be replaced in one step
    /// where the server can; otherwise, and on servers that cannot, an existing `to` makes it
    /// fail.
    fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()>;
    fn remove(&self, path: &str) -> Result<()>;
}

/// Attached to an upload's error when the file on the server is as it was before: nothing of
/// the upload took its place.
#[derive(Debug)]
pub struct Unchanged {
    pub path: String,
}

impl std::fmt::Display for Unchanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&trf("msg.remote_unchanged", &[("path", self.path.clone())]))
    }
}

/// Attached to an upload's error when it could not tell how it left the server (the connection
/// was lost while renaming): `path` holds either version, and the other is at `left`.
#[derive(Debug)]
pub struct Uncertain {
    pub path: String,
    pub left: Vec<String>,
}

impl std::fmt::Display for Uncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&trf(
            "msg.remote_uncertain",
            &[("path", self.path.clone()), ("left", self.left.join(", "))],
        ))
    }
}

/// A file a failed upload could not remove from the server.
#[derive(Debug)]
pub struct LeftOver {
    pub path: String,
}

impl std::fmt::Display for LeftOver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&trf("msg.remote_left_over", &[("path", self.path.clone())]))
    }
}

/// A name for a temporary file beside `target`: hidden, marked as this program's with `what`
/// (`new` for a staged copy, `old` for an original renamed aside), unique to this process and
/// moment, and short enough for the 255 bytes most file systems allow. Holds nothing of the
/// connection (no user name or password).
pub(crate) fn temp_name(target: &str, what: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let (dir, name) = match target.rfind('/') {
        Some(i) => target.split_at(i + 1),
        None => ("", target),
    };
    let mut end = name.len().min(100);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{dir}.{}.oxidemerge-{what}-{}-{unique}",
        &name[..end],
        std::process::id()
    )
}

fn changed(path: &str) -> anyhow::Error {
    anyhow::anyhow!(trf(
        "msg.sftp_changed_on_server",
        &[("path", path.to_string())]
    ))
}

/// A reader taking the SHA-256 digest of what it reads, and counting it.
struct Hashing<R> {
    inner: R,
    hasher: sha2::Sha256,
    bytes: u64,
}

impl<R: Read> Hashing<R> {
    fn new(inner: R) -> Self {
        use sha2::Digest;
        Self {
            inner,
            hasher: sha2::Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> ([u8; 32], u64) {
        use sha2::Digest;
        (self.hasher.finalize().into(), self.bytes)
    }
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// The SHA-256 digest of what `from` reads. Setting `cancel` stops it, and it fails.
fn sha256_of(from: impl Read, cancel: &AtomicBool) -> Result<[u8; 32]> {
    let mut from = Hashing::new(from);
    let mut buf = vec![0u8; super::remote::CHUNK];
    loop {
        check(cancel)?;
        if from.read(&mut buf)? == 0 {
            return Ok(from.finish().0);
        }
    }
}

/// Writes `local` to a new file beside `target`, checks it holds exactly what was sent, and
/// returns its name; removes it again if any of that fails. `prepare`, when given, runs on the
/// empty file first, and returning `Ok(false)` gives up without an error (`Ok(None)`), as does
/// failing to create the file at all.
fn stage(
    s: &dyn Stage,
    local: &Path,
    target: &str,
    perm: u32,
    prepare: Option<impl FnOnce(&str) -> Result<bool>>,
    cancel: &AtomicBool,
) -> Result<Option<String>> {
    let tmp = temp_name(target, "new");
    let from = std::fs::File::open(local).with_context(|| {
        trf(
            "msg.sftp_cannot_read",
            &[("path", local.display().to_string())],
        )
    })?;
    let size = from.metadata()?.len();
    check(cancel)?;
    match s.create(&tmp, perm) {
        Ok(()) => {}
        // A folder the user may not create files in, whose file may still be writable.
        Err(_) if prepare.is_some() => return Ok(None),
        Err(e) => return Err(e),
    }
    let staged = (|| -> Result<bool> {
        if let Some(prepare) = prepare {
            if !prepare(&tmp)? {
                return Ok(false);
            }
        }
        // Read back and compared by SHA-256 digest: a copy is only used if it is exactly
        // what was sent.
        let mut from = Hashing::new(from);
        let written = s.write(&tmp, &mut from, cancel)?;
        let (hash, read) = from.finish();
        let on_server = s.meta(&tmp)?.map(|m| m.stamp.size);
        if read != size || written != read || on_server != Some(read) {
            bail!(trf(
                "msg.remote_verify_failed",
                &[("path", target.to_string())]
            ));
        }
        if sha256_of(s.read(&tmp)?, cancel)? != hash {
            bail!(trf(
                "msg.remote_verify_failed",
                &[("path", target.to_string())]
            ));
        }
        Ok(true)
    })();
    match staged {
        Ok(true) => Ok(Some(tmp)),
        Ok(false) => match s.remove(&tmp) {
            Ok(()) => Ok(None),
            Err(e) => Err(e.context(LeftOver { path: tmp })),
        },
        Err(e) => Err(match s.remove(&tmp) {
            Ok(()) => e,
            Err(_) => e.context(LeftOver { path: tmp }),
        }),
    }
}

/// Uploads `local` as the new file `target`, which must not exist: written to a staged copy
/// first, and renamed into place only once complete, never over a file someone else created
/// meanwhile (on a server whose plain rename refuses to replace, as OpenSSH's).
pub(crate) fn create(
    s: &dyn Stage,
    local: &Path,
    target: &str,
    cancel: &AtomicBool,
) -> Result<RemoteStamp> {
    if s.meta(target)?.is_some() {
        bail!(trf(
            "msg.remote_created_meanwhile",
            &[("path", target.to_string())]
        ));
    }
    let tmp = stage(
        s,
        local,
        target,
        0o644,
        None::<fn(&str) -> Result<bool>>,
        cancel,
    )?
    .expect("staged unless it fails");
    let published = (|| {
        check(cancel)?;
        s.rename(&tmp, target, false)
    })();
    if let Err(e) = published {
        return Err(match s.remove(&tmp) {
            Ok(()) => e,
            Err(_) => e.context(LeftOver { path: tmp }),
        });
    }
    Ok(s.meta(target)?.map(|m| m.stamp).unwrap_or_default())
}

/// Replaces the existing file `target`, which must still be as `expected`, with `local`'s
/// bytes through a staged copy. `Ok(None)` when it cannot be replaced keeping its owner, group
/// and permissions (see the module's documentation), with the server left as it was, for the
/// caller to write the file in place instead.
///
/// Fails without changing `target` (the error then carries [`Unchanged`]) when the file
/// changed meanwhile, the upload is cancelled, or anything fails before the staged copy is
/// complete and checked. Once it is, the original is checked once more and replaced.
pub(crate) fn replace(
    s: &dyn Stage,
    local: &Path,
    target: &str,
    expected: RemoteStamp,
    cancel: &AtomicBool,
) -> Result<Option<RemoteStamp>> {
    let unchanged = |e: anyhow::Error| {
        e.context(Unchanged {
            path: target.to_string(),
        })
    };
    let now = s.meta(target).map_err(unchanged)?;
    let now = match now {
        Some(now) if now.is_file && now.stamp == expected => now,
        _ => return Err(unchanged(changed(target))),
    };
    let (Some((uid, gid)), Some(perm)) = (now.owner, now.perm) else {
        return Ok(None);
    };
    let perm = perm & 0o7777;
    let prepare = |tmp: &str| -> Result<bool> {
        let Some(made) = s.meta(tmp)? else {
            return Ok(false);
        };
        if made.owner != Some((uid, gid))
            && (s.set_owner(tmp, uid, gid).is_err()
                || s.meta(tmp)?.and_then(|m| m.owner) != Some((uid, gid)))
        {
            return Ok(false);
        }
        Ok(s.set_perm(tmp, perm).is_ok() && s.meta(tmp)?.and_then(|m| m.perm) == Some(perm))
    };
    let tmp = match stage(s, local, target, 0o600, Some(prepare), cancel) {
        Ok(Some(tmp)) => tmp,
        Ok(None) => return Ok(None),
        Err(e) => return Err(unchanged(e)),
    };
    let drop_tmp = |e: anyhow::Error| match s.remove(&tmp) {
        Ok(()) => unchanged(e),
        Err(_) => unchanged(e).context(LeftOver { path: tmp.clone() }),
    };
    if let Err(e) = check(cancel) {
        return Err(drop_tmp(e));
    }
    match s.meta(target) {
        Ok(Some(m)) if m.is_file && m.stamp == expected => {}
        Ok(_) => return Err(drop_tmp(changed(target))),
        Err(e) => return Err(drop_tmp(e)),
    }
    let done = || -> Result<Option<RemoteStamp>> {
        Ok(Some(s.meta(target)?.map(|m| m.stamp).unwrap_or_default()))
    };
    let uncertain = |e: anyhow::Error, left: Vec<String>| {
        e.context(Uncertain {
            path: target.to_string(),
            left,
        })
    };

    // In one step, where the server can rename over a file.
    if let Err(e) = s.rename(&tmp, target, true) {
        match s.meta(&tmp) {
            // Renamed after all, though the reply was lost.
            Ok(None) => return done(),
            Ok(Some(_)) => {}
            Err(_) => return Err(uncertain(e, vec![tmp])),
        }
    } else {
        return done();
    }

    // Otherwise the original is renamed aside first.
    let old = temp_name(target, "old");
    if let Err(e) = s.rename(target, &old, false) {
        return match s.meta(&old) {
            Ok(None) => Err(drop_tmp(e)),
            _ => Err(uncertain(e, vec![tmp, old])),
        };
    }
    let put_back = |e: anyhow::Error| match s.rename(&old, target, false) {
        Ok(()) => drop_tmp(e),
        Err(_) => e.context(Uncertain {
            path: target.to_string(),
            left: vec![tmp.clone(), old.clone()],
        }),
    };
    // What was renamed is the file checked: had it changed just before, it is put back.
    match s.meta(&old) {
        Ok(Some(m)) if m.is_file && m.stamp == expected => {}
        Ok(_) => return Err(put_back(changed(target))),
        Err(e) => return Err(uncertain(e, vec![tmp, old])),
    }
    if let Err(e) = s.rename(&tmp, target, false) {
        return match s.meta(&tmp) {
            Ok(Some(_)) => Err(put_back(e)),
            Ok(None) => {
                let _ = s.remove(&old);
                done()
            }
            Err(_) => Err(uncertain(e, vec![tmp, old])),
        };
    }
    // The original is no longer needed; tried twice, as a lost reply may have removed it.
    if s.remove(&old).is_err() && !matches!(s.meta(&old), Ok(None)) {
        let _ = s.remove(&old);
    }
    done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    struct File {
        bytes: Vec<u8>,
        version: u64,
        owner: (u32, u32),
        perm: u32,
    }

    type Files = BTreeMap<String, File>;
    type Hook = Box<dyn FnOnce(&mut Files)>;

    /// What happens at a step of an upload, once.
    #[derive(Clone, Copy)]
    enum Fault {
        /// The server refuses it, and the connection stays up.
        Refuse,
        /// The connection drops before the server does it.
        Drop,
        /// The server does it, then the connection drops before the reply arrives.
        LoseReply,
        /// The connection drops after this percentage of the bytes was written.
        DropAt(usize),
    }

    /// A server in memory, whose every step of an upload can fail or be preceded by someone
    /// else's change. Paths are told apart by kind in step names: `target`, `new` (a staged
    /// copy) and `old` (an original renamed aside), as in `rename new>target`.
    struct Mem {
        files: RefCell<Files>,
        /// The owner of what this user creates.
        me: (u32, u32),
        /// Whether files may be given another owner or group than `me`.
        chown: bool,
        /// Whether files may be created in the folder.
        mkfile: bool,
        /// Whether a rename may replace a file in one step (SFTP versions 5 and later).
        rename_over: bool,
        /// Whether what is written arrives with a byte changed.
        corrupt: bool,
        faults: RefCell<Vec<(String, Fault)>>,
        hooks: RefCell<Vec<(String, Hook)>>,
        down: Cell<bool>,
        log: RefCell<Vec<String>>,
        clock: Cell<u64>,
    }

    const TARGET: &str = "/srv/site/page.html";

    impl Mem {
        fn new() -> Self {
            let s = Self {
                files: Default::default(),
                me: (1000, 100),
                chown: false,
                mkfile: true,
                rename_over: false,
                corrupt: false,
                faults: Default::default(),
                hooks: Default::default(),
                down: Cell::new(false),
                log: Default::default(),
                clock: Cell::new(1_000),
            };
            s.put(TARGET, b"original contents\n", (1000, 100), 0o640);
            s
        }

        fn tick(&self) -> u64 {
            self.clock.set(self.clock.get() + 1);
            self.clock.get()
        }

        fn put(&self, path: &str, bytes: &[u8], owner: (u32, u32), perm: u32) {
            let version = self.tick();
            self.files.borrow_mut().insert(
                path.to_string(),
                File {
                    bytes: bytes.to_vec(),
                    version,
                    owner,
                    perm,
                },
            );
        }

        fn fault(self, step: &str, fault: Fault) -> Self {
            self.faults.borrow_mut().push((step.to_string(), fault));
            self
        }

        fn before(self, step: &str, hook: impl FnOnce(&mut Files) + 'static) -> Self {
            self.hooks
                .borrow_mut()
                .push((step.to_string(), Box::new(hook)));
            self
        }

        fn bytes(&self, path: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(path).map(|f| f.bytes.clone())
        }

        fn stamp(&self, path: &str) -> RemoteStamp {
            self.meta_now(path).unwrap().stamp
        }

        fn meta_now(&self, path: &str) -> Option<Meta> {
            self.files.borrow().get(path).map(|f| Meta {
                stamp: RemoteStamp::new(f.bytes.len() as u64, f.version),
                is_file: true,
                owner: Some(f.owner),
                perm: Some(f.perm),
            })
        }

        /// Files other than the target: what an upload left behind.
        fn others(&self) -> Vec<String> {
            let files = self.files.borrow();
            files.keys().filter(|p| *p != TARGET).cloned().collect()
        }

        fn kind(path: &str) -> &'static str {
            if path.contains(".oxidemerge-new-") {
                "new"
            } else if path.contains(".oxidemerge-old-") {
                "old"
            } else {
                "target"
            }
        }

        /// Logs `step`, runs what was to happen before it, and fails it as asked.
        fn step(&self, step: String) -> Result<Option<Fault>> {
            if self.down.get() {
                bail!("connection lost");
            }
            let hooks: Vec<Hook> = {
                let mut all = self.hooks.borrow_mut();
                let (now, later) = std::mem::take(&mut *all)
                    .into_iter()
                    .partition(|(s, _)| *s == step);
                *all = later;
                now.into_iter().map(|(_, h)| h).collect()
            };
            for hook in hooks {
                hook(&mut self.files.borrow_mut());
            }
            self.log.borrow_mut().push(step.clone());
            let fault = {
                let mut faults = self.faults.borrow_mut();
                let i = faults.iter().position(|(s, _)| *s == step);
                i.map(|i| faults.remove(i).1)
            };
            match fault {
                Some(Fault::Refuse) => bail!("{step}: refused"),
                Some(Fault::Drop) => {
                    self.down.set(true);
                    bail!("connection lost")
                }
                other => Ok(other),
            }
        }

        /// After an operation done with [`Fault::LoseReply`]: its reply never arrives.
        fn reply(&self, fault: Option<Fault>) -> Result<()> {
            if matches!(fault, Some(Fault::LoseReply)) {
                self.down.set(true);
                bail!("connection lost");
            }
            Ok(())
        }
    }

    impl Stage for Mem {
        fn meta(&self, path: &str) -> Result<Option<Meta>> {
            let fault = self.step(format!("meta {}", Self::kind(path)))?;
            let meta = self.meta_now(path);
            self.reply(fault)?;
            Ok(meta)
        }

        fn create(&self, path: &str, perm: u32) -> Result<()> {
            let fault = self.step(format!("create {}", Self::kind(path)))?;
            if !self.mkfile || self.files.borrow().contains_key(path) {
                bail!("{path}: cannot create");
            }
            self.put(path, b"", self.me, perm & !0o022);
            self.reply(fault)
        }

        fn write(&self, path: &str, from: &mut dyn Read, cancel: &AtomicBool) -> Result<u64> {
            let fault = self.step(format!("write {}", Self::kind(path)))?;
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                check(cancel)?;
                let n = from.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            let written = bytes.len() as u64;
            if let Some(Fault::DropAt(percent)) = fault {
                bytes.truncate(bytes.len() * percent / 100);
                self.files.borrow_mut().get_mut(path).unwrap().bytes = bytes;
                self.down.set(true);
                bail!("connection lost");
            }
            if self.corrupt && !bytes.is_empty() {
                bytes[0] ^= 1;
            }
            let version = self.tick();
            let mut files = self.files.borrow_mut();
            let file = files.get_mut(path).unwrap();
            file.bytes = bytes;
            file.version = version;
            drop(files);
            self.reply(fault)?;
            Ok(written)
        }

        fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
            let fault = self.step(format!("set_owner {}", Self::kind(path)))?;
            if !self.chown && (uid, gid) != self.me {
                bail!("{path}: not permitted");
            }
            self.files.borrow_mut().get_mut(path).unwrap().owner = (uid, gid);
            self.reply(fault)
        }

        fn set_perm(&self, path: &str, perm: u32) -> Result<()> {
            let fault = self.step(format!("set_perm {}", Self::kind(path)))?;
            self.files.borrow_mut().get_mut(path).unwrap().perm = perm;
            self.reply(fault)
        }

        fn read(&self, path: &str) -> Result<Box<dyn Read + '_>> {
            let fault = self.step(format!("read {}", Self::kind(path)))?;
            let bytes = self.bytes(path).context("no such file")?;
            self.reply(fault)?;
            Ok(Box::new(std::io::Cursor::new(bytes)))
        }

        fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
            let fault = self.step(format!("rename {}>{}", Self::kind(from), Self::kind(to)))?;
            let mut files = self.files.borrow_mut();
            if files.contains_key(to) && !(replace && self.rename_over) {
                bail!("{to}: exists");
            }
            let file = files.remove(from).context("no such file")?;
            files.insert(to.to_string(), file);
            drop(files);
            self.reply(fault)
        }

        fn remove(&self, path: &str) -> Result<()> {
            let fault = self.step(format!("remove {}", Self::kind(path)))?;
            self.files
                .borrow_mut()
                .remove(path)
                .context("no such file")?;
            self.reply(fault)
        }
    }

    const NEW: &[u8] = b"edited contents, longer than before\n";

    fn local(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.html");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    fn run(s: &Mem) -> Result<Option<RemoteStamp>> {
        let (_dir, path) = local(NEW);
        replace(s, &path, TARGET, s.stamp(TARGET), &AtomicBool::new(false))
    }

    fn has<T: std::fmt::Display + std::fmt::Debug + Send + Sync + 'static>(
        e: &anyhow::Error,
    ) -> bool {
        e.downcast_ref::<T>().is_some()
    }

    fn renames(s: &Mem) -> Vec<String> {
        let log = s.log.borrow();
        log.iter()
            .filter(|l| l.starts_with("rename") || l.starts_with("remove"))
            .cloned()
            .collect()
    }

    #[test]
    fn replaces_through_a_staged_copy_keeping_owner_and_permissions() {
        let s = Mem::new();
        let stamp = run(&s).unwrap().unwrap();
        assert_eq!(s.bytes(TARGET).unwrap(), NEW);
        assert_eq!(stamp, s.stamp(TARGET));
        let meta = s.meta_now(TARGET).unwrap();
        assert_eq!((meta.owner, meta.perm), (Some((1000, 100)), Some(0o640)));
        assert!(s.others().is_empty(), "{:?}", s.others());
        // OpenSSH's rename refuses to replace, so the original goes aside first.
        assert_eq!(
            renames(&s),
            [
                "rename new>target",
                "rename target>old",
                "rename new>target",
                "remove old"
            ]
        );
        // The staged copy was read back before it took the original's place.
        let log = s.log.borrow();
        let read = log.iter().position(|l| l == "read new").unwrap();
        let first = log.iter().position(|l| l.starts_with("rename")).unwrap();
        assert!(read < first);
    }

    #[test]
    fn a_server_that_renames_over_a_file_replaces_it_in_one_step() {
        let mut s = Mem::new();
        s.rename_over = true;
        run(&s).unwrap().unwrap();
        assert_eq!(s.bytes(TARGET).unwrap(), NEW);
        assert_eq!(renames(&s), ["rename new>target"]);
        assert!(s.others().is_empty());
    }

    #[test]
    fn a_connection_lost_during_the_upload_leaves_the_original_untouched() {
        for percent in [10, 50, 95] {
            let s = Mem::new().fault("write new", Fault::DropAt(percent));
            let e = run(&s).unwrap_err();
            assert_eq!(
                s.bytes(TARGET).unwrap(),
                b"original contents\n",
                "{percent}%"
            );
            assert!(has::<Unchanged>(&e), "{percent}%: {e:#}");
            // Nothing could be removed once the connection was gone, and that is said.
            assert!(has::<LeftOver>(&e), "{percent}%: {e:#}");
            assert_eq!(s.others().len(), 1);
        }
        // A write the server refuses, with the connection still up, leaves nothing behind.
        let s = Mem::new().fault("write new", Fault::Refuse);
        let e = run(&s).unwrap_err();
        assert!(has::<Unchanged>(&e) && !has::<LeftOver>(&e), "{e:#}");
        assert!(s.others().is_empty());
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");
    }

    #[test]
    fn a_staged_copy_that_differs_from_what_was_sent_is_never_used() {
        let mut s = Mem::new();
        s.corrupt = true;
        let e = run(&s).unwrap_err();
        assert!(has::<Unchanged>(&e), "{e:#}");
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");
        assert!(s.others().is_empty());
        assert!(renames(&s).iter().all(|r| r == "remove new"));
    }

    #[test]
    fn cancelling_before_the_rename_leaves_the_original_untouched() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let s = Mem::new().before("write new", move |_| flag.store(true, Ordering::Relaxed));
        let (_dir, path) = local(NEW);
        let e = replace(&s, &path, TARGET, s.stamp(TARGET), &cancel).unwrap_err();
        assert!(has::<Unchanged>(&e), "{e:#}");
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");
        assert!(s.others().is_empty());
    }

    #[test]
    fn a_change_on_the_server_while_staging_is_not_overwritten() {
        // Changed while the staged copy was being checked: noticed before any rename.
        let s = Mem::new().before("read new", |files| {
            let f = files.get_mut(TARGET).unwrap();
            f.bytes = b"their edit\n".to_vec();
            f.version += 100;
        });
        let e = run(&s).unwrap_err();
        assert!(has::<Unchanged>(&e), "{e:#}");
        assert_eq!(s.bytes(TARGET).unwrap(), b"their edit\n");
        assert!(s.others().is_empty());
        assert!(renames(&s).iter().all(|r| r == "remove new"));

        // Changed in the instant between that check and the rename: what was renamed aside
        // is checked too, and put back.
        let s = Mem::new().before("rename target>old", |files| {
            let f = files.get_mut(TARGET).unwrap();
            f.bytes = b"their edit\n".to_vec();
            f.version += 100;
        });
        let e = run(&s).unwrap_err();
        assert!(has::<Unchanged>(&e), "{e:#}");
        assert_eq!(s.bytes(TARGET).unwrap(), b"their edit\n");
        assert!(s.others().is_empty(), "{:?}", s.others());
    }

    #[test]
    fn a_connection_lost_around_the_renames_names_what_is_where() {
        // Before the original is renamed aside: it stays, and the error says the outcome is
        // not known for sure, naming the staged copy.
        let s = Mem::new().fault("rename target>old", Fault::Drop);
        let e = run(&s).unwrap_err();
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");
        assert!(has::<Uncertain>(&e), "{e:#}");

        // Between the two renames: the original is kept, under the name the error gives.
        let s = Mem::new().fault("rename target>old", Fault::LoseReply);
        let e = run(&s).unwrap_err();
        let uncertain = e.downcast_ref::<Uncertain>().unwrap();
        assert!(s.bytes(TARGET).is_none());
        let old = uncertain.left.iter().find(|p| p.contains("-old-")).unwrap();
        assert_eq!(s.bytes(old).unwrap(), b"original contents\n");

        // Just after the staged copy took its place: the new file is there, the original
        // too, and both are named.
        let s = Mem::new()
            .fault("rename new>target", Fault::Refuse)
            .fault("rename new>target", Fault::LoseReply);
        let e = run(&s).unwrap_err();
        assert_eq!(s.bytes(TARGET).unwrap(), NEW);
        let uncertain = e.downcast_ref::<Uncertain>().unwrap();
        let old = uncertain.left.iter().find(|p| p.contains("-old-")).unwrap();
        assert_eq!(s.bytes(old).unwrap(), b"original contents\n");
    }

    /// [`Mem`], whose `rename`s of a staged copy listed in `lie` fail though done, with the
    /// connection still up, and whose owners are hidden when `hide_owner`.
    struct Lying<'a> {
        mem: &'a Mem,
        renames: Cell<u32>,
        lie: &'a [u32],
        hide_owner: bool,
    }

    impl<'a> Lying<'a> {
        fn new(mem: &'a Mem) -> Self {
            Self {
                mem,
                renames: Cell::new(0),
                lie: &[],
                hide_owner: false,
            }
        }
    }

    impl Stage for Lying<'_> {
        fn meta(&self, p: &str) -> Result<Option<Meta>> {
            let meta = self.mem.meta(p)?;
            Ok(meta.map(|m| match self.hide_owner {
                true => Meta { owner: None, ..m },
                false => m,
            }))
        }
        fn create(&self, p: &str, perm: u32) -> Result<()> {
            self.mem.create(p, perm)
        }
        fn write(&self, p: &str, f: &mut dyn Read, c: &AtomicBool) -> Result<u64> {
            self.mem.write(p, f, c)
        }
        fn set_owner(&self, p: &str, u: u32, g: u32) -> Result<()> {
            self.mem.set_owner(p, u, g)
        }
        fn set_perm(&self, p: &str, perm: u32) -> Result<()> {
            self.mem.set_perm(p, perm)
        }
        fn read(&self, p: &str) -> Result<Box<dyn Read + '_>> {
            self.mem.read(p)
        }
        fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
            let done = self.mem.rename(from, to, replace);
            if Mem::kind(from) == "new" {
                self.renames.set(self.renames.get() + 1);
                if self.lie.contains(&self.renames.get()) {
                    done?;
                    bail!("no reply");
                }
            }
            done
        }
        fn remove(&self, p: &str) -> Result<()> {
            self.mem.remove(p)
        }
    }

    #[test]
    fn a_lost_reply_with_the_connection_still_up_is_told_by_looking() {
        let (_dir, path) = local(NEW);
        let never = AtomicBool::new(false);
        // The rename into place happened, but its reply was lost; the staged copy is gone,
        // so it took the original's place.
        for (rename_over, lie) in [(true, &[1][..]), (false, &[2][..])] {
            let mut s = Mem::new();
            s.rename_over = rename_over;
            let lying = Lying {
                lie,
                ..Lying::new(&s)
            };
            let stamp = replace(&lying, &path, TARGET, s.stamp(TARGET), &never).unwrap();
            assert_eq!(stamp, Some(s.stamp(TARGET)));
            assert_eq!(s.bytes(TARGET).unwrap(), NEW);
            assert!(s.others().is_empty(), "{:?}", s.others());
        }
    }

    #[test]
    fn a_failed_rename_into_place_puts_the_original_back() {
        let s = Mem::new()
            .fault("rename new>target", Fault::Refuse)
            .fault("rename new>target", Fault::Refuse);
        let e = run(&s).unwrap_err();
        assert!(has::<Unchanged>(&e), "{e:#}");
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");
        assert!(s.others().is_empty(), "{:?}", s.others());
    }

    #[test]
    fn a_file_that_cannot_keep_its_owner_or_folder_is_left_to_write_in_place() {
        // Owned by someone else, and this user may not give files away.
        let s = Mem::new();
        s.put(TARGET, b"shared\n", (0, 100), 0o664);
        assert!(run(&s).unwrap().is_none());
        assert_eq!(s.bytes(TARGET).unwrap(), b"shared\n");
        assert!(s.others().is_empty());

        // Another group this user is in can be set, as `chgrp` allows.
        let mut s = Mem::new();
        s.chown = true;
        s.put(TARGET, b"group file\n", (1000, 200), 0o660);
        run(&s).unwrap().unwrap();
        let meta = s.meta_now(TARGET).unwrap();
        assert_eq!((meta.owner, meta.perm), (Some((1000, 200)), Some(0o660)));

        // A folder no file can be created in.
        let mut s = Mem::new();
        s.mkfile = false;
        assert!(run(&s).unwrap().is_none());
        assert_eq!(s.bytes(TARGET).unwrap(), b"original contents\n");

        // A server that does not tell owners.
        let s = Mem::new();
        let hidden = Lying {
            hide_owner: true,
            ..Lying::new(&s)
        };
        let (_dir, path) = local(NEW);
        let never = AtomicBool::new(false);
        let stamp = s.stamp(TARGET);
        assert!(replace(&hidden, &path, TARGET, stamp, &never)
            .unwrap()
            .is_none());
        assert!(s.others().is_empty());
    }

    #[test]
    fn a_new_file_is_published_only_complete_and_never_over_another() {
        let s = Mem::new();
        s.files.borrow_mut().remove(TARGET);
        let (_dir, path) = local(NEW);
        create(&s, &path, TARGET, &AtomicBool::new(false)).unwrap();
        assert_eq!(s.bytes(TARGET).unwrap(), NEW);
        assert!(s.others().is_empty());

        // Someone else creates it while the upload runs.
        let s = Mem::new().before("rename new>target", |files| {
            files.insert(
                TARGET.to_string(),
                File {
                    bytes: b"theirs\n".to_vec(),
                    version: 1,
                    owner: (1, 1),
                    perm: 0o644,
                },
            );
        });
        s.files.borrow_mut().remove(TARGET);
        let e = create(&s, &path, TARGET, &AtomicBool::new(false)).unwrap_err();
        assert!(!format!("{e:#}").is_empty());
        assert_eq!(s.bytes(TARGET).unwrap(), b"theirs\n");
        assert!(s.others().is_empty());

        // Interrupted part way: nothing appears under the file's name.
        let s = Mem::new().fault("write new", Fault::DropAt(50));
        s.files.borrow_mut().remove(TARGET);
        let e = create(&s, &path, TARGET, &AtomicBool::new(false)).unwrap_err();
        assert!(has::<LeftOver>(&e), "{e:#}");
        assert!(s.bytes(TARGET).is_none());
    }

    #[test]
    fn temporary_names_are_hidden_short_and_beside_the_file() {
        let long = format!("/a/b/{}.txt", "é".repeat(200));
        for (target, what) in [
            (TARGET, "new"),
            ("page.html", "old"),
            (long.as_str(), "new"),
        ] {
            let name = temp_name(target, what);
            let (dir, base) = name.rsplit_once('/').unwrap_or(("", &name));
            let want = target.rsplit_once('/').map_or("", |(d, _)| d);
            assert_eq!(dir, want);
            assert!(base.starts_with('.'), "{base}");
            assert!(base.contains(&format!(".oxidemerge-{what}-")), "{base}");
            assert!(base.len() <= 255, "{}", base.len());
        }
    }
}
