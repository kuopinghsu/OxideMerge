//! Files and folders on another machine, read and written over SFTP: an SSH connection this
//! program makes itself, authenticated by a password or a private key, to a path the server
//! sees under the signed-in user's own account.
//!
//! A file or folder opened this way is staged in a temporary local copy first, the same as a
//! Git revision or an extracted archive is. Unlike those, a single file stays writable: saving
//! it uploads the local copy back over a fresh connection made with the same credentials kept
//! with its tab, since a connection is not worth holding open for as long as a tab might stay
//! open (through sleep, a changed network, or a server that closes idle sessions). The upload
//! goes to a temporary file beside the original, which then takes its place, and is refused
//! when the file changed on the server since it was fetched (see [`upload`]). A folder is
//! read-only, as a Git revision or a web page is: there is no equivalent here yet of the
//! archive repacking a folder compare's own files use to save back what changed.
//!
//! The server's host key is checked before a password or a key ever reaches it: against the
//! user's own `~/.ssh/known_hosts`, then against the servers this program was told to trust
//! (kept in [`known_hosts_path`]). A server in neither is not trusted silently: the fetch
//! fails with [`UnknownHost`], carrying the key's fingerprint for the user to check and
//! accept with [`trust_host`]. See [`verify_host_key`].
//!
//! A fetch is bounded: see [`Budget`]. The one real network path, [`RemoteFs`] for
//! `ssh2::Sftp`, is a thin seam so the rest — walking a folder, the budget, cancelling midway —
//! is tested without a real connection, the same way [`fake`] stands in for one entirely in
//! GUI tests.

use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How a connection is authenticated. The password or passphrase is overwritten in memory
/// once it is dropped.
#[derive(Clone)]
pub enum Auth {
    Password(String),
    /// A private key file and its passphrase (empty for a key that needs none).
    Key {
        path: PathBuf,
        passphrase: String,
    },
}

impl Drop for Auth {
    fn drop(&mut self) {
        match self {
            Auth::Password(p) | Auth::Key { passphrase: p, .. } => wipe(p),
        }
    }
}

/// Overwrites `s` with zeros before emptying it, so a password does not linger in freed
/// memory.
pub fn wipe(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8, and the string is emptied right after.
    for b in unsafe { s.as_mut_vec() }.iter_mut() {
        // SAFETY: `b` is a valid, aligned reference into the string's own buffer.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    s.clear();
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

/// A remote file's size and modified time, to notice it changed on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteStamp {
    pub size: u64,
    pub modified: u64,
}

impl RemoteStamp {
    fn of(stat: &ssh2::FileStat) -> Self {
        RemoteStamp {
            size: stat.size.unwrap_or(0),
            modified: stat.mtime.unwrap_or(0),
        }
    }
}

/// What [`fetch`] staged: the temporary folder holding it (deleted when dropped), the local
/// copy in it, whether that is a folder, and for a file, how it was on the server.
pub struct Fetched {
    pub temp: tempfile::TempDir,
    pub local: PathBuf,
    pub is_dir: bool,
    pub stamp: Option<RemoteStamp>,
}

/// How long connecting, looking up the address, and authenticating may each take before the
/// server is treated as unreachable, and how long a single read or write of the connection may
/// then take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// `host:port` as shown to the user, with an IPv6 address in brackets.
fn server(host: &str, port: u16) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The name a server's host key is remembered under: [`server`], with the host in lowercase.
fn host_id(host: &str, port: u16) -> String {
    server(&host.to_ascii_lowercase(), port)
}

/// `host`'s addresses, looked up with its own bound on how long that may take: `ToSocketAddrs`
/// gives the operating system's resolver no timeout of its own, so without this a lookup that
/// hangs (an unreachable DNS server, say) would hang the fetch past `CONNECT_TIMEOUT` and past
/// *Cancel*. The lookup itself still runs to completion on its own thread even after timing
/// out here; that thread is harmless once nothing reads its result.
fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let (tx, rx) = mpsc::channel();
    let owned = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    std::thread::spawn(move || {
        let _ = tx.send(
            (owned.as_str(), port)
                .to_socket_addrs()
                .map(|a| a.collect::<Vec<_>>()),
        );
    });
    let lookup = || trf("msg.sftp_lookup", &[("host", host.to_string())]);
    match rx.recv_timeout(CONNECT_TIMEOUT) {
        Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
        Ok(Ok(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => bail!(lookup()),
        Ok(Err(e)) => Err(e).context(lookup()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!(trf("msg.sftp_lookup_slow", &[("host", host.to_string())]))
        }
    }
}

/// The file this program remembers the host keys of servers it was told to trust in: one line
/// per server, `host:port` and the key in hexadecimal, separated by one space. Lines it does not
/// understand, blank ones, and ones starting with `#` are kept as they are, so entries can be
/// removed by hand (see [`verify_host_key`]) without disturbing the rest of the file. Kept with
/// the settings file, next to the program in portable use.
pub fn known_hosts_path() -> Option<PathBuf> {
    let settings = crate::config::config_path()?;
    let name = if settings.file_name().is_some_and(|n| n == "oxidemerge.json") {
        "oxidemerge-known_hosts"
    } else {
        "known_hosts"
    };
    Some(settings.with_file_name(name))
}

/// The user's own `ssh` known hosts file, checked first.
fn user_known_hosts() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
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

/// `bytes` in unpadded base64, the way `ssh` shows a key fingerprint.
fn encode_base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The type a host key names itself with (`ssh-ed25519`, `ecdsa-sha2-nistp256`, ...), the
/// first field of its wire form.
fn key_type(key: &[u8]) -> Option<&str> {
    let len = u32::from_be_bytes(key.get(..4)?.try_into().ok()?) as usize;
    std::str::from_utf8(key.get(4..4 + len)?).ok()
}

/// The remembered host keys, by [`host_id`]. A missing file is none; one that cannot be read is
/// an error, so that a server whose key changed is never mistaken for one never seen.
fn load_known_hosts(path: &Path) -> Result<HashMap<String, Vec<u8>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => {
            return Err(e).context(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ))
        }
    };
    let mut known = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A line that can't be read could be the one for the server being checked.
        let Some((id, key)) = line
            .split_once(' ')
            .and_then(|(id, hex)| Some((id, decode_hex(hex.trim())?)))
        else {
            bail!(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ));
        };
        known.insert(id.to_string(), key);
    }
    Ok(known)
}

