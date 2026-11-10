//! Saving a folder fetched over SFTP or FTP back to the server: its local copy is compared
//! with what the fetch read (its [`Manifest`]), and only what changed is written. A file
//! edited here is uploaded, a new one created, and a file or folder deleted here deleted
//! there, each only while the server's copy is still as it was fetched: one someone else
//! changed meanwhile is left alone and reported, as a single fetched file's upload refuses to
//! overwrite a change made on the server (see [`super::remote::upload`]).
//!
//! What a fetch left out (symbolic links, special files, names this machine cannot have) is
//! not in the manifest, and so never touched here.

use super::remote::{
    connect, file_hash, hash_of, join, still_as, Manifest, PartlyWritten, Remote, RemoteConnection,
};
use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// What [`save_folder`] did.
#[derive(Debug, Default)]
pub struct Saved {
    /// The folder as it now is on the server, for the next save to compare with.
    pub manifest: Manifest,
    pub uploaded: usize,
    pub deleted: usize,
    /// What could not be saved, one message each; everything else was.
    pub errors: Vec<String>,
    /// Files saved, whose original renamed aside could not be removed from the server once
    /// replaced: one message each, naming it for the user to delete.
    pub left_over: Vec<String>,
}

/// The files and folders of a local copy, by their `/`-separated paths relative to it.
#[derive(Default)]
struct Tree {
    files: BTreeMap<String, PathBuf>,
    dirs: BTreeSet<String>,
    /// Links and special files, which are not saved.
    skipped: BTreeSet<String>,
    /// What has a name that is not Unicode, which cannot be saved (shown as near as can be).
    unnamed: BTreeSet<String>,
}

