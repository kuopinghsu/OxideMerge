//! Paths on a server named on the command line and in job files: `sftp://`, `ftp://`,
//! `ftpes://` (FTPS switched to TLS with `AUTH TLS`) and `ftps://` (implicit FTPS) URLs. Each is
//! fetched to a temporary copy, compared like any local path, and written back afterwards when
//! the command changed it (a synchronization applied to it, say).
//!
//! A URL never holds a password: one there would end up in shell histories and process lists.
//! The password, or a private key's passphrase, comes from [`PASSWORD_ENV`] instead; SFTP
//! without either signs in with `ssh-agent`'s keys and those `~/.ssh/config` names, which also
//! says how to reach a host (an alias, its user, port and jump hosts). A proxy's password
//! comes from [`PROXY_PASSWORD_ENV`] the same way.

use super::folder_sync;
use super::remote::{self, Auth, Fetched, Protocol, RemoteConnection, UnknownHost};
use super::remote_sync;
use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The environment variable holding the password, or with `--ssh-key` the key's passphrase.
pub const PASSWORD_ENV: &str = "OXIDEMERGE_REMOTE_PASSWORD";

/// The environment variable holding the password of the `--proxy` user.
pub const PROXY_PASSWORD_ENV: &str = "OXIDEMERGE_PROXY_PASSWORD";

/// [`PASSWORD_ENV`]'s and [`PROXY_PASSWORD_ENV`]'s values, once [`take_password_env`] took them
/// out of the environment.
static PASSWORD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
static PROXY_PASSWORD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Takes [`PASSWORD_ENV`] and [`PROXY_PASSWORD_ENV`] out of the environment, keeping their
/// values for [`Credentials`], so the programs this one starts (`git`, an editor, a tool a job
/// runs) do not inherit the passwords. Called first thing, before any other thread reads the
/// environment.
pub fn take_password_env() {
    for (var, slot) in [
        (PASSWORD_ENV, &PASSWORD),
        (PROXY_PASSWORD_ENV, &PROXY_PASSWORD),
    ] {
        let value = std::env::var(var).ok().filter(|s| !s.is_empty());
        if std::env::var_os(var).is_some() {
            std::env::remove_var(var);
        }
        let _ = slot.set(value);
    }
}

/// The value of the password variable `var`, taken out of the environment into `slot` or still
/// there.
fn password_env(var: &str, slot: &std::sync::OnceLock<Option<String>>) -> Option<String> {
    match slot.get() {
        Some(taken) => taken.clone(),
        None => std::env::var(var).ok().filter(|s| !s.is_empty()),
    }
}

/// The proxy `--proxy` names (see [`super::net::Proxy::parse`]), signing in with the password
/// in [`PROXY_PASSWORD_ENV`]: one written in the URL is refused, as for a server's.
pub fn proxy(text: Option<&str>) -> Result<Option<super::net::Proxy>> {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        return Ok(None);
    };
    let mut proxy = super::net::Proxy::parse(text)?;
    if proxy.password.is_some() {
        bail!(trf(
            "msg.proxy_url_password",
            &[("var", PROXY_PASSWORD_ENV.to_string())]
        ));
    }
    if proxy.user.is_some() {
        proxy.password = password_env(PROXY_PASSWORD_ENV, &PROXY_PASSWORD);
    }
    Ok(Some(proxy))
}

/// A parsed remote URL.
#[derive(Debug, PartialEq, Eq)]
pub struct RemoteUrl {
    pub protocol: Protocol,
    /// Without the brackets of an IPv6 address.
    pub host: String,
    pub port: u16,
    /// Whether the URL names the port, rather than the protocol's usual one being taken.
    pub port_given: bool,
    pub user: Option<String>,
    /// The path on the server: absolute, or relative to the user's home folder for a URL whose
    /// path starts with `/~`.
    pub path: String,
}

/// Whether `text` names a path on a server rather than a local one.
pub fn is_remote_url(text: &str) -> bool {
    text.split_once("://")
        .is_some_and(|(scheme, _)| Protocol::from_scheme(scheme).is_some())
}