/// The key types libssh2 reads in a known hosts file. A line of another type (a security key,
/// a certificate) can't be checked here, and no server key this program accepts has its type.
const KEY_TYPES: &[&str] = &[
    "ssh-rsa",
    "ssh-dss",
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

/// What the user's own `~/.ssh/known_hosts` says of a server's key.
#[derive(Debug, PartialEq, Eq)]
enum UserHosts {
    /// It records this very key for the server.
    Match,
    /// It records another key of the same type for the server, or revokes this one.
    Changed,
    /// It records only keys of other types for the server.
    OtherType,
    /// It says nothing of the server.
    Unknown,
}

/// The name `ssh` records a server under in a known hosts file: the host alone on port 22,
/// `[host]:port` on any other.
fn ssh_name(host: &str, port: u16) -> String {
    let bare = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if port == 22 {
        bare
    } else {
        format!("[{bare}]:{port}")
    }
}

/// Checks `key`, the host key of `host` at `port`, against the known hosts file at `path`, as
/// `ssh` would: by the host's name alone on port 22, as `[host]:port` on any other. A missing
/// file knows no server; one that can't be read, or holding a line of a key type it checks
/// that can't be read, is an error, since that line could be the one for this server.
fn check_user_hosts(path: &Path, host: &str, port: u16, key: &[u8]) -> Result<UserHosts> {
    let unreadable = || {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", path.display().to_string())],
        )
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UserHosts::Unknown),
        Err(e) => return Err(e).context(unreadable()),
    };
    let sess = ssh2::Session::new().context(unreadable())?;
    let lists = || sess.known_hosts().context(unreadable());
    let (mut same, mut other, mut revoked) = (lists()?, lists()?, lists()?);
    let kind = key_type(key);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (into, entry) = match line.split_once(char::is_whitespace) {
            Some(("@revoked", rest)) => (&mut revoked, rest.trim_start()),
            // Certificate authorities: host certificates are not used here.
            Some((marker, _)) if marker.starts_with('@') => continue,
            _ => {
                let line_type = line.split_whitespace().nth(1);
                if line_type.is_some() && line_type == kind {
                    (&mut same, line)
                } else {
                    (&mut other, line)
                }
            }
        };
        // Keys of types this program can't be sent are no concern of its.
        let line_type = entry.split_whitespace().nth(1).unwrap_or("");
        if !KEY_TYPES.contains(&line_type) {
            continue;
        }
        if into
            .read_str(entry, ssh2::KnownHostFileKind::OpenSSH)
            .is_err()
        {
            bail!(unreadable());
        }
    }
    let name = ssh_name(host, port);
    let check = |list: &ssh2::KnownHosts| match list.check(&name, key) {
        ssh2::CheckResult::Failure => Err(anyhow::anyhow!(unreadable())),
        found => Ok(found),
    };
    if matches!(check(&revoked)?, ssh2::CheckResult::Match) {
        return Ok(UserHosts::Changed);
    }
    match check(&same)? {
        ssh2::CheckResult::Match => return Ok(UserHosts::Match),
        ssh2::CheckResult::Mismatch => return Ok(UserHosts::Changed),
        _ => {}
    }
    Ok(match check(&other)? {
        ssh2::CheckResult::NotFound => UserHosts::Unknown,
        _ => UserHosts::OtherType,
    })
}

/// Adds `id`'s `key` to the known hosts file at `path`, replacing a line for the same server
/// and keeping every other line as it is now on disk (another window may have added one since
/// this one read it). Written to a temporary file that then takes the file's place.
fn remember_host(path: &Path, id: &str, key: &[u8]) -> Result<()> {
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).context(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ))
        }
    };
    let mut text: String = old
        .lines()
        .filter(|line| line.trim().split_once(' ').map(|(i, _)| i) != Some(id))
        .flat_map(|line| [line, "\n"])
        .collect();
    text.push_str(&format!("{id} {}\n", encode_hex(key)));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    super::fsutil::write_atomic_with(path, text.as_bytes(), false)
}

/// What [`verify_host_key`] decided about a server's host key.
#[derive(Debug, PartialEq, Eq)]
enum HostKeyCheck {
    /// Matches the key this program was told to trust for this server.
    Trusted,
    /// A server this program was never told to trust.
    New,
    /// Does not match the key this program was told to trust: a changed host key, a different
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

/// A server neither the user's `~/.ssh/known_hosts` nor this program's own list knows: the
/// error a fetch fails with, for the user to compare `fingerprint` with the server's real one
/// and, if it matches, [`trust_host`] it and fetch again.
#[derive(Debug, Clone)]
pub struct UnknownHost {
    /// `host:port`, as shown.
    pub server: String,
    id: String,
    /// The key's type, such as `ssh-ed25519`.
    pub key_type: String,
    /// `SHA256:` and the key's hash in base64, as `ssh` shows it.
    pub fingerprint: String,
    key: Vec<u8>,
    path: PathBuf,
    /// The user's own known hosts file, when it records a different key for this server.
    pub differs_from: Option<PathBuf>,
}

impl std::fmt::Display for UnknownHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}",
            trf(
                "msg.sftp_unknown_host",
                &[
                    ("server", self.server.clone()),
                    ("type", self.key_type.clone())
                ]
            ),
            self.fingerprint
        )
    }
}

impl std::error::Error for UnknownHost {}

