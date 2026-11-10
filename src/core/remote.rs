//! Files and folders on another machine, read and written over SFTP (or FTP, see
//! [`super::ftp`]): an SSH connection this program makes itself, to a path the server sees
//! under the signed-in user's own account. The connection is reached as `ssh` would reach it,
//! following the user's `~/.ssh/config` (see [`super::ssh_config`]): an alias, its jump hosts
//! (`ProxyJump`) or `ProxyCommand`, or else through a SOCKS5 or HTTP proxy (see
//! [`super::net`]). It signs in with a password, a private key, or the SSH agent's and the
//! configuration's keys, answering whatever else the server asks while signing in (a one-time
//! code, say) by asking the user (see [`set_asker`]).
//!
//! A file or folder opened this way is staged in a temporary local copy first, the same as a
//! Git revision or an extracted archive is. Unlike those, it stays writable: saving uploads
//! the local copy back over a fresh connection made with the same credentials kept with its
//! tab, since a connection is not worth holding open for as long as a tab might stay open
//! (through sleep, a changed network, or a server that closes idle sessions). An upload
//! updates the existing file in place to keep its access controls, and is refused when the
//! file changed on the server since it was fetched (see [`upload`]); a folder is synchronized
//! back the same way, file by file (see [`super::remote_sync`]).
//!
//! The server's host key is checked before a password or a key ever reaches it: against the
//! user's own `~/.ssh/known_hosts` (a host certificate against the authorities it names, see
//! [`super::ssh_cert`]), then against the servers this program was told to trust (kept in
//! [`known_hosts_path`]). A server in neither is not trusted silently: the fetch fails with
//! [`UnknownHost`], carrying the key's fingerprint for the user to check and accept with
//! [`trust_host`]. See [`verify_host_key`].
//!
//! A fetch is bounded: see [`Budget`]. The one real network path, [`RemoteFs`] for
//! `ssh2::Sftp`, is a thin seam so the rest — walking a folder, the budget, cancelling midway —
//! is tested without a real connection, the same way [`fake`] stands in for one entirely in
//! GUI tests.

use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    /// The keys of the running SSH agent (`SSH_AUTH_SOCK`, or Pageant on Windows), then those
    /// `~/.ssh/config` names for the server or else the usual `~/.ssh/id_*`: SFTP only.
    Agent,
}

impl Drop for Auth {
    fn drop(&mut self) {
        match self {
            Auth::Password(p) | Auth::Key { passphrase: p, .. } => wipe(p),
            Auth::Agent => {}
        }
    }
}

/// How a connection talks to the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// SFTP over SSH.
    #[default]
    Sftp,
    /// FTP without encryption: the password and the files cross the network as they are.
    Ftp,
    /// FTP switched to TLS before signing in (`AUTH TLS`, "explicit FTPS").
    Ftps,
    /// FTP over a connection that is TLS from the start ("implicit FTPS").
    FtpsImplicit,
}

impl Protocol {
    pub const ALL: [Protocol; 4] = [
        Protocol::Sftp,
        Protocol::Ftps,
        Protocol::FtpsImplicit,
        Protocol::Ftp,
    ];

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Sftp => 22,
            Protocol::Ftp | Protocol::Ftps => 21,
            Protocol::FtpsImplicit => 990,
        }
    }

    /// The URL scheme naming it, as FileZilla and others spell it.
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Sftp => "sftp",
            Protocol::Ftp => "ftp",
            Protocol::Ftps => "ftpes",
            Protocol::FtpsImplicit => "ftps",
        }
    }

    pub fn from_scheme(scheme: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.scheme().eq_ignore_ascii_case(scheme))
    }

    pub fn is_ftp(self) -> bool {
        self != Protocol::Sftp
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
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    /// Empty for SFTP to sign in as `~/.ssh/config` says, or as the local user.
    pub user: String,
    pub auth: Auth,
    pub options: ConnOptions,
}

/// How a connection is made, beyond where it goes and who signs in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnOptions {
    /// A proxy to connect through, for SFTP and FTP alike. For SFTP, a `ProxyJump` or
    /// `ProxyCommand` of `~/.ssh/config` takes its place.
    pub proxy: Option<super::net::Proxy>,
    /// FTP only: active mode, where the server connects back to this machine for each
    /// transfer (`EPRT`, or `PORT`), instead of passive mode.
    pub active: bool,
    /// FTPS only: a certificate (PEM) to sign in with, for a server that asks for one.
    pub client_cert: Option<PathBuf>,
    /// The certificate's private key (PEM), when it is not in the certificate's file.
    pub client_key: Option<PathBuf>,
    /// Whether *Save* writes an existing file over in place, rather than replacing it through
    /// a staged copy where its owner, group and permissions can be kept (see
    /// [`super::remote_stage`]). For servers with access-control lists or hard links, which a
    /// replacement would lose. FTP always writes in place.
    pub in_place: bool,
    /// SFTP only: whether the port was written out, so that it wins over the `Port` of
    /// `~/.ssh/config` even when it is 22. Otherwise only another port does.
    pub port_given: bool,
}

/// One question of a [`Challenge`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    /// Whether the answer may be shown as it is typed (it is not a password or a code).
    pub echo: bool,
}

/// What a server asks while signing in, beyond the password or key given (a one-time code,
/// a new password, a key's passphrase): the questions are answered together, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    /// The server asking, as shown.
    pub server: String,
    /// The server's title for the questions, often empty.
    pub name: String,
    /// The server's explanation, often empty.
    pub instructions: String,
    pub prompts: Vec<Prompt>,
}

/// Answers a [`Challenge`] by asking the user, one answer for each question, or `None` when
/// the user declined, or once the flag (*Cancel*) is set.
pub type Asker = dyn Fn(&Challenge, &AtomicBool) -> Option<Vec<String>> + Send + Sync;

static ASKER: std::sync::RwLock<Option<std::sync::Arc<Asker>>> = std::sync::RwLock::new(None);

/// Sets how a server's questions are put to the user: in the window, or on the terminal. With
/// none, every question is declined.
pub fn set_asker(asker: Option<std::sync::Arc<Asker>>) {
    if let Ok(mut slot) = ASKER.write() {
        *slot = asker;
    }
}

/// Puts `challenge` to the user, as [`set_asker`] says.
fn ask(challenge: &Challenge, cancel: &AtomicBool) -> Option<Vec<String>> {
    let asker = ASKER.read().ok()?.clone()?;
    asker(challenge, cancel).filter(|a| a.len() == challenge.prompts.len())
}

/// A remote file's size and modified time, to notice it changed on the server. Two stamps are
/// the same when their sizes and times are.
#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteStamp {
    pub size: u64,
    pub modified: u64,
    /// Whether `modified` was within [`RECENT`] seconds of this machine's clock when the stamp
    /// was read: another change in the same second would keep it, size and all.
    pub recent: bool,
}

impl PartialEq for RemoteStamp {
    fn eq(&self, other: &Self) -> bool {
        (self.size, self.modified) == (other.size, other.modified)
    }
}

impl Eq for RemoteStamp {}

/// How many seconds old a modified time read is still too new to tell a change by, allowing
/// for the server's clock being a little ahead.
const RECENT: u64 = 5;

impl RemoteStamp {
    pub(crate) fn new(size: u64, modified: u64) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        RemoteStamp {
            size,
            modified,
            recent: modified != 0 && modified + RECENT >= now,
        }
    }

    fn of(stat: &ssh2::FileStat) -> Self {
        Self::new(stat.size.unwrap_or(0), stat.mtime.unwrap_or(0))
    }

    /// Whether a change that kept the size cannot be told from the stamp alone: the server
    /// gave no modified time (an FTP server without `MDTM` or `MLST`), or it was
    /// [`RemoteStamp::recent`].
    pub fn is_weak(self) -> bool {
        self.modified == 0 || self.recent
    }
}

/// An upload that failed after it had started writing over the file `path`, which may now be
/// partly written; `stamp` is how it is on the server now, when that could be read. Attached to
/// the upload's error, so the next attempt expects that stamp instead of refusing the file as
/// changed by someone else.
#[derive(Debug)]
pub struct PartlyWritten {
    pub path: String,
    pub stamp: Option<RemoteStamp>,
}

impl std::fmt::Display for PartlyWritten {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&trf(
            "msg.remote_partly_written",
            &[("path", self.path.clone())],
        ))
    }
}

/// What [`fetch`] staged: the temporary folder holding it (deleted when dropped), the local
/// copy in it, whether that is a folder, for a file how it was on the server, and for a folder
/// what it held.
pub struct Fetched {
    pub temp: tempfile::TempDir,
    pub local: PathBuf,
    pub is_dir: bool,
    pub stamp: Option<RemoteStamp>,
    pub manifest: Option<Manifest>,
}

/// What a folder fetch read, to tell later what changed in its local copy and whether the
/// server's copy changed meanwhile (see [`super::remote_sync`]). Paths are relative to the
/// folder and `/`-separated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Each file: how it was on the server, and a hash of its bytes as written here.
    pub files: std::collections::BTreeMap<String, (RemoteStamp, u64)>,
    pub dirs: std::collections::BTreeSet<String>,
}

/// A hash of the file at `path`'s bytes, to tell whether it changed since it was fetched or
/// last uploaded; only ever compared within one run of the program.
pub fn file_hash(path: &Path) -> Option<u64> {
    hash_of(std::fs::File::open(path).ok()?, &AtomicBool::new(false)).ok()
}

/// [`file_hash`] of what `from` reads. Setting `cancel` stops it, and it fails.
pub(crate) fn hash_of(mut from: impl Read, cancel: &AtomicBool) -> Result<u64> {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        check(cancel)?;
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finish());
        }
        h.write(&buf[..n]);
    }
}

/// Whether the file `remote` is still as it was when stamped `stamp` with contents hashing to
/// `hash`: by its stamp, and when the stamp has no time to tell a change that kept the size
/// (see [`RemoteStamp::is_weak`]), by reading it.
pub(crate) fn still_as(
    r: &(impl RemoteFs + ?Sized),
    remote: &str,
    stamp: RemoteStamp,
    hash: u64,
    cancel: &AtomicBool,
) -> Result<bool> {
    if r.stamp(remote)? != Some(stamp) {
        return Ok(false);
    }
    if !stamp.is_weak() {
        return Ok(true);
    }
    Ok(hash_of(r.open_read(remote)?, cancel)? == hash)
}

/// How long connecting, looking up the address, and authenticating may each take before the
/// server is treated as unreachable, and how long a single read or write of the connection may
/// then take.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// `host:port` as shown to the user, with an IPv6 address in brackets.
pub(crate) fn server(host: &str, port: u16) -> String {
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

/// Older pins retained the host's case and could leave IPv6 addresses unbracketed.
fn canonical_host_id(id: &str) -> Option<String> {
    let (host, port) = id.rsplit_once(':')?;
    Some(host_id(host, port.parse().ok()?))
}

/// `host`'s addresses, looked up with its own bound on how long that may take: `ToSocketAddrs`
/// gives the operating system's resolver no timeout of its own, so without this a lookup that
/// hangs (an unreachable DNS server, say) would hang the fetch past `CONNECT_TIMEOUT` and past
/// *Cancel*. The lookup itself still runs to completion on its own thread even after timing
/// out here; that thread is harmless once nothing reads its result.
pub(crate) fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
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

/// The FTPS server certificates the user chose to trust although no certificate authority
/// vouches for them (a self-signed one, say), in the same format as [`known_hosts_path`]: the
/// whole certificate, in hexadecimal, after `host:port`.
pub fn known_certs_path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = CERTS_PATH.lock().ok().and_then(|p| p.clone()) {
        return Some(path);
    }
    let settings = crate::config::config_path()?;
    let name = if settings.file_name().is_some_and(|n| n == "oxidemerge.json") {
        "oxidemerge-known_certs"
    } else {
        "known_certs"
    };
    Some(settings.with_file_name(name))
}

/// Where a test keeps trusted certificates instead (see [`known_certs_path`]): for every
/// thread, as listing a server's folders connects on one of its own.
#[cfg(test)]
pub(crate) static CERTS_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Checks an FTPS server's certificate, `cert`, that no certificate authority vouches for,
/// against the one the user chose to trust for `host` and `port`: a server whose certificate
/// was never trusted fails with [`UnknownHost`], for the user to check its fingerprint, and one
/// whose certificate differs from the trusted one fails without asking, as a changed SSH host
/// key does (see [`verify_host_key`]).
pub(crate) fn check_pinned_cert(host: &str, port: u16, cert: &[u8]) -> Result<()> {
    let path = known_certs_path().with_context(|| {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", "known_certs".to_string())],
        )
    })?;
    let known = load_known_hosts(&path)?;
    let id = host_id(host, port);
    match known.get(&id) {
        Some(pin) if pin == cert => Ok(()),
        Some(_) => bail!(trf(
            "msg.ftps_cert_changed",
            &[
                ("server", server(host, port)),
                ("id", id),
                ("file", path.display().to_string()),
            ],
        )),
        None => Err(UnknownHost {
            server: server(host, port),
            id,
            key_type: tr("label.tls_certificate").to_string(),
            fingerprint: format!(
                "SHA256:{}",
                Sha256::digest(cert)
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":")
            ),
            key: cert.to_vec(),
            path,
            differs_from: None,
        }
        .into()),
    }
}

/// The known hosts files of `ssh`'s, checked first: the user's own (`user`, as
/// `UserKnownHostsFile` names them, or else `~/.ssh/known_hosts` and `known_hosts2`), then the
/// system's. A test reads only those it names.
fn ssh_known_hosts(user: Option<&[PathBuf]>) -> Vec<PathBuf> {
    if cfg!(test) {
        return user.map(<[PathBuf]>::to_vec).unwrap_or_default();
    }
    let mut files = match user {
        Some(user) => user.to_vec(),
        None => dirs::home_dir()
            .map(|h| {
                let ssh = h.join(".ssh");
                vec![ssh.join("known_hosts"), ssh.join("known_hosts2")]
            })
            .unwrap_or_default(),
    };
    if cfg!(windows) {
        if let Some(data) = std::env::var_os("ProgramData") {
            files.push(Path::new(&data).join("ssh").join("ssh_known_hosts"));
        }
    } else {
        files.push(PathBuf::from("/etc/ssh/ssh_known_hosts"));
        files.push(PathBuf::from("/etc/ssh/ssh_known_hosts2"));
    }
    files
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
pub(crate) fn encode_base64(bytes: &[u8]) -> String {
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
            ));
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
            .and_then(|(id, hex)| Some((canonical_host_id(id)?, decode_hex(hex.trim())?)))
        else {
            bail!(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ));
        };
        if known.get(&id).is_some_and(|old| old != &key) {
            bail!(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ));
        }
        known.insert(id, key);
    }
    Ok(known)
}

/// `s` decoded from base64, padded or not; `None` if it is not base64.
pub(crate) fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut n, mut bits) = (0u32, 0);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        n = n << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
            n &= (1 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
}

/// The key types checked in a known hosts file: those of the host key algorithms this program
/// asks a server for (see [`host_key_methods`]), and DSA, so that a line for one is reported.
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
    /// It records another key of the same type for the server.
    Changed,
    /// It marks this key `@revoked`.
    Revoked,
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

/// What a line of a known hosts file says of its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    /// The host's own key.
    None,
    /// `@revoked`: never to be accepted.
    Revoked,
    /// `@cert-authority`: an authority whose host certificates are trusted for the hosts.
    CertAuthority,
}

/// A line of an OpenSSH known hosts file for a key type in [`KEY_TYPES`].
struct KnownLine {
    marker: Marker,
    /// The host patterns, or a hashed name (`|1|salt|hash`).
    hosts: String,
    kind: String,
    key: Vec<u8>,
}