/// `text` with its `%XX` escapes decoded; `bad` is the error when one is not valid.
pub(crate) fn decode(text: &str, bad: impl Fn() -> String + Copy) -> Result<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3).with_context(bad)?;
            out.push(u8::from_str_radix(hex, 16).ok().with_context(bad)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().with_context(bad)
}

/// `url` to show in a message, without what may be a password in it: all between the scheme
/// and the last `@`, when that holds a `:`.
pub fn shown(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((user, host)) = rest.rsplit_once('@') {
            if user.contains(':') {
                return format!("{scheme}://…@{host}");
            }
        }
    }
    url.to_string()
}

/// Reads `scheme://[user@]host[:port][/path]`.
pub fn parse(url: &str) -> Result<RemoteUrl> {
    parse_with_password(url, false).map(|(url, _)| url)
}

/// Reads a server address entered in the GUI, of any protocol, including an optional inline
/// password.
pub(crate) fn parse_input(url: &str) -> Result<(RemoteUrl, Option<String>)> {
    parse_with_password(url, true)
}

/// Removes an inline password while retaining the encoded username and path for a retry.
pub(crate) fn without_password(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if let Some((user, host)) = authority.rsplit_once('@') {
        if let Some((user, _)) = user.split_once(':') {
            return format!("{scheme}://{user}@{host}{path}");
        }
    }
    url.to_owned()
}

/// An address for the comparison history, without credentials or a staging path.
pub(crate) fn address(conn: &RemoteConnection, path: &str) -> String {
    let encode = |s: &str| {
        url::form_urlencoded::byte_serialize(s.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    };
    let user = if conn.user.is_empty() {
        String::new()
    } else {
        format!("{}@", encode(&conn.user))
    };
    let server = if conn.options.port_given || conn.port != conn.protocol.default_port() {
        remote::server(&conn.host, conn.port)
    } else if conn.host.contains(':') {
        format!("[{}]", conn.host)
    } else {
        conn.host.clone()
    };
    let prefix = if path.starts_with('/') { "" } else { "/~/" };
    let path = encode(path).replace("%2F", "/");
    format!("{}://{user}{server}{prefix}{path}", conn.protocol.scheme())
}

fn parse_with_password(url: &str, allow_password: bool) -> Result<(RemoteUrl, Option<String>)> {
    let bad = || trf("msg.remote_url_bad", &[("url", shown(url))]);
    let (scheme, rest) = url.split_once("://").with_context(bad)?;
    let protocol = Protocol::from_scheme(scheme).with_context(bad)?;
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, host_port)) => (Some(user), host_port),
        None => (None, authority),
    };
    // A password with a `/` in it ends the authority early, leaving its rest and the host in
    // the path's first name: `sftp://ann:12/34@h/x` is not port 12 of a host `ann`.
    let cut_password = user.is_none()
        && !authority.starts_with('[')
        && authority.contains(':')
        && path
            .get(1..)
            .and_then(|p| p.split('/').next())
            .is_some_and(|first| first.contains('@'));
    let (user, password) = match user.and_then(|u| u.split_once(':')) {
        Some((user, password)) if allow_password => (Some(user), Some(password)),
        Some(_) => bail!(tr("msg.remote_url_password")),
        None => (user, None),
    };
    if cut_password {
        bail!(tr("msg.remote_url_password"));
    }
    let user = user
        .map(|u| decode(u, bad))
        .transpose()?
        .filter(|u| !u.is_empty());
    // A line break would end a command to the server or proxy early, and start another.
    if user
        .as_deref()
        .is_some_and(|u| u.chars().any(char::is_control))
    {
        bail!(bad());
    }
    let (host, port) = match host_port.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']').with_context(bad)?;
            match after {
                "" => (host, None),
                _ => (host, Some(after.strip_prefix(':').with_context(bad)?)),
            }
        }
        None => match host_port.rsplit_once(':') {
            // An IPv6 address needs its brackets, or its last group would read as the port.
            Some((host, _)) if host.contains(':') => bail!(bad()),
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        },
    };
    if host.is_empty()
        || host.contains(['/', '@'])
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!(bad());
    }
    let port_given = port.is_some();
    let port = match port {
        Some(p) => p.parse().ok().filter(|&p: &u16| p > 0).with_context(bad)?,
        None => protocol.default_port(),
    };
    let path = decode(path, bad)?;
    let path = match path.strip_prefix("/~") {
        Some(rest) if rest.is_empty() || rest == "/" => ".".to_string(),
        Some(rest) if rest.starts_with('/') => rest[1..].to_string(),
        _ if path.is_empty() => ".".to_string(),
        _ => path,
    };
    let password = password.map(|p| decode(p, bad)).transpose()?;
    Ok((
        RemoteUrl {
            protocol,
            host: host.to_string(),
            port,
            port_given,
            user,
            path,
        },
        password,
    ))
}