/// Trusts `host`'s key from now on, remembering it in [`known_hosts_path`].
pub fn trust_host(host: &UnknownHost) -> Result<()> {
    #[cfg(test)]
    if fake::trust(host) {
        return Ok(());
    }
    remember_host(&host.path, &host.id, &host.key)
}

/// Checks `sess`'s host key: against the user's own `~/.ssh/known_hosts` (a match is trusted,
/// another key of the same type refuses the connection), then against the keys this program
/// was told to trust, `known` (see [`known_hosts_path`]). Called right after the handshake and
/// before a password or a key ever reaches the connection, so a server this fails for never
/// sees them. A server neither knows fails with [`UnknownHost`].
///
/// There is no prompt to trust a changed key anyway: that choice is easy to click past without
/// reading it, which defeats the point of checking at all. A key that genuinely changed (a
/// reinstalled server, a rotated key) is trusted again only by removing its line from the
/// known hosts file by hand.
fn verify_host_key(
    sess: &ssh2::Session,
    conn: &RemoteConnection,
    known: &HashMap<String, Vec<u8>>,
    path: &Path,
) -> Result<()> {
    let (key, _) = sess
        .host_key()
        .with_context(|| tr("msg.sftp_no_host_key"))?;
    let mut differs_from = None;
    if let Some(user_file) = user_known_hosts() {
        match check_user_hosts(&user_file, &conn.host, conn.port, key)? {
            UserHosts::Match => return Ok(()),
            UserHosts::Changed => bail!(trf(
                "msg.sftp_key_changed_ssh",
                &[
                    ("server", server(&conn.host, conn.port)),
                    ("host", ssh_name(&conn.host, conn.port)),
                    ("file", user_file.display().to_string()),
                ],
            )),
            // The server sent another type of key than the file records for it: this
            // program's own list, or the user, decides.
            UserHosts::OtherType => differs_from = Some(user_file),
            UserHosts::Unknown => {}
        }
    }
    let id = host_id(&conn.host, conn.port);
    match check_host_key(known, &id, key) {
        HostKeyCheck::Trusted => Ok(()),
        HostKeyCheck::Changed => bail!(trf(
            "msg.sftp_key_changed",
            &[
                ("server", server(&conn.host, conn.port)),
                ("id", id),
                ("file", path.display().to_string()),
            ],
        )),
        HostKeyCheck::New => {
            let hash = sess
                .host_key_hash(ssh2::HashType::Sha256)
                .unwrap_or_default();
            Err(UnknownHost {
                server: server(&conn.host, conn.port),
                id,
                key_type: key_type(key).unwrap_or("?").to_string(),
                fingerprint: format!("SHA256:{}", encode_base64(hash)),
                key: key.to_vec(),
                path: path.to_path_buf(),
                differs_from,
            }
            .into())
        }
    }
}

/// The host key algorithms to ask a server for when this program already trusts its key of
/// type `kind`, so that it presents that one rather than another the program never saw.
fn host_key_methods(kind: &str) -> &str {
    match kind {
        "ssh-rsa" => "rsa-sha2-512,rsa-sha2-256,ssh-rsa",
        other => other,
    }
}

/// Opens an SFTP session authenticated as `conn` says. Setting `cancel` stops it between steps.
fn session(conn: &RemoteConnection, cancel: &AtomicBool) -> Result<ssh2::Sftp> {
    let shown = server(&conn.host, conn.port);
    let path = known_hosts_path().with_context(|| {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", "known_hosts".to_string())],
        )
    })?;
    let known = load_known_hosts(&path)?;
    let addrs = resolve(&conn.host, conn.port)?;
    check(cancel)?;
    let mut tcp = Err(std::io::Error::other("no address"));
    for addr in &addrs {
        tcp = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT);
        if tcp.is_ok() {
            break;
        }
        check(cancel)?;
    }
    let tcp = tcp.with_context(|| trf("msg.sftp_unreachable", &[("server", shown.clone())]))?;
    check(cancel)?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    let mut sess = ssh2::Session::new().context("SSH")?;
    sess.set_timeout(CONNECT_TIMEOUT.as_millis() as u32);
    sess.set_tcp_stream(tcp);
    if let Some(kind) = known
        .get(&host_id(&conn.host, conn.port))
        .and_then(|k| key_type(k))
    {
        let _ = sess.method_pref(ssh2::MethodType::HostKey, host_key_methods(kind));
    }
    sess.handshake()
        .with_context(|| trf("msg.sftp_handshake", &[("server", shown.clone())]))?;
    verify_host_key(&sess, conn, &known, &path)?;
    check(cancel)?;
    match &conn.auth {
        Auth::Password(password) => sess
            .userauth_password(&conn.user, password)
            .with_context(|| trf("msg.sftp_password", &[("server", shown.clone())]))?,
        Auth::Key { path, passphrase } => {
            let passphrase = (!passphrase.is_empty()).then_some(passphrase.as_str());
            sess.userauth_pubkey_file(&conn.user, None, path, passphrase)
                .with_context(|| trf("msg.sftp_key", &[("server", shown.clone())]))?
        }
    }
    if !sess.authenticated() {
        bail!(trf("msg.sftp_password", &[("server", shown)]));
    }
    check(cancel)?;
    sess.sftp()
        .with_context(|| trf("msg.sftp_no_sftp", &[("server", shown)]))
}

/// Cooperative cancellation, checked between the steps of connecting and the chunks of a
/// transfer: a network read or write itself cannot be interrupted, but a large file then stops
/// within one chunk of it.
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!(tr("msg.sftp_cancelled"));
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
    entries: usize,
    max_bytes: u64,
    max_entries: usize,
}

/// The most bytes a single fetch (one file, or a whole folder together) reads from the server.
const MAX_REMOTE_BYTES: u64 = 4 << 30;
/// The most files and folders a single folder fetch reads from the server.
const MAX_REMOTE_ENTRIES: usize = 50_000;
/// The most folders deep a folder fetch goes.
const MAX_REMOTE_DEPTH: usize = 64;