/// The lines of the known hosts file at `path` for the key types it checks; `None` when there
/// is no such file. Lines of other types, and with markers other than `@revoked` and
/// `@cert-authority`, are passed over; a line of a checked type whose key can't be read is an
/// error, since it could be the one for the server being checked.
fn read_known_lines(path: &Path) -> Result<Option<Vec<KnownLine>>> {
    let unreadable = || {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", path.display().to_string())],
        )
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context(unreadable()),
    };
    let mut lines = Vec::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let mut hosts = fields.next();
        let marker = match hosts {
            Some("@revoked") => {
                hosts = fields.next();
                Marker::Revoked
            }
            Some("@cert-authority") => {
                hosts = fields.next();
                Marker::CertAuthority
            }
            Some(marker) if marker.starts_with('@') => continue,
            _ => Marker::None,
        };
        let (Some(hosts), Some(kind)) = (hosts, fields.next()) else {
            continue;
        };
        if !KEY_TYPES.contains(&kind) {
            continue;
        }
        let key = fields.next().and_then(decode_base64);
        let Some(key) = key.filter(|k| key_type(k) == Some(kind)) else {
            bail!(unreadable());
        };
        lines.push(KnownLine {
            marker,
            hosts: hosts.to_string(),
            kind: kind.to_string(),
            key,
        });
    }
    Ok(Some(lines))
}

/// Whether `name` matches the glob `pattern`, where `*` stands for any run of characters and
/// `?` for any one, as OpenSSH matches host patterns.
pub(crate) fn glob_match(name: &[u8], pattern: &[u8]) -> bool {
    let (mut n, mut p) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == name[n]) {
            n += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// Whether the host field of `line` names `name` (lowercase): a hashed name, checked by
/// libssh2 with the help of `sess`, or comma-separated patterns, ignoring case, any of them
/// matching and none negated with `!` matching, as OpenSSH reads them.
fn names_host(line: &KnownLine, name: &str, sess: &ssh2::Session) -> Result<bool> {
    if line.hosts.starts_with('|') {
        let mut hosts = sess.known_hosts()?;
        let text = format!("{} {} {}", line.hosts, line.kind, encode_base64(&line.key));
        hosts.read_str(&text, ssh2::KnownHostFileKind::OpenSSH)?;
        return match hosts.check(name, &line.key) {
            ssh2::CheckResult::Failure => bail!("known hosts"),
            found => Ok(matches!(found, ssh2::CheckResult::Match)),
        };
    }
    let mut named = false;
    for pattern in line.hosts.split(',') {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pattern),
        };
        if glob_match(name.as_bytes(), pattern.to_ascii_lowercase().as_bytes()) {
            if negated {
                return Ok(false);
            }
            named = true;
        }
    }
    Ok(named)
}

/// The lines of the known hosts file at `path` naming the server `name` (see [`KeyName`]), or
/// `None` when there is no such file.
fn lines_for(path: &Path, name: &str) -> Result<Option<Vec<KnownLine>>> {
    let Some(lines) = read_known_lines(path)? else {
        return Ok(None);
    };
    let unreadable = || {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", path.display().to_string())],
        )
    };
    let sess = ssh2::Session::new().context(unreadable())?;
    let mut out = Vec::new();
    for line in lines {
        if names_host(&line, name, &sess).context(unreadable())? {
            out.push(line);
        }
    }
    Ok(Some(out))
}

/// The key types the known hosts `files` record for the server `name`, to ask the server for
/// those first, and whether they name a certificate authority for it. Nothing for a file that
/// can't be read: checking the key says why.
fn user_host_types(files: &[PathBuf], name: &str) -> (Vec<String>, bool) {
    let mut types = Vec::new();
    let mut authority = false;
    for path in files {
        for line in lines_for(path, name).ok().flatten().unwrap_or_default() {
            match line.marker {
                Marker::None => types.push(line.kind),
                Marker::CertAuthority => authority = true,
                Marker::Revoked => {}
            }
        }
    }
    (types, authority)
}

/// Checks `key`, the plain host key of the server `name`, against the known hosts file at
/// `path`, as `ssh` would, against host patterns and hashed names. A missing file knows no
/// server; one that can't be read, or holding a line of a key type it checks whose key can't
/// be read, is an error.
fn check_user_hosts(path: &Path, name: &str, key: &[u8]) -> Result<UserHosts> {
    let Some(lines) = lines_for(path, name)? else {
        return Ok(UserHosts::Unknown);
    };
    if lines
        .iter()
        .any(|l| l.marker == Marker::Revoked && l.key == key)
    {
        return Ok(UserHosts::Revoked);
    }
    let listed: Vec<&KnownLine> = lines.iter().filter(|l| l.marker == Marker::None).collect();
    Ok(if listed.iter().any(|l| l.key == key) {
        UserHosts::Match
    } else if listed
        .iter()
        .any(|l| Some(l.kind.as_str()) == key_type(key))
    {
        UserHosts::Changed
    } else if !listed.is_empty() {
        UserHosts::OtherType
    } else {
        UserHosts::Unknown
    })
}

/// Adds `id`'s `key` to the known hosts file at `path`, replacing a line for the same server
/// and keeping every other line as it is now on disk (another window may have added one since
/// this one read it). Written to a temporary file that then takes the file's place.
fn remember_host(path: &Path, id: &str, key: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Held from reading to writing, so two windows trusting a server at the same moment both
    // keep theirs. A file of its own, since writing replaces the list's file with a new one.
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| trf("msg.cannot_write", &[("file", path.display().to_string())]))?;
    lock.lock()?;
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).context(trf(
                "msg.sftp_known_hosts_unreadable",
                &[("file", path.display().to_string())],
            ));
        }
    };
    let mut text: String = old
        .lines()
        .filter(|line| {
            line.trim()
                .split_once(' ')
                .and_then(|(i, _)| canonical_host_id(i))
                .as_deref()
                != Some(id)
        })
        .flat_map(|line| [line, "\n"])
        .collect();
    text.push_str(&format!("{id} {}\n", encode_hex(key)));
    super::fsutil::write_atomic_with(path, text.as_bytes(), false)
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
    /// The user's own known hosts file, when it records only keys of other types for this
    /// server.
    pub differs_from: Option<PathBuf>,
}

impl std::fmt::Display for UnknownHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = if self.key_type == tr("label.tls_certificate") {
            trf("msg.ftps_unknown_cert", &[("server", self.server.clone())])
        } else {
            trf(
                "msg.sftp_unknown_host",
                &[
                    ("server", self.server.clone()),
                    ("type", self.key_type.clone()),
                ],
            )
        };
        write!(f, "{message} {}", self.fingerprint)
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

/// The names a server's host key is looked up under.
#[derive(Debug)]
struct KeyName {
    /// `host:port`, as shown.
    shown: String,
    /// In a known hosts file of `ssh`'s: the host alone on port 22, `[host]:port` on any other
    /// (see [`ssh_name`]), or the `HostKeyAlias` as it is.
    ssh: String,
    /// In this program's own list (see [`host_id`]).
    id: String,
    /// What a host certificate must name among its principals: the host, or its alias.
    principal: String,
}

impl KeyName {
    fn new(host: &str, port: u16, alias: Option<&str>) -> Self {
        let named = alias.unwrap_or(host);
        KeyName {
            shown: server(host, port),
            ssh: match alias {
                Some(alias) => alias.to_ascii_lowercase(),
                None => ssh_name(host, port),
            },
            id: host_id(named, port),
            principal: named
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase(),
        }
    }
}

/// Checks the server's host key against both the user's known hosts `files` (normally just
/// `~/.ssh/known_hosts`) and the keys this program was told to trust, `known` (see
/// [`known_hosts_path`]). A match in either is sufficient only when neither rejects the key.
/// A host certificate is checked against the authorities the files name for the server (see
/// [`verify_host_cert`]). Called right after the handshake and before a password or a key ever
/// reaches the connection, so a server this fails for never sees them. A server neither knows
/// fails with [`UnknownHost`].
///
/// There is no prompt to trust a changed key anyway: that choice is easy to click past without
/// reading it, which defeats the point of checking at all. A key that genuinely changed (a
/// reinstalled server, a rotated key) is trusted again only by removing its line from the
/// known hosts file by hand.
fn verify_host_key(
    key: &[u8],
    name: &KeyName,
    known: &HashMap<String, Vec<u8>>,
    path: &Path,
    files: &[PathBuf],
    now: u64,
) -> Result<()> {
    if super::ssh_cert::is_cert(key) {
        return verify_host_cert(key, name, known, path, files, now);
    }
    let mut differs_from = None;
    let mut changed = None;
    let mut ssh_match = false;
    for file in files {
        match check_user_hosts(file, &name.ssh, key)? {
            UserHosts::Match => ssh_match = true,
            UserHosts::Changed => changed = changed.or(Some(file)),
            UserHosts::Revoked => bail!(trf(
                "msg.sftp_key_revoked",
                &[
                    ("server", name.shown.clone()),
                    ("file", file.display().to_string()),
                ],
            )),
            // The server sent another type of key than the file records for it: this
            // program's own list, or the user, decides.
            UserHosts::OtherType => differs_from = differs_from.or(Some(file.clone())),
            UserHosts::Unknown => {}
        }
    }
    if let (Some(file), false) = (changed, ssh_match) {
        bail!(trf(
            "msg.sftp_key_changed_ssh",
            &[
                ("server", name.shown.clone()),
                ("host", name.ssh.clone()),
                ("file", file.display().to_string()),
            ],
        ));
    }
    match known.get(&name.id) {
        Some(pin) if pin == key => Ok(()),
        Some(_) => bail!(trf(
            "msg.sftp_key_changed",
            &[
                ("server", name.shown.clone()),
                ("id", name.id.clone()),
                ("file", path.display().to_string()),
            ],
        )),
        None if ssh_match => Ok(()),
        None => Err(UnknownHost {
            server: name.shown.clone(),
            id: name.id.clone(),
            key_type: key_type(key).unwrap_or("?").to_string(),
            fingerprint: format!("SHA256:{}", encode_base64(&Sha256::digest(key))),
            key: key.to_vec(),
            path: path.to_path_buf(),
            differs_from,
        }
        .into()),
    }
}

/// Checks the host certificate `blob` (see [`verify_host_key`]) as `ssh` does: signed by an
/// authority a known hosts file names for the server (`@cert-authority`), neither revoked,
/// and valid for it now (see [`super::ssh_cert::check`]); a key this program was told to trust
/// for the server must still be the certificate's. A certificate from an authority not named
/// for the server is no better than its key, which is then checked as any other.
fn verify_host_cert(
    blob: &[u8],
    name: &KeyName,
    known: &HashMap<String, Vec<u8>>,
    path: &Path,
    files: &[PathBuf],
    now: u64,
) -> Result<()> {
    let invalid = |why: String| {
        anyhow::anyhow!(trf(
            "msg.sftp_cert_invalid",
            &[("server", name.shown.clone()), ("why", why)],
        ))
    };
    let cert = super::ssh_cert::parse(blob).map_err(invalid)?;
    let mut trusted = false;
    for file in files {
        for line in lines_for(file, &name.ssh)?.unwrap_or_default() {
            match line.marker {
                Marker::Revoked if line.key == cert.ca || line.key == cert.key => {
                    bail!(trf(
                        "msg.sftp_key_revoked",
                        &[
                            ("server", name.shown.clone()),
                            ("file", file.display().to_string()),
                        ],
                    ))
                }
                Marker::CertAuthority if line.key == cert.ca => trusted = true,
                _ => {}
            }
        }
    }
    if !trusted {
        return verify_host_key(&cert.key, name, known, path, files, now);
    }
    super::ssh_cert::check(&cert, blob, &name.principal, now).map_err(invalid)?;
    match known.get(&name.id) {
        Some(pin) if *pin != cert.key => bail!(trf(
            "msg.sftp_key_changed",
            &[
                ("server", name.shown.clone()),
                ("id", name.id.clone()),
                ("file", path.display().to_string()),
            ],
        )),
        _ => Ok(()),
    }
}

/// The plain host key algorithms asked for, most wanted first.
const HOST_KEY_METHODS: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "rsa-sha2-512",
    "rsa-sha2-256",
    "ssh-rsa",
];

/// The host certificate algorithms asked for, only of a server a known hosts file names a
/// certificate authority for: a certificate is otherwise no better than its key.
const CERT_METHODS: &[&str] = &[
    "ssh-ed25519-cert-v01@openssh.com",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "ecdsa-sha2-nistp521-cert-v01@openssh.com",
    "rsa-sha2-512-cert-v01@openssh.com",
    "rsa-sha2-256-cert-v01@openssh.com",
];

/// The host key algorithms to ask a server for: first those for the key `types` a known hosts
/// file records for it, so that it presents a key that can be checked, then certificates when
/// `certs` (an authority is named for it), then the others.
fn host_key_methods(types: &[String], certs: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    let wanted = types.iter().flat_map(|kind| -> &[&str] {
        match kind.as_str() {
            "ssh-rsa" => &["rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"],
            other => HOST_KEY_METHODS
                .iter()
                .find(|m| **m == other)
                .map_or(&[], std::slice::from_ref),
        }
    });
    let certs: &[&str] = if certs { CERT_METHODS } else { &[] };
    for m in wanted.chain(certs).chain(HOST_KEY_METHODS) {
        if !out.contains(m) {
            out.push(m);
        }
    }
    out.join(",")
}

/// The most jump hosts one connection goes through, counting those a jump host's own
/// configuration adds.
const MAX_JUMPS: usize = 8;

/// Opens an SFTP session authenticated as `conn` says, reaching the server as `~/.ssh/config`
/// says. Setting `cancel` stops it between steps.
fn session(conn: &RemoteConnection, cancel: &AtomicBool) -> Result<ssh2::Sftp> {
    let target = super::ssh_config::lookup(
        &conn.host,
        (conn.options.port_given || conn.port != Protocol::Sftp.default_port())
            .then_some(conn.port),
        Some(conn.user.as_str()).filter(|u| !u.is_empty()),
    )?;
    let sess = open_ssh(&target, &conn.auth, conn.options.proxy.as_ref(), 0, cancel)?;
    check(cancel)?;
    sess.sftp().with_context(|| {
        trf(
            "msg.sftp_no_sftp",
            &[("server", server(&target.host, target.port))],
        )
    })
}

