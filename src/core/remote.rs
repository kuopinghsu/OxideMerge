//! Files and folders on another machine, read and written over SFTP: an SSH connection this
//! program makes itself, authenticated by a password or a private key, to a path the server
//! sees under the signed-in user's own account.
//!
//! A file or folder opened this way is staged in a temporary local copy first, the same as a
//! Git revision or an extracted archive is. Unlike those, a single file stays writable: saving
//! it uploads the local copy back over a fresh connection made with the same credentials kept
//! with its tab, since a connection is not worth holding open for as long as a tab might stay
//! open (through sleep, a changed network, or a server that closes idle sessions). A folder is
//! read-only, as a Git revision or a web page is: there is no equivalent here yet of the
//! archive repacking a folder compare's own files use to save back what changed.
//!
//! The server's host key is checked against the ones this program has seen before (trust on
//! first connection, kept in [`known_hosts_path`]), the same protection `ssh` itself gives:
//! without it, a connection could be quietly handed to an attacker between the program and the
//! real server before a password or a key ever reaches either end. See [`verify_host_key`].
//!
//! A fetch is bounded: see [`Budget`]. The one real network path, [`RemoteFs`] for
//! `ssh2::Sftp`, is a thin seam so the rest — walking a folder, the budget, cancelling midway —
//! is tested without a real connection, the same way [`fake`] stands in for one entirely in
//! GUI tests.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How a connection is authenticated.
#[derive(Clone)]
pub enum Auth {
    Password(String),
    /// A private key file and its passphrase (empty for a key that needs none).
    Key {
        path: PathBuf,
        passphrase: String,
    },
}

/// Enough to open an SFTP connection again. Kept with a tab for as long as it is open so that
/// *Save* can write a file back to it; never written to the settings file, a saved workspace
/// or a restored session, since that would save a password or a key's passphrase to disk.
#[derive(Clone)]
pub struct RemoteConnection {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
}

/// How long connecting, looking up the address, and authenticating may each take before the
/// server is treated as unreachable, and how long a single read or write of the connection may
/// then take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// `host`'s address, looked up with its own bound on how long that may take: `ToSocketAddrs`
/// gives the operating system's resolver no timeout of its own, so without this a lookup that
/// hangs (an unreachable DNS server, say) would hang the fetch past `CONNECT_TIMEOUT` and past
/// *Cancel*, which is only checked between the chunks of a transfer, not during this or the
/// connection and authentication steps after it. The lookup itself still runs to completion on
/// its own thread even after timing out here; that thread is harmless once nothing reads its
/// result.
fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let (tx, rx) = mpsc::channel();
    let owned = host.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(
            (owned.as_str(), port)
                .to_socket_addrs()
                .map(|mut a| a.next()),
        );
    });
    match rx.recv_timeout(CONNECT_TIMEOUT) {
        Ok(Ok(Some(addr))) => Ok(addr),
        Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("{host}: cannot look up this address")
        }
        Ok(Err(e)) => Err(e).with_context(|| format!("{host}: cannot look up this address")),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!("{host}: looking up this address took too long")
        }
    }
}

/// The file this program remembers server host keys in, to notice if one ever changes: one line
/// per server, `host:port` and the key in hexadecimal, separated by one space. Blank lines and
/// lines starting with `#` are ignored, so entries can be removed by hand (see
/// [`verify_host_key`]) without disturbing the rest of the file.
fn known_hosts_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("known_hosts"))
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn load_known_hosts(path: &Path) -> HashMap<String, Vec<u8>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (id, hex) = line.split_once(' ')?;
            Some((id.to_string(), decode_hex(hex)?))
        })
        .collect()
}

fn save_known_hosts(path: &Path, hosts: &HashMap<String, Vec<u8>>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut ids: Vec<&String> = hosts.keys().collect();
    ids.sort();
    let text: String = ids
        .iter()
        .map(|id| format!("{id} {}\n", encode_hex(&hosts[*id])))
        .collect();
    std::fs::write(path, text)
}