#[cfg(test)]
#[test]
fn sftp_input_decodes_password_and_preserves_the_encoded_address_without_it() {
    let address = "sftp://ann:p%40ss%2Fword@[::1]:22/~/my%20file.txt";
    let (url, password) = parse_input(address).unwrap();
    assert_eq!(url.host, "::1");
    assert!(url.port_given);
    assert_eq!(url.user.as_deref(), Some("ann"));
    assert_eq!(url.path, "my file.txt");
    assert_eq!(password.as_deref(), Some("p@ss/word"));
    assert_eq!(
        without_password(address),
        "sftp://ann@[::1]:22/~/my%20file.txt"
    );
    assert!(parse(address).is_err());
    assert!(parse_input("sftp://ann:secret@host/%zz")
        .unwrap_err()
        .to_string()
        .contains("…@host"));
    assert_eq!(
        without_password("sftp://[::1]/file@name"),
        "sftp://[::1]/file@name"
    );
    assert_eq!(
        parse_input("sftp://[::1]/file@name").unwrap().0.path,
        "/file@name"
    );
    // The other protocols are read the same way, and written back with their own scheme and
    // usual port.
    let (url, password) = parse_input("ftpes://bob:pw@files.example/pub/a.txt").unwrap();
    assert_eq!(url.protocol, Protocol::Ftps);
    assert_eq!(password.as_deref(), Some("pw"));
    let conn = RemoteConnection {
        protocol: url.protocol,
        host: url.host,
        port: url.port,
        user: url.user.unwrap(),
        auth: Auth::Password(String::new()),
        options: Default::default(),
    };
    assert_eq!(
        self::address(&conn, &url.path),
        "ftpes://bob@files.example/pub/a.txt"
    );
}

/// How the command line or a job signs in to the servers its URLs name.
#[derive(Default)]
pub struct Credentials {
    /// `--ssh-key`: a private key file for SFTP.
    pub ssh_key: Option<PathBuf>,
    /// The password, or the key's passphrase (see [`PASSWORD_ENV`]).
    pub secret: Option<String>,
    /// `--trust-fingerprint`: the fingerprint of a server's key or certificate to trust, if
    /// the server has not been trusted yet and shows exactly this one.
    pub trust: Option<String>,
    /// `--proxy`, `--ftp-active`, `--client-cert` and `--client-key`.
    pub options: remote::ConnOptions,
}

impl Credentials {
    /// `ssh_key` and `trust` as given, with the secret from the environment (empty is none).
    pub fn from_env(ssh_key: Option<PathBuf>, trust: Option<String>) -> Self {
        Self {
            ssh_key,
            secret: password_env(PASSWORD_ENV, &PASSWORD),
            trust,
            options: Default::default(),
        }
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        if let Some(s) = &mut self.secret {
            remote::wipe(s);
        }
    }
}

