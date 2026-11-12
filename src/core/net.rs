//! Connections to a server, for both SFTP and FTP: made directly, through a proxy (SOCKS5 or
//! HTTP `CONNECT`), or carried by another program's input and output, as `ssh`'s
//! `ProxyCommand` is. Each ends as a [`TcpStream`], what libssh2 and the FTP client read and
//! write; a stream that is not a socket is handed over through a [`socket_pair`].

use super::remote::{check, resolve, server, wipe, CONNECT_TIMEOUT};
use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The kind of proxy a connection goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    /// SOCKS version 5: the proxy looks up the server's name itself.
    Socks5,
    /// An HTTP proxy that tunnels with `CONNECT`.
    Http,
}

/// A proxy to connect through. Its password is overwritten in memory once dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(p) = &mut self.password {
            wipe(p);
        }
    }
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url())
    }
}

impl Proxy {
    /// Reads `socks5://[user[:password]@]host[:port]` (or `socks5h://`, the same here) or
    /// `http://[user[:password]@]host[:port]`, with `%XX` escapes; the port defaults to 1080
    /// for SOCKS and 8080 for HTTP.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let shown = redacted(text);
        let bad = || trf("msg.proxy_bad", &[("proxy", shown.clone())]);
        let (scheme, rest) = text.split_once("://").with_context(bad)?;
        let (kind, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => (ProxyKind::Socks5, 1080),
            "http" => (ProxyKind::Http, 8080),
            _ => bail!(bad()),
        };
        let rest = rest.trim_end_matches('/');
        if rest.contains('/') {
            bail!(bad());
        }
        let (auth, host_port) = match rest.rsplit_once('@') {
            Some((auth, host_port)) => (Some(auth), host_port),
            None => (None, rest),
        };
        let (user, password) = match auth {
            Some(auth) => {
                let (user, password) = match auth.split_once(':') {
                    Some((u, p)) => (u, Some(p)),
                    None => (auth, None),
                };
                let decode = |s: &str| super::remote_url::decode(s, bad);
                (
                    Some(decode(user)?).filter(|u| !u.is_empty()),
                    password.map(decode).transpose()?,
                )
            }
            None => (None, None),
        };
        let (host, port) = match host_port.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').with_context(bad)?;
                match after {
                    "" => (host, None),
                    _ => (host, Some(after.strip_prefix(':').with_context(bad)?)),
                }
            }
            None => match host_port.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (host_port, None),
            },
        };
        if host.is_empty() || host.contains([' ', '@']) {
            bail!(bad());
        }
        let port = match port {
            Some(p) => p.parse().ok().filter(|&p: &u16| p > 0).with_context(bad)?,
            None => default_port,
        };
        Ok(Proxy {
            kind,
            host: host.to_string(),
            port,
            user,
            password,
        })
    }

    /// The proxy as written, without its password: to remember and to show.
    pub fn url(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
        };
        let user = self
            .user
            .as_ref()
            .map(|u| format!("{}@", escape(u)))
            .unwrap_or_default();
        format!("{scheme}://{user}{}", server(&self.host, self.port))
    }
}

/// The proxy URL `text` with any password in it hidden, to show in a message: whatever
/// follows the user name before the last `@`, with or without a scheme (or a mistyped one).
fn redacted(text: &str) -> String {
    let Some(at) = text.rfind('@') else {
        return text.to_string();
    };
    let mut start = 0;
    if let Some(colon) = text[..at].find(':') {
        let (scheme, after) = (&text[..colon], &text[colon + 1..at]);
        let is_scheme = !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if is_scheme && after.starts_with('/') {
            start = colon + 1 + (after.len() - after.trim_start_matches('/').len());
        }
    }
    match text[start..at].split_once(':') {
        Some((user, _)) => format!("{}{user}:***{}", &text[..start], &text[at..]),
        None => text.to_string(),
    }
}

/// `text` with the characters that would end a user name in a URL escaped.
fn escape(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'%' | b'@' | b':' | b'/' | b' ' => format!("%{b:02X}"),
            _ => (b as char).to_string(),
        })
        .collect()
}