/// What [`verify_host_key`] decided about a server's host key.
#[derive(Debug, PartialEq, Eq)]
enum HostKeyCheck {
    /// Matches the key this program saw the last time it connected to this server.
    Trusted,
    /// The first time this program has connected to this server: trusted and remembered.
    New,
    /// Does not match the key this program saw the last time: a changed host key, a different
    /// server now answering at the same address, or someone between this program and the
    /// server.
    Changed,
}

fn check_host_key(known: &HashMap<String, Vec<u8>>, id: &str, key: &[u8]) -> HostKeyCheck {
    match known.get(id) {
        None => HostKeyCheck::New,
        Some(k) if k == key => HostKeyCheck::Trusted,
        Some(_) => HostKeyCheck::Changed,
    }
}

/// Checks `sess`'s host key against the ones this program has connected to before, trusting
/// and remembering it the first time (see [`known_hosts_path`]), the same way `ssh` itself
/// does with `~/.ssh/known_hosts`. Called right after the handshake and before a password or a
/// key ever reaches the connection, so a server this fails for never sees them.
///
/// Unlike `ssh`, there is no prompt to trust a changed key anyway: that choice is easy to
/// click past without reading it, which defeats the point of checking at all. A key that
/// genuinely changed (a reinstalled server, a rotated key) is trusted again only by removing
/// its line from the known hosts file by hand.
fn verify_host_key(sess: &ssh2::Session, conn: &RemoteConnection) -> Result<()> {
    let (key, _) = sess.host_key().context("the server sent no host key")?;
    let id = format!("{}:{}", conn.host, conn.port);
    let path = known_hosts_path();
    let known = path.as_deref().map(load_known_hosts).unwrap_or_default();
    match check_host_key(&known, &id, key) {
        HostKeyCheck::Trusted => Ok(()),
        HostKeyCheck::New => {
            if let Some(path) = &path {
                let mut known = known;
                known.insert(id, key.to_vec());
                // Not trusting the connection if this fails would make a read-only settings
                // folder unable to connect at all; the key is still checked, just not
                // remembered between runs.
                let _ = save_known_hosts(path, &known);
            }
            Ok(())
        }
        HostKeyCheck::Changed => bail!(
            "{id}'s key has changed since the last connection to it. This could mean someone \
            is between this program and the server, or that the server was reinstalled or its \
            key renewed. If you are sure the server is genuine, remove its line (starting with \
            \"{id} \") from {}",
            path.as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the known hosts file".to_string()),
        ),
    }
}

/// Opens an SFTP session authenticated as `conn` says.
fn session(conn: &RemoteConnection) -> Result<ssh2::Sftp> {
    let addr = resolve(&conn.host, conn.port)?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .with_context(|| format!("{}:{} is not reachable", conn.host, conn.port))?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    let mut sess = ssh2::Session::new().context("cannot start an SSH session")?;
    sess.set_timeout(CONNECT_TIMEOUT.as_millis() as u32);
    sess.set_tcp_stream(tcp);
    sess.handshake().context("the SSH handshake failed")?;
    verify_host_key(&sess, conn)?;
    match &conn.auth {
        Auth::Password(password) => sess
            .userauth_password(&conn.user, password)
            .context("the server refused this user name and password")?,
        Auth::Key { path, passphrase } => {
            let passphrase = (!passphrase.is_empty()).then_some(passphrase.as_str());
            sess.userauth_pubkey_file(&conn.user, None, path, passphrase)
                .context(
                    "could not sign in with this key: check the key file, its passphrase, and \
                    that the server accepts it",
                )?
        }
    }
    if !sess.authenticated() {
        bail!("authentication failed");
    }
    sess.sftp().context("the server has no SFTP subsystem")
}

/// Cooperative cancellation, checked between chunks of a transfer: a network read or write
/// itself cannot be interrupted, but a large file then stops within one chunk of it.
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    Ok(())
}

const CHUNK: usize = 64 * 1024;