impl Budget {
    fn new() -> Self {
        Self {
            bytes: 0,
            entries: 0,
            max_bytes: MAX_REMOTE_BYTES,
            max_entries: MAX_REMOTE_ENTRIES,
        }
    }

    #[cfg(test)]
    fn with_limits(max_bytes: u64, max_entries: usize) -> Self {
        Self {
            bytes: 0,
            entries: 0,
            max_bytes,
            max_entries,
        }
    }

    /// How many more files and folders may be read.
    fn entries_left(&self) -> usize {
        self.max_entries.saturating_sub(self.entries)
    }

    fn too_many(&self) -> anyhow::Error {
        anyhow::anyhow!(trf(
            "msg.sftp_too_many",
            &[("count", self.max_entries.to_string())]
        ))
    }

    fn take_entry(&mut self) -> Result<()> {
        self.entries += 1;
        if self.entries > self.max_entries {
            return Err(self.too_many());
        }
        Ok(())
    }

    fn too_large(&self) -> anyhow::Error {
        anyhow::anyhow!(trf(
            "msg.sftp_too_large",
            &[("size", format!("{} GiB", self.max_bytes >> 30))]
        ))
    }

    fn take_bytes(&mut self, n: u64) -> Result<()> {
        self.bytes += n;
        if self.bytes > self.max_bytes {
            return Err(self.too_large());
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
    /// `(name, is_dir, is_file)` for each entry of `remote`, a folder: no more than `limit + 1`
    /// of them, so a caller sees there were too many without reading them all. Fails once
    /// `cancel` is set.
    fn list_dir(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, bool, bool)>>;
}

impl RemoteFs for ssh2::Sftp {
    fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.open(Path::new(remote)).with_context(
            || trf("msg.sftp_cannot_read", &[("path", remote.to_string())]),
        )?))
    }

    /// Entries without a name valid in UTF-8 are left out too, along with symbolic links and
    /// other special entries ([`RemoteFs::list_dir`]): this program's paths are UTF-8
    /// throughout, the same as a local folder compare already assumes of a file's name.
    fn list_dir(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, bool, bool)>> {
        let cannot = || trf("msg.sftp_cannot_list", &[("path", remote.to_string())]);
        let mut dir = self.opendir(Path::new(remote)).with_context(cannot)?;
        let mut out = Vec::new();
        loop {
            check(cancel)?;
            let (name, stat) = match dir.readdir() {
                Ok(entry) => entry,
                // libssh2's "no more files"
                Err(e) if matches!(e.code(), ssh2::ErrorCode::Session(-16)) => break,
                Err(e) => return Err(e).with_context(cannot),
            };
            let Some(name) = name.to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            out.push((name.to_string(), stat.is_dir(), stat.is_file()));
            if out.len() > limit {
                break;
            }
        }
        Ok(out)
    }
}

/// Whether `name`, a remote folder entry, can be a file name here: not a path of its own, and on
/// Windows, none of the names or characters Windows refuses.
fn local_name_ok(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return false;
    }
    if cfg!(windows) {
        const RESERVED: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
            || name.chars().any(|c| (c as u32) < 32)
            || name.ends_with(['.', ' '])
            || RESERVED.contains(&stem.trim_end())
        {
            return false;
        }
    }
    true
}