/// The connection `url` names, signed in as `creds` say: a key, or ssh-agent's and
/// `~/.ssh/config`'s keys, for SFTP (or a password, when one is given), and a password for FTP.
/// Without a user name, SFTP signs in as `~/.ssh/config` says or as the local user, and FTP
/// anonymously, never with the password, which is for a user named.
pub fn connection(url: &RemoteUrl, creds: &Credentials) -> Result<RemoteConnection> {
    let user = match &url.user {
        Some(user) => user.clone(),
        None if url.protocol.is_ftp() => "anonymous".to_string(),
        None => String::new(),
    };
    let auth = if url.protocol.is_ftp() {
        if creds.ssh_key.is_some() {
            bail!(tr("msg.ftp_password_only"));
        }
        match &creds.secret {
            _ if url.user.is_none() => Auth::Password("anonymous@".to_string()),
            Some(p) => Auth::Password(p.clone()),
            None if user == "anonymous" => Auth::Password("anonymous@".to_string()),
            None => bail!(trf(
                "msg.remote_url_no_password",
                &[("var", PASSWORD_ENV.to_string())]
            )),
        }
    } else {
        match (&creds.ssh_key, &creds.secret) {
            (Some(key), secret) => Auth::Key {
                path: key.clone(),
                passphrase: secret.clone().unwrap_or_default(),
            },
            (None, Some(p)) => Auth::Password(p.clone()),
            (None, None) => Auth::Agent,
        }
    };
    Ok(RemoteConnection {
        protocol: url.protocol,
        host: url.host.clone(),
        port: url.port,
        user,
        auth,
        options: remote::ConnOptions {
            port_given: url.port_given,
            ..creds.options.clone()
        },
    })
}