/// How much of one fetch has been read so far, so a mistaken or very large path cannot fill
/// the disk or hang indefinitely: generous enough for ordinary use, bounded the same way the
/// built-in document readers bound how much of a file they read (see
/// `crate::core::extract::MAX_FILE`). The limits are fields, not just constants, so a test can
/// hit them without actually downloading gigabytes or tens of thousands of files.
struct Budget {
    bytes: u64,
    files: usize,
    max_bytes: u64,
    max_files: usize,
}

/// The most bytes a single fetch (one file, or a whole folder together) reads from the server.
const MAX_REMOTE_BYTES: u64 = 4 << 30;
/// The most files a single folder fetch reads from the server.
const MAX_REMOTE_FILES: usize = 50_000;

impl Budget {
    fn new() -> Self {
        Self {
            bytes: 0,
            files: 0,
            max_bytes: MAX_REMOTE_BYTES,
            max_files: MAX_REMOTE_FILES,
        }
    }

    #[cfg(test)]
    fn with_limits(max_bytes: u64, max_files: usize) -> Self {
        Self {
            bytes: 0,
            files: 0,
            max_bytes,
            max_files,
        }
    }

    fn take_file(&mut self) -> Result<()> {
        self.files += 1;
        if self.files > self.max_files {
            bail!(
                "stopped after {} files; fetch a smaller folder",
                self.max_files
            );
        }
        Ok(())
    }

    fn take_bytes(&mut self, n: u64) -> Result<()> {
        self.bytes += n;
        if self.bytes > self.max_bytes {
            bail!(
                "stopped after {} GB; fetch a smaller file or folder",
                self.max_bytes >> 30
            );
        }
        Ok(())
    }
}

/// What [`download_file`] and [`download_dir`] need from the server: read a file, or list a
/// folder's entries with whether each is a file or a folder (anything else, such as a symbolic
/// link, is left out, the same as a folder compare leaves out what it does not understand on
/// either side). A seam so they can be tested without a real SFTP connection: [`fake::Server`]
/// implements this trait too, in memory.
trait RemoteFs {
    fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>>;
    /// `(name, is_dir, is_file)` for each entry of `remote`, a folder.
    fn list_dir(&self, remote: &str) -> Result<Vec<(String, bool, bool)>>;
}

impl RemoteFs for ssh2::Sftp {
    fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(
            self.open(Path::new(remote))
                .with_context(|| format!("{remote}: cannot open it"))?,
        ))
    }

    /// Entries without a name valid in UTF-8 are left out too, along with symbolic links and
    /// other special entries ([`RemoteFs::list_dir`]): this program's paths are UTF-8
    /// throughout, the same as a local folder compare already assumes of a file's name.
    fn list_dir(&self, remote: &str) -> Result<Vec<(String, bool, bool)>> {
        Ok(self
            .readdir(Path::new(remote))
            .with_context(|| format!("{remote}: cannot list it"))?
            .into_iter()
            .filter_map(|(path, stat)| {
                let name = path.file_name()?.to_str()?.to_string();
                Some((name, stat.is_dir(), stat.is_file()))
            })
            .collect())
    }
}

fn download_file(
    fs: &dyn RemoteFs,
    remote: &str,
    local: &Path,
    cancel: &AtomicBool,
    budget: &mut Budget,
) -> Result<()> {
    budget.take_file()?;
    let mut from = fs.open_read(remote)?;
    let mut to = std::fs::File::create(local)
        .with_context(|| format!("{}: cannot write it", local.display()))?;
    let mut buf = [0u8; CHUNK];
    loop {
        check(cancel)?;
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        budget.take_bytes(n as u64)?;
        to.write_all(&buf[..n])?;
    }
}

/// `remote`'s files and subfolders, downloaded into `local` (already created); symbolic links
/// and other special entries are left out, the same as a folder compare leaves out what it does
/// not understand on either side.
fn download_dir(
    fs: &dyn RemoteFs,
    remote: &str,
    local: &Path,
    cancel: &AtomicBool,
    budget: &mut Budget,
) -> Result<()> {
    check(cancel)?;
    std::fs::create_dir_all(local)?;
    for (name, is_dir, is_file) in fs.list_dir(remote)? {
        let child_remote = join(remote, &name);
        let child_local = local.join(&name);
        if is_dir {
            download_dir(fs, &child_remote, &child_local, cancel, budget)?;
        } else if is_file {
            download_file(fs, &child_remote, &child_local, cancel, budget)?;
        }
    }
    Ok(())
}