impl Tree {
    /// Whether `rel`, or a folder it is in, is [`Tree::skipped`]: what the server has there
    /// is not to be deleted, as though it were gone here.
    fn shadowed(&self, rel: &str) -> bool {
        self.skipped.iter().any(|s| {
            rel == s
                || rel
                    .strip_prefix(s.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
    }
}

/// The files and folders of `local`: regular files and folders only, without following links,
/// as a fetch wrote them. Fails when `local` is gone or any of it cannot be read: what could not
/// be read would otherwise look deleted, and be deleted on the server.
fn local_tree(local: &Path) -> Result<Tree> {
    let unreadable = || {
        trf(
            "msg.remote_local_unreadable",
            &[("path", local.display().to_string())],
        )
    };
    if !std::fs::symlink_metadata(local).is_ok_and(|m| m.is_dir()) {
        bail!(unreadable());
    }
    let mut tree = Tree::default();
    let mut walk = walkdir::WalkDir::new(local)
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(e) = walk.next() {
        let e = e.with_context(unreadable)?;
        let Ok(rel) = e.path().strip_prefix(local) else {
            continue;
        };
        let named = rel
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect::<Option<Vec<_>>>();
        let Some(rel) = named.map(|parts| parts.join("/")) else {
            tree.unnamed.insert(
                rel.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
            if e.file_type().is_dir() {
                walk.skip_current_dir();
            }
            continue;
        };
        if e.file_type().is_dir() {
            tree.dirs.insert(rel);
        } else if e.file_type().is_file() {
            tree.files.insert(rel, e.into_path());
        } else {
            tree.skipped.insert(rel);
        }
    }
    Ok(tree)
}

/// Whether `local` differs from what `manifest` says was fetched into it (or last saved). One
/// that cannot be read counts as changed, so saving it tells why it cannot be saved.
pub fn changed(local: &Path, manifest: &Manifest) -> bool {
    let Ok(Tree { files, dirs, .. }) = local_tree(local) else {
        return true;
    };
    dirs != manifest.dirs
        || files.len() != manifest.files.len()
        || files.iter().any(|(rel, path)| {
            manifest
                .files
                .get(rel)
                .is_none_or(|(_, hash)| file_hash(path) != Some(*hash))
        })
}

/// Saves what changed in `local`, the copy of the server's folder `remote_dir` a fetch
/// recorded in `manifest`, back to the server, over one connection. Fails only when it cannot
/// connect; what it could not save once connected is in [`Saved::errors`].
pub fn save_folder(
    conn: &RemoteConnection,
    local: &Path,
    remote_dir: &str,
    manifest: &Manifest,
    cancel: &AtomicBool,
) -> Result<Saved> {
    let remote = connect(conn, cancel)?;
    Ok(save_with(
        &*remote,
        local,
        remote_dir,
        manifest,
        conn.options.in_place,
        cancel,
    ))
}

fn save_with(
    r: &dyn Remote,
    local: &Path,
    remote_dir: &str,
    manifest: &Manifest,
    in_place: bool,
    cancel: &AtomicBool,
) -> Saved {
    let remote_dir = match remote_dir.trim_end_matches('/') {
        "" => "/",
        dir => dir,
    };
    let at = |rel: &str| join(remote_dir, rel);
    let mut out = Saved {
        manifest: manifest.clone(),
        ..Saved::default()
    };
    let tree = match local_tree(local) {
        Ok(tree) => tree,
        Err(e) => {
            out.errors.push(format!("{e:#}"));
            return out;
        }
    };
    let stopped = |out: &mut Saved| {
        let stop = cancel.load(Ordering::Relaxed);
        if stop && !out.errors.iter().any(|e| e == tr("msg.sftp_cancelled")) {
            out.errors.push(tr("msg.sftp_cancelled").to_string());
        }
        stop
    };
    // What is here but cannot be saved, where it would otherwise be lost: a name that is not
    // Unicode, or a link or special file where the server has a file or folder.
    for s in &tree.skipped {
        let in_manifest = |rel: &String| rel == s || rel.starts_with(&format!("{s}/"));
        if manifest.files.keys().any(in_manifest) || manifest.dirs.iter().any(in_manifest) {
            out.errors
                .push(trf("msg.remote_local_skipped", &[("path", at(s))]));
        }
    }
    for s in &tree.unnamed {
        out.errors
            .push(trf("msg.remote_local_skipped", &[("path", at(s))]));
    }
    let gone_files: Vec<(&String, &(super::remote::RemoteStamp, u64))> = manifest
        .files
        .iter()
        .filter(|(rel, _)| !tree.files.contains_key(*rel) && !tree.shadowed(rel))
        .collect();
    let gone_dirs: Vec<&String> = manifest
        .dirs
        .difference(&tree.dirs)
        .filter(|rel| !tree.shadowed(rel))
        .collect();

    // Files a folder made here takes the place of go first, so the folder can be made; every
    // other deletion waits until all that is new is written, so that a file renamed here is
    // never only deleted on the server.
    let (in_the_way, gone_files): (Vec<_>, Vec<_>) = gone_files
        .into_iter()
        .partition(|(rel, _)| tree.dirs.contains(*rel));
    for (rel, (stamp, hash)) in in_the_way {
        if stopped(&mut out) {
            break;
        }
        delete_file(r, &at(rel), rel, *stamp, *hash, cancel, &mut out);
    }

    // Folders made here, each before what is in it.
    let mut failed_dirs: Vec<String> = Vec::new();
    for rel in tree.dirs.difference(&manifest.dirs) {
        if stopped(&mut out) {
            break;
        }
        if failed_dirs
            .iter()
            .any(|d| rel.starts_with(&format!("{d}/")))
        {
            continue;
        }
        let path = at(rel);
        let made = r.make_dir(&path).or_else(|e| match r.kind(&path) {
            Ok(Some(super::remote::EntryKind::Dir)) => Ok(()),
            _ => Err(e),
        });
        match made {
            Ok(()) => {
                out.manifest.dirs.insert(rel.clone());
            }
            Err(e) => {
                out.errors.push(format!("{e:#}"));
                failed_dirs.push(rel.clone());
            }
        }
    }

    // Files edited or made here; those where a folder deleted here still is, once it is gone.
    // What could not be written is remembered by its bytes: it may be a file moved here, whose
    // old place then must not be deleted on the server yet.
    let mut unsaved = std::collections::HashSet::new();
    let (failed, files): (Vec<_>, Vec<_>) = tree.files.iter().partition(|(rel, _)| {
        failed_dirs
            .iter()
            .any(|d| rel.starts_with(&format!("{d}/")))
    });
    unsaved.extend(failed.into_iter().filter_map(|(_, p)| file_hash(p)));
    let (later, now): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|(rel, _)| gone_dirs.contains(rel));
    for (rel, local_path) in now {
        if stopped(&mut out) {
            break;
        }
        unsaved.extend(upload_file(
            r,
            &at(rel),
            rel,
            local_path,
            manifest,
            in_place,
            cancel,
            &mut out,
        ));
    }

    // Files deleted here, while each is still on the server as fetched.
    for (rel, (stamp, hash)) in gone_files {
        if stopped(&mut out) {
            break;
        }
        if !unsaved.contains(hash) {
            delete_file(r, &at(rel), rel, *stamp, *hash, cancel, &mut out);
        }
    }

    // Folders deleted here, each after what was in it.
    for rel in gone_dirs.into_iter().rev() {
        if stopped(&mut out) {
            break;
        }
        match r.remove_dir(&at(rel)) {
            Ok(()) => {
                out.manifest.dirs.remove(rel);
                out.deleted += 1;
            }
            Err(e) => out.errors.push(format!("{e:#}")),
        }
    }

    for (rel, local_path) in later {
        if stopped(&mut out) {
            break;
        }
        let _ = upload_file(
            r,
            &at(rel),
            rel,
            local_path,
            manifest,
            in_place,
            cancel,
            &mut out,
        );
    }
    out
}

/// Deletes the file `path` (`rel` in the manifest) deleted here, if it is still on the server
/// as fetched (`stamp`, its bytes hashing to `hash`).
fn delete_file(
    r: &dyn Remote,
    path: &str,
    rel: &str,
    stamp: super::remote::RemoteStamp,
    hash: u64,
    cancel: &AtomicBool,
    out: &mut Saved,
) {
    if r.stamp(path).is_ok_and(|now| now.is_none()) {
        out.manifest.files.remove(rel);
        return;
    }
    match still_as(r, path, stamp, hash, cancel) {
        Ok(true) => match r.remove_file(path) {
            Ok(()) => {
                out.manifest.files.remove(rel);
                out.deleted += 1;
            }
            Err(e) => out.errors.push(format!("{e:#}")),
        },
        Ok(false) => out
            .errors
            .push(trf("msg.remote_not_deleted", &[("path", path.to_string())])),
        Err(e) => out.errors.push(format!("{e:#}")),
    }
}

/// Uploads `local_path`, the file `rel` here, to `path` if it is new or changed since
/// `manifest` was made, and the server's copy did not change meanwhile. Returns the hash of
/// its bytes when it was not written.
#[allow(clippy::too_many_arguments)]
fn upload_file(
    r: &dyn Remote,
    path: &str,
    rel: &str,
    local_path: &Path,
    manifest: &Manifest,
    in_place: bool,
    cancel: &AtomicBool,
    out: &mut Saved,
) -> Option<u64> {
    let Some(hash) = file_hash(local_path) else {
        out.errors.push(trf(
            "msg.sftp_cannot_read",
            &[("path", local_path.display().to_string())],
        ));
        return None;
    };
    // A new file is refused by `put` if someone made one there meanwhile.
    let expected = match manifest.files.get(rel) {
        Some((_, fetched)) if *fetched == hash => return None,
        Some((stamp, fetched)) => {
            // The stamp alone may not tell a change (`put` checks that much).
            if stamp.is_weak() {
                match still_as(r, path, *stamp, *fetched, cancel) {
                    Ok(true) => {}
                    Ok(false) => {
                        out.errors.push(trf(
                            "msg.sftp_changed_on_server",
                            &[("path", path.to_string())],
                        ));
                        return Some(hash);
                    }
                    Err(e) => {
                        out.errors.push(format!("{e:#}"));
                        return Some(hash);
                    }
                }
            }
            Some(*stamp)
        }
        None => None,
    };
    match r.put_saved(local_path, path, expected, in_place, cancel) {
        Ok(super::remote_stage::Saved { stamp, left_over }) => {
            out.left_over.extend(left_over.map(|l| l.to_string()));
            out.manifest.files.insert(rel.to_string(), (stamp, hash));
            out.uploaded += 1;
            None
        }
        Err(e) => {
            // Expected as this upload left it, so saving again finishes it rather than
            // being refused; what it holds now is read back, as its stamp may not tell.
            if let Some(stamp) = e.downcast_ref::<PartlyWritten>().and_then(|p| p.stamp) {
                if let Ok(now) = r.open_read(path).and_then(|f| hash_of(f, cancel)) {
                    out.manifest.files.insert(rel.to_string(), (stamp, now));
                }
            }
            out.errors.push(format!("{e:#}"));
            Some(hash)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::remote::{self, fake, Auth, Protocol};

    fn conn() -> RemoteConnection {
        RemoteConnection {
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        }
    }

    fn server() -> fake::Server {
        fake::Server::new()
            .dir("/srv")
            .dir("/srv/data")
            .file("/srv/data/a.txt", "a")
            .file("/srv/data/b.txt", "b")
            .dir("/srv/data/old")
            .file("/srv/data/old/c.txt", "c")
            .dir("/srv/data/keep")
            .file("/srv/data/keep/d.txt", "d")
    }

    #[test]
    fn saving_a_folder_writes_only_what_changed_here() {
        let never = AtomicBool::new(false);
        fake::install(server());
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        assert_eq!(manifest.files.len(), 4);
        assert!(manifest.dirs.contains("old") && manifest.dirs.contains("keep"));
        assert!(!changed(&f.local, &manifest));

        let l = &f.local;
        std::fs::write(l.join("a.txt"), "a edited").unwrap();
        std::fs::remove_file(l.join("b.txt")).unwrap();
        std::fs::remove_dir_all(l.join("old")).unwrap();
        std::fs::create_dir_all(l.join("new/deeper")).unwrap();
        std::fs::write(l.join("new/deeper/e.txt"), "e").unwrap();
        std::fs::write(l.join("f.txt"), "f").unwrap();
        assert!(changed(l, &manifest));

        let saved = save_folder(&conn(), l, "/srv/data/", &manifest, &never).unwrap();
        assert!(saved.errors.is_empty(), "{:?}", saved.errors);
        assert_eq!((saved.uploaded, saved.deleted), (3, 3));
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"a edited".to_vec()))
        );
        assert_eq!(
            fake::entry("/srv/data/new/deeper/e.txt"),
            Some(Some(b"e".to_vec()))
        );
        assert_eq!(fake::entry("/srv/data/new"), Some(None));
        assert_eq!(fake::entry("/srv/data/f.txt"), Some(Some(b"f".to_vec())));
        assert!(fake::was_removed("/srv/data/b.txt"));
        assert!(fake::was_removed("/srv/data/old/c.txt"));
        assert!(fake::was_removed("/srv/data/old"));
        assert_eq!(fake::upload_count("/srv/data/keep/d.txt"), 0, "unchanged");
        assert!(!changed(l, &saved.manifest));

        // Saving again with nothing changed writes nothing.
        let again = save_folder(&conn(), l, "/srv/data", &saved.manifest, &never).unwrap();
        assert_eq!((again.uploaded, again.deleted), (0, 0));
        assert_eq!(again.manifest, saved.manifest);
        fake::uninstall();
    }

    #[test]
    fn what_changed_on_the_server_meanwhile_is_left_alone_and_reported() {
        let never = AtomicBool::new(false);
        fake::install(server().write_fails("/srv/data/keep/d.txt"));
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        let l = &f.local;
        fake::change("/srv/data/a.txt", "theirs");
        fake::change("/srv/data/b.txt", "theirs too");
        fake::change("/srv/data/g.txt", "new there");
        std::fs::write(l.join("a.txt"), "mine").unwrap();
        std::fs::remove_file(l.join("b.txt")).unwrap();
        std::fs::write(l.join("g.txt"), "new here").unwrap();
        std::fs::write(l.join("keep/d.txt"), "cannot write").unwrap();
        std::fs::write(l.join("h.txt"), "fine").unwrap();

        let saved = save_folder(&conn(), l, "/srv/data", &manifest, &never).unwrap();
        assert_eq!(saved.errors.len(), 4, "{:?}", saved.errors);
        let all = saved.errors.join("\n");
        assert!(all.contains("/srv/data/a.txt") && all.contains("/srv/data/b.txt"));
        assert!(all.contains("/srv/data/g.txt") && all.contains("/srv/data/keep/d.txt"));
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"theirs".to_vec()))
        );
        assert_eq!(
            fake::entry("/srv/data/b.txt"),
            Some(Some(b"theirs too".to_vec()))
        );
        assert_eq!(
            fake::entry("/srv/data/g.txt"),
            Some(Some(b"new there".to_vec()))
        );
        assert_eq!(fake::entry("/srv/data/h.txt"), Some(Some(b"fine".to_vec())));
        assert_eq!(saved.uploaded, 1);
        // What was not saved still counts as changed, for *Save* to try again.
        assert!(changed(l, &saved.manifest));