/// A connection to `host` at `port`, through `proxy` when there is one, with the usual
/// timeouts set. Setting `cancel` stops it between steps.
pub(crate) fn connect(
    host: &str,
    port: u16,
    proxy: Option<&Proxy>,
    cancel: &AtomicBool,
) -> Result<TcpStream> {
    let shown = server(host, port);
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!(trf(
            "msg.sftp_lookup",
            &[("host", host.escape_debug().to_string())]
        ));
    }
    let tcp = match proxy {
        None => direct(host, port, cancel)?
            .with_context(|| trf("msg.sftp_unreachable", &[("server", shown.clone())]))?,
        Some(p) => {
            let at = server(&p.host, p.port);
            let mut tcp = direct(&p.host, p.port, cancel)?
                .with_context(|| trf("msg.proxy_unreachable", &[("proxy", at.clone())]))?;
            check(cancel)?;
            tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
            let mut talk = Handshake {
                tcp: &mut tcp,
                until: Instant::now() + CONNECT_TIMEOUT,
                cancel,
            };
            let through = match p.kind {
                ProxyKind::Socks5 => socks5(&mut talk, p, host, port),
                ProxyKind::Http => http_connect(&mut talk, p, host, port),
            };
            through.with_context(|| {
                trf(
                    "msg.proxy_refused",
                    &[("proxy", at.clone()), ("server", shown.clone())],
                )
            })?;
            tcp
        }
    };
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    Ok(tcp)
}

/// Looks `host` up (failing with why it could not be) and tries each of its addresses in turn,
/// giving the last one's failure to connect.
fn direct(host: &str, port: u16, cancel: &AtomicBool) -> Result<std::io::Result<TcpStream>> {
    let addrs = resolve(host, port)?;
    check(cancel)?;
    // Local services can listen on just one address family. A loopback address that
    // silently drops connections must not delay trying the other one for 20 seconds.
    let timeout = if addrs.len() > 1 && addrs.iter().all(|addr| addr.ip().is_loopback()) {
        Duration::from_millis(250)
    } else {
        CONNECT_TIMEOUT
    };
    let mut tcp = Err(std::io::Error::other("no address"));
    for addr in &addrs {
        tcp = TcpStream::connect_timeout(addr, timeout);
        if tcp.is_ok() {
            break;
        }
        check(cancel)?;
    }
    Ok(tcp)
}

/// A proxy's side of the conversation, read within one time limit for all of it, and stopped
/// once `cancel` is set, however slowly the proxy answers.
struct Handshake<'a> {
    tcp: &'a mut TcpStream,
    until: Instant,
    cancel: &'a AtomicBool,
}

impl Handshake<'_> {
    /// Waits at most a tenth of a second at a time, to see `cancel` set.
    fn wait(&mut self) -> std::io::Result<()> {
        // Not `Interrupted`, which `read_exact` would only try again after.
        if let Err(e) = check(self.cancel) {
            return Err(std::io::Error::other(e.to_string()));
        }
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        self.tcp
            .set_read_timeout(Some(left.min(Duration::from_millis(100))))
    }

    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.tcp.write_all(bytes)
    }
}

impl Read for Handshake<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            self.wait()?;
            match self.tcp.read(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                other => return other,
            }
        }
    }
}

/// Asks the SOCKS5 proxy at the other end of `tcp` to connect to `host` at `port` (RFC 1928),
/// signing in with a user name and password (RFC 1929) when the proxy has them.
fn socks5(tcp: &mut Handshake, proxy: &Proxy, host: &str, port: u16) -> Result<()> {
    let with_password = proxy.user.is_some();
    let methods: &[u8] = if with_password { &[0, 2] } else { &[0] };
    tcp.write_all(&[&[5, methods.len() as u8], methods].concat())?;
    let mut chosen = [0u8; 2];
    tcp.read_exact(&mut chosen)?;
    if chosen[0] != 5 {
        bail!(tr("msg.proxy_not_socks"));
    }
    match chosen[1] {
        0 => {}
        2 if with_password => {
            let user = proxy.user.as_deref().unwrap_or("").as_bytes();
            let password = proxy.password.as_deref().unwrap_or("").as_bytes();
            if user.len() > 255 || password.len() > 255 {
                bail!(tr("msg.proxy_sign_in"));
            }
            let mut hello = vec![1, user.len() as u8];
            hello.extend_from_slice(user);
            hello.push(password.len() as u8);
            hello.extend_from_slice(password);
            let sent = tcp.write_all(&hello);
            hello.iter_mut().for_each(|b| *b = 0);
            sent?;
            let mut status = [0u8; 2];
            tcp.read_exact(&mut status)?;
            if status[1] != 0 {
                bail!(tr("msg.proxy_sign_in"));
            }
        }
        _ => bail!(tr("msg.proxy_sign_in")),
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let mut request = vec![5, 1, 0];
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if bare.len() > 255 {
                bail!(trf("msg.sftp_lookup", &[("host", host.to_string())]));
            }
            request.push(3);
            request.push(bare.len() as u8);
            request.extend_from_slice(bare.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&request)?;
    let mut reply = [0u8; 4];
    tcp.read_exact(&mut reply)?;
    if reply[0] != 5 {
        bail!(tr("msg.proxy_not_socks"));
    }
    if reply[1] != 0 {
        let why = match reply[1] {
            2 => "connection not allowed by the proxy's rules",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "TTL expired",
            _ => "general failure",
        };
        bail!("SOCKS5: {why} ({})", reply[1]);
    }
    let rest = match reply[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len)?;
            len[0] as usize + 2
        }
        _ => bail!(tr("msg.proxy_not_socks")),
    };
    let mut bound = vec![0u8; rest];
    tcp.read_exact(&mut bound)?;
    Ok(())
}