/// `base` and `name` as one SFTP path: always `/`-separated, whatever this machine's own
/// separator is, since the server is not necessarily this kind of machine.
fn join(base: &str, name: &str) -> String {
    format!("{}/{name}", base.trim_end_matches('/'))
}

/// The last path segment of an SFTP path, trimming a trailing `/` first so a path to a folder
/// given that way (`/srv/data/`) still names it `data`, not nothing.
fn remote_file_name(remote_path: &str) -> std::ffi::OsString {
    Path::new(remote_path.trim_end_matches('/'))
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "remote".into())
}

/// Fetches the file or folder `remote_path` into a new temporary folder (deleted when it is
/// dropped), returning it together with the local copy and whether it was a folder. Setting
/// `cancel` stops it, and it fails. Bounded by [`MAX_REMOTE_BYTES`] and [`MAX_REMOTE_FILES`].
pub fn fetch(
    conn: &RemoteConnection,
    remote_path: &str,
    cancel: &AtomicBool,
) -> Result<(tempfile::TempDir, PathBuf, bool)> {
    #[cfg(test)]
    if let Some(result) = fake::fetch(remote_path, cancel) {
        return result;
    }
    let sftp = session(conn)?;
    let stat = sftp
        .stat(Path::new(remote_path))
        .with_context(|| format!("{remote_path}: cannot read it"))?;
    let is_dir = stat.is_dir();
    let temp = tempfile::Builder::new()
        .prefix("oxidemerge-sftp-")
        .tempdir()?;
    let local = temp.path().join(remote_file_name(remote_path));
    let mut budget = Budget::new();
    if is_dir {
        download_dir(&sftp, remote_path, &local, cancel, &mut budget)?;
    } else {
        download_file(&sftp, remote_path, &local, cancel, &mut budget)?;
    }
    Ok((temp, local, is_dir))
}

/// Writes `local_path`'s bytes to `remote_path` on the server, for *Save* of a file opened
/// with [`fetch`].
pub fn upload(conn: &RemoteConnection, local_path: &Path, remote_path: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(result) = fake::upload(local_path, remote_path) {
        return result;
    }
    let sftp = session(conn)?;
    let mut from = std::fs::File::open(local_path)
        .with_context(|| format!("{}: cannot read it", local_path.display()))?;
    let mut to = sftp
        .create(Path::new(remote_path))
        .with_context(|| format!("{remote_path}: cannot write it"))?;
    let mut buf = [0u8; CHUNK];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
    }
}

/// A connection's user and host, for showing what a tab is reading from or saving to, without
/// its password or key.
pub fn label(conn: &RemoteConnection, remote_path: &str) -> String {
    format!(
        "sftp://{}@{}:{}{remote_path}",
        conn.user, conn.host, conn.port
    )
}