fn download_file(
    fs: &dyn RemoteFs,
    remote: &str,
    local: &Path,
    cancel: &AtomicBool,
    budget: &mut Budget,
) -> Result<()> {
    budget.take_entry()?;
    let mut from = fs.open_read(remote)?;
    let mut to = std::fs::File::create(local)
        .with_context(|| trf("msg.cannot_write", &[("file", local.display().to_string())]))?;
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

/// `remote`'s files and subfolders, downloaded into `local`; symbolic links, other special
/// entries, and names this machine cannot have are left out, the same as a folder compare
/// leaves out what it does not understand on either side. `depth` is how many folders deep
/// `remote` is in the fetch.
fn download_dir(
    fs: &dyn RemoteFs,
    remote: &str,
    local: &Path,
    depth: usize,
    cancel: &AtomicBool,
    budget: &mut Budget,
) -> Result<()> {
    check(cancel)?;
    if depth > MAX_REMOTE_DEPTH {
        bail!(trf(
            "msg.sftp_too_deep",
            &[
                ("path", remote.to_string()),
                ("count", MAX_REMOTE_DEPTH.to_string())
            ]
        ));
    }
    budget.take_entry()?;
    std::fs::create_dir_all(local)?;
    let entries = fs.list_dir(remote, budget.entries_left(), cancel)?;
    if entries.len() > budget.entries_left() {
        return Err(budget.too_many());
    }
    for (name, is_dir, is_file) in entries {
        if !local_name_ok(&name) {
            continue;
        }
        let child_remote = join(remote, &name);
        let child_local = local.join(&name);
        if is_dir {
            download_dir(fs, &child_remote, &child_local, depth + 1, cancel, budget)?;
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
    let name = remote_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if local_name_ok(name) {
        name.into()
    } else {
        "remote".into()
    }
}

/// Fetches the file or folder `remote_path` into a new temporary folder (deleted when it is
/// dropped). Setting `cancel` stops it, and it fails. Bounded by [`MAX_REMOTE_BYTES`],
/// [`MAX_REMOTE_ENTRIES`] and [`MAX_REMOTE_DEPTH`]. Fails with [`UnknownHost`] for a server
/// not trusted yet.
pub fn fetch(conn: &RemoteConnection, remote_path: &str, cancel: &AtomicBool) -> Result<Fetched> {
    #[cfg(test)]
    if let Some(result) = fake::fetch(remote_path, cancel) {
        return result;
    }
    let sftp = session(conn, cancel)?;
    let stat = sftp
        .stat(Path::new(remote_path))
        .with_context(|| trf("msg.sftp_cannot_read", &[("path", remote_path.to_string())]))?;
    let is_dir = stat.is_dir();
    if !is_dir && !stat.is_file() {
        bail!(trf(
            "msg.sftp_not_regular",
            &[("path", remote_path.to_string())]
        ));
    }
    let mut budget = Budget::new();
    if stat.size.is_some_and(|s| s > budget.max_bytes) {
        return Err(budget.too_large());
    }
    let temp = tempfile::Builder::new()
        .prefix("oxidemerge-sftp-")
        .tempdir()?;
    let local = temp.path().join(remote_file_name(remote_path));
    if is_dir {
        download_dir(&sftp, remote_path, &local, 0, cancel, &mut budget)?;
    } else {
        download_file(&sftp, remote_path, &local, cancel, &mut budget)?;
    }
    Ok(Fetched {
        temp,
        local,
        is_dir,
        stamp: (!is_dir).then(|| RemoteStamp::of(&stat)),
    })
}

/// Writes `local_path`'s bytes to `remote_path` on the server, for *Save* of a file opened
/// with [`fetch`], returning how the file is on the server now. A link on the server is kept,
/// and the file it names is written instead.
///
/// Refused when the file is not as `expected` (how it was fetched or last uploaded) any more:
/// someone else changed or deleted it on the server meanwhile, and writing over it would lose
/// their change. The bytes go to a temporary file beside it first, given the file's
/// permissions, which then takes its place, so a failed or cancelled upload leaves the
/// server's file as it was. A server that cannot rename over a file (OpenSSH cannot) has the
/// old file moved aside for that moment, and moved back if the upload cannot finish.
pub fn upload(
    conn: &RemoteConnection,
    local_path: &Path,
    remote_path: &str,
    expected: Option<RemoteStamp>,
    cancel: &AtomicBool,
) -> Result<RemoteStamp> {
    #[cfg(test)]
    if let Some(result) = fake::upload(local_path, remote_path, expected) {
        return result;
    }
    let cannot_write = || {
        trf(
            "msg.sftp_cannot_write",
            &[("path", remote_path.to_string())],
        )
    };
    let changed = || {
        anyhow::anyhow!(trf(
            "msg.sftp_changed_on_server",
            &[("path", remote_path.to_string())]
        ))
    };
    let sftp = session(conn, cancel)?;
    let real = match sftp.lstat(Path::new(remote_path)) {
        Ok(s) if s.file_type().is_symlink() => sftp
            .realpath(Path::new(remote_path))
            .with_context(cannot_write)?
            .to_string_lossy()
            .into_owned(),
        _ => remote_path.to_string(),
    };
    let target = Path::new(&real);
    let stat_now = || match sftp.stat(target) {
        Ok(s) => Ok(Some(s)),
        Err(e) => {
            let e = std::io::Error::from(e);
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(anyhow::Error::from(e).context(cannot_write()))
            }
        }
    };
    let now = stat_now()?;
    let stamp_now = now.as_ref().map(RemoteStamp::of);
    if expected.is_some() && stamp_now != expected {
        return Err(changed());
    }
    let mut from = std::fs::File::open(local_path).with_context(|| {
        trf(
            "msg.sftp_cannot_read",
            &[("path", local_path.display().to_string())],
        )
    })?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = remote_file_name(&real).to_string_lossy().into_owned();
    // Short enough that the temporary name, and the one the old file is moved aside to, still
    // fit the 255 bytes most file systems allow a name.
    let mut end = name.len().min(100);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let dir = real.trim_end_matches('/');
    let dir = &dir[..dir.rfind('/').map_or(0, |i| i + 1)];
    let tmp_path = format!(
        "{dir}.{}.oxidemerge-{}-{unique}",
        &name[..end],
        std::process::id()
    );
    let tmp = Path::new(&tmp_path);
    let perm = now
        .as_ref()
        .and_then(|s| s.perm)
        .map_or(0o644, |p| p & 0o7777);
    let mut to = sftp
        .open_mode(
            tmp,
            ssh2::OpenFlags::WRITE | ssh2::OpenFlags::EXCLUSIVE,
            perm as i32,
            ssh2::OpenType::File,
        )
        .with_context(cannot_write)?;
    let written = (|| -> Result<()> {
        let mut buf = [0u8; CHUNK];
        loop {
            check(cancel)?;
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n])?;
        }
        let _ = to.fsync();
        to.close()?;
        let _ = sftp.setstat(
            tmp,
            ssh2::FileStat {
                size: None,
                uid: None,
                gid: None,
                perm: Some(perm),
                atime: None,
                mtime: None,
            },
        );
        check(cancel)?;
        // Someone may have changed the file while this one was being sent.
        if stat_now()?.as_ref().map(RemoteStamp::of) != stamp_now {
            return Err(changed());
        }
        replace(&sftp, tmp, target, now.is_some())
    })();
    if let Err(e) = written {
        let _ = sftp.unlink(tmp);
        return Err(e).with_context(cannot_write);
    }
    let stat = sftp.stat(target).with_context(cannot_write)?;
    Ok(RemoteStamp::of(&stat))
}

/// Renames `tmp` over `target` on the server. A server speaking SFTP version 3 (OpenSSH, and
/// most others) refuses to rename over an existing file, so then the old file is first moved
/// aside, and moved back if the second rename fails too; failing that, the error names where
/// the old file is now.
fn replace(sftp: &ssh2::Sftp, tmp: &Path, target: &Path, exists: bool) -> Result<()> {
    let flags =
        ssh2::RenameFlags::OVERWRITE | ssh2::RenameFlags::ATOMIC | ssh2::RenameFlags::NATIVE;
    let first = sftp.rename(tmp, target, Some(flags));
    if first.is_ok() || !exists {
        return Ok(first?);
    }
    let mut aside = tmp.as_os_str().to_owned();
    aside.push(".old");
    let aside = PathBuf::from(aside);
    sftp.rename(target, &aside, None)?;
    match sftp.rename(tmp, target, None) {
        Ok(()) => {
            let _ = sftp.unlink(&aside);
            Ok(())
        }
        Err(e) => {
            if sftp.rename(&aside, target, None).is_err() {
                return Err(e).context(trf(
                    "msg.sftp_original_moved",
                    &[("path", aside.display().to_string())],
                ));
            }
            Err(e.into())
        }
    }
}