/// The most a proxy's reply to `CONNECT` is read before giving up on it.
const MAX_HTTP_REPLY: usize = 16 << 10;

/// Asks the HTTP proxy at the other end of `tcp` to tunnel to `host` at `port` with `CONNECT`,
/// signing in with Basic authentication when the proxy has a user name. The reply is read a
/// byte at a time, so nothing the server sends through the tunnel right after is taken with it.
fn http_connect(tcp: &mut Handshake, proxy: &Proxy, host: &str, port: u16) -> Result<()> {
    let target = server(host, port);
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(user) = &proxy.user {
        let mut pair = format!("{user}:{}", proxy.password.as_deref().unwrap_or(""));
        let mut encoded = super::remote::encode_base64(pair.as_bytes());
        wipe(&mut pair);
        while !encoded.len().is_multiple_of(4) {
            encoded.push('=');
        }
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
        wipe(&mut encoded);
    }
    request.push_str("\r\n");
    let sent = tcp.write_all(request.as_bytes());
    wipe(&mut request);
    sent?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HTTP_REPLY || tcp.read(&mut byte)? == 0 {
            bail!(tr("msg.proxy_not_http"));
        }
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head);
    let status = status.lines().next().unwrap_or("");
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(200..=299) => Ok(()),
        // Some proxies, tinyproxy among them, refuse a sign-in with 401 rather than 407.
        Some(401 | 407) => bail!("{}: {status}", tr("msg.proxy_sign_in")),
        Some(_) => bail!("{status}"),
        None => bail!(tr("msg.proxy_not_http")),
    }
}

/// The two ends of a connection on this machine, what one sends the other reads: for a stream
/// that is not a socket (another program's input and output, an SSH channel) to stand in for
/// one. Only the connection made from the first end is taken, should another program connect
/// to the listening port meanwhile.
pub(crate) fn socket_pair() -> Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    let ours = TcpStream::connect(listener.local_addr()?)?;
    let from = ours.local_addr()?;
    listener.set_nonblocking(true)?;
    let until = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((theirs, at)) if at == from => {
                theirs.set_nonblocking(false)?;
                ours.set_nodelay(true)?;
                theirs.set_nodelay(true)?;
                return Ok((ours, theirs));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > until {
                    bail!("{e}");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// A connection carried by `command`'s standard input and output, run by the shell, as `ssh`
/// runs a `ProxyCommand`. The program is stopped once the connection is closed.
pub(crate) fn command_stream(command: &str) -> Result<TcpStream> {
    let (ours, theirs) = socket_pair()?;
    #[cfg(windows)]
    let mut shell = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // As written: `cmd` reads its own quotes, not the escaped ones `arg` would add. With
        // `/S` it takes off only the outer pair added here, so a command starting with a
        // quoted program path keeps its quotes.
        let mut c = std::process::Command::new("cmd");
        c.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{command}\""))
            .creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(windows))]
    let mut shell = {
        // `exec`, as `ssh` does: stopping the shell then stops the program itself.
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(format!("exec {command}"));
        c
    };
    let mut child = shell
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| trf("msg.ssh_proxy_command", &[("command", command.to_string())]))?;
    let (Some(mut to_child), Some(mut from_child)) = (child.stdin.take(), child.stdout.take())
    else {
        bail!(trf(
            "msg.ssh_proxy_command",
            &[("command", command.to_string())]
        ));
    };
    let mut reading = theirs.try_clone()?;
    let mut writing = theirs;
    std::thread::spawn(move || {
        pump(&mut reading, &mut to_child);
        drop(to_child);
        // The connection is closed: nothing the program still sends would be read.
        let _ = child.kill();
        let _ = child.wait();
    });
    std::thread::spawn(move || {
        pump(&mut from_child, &mut writing);
        let _ = writing.shutdown(std::net::Shutdown::Both);
    });
    Ok(ours)
}