/// An SSH session to `target`, its host key checked and signed in with `auth`, reached
/// through `proxy` unless its configuration names another way (see [`transport`]). `depth`
/// jump hosts led here already.
fn open_ssh(
    target: &super::ssh_config::Resolved,
    auth: &Auth,
    proxy: Option<&super::net::Proxy>,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<ssh2::Session> {
    let name = KeyName::new(&target.host, target.port, target.host_key_alias.as_deref());
    let path = known_hosts_path().with_context(|| {
        trf(
            "msg.sftp_known_hosts_unreadable",
            &[("file", "known_hosts".to_string())],
        )
    })?;
    let known = load_known_hosts(&path)?;
    let files = ssh_known_hosts(target.known_hosts_files.as_deref());
    let tcp = transport(target, proxy, depth, cancel)?;
    check(cancel)?;
    let mut sess = ssh2::Session::new().context("SSH")?;
    sess.set_timeout(CONNECT_TIMEOUT.as_millis() as u32);
    sess.set_tcp_stream(tcp);
    let mut types: Vec<String> = known
        .get(&name.id)
        .and_then(|k| key_type(k))
        .map(str::to_string)
        .into_iter()
        .collect();
    let (user_types, authority) = user_host_types(&files, &name.ssh);
    types.extend(user_types);
    sess.method_pref(
        ssh2::MethodType::HostKey,
        &host_key_methods(&types, authority),
    )
    .context("SSH")?;
    sess.handshake()
        .with_context(|| trf("msg.sftp_handshake", &[("server", name.shown.clone())]))?;
    let (key, _) = sess
        .host_key()
        .with_context(|| tr("msg.sftp_no_host_key"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    verify_host_key(key, &name, &known, &path, &files, now)?;
    check(cancel)?;
    sign_in(&sess, target, auth, &name.shown, cancel)?;
    Ok(sess)
}

/// The connection an SSH session to `target` runs over: carried by its `ProxyCommand`, through
/// its jump hosts (`ProxyJump`), or else made directly or through `proxy`. As with `ssh -J`, the
/// first jump host is reached as its own configuration says, and each one after through the
/// one before; each signs in with the SSH agent's and the configuration's keys, asking the
/// user for anything else (see [`sign_in`]).
fn transport(
    target: &super::ssh_config::Resolved,
    proxy: Option<&super::net::Proxy>,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<TcpStream> {
    if let Some(command) = &target.proxy_command {
        return super::net::command_stream(command);
    }
    let Some((last, before)) = target.jumps.split_last() else {
        return super::net::connect(&target.host, target.port, proxy, cancel);
    };
    if depth + target.jumps.len() > MAX_JUMPS {
        bail!(trf(
            "msg.ssh_too_many_jumps",
            &[("count", MAX_JUMPS.to_string())]
        ));
    }
    let mut hop = super::ssh_config::lookup(&last.host, last.port, last.user.as_deref())?;
    if !before.is_empty() {
        hop.jumps = before.to_vec();
        hop.proxy_command = None;
    }
    let hop_sess = open_ssh(&hop, &Auth::Agent, proxy, depth + 1, cancel)?;
    check(cancel)?;
    forward(hop_sess, &hop, &target.host, target.port)
}

/// A connection to `host` at `port` made by the jump host `sess` (the SSH session to `hop`),
/// relayed by a thread of its own for as long as either end stays open.
fn forward(
    sess: ssh2::Session,
    hop: &super::ssh_config::Resolved,
    host: &str,
    port: u16,
) -> Result<TcpStream> {
    let channel = sess
        .channel_direct_tcpip(
            host.trim_start_matches('[').trim_end_matches(']'),
            port,
            None,
        )
        .with_context(|| {
            trf(
                "msg.ssh_jump_refused",
                &[
                    ("jump", server(&hop.host, hop.port)),
                    ("server", server(host, port)),
                ],
            )
        })?;
    let (ours, theirs) = super::net::socket_pair()?;
    std::thread::spawn(move || relay(sess, channel, theirs));
    Ok(ours)
}

/// The most one direction of a jump host's relay holds while the other end is slow to take it.
const RELAY_BUFFER: usize = 256 << 10;

/// Copies between `socket` and `channel`, both ways, until both are done or either fails. The
/// session is non-blocking so one thread serves both directions, waiting while nothing moves
/// for either side to be ready (see [`relay_wait`]). Once `socket` is closed, what it sent is
/// still passed on, then the end of it, and the channel is read to its end, or given up on
/// once nothing came for [`CONNECT_TIMEOUT`], so a jump host that never ends it does not keep
/// the session open for good.
fn relay(sess: ssh2::Session, mut channel: ssh2::Channel, mut socket: TcpStream) {
    use std::io::ErrorKind::{Interrupted, WouldBlock};
    sess.set_blocking(false);
    if socket.set_nonblocking(true).is_err() {
        return;
    }
    let (mut up, mut down) = (Vec::new(), Vec::new());
    let mut buf = vec![0u8; 32 << 10];
    let (mut closed, mut eof_sent) = (false, false);
    let mut idle = 0;
    let mut last_moved = Instant::now();
    loop {
        let mut moved = false;
        if !closed && up.len() < RELAY_BUFFER {
            match socket.read(&mut buf) {
                Ok(0) => {
                    closed = true;
                    moved = true;
                }
                Ok(n) => {
                    up.extend_from_slice(&buf[..n]);
                    moved = true;
                }
                Err(e) if matches!(e.kind(), WouldBlock | Interrupted) => {}
                Err(_) => break,
            }
        }
        if !up.is_empty() {
            match channel.write(&up) {
                Ok(n) => {
                    up.drain(..n);
                    moved |= n > 0;
                }
                Err(e) if matches!(e.kind(), WouldBlock | Interrupted) => {}
                Err(_) => break,
            }
        }
        if down.len() < RELAY_BUFFER {
            match channel.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    down.extend_from_slice(&buf[..n]);
                    moved = true;
                }
                Err(e) if matches!(e.kind(), WouldBlock | Interrupted) => {}
                Err(_) => break,
            }
        }
        if !down.is_empty() {
            match socket.write(&down) {
                Ok(n) => {
                    down.drain(..n);
                    moved |= n > 0;
                }
                Err(e) if matches!(e.kind(), WouldBlock | Interrupted) => {}
                Err(_) => break,
            }
        }
        if closed && up.is_empty() && !eof_sent {
            match channel.send_eof() {
                Ok(()) => {
                    eof_sent = true;
                    moved = true;
                }
                Err(e) if e.code() == ssh2::ErrorCode::Session(-37) => {}
                Err(_) => break,
            }
        }
        if channel.eof() && down.is_empty() {
            break;
        }
        if moved {
            last_moved = Instant::now();
        } else if eof_sent && last_moved.elapsed() > CONNECT_TIMEOUT {
            break;
        }
        idle = if moved { 0 } else { (idle + 1).min(10) };
        if idle > 0 {
            relay_wait(&sess, &socket, &up, &down, closed, idle);
        }
    }
    let _ = socket.shutdown(std::net::Shutdown::Both);
}

/// Waits, while nothing moved, until the socket or the session's connection is ready for what
/// the relay has to do next, or a tenth of a second at most. Elsewhere than on Unix, sleeps a
/// little longer each time nothing moved, up to `idle` milliseconds.
#[cfg(unix)]
fn relay_wait(
    sess: &ssh2::Session,
    socket: &TcpStream,
    up: &[u8],
    down: &[u8],
    closed: bool,
    _idle: u64,
) {
    use std::os::unix::io::AsRawFd;
    let events = |read: bool, write: bool| {
        (if read { libc::POLLIN } else { 0 }) | (if write { libc::POLLOUT } else { 0 })
    };
    let mut fds = [
        libc::pollfd {
            fd: socket.as_raw_fd(),
            events: events(!closed && up.len() < RELAY_BUFFER, !down.is_empty()),
            revents: 0,
        },
        libc::pollfd {
            fd: sess.as_raw_fd(),
            events: events(down.len() < RELAY_BUFFER, !up.is_empty()),
            revents: 0,
        },
    ];
    // SAFETY: `fds` is a valid array of two `pollfd`s for the call.
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
}

#[cfg(not(unix))]
fn relay_wait(
    _sess: &ssh2::Session,
    _socket: &TcpStream,
    _up: &[u8],
    _down: &[u8],
    _closed: bool,
    idle: u64,
) {
    std::thread::sleep(Duration::from_millis(idle));
}

/// The ways of signing in tried, in order, for each kind of [`Auth`]: the one given first,
/// then what else the server asks for (a second factor, or a server that refused the first).
fn sign_in_order(auth: &Auth) -> [&'static str; 3] {
    match auth {
        Auth::Password(_) => ["password", "keyboard-interactive", "publickey"],
        Auth::Key { .. } | Auth::Agent => ["publickey", "keyboard-interactive", "password"],
    }
}

/// What the server still accepts to sign in `user`, or `None` once signed in (a server that
/// lets anybody in, or a second factor done).
fn offered(sess: &ssh2::Session, user: &str) -> Result<Option<String>> {
    match sess.auth_methods(user) {
        Ok(methods) => Ok(Some(methods.to_string())),
        Err(_) if sess.authenticated() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Signs `target`'s user in with `auth` as `ssh` would: the way `auth` gives first, then
/// whatever else the server still asks for, its questions answered with the password given
/// or by asking the user (see [`set_asker`]). Fails with why the way given was refused.
fn sign_in(
    sess: &ssh2::Session,
    target: &super::ssh_config::Resolved,
    auth: &Auth,
    shown: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let user = target.user.as_str();
    let refused = || {
        let key = match auth {
            Auth::Password(_) => "msg.sftp_password",
            Auth::Key { .. } => "msg.sftp_key",
            Auth::Agent => "msg.sftp_agent",
        };
        trf(key, &[("server", shown.to_string())])
    };
    let Some(mut methods) = offered(sess, user).with_context(refused)? else {
        return Ok(());
    };
    let password = match auth {
        Auth::Password(p) => Some(p.as_str()),
        _ => None,
    };
    let mut tried = Vec::new();
    let mut first_error = None;
    // Whether the password given was sent already, so that a server that refused it is not
    // sent it again another way: each try counts against the account.
    let mut spent = false;
    loop {
        let offers = |m: &str| methods.split(',').any(|o| o == m);
        let Some(method) = sign_in_order(auth)
            .into_iter()
            .find(|m| offers(m) && !tried.contains(m))
        else {
            break;
        };
        tried.push(method);
        let step = match method {
            "publickey" => match auth {
                Auth::Key { path, passphrase } => {
                    key_file(sess, user, path, passphrase, shown, cancel)
                }
                _ => any_key(sess, target, shown, cancel),
            },
            "password" => match password {
                Some(p) if !spent => {
                    spent = true;
                    sess.userauth_password(user, p).map_err(Into::into)
                }
                Some(_) => Err(anyhow::anyhow!(refused())),
                None => {
                    let prompt = trf(
                        "label.ssh_password_for",
                        &[("user", user.to_string()), ("server", shown.to_string())],
                    );
                    match ask_secret(shown, prompt, cancel) {
                        Some(mut p) => {
                            let r = sess.userauth_password(user, &p);
                            wipe(&mut p);
                            r.map_err(Into::into)
                        }
                        None => Err(anyhow::anyhow!(tr("msg.ssh_declined"))),
                    }
                }
            },
            _ => {
                let ask = |c: &Challenge| ask(c, cancel);
                let given = password.filter(|_| !spent);
                let mut answerer = Answerer::new(shown, given, &ask);
                let r = sess.userauth_keyboard_interactive(user, &mut answerer);
                spent |= given.is_some() && answerer.password.is_none();
                r.map_err(Into::into)
            }
        };
        check(cancel)?;
        if sess.authenticated() {
            return Ok(());
        }
        if let Err(e) = step {
            first_error.get_or_insert(e);
        }
        match offered(sess, user).with_context(refused)? {
            Some(now) => methods = now,
            None => return Ok(()),
        }
    }
    match first_error {
        Some(e) => Err(e).context(refused()),
        None => bail!(refused()),
    }
}

/// Asks the user one question whose answer is not shown as it is typed.
fn ask_secret(shown: &str, text: String, cancel: &AtomicBool) -> Option<String> {
    let challenge = Challenge {
        server: shown.to_string(),
        name: String::new(),
        instructions: String::new(),
        prompts: vec![Prompt { text, echo: false }],
    };
    ask(&challenge, cancel)?.into_iter().next()
}

/// Whether the private key file at `path` needs a passphrase: an OpenSSH key with a cipher, or
/// an encrypted PEM key. Unreadable files are left for signing in to report.
fn key_encrypted(path: &Path) -> bool {
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|mut f| {
        // Room for all of it at once: growing the buffer would leave copies of the key behind.
        text.reserve(f.metadata()?.len() as usize + 1);
        f.read_to_string(&mut text)
    });
    let encrypted = read.is_ok()
        && (text.contains("ENCRYPTED") || {
            // Only the start, which names the cipher: the key itself is never decoded here.
            let head: String = text
                .split("-----BEGIN OPENSSH PRIVATE KEY-----")
                .nth(1)
                .map(|rest| {
                    rest.chars()
                        .filter(|c| !c.is_whitespace())
                        .take_while(|&c| c != '-')
                        .take(64)
                        .collect()
                })
                .unwrap_or_default();
            let cipher = decode_base64(&head).and_then(|b| {
                let b = b.strip_prefix(b"openssh-key-v1\0")?;
                let len = u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as usize;
                b.get(4..4 + len).map(<[u8]>::to_vec)
            });
            cipher.is_some_and(|c| c != b"none")
        });
    wipe(&mut text);
    encrypted
}

/// Signs `user` in with the private key file at `path`, asking for its passphrase when none
/// is given and it needs one. As `ssh` does, the passphrase is asked for only once the server
/// said it takes the key, when its public key is at hand (`path.pub`) to offer first.
fn key_file(
    sess: &ssh2::Session,
    user: &str,
    path: &Path,
    passphrase: &str,
    shown: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut asked = None;
    if passphrase.is_empty() && key_encrypted(path) {
        let mut public = path.as_os_str().to_owned();
        public.push(".pub");
        let public = PathBuf::from(public);
        if public.is_file() {
            // libssh2 offers the public key and signs, reading the private key, only if the
            // server takes it: a refusal is told apart from the key not opening without its
            // passphrase.
            match sess.userauth_pubkey_file(user, Some(&public), path, Some("")) {
                Ok(()) => return Ok(()),
                Err(e) if e.code() == ssh2::ErrorCode::Session(REFUSED) => return Err(e.into()),
                Err(_) => check(cancel)?,
            }
        }
        let prompt = trf(
            "label.ssh_passphrase_for",
            &[("file", path.display().to_string())],
        );
        asked = Some(ask_secret(shown, prompt, cancel).with_context(|| tr("msg.ssh_declined"))?);
    }
    let passphrase = asked
        .as_deref()
        .or((!passphrase.is_empty()).then_some(passphrase));
    let signed = sess.userauth_pubkey_file(user, None, path, passphrase);
    if let Some(mut p) = asked {
        wipe(&mut p);
    }
    Ok(signed?)
}

/// libssh2's `LIBSSH2_ERROR_AUTHENTICATION_FAILED`: what the server said of a key offered.
const REFUSED: std::os::raw::c_int = -18;

/// The key files tried when the configuration names none, as `ssh` tries them.
fn default_identities() -> Vec<PathBuf> {
    if cfg!(test) {
        return Vec::new();
    }
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .collect()
}

/// The public key, in its wire form, of the private key file `path`: read from `path.pub`.
fn public_key_of(path: &Path) -> Option<Vec<u8>> {
    let mut public = path.as_os_str().to_owned();
    public.push(".pub");
    let text = std::fs::read_to_string(public).ok()?;
    decode_base64(text.split_whitespace().nth(1)?)
}

/// Signs `target`'s user in with any key it may use, as `ssh` does: the SSH agent's (only those
/// of the configuration's files with `IdentitiesOnly`), then the configuration's key files, or
/// else the usual `~/.ssh/id_*`. A key file whose public key the agent offered already is not
/// tried again (nor its passphrase asked for), and none is once the server takes no more keys
/// (a key it accepted, it now wants a password, say).
fn any_key(
    sess: &ssh2::Session,
    target: &super::ssh_config::Resolved,
    shown: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let user = target.user.as_str();
    let files = match target.identity_files.is_empty() {
        true => default_identities(),
        false => target.identity_files.clone(),
    };
    let mut offered_keys: Vec<Vec<u8>> = Vec::new();
    if let Ok(mut agent) = sess.agent() {
        if agent.connect().is_ok() && agent.list_identities().is_ok() {
            let only: Option<Vec<Vec<u8>>> = target
                .identities_only
                .then(|| files.iter().filter_map(|f| public_key_of(f)).collect());
            for id in agent.identities().unwrap_or_default() {
                if only
                    .as_ref()
                    .is_some_and(|o| !o.iter().any(|k| k == id.blob()))
                {
                    continue;
                }
                check(cancel)?;
                offered_keys.push(id.blob().to_vec());
                if agent.userauth(user, &id).is_ok() && sess.authenticated() {
                    break;
                }
            }
            let _ = agent.disconnect();
        }
    }
    // Asked once only: each time counts against the tries the server allows.
    if !offered_keys.is_empty() && !sess.authenticated() && !takes_keys(sess, user) {
        bail!(tr("msg.ssh_no_key_accepted"));
    }
    for file in files.iter().filter(|f| f.is_file()) {
        if sess.authenticated() {
            break;
        }
        if public_key_of(file).is_some_and(|k| offered_keys.contains(&k)) {
            continue;
        }
        check(cancel)?;
        let _ = key_file(sess, user, file, "", shown, cancel);
    }
    match sess.authenticated() {
        true => Ok(()),
        false => bail!(tr("msg.ssh_no_key_accepted")),
    }
}

/// Whether the server still takes a key to sign `user` in.
fn takes_keys(sess: &ssh2::Session, user: &str) -> bool {
    matches!(offered(sess, user), Ok(Some(m)) if m.split(',').any(|o| o == "publickey"))
}

/// The most rounds of questions answered while signing in once, so a server that keeps asking
/// cannot keep the connection waiting forever.
const MAX_ROUNDS: usize = 8;

/// Answers a server's keyboard-interactive questions: a lone password question with the
/// password given, once; anything else by asking the user.
struct Answerer<'a> {
    server: &'a str,
    password: Option<&'a str>,
    ask: &'a dyn Fn(&Challenge) -> Option<Vec<String>>,
    declined: bool,
    rounds: usize,
}

impl<'a> Answerer<'a> {
    fn new(
        server: &'a str,
        password: Option<&'a str>,
        ask: &'a dyn Fn(&Challenge) -> Option<Vec<String>>,
    ) -> Self {
        Self {
            server,
            password,
            ask,
            declined: false,
            rounds: 0,
        }
    }
}

impl ssh2::KeyboardInteractivePrompt for Answerer<'_> {
    fn prompt<'p>(
        &mut self,
        username: &str,
        instructions: &str,
        prompts: &[ssh2::Prompt<'p>],
    ) -> Vec<String> {
        self.rounds += 1;
        let blank = || vec![String::new(); prompts.len()];
        if prompts.is_empty() || self.declined || self.rounds > MAX_ROUNDS {
            return blank();
        }
        if let [only] = prompts {
            if !only.echo && only.text.to_lowercase().contains("password") {
                if let Some(password) = self.password.take() {
                    return vec![password.to_string()];
                }
            }
        }
        let challenge = Challenge {
            server: self.server.to_string(),
            name: username.to_string(),
            instructions: instructions.to_string(),
            prompts: prompts
                .iter()
                .map(|p| Prompt {
                    text: p.text.to_string(),
                    echo: p.echo,
                })
                .collect(),
        };
        match (self.ask)(&challenge) {
            Some(answers) if answers.len() == prompts.len() => answers,
            _ => {
                self.declined = true;
                blank()
            }
        }
    }
}

/// Cooperative cancellation, checked between the steps of connecting and the chunks of a
/// transfer: a network read or write itself cannot be interrupted, but a large file then stops
/// within one chunk of it.
pub(crate) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!(tr("msg.sftp_cancelled"));
    }
    Ok(())
}