/// Whether the fingerprint the user gave, `given`, is `shown`: as written, or for a
/// certificate's hexadecimal one, in either case and with or without its colons.
fn same_fingerprint(given: &str, shown: &str) -> bool {
    let given = given.trim();
    if given == shown {
        return true;
    }
    let hex = |s: &str| -> Option<String> {
        let s = match s.get(..7) {
            Some(p) if p.eq_ignore_ascii_case("sha256:") => &s[7..],
            _ => s,
        }
        .replace(':', "");
        (s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
    };
    matches!((hex(given), hex(shown)), (Some(a), Some(b)) if a == b)
}

/// Fetches `path` over `conn`, trusting a server not trusted yet only when it shows exactly the
/// fingerprint `trust`.
fn fetch_trusting(
    conn: &RemoteConnection,
    path: &str,
    trust: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Fetched> {
    match remote::fetch(conn, path, cancel) {
        Err(e) => match e.downcast::<UnknownHost>() {
            Ok(host) if trust.is_some_and(|t| same_fingerprint(t, &host.fingerprint)) => {
                remote::trust_host(&host)?;
                remote::fetch(conn, path, cancel)
            }
            Ok(host) => bail!(
                "{host}\n{}",
                trf(
                    "msg.remote_trust_hint",
                    &[("fingerprint", host.fingerprint.clone())]
                )
            ),
            Err(e) => Err(e),
        },
        fetched => fetched,
    }
}

/// A path of the command line fetched from a server.
pub struct Staged {
    pub conn: RemoteConnection,
    pub remote_path: String,
    pub fetched: Fetched,
    /// For a file, a hash of its bytes as fetched.
    sent: Option<u64>,
}

impl Staged {
    pub fn label(&self) -> String {
        remote::label(&self.conn, &self.remote_path)
    }

    /// The connection, the path on the server and the fetch, for the window to write back.
    pub fn into_parts(self) -> (RemoteConnection, String, Fetched) {
        (self.conn, self.remote_path, self.fetched)
    }
}

/// Fetches each of `paths` that is a remote URL, replacing it with its local copy.
pub fn stage(
    paths: &mut [PathBuf],
    creds: &Credentials,
    cancel: &AtomicBool,
) -> Result<Vec<Staged>> {
    let mut staged = Vec::new();
    for p in paths.iter_mut() {
        let Some(text) = p.to_str().filter(|t| is_remote_url(t)) else {
            continue;
        };
        let url = parse(text)?;
        let conn = connection(&url, creds)?;
        let label = remote::label(&conn, &url.path);
        let fetched = fetch_trusting(&conn, &url.path, creds.trust.as_deref(), cancel)
            .with_context(|| label.clone())?;
        *p = fetched.local.clone();
        let sent = (!fetched.is_dir)
            .then(|| remote::file_hash(&fetched.local))
            .flatten();
        staged.push(Staged {
            conn,
            remote_path: url.path,
            fetched,
            sent,
        });
    }
    Ok(staged)
}

/// Whether the local copy of `s` changed since it was fetched.
fn changed(s: &Staged) -> bool {
    match &s.fetched.manifest {
        Some(manifest) => remote_sync::changed(&s.fetched.local, manifest),
        None => remote::file_hash(&s.fetched.local) != s.sent,
    }
}

/// [`write_back`], after a command that ended with exit code `code`. A synchronization that
/// stopped without undoing all it changed (5) is undone instead, from its journal under
/// `journal_root`, where it changed a local copy of `staged`, and nothing is written back: the
/// servers keep what they had, and no journal is left for a copy about to be deleted.
pub fn finish(
    staged: &[Staged],
    code: u8,
    journal_root: Option<&Path>,
    cancel: &AtomicBool,
) -> Vec<String> {
    if code != 5 {
        return write_back(staged, cancel);
    }
    if staged.is_empty() {
        return Vec::new();
    }
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let copies: Vec<PathBuf> = staged.iter().map(|s| real(&s.fetched.local)).collect();
    for u in journal_root
        .map(folder_sync::unfinished)
        .unwrap_or_default()
    {
        let changed = real(&u.roots[u.destination.min(1)]);
        if copies.iter().any(|c| changed.starts_with(c)) {
            folder_sync::recover(&u);
        }
    }
    vec![tr("msg.remote_not_written_back").to_string()]
}

/// Writes what changed in the local copies of `staged` back to their servers; returns what
/// could not be, one message each.
pub fn write_back(staged: &[Staged], cancel: &AtomicBool) -> Vec<String> {
    let mut errors = Vec::new();
    for s in staged.iter().filter(|s| changed(s)) {
        let local: &Path = &s.fetched.local;
        let label = s.label();
        match &s.fetched.manifest {
            Some(manifest) => {
                match remote_sync::save_folder(&s.conn, local, &s.remote_path, manifest, cancel) {
                    Ok(saved) => {
                        errors.extend(saved.errors.into_iter().map(|e| format!("{label}: {e}")))
                    }
                    Err(e) => errors.push(format!("{label}: {e:#}")),
                }
            }
            None => match remote::upload(
                &s.conn,
                local,
                &s.remote_path,
                s.fetched.stamp,
                s.sent,
                cancel,
            ) {
                // Saved all the same: said, but not a failure.
                Ok(saved) => {
                    if let Some(left) = saved.left_over {
                        eprintln!("note: {label}: {left}");
                    }
                }
                Err(e) => errors.push(format!("{label}: {e:#}")),
            },
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::remote::fake;

    #[test]
    fn urls_name_the_protocol_server_user_and_path() {
        let u = parse("sftp://ann@example.com/srv/data").unwrap();
        assert_eq!(
            u,
            RemoteUrl {
                protocol: Protocol::Sftp,
                host: "example.com".into(),
                port: 22,
                port_given: false,
                user: Some("ann".into()),
                path: "/srv/data".into(),
            }
        );
        let u = parse("ftps://bob%40corp@[::1]:2990/a%20b.txt").unwrap();
        assert_eq!(u.protocol, Protocol::FtpsImplicit);
        assert_eq!((u.host.as_str(), u.port), ("::1", 2990));
        assert_eq!(u.user.as_deref(), Some("bob@corp"));
        assert_eq!(u.path, "/a b.txt");
        let u = parse("FTPES://files.example.com:21/~/notes").unwrap();
        assert_eq!(
            (u.protocol, u.user, u.path.as_str()),
            (Protocol::Ftps, None, "notes")
        );
        assert_eq!(parse("ftp://h").unwrap().path, ".");
        assert_eq!(parse("ftp://h/~").unwrap().path, ".");
        assert_eq!(parse("ftp://h").unwrap().port, 21);

        assert!(is_remote_url("sftp://h/x") && is_remote_url("ftpes://h"));
        assert!(!is_remote_url("https://h/x") && !is_remote_url("/tmp/sftp://x"));
        for bad in [
            "sftp://",
            "sftp://h:0/x",
            "sftp://h:port/x",
            "sftp://[::1/x",
            "ftp://h/%zz",
            "ftp://a%0D%0AUSER%20x@h/x",
            "ftp://h\r\nX/x",
            "sftp://h\tx/x",
            "sftp://::1/x",
            "sftp://fe80::1:22/x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        for leaky in [
            "sftp://ann:secret@example.com/x",
            "sftp://ann:sec/ret@example.com/x",
            "sftp://ann:12/34@example.com/x",
            "sftp://ann:s/e/cret@example.com/x",
        ] {
            let e = parse(leaky).unwrap_err().to_string();
            assert!(!e.contains("ret") && !e.contains("34"), "{e}");
        }
        assert_eq!(
            parse("sftp://h:2222/etc/getty@tty1.service").unwrap().path,
            "/etc/getty@tty1.service"
        );
        assert_eq!(shown("sftp://ann:pw@h/x"), "sftp://…@h/x");
        assert_eq!(shown("sftp://ann@h/x"), "sftp://ann@h/x");
    }

    #[test]
    fn sign_in_follows_the_protocol_and_what_was_given() {
        let url = |s: &str| parse(s).unwrap();
        let creds = |key: Option<&str>, secret: Option<&str>| Credentials {
            ssh_key: key.map(PathBuf::from),
            secret: secret.map(str::to_string),
            trust: None,
            options: Default::default(),
        };
        let c = connection(&url("sftp://ann@h/x"), &creds(None, None)).unwrap();
        assert!(matches!(c.auth, Auth::Agent));
        let c = connection(&url("sftp://ann@h/x"), &creds(Some("/k"), Some("pp"))).unwrap();
        assert!(
            matches!(&c.auth, Auth::Key { path, passphrase } if path == Path::new("/k") && passphrase == "pp")
        );
        let c = connection(&url("sftp://ann@h/x"), &creds(None, Some("pw"))).unwrap();
        assert!(matches!(&c.auth, Auth::Password(p) if p == "pw"));

        let c = connection(&url("ftp://h/x"), &creds(None, None)).unwrap();
        assert_eq!(c.user, "anonymous");
        let c = connection(&url("ftp://h/x"), &creds(None, Some("pw"))).unwrap();
        assert!(matches!(&c.auth, Auth::Password(p) if p == "anonymous@"));
        assert!(connection(&url("ftp://ann@h/x"), &creds(None, None)).is_err());
        assert!(connection(&url("ftpes://ann@h/x"), &creds(Some("/k"), Some("pw"))).is_err());
        let c = connection(&url("ftpes://ann@h:2121/x"), &creds(None, Some("pw"))).unwrap();
        assert_eq!((c.protocol, c.port), (Protocol::Ftps, 2121));
        // Port 22 written out wins over `~/.ssh/config`'s, as with `ssh -p 22`.
        assert!(
            connection(&url("sftp://h:22/x"), &creds(None, None))
                .unwrap()
                .options
                .port_given
        );
        assert!(
            !connection(&url("sftp://h/x"), &creds(None, None))
                .unwrap()
                .options
                .port_given
        );

        // Without a user name, SFTP leaves it to `~/.ssh/config` or the local user.
        let c = connection(&url("sftp://prod/x"), &creds(None, None)).unwrap();
        assert_eq!(c.user, "");
        let mut with = creds(None, None);
        with.options.active = true;
        with.options.proxy = proxy(Some("socks5://bob@p")).unwrap();
        let c = connection(&url("ftp://h/x"), &with).unwrap();
        assert!(c.options.active);
        assert_eq!(c.options.proxy.unwrap().url(), "socks5://bob@p:1080");
        assert!(proxy(Some("http://bob:pw@p")).is_err());
        assert_eq!(proxy(Some(" ")).unwrap(), None);
    }

    #[test]
    fn fingerprints_match_as_shown_or_as_hexadecimal_in_any_case() {
        let hex = format!("SHA256:{}", vec!["AB"; 32].join(":"));
        assert!(same_fingerprint(&hex, &hex));
        assert!(same_fingerprint(&"ab".repeat(32), &hex));
        assert!(same_fingerprint(&format!(" {} ", hex.to_lowercase()), &hex));
        assert!(!same_fingerprint(&"ac".repeat(32), &hex));
        assert!(same_fingerprint("SHA256:abcDEF", "SHA256:abcDEF"));
        assert!(!same_fingerprint("SHA256:abcdef", "SHA256:abcDEF"));
    }

    #[test]
    fn staged_paths_are_fetched_and_what_changed_is_written_back() {
        let never = AtomicBool::new(false);
        let creds = |trust: Option<&str>| Credentials {
            ssh_key: None,
            secret: Some("pw".into()),
            trust: trust.map(String::from),
            options: Default::default(),
        };
        fake::install(
            fake::Server::new()
                .dir("/srv")
                .dir("/srv/data")
                .file("/srv/data/a.txt", "a")
                .file("/srv/b.txt", "b")
                .unknown_host(),
        );
        let plain = creds(None);
        let mut paths = vec![
            PathBuf::from("sftp://ann@example.com/srv/data"),
            PathBuf::from("/local/side"),
            PathBuf::from("sftp://ann@example.com/srv/b.txt"),
        ];
        let e = stage(&mut paths.clone(), &plain, &never).err().unwrap();
        let e = format!("{e:#}");
        assert!(
            e.contains("SHA256:fake") && e.contains("--trust-fingerprint"),
            "{e}"
        );
        let wrong = creds(Some("SHA256:other"));
        assert!(stage(&mut paths.clone(), &wrong, &never).is_err());
        let trusting = creds(Some("SHA256:fake"));
        let staged = stage(&mut paths, &trusting, &never).unwrap();
        assert_eq!(staged.len(), 2);
        assert_eq!(paths[1], PathBuf::from("/local/side"));
        assert_eq!(
            std::fs::read_to_string(paths[0].join("a.txt")).unwrap(),
            "a"
        );
        assert_eq!(std::fs::read_to_string(&paths[2]).unwrap(), "b");

        // Nothing changed, nothing is written.
        assert!(write_back(&staged, &never).is_empty());
        assert_eq!(fake::upload_count("/srv/b.txt"), 0);

        std::fs::write(paths[0].join("new.txt"), "new").unwrap();
        std::fs::write(&paths[2], "b edited").unwrap();
        assert!(write_back(&staged, &never).is_empty());
        assert_eq!(
            fake::entry("/srv/data/new.txt"),
            Some(Some(b"new".to_vec()))
        );
        assert_eq!(fake::entry("/srv/b.txt"), Some(Some(b"b edited".to_vec())));
        fake::uninstall();
    }

    #[test]
    fn a_synchronization_left_unfinished_is_undone_and_nothing_written_back() {
        let never = AtomicBool::new(false);
        let creds = Credentials {
            ssh_key: None,
            secret: Some("pw".into()),
            trust: None,
            options: Default::default(),
        };
        fake::install(
            fake::Server::new()
                .dir("/srv")
                .dir("/srv/data")
                .file("/srv/data/a.txt", "server a"),
        );
        let mut paths = vec![PathBuf::from("sftp://ann@example.com/srv/data")];
        let staged = stage(&mut paths, &creds, &never).unwrap();
        let mine = tempfile::tempdir().unwrap();
        std::fs::write(mine.path().join("a.txt"), "local a").unwrap();
        std::fs::write(mine.path().join("b.txt"), "local b").unwrap();
        let journals = tempfile::tempdir().unwrap();
        let roots = [mine.path().to_path_buf(), paths[0].clone()];
        let outcome = folder_sync::mirror_then_crash(&roots, journals.path());
        assert!(
            matches!(outcome, folder_sync::SyncOutcome::Stopped { .. }),
            "{outcome:?}"
        );
        assert_eq!(folder_sync::unfinished(journals.path()).len(), 1);

        let errors = finish(&staged, 5, Some(journals.path()), &never);
        assert_eq!(errors, [tr("msg.remote_not_written_back").to_string()]);
        assert!(folder_sync::unfinished(journals.path()).is_empty());
        assert_eq!(
            std::fs::read_to_string(paths[0].join("a.txt")).unwrap(),
            "server a"
        );
        assert!(!paths[0].join("b.txt").exists());
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"server a".to_vec()))
        );
        assert_eq!(fake::entry("/srv/data/b.txt"), None);

        // Any other exit code writes back as usual.
        std::fs::write(paths[0].join("a.txt"), "edited").unwrap();
        assert!(finish(&staged, 0, Some(journals.path()), &never).is_empty());
        assert_eq!(
            fake::entry("/srv/data/a.txt"),
            Some(Some(b"edited".to_vec()))
        );
        fake::uninstall();
    }
}