/// Copies `from` to `to` until either ends, passing each read on at once. Not `io::copy`,
/// which on Linux splices a socket into a pipe and may hold data back until more arrives.
fn pump(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if to.write_all(&buf[..n]).and_then(|()| to.flush()).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Proxies for tests of what connects through them.
#[cfg(test)]
pub(crate) mod testing {
    use std::io::{Read, Write};
    use std::net::{IpAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A SOCKS5 proxy without sign-in, for any number of connections, each relayed to the
    /// address or name it asks for; with how many it carried.
    pub(crate) fn socks_relay() -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let carried = count.clone();
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                carried.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || relay_one(c));
            }
        });
        (port, count)
    }

    fn relay_one(mut c: TcpStream) -> std::io::Result<()> {
        let mut head = [0u8; 2];
        c.read_exact(&mut head)?;
        let mut methods = vec![0u8; head[1] as usize];
        c.read_exact(&mut methods)?;
        c.write_all(&[5, 0])?;
        let mut req = [0u8; 4];
        c.read_exact(&mut req)?;
        let host = match req[3] {
            1 => {
                let mut ip = [0u8; 4];
                c.read_exact(&mut ip)?;
                IpAddr::from(ip).to_string()
            }
            4 => {
                let mut ip = [0u8; 16];
                c.read_exact(&mut ip)?;
                IpAddr::from(ip).to_string()
            }
            _ => {
                let mut len = [0u8; 1];
                c.read_exact(&mut len)?;
                let mut name = vec![0u8; len[0] as usize];
                c.read_exact(&mut name)?;
                String::from_utf8_lossy(&name).into_owned()
            }
        };
        let mut port = [0u8; 2];
        c.read_exact(&mut port)?;
        let mut server = TcpStream::connect((host.as_str(), u16::from_be_bytes(port)))?;
        c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])?;
        let mut back = server.try_clone()?;
        let mut c2 = c.try_clone()?;
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut back, &mut c2);
            let _ = c2.shutdown(std::net::Shutdown::Write);
        });
        let _ = std::io::copy(&mut c, &mut server);
        let _ = server.shutdown(std::net::Shutdown::Write);
        Ok(())
    }

    /// The real proxies the Docker test environments start (`docker/servers.sh`): SOCKS5 on
    /// port 1080 and HTTP CONNECT on port 8889, and the same signing in as the user running
    /// the tests with `password` on ports 1081 and 8888.
    pub(crate) fn local_proxies(password: &str) -> Vec<super::Proxy> {
        let user = std::env::var("USER").unwrap();
        [
            "socks5://127.0.0.1:1080".to_string(),
            format!("socks5://{user}:{password}@127.0.0.1:1081"),
            "http://127.0.0.1:8889".to_string(),
            format!("http://{user}:{password}@127.0.0.1:8888"),
        ]
        .iter()
        .map(|p| super::Proxy::parse(p).unwrap())
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_are_read_with_their_kind_port_and_sign_in() {
        let p = Proxy::parse("socks5://ann:s%40cret@proxy.example:1081").unwrap();
        assert_eq!(p.kind, ProxyKind::Socks5);
        assert_eq!((p.host.as_str(), p.port), ("proxy.example", 1081));
        assert_eq!(p.user.as_deref(), Some("ann"));
        assert_eq!(p.password.as_deref(), Some("s@cret"));
        assert_eq!(p.url(), "socks5://ann@proxy.example:1081");
        assert!(!format!("{p:?}").contains("cret"));
        let p = Proxy::parse("http://[::1]").unwrap();
        assert_eq!(
            (p.kind, p.host.as_str(), p.port),
            (ProxyKind::Http, "::1", 8080)
        );
        assert_eq!(p.url(), "http://[::1]:8080");
        assert_eq!(Proxy::parse("socks5h://h").unwrap().port, 1080);
        for bad in [
            "h:1080",
            "ftp://h",
            "socks5://",
            "socks5://h:0",
            "http://h/path",
        ] {
            assert!(Proxy::parse(bad).is_err(), "{bad}");
        }
        for bad in [
            "socks5://ann:secret@h:0",
            "http://ann:secret%zz@h",
            "ann:secret@h:1080",
            "socks5:/ann:secret@h",
            "ftp://ann:secret@h",
        ] {
            let e = format!("{:#}", Proxy::parse(bad).unwrap_err());
            assert!(!e.contains("secret") && e.contains("ann:***@h"), "{e}");
        }
    }

    #[test]
    fn a_proxy_that_never_answers_is_given_up_on_once_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(2).collect();
            std::thread::sleep(Duration::from_secs(30));
            drop(held);
        });
        for url in ["socks5", "http"] {
            let proxy = Proxy::parse(&format!("{url}://127.0.0.1:{port}")).unwrap();
            let cancel = std::sync::Arc::new(AtomicBool::new(false));
            let setter = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                setter.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            let start = Instant::now();
            assert!(connect("example.com", 22, Some(&proxy), &cancel).is_err());
            assert!(start.elapsed() < Duration::from_secs(5), "{url}");
        }
        let never = AtomicBool::new(false);
        let e = connect("h\r\nX-Injected: 1", 80, None, &never).unwrap_err();
        assert!(format!("{e:#}").contains("\\r\\n"), "{e:#}");
    }

    /// A SOCKS5 proxy for one connection: checks the sign-in and the request, then relays to
    /// `to`.
    fn socks_proxy(to: SocketAddr, login: Option<(&'static str, &'static str)>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut head = [0u8; 2];
            c.read_exact(&mut head).unwrap();
            let mut methods = vec![0u8; head[1] as usize];
            c.read_exact(&mut methods).unwrap();
            match login {
                Some((user, password)) => {
                    assert!(methods.contains(&2));
                    c.write_all(&[5, 2]).unwrap();
                    let mut v = [0u8; 2];
                    c.read_exact(&mut v).unwrap();
                    let mut u = vec![0u8; v[1] as usize];
                    c.read_exact(&mut u).unwrap();
                    let mut l = [0u8; 1];
                    c.read_exact(&mut l).unwrap();
                    let mut p = vec![0u8; l[0] as usize];
                    c.read_exact(&mut p).unwrap();
                    let ok = u == user.as_bytes() && p == password.as_bytes();
                    c.write_all(&[1, if ok { 0 } else { 1 }]).unwrap();
                    if !ok {
                        return;
                    }
                }
                None => c.write_all(&[5, 0]).unwrap(),
            }
            let mut req = [0u8; 5];
            c.read_exact(&mut req).unwrap();
            assert_eq!(
                (req[0], req[1], req[3]),
                (5, 1, 3),
                "a name, looked up there"
            );
            let mut name = vec![0u8; req[4] as usize + 2];
            c.read_exact(&mut name).unwrap();
            assert_eq!(&name[..name.len() - 2], b"localhost");
            c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
            let mut server = TcpStream::connect(to).unwrap();
            let mut back = server.try_clone().unwrap();
            let mut c2 = c.try_clone().unwrap();
            std::thread::spawn(move || std::io::copy(&mut back, &mut c2));
            let _ = std::io::copy(&mut c, &mut server);
        });
        port
    }

    /// A server that greets each connection with `hello`, then echoes what it reads.
    fn greeter() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in listener.incoming() {
                let mut c = c.unwrap();
                std::thread::spawn(move || {
                    c.write_all(b"hello\n").unwrap();
                    let mut back = c.try_clone().unwrap();
                    let _ = std::io::copy(&mut c, &mut back);
                });
            }
        });
        at
    }

    fn greeted(mut tcp: TcpStream) {
        let mut hello = [0u8; 6];
        tcp.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"hello\n");
        tcp.write_all(b"ping").unwrap();
        let mut back = [0u8; 4];
        tcp.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"ping");
    }

    #[test]
    fn a_socks5_proxy_carries_the_connection_signing_in_when_it_asks() {
        let never = AtomicBool::new(false);
        let to = greeter();
        let proxy = Proxy::parse(&format!("socks5://127.0.0.1:{}", socks_proxy(to, None)));
        let tcp = connect("localhost", to.port(), Some(&proxy.unwrap()), &never).unwrap();
        greeted(tcp);

        let port = socks_proxy(to, Some(("ann", "pw")));
        let proxy = Proxy::parse(&format!("socks5://ann:pw@127.0.0.1:{port}")).unwrap();
        greeted(connect("localhost", to.port(), Some(&proxy), &never).unwrap());
        let port = socks_proxy(to, Some(("ann", "pw")));
        let proxy = Proxy::parse(&format!("socks5://ann:wrong@127.0.0.1:{port}")).unwrap();
        let e = connect("localhost", to.port(), Some(&proxy), &never).unwrap_err();
        assert!(format!("{e:#}").contains(tr("msg.proxy_sign_in")), "{e:#}");
    }

    /// An HTTP proxy for one connection, answering `CONNECT` with `status` (relaying to `to`
    /// when it is 200) once the request carries `auth`, if any.
    fn http_proxy(to: SocketAddr, status: &'static str, auth: Option<&'static str>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                c.read_exact(&mut b).unwrap();
                head.push(b[0]);
            }
            let head = String::from_utf8(head).unwrap();
            assert!(head.starts_with(&format!("CONNECT localhost:{} HTTP/1.1\r\n", to.port())));
            let status = match auth {
                Some(a) if !head.contains(&format!("Proxy-Authorization: Basic {a}\r\n")) => {
                    "407 Proxy Authentication Required"
                }
                _ => status,
            };
            c.write_all(format!("HTTP/1.1 {status}\r\nVia: test\r\n\r\n").as_bytes())
                .unwrap();
            if !status.starts_with("200") {
                return;
            }
            let mut server = TcpStream::connect(to).unwrap();
            let mut back = server.try_clone().unwrap();
            let mut c2 = c.try_clone().unwrap();
            std::thread::spawn(move || std::io::copy(&mut back, &mut c2));
            let _ = std::io::copy(&mut c, &mut server);
        });
        port
    }

    #[test]
    fn an_http_proxy_tunnels_the_connection_and_its_refusal_is_reported() {
        let never = AtomicBool::new(false);
        let to = greeter();
        let port = http_proxy(to, "200 Connection established", None);
        let proxy = Proxy::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        greeted(connect("localhost", to.port(), Some(&proxy), &never).unwrap());

        // `ann:pw` in base64.
        let port = http_proxy(to, "200 OK", Some("YW5uOnB3"));
        let proxy = Proxy::parse(&format!("http://ann:pw@127.0.0.1:{port}")).unwrap();
        greeted(connect("localhost", to.port(), Some(&proxy), &never).unwrap());
        let port = http_proxy(to, "200 OK", Some("YW5uOnB3"));
        let proxy = Proxy::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let e = connect("localhost", to.port(), Some(&proxy), &never).unwrap_err();
        assert!(format!("{e:#}").contains("407"), "{e:#}");
        let port = http_proxy(to, "401 Unauthorized", None);
        let proxy = Proxy::parse(&format!("http://ann:wrong@127.0.0.1:{port}")).unwrap();
        let e = format!(
            "{:#}",
            connect("localhost", to.port(), Some(&proxy), &never).unwrap_err()
        );
        assert!(
            e.contains(tr("msg.proxy_sign_in")) && e.contains("401"),
            "{e}"
        );
        let port = http_proxy(to, "403 Forbidden", None);
        let proxy = Proxy::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let e = connect("localhost", to.port(), Some(&proxy), &never).unwrap_err();
        assert!(format!("{e:#}").contains("403 Forbidden"), "{e:#}");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_carries_the_connection_as_a_proxy_command_does() {
        let tcp = command_stream("sh -c \"printf 'hello\\n'; exec cat\"").unwrap();
        tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).unwrap();
        greeted(tcp);
    }

    #[cfg(windows)]
    #[test]
    fn a_command_carries_the_connection_as_a_proxy_command_does() {
        // cmd reads the command as written, quotes included.
        let mut tcp =
            command_stream(r#"echo "hello there"& set /p proxied=& set proxied"#).unwrap();
        tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).unwrap();
        tcp.write_all(b"ping\r\n").unwrap();
        let mut out = String::new();
        tcp.read_to_string(&mut out).unwrap();
        assert_eq!(out, "\"hello there\"\r\nproxied=ping\r\n");
        // A program path in quotes, as one under `C:\Program Files` needs.
        let mut tcp = command_stream(r#""cmd.exe" /D /C echo quoted"#).unwrap();
        tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).unwrap();
        let mut out = String::new();
        tcp.read_to_string(&mut out).unwrap();
        assert_eq!(out, "quoted\r\n");
    }
}