pub(crate) const CHUNK: usize = 64 * 1024;

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
pub(crate) trait RemoteFs {
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
    /// `remote`, a relative path, as the absolute one it is relative to the user's home folder.
    fn real_path(&self, remote: &str) -> Result<String> {
        bail!(trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))
    }
    /// Whether `remote` (following a symbolic link) is a folder, a regular file, or neither.
    fn kind(&self, remote: &str) -> Result<Option<EntryKind>> {
        bail!(trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))
    }
    /// The entries of `remote`, a folder, for [`Browser`]: no more than `limit + 1` of them, as
    /// [`RemoteFs::list_dir`]. A symbolic link is listed as what it points to, or as
    /// [`EntryKind::Link`] once too many were looked up; one pointing nowhere is left out.
    fn list_entries(
        &self,
        remote: &str,
        _limit: usize,
        _cancel: &AtomicBool,
    ) -> Result<Vec<RemoteEntry>> {
        bail!(trf("msg.sftp_cannot_list", &[("path", remote.to_string())]))
    }
    /// How the file `remote` is now (following a symbolic link); `None` when nothing is there.
    fn stamp(&self, remote: &str) -> Result<Option<RemoteStamp>> {
        bail!(trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))
    }
    /// [`RemoteFs::list_dir`], with how each file is on the server when the listing tells.
    /// Links, other special entries and names not valid in UTF-8 (with an empty name) are
    /// listed too, as neither files nor folders, so a fetch counts them towards its limit.
    fn list_dir_stamped(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Listed>> {
        Ok(self
            .list_dir(remote, limit, cancel)?
            .into_iter()
            .map(|(name, is_dir, is_file)| Listed {
                name,
                is_dir,
                is_file,
                stamp: None,
            })
            .collect())
    }
}

/// An entry of a folder as [`RemoteFs::list_dir_stamped`] lists it.
pub(crate) struct Listed {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub stamp: Option<RemoteStamp>,
}

/// What saving a file or a folder needs from the server, besides reading it.
pub(crate) trait RemoteWrite {
    /// Writes `local`'s bytes to the file `remote`, which must be as `expected` (`None`: not
    /// there at all), and returns how it is on the server now. With `in_place`, an existing
    /// file is written over in place even where it could be replaced through a staged copy
    /// (see [`super::remote_stage`]).
    fn put(
        &self,
        local: &Path,
        remote: &str,
        expected: Option<RemoteStamp>,
        in_place: bool,
        cancel: &AtomicBool,
    ) -> Result<RemoteStamp>;
    fn make_dir(&self, remote: &str) -> Result<()>;
    fn remove_file(&self, remote: &str) -> Result<()>;
    /// Removes the folder `remote`, which must be empty.
    fn remove_dir(&self, remote: &str) -> Result<()>;
}

/// A connection to a server, over SFTP or FTP.
pub(crate) trait Remote: RemoteFs + RemoteWrite + Send {}
impl<T: RemoteFs + RemoteWrite + Send> Remote for T {}

/// Connects and signs in as `conn` says, over its protocol. Setting `cancel` stops it between
/// steps. Fails with [`UnknownHost`] for a server not trusted yet.
pub(crate) fn connect(conn: &RemoteConnection, cancel: &AtomicBool) -> Result<Box<dyn Remote>> {
    #[cfg(test)]
    if let Some(fake) = fake::connect() {
        return fake;
    }
    if conn.protocol.is_ftp() {
        Ok(Box::new(super::ftp::Client::connect(conn, cancel)?))
    } else {
        Ok(Box::new(session(conn, cancel)?))
    }
}

/// The entries of `remote` with their names and attributes, no more than `limit + 1` of them,
/// without `.` and `..`; a name not valid in UTF-8 is `None`.
fn read_dir(
    sftp: &ssh2::Sftp,
    remote: &str,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<(Option<String>, ssh2::FileStat)>> {
    let cannot = || trf("msg.sftp_cannot_list", &[("path", remote.to_string())]);
    let mut dir = sftp.opendir(Path::new(remote)).with_context(cannot)?;
    let mut out = Vec::new();
    loop {
        check(cancel)?;
        let (name, stat) = match dir.readdir() {
            Ok(entry) => entry,
            // libssh2's "no more files"
            Err(e) if matches!(e.code(), ssh2::ErrorCode::Session(-16)) => break,
            Err(e) => return Err(e).with_context(cannot),
        };
        let name = name.to_str();
        if name == Some(".") || name == Some("..") {
            continue;
        }
        out.push((name.map(str::to_string), stat));
        if out.len() > limit {
            break;
        }
    }
    Ok(out)
}

/// The most symbolic links one listing looks up to show as the folder or file they point to:
/// each takes a round trip to the server, so a folder of thousands of links would otherwise
/// take minutes to list over a slow connection.
const MAX_LINK_LOOKUPS: usize = 256;

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
        Ok(read_dir(self, remote, limit, cancel)?
            .into_iter()
            .filter_map(|(name, stat)| Some((name?, stat.is_dir(), stat.is_file())))
            .collect())
    }

    fn real_path(&self, remote: &str) -> Result<String> {
        let path = self
            .realpath(Path::new(remote))
            .with_context(|| trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))?;
        path.to_str()
            .map(str::to_string)
            .with_context(|| trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))
    }

    fn kind(&self, remote: &str) -> Result<Option<EntryKind>> {
        let stat = self
            .stat(Path::new(remote))
            .with_context(|| trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))?;
        Ok(EntryKind::of(&stat))
    }

    fn stamp(&self, remote: &str) -> Result<Option<RemoteStamp>> {
        match self.stat(Path::new(remote)) {
            Ok(stat) => Ok(Some(RemoteStamp::of(&stat))),
            Err(e) => {
                let e = std::io::Error::from(e);
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(anyhow::Error::from(e)
                        .context(trf("msg.sftp_cannot_read", &[("path", remote.to_string())])))
                }
            }
        }
    }

    fn list_dir_stamped(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Listed>> {
        Ok(read_dir(self, remote, limit, cancel)?
            .into_iter()
            .map(|(name, stat)| {
                let usable = name.is_some();
                Listed {
                    name: name.unwrap_or_default(),
                    is_dir: usable && stat.is_dir(),
                    is_file: usable && stat.is_file(),
                    stamp: stat.is_file().then(|| RemoteStamp::of(&stat)),
                }
            })
            .collect())
    }

    fn list_entries(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<RemoteEntry>> {
        let mut lookups = 0;
        let mut out = Vec::new();
        for (name, mut stat) in read_dir(self, remote, limit, cancel)? {
            let Some(name) = name else {
                continue;
            };
            let kind = if stat.file_type().is_symlink() {
                if lookups == MAX_LINK_LOOKUPS {
                    Some(EntryKind::Link)
                } else {
                    lookups += 1;
                    check(cancel)?;
                    match self.stat(Path::new(&join(remote, &name))) {
                        Ok(target) => {
                            stat = target;
                            EntryKind::of(&stat)
                        }
                        Err(_) => None,
                    }
                }
            } else {
                EntryKind::of(&stat)
            };
            if let Some(kind) = kind {
                out.push(RemoteEntry {
                    name,
                    kind,
                    size: stat.size.unwrap_or(0),
                });
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
            "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
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
#[cfg(test)]
fn download_dir(
    fs: &dyn RemoteFs,
    remote: &str,
    local: &Path,
    depth: usize,
    cancel: &AtomicBool,
    budget: &mut Budget,
) -> Result<()> {
    let mut manifest = Manifest::default();
    let mut at = Walk {
        rel: String::new(),
        depth,
        cancel,
        budget,
        manifest: &mut manifest,
    };
    download_tree(fs, remote, local, &mut at)
}

/// Where [`download_tree`] is in a folder fetch: the path relative to the folder fetched, how
/// many folders deep that is, and what the fetch has read so far.
struct Walk<'a> {
    rel: String,
    depth: usize,
    cancel: &'a AtomicBool,
    budget: &'a mut Budget,
    manifest: &'a mut Manifest,
}

/// [`download_dir`], recording each folder and file in `at.manifest`.
fn download_tree(fs: &dyn RemoteFs, remote: &str, local: &Path, at: &mut Walk) -> Result<()> {
    check(at.cancel)?;
    if at.depth > MAX_REMOTE_DEPTH {
        bail!(trf(
            "msg.sftp_too_deep",
            &[
                ("path", remote.to_string()),
                ("count", MAX_REMOTE_DEPTH.to_string())
            ]
        ));
    }
    at.budget.take_entry()?;
    std::fs::create_dir_all(local)?;
    let entries = fs.list_dir_stamped(remote, at.budget.entries_left(), at.cancel)?;
    if entries.len() > at.budget.entries_left() {
        return Err(at.budget.too_many());
    }
    for e in entries {
        // Left out: a name this machine cannot have, a link or other special entry, and a name
        // this file system takes for one already written here (`a.txt` after `A.txt` where case
        // does not count), whose download would overwrite the other's. Each still counts, so a
        // listing of nothing else cannot go on without end.
        let child_local = local.join(&e.name);
        if !local_name_ok(&e.name)
            || !(e.is_dir || e.is_file)
            || std::fs::symlink_metadata(&child_local).is_ok()
        {
            at.budget.take_entry()?;
            continue;
        }
        let child_remote = join(remote, &e.name);
        let rel = if at.rel.is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", at.rel, e.name)
        };
        if e.is_dir {
            at.manifest.dirs.insert(rel.clone());
            let parent = std::mem::replace(&mut at.rel, rel);
            at.depth += 1;
            let done = download_tree(fs, &child_remote, &child_local, at);
            at.depth -= 1;
            at.rel = parent;
            done?;
        } else {
            // Read before the file, so a change made during the download shows as one later.
            let stamp = match e.stamp {
                Some(stamp) => stamp,
                None => fs.stamp(&child_remote).ok().flatten().unwrap_or_default(),
            };
            download_file(fs, &child_remote, &child_local, at.cancel, at.budget)?;
            let hash = file_hash(&child_local).unwrap_or_default();
            at.manifest.files.insert(rel, (stamp, hash));
        }
    }
    Ok(())
}

/// `base` and `name` as one SFTP path: always `/`-separated, whatever this machine's own
/// separator is, since the server is not necessarily this kind of machine.
pub fn join(base: &str, name: &str) -> String {
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
    let remote = connect(conn, cancel)?;
    let temp = tempfile::Builder::new()
        .prefix("oxidemerge-sftp-")
        .tempdir()?;
    if let Ok(mut staged) = STAGED.lock() {
        staged.push(temp.path().to_path_buf());
        if let Ok(real) = std::fs::canonicalize(temp.path()) {
            staged.push(real);
        }
    }
    let local = temp.path().join(remote_file_name(remote_path));
    let staged = fetch_into(&*remote, remote_path, &local, cancel)?;
    Ok(Fetched {
        temp,
        local,
        is_dir: staged.is_dir,
        stamp: staged.stamp,
        manifest: staged.manifest,
    })
}

/// The temporary folders [`fetch`] staged into (and their real paths), for [`inside_fetch`].
static STAGED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether `path` is in a copy [`fetch`] staged, whose contents saving uploads.
pub fn inside_fetch(path: &Path) -> bool {
    let Ok(staged) = STAGED.lock() else {
        return false;
    };
    if staged.iter().any(|d| path.starts_with(d)) {
        return true;
    }
    let real = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.join(path.file_name().unwrap_or_default()));
    real.is_some_and(|real| staged.iter().any(|d| real.starts_with(d)))
}

/// What [`refetch`] wrote: whether it is a folder, for a file how it is on the server, and for
/// a folder what it holds.
pub struct Staged {
    pub is_dir: bool,
    pub stamp: Option<RemoteStamp>,
    pub manifest: Option<Manifest>,
}

/// Downloads the file or folder `remote_path` to `local`, which must not exist yet.
fn fetch_into(
    fs: &dyn RemoteFs,
    remote_path: &str,
    local: &Path,
    cancel: &AtomicBool,
) -> Result<Staged> {
    let trimmed = remote_path.trim_end_matches('/');
    let remote_path = if trimmed.is_empty() { "/" } else { trimmed };
    let cannot = || trf("msg.sftp_cannot_read", &[("path", remote_path.to_string())]);
    let mut budget = Budget::new();
    match fs.kind(remote_path).with_context(cannot)? {
        Some(EntryKind::Dir) => {
            let mut manifest = Manifest::default();
            let mut at = Walk {
                rel: String::new(),
                depth: 0,
                cancel,
                budget: &mut budget,
                manifest: &mut manifest,
            };
            download_tree(fs, remote_path, local, &mut at)?;
            Ok(Staged {
                is_dir: true,
                stamp: None,
                manifest: Some(manifest),
            })
        }
        Some(EntryKind::File) => {
            let stamp = fs.stamp(remote_path)?.with_context(cannot)?;
            if stamp.size > budget.max_bytes {
                return Err(budget.too_large());
            }
            download_file(fs, remote_path, local, cancel, &mut budget)?;
            Ok(Staged {
                is_dir: false,
                stamp: Some(stamp),
                manifest: None,
            })
        }
        _ => bail!(trf(
            "msg.sftp_not_regular",
            &[("path", remote_path.to_string())]
        )),
    }
}

/// Fetches `remote_path` again over the local copy at `local` an earlier [`fetch`] staged,
/// discarding what changed there. The new copy is downloaded beside it first, so a failed or
/// cancelled fetch leaves the old copy as it was.
pub fn refetch(
    conn: &RemoteConnection,
    remote_path: &str,
    local: &Path,
    cancel: &AtomicBool,
) -> Result<Staged> {
    let remote = connect(conn, cancel)?;
    let parent = local
        .parent()
        .with_context(|| trf("msg.cannot_write", &[("file", local.display().to_string())]))?;
    let fresh = tempfile::Builder::new()
        .prefix(".oxidemerge-reload-")
        .tempdir_in(parent)?;
    let new = fresh.path().join("new");
    let staged = fetch_into(&*remote, remote_path, &new, cancel)?;
    check(cancel)?;
    let old = fresh.path().join("old");
    if local.exists() {
        std::fs::rename(local, &old)?;
    }
    if let Err(e) = std::fs::rename(&new, local) {
        if old.exists() && std::fs::rename(&old, local).is_err() {
            // Neither copy is where the tab looks for it; the old one is kept, not deleted.
            let kept = fresh.keep().join("old");
            return Err(anyhow::Error::from(e).context(trf(
                "msg.remote_reload_kept",
                &[("path", kept.display().to_string())],
            )));
        }
        return Err(e.into());
    }
    Ok(staged)
}

/// Writes `local_path`'s bytes to `remote_path` on the server, for *Save* of a file opened
/// with [`fetch`], returning how the file is on the server now. A link on the server is kept,
/// and the file it names is written instead.
///
/// Refused when the file is not as `expected` (how it was fetched or last uploaded) any more:
/// someone else changed or deleted it on the server meanwhile, and writing over it would lose
/// their change. Existing files are updated through their open handle to preserve ownership
/// and ACLs, which cannot be copied portably through SFTP. Cancellation stops the upload
/// before it starts updating the file. A failed write may leave a partial update, and its
/// error then carries [`PartlyWritten`]. With `expected` `None` the file must not exist.
/// `sent`, the hash of the file as fetched or last uploaded, tells a change the stamp cannot.
pub fn upload(
    conn: &RemoteConnection,
    local_path: &Path,
    remote_path: &str,
    expected: Option<RemoteStamp>,
    sent: Option<u64>,
    cancel: &AtomicBool,
) -> Result<RemoteStamp> {
    let remote = connect(conn, cancel)?;
    if let (Some(stamp), Some(sent)) = (expected, sent) {
        if stamp.is_weak() && !still_as(&*remote, remote_path, stamp, sent, cancel)? {
            bail!(trf(
                "msg.sftp_changed_on_server",
                &[("path", remote_path.to_string())]
            ));
        }
    }
    remote.put(
        local_path,
        remote_path,
        expected,
        conn.options.in_place,
        cancel,
    )
}

impl RemoteWrite for ssh2::Sftp {
    fn put(
        &self,
        local: &Path,
        remote: &str,
        expected: Option<RemoteStamp>,
        in_place: bool,
        cancel: &AtomicBool,
    ) -> Result<RemoteStamp> {
        upload_on(self, local, remote, expected, in_place, cancel)
    }

    fn make_dir(&self, remote: &str) -> Result<()> {
        self.mkdir(Path::new(remote), 0o755)
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))
    }

    fn remove_file(&self, remote: &str) -> Result<()> {
        self.unlink(Path::new(remote))
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))
    }

    fn remove_dir(&self, remote: &str) -> Result<()> {
        self.rmdir(Path::new(remote))
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))
    }
}