/// A fake SFTP server tests fetch from and upload to, instead of a real network connection: a
/// set of remote paths, each either a file's bytes or a folder (listed by the files and folders
/// one path longer that start with it), installed on the thread running a test's GUI.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeMap;

    thread_local! {
        static SERVER: std::cell::RefCell<Option<Server>> = const { std::cell::RefCell::new(None) };
    }

    #[derive(Default)]
    pub struct Server {
        entries: BTreeMap<String, Option<Vec<u8>>>,
        fail: Option<String>,
        uploaded: Vec<(String, Vec<u8>)>,
    }

    impl Server {
        pub fn new() -> Self {
            Self::default()
        }

        /// A file at `path` (absolute, `/`-separated) with `contents`.
        pub fn file(mut self, path: &str, contents: &str) -> Self {
            self.entries
                .insert(path.to_string(), Some(contents.as_bytes().to_vec()));
            self
        }

        /// A folder at `path`; its files and subfolders are the other entries one path
        /// segment longer that start with `path/`.
        pub fn dir(mut self, path: &str) -> Self {
            self.entries.insert(path.to_string(), None);
            self
        }

        /// Every connection and transfer fails with `message`, instead of reading `entries`.
        pub fn failing(mut self, message: &str) -> Self {
            self.fail = Some(message.to_string());
            self
        }
    }

    impl RemoteFs for Server {
        fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>> {
            match self.entries.get(remote) {
                Some(Some(bytes)) => Ok(Box::new(std::io::Cursor::new(bytes.clone()))),
                Some(None) => bail!("{remote}: is a folder"),
                None => bail!("{remote}: no such file or folder"),
            }
        }

        fn list_dir(&self, remote: &str) -> Result<Vec<(String, bool, bool)>> {
            let prefix = format!("{}/", remote.trim_end_matches('/'));
            Ok(self
                .entries
                .iter()
                .filter_map(|(path, contents)| {
                    let rel = path.strip_prefix(&prefix)?;
                    (!rel.is_empty() && !rel.contains('/'))
                        .then(|| (rel.to_string(), contents.is_none(), contents.is_some()))
                })
                .collect())
        }
    }

    /// Stands in for the network for the rest of this thread's test: [`super::fetch`] and
    /// [`super::upload`] read and write `server` instead of making a real connection.
    pub fn install(server: Server) {
        SERVER.with(|s| *s.borrow_mut() = Some(server));
    }

    /// Removes the fake server, so a later test on the same thread starts without one.
    pub fn uninstall() {
        SERVER.with(|s| *s.borrow_mut() = None);
    }

    /// Whether a fake server is installed on this thread: the GUI test harness runs a fetch
    /// started this way on the calling thread instead of a spawned one, since the fake server
    /// is kept in a thread-local and a real fetch is backgrounded only because real network
    /// I/O, unlike the fake server, can be slow.
    pub fn installed() -> bool {
        SERVER.with(|s| s.borrow().is_some())
    }

    /// The bytes last uploaded to `path`, if any, for a test to check what *Save* wrote.
    pub fn uploaded(path: &str) -> Option<Vec<u8>> {
        SERVER.with(|s| {
            s.borrow()
                .as_ref()?
                .uploaded
                .iter()
                .rev()
                .find(|(p, _)| p == path)
                .map(|(_, b)| b.clone())
        })
    }

    /// How many times `path` was uploaded to, for a test to check *Save* did or did not run
    /// again when nothing of its tab changed.
    pub fn upload_count(path: &str) -> usize {
        SERVER.with(|s| {
            s.borrow()
                .as_ref()
                .map(|s| s.uploaded.iter().filter(|(p, _)| p == path).count())
                .unwrap_or(0)
        })
    }

    pub(super) fn fetch(
        remote_path: &str,
        cancel: &AtomicBool,
    ) -> Option<Result<(tempfile::TempDir, PathBuf, bool)>> {
        // Trimmed once, up front, and used everywhere below: `is_dir`'s lookup and the actual
        // read must agree on the same path, or a file fetched with a trailing slash would
        // pass the first and fail the second with a confusing "no such file or folder".
        let remote_path = remote_path.trim_end_matches('/');
        SERVER.with(|s| {
            let server = s.borrow();
            let server = server.as_ref()?;
            if let Some(message) = &server.fail {
                return Some(Err(anyhow::anyhow!("{message}")));
            }
            Some((|| {
                check(cancel)?;
                let temp = tempfile::Builder::new()
                    .prefix("oxidemerge-sftp-")
                    .tempdir()?;
                let local = temp.path().join(remote_file_name(remote_path));
                let is_dir = match server.entries.get(remote_path) {
                    Some(Some(_)) => false,
                    Some(None) => true,
                    None => bail!("{remote_path}: no such file or folder"),
                };
                let mut budget = Budget::new();
                if is_dir {
                    download_dir(server, remote_path, &local, cancel, &mut budget)?;
                } else {
                    download_file(server, remote_path, &local, cancel, &mut budget)?;
                }
                Ok((temp, local, is_dir))
            })())
        })
    }

    pub(super) fn upload(local_path: &Path, remote_path: &str) -> Option<Result<()>> {
        SERVER.with(|s| {
            let mut server = s.borrow_mut();
            let server = server.as_mut()?;
            if let Some(message) = &server.fail {
                return Some(Err(anyhow::anyhow!("{message}")));
            }
            Some((|| {
                let bytes = std::fs::read(local_path)?;
                server
                    .entries
                    .insert(remote_path.to_string(), Some(bytes.clone()));
                server.uploaded.push((remote_path.to_string(), bytes));
                Ok(())
            })())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manual smoke test of the real network path (not the fake server), against a local test
    // sshd: `/usr/sbin/sshd -f /tmp/sshtest/sshd_config -D &` with a matching config, key and
    // files. Ignored so it never runs as part of the suite or in CI, which has none of that.
    // The host key is trusted on the first connection (see `verify_host_key`); delete
    // `OXIDEMERGE_CONFIG_DIR`'s `known_hosts` file first to try that again.
    #[test]
    #[ignore]
    fn smoke_test_against_a_local_sshd() {
        let never = AtomicBool::new(false);
        let conn = RemoteConnection {
            host: "127.0.0.1".into(),
            port: 2222,
            user: std::env::var("USER").unwrap(),
            auth: Auth::Key {
                path: "/tmp/sshtest/userkey".into(),
                passphrase: String::new(),
            },
        };
        let (_t, local, is_dir) = fetch(&conn, "/tmp/sshtest/data_file.txt", &never).unwrap();
        assert!(!is_dir);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "hello remote\n");

        let (_t, local, is_dir) = fetch(&conn, "/tmp/sshtest/data", &never).unwrap();
        assert!(is_dir);
        assert_eq!(
            std::fs::read_to_string(local.join("sub/b.txt")).unwrap(),
            "nested\n"
        );

        let up_dir = tempfile::tempdir().unwrap();
        let up_local = up_dir.path().join("out.txt");
        std::fs::write(&up_local, "uploaded by smoke test\n").unwrap();
        upload(&conn, &up_local, "/tmp/sshtest/uploaded.txt").unwrap();
        assert_eq!(
            std::fs::read_to_string("/tmp/sshtest/uploaded.txt").unwrap(),
            "uploaded by smoke test\n"
        );
    }

    #[test]
    fn join_always_uses_forward_slashes() {
        assert_eq!(join("/srv/data", "a.txt"), "/srv/data/a.txt");
        assert_eq!(join("/srv/data/", "a.txt"), "/srv/data/a.txt");
    }

    #[test]
    fn remote_file_name_trims_a_trailing_slash_before_taking_the_last_segment() {
        assert_eq!(remote_file_name("/srv/data"), "data");
        assert_eq!(remote_file_name("/srv/data/"), "data");
        assert_eq!(remote_file_name("/srv/report.txt"), "report.txt");
        assert_eq!(remote_file_name("/"), "remote");
    }

    #[test]
    fn a_connection_label_names_the_server_but_not_the_password() {
        let conn = RemoteConnection {
            host: "example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: Auth::Password("s3cret".into()),
        };
        let shown = label(&conn, "/srv/report.txt");
        assert_eq!(shown, "sftp://ann@example.com:22/srv/report.txt");
        assert!(!shown.contains("s3cret"));
    }

    #[test]
    fn the_fake_server_fetches_a_file_and_a_folders_files_and_subfolders() {
        let never = AtomicBool::new(false);
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        fake::install(
            fake::Server::new()
                .file("/srv/report.txt", "hello")
                .dir("/srv/data")
                .file("/srv/data/a.txt", "a")
                .dir("/srv/data/sub")
                .file("/srv/data/sub/b.txt", "b"),
        );
        let (_t, path, is_dir) = fetch(&conn, "/srv/report.txt", &never).unwrap();
        assert!(!is_dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let (_t, path, is_dir) = fetch(&conn, "/srv/data", &never).unwrap();
        assert!(is_dir);
        assert_eq!(std::fs::read_to_string(path.join("a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_to_string(path.join("sub/b.txt")).unwrap(),
            "b"
        );
        // A trailing slash on the remote path still names the staged folder after it, not
        // the fallback name.
        let (_t, path, _) = fetch(&conn, "/srv/data/", &never).unwrap();
        assert_eq!(path.file_name().unwrap(), "data");
        // A trailing slash on a *file*'s path does not make it look missing: whatever
        // decided it is a file is the same lookup that then reads it.
        let (_t, path, is_dir) = fetch(&conn, "/srv/report.txt/", &never).unwrap();
        assert!(!is_dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        fake::uninstall();
    }

    #[test]
    fn the_fake_server_reports_a_missing_path_and_a_connection_failure() {
        let never = AtomicBool::new(false);
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        fake::install(fake::Server::new().file("/a.txt", "a"));
        assert!(fetch(&conn, "/missing.txt", &never).is_err());
        fake::install(fake::Server::new().failing("no route to host"));
        let err = fetch(&conn, "/a.txt", &never).unwrap_err();
        assert!(err.to_string().contains("no route to host"));
        fake::uninstall();
    }

    #[test]
    fn a_fetch_already_cancelled_stops_before_reading_anything() {
        let cancelled = AtomicBool::new(true);
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        fake::install(fake::Server::new().dir("/srv").file("/srv/a.txt", "a"));
        let err = fetch(&conn, "/srv", &cancelled).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        fake::uninstall();
    }

    #[test]
    fn uploading_replaces_the_fake_servers_file_and_is_counted() {
        fake::install(fake::Server::new().file("/a.txt", "old"));
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        std::fs::write(&local, "new").unwrap();
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        upload(&conn, &local, "/a.txt").unwrap();
        assert_eq!(fake::uploaded("/a.txt").unwrap(), b"new");
        assert_eq!(fake::upload_count("/a.txt"), 1);
        upload(&conn, &local, "/a.txt").unwrap();
        assert_eq!(fake::upload_count("/a.txt"), 2);
        fake::uninstall();
    }

    /// A `Read` that cancels after its first chunk, so a cancellation mid-transfer (not
    /// just a fetch already cancelled before it starts) is exercised too, without a real
    /// connection to pace it.
    struct CancelAfterFirstRead<'a> {
        inner: std::io::Cursor<Vec<u8>>,
        cancel: &'a AtomicBool,
        done_once: bool,
    }

    impl Read for CancelAfterFirstRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                !self.done_once,
                "read again after cancellation: `check` did not stop the loop"
            );
            let n = self.inner.read(buf)?;
            self.done_once = true;
            // Set only once the first chunk's data is already in hand, so the loop's next
            // `check` (before it would read a second chunk) is what stops it, not this call.
            self.cancel.store(true, Ordering::Relaxed);
            Ok(n)
        }
    }

    struct CancellingFile<'a> {
        bytes: Vec<u8>,
        cancel: &'a AtomicBool,
    }

    impl RemoteFs for CancellingFile<'_> {
        fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(CancelAfterFirstRead {
                inner: std::io::Cursor::new(self.bytes.clone()),
                cancel: self.cancel,
                done_once: false,
            }))
        }

        fn list_dir(&self, _remote: &str) -> Result<Vec<(String, bool, bool)>> {
            unreachable!("not a folder")
        }
    }

    #[test]
    fn cancelling_mid_transfer_stops_within_one_chunk_with_what_was_read_so_far_on_disk() {
        let cancel = AtomicBool::new(false);
        // Three chunks, so the first `read` (one chunk) succeeds before `cancel` is set, and
        // the loop's next `check` stops it before a second chunk is read.
        let fs = CancellingFile {
            bytes: vec![7u8; CHUNK * 3],
            cancel: &cancel,
        };
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("big.bin");
        let mut budget = Budget::new();
        let err = download_file(&fs, "/big.bin", &local, &cancel, &mut budget).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert_eq!(std::fs::metadata(&local).unwrap().len(), CHUNK as u64);
    }

    /// A file of `bytes`, with no cancellation of its own: for the byte-budget test, so it is
    /// not entangled with the cancellation flag [`CancellingFile`] sets.
    struct OneBigFile {
        bytes: Vec<u8>,
    }

    impl RemoteFs for OneBigFile {
        fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(std::io::Cursor::new(self.bytes.clone())))
        }

        fn list_dir(&self, _remote: &str) -> Result<Vec<(String, bool, bool)>> {
            unreachable!("not a folder")
        }
    }

    struct ManyFiles(usize);

    impl RemoteFs for ManyFiles {
        fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(std::io::Cursor::new(b"x".to_vec())))
        }

        fn list_dir(&self, remote: &str) -> Result<Vec<(String, bool, bool)>> {
            if remote != "/many" {
                return Ok(Vec::new());
            }
            Ok((0..self.0)
                .map(|i| (format!("f{i}"), false, true))
                .collect())
        }
    }

    #[test]
    fn a_folder_with_too_many_files_stops_instead_of_reading_them_all() {
        let never = AtomicBool::new(false);
        // A small limit, so the test does not actually have to write tens of thousands of
        // files to disk to reach it.
        let fs = ManyFiles(10);
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("many");
        let mut budget = Budget::with_limits(u64::MAX, 3);
        let err = download_dir(&fs, "/many", &local, &never, &mut budget).unwrap_err();
        assert!(err.to_string().contains("files"));
        // The files up to the limit were still written; the budget stops the next one, not
        // the whole folder retroactively.
        assert_eq!(std::fs::read_dir(&local).unwrap().count(), 3);
    }

    #[test]
    fn a_file_larger_than_the_byte_budget_stops_partway() {
        let never = AtomicBool::new(false);
        let fs = OneBigFile {
            bytes: vec![0u8; CHUNK * 2],
        };
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("big.bin");
        // Exactly one chunk's worth: the first chunk fits and is written, the second would
        // not fit and is rejected before it is, so the file is left with just the first.
        let mut budget = Budget::with_limits(CHUNK as u64, usize::MAX);
        let err = download_file(&fs, "/big.bin", &local, &never, &mut budget).unwrap_err();
        assert!(err.to_string().contains("GB"), "{err}");
        assert_eq!(std::fs::metadata(&local).unwrap().len(), CHUNK as u64);
    }

    #[test]
    fn hex_round_trips_every_byte_value() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(decode_hex(&encode_hex(&bytes)).unwrap(), bytes);
        assert_eq!(decode_hex("not hex!"), None);
        assert_eq!(decode_hex("abc"), None); // odd length
    }

    #[test]
    fn known_hosts_are_loaded_and_saved_as_one_line_per_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let mut hosts = HashMap::new();
        hosts.insert("example.com:22".to_string(), vec![1, 2, 3]);
        save_known_hosts(&path, &hosts).unwrap();
        let loaded = load_known_hosts(&path);
        assert_eq!(loaded, hosts);
    }

    #[test]
    fn load_known_hosts_ignores_a_missing_file_blank_lines_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        assert!(load_known_hosts(&path).is_empty());
        std::fs::write(&path, "# a comment\n\nexample.com:22 0102ff\n").unwrap();
        let loaded = load_known_hosts(&path);
        assert_eq!(loaded.get("example.com:22"), Some(&vec![1, 2, 0xff]));
    }

    #[test]
    fn a_new_servers_key_is_trusted_and_remembered_a_changed_one_is_rejected() {
        let known = HashMap::new();
        assert_eq!(check_host_key(&known, "h:22", b"key-a"), HostKeyCheck::New);
        let mut known = HashMap::new();
        known.insert("h:22".to_string(), b"key-a".to_vec());
        assert_eq!(
            check_host_key(&known, "h:22", b"key-a"),
            HostKeyCheck::Trusted
        );
        assert_eq!(
            check_host_key(&known, "h:22", b"key-b"),
            HostKeyCheck::Changed
        );
    }
}