/// A connection's user and host, for showing what a tab is reading from or saving to, without
/// its password or key. A path not starting with `/` is relative to the user's home folder,
/// shown as `/~/`.
pub fn label(conn: &RemoteConnection, remote_path: &str) -> String {
    let path = if remote_path.starts_with('/') {
        remote_path.to_string()
    } else {
        format!("/~/{remote_path}")
    };
    format!(
        "sftp://{}@{}{path}",
        conn.user,
        server(&conn.host, conn.port)
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
        /// How many times each file was written, standing in for its modified time.
        versions: BTreeMap<String, u64>,
        fail: Option<String>,
        uploaded: Vec<(String, Vec<u8>)>,
        /// Whether the server's key is not trusted yet (see [`Server::unknown_host`]).
        unknown: bool,
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

        /// A server whose key is not trusted yet: fetching fails with [`UnknownHost`] until
        /// [`trust_host`] is called with it.
        pub fn unknown_host(mut self) -> Self {
            self.unknown = true;
            self
        }

        fn stamp(&self, path: &str) -> Option<RemoteStamp> {
            let bytes = self.entries.get(path)?.as_ref()?;
            Some(RemoteStamp {
                size: bytes.len() as u64,
                modified: self.versions.get(path).copied().unwrap_or(0),
            })
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

        fn list_dir(
            &self,
            remote: &str,
            limit: usize,
            cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
            check(cancel)?;
            let prefix = format!("{}/", remote.trim_end_matches('/'));
            Ok(self
                .entries
                .iter()
                .filter_map(|(path, contents)| {
                    let rel = path.strip_prefix(&prefix)?;
                    (!rel.is_empty() && !rel.contains('/'))
                        .then(|| (rel.to_string(), contents.is_none(), contents.is_some()))
                })
                .take(limit.saturating_add(1))
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

    /// Someone else writing `contents` to the file at `path` on the server.
    pub fn change(path: &str, contents: &str) {
        SERVER.with(|s| {
            if let Some(server) = s.borrow_mut().as_mut() {
                server
                    .entries
                    .insert(path.to_string(), Some(contents.as_bytes().to_vec()));
                *server.versions.entry(path.to_string()).or_default() += 1;
            }
        });
    }

    /// Deletes `path` on the installed server, as someone else working on it might.
    pub fn remove(path: &str) {
        SERVER.with(|s| {
            if let Some(server) = s.borrow_mut().as_mut() {
                server.entries.remove(path);
            }
        });
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

    /// Trusts the fake server's key, if one is installed.
    pub(super) fn trust(_host: &UnknownHost) -> bool {
        SERVER.with(|s| match s.borrow_mut().as_mut() {
            Some(server) => {
                server.unknown = false;
                true
            }
            None => false,
        })
    }

    pub(super) fn fetch(remote_path: &str, cancel: &AtomicBool) -> Option<Result<Fetched>> {
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
            if server.unknown {
                return Some(Err(UnknownHost {
                    server: "example.com:22".into(),
                    id: "example.com:22".into(),
                    key_type: "ssh-ed25519".into(),
                    fingerprint: "SHA256:fake".into(),
                    key: b"fake".to_vec(),
                    path: PathBuf::new(),
                    differs_from: None,
                }
                .into()));
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
                    download_dir(server, remote_path, &local, 0, cancel, &mut budget)?;
                } else {
                    download_file(server, remote_path, &local, cancel, &mut budget)?;
                }
                Ok(Fetched {
                    temp,
                    local,
                    is_dir,
                    stamp: server.stamp(remote_path),
                })
            })())
        })
    }

    pub(super) fn upload(
        local_path: &Path,
        remote_path: &str,
        expected: Option<RemoteStamp>,
    ) -> Option<Result<RemoteStamp>> {
        SERVER.with(|s| {
            let mut server = s.borrow_mut();
            let server = server.as_mut()?;
            if let Some(message) = &server.fail {
                return Some(Err(anyhow::anyhow!("{message}")));
            }
            Some((|| {
                if expected.is_some() && server.stamp(remote_path) != expected {
                    bail!(trf(
                        "msg.sftp_changed_on_server",
                        &[("path", remote_path.to_string())]
                    ));
                }
                let bytes = std::fs::read(local_path)?;
                server
                    .entries
                    .insert(remote_path.to_string(), Some(bytes.clone()));
                *server.versions.entry(remote_path.to_string()).or_default() += 1;
                server.uploaded.push((remote_path.to_string(), bytes));
                Ok(server.stamp(remote_path).unwrap())
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
        let f = fetch(&conn, "/tmp/sshtest/data_file.txt", &never).unwrap();
        assert!(!f.is_dir);
        assert_eq!(std::fs::read_to_string(&f.local).unwrap(), "hello remote\n");

        let f = fetch(&conn, "/tmp/sshtest/data", &never).unwrap();
        assert!(f.is_dir);
        assert_eq!(
            std::fs::read_to_string(f.local.join("sub/b.txt")).unwrap(),
            "nested\n"
        );

        let up_dir = tempfile::tempdir().unwrap();
        let up_local = up_dir.path().join("out.txt");
        std::fs::write(&up_local, "uploaded by smoke test\n").unwrap();
        upload(&conn, &up_local, "/tmp/sshtest/uploaded.txt", None, &never).unwrap();
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
        assert_eq!(
            label(&conn, "notes.txt"),
            "sftp://ann@example.com:22/~/notes.txt"
        );
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
        let f = fetch(&conn, "/srv/report.txt", &never).unwrap();
        assert!(!f.is_dir);
        assert_eq!(std::fs::read_to_string(&f.local).unwrap(), "hello");
        let f = fetch(&conn, "/srv/data", &never).unwrap();
        assert!(f.is_dir);
        assert_eq!(std::fs::read_to_string(f.local.join("a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_to_string(f.local.join("sub/b.txt")).unwrap(),
            "b"
        );
        // A trailing slash on the remote path still names the staged folder after it, not
        // the fallback name.
        let f = fetch(&conn, "/srv/data/", &never).unwrap();
        assert_eq!(f.local.file_name().unwrap(), "data");
        // A trailing slash on a *file*'s path does not make it look missing: whatever
        // decided it is a file is the same lookup that then reads it.
        let f = fetch(&conn, "/srv/report.txt/", &never).unwrap();
        assert!(!f.is_dir);
        assert_eq!(std::fs::read_to_string(&f.local).unwrap(), "hello");
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
        let err = fetch(&conn, "/a.txt", &never).err().unwrap();
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
        let err = fetch(&conn, "/srv", &cancelled).err().unwrap();
        assert_eq!(err.to_string(), tr("msg.sftp_cancelled"));
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
        let never = AtomicBool::new(false);
        let first = upload(&conn, &local, "/a.txt", None, &never).unwrap();
        assert_eq!(fake::uploaded("/a.txt").unwrap(), b"new");
        assert_eq!(fake::upload_count("/a.txt"), 1);
        upload(&conn, &local, "/a.txt", Some(first), &never).unwrap();
        assert_eq!(fake::upload_count("/a.txt"), 2);
        fake::uninstall();
    }

    /// A file someone else changed on the server since it was fetched is not written over.
    #[test]
    fn uploading_over_a_file_changed_on_the_server_is_refused() {
        fake::install(fake::Server::new().file("/a.txt", "old"));
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        let never = AtomicBool::new(false);
        let fetched = fetch(&conn, "/a.txt", &never).unwrap();
        std::fs::write(&fetched.local, "mine").unwrap();
        fake::change("/a.txt", "theirs");
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, &never).unwrap_err();
        assert!(err.to_string().contains("/a.txt"), "{err}");
        fake::remove("/a.txt");
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, &never).unwrap_err();
        assert!(err.to_string().contains("/a.txt"), "{err}");
        assert_eq!(fake::upload_count("/a.txt"), 0);
        fake::uninstall();
    }

    #[test]
    fn a_server_not_trusted_yet_fails_with_its_fingerprint_until_trusted() {
        fake::install(fake::Server::new().file("/a.txt", "a").unknown_host());
        let conn = RemoteConnection {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
        };
        let never = AtomicBool::new(false);
        let err = fetch(&conn, "/a.txt", &never).err().unwrap();
        let host = err.downcast_ref::<UnknownHost>().unwrap().clone();
        assert!(err.to_string().contains(&host.fingerprint));
        trust_host(&host).unwrap();
        assert!(fetch(&conn, "/a.txt", &never).is_ok());
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

        fn list_dir(
            &self,
            _remote: &str,
            _limit: usize,
            _cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
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
        assert_eq!(err.to_string(), tr("msg.sftp_cancelled"));
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

        fn list_dir(
            &self,
            _remote: &str,
            _limit: usize,
            _cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
            unreachable!("not a folder")
        }
    }

    struct ManyFiles(usize);

    impl RemoteFs for ManyFiles {
        fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(std::io::Cursor::new(b"x".to_vec())))
        }

        fn list_dir(
            &self,
            remote: &str,
            limit: usize,
            _cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
            if remote != "/many" {
                return Ok(Vec::new());
            }
            Ok((0..self.0.min(limit + 1))
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
        let mut budget = Budget::with_limits(u64::MAX, 4);
        let err = download_dir(&fs, "/many", &local, 0, &never, &mut budget).unwrap_err();
        assert_eq!(
            err.to_string(),
            trf("msg.sftp_too_many", &[("count", "4".to_string())])
        );
        // The folder itself counts as one entry; a listing longer than what is left stops
        // the fetch before any of it is read.
        assert_eq!(std::fs::read_dir(&local).unwrap().count(), 0);
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
        assert_eq!(err.to_string(), budget.too_large().to_string());
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
    fn known_hosts_are_remembered_one_line_per_server_keeping_the_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/known_hosts");
        remember_host(&path, "example.com:22", &[1, 2, 3]).unwrap();
        let loaded = load_known_hosts(&path).unwrap();
        assert_eq!(loaded.get("example.com:22"), Some(&vec![1, 2, 3]));

        // Another window added a server, and someone wrote a note: both are kept, and a
        // server remembered again replaces its own line only.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("# a note\nother:22 ff\nnot a host line\n");
        std::fs::write(&path, text).unwrap();
        remember_host(&path, "example.com:22", &[9]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# a note\nother:22 ff\nnot a host line\nexample.com:22 09\n"
        );
    }

    #[test]
    fn load_known_hosts_ignores_a_missing_file_blank_lines_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        assert!(load_known_hosts(&path).unwrap().is_empty());
        std::fs::write(&path, "# a comment\n\nexample.com:22 0102ff\n").unwrap();
        let loaded = load_known_hosts(&path).unwrap();
        assert_eq!(loaded.get("example.com:22"), Some(&vec![1, 2, 0xff]));
    }

    /// A known hosts file that exists but cannot be read must not look like one listing no
    /// servers: a server whose key changed would then look like one never seen.
    #[test]
    fn an_unreadable_known_hosts_file_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::create_dir(&path).unwrap();
        assert!(load_known_hosts(&path).is_err());
        assert!(remember_host(&path, "h:22", &[1]).is_err());
        let path = dir.path().join("corrupt");
        std::fs::write(&path, "h:22 0102\nh2:22 not-hex\n").unwrap();
        assert!(load_known_hosts(&path).is_err());
    }

    /// An SSH key blob of type `kind` with `rest` after it.
    fn blob(kind: &str, rest: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in std::iter::once(kind.as_bytes()).chain(rest.iter().copied()) {
            out.extend_from_slice(&(part.len() as u32).to_be_bytes());
            out.extend_from_slice(part);
        }
        out
    }

    /// `key` in padded base64, as a known hosts file holds it.
    fn padded(key: &[u8]) -> String {
        let mut s = encode_base64(key);
        while !s.len().is_multiple_of(4) {
            s.push('=');
        }
        s
    }

    /// The user's own known hosts file is read as `ssh` reads it: a matching key is trusted, a
    /// different key of the same type (or a revoked one) refuses the connection, a key of
    /// another type is only a warning, and a line that can't be read fails closed.
    #[test]
    fn the_users_known_hosts_file_is_checked_as_ssh_checks_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let ours = blob("ssh-ed25519", &[&[1; 32]]);
        let theirs = blob("ssh-ed25519", &[&[2; 32]]);
        let ecdsa = blob("ecdsa-sha2-nistp256", &[b"nistp256", &[4; 65]]);
        let check = |text: &str, host: &str, port: u16, key: &[u8]| {
            std::fs::write(&path, text).unwrap();
            check_user_hosts(&path, host, port, key)
        };
        let line = |name: &str, kind: &str, key: &[u8]| format!("{name} {kind} {}\n", padded(key));

        assert_eq!(
            check_user_hosts(&dir.path().join("none"), "h", 22, &ours).unwrap(),
            UserHosts::Unknown
        );
        let known = line("example.com", "ssh-ed25519", &ours);
        assert_eq!(
            check(&known, "Example.COM", 22, &ours).unwrap(),
            UserHosts::Match
        );
        assert_eq!(
            check(&known, "example.com", 22, &theirs).unwrap(),
            UserHosts::Changed
        );
        // Another port is another server to `ssh`, recorded as `[host]:port`.
        assert_eq!(
            check(&known, "example.com", 2222, &theirs).unwrap(),
            UserHosts::Unknown
        );
        let on_port = line("[example.com]:2222", "ssh-ed25519", &ours);
        assert_eq!(
            check(&on_port, "example.com", 2222, &ours).unwrap(),
            UserHosts::Match
        );

        let other = line("example.com", "ecdsa-sha2-nistp256", &ecdsa);
        assert_eq!(
            check(&other, "example.com", 22, &ours).unwrap(),
            UserHosts::OtherType
        );

        let revoked = format!("{known}@revoked {known}");
        assert_eq!(
            check(&revoked, "example.com", 22, &ours).unwrap(),
            UserHosts::Changed
        );
        let authority = format!("@cert-authority {}", line("*.com", "ssh-ed25519", &theirs));
        assert_eq!(
            check(&authority, "example.com", 22, &ours).unwrap(),
            UserHosts::Unknown
        );
        let unknown_type = "example.com sk-ssh-ed25519@openssh.com AAAA\n";
        assert_eq!(
            check(unknown_type, "example.com", 22, &ours).unwrap(),
            UserHosts::Unknown
        );

        assert!(check("example.com ssh-ed25519 !!!\n", "example.com", 22, &ours).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(check_user_hosts(&path, "example.com", 22, &ours).is_err());
    }

    #[test]
    fn fingerprints_and_key_types_read_as_ssh_shows_them() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg");
        assert_eq!(encode_base64(b"fo"), "Zm8");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"hello"), "aGVsbG8");
        let mut key = 11u32.to_be_bytes().to_vec();
        key.extend_from_slice(b"ssh-ed25519");
        key.extend_from_slice(&[0, 0, 0, 1, 7]);
        assert_eq!(key_type(&key), Some("ssh-ed25519"));
        assert_eq!(key_type(&[0, 0]), None);
        assert_eq!(
            host_key_methods("ssh-rsa"),
            "rsa-sha2-512,rsa-sha2-256,ssh-rsa"
        );
    }

    #[test]
    fn servers_are_named_with_ipv6_addresses_in_brackets() {
        assert_eq!(host_id("Example.COM", 22), "example.com:22");
        assert_eq!(host_id("::1", 2222), "[::1]:2222");
        assert_eq!(host_id("[::1]", 22), "[::1]:22");
    }

    #[test]
    fn remote_names_that_cannot_be_local_file_names_are_left_out() {
        assert!(local_name_ok("a.txt"));
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            assert!(!local_name_ok(bad), "{bad:?}");
        }
        if cfg!(windows) {
            for bad in ["CON", "nul.txt", "a:b", "trailing.", "a?"] {
                assert!(!local_name_ok(bad), "{bad:?}");
            }
        }
    }

    /// Folders nested deeper than any real tree (a server looping a folder into itself, say)
    /// stop the fetch.
    #[test]
    fn a_folder_nested_too_deep_stops_the_fetch() {
        struct Endless;
        impl RemoteFs for Endless {
            fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
                unreachable!("only folders")
            }
            fn list_dir(
                &self,
                _remote: &str,
                _limit: usize,
                _cancel: &AtomicBool,
            ) -> Result<Vec<(String, bool, bool)>> {
                Ok(vec![("d".to_string(), true, false)])
            }
        }
        let never = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let mut budget = Budget::new();
        let err = download_dir(
            &Endless,
            "/d",
            &dir.path().join("d"),
            0,
            &never,
            &mut budget,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains(&MAX_REMOTE_DEPTH.to_string()),
            "{err}"
        );
    }

    #[test]
    fn a_password_is_overwritten_when_wiped() {
        let mut s = String::from("s3cret");
        wipe(&mut s);
        assert!(s.is_empty());
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