/// [`upload`] over the open session `sftp`: through a staged copy (see
/// [`super::remote_stage`]) unless `in_place`, or the file cannot be replaced keeping its
/// owner, group and permissions; otherwise written over in place.
fn upload_on(
    sftp: &ssh2::Sftp,
    local_path: &Path,
    remote_path: &str,
    expected: Option<RemoteStamp>,
    in_place: bool,
    cancel: &AtomicBool,
) -> Result<RemoteStamp> {
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
    let real = match sftp.lstat(Path::new(remote_path)) {
        Ok(s) if s.file_type().is_symlink() => sftp
            .realpath(Path::new(remote_path))
            .with_context(cannot_write)?
            .to_string_lossy()
            .into_owned(),
        _ => remote_path.to_string(),
    };
    let target = Path::new(&real);
    let now = match sftp.stat(target) {
        Ok(s) => Some(s),
        Err(e) => {
            let e = std::io::Error::from(e);
            if e.kind() == std::io::ErrorKind::NotFound {
                None
            } else {
                return Err(anyhow::Error::from(e).context(cannot_write()));
            }
        }
    };
    let stamp_now = now.as_ref().map(RemoteStamp::of);
    if expected.is_some() && stamp_now != expected {
        return Err(changed());
    }
    if expected.is_none() && stamp_now.is_some() {
        bail!(trf(
            "msg.remote_created_meanwhile",
            &[("path", remote_path.to_string())]
        ));
    }
    let Some(stamp_now) = stamp_now else {
        // A new file has no access controls to preserve.
        return super::remote_stage::create(sftp, local_path, &real, cancel)
            .with_context(cannot_write);
    };
    if !in_place {
        if let Some(stamp) =
            super::remote_stage::replace(sftp, local_path, &real, stamp_now, cancel)
                .with_context(cannot_write)?
        {
            return Ok(stamp);
        }
    }
    let mut from = std::fs::File::open(local_path).with_context(|| {
        trf(
            "msg.sftp_cannot_read",
            &[("path", local_path.display().to_string())],
        )
    })?;
    let mut to = sftp
        .open_mode(target, ssh2::OpenFlags::WRITE, 0, ssh2::OpenType::File)
        .with_context(cannot_write)?;
    // Validate the handle before writing so the check and the write use the same file.
    let stat = to.stat().with_context(cannot_write)?;
    if !stat.is_file() || RemoteStamp::of(&stat) != stamp_now {
        return Err(changed());
    }
    check(cancel)?;
    // Not cancelled once writing has started: stopping then would leave the file partly
    // written on the server.
    let written = (|| -> Result<()> {
        let size = std::io::copy(&mut from, &mut to)?;
        if size < stamp_now.size {
            to.setstat(ssh2::FileStat {
                size: Some(size),
                uid: None,
                gid: None,
                perm: None,
                atime: None,
                mtime: None,
            })?;
        }
        let _ = to.fsync();
        to.close()?;
        Ok(())
    })();
    let after = sftp.stat(target).map(|s| RemoteStamp::of(&s));
    match (written, after) {
        (Ok(()), Ok(stamp)) => Ok(stamp),
        (Ok(()), Err(e)) => Err(anyhow::Error::from(e).context(cannot_write())),
        (Err(e), after) => Err(e.context(cannot_write()).context(PartlyWritten {
            path: remote_path.to_string(),
            stamp: after.ok(),
        })),
    }
}

impl super::remote_stage::Stage for ssh2::Sftp {
    fn meta(&self, path: &str) -> Result<Option<super::remote_stage::Meta>> {
        match self.stat(Path::new(path)) {
            Ok(s) => Ok(Some(super::remote_stage::Meta {
                stamp: RemoteStamp::of(&s),
                is_file: s.is_file(),
                owner: s.uid.zip(s.gid),
                perm: s.perm.map(|p| p & 0o7777),
            })),
            Err(e) => {
                let e = std::io::Error::from(e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Ok(None),
                    _ => Err(e.into()),
                }
            }
        }
    }

    fn create(&self, path: &str, perm: u32) -> Result<()> {
        let f = self.open_mode(
            Path::new(path),
            ssh2::OpenFlags::WRITE | ssh2::OpenFlags::EXCLUSIVE,
            perm as i32,
            ssh2::OpenType::File,
        )?;
        drop(f);
        Ok(())
    }

    fn write(&self, path: &str, from: &mut dyn Read, cancel: &AtomicBool) -> Result<u64> {
        let mut to = self.open_mode(
            Path::new(path),
            ssh2::OpenFlags::WRITE,
            0,
            ssh2::OpenType::File,
        )?;
        let mut buf = vec![0u8; CHUNK];
        let mut written = 0;
        loop {
            check(cancel)?;
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n])?;
            written += n as u64;
        }
        // Not every server can flush a file to disk (`fsync@openssh.com`).
        let _ = to.fsync();
        to.close()?;
        Ok(written)
    }

    fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        Ok(self.setstat(
            Path::new(path),
            ssh2::FileStat {
                size: None,
                uid: Some(uid),
                gid: Some(gid),
                perm: None,
                atime: None,
                mtime: None,
            },
        )?)
    }

    fn set_perm(&self, path: &str, perm: u32) -> Result<()> {
        Ok(self.setstat(
            Path::new(path),
            ssh2::FileStat {
                size: None,
                uid: None,
                gid: None,
                perm: Some(perm),
                atime: None,
                mtime: None,
            },
        )?)
    }

    fn read(&self, path: &str) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.open(Path::new(path))?))
    }

    fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
        // Version 3 of the protocol (OpenSSH's) has no flags: its rename refuses to replace.
        let flags = match replace {
            true => {
                ssh2::RenameFlags::OVERWRITE | ssh2::RenameFlags::ATOMIC | ssh2::RenameFlags::NATIVE
            }
            false => ssh2::RenameFlags::empty(),
        };
        Ok(self.rename(Path::new(from), Path::new(to), Some(flags))?)
    }

    fn remove(&self, path: &str) -> Result<()> {
        Ok(self.unlink(Path::new(path))?)
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
        "{}://{}@{}{path}",
        conn.protocol.scheme(),
        conn.user,
        server(&conn.host, conn.port)
    )
}

/// What a folder entry on the server is, as [`Browser`] shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A symbolic link not looked up (see [`MAX_LINK_LOOKUPS`]): opening it shows what it is.
    Link,
}

impl EntryKind {
    /// A folder or a regular file; `None` for anything else, such as a device or a socket.
    fn of(stat: &ssh2::FileStat) -> Option<Self> {
        if stat.is_dir() {
            Some(Self::Dir)
        } else if stat.is_file() {
            Some(Self::File)
        } else {
            None
        }
    }
}

/// One entry of a [`Listing`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

/// A folder on the server, listed by [`Browser`].
#[derive(Clone, Debug)]
pub struct Listing {
    /// The folder's absolute path.
    pub path: String,
    /// Its folders, then its files, each by name, with hidden ones (starting with `.`) last.
    pub entries: Vec<RemoteEntry>,
    /// The name of the file asked for, when the path asked for was a file in this folder.
    pub file: Option<String>,
    /// Whether the folder has more than [`MAX_BROWSE_ENTRIES`] entries, the rest left out.
    pub truncated: bool,
}

/// The most entries of one folder a [`Listing`] holds.
pub const MAX_BROWSE_ENTRIES: usize = 10_000;

/// The folder `path` is in, or `None` for a root (`/`) or a single relative name.
pub fn parent_path(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    match path.rfind('/')? {
        0 if path.len() > 1 => Some("/".to_string()),
        0 => None,
        i => Some(path[..i].to_string()),
    }
}