        // A save already cancelled writes nothing.
        let cancelled = AtomicBool::new(true);
        let none = save_folder(&conn(), l, "/srv/data", &saved.manifest, &cancelled).unwrap();
        assert_eq!(none.errors, [tr("msg.sftp_cancelled").to_string()]);
        assert_eq!((none.uploaded, none.deleted), (0, 0));
        fake::uninstall();
    }

    #[test]
    fn fetching_again_replaces_the_local_copy_with_the_servers() {
        let never = AtomicBool::new(false);
        fake::install(server());
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        std::fs::write(f.local.join("a.txt"), "local edit").unwrap();
        std::fs::write(f.local.join("stray.txt"), "x").unwrap();
        fake::change("/srv/data/b.txt", "newer");
        let staged = remote::refetch(&conn(), "/srv/data", &f.local, &never).unwrap();
        assert!(staged.is_dir);
        assert_eq!(std::fs::read_to_string(f.local.join("a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_to_string(f.local.join("b.txt")).unwrap(),
            "newer"
        );
        assert!(!f.local.join("stray.txt").exists());
        assert!(!changed(&f.local, &staged.manifest.unwrap()));
        // Nothing of the download is left beside the copy.
        let names: Vec<_> = std::fs::read_dir(f.temp.path())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(names.len(), 1);

        // A failed fetch leaves the old copy as it was.
        std::fs::write(f.local.join("a.txt"), "kept").unwrap();
        fake::install(fake::Server::new().failing("connection refused"));
        assert!(remote::refetch(&conn(), "/srv/data", &f.local, &never).is_err());
        assert_eq!(
            std::fs::read_to_string(f.local.join("a.txt")).unwrap(),
            "kept"
        );

        // A single file is fetched again the same way.
        fake::install(server());
        let f = remote::fetch(&conn(), "/srv/data/a.txt", &never).unwrap();
        std::fs::write(&f.local, "edit").unwrap();
        fake::change("/srv/data/a.txt", "server");
        let staged = remote::refetch(&conn(), "/srv/data/a.txt", &f.local, &never).unwrap();
        assert!(!staged.is_dir);
        assert_eq!(std::fs::read_to_string(&f.local).unwrap(), "server");
        assert_eq!(staged.stamp.unwrap().size, 6);
        fake::uninstall();
    }

    #[test]
    fn a_local_copy_gone_or_not_readable_in_full_deletes_nothing_on_the_server() {
        let never = AtomicBool::new(false);
        fake::install(server());
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        let unreadable = trf(
            "msg.remote_local_unreadable",
            &[("path", f.local.display().to_string())],
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let keep = f.local.join("keep");
            std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Whoever may read it anyway (root) has nothing here to test.
            if std::fs::read_dir(&keep).is_err() {
                assert!(changed(&f.local, &manifest));
                let saved = save_folder(&conn(), &f.local, "/srv/data", &manifest, &never).unwrap();
                assert_eq!(saved.errors.len(), 1, "{:?}", saved.errors);
                assert!(
                    saved.errors[0].starts_with(&unreadable),
                    "{:?}",
                    saved.errors
                );
                assert_eq!((saved.uploaded, saved.deleted), (0, 0));
            }
            std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        std::fs::remove_dir_all(&f.local).unwrap();
        assert!(changed(&f.local, &manifest));
        let saved = save_folder(&conn(), &f.local, "/srv/data", &manifest, &never).unwrap();
        assert_eq!(saved.errors, [unreadable]);
        assert_eq!((saved.uploaded, saved.deleted), (0, 0));
        assert_eq!(saved.manifest, manifest);
        for path in ["a.txt", "b.txt", "old/c.txt", "keep/d.txt"] {
            assert!(
                fake::entry(&format!("/srv/data/{path}")).is_some(),
                "{path}"
            );
        }
        fake::uninstall();
    }

    /// Without modified times (as from an FTP server without `MDTM`), a change on the server
    /// that keeps a file's size is told by its contents.
    #[test]
    fn a_change_on_the_server_keeping_the_size_is_seen_without_modified_times() {
        let never = AtomicBool::new(false);
        fake::install(server().weak_stamps());
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        let l = &f.local;
        fake::change("/srv/data/a.txt", "A");
        fake::change("/srv/data/b.txt", "B");
        std::fs::write(l.join("a.txt"), "mine").unwrap();
        std::fs::remove_file(l.join("b.txt")).unwrap();
        std::fs::write(l.join("keep/d.txt"), "d edited").unwrap();
        std::fs::remove_file(l.join("old/c.txt")).unwrap();

        let saved = save_folder(&conn(), l, "/srv/data", &manifest, &never).unwrap();
        assert_eq!(saved.errors.len(), 2, "{:?}", saved.errors);
        let all = saved.errors.join("\n");
        assert!(all.contains("/srv/data/a.txt") && all.contains("/srv/data/b.txt"));
        assert_eq!(fake::entry("/srv/data/a.txt"), Some(Some(b"A".to_vec())));
        assert_eq!(fake::entry("/srv/data/b.txt"), Some(Some(b"B".to_vec())));
        // What did not change there is still saved.
        assert_eq!(
            fake::entry("/srv/data/keep/d.txt"),
            Some(Some(b"d edited".to_vec()))
        );
        assert!(fake::was_removed("/srv/data/old/c.txt"));
        assert_eq!((saved.uploaded, saved.deleted), (1, 1));
        fake::uninstall();
    }

    #[test]
    fn a_file_written_half_way_is_finished_by_saving_again() {
        let never = AtomicBool::new(false);
        fake::install(server().partly_fails("/srv/data/a.txt"));
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        std::fs::write(f.local.join("a.txt"), "a edited").unwrap();

        let saved = save_folder(&conn(), &f.local, "/srv/data", &manifest, &never).unwrap();
        let partly = trf(
            "msg.remote_partly_written",
            &[("path", "/srv/data/a.txt".to_string())],
        );
        assert_eq!(saved.errors.len(), 1, "{:?}", saved.errors);
        assert!(saved.errors[0].starts_with(&partly), "{:?}", saved.errors);
        assert_eq!(fake::entry("/srv/data/a.txt"), Some(Some(b"a ed".to_vec())));
        assert!(changed(&f.local, &saved.manifest));

        let again = save_folder(&conn(), &f.local, "/srv/data", &saved.manifest, &never).unwrap();
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(again.uploaded, 1);
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"a edited".to_vec()))
        );
        assert!(!changed(&f.local, &again.manifest));
        fake::uninstall();
    }

    /// Where case does not tell names apart (as by default on macOS and Windows), only the
    /// first of `A.txt` and `a.txt` is fetched, and saving writes neither over the other.
    #[test]
    fn names_told_apart_only_by_case_are_never_saved_over_each_other() {
        let never = AtomicBool::new(false);
        fake::install(
            fake::Server::new()
                .dir("/srv")
                .dir("/srv/data")
                .file("/srv/data/A.txt", "upper")
                .file("/srv/data/a.txt", "lower"),
        );
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        let here = std::fs::read_dir(&f.local).unwrap().count();
        assert_eq!(manifest.files.len(), here);
        for (rel, (_, hash)) in &manifest.files {
            assert_eq!(file_hash(&f.local.join(rel)), Some(*hash), "{rel}");
        }
        assert!(!changed(&f.local, &manifest));
        let saved = save_folder(&conn(), &f.local, "/srv/data", &manifest, &never).unwrap();
        assert!(saved.errors.is_empty(), "{:?}", saved.errors);
        assert_eq!((saved.uploaded, saved.deleted), (0, 0));
        assert_eq!(
            fake::entry("/srv/data/A.txt"),
            Some(Some(b"upper".to_vec()))
        );
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"lower".to_vec()))
        );
        fake::uninstall();
    }

    #[test]
    fn moves_folders_turned_files_and_links_here_lose_nothing_on_the_server() {
        let never = AtomicBool::new(false);
        fake::install(server().write_fails("/srv/data/moved.txt"));
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let manifest = f.manifest.clone().unwrap();
        let l = &f.local;
        std::fs::remove_dir_all(l.join("old")).unwrap();
        std::fs::write(l.join("old"), "now a file").unwrap();
        // Moved, but the new place cannot be written: the old one is kept.
        std::fs::rename(l.join("a.txt"), l.join("moved.txt")).unwrap();
        #[cfg(unix)]
        {
            std::fs::remove_dir_all(l.join("keep")).unwrap();
            std::os::unix::fs::symlink("/nowhere", l.join("keep")).unwrap();
        }

        let saved = save_folder(&conn(), l, "/srv/data", &manifest, &never).unwrap();
        assert_eq!(
            fake::entry("/srv/data/old"),
            Some(Some(b"now a file".to_vec()))
        );
        assert!(fake::was_removed("/srv/data/old/c.txt"));
        assert_eq!(fake::entry("/srv/data/a.txt"), Some(Some(b"a".to_vec())));
        let all = saved.errors.join("\n");
        assert!(all.contains("/srv/data/moved.txt"), "{all}");
        #[cfg(unix)]
        {
            assert_eq!(saved.errors.len(), 2, "{all}");
            assert!(all.contains(&trf(
                "msg.remote_local_skipped",
                &[("path", "/srv/data/keep".to_string())]
            )));
            assert_eq!(
                fake::entry("/srv/data/keep/d.txt"),
                Some(Some(b"d".to_vec()))
            );
        }
        assert!(changed(l, &saved.manifest));
        fake::uninstall();
    }

    #[test]
    fn no_backups_are_made_inside_a_fetched_folder() {
        let never = AtomicBool::new(false);
        fake::install(server());
        let f = remote::fetch(&conn(), "/srv/data", &never).unwrap();
        let file = f.local.join("a.txt");
        assert!(!crate::core::fsutil::backs_up(&file, true));
        let real = std::fs::canonicalize(&f.local).unwrap().join("a.txt");
        assert!(!crate::core::fsutil::backs_up(&real, true));
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(crate::core::fsutil::backs_up(
            &elsewhere.path().join("a.txt"),
            true
        ));
        fake::uninstall();
    }
}