/// An absolute path with its `.` and `..` segments and repeated or trailing `/` taken out.
pub(crate) fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Lists the folder `path` names (the user's home folder when it is empty), or the folder of
/// the file it names. A path that does not exist lists the nearest folder above it that does,
/// so a path half typed or mistyped still shows somewhere to go from.
fn browse_in(fs: &dyn RemoteFs, path: &str, cancel: &AtomicBool) -> Result<Listing> {
    let asked = path.trim();
    let mut at = if asked.is_empty() { "." } else { asked }.to_string();
    let (found, kind) = loop {
        check(cancel)?;
        let found = if at.starts_with('/') {
            Ok(normalize(&at))
        } else {
            fs.real_path(&at)
        };
        match found.and_then(|p| Ok((fs.kind(&p)?, p))) {
            Ok((kind, p)) => break (p, kind),
            Err(e) => match parent_path(&at) {
                Some(up) => at = up,
                None if at != "." => at = ".".to_string(),
                None => return Err(e),
            },
        }
    };
    let (dir, file) = match kind {
        Some(EntryKind::Dir) => (found, None),
        _ => {
            let name = found.rsplit('/').next().unwrap_or_default().to_string();
            let file = (kind == Some(EntryKind::File) && at == asked).then_some(name);
            (parent_path(&found).unwrap_or_else(|| "/".to_string()), file)
        }
    };
    let mut entries = fs.list_entries(&dir, MAX_BROWSE_ENTRIES, cancel)?;
    let truncated = entries.len() > MAX_BROWSE_ENTRIES;
    entries.truncate(MAX_BROWSE_ENTRIES);
    entries.sort_by_cached_key(|e| {
        (
            e.kind != EntryKind::Dir,
            e.name.starts_with('.'),
            e.name.to_lowercase(),
        )
    });
    Ok(Listing {
        path: dir,
        entries,
        file,
        truncated,
    })
}

/// A connection kept open on another thread for listing folders on the server, one after
/// another, as the user goes through them: connecting and signing in again for each folder
/// would take seconds every time. The connection is made on the first [`Browser::list`], and
/// again on the next one after it failed; it is closed when the browser is dropped.
pub struct Browser {
    requests: Option<mpsc::Sender<(u64, String)>>,
    #[cfg(test)]
    done: mpsc::Sender<(u64, Result<Listing>)>,
    results: mpsc::Receiver<(u64, Result<Listing>)>,
    /// How many folders were asked for: each listing comes back with its number, and only
    /// the last one asked for is shown.
    asked: std::cell::Cell<u64>,
    cancel: std::sync::Arc<AtomicBool>,
}

impl Browser {
    pub fn start(conn: RemoteConnection) -> Self {
        let (done, results) = mpsc::channel();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        if fake::installed() {
            return Self {
                requests: None,
                done,
                results,
                asked: Default::default(),
                cancel,
            };
        }
        let (requests, asked) = mpsc::channel::<(u64, String)>();
        let (tx, stop) = (done.clone(), cancel.clone());
        std::thread::spawn(move || {
            let mut open: Option<Box<dyn Remote>> = None;
            while let Ok((mut n, mut path)) = asked.recv() {
                // Only the folder asked for last matters once the user has moved on.
                while let Ok(newer) = asked.try_recv() {
                    (n, path) = newer;
                }
                let listed = match &open {
                    Some(r) => browse_in(&**r, &path, &stop),
                    None => connect(&conn, &stop).and_then(|r| {
                        let listed = browse_in(&*r, &path, &stop);
                        open = Some(r);
                        listed
                    }),
                };
                // The connection may be what failed (a server closes an idle one), so the next
                // folder asked for connects again.
                if listed.is_err() {
                    open = None;
                }
                if tx.send((n, listed)).is_err() {
                    break;
                }
            }
        });
        Self {
            requests: Some(requests),
            #[cfg(test)]
            done,
            results,
            asked: Default::default(),
            cancel,
        }
    }

    /// Asks for the folder `path` names to be listed (see [`browse_in`]); [`Browser::poll`]
    /// returns it once it is. Fails with [`UnknownHost`] for a server not trusted yet.
    pub fn list(&self, path: &str) {
        let n = self.asked.get() + 1;
        self.asked.set(n);
        #[cfg(test)]
        if self.requests.is_none() {
            if let Some(remote) = fake::connect() {
                let listed = remote.and_then(|r| browse_in(&*r, path, &self.cancel));
                let _ = self.done.send((n, listed));
            }
            return;
        }
        if let Some(requests) = &self.requests {
            let _ = requests.send((n, path.to_string()));
        }
    }

    /// The listing of the folder asked for last, once it is done; those of folders asked for
    /// before it are dropped.
    pub fn poll(&self) -> Option<Result<Listing>> {
        while let Ok((n, listed)) = self.results.try_recv() {
            if n == self.asked.get() {
                return Some(listed);
            }
        }
        None
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
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
        /// The folder a relative path is in; the root when empty.
        home: String,
        /// Files and folders a save deleted.
        removed: Vec<String>,
        /// Paths every write to fails (see [`Server::write_fails`]).
        write_fails: Vec<String>,
        /// Paths the next write to fails half way (see [`Server::partly_fails`]).
        partly_fails: Vec<String>,
        /// Whether stamps have no time (see [`Server::weak_stamps`]).
        weak: bool,
    }

    impl Server {
        pub fn new() -> Self {
            Self::default()
        }

        /// The signed-in user's home folder, that a relative path (or none) is in.
        pub fn home(mut self, path: &str) -> Self {
            self.home = path.to_string();
            self
        }

        /// Writing the file `path` fails, as for one the user may not change on the server.
        pub fn write_fails(mut self, path: &str) -> Self {
            self.write_fails.push(path.to_string());
            self
        }

        /// The next write to the file `path` stops half way, as when the connection drops.
        pub fn partly_fails(mut self, path: &str) -> Self {
            self.partly_fails.push(path.to_string());
            self
        }

        /// Files have sizes but no modified times, as on an FTP server without `MDTM`.
        pub fn weak_stamps(mut self) -> Self {
            self.weak = true;
            self
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
            Some(RemoteStamp::new(
                bytes.len() as u64,
                match self.weak {
                    true => 0,
                    false => self.versions.get(path).copied().unwrap_or(0) + 1,
                },
            ))
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

        fn real_path(&self, remote: &str) -> Result<String> {
            let path = normalize(&format!("{}/{remote}", self.home));
            self.kind(&path)?;
            Ok(path)
        }

        fn kind(&self, remote: &str) -> Result<Option<EntryKind>> {
            if remote == "/" {
                return Ok(Some(EntryKind::Dir));
            }
            match self.entries.get(remote) {
                Some(Some(_)) => Ok(Some(EntryKind::File)),
                Some(None) => Ok(Some(EntryKind::Dir)),
                None => bail!("{remote}: no such file or folder"),
            }
        }

        fn list_entries(
            &self,
            remote: &str,
            limit: usize,
            cancel: &AtomicBool,
        ) -> Result<Vec<RemoteEntry>> {
            self.kind(remote)?;
            Ok(self
                .list_dir(remote, limit, cancel)?
                .into_iter()
                .map(|(name, is_dir, _)| {
                    let size = self
                        .stamp(&join(remote, &name))
                        .map_or(0, |stamp| stamp.size);
                    let kind = if is_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    RemoteEntry { name, kind, size }
                })
                .collect())
        }
    }

    /// A connection to the installed server, as [`super::connect`] makes to a real one; `None`
    /// when none is installed.
    pub(super) fn connect() -> Option<Result<Box<dyn Remote>>> {
        SERVER.with(|s| {
            let server = s.borrow();
            let server = server.as_ref()?;
            if let Some(message) = &server.fail {
                return Some(Err(anyhow::anyhow!("{message}")));
            }
            if server.unknown {
                return Some(Err(unknown_host().into()));
            }
            Some(Ok(Box::new(Conn) as Box<dyn Remote>))
        })
    }

    /// The installed server, through the connection interface; every call reads and writes
    /// it on this thread.
    struct Conn;

    fn with<T>(f: impl FnOnce(&mut Server) -> Result<T>) -> Result<T> {
        SERVER.with(|s| match s.borrow_mut().as_mut() {
            Some(server) => f(server),
            None => bail!("no fake server"),
        })
    }

    impl RemoteFs for Conn {
        fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>> {
            let mut bytes = Vec::new();
            with(|s| Ok(s.open_read(remote)?.read_to_end(&mut bytes)?))?;
            Ok(Box::new(std::io::Cursor::new(bytes)))
        }

        fn list_dir(
            &self,
            remote: &str,
            limit: usize,
            cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
            with(|s| s.list_dir(remote, limit, cancel))
        }

        fn real_path(&self, remote: &str) -> Result<String> {
            with(|s| s.real_path(remote))
        }

        fn kind(&self, remote: &str) -> Result<Option<EntryKind>> {
            with(|s| s.kind(remote))
        }

        fn list_entries(
            &self,
            remote: &str,
            limit: usize,
            cancel: &AtomicBool,
        ) -> Result<Vec<RemoteEntry>> {
            with(|s| s.list_entries(remote, limit, cancel))
        }

        fn stamp(&self, remote: &str) -> Result<Option<RemoteStamp>> {
            with(|s| {
                Ok(match s.entries.get(remote) {
                    Some(Some(_)) => s.stamp(remote),
                    Some(None) => Some(RemoteStamp::default()),
                    None if remote == "/" => Some(RemoteStamp::default()),
                    None => None,
                })
            })
        }
    }

    impl RemoteWrite for Conn {
        fn put(
            &self,
            local: &Path,
            remote: &str,
            expected: Option<RemoteStamp>,
            _in_place: bool,
            cancel: &AtomicBool,
        ) -> Result<RemoteStamp> {
            check(cancel)?;
            let bytes = std::fs::read(local)?;
            with(|s| {
                if s.write_fails.iter().any(|p| p == remote) {
                    bail!(trf(
                        "msg.sftp_cannot_write",
                        &[("path", remote.to_string())]
                    ));
                }
                if expected.is_some() && s.stamp(remote) != expected {
                    bail!(trf(
                        "msg.sftp_changed_on_server",
                        &[("path", remote.to_string())]
                    ));
                }
                if expected.is_none() && s.entries.contains_key(remote) {
                    bail!(trf(
                        "msg.remote_created_meanwhile",
                        &[("path", remote.to_string())]
                    ));
                }
                if let Some(i) = s.partly_fails.iter().position(|p| p == remote) {
                    s.partly_fails.remove(i);
                    let mut half = bytes.clone();
                    half.truncate(bytes.len() / 2);
                    s.entries.insert(remote.to_string(), Some(half));
                    *s.versions.entry(remote.to_string()).or_default() += 1;
                    let stamp = s.stamp(remote);
                    return Err(anyhow::anyhow!(trf(
                        "msg.sftp_cannot_write",
                        &[("path", remote.to_string())]
                    ))
                    .context(PartlyWritten {
                        path: remote.to_string(),
                        stamp,
                    }));
                }
                s.entries.insert(remote.to_string(), Some(bytes.clone()));
                *s.versions.entry(remote.to_string()).or_default() += 1;
                s.uploaded.push((remote.to_string(), bytes));
                Ok(s.stamp(remote).unwrap())
            })
        }

        fn make_dir(&self, remote: &str) -> Result<()> {
            with(|s| {
                if s.entries.contains_key(remote) {
                    bail!("{remote}: already exists");
                }
                s.entries.insert(remote.to_string(), None);
                Ok(())
            })
        }

        fn remove_file(&self, remote: &str) -> Result<()> {
            with(|s| match s.entries.get(remote) {
                Some(Some(_)) => {
                    s.entries.remove(remote);
                    s.removed.push(remote.to_string());
                    Ok(())
                }
                _ => bail!("{remote}: no such file"),
            })
        }

        fn remove_dir(&self, remote: &str) -> Result<()> {
            with(|s| {
                let prefix = format!("{remote}/");
                if s.entries.keys().any(|p| p.starts_with(&prefix)) {
                    bail!("{remote}: folder not empty");
                }
                match s.entries.get(remote) {
                    Some(None) => {
                        s.entries.remove(remote);
                        s.removed.push(remote.to_string());
                        Ok(())
                    }
                    _ => bail!("{remote}: no such folder"),
                }
            })
        }
    }

    fn unknown_host() -> UnknownHost {
        UnknownHost {
            server: "example.com:22".into(),
            id: "example.com:22".into(),
            key_type: "ssh-ed25519".into(),
            fingerprint: "SHA256:fake".into(),
            key: b"fake".to_vec(),
            path: PathBuf::new(),
            differs_from: None,
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

    /// Whether `path` was deleted on the server by a save, as against by [`remove`].
    pub fn was_removed(path: &str) -> bool {
        SERVER.with(|s| {
            s.borrow()
                .as_ref()
                .is_some_and(|s| s.removed.iter().any(|p| p == path))
        })
    }

    /// The file or folder at `path` on the installed server: its bytes, `Some(None)` for a
    /// folder, `None` when nothing is there.
    pub fn entry(path: &str) -> Option<Option<Vec<u8>>> {
        SERVER.with(|s| s.borrow().as_ref()?.entries.get(path).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manual smoke test of the real network path (not the fake server), against a local test
    // sshd: `/usr/sbin/sshd -f /tmp/sshtest/sshd_config -D &` with a matching config, key and
    // files. Ignored so it never runs as part of the suite or in CI, which has none of that.
    // Pin the fixture's public host key in `oxidemerge-test-config/known_hosts` under the
    // test process's temporary directory, using the app's hexadecimal key format.
    // `docker/servers.sh` sets all of this up for the tests below.
    #[test]
    #[ignore]
    fn smoke_test_against_a_local_sshd() {
        let never = AtomicBool::new(false);
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "127.0.0.1".into(),
            port: 2222,
            user: std::env::var("USER").unwrap(),
            auth: Auth::Key {
                path: "/tmp/sshtest/userkey".into(),
                passphrase: String::new(),
            },
            options: Default::default(),
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
        let uploaded = "/tmp/sshtest/uploaded.txt";
        upload(&conn, &up_local, uploaded, None, None, &never).unwrap();
        assert_eq!(
            std::fs::read_to_string(uploaded).unwrap(),
            "uploaded by smoke test\n"
        );

        // Saved through a staged copy: a new file takes the original's place, with its
        // permissions, so a hard link keeps the old contents, and nothing is left beside it.
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(uploaded, std::fs::Permissions::from_mode(0o640)).unwrap();
        let stamp =
            std::fs::metadata(uploaded).map(|m| RemoteStamp::new(m.len(), m.mtime() as u64));
        let stamp = stamp.unwrap();
        let alias = up_dir.path().join("alias.txt");
        std::fs::hard_link(uploaded, &alias).unwrap();
        let inode = std::fs::metadata(uploaded).unwrap().ino();
        std::fs::write(&up_local, "staged save\n").unwrap();
        let stamp = upload(&conn, &up_local, uploaded, Some(stamp), None, &never).unwrap();
        let meta = std::fs::metadata(uploaded).unwrap();
        assert_eq!(std::fs::read_to_string(uploaded).unwrap(), "staged save\n");
        assert_ne!(meta.ino(), inode);
        assert_eq!(meta.permissions().mode() & 0o7777, 0o640);
        assert_eq!(
            std::fs::read_to_string(&alias).unwrap(),
            "uploaded by smoke test\n"
        );
        let left: Vec<_> = std::fs::read_dir("/tmp/sshtest")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.contains("oxidemerge-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");

        // In place, as asked: a hard link sees the new contents. A shorter payload checks
        // that an in-place save also removes the old tail.
        std::fs::remove_file(&alias).unwrap();
        std::fs::hard_link(uploaded, &alias).unwrap();
        let in_place = RemoteConnection {
            options: ConnOptions {
                in_place: true,
                ..Default::default()
            },
            ..conn.clone()
        };
        std::fs::write(&up_local, "short\n").unwrap();
        let saved = upload(&in_place, &up_local, uploaded, Some(stamp), None, &never).unwrap();
        assert_eq!(saved.size, 6);
        assert_eq!(std::fs::read_to_string(&alias).unwrap(), "short\n");
        assert!(upload(&conn, &up_local, uploaded, Some(stamp), None, &never).is_err());

        // A folder no file can be created in, whose file can still be written: in place.
        let locked = "/tmp/sshtest/locked";
        let _ = std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(locked);
        std::fs::create_dir(locked).unwrap();
        let file = format!("{locked}/file.txt");
        std::fs::write(&file, "before\n").unwrap();
        std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let m = std::fs::metadata(&file).unwrap();
        let before = RemoteStamp::new(m.len(), m.mtime() as u64);
        std::fs::write(&up_local, "after\n").unwrap();
        upload(&conn, &up_local, &file, Some(before), None, &never).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "after\n");
        assert_eq!(std::fs::metadata(&file).unwrap().ino(), m.ino());
        std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Jump hosts, a proxy command and host certificates against the same local test sshd as
    // above, also allowing TCP forwarding and presenting `/tmp/sshtest/hostkey-cert.pub`, a
    // certificate for `certified.test` signed by `/tmp/sshtest/ca`. Its host key is pinned as
    // above, and only under `127.0.0.1`.
    #[test]
    #[ignore]
    fn jumps_and_host_certificates_against_a_local_sshd() {
        let never = AtomicBool::new(false);
        let user = std::env::var("USER").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ca = std::fs::read_to_string("/tmp/sshtest/ca.pub").unwrap();
        let authorities = dir.path().join("known_hosts");
        std::fs::write(
            &authorities,
            format!("@cert-authority certified.test,other.test {ca}"),
        )
        .unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            format!(
                "Host jumped piped certified wrong both\n  HostName 127.0.0.1\n  Port 2222\n\
                 Host jumped both\n  ProxyJump 127.0.0.1:2222\n\
                 Host piped\n  ProxyCommand nc %h %p\n\
                 Host certified both\n  HostKeyAlias certified.test\n\
                 Host wrong\n  HostKeyAlias other.test\n\
                 Host certified wrong both\n  UserKnownHostsFile {}\n\
                 Host *\n  User {user}\n  IdentityFile /tmp/sshtest/userkey\n",
                authorities.display()
            ),
        )
        .unwrap();
        *super::super::ssh_config::CONFIG_PATH.lock().unwrap() = Some(config);
        let conn = |host: &str| RemoteConnection {
            protocol: Protocol::Sftp,
            host: host.into(),
            port: 22,
            user: String::new(),
            auth: Auth::Agent,
            options: Default::default(),
        };
        let read = |host: &str| {
            fetch(&conn(host), "/tmp/sshtest/data_file.txt", &never)
                .map(|f| std::fs::read_to_string(f.local).unwrap())
        };
        for host in ["jumped", "piped", "certified", "both"] {
            assert_eq!(read(host).unwrap(), "hello remote\n", "{host}");
        }
        let err = format!("{:#}", read("wrong").unwrap_err());
        assert!(err.contains("certified.test, not other.test"), "{err}");

        // Megabytes each way through a jump host arrive whole.
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let local = dir.path().join("big.bin");
        std::fs::write(&local, &big).unwrap();
        let remote = "/tmp/sshtest/big.bin";
        upload(&conn("jumped"), &local, remote, None, None, &never).unwrap();
        assert!(std::fs::read(remote).unwrap() == big);
        let back = fetch(&conn("jumped"), remote, &never).unwrap();
        assert!(std::fs::read(back.local).unwrap() == big);
        *super::super::ssh_config::CONFIG_PATH.lock().unwrap() = None;
    }

    // Against the same local test sshd as above, with `/tmp/sshtest/enckey` (passphrase `pw`)
    // allowed to sign in and `/tmp/sshtest/strangerkey` not, both encrypted and with their
    // `.pub` files beside them.
    #[test]
    #[ignore]
    fn passphrases_are_asked_only_for_keys_a_local_sshd_takes() {
        let never = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            format!(
                "Host keyed\n  HostName 127.0.0.1\n  Port 2222\n  User {}\n  IdentitiesOnly yes\n\
                 IdentityFile /tmp/sshtest/strangerkey\n  IdentityFile /tmp/sshtest/enckey\n",
                std::env::var("USER").unwrap()
            ),
        )
        .unwrap();
        *super::super::ssh_config::CONFIG_PATH.lock().unwrap() = Some(config);
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        set_asker(Some(std::sync::Arc::new(
            move |c: &Challenge, _: &AtomicBool| {
                seen.lock().unwrap().push(c.prompts[0].text.clone());
                Some(vec!["pw".to_string()])
            },
        )));
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "keyed".into(),
            port: 22,
            user: String::new(),
            auth: Auth::Agent,
            options: Default::default(),
        };
        let fetched = fetch(&conn, "/tmp/sshtest/data_file.txt", &never);
        set_asker(None);
        *super::super::ssh_config::CONFIG_PATH.lock().unwrap() = None;
        assert_eq!(
            std::fs::read_to_string(fetched.unwrap().local).unwrap(),
            "hello remote\n"
        );
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].contains("enckey"), "{asked:?}");
    }

    // Keyboard-interactive sign-in against a second local test sshd, on port 2223, that offers
    // nothing else and asks for the password of the user running the test through PAM, as the
    // Docker test environments set up (`docker/servers.sh`). The password is in
    // `OXIDEMERGE_TEST_PASSWORD`, and the host key is pinned as above, under
    // `127.0.0.1:2223`.
    #[test]
    #[ignore]
    fn keyboard_interactive_against_a_local_sshd() {
        let password = std::env::var("OXIDEMERGE_TEST_PASSWORD").unwrap();
        let never = AtomicBool::new(false);
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let answer = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let (seen, given) = (asked.clone(), answer.clone());
        set_asker(Some(std::sync::Arc::new(
            move |c: &Challenge, _: &AtomicBool| {
                seen.lock().unwrap().push(c.prompts[0].text.clone());
                given.lock().unwrap().clone().map(|a| vec![a])
            },
        )));
        let conn = |auth: Auth| RemoteConnection {
            protocol: Protocol::Sftp,
            host: "127.0.0.1".into(),
            port: 2223,
            user: std::env::var("USER").unwrap(),
            auth,
            options: Default::default(),
        };
        let read = |auth: Auth| {
            fetch(&conn(auth), "/tmp/sshtest/data_file.txt", &never)
                .map(|f| std::fs::read_to_string(f.local).unwrap())
        };

        // The password given answers the server's password question by itself.
        let signed_in = read(Auth::Password(password.clone()));
        let asked_with_password = asked.lock().unwrap().clone();
        // A wrong one is sent once, and the question is not then put to the user.
        let refused = read(Auth::Password("wrong".into()));
        let asked_after_refusal = asked.lock().unwrap().clone();
        // Without a password, the user is asked, and the answer signs in.
        *answer.lock().unwrap() = Some(password);
        let answered = read(Auth::Agent);
        set_asker(None);

        assert_eq!(signed_in.unwrap(), "hello remote\n");
        assert!(asked_with_password.is_empty(), "{asked_with_password:?}");
        assert!(refused.is_err());
        assert!(asked_after_refusal.is_empty(), "{asked_after_refusal:?}");
        assert_eq!(answered.unwrap(), "hello remote\n");
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].to_lowercase().contains("password"), "{asked:?}");
    }

    // Browsing against the same local test sshd as above, with `/tmp/sshtest/data` also
    // holding a link `link_to_sub` to `sub`, `link_to_file` to `../data_file.txt`, and
    // `dangling` to nothing.
    #[test]
    #[ignore]
    fn browsing_against_a_local_sshd() {
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "127.0.0.1".into(),
            port: 2222,
            user: std::env::var("USER").unwrap(),
            auth: Auth::Key {
                path: "/tmp/sshtest/userkey".into(),
                passphrase: String::new(),
            },
            options: Default::default(),
        };
        let browser = Browser::start(conn);
        let list = |path: &str| {
            browser.list(path);
            let start = std::time::Instant::now();
            loop {
                if let Some(listed) = browser.poll() {
                    return listed.unwrap();
                }
                assert!(start.elapsed() < Duration::from_secs(30));
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let l = list("/tmp/sshtest/data");
        let shown: Vec<_> = l
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind, e.size))
            .collect();
        assert_eq!(
            shown,
            [
                ("link_to_sub", EntryKind::Dir, shown[0].2),
                ("sub", EntryKind::Dir, shown[1].2),
                ("a.txt", EntryKind::File, 2),
                ("link_to_file", EntryKind::File, 13),
            ]
        );
        let l = list("/tmp/sshtest/data/link_to_sub");
        assert_eq!(l.path, "/tmp/sshtest/data/link_to_sub");
        assert_eq!(l.entries[0].name, "b.txt");
        let l = list("/tmp/sshtest/data/link_to_file");
        assert_eq!(
            (l.path.as_str(), l.file.as_deref()),
            ("/tmp/sshtest/data", Some("link_to_file"))
        );
        assert_eq!(
            list("/tmp/sshtest/data/missing/x").path,
            "/tmp/sshtest/data"
        );
        assert_eq!(
            list("").path,
            dirs::home_dir().unwrap().to_str().unwrap().to_string()
        );
    }

    // Through real proxies on this machine (`net::testing::local_proxies`) to the local test
    // sshd above.
    #[test]
    #[ignore]
    fn sftp_through_local_socks5_and_http_proxies() {
        let never = AtomicBool::new(false);
        let password = std::env::var("OXIDEMERGE_TEST_PASSWORD").unwrap();
        for proxy in super::super::net::testing::local_proxies(&password) {
            let conn = RemoteConnection {
                protocol: Protocol::Sftp,
                host: "127.0.0.1".into(),
                port: 2222,
                user: std::env::var("USER").unwrap(),
                auth: Auth::Key {
                    path: "/tmp/sshtest/userkey".into(),
                    passphrase: String::new(),
                },
                options: ConnOptions {
                    proxy: Some(proxy.clone()),
                    ..Default::default()
                },
            };
            let f = fetch(&conn, "/tmp/sshtest/data_file.txt", &never);
            let f = f.unwrap_or_else(|e| panic!("{proxy:?}: {e:#}"));
            assert_eq!(std::fs::read_to_string(&f.local).unwrap(), "hello remote\n");
        }
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
            protocol: Protocol::Sftp,
            host: "example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: Auth::Password("s3cret".into()),
            options: Default::default(),
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
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
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
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        fake::install(fake::Server::new().file("/a.txt", "a"));
        assert!(fetch(&conn, "/missing.txt", &never).is_err());
        fake::install(fake::Server::new().failing("no route to host"));
        let err = fetch(&conn, "/a.txt", &never).err().unwrap();
        assert!(err.to_string().contains("no route to host"));
        fake::uninstall();
    }

    #[test]
    fn parent_paths_stop_at_the_root() {
        assert_eq!(parent_path("/srv/data/a.txt").as_deref(), Some("/srv/data"));
        assert_eq!(parent_path("/srv/data/").as_deref(), Some("/srv"));
        assert_eq!(parent_path("/srv").as_deref(), Some("/"));
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("notes.txt"), None);
        assert_eq!(parent_path("/C:/Users").as_deref(), Some("/C:"));
        assert_eq!(normalize("/srv//data/./sub/../a.txt/"), "/srv/data/a.txt");
        assert_eq!(normalize("/.."), "/");
    }

    #[test]
    fn browsing_lists_folders_first_and_finds_the_nearest_folder_of_a_path() {
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        fake::install(
            fake::Server::new()
                .home("/home/ann")
                .dir("/home")
                .dir("/home/ann")
                .file("/home/ann/notes.txt", "1234")
                .file("/home/ann/.profile", "")
                .dir("/home/ann/.cache")
                .dir("/home/ann/Work")
                .file("/home/ann/apple.txt", ""),
        );
        let browser = Browser::start(conn);
        let list = |path: &str| {
            browser.list(path);
            browser.poll().unwrap()
        };

        // No path: the home folder, folders before files, hidden ones last in each.
        let l = list("").unwrap();
        assert_eq!(l.path, "/home/ann");
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["Work", ".cache", "apple.txt", "notes.txt", ".profile"]
        );
        assert_eq!(l.entries[3].size, 4);
        assert_eq!(l.entries[0].kind, EntryKind::Dir);
        assert_eq!(l.file, None);
        assert!(!l.truncated);

        // A file lists its folder, naming the file; a relative path is in the home folder.
        let l = list("/home/ann/notes.txt").unwrap();
        assert_eq!(
            (l.path.as_str(), l.file.as_deref()),
            ("/home/ann", Some("notes.txt"))
        );
        assert_eq!(list("Work").unwrap().path, "/home/ann/Work");
        assert_eq!(list("/home/ann/Work/../").unwrap().path, "/home/ann");

        // A path that is not there lists the nearest folder above it that is.
        let l = list("/home/ann/Wo/x.txt").unwrap();
        assert_eq!((l.path.as_str(), l.file), ("/home/ann", None));
        assert_eq!(list("/nowhere").unwrap().path, "/");
        assert_eq!(list("nowhere").unwrap().path, "/home/ann");

        fake::install(fake::Server::new().failing("connection refused"));
        let err = list("/").err().unwrap();
        assert!(err.to_string().contains("connection refused"));
        fake::install(fake::Server::new().unknown_host());
        assert!(list("/").unwrap_err().downcast::<UnknownHost>().is_ok());
        fake::uninstall();
    }

    #[test]
    fn a_very_large_folder_lists_only_its_first_entries() {
        let mut server = fake::Server::new().dir("/big");
        for i in 0..=MAX_BROWSE_ENTRIES {
            server = server.file(&format!("/big/{i:05}.txt"), "");
        }
        let never = AtomicBool::new(false);
        let l = browse_in(&server, "/big", &never).unwrap();
        assert!(l.truncated);
        assert_eq!(l.entries.len(), MAX_BROWSE_ENTRIES);
        let cancelled = AtomicBool::new(true);
        assert!(browse_in(&server, "/big", &cancelled).is_err());
    }

    #[test]
    fn a_fetch_already_cancelled_stops_before_reading_anything() {
        let cancelled = AtomicBool::new(true);
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
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
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        let never = AtomicBool::new(false);
        // Expected not to be there, it is not written over.
        let e = upload(&conn, &local, "/a.txt", None, None, &never).unwrap_err();
        assert!(e.to_string().contains("/a.txt"), "{e}");
        assert_eq!(fake::upload_count("/a.txt"), 0);
        let fetched = fetch(&conn, "/a.txt", &never).unwrap();
        let first = upload(&conn, &local, "/a.txt", fetched.stamp, None, &never).unwrap();
        assert_eq!(fake::uploaded("/a.txt").unwrap(), b"new");
        assert_eq!(fake::upload_count("/a.txt"), 1);
        upload(&conn, &local, "/a.txt", Some(first), None, &never).unwrap();
        assert_eq!(fake::upload_count("/a.txt"), 2);
        fake::uninstall();
    }

    /// A file someone else changed on the server since it was fetched is not written over.
    #[test]
    fn uploading_over_a_file_changed_on_the_server_is_refused() {
        fake::install(fake::Server::new().file("/a.txt", "old"));
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        let never = AtomicBool::new(false);
        let fetched = fetch(&conn, "/a.txt", &never).unwrap();
        std::fs::write(&fetched.local, "mine").unwrap();
        fake::change("/a.txt", "theirs");
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, None, &never).unwrap_err();
        assert!(err.to_string().contains("/a.txt"), "{err}");
        fake::remove("/a.txt");
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, None, &never).unwrap_err();
        assert!(err.to_string().contains("/a.txt"), "{err}");
        assert_eq!(fake::upload_count("/a.txt"), 0);
        fake::uninstall();
    }

    /// Without modified times, a change on the server keeping the file's size is told by its
    /// contents, given how they were sent.
    #[test]
    fn a_change_keeping_the_size_is_seen_where_the_server_has_no_times() {
        fake::install(fake::Server::new().file("/a.txt", "old").weak_stamps());
        let conn = RemoteConnection {
            protocol: Protocol::Ftps,
            host: "h".into(),
            port: 21,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        let never = AtomicBool::new(false);
        let fetched = fetch(&conn, "/a.txt", &never).unwrap();
        assert!(fetched.stamp.unwrap().is_weak());
        let sent = file_hash(&fetched.local);
        std::fs::write(&fetched.local, "mine").unwrap();
        fake::change("/a.txt", "OLD");
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, sent, &never);
        assert!(err.unwrap_err().to_string().contains("/a.txt"));
        assert_eq!(fake::entry("/a.txt"), Some(Some(b"OLD".to_vec())));

        fake::change("/a.txt", "old");
        upload(&conn, &fetched.local, "/a.txt", fetched.stamp, sent, &never).unwrap();
        assert_eq!(fake::entry("/a.txt"), Some(Some(b"mine".to_vec())));
        fake::uninstall();
    }

    #[test]
    fn an_upload_stopped_half_way_tells_how_it_left_the_file_to_save_again_over() {
        fake::install(
            fake::Server::new()
                .file("/a.txt", "old")
                .partly_fails("/a.txt"),
        );
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
        };
        let never = AtomicBool::new(false);
        let fetched = fetch(&conn, "/a.txt", &never).unwrap();
        std::fs::write(&fetched.local, "mine!!").unwrap();
        let err = upload(&conn, &fetched.local, "/a.txt", fetched.stamp, None, &never);
        let err = err.unwrap_err();
        let partly = err.downcast_ref::<PartlyWritten>().unwrap();
        assert_eq!(partly.path, "/a.txt");
        assert_eq!(fake::entry("/a.txt"), Some(Some(b"min".to_vec())));
        // As fetched it is not there any more.
        assert!(upload(&conn, &fetched.local, "/a.txt", fetched.stamp, None, &never).is_err());
        upload(&conn, &fetched.local, "/a.txt", partly.stamp, None, &never).unwrap();
        assert_eq!(fake::entry("/a.txt"), Some(Some(b"mine!!".to_vec())));
        fake::uninstall();
    }

    #[test]
    fn a_server_not_trusted_yet_fails_with_its_fingerprint_until_trusted() {
        fake::install(fake::Server::new().file("/a.txt", "a").unknown_host());
        let conn = RemoteConnection {
            protocol: Protocol::Sftp,
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Password(String::new()),
            options: Default::default(),
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

    /// Folders of nothing but links, each listed in full.
    struct Links;

    impl RemoteFs for Links {
        fn open_read(&self, _remote: &str) -> Result<Box<dyn Read + '_>> {
            unreachable!("no files")
        }

        fn list_dir(
            &self,
            remote: &str,
            _limit: usize,
            _cancel: &AtomicBool,
        ) -> Result<Vec<(String, bool, bool)>> {
            let (prefix, is_dir) = if remote == "/links" {
                ("d", true)
            } else {
                ("link", false)
            };
            Ok((0..3)
                .map(|i| (format!("{prefix}{i}"), is_dir, false))
                .collect())
        }
    }

    #[test]
    fn entries_left_out_of_a_fetch_still_count_towards_its_limit() {
        let never = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        // The root and its three folders, then three links in each: 13 in all.
        let mut budget = Budget::with_limits(u64::MAX, 8);
        let local = dir.path().join("links");
        let err = download_dir(&Links, "/links", &local, 0, &never, &mut budget);
        assert_eq!(
            err.unwrap_err().to_string(),
            trf("msg.sftp_too_many", &[("count", "8".to_string())])
        );
        let mut budget = Budget::with_limits(u64::MAX, 13);
        let local = dir.path().join("all");
        download_dir(&Links, "/links", &local, 0, &never, &mut budget).unwrap();
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
    fn servers_trusted_at_the_same_time_are_all_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::thread::scope(|s| {
            for i in 0..8u8 {
                let path = &path;
                s.spawn(move || remember_host(path, &format!("host{i}.example:22"), &[i]).unwrap());
            }
        });
        let loaded = load_known_hosts(&path).unwrap();
        for i in 0..8u8 {
            assert_eq!(loaded.get(&format!("host{i}.example:22")), Some(&vec![i]));
        }
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

    #[test]
    fn older_host_ids_still_pin_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, "Example.COM:22 0102\n2001:DB8::1:2222 0304\n").unwrap();
        let known = load_known_hosts(&path).unwrap();
        assert_eq!(known["example.com:22"], [1, 2]);
        assert_eq!(known["[2001:db8::1]:2222"], [3, 4]);

        // Conflicting pins for equivalent names must never silently discard a pin.
        std::fs::write(&path, "Example.COM:22 0102\nexample.com:22 0304\n").unwrap();
        assert!(load_known_hosts(&path).is_err());
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
    /// another type is only a warning, and a line for a key type it checks that can't be read
    /// fails closed.
    #[test]
    fn the_users_known_hosts_file_is_checked_as_ssh_checks_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let ours = blob("ssh-ed25519", &[&[1; 32]]);
        let theirs = blob("ssh-ed25519", &[&[2; 32]]);
        let ecdsa = blob("ecdsa-sha2-nistp256", &[b"nistp256", &[4; 65]]);
        let check = |text: &str, host: &str, port: u16, key: &[u8]| {
            std::fs::write(&path, text).unwrap();
            check_user_hosts(&path, &ssh_name(host, port), key)
        };
        let line = |name: &str, kind: &str, key: &[u8]| format!("{name} {kind} {}\n", padded(key));

        assert_eq!(
            check_user_hosts(&dir.path().join("none"), "h", &ours).unwrap(),
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
            UserHosts::Revoked
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
        // A key that is base64 but not of the type its line names can't be read either.
        let wrong = format!("example.com ssh-ed25519 {}\n", padded(&ecdsa));
        assert!(check(&wrong, "example.com", 22, &ours).is_err());

        // Patterns, lists and negations, whatever their case.
        let wild = line("*.COM", "ssh-ed25519", &theirs);
        assert_eq!(
            check(&wild, "example.com", 22, &ours).unwrap(),
            UserHosts::Changed
        );
        let list = line("other.org,EXAMPLE.com", "ssh-ed25519", &ours);
        assert_eq!(
            check(&list, "example.com", 22, &ours).unwrap(),
            UserHosts::Match
        );
        let negated = line("!example.com,*.com", "ssh-ed25519", &theirs);
        assert_eq!(
            check(&negated, "example.com", 22, &ours).unwrap(),
            UserHosts::Unknown
        );
        assert_eq!(
            check(&negated, "other.com", 22, &ours).unwrap(),
            UserHosts::Changed
        );
        // Hashed names, as `ssh-keygen -H` writes them.
        let salt = "|1|AAECAwQFBgcICQoLDA0ODxAREhM=";
        let hashed = line(
            &format!("{salt}|nnUK16ANsXd3hL31YfAkGOluSjU="),
            "ssh-ed25519",
            &ours,
        );
        assert_eq!(
            check(&hashed, "example.com", 22, &ours).unwrap(),
            UserHosts::Match
        );
        assert_eq!(
            check(&hashed, "example.com", 22, &theirs).unwrap(),
            UserHosts::Changed
        );
        assert_eq!(
            check(&hashed, "example.org", 22, &theirs).unwrap(),
            UserHosts::Unknown
        );
        let hashed_port = line(
            &format!("{salt}|Wgcx+Fm+LmaWwC7rQ80eIf2uHe0="),
            "ssh-ed25519",
            &ours,
        );
        assert_eq!(
            check(&hashed_port, "example.com", 2222, &ours).unwrap(),
            UserHosts::Match
        );
        // A key revoked for every host is refused even where another line records it.
        let everywhere = format!("{known}@revoked {}", line("*", "ssh-ed25519", &ours));
        assert_eq!(
            check(&everywhere, "example.com", 22, &ours).unwrap(),
            UserHosts::Revoked
        );
        // A file that is not UTF-8 throughout is still read.
        let mut latin1 = b"# caf\xe9\n".to_vec();
        latin1.extend_from_slice(known.as_bytes());
        std::fs::write(&path, latin1).unwrap();
        assert_eq!(
            check_user_hosts(&path, "example.com", &ours).unwrap(),
            UserHosts::Match
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(check_user_hosts(&path, "example.com", &ours).is_err());
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
        // Plain keys only, those a known hosts file records for the server first.
        let all = "ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,\
                   rsa-sha2-512,rsa-sha2-256,ssh-rsa";
        assert_eq!(host_key_methods(&[], false), all);
        assert_eq!(
            host_key_methods(
                &["ssh-rsa".into(), "ssh-dss".into(), "ssh-rsa".into()],
                false
            ),
            "rsa-sha2-512,rsa-sha2-256,ssh-rsa,ssh-ed25519,ecdsa-sha2-nistp256,\
             ecdsa-sha2-nistp384,ecdsa-sha2-nistp521"
        );
        assert!(
            !host_key_methods(&["ssh-ed25519-cert-v01@openssh.com".into()], false).contains("cert")
        );
        // Certificates only of a server an authority is named for, after its recorded keys.
        assert_eq!(
            host_key_methods(&["ecdsa-sha2-nistp256".into()], true)
                .split(',')
                .take(3)
                .collect::<Vec<_>>(),
            [
                "ecdsa-sha2-nistp256",
                "ssh-ed25519-cert-v01@openssh.com",
                "ecdsa-sha2-nistp256-cert-v01@openssh.com"
            ]
        );
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8", b"fo"),
            ("aGVsbG8=", b"hello"),
        ] {
            assert_eq!(decode_base64(text).as_deref(), Some(bytes));
        }
        assert_eq!(decode_base64("Z"), None);
        assert_eq!(decode_base64("Zm9v!"), None);
    }

    /// Host patterns match as OpenSSH matches them.
    #[test]
    fn known_host_patterns_match_as_ssh_matches_them() {
        for (name, pattern, matches) in [
            ("a.example.com", "*.example.com", true),
            ("example.com", "*.example.com", false),
            ("a.b.example.com", "*.example.com", true),
            ("host1", "host?", true),
            ("host10", "host?", false),
            ("anything", "*", true),
            ("[a.example.com]:2222", "[*.example.com]:2222", true),
            ("[a.example.com]:2222", "*.example.com", false),
            ("abc", "a*b*c", true),
            ("abd", "a*b*c", false),
        ] {
            assert_eq!(
                glob_match(name.as_bytes(), pattern.as_bytes()),
                matches,
                "{name} {pattern}"
            );
        }
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
    fn either_known_hosts_file_can_reject_a_key_the_other_trusts() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app_known_hosts");
        let ssh = dir.path().join("ssh_known_hosts");
        let key = blob("ssh-ed25519", &[&[1; 32]]);
        let other = blob("ssh-ed25519", &[&[2; 32]]);
        let name = KeyName::new("h", 22, None);
        let verify = |key: &[u8]| {
            verify_host_key(
                key,
                &name,
                &load_known_hosts(&app)?,
                &app,
                std::slice::from_ref(&ssh),
                0,
            )
        };
        let unknown = verify(&key).unwrap_err().downcast::<UnknownHost>().unwrap();
        trust_host(&unknown).unwrap();
        assert!(verify(&key).is_ok());
        assert!(verify(&other).is_err());

        let ssh_line = format!("h ssh-ed25519 {}\n", padded(&other));
        std::fs::write(&ssh, &ssh_line).unwrap();
        assert!(verify(&key).is_err());
        assert!(verify(&other).is_err());

        std::fs::remove_file(&app).unwrap();
        assert!(verify(&other).is_ok());
        remember_host(&app, "h:22", &other).unwrap();
        assert!(verify(&other).is_ok());
        std::fs::write(&ssh, format!("@revoked {ssh_line}")).unwrap();
        assert!(verify(&other).is_err());
    }

    /// The key and the base64 text of the public key fixture `name`.
    fn ssh_fixture(name: &str) -> (Vec<u8>, String) {
        let path = format!(
            "{}/tests/fixtures/ssh-certs/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(path).unwrap();
        let b64 = text.split_whitespace().nth(1).unwrap().to_string();
        (decode_base64(&b64).unwrap(), b64)
    }

    /// A host certificate is trusted as `ssh` trusts it: signed by an authority named for the
    /// server, valid for its name (or `HostKeyAlias`) now, and neither revoked nor at odds with
    /// a key trusted here; one from an authority not named for it is only its key.
    #[test]
    fn host_certificates_are_trusted_through_the_authorities_named_for_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app_known_hosts");
        let ssh = dir.path().join("known_hosts");
        let (cert, _) = ssh_fixture("h_ed-cert.pub");
        let (host_key, _) = ssh_fixture("host.pub");
        let (_, ca_ed) = ssh_fixture("ca_ed.pub");
        let (_, ca_rsa) = ssh_fixture("ca_rsa.pub");
        // 2026-11-08.
        let now = 1_794_000_000;
        let verify = |name: &KeyName, key: &[u8]| {
            verify_host_key(
                key,
                name,
                &load_known_hosts(&app)?,
                &app,
                std::slice::from_ref(&ssh),
                now,
            )
        };
        let named = KeyName::new("host.example", 22, None);
        std::fs::write(
            &ssh,
            format!("@cert-authority *.example ssh-ed25519 {ca_ed}\n"),
        )
        .unwrap();
        verify(&named, &cert).unwrap();
        assert_eq!(
            user_host_types(std::slice::from_ref(&ssh), "host.example"),
            (vec![], true)
        );
        // Through an alias, the alias is what the certificate must name.
        verify(&KeyName::new("10.0.0.5", 22, Some("host.example")), &cert).unwrap();
        let err = verify(&KeyName::new("other.example", 22, None), &cert).unwrap_err();
        assert!(format!("{err:#}").contains("other.example"), "{err:#}");
        let err = verify(&named, &cert[..cert.len() - 4]).unwrap_err();
        assert!(err.downcast_ref::<UnknownHost>().is_none());

        // An authority named only for other servers vouches for nothing here.
        std::fs::write(
            &ssh,
            format!("@cert-authority *.example ssh-rsa {ca_rsa}\n"),
        )
        .unwrap();
        let unknown = verify(&named, &cert)
            .unwrap_err()
            .downcast::<UnknownHost>()
            .unwrap();
        assert_eq!(unknown.key, host_key);
        trust_host(&unknown).unwrap();
        verify(&named, &cert).unwrap();

        // A key trusted here must still be the certificate's.
        std::fs::write(
            &ssh,
            format!("@cert-authority *.example ssh-ed25519 {ca_ed}\n"),
        )
        .unwrap();
        verify(&named, &cert).unwrap();
        remember_host(&app, "host.example:22", &blob("ssh-ed25519", &[&[9; 32]])).unwrap();
        assert!(verify(&named, &cert).is_err());
        std::fs::remove_file(&app).unwrap();

        // A revoked authority, or a revoked key, is refused.
        for revoked in [
            format!("ssh-ed25519 {ca_ed}"),
            format!("ssh-ed25519 {}", padded(&host_key)),
        ] {
            std::fs::write(
                &ssh,
                format!("@cert-authority *.example ssh-ed25519 {ca_ed}\n@revoked * {revoked}\n"),
            )
            .unwrap();
            let err = verify(&named, &cert).unwrap_err();
            assert!(err.downcast_ref::<UnknownHost>().is_none(), "{err:#}");
        }
    }

    #[test]
    fn encrypted_private_keys_are_told_from_plain_ones() {
        let dir = tempfile::tempdir().unwrap();
        let key = |cipher: &str| {
            let mut body = b"openssh-key-v1\0".to_vec();
            body.extend_from_slice(&(cipher.len() as u32).to_be_bytes());
            body.extend_from_slice(cipher.as_bytes());
            body.extend_from_slice(&[0; 16]);
            let path = dir.path().join(cipher);
            std::fs::write(
                &path,
                format!(
                    "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
                    padded(&body)
                ),
            )
            .unwrap();
            path
        };
        assert!(key_encrypted(&key("aes256-ctr")));
        assert!(!key_encrypted(&key("none")));
        let pem = dir.path().join("pem");
        std::fs::write(
            &pem,
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n\nAAAA\n",
        )
        .unwrap();
        assert!(key_encrypted(&pem));
        assert!(!key_encrypted(&dir.path().join("missing")));
    }

    /// A server's keyboard-interactive questions: a lone password question is answered with the
    /// password given, once; anything else is asked; once the user declines, nothing more is.
    #[test]
    fn a_servers_sign_in_questions_are_answered_or_asked() {
        use ssh2::KeyboardInteractivePrompt;
        let asked = std::sync::Mutex::new(Vec::new());
        let ask = |c: &Challenge| -> Option<Vec<String>> {
            asked.lock().unwrap().push(c.clone());
            let first = c.prompts.first()?;
            (!first.text.contains("decline")).then(|| vec!["123456".to_string(); c.prompts.len()])
        };
        let prompt = |text: &'static str, echo: bool| ssh2::Prompt {
            text: text.into(),
            echo,
        };
        let mut a = Answerer::new("h:22", Some("pw"), &ask);
        assert_eq!(a.prompt("", "", &[prompt("Password: ", false)]), ["pw"]);
        assert!(asked.lock().unwrap().is_empty());
        // The password is given once only: asked again, it was wrong.
        assert_eq!(
            a.prompt("2FA", "Enter the code", &[prompt("Password: ", false)]),
            ["123456"]
        );
        assert_eq!(
            a.prompt("", "", &[prompt("Code: ", false), prompt("Name: ", true)]),
            ["123456", "123456"]
        );
        {
            let asked = asked.lock().unwrap();
            assert_eq!(asked.len(), 2);
            assert_eq!(asked[0].server, "h:22");
            assert_eq!(asked[0].name, "2FA");
            assert_eq!(asked[0].instructions, "Enter the code");
            assert_eq!(
                asked[1].prompts[1],
                Prompt {
                    text: "Name: ".into(),
                    echo: true
                }
            );
        }
        // A round with nothing to answer is only shown text.
        assert!(a.prompt("", "Welcome", &[]).is_empty());
        assert_eq!(asked.lock().unwrap().len(), 2);

        let mut a = Answerer::new("h:22", None, &ask);
        assert_eq!(a.prompt("", "", &[prompt("Password: ", false)]), ["123456"]);
        assert_eq!(a.prompt("", "", &[prompt("decline", false)]), [""]);
        let before = asked.lock().unwrap().len();
        assert_eq!(a.prompt("", "", &[prompt("Code: ", false)]), [""]);
        assert_eq!(
            asked.lock().unwrap().len(),
            before,
            "not asked once declined"
        );

        // A server that keeps asking is answered blank after a while.
        let mut a = Answerer::new("h:22", None, &ask);
        for _ in 0..MAX_ROUNDS {
            assert_eq!(a.prompt("", "", &[prompt("Code: ", false)]), ["123456"]);
        }
        assert_eq!(a.prompt("", "", &[prompt("Code: ", false)]), [""]);
    }

    #[test]
    fn ways_of_signing_in_are_tried_from_the_one_given() {
        assert_eq!(sign_in_order(&Auth::Password(String::new()))[0], "password");
        assert_eq!(sign_in_order(&Auth::Agent)[0], "publickey");
        let key = Auth::Key {
            path: PathBuf::new(),
            passphrase: String::new(),
        };
        assert_eq!(
            sign_in_order(&key),
            ["publickey", "keyboard-interactive", "password"]
        );
    }
}
