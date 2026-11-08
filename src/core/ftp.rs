//! FTP and FTPS: the other protocol [`super::remote`] reads and writes files and folders on
//! another machine with. The client here knows only the few commands that needs, over a plain
//! connection or one secured with TLS.
//!
//! Plain FTP sends the password and every file as they are, readable by anyone on the network
//! in between; the connection dialog says so. FTPS checks the server's certificate the way a
//! web browser does, and one no certificate authority vouches for (a self-signed one, as many
//! FTP servers have) is trusted only once the user has checked its fingerprint, then pinned
//! like an SSH host key (see [`super::remote::check_pinned_cert`]). Either way the certificate
//! is checked before the password is sent. A server that asks for a client certificate is
//! given the one the connection names, if any.
//!
//! Data connections are passive by default (`EPSV`, then `PASV`), and go to the address the
//! control connection reached, whatever address a `PASV` reply names, so a server cannot point
//! a transfer at another machine; through a proxy, they go through it to the server's name.
//! In active mode (`EPRT`, then `PORT`) the server connects back instead, and only a
//! connection from the address the control connection reached is taken. Active mode does not
//! go through a proxy, nor through most NAT routers.

use super::remote::{
    check, check_pinned_cert, join, normalize, server, Auth, EntryKind, Listed, PartlyWritten,
    Protocol, RemoteConnection, RemoteEntry, RemoteFs, RemoteStamp, RemoteWrite, CHUNK,
    CONNECT_TIMEOUT,
};
use crate::i18n::{tr, trf};
use anyhow::{bail, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::cell::{RefCell, RefMut};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The longest line of a reply read, and the most lines of one.
const MAX_LINE: u64 = 8 << 10;
const MAX_LINES: usize = 1000;
/// The largest folder listing read.
const MAX_LISTING: u64 = 64 << 20;
/// The most symbolic links one listing looks up, as for SFTP.
const MAX_LINK_LOOKUPS: usize = 256;

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    /// Between taking a plain connection out and putting it back secured.
    Gone,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
            Stream::Gone => Err(std::io::ErrorKind::NotConnected.into()),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
            Stream::Gone => Err(std::io::ErrorKind::NotConnected.into()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
            Stream::Gone => Ok(()),
        }
    }
}

impl Stream {
    /// Ends the TLS session, if any, and closes the sending side, as the end of an upload is
    /// marked; then waits for the server to close its side.
    fn finish_upload(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => {
                s.shutdown(Shutdown::Write)?;
                drain(s);
            }
            Stream::Tls(s) => {
                s.conn.send_close_notify();
                while s.conn.wants_write() {
                    s.conn.write_tls(&mut s.sock)?;
                }
                s.sock.shutdown(Shutdown::Write)?;
                drain_tls(s);
            }
            Stream::Gone => {}
        }
        Ok(())
    }

    /// Reads a data connection: a TLS one closed without a `close_notify` (as many servers do)
    /// ends there too, setting `unclean`. Anyone in between can end a connection that way, so
    /// what came is then checked another way (its size, say).
    fn read_data(&mut self, buf: &mut [u8], unclean: &mut bool) -> std::io::Result<usize> {
        match self.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                *unclean = true;
                Ok(0)
            }
            other => other,
        }
    }
}

/// Reads and drops what the server still sends until it closes the connection. Closing a
/// connection with data unread (a TLS session ticket, say) resets it, and the server's system
/// may then throw away the end of an upload before the server has read it. The control
/// connection's reply says whether the upload arrived, so a failure here is left to it.
fn drain(sock: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let mut left = 1usize << 20;
    loop {
        match sock.read(&mut buf) {
            Ok(n) if n > 0 && n < left => left -= n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

/// [`drain`] for a TLS data connection, taking in what comes through the session on the way:
/// the server's session tickets, each good for resuming the session once, which the next data
/// connections need when the server requires them to resume the control connection's session.
/// An upload reads nothing else, so without this its ticket would be lost.
fn drain_tls(s: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>) {
    let mut buf = [0u8; 4096];
    let mut left = 1usize << 20;
    loop {
        match s.conn.read_tls(&mut s.sock) {
            Ok(n) if n > 0 && n < left => left -= n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            _ => return,
        }
        if s.conn.process_new_packets().is_err() {
            return;
        }
        while matches!(s.conn.reader().read(&mut buf), Ok(n) if n > 0) {}
    }
}

/// A server's reply: its code and text.
#[derive(Debug)]
struct Reply {
    code: u16,
    text: String,
}

impl std::fmt::Display for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.code, self.text.trim())
    }
}

/// What a TLS connection to the server needs: the configuration (shared by the control and
/// the data connections, so that a data connection resumes the control connection's TLS
/// session, as many servers require), the name to check the certificate for, why the
/// certificate was refused, when it was, and the session tickets at hand.
#[derive(Clone)]
struct Tls {
    config: Arc<rustls::ClientConfig>,
    name: ServerName<'static>,
    refused: Arc<Mutex<Option<anyhow::Error>>>,
    tickets: Arc<Tickets>,
}

/// The TLS sessions kept to resume, counting the TLS 1.3 tickets: each is good for one data
/// connection, and a server gives only a few (some none on a data connection the client only
/// sends on), so that a server that requires data connections to resume the session refuses
/// one once they are used up, until the control connection is made again.
#[derive(Debug)]
struct Tickets {
    store: rustls::client::ClientSessionMemoryCache,
    /// Whether the server gave any, and how many are left.
    count: Mutex<(bool, usize)>,
}

/// The most TLS 1.3 tickets [`rustls::client::ClientSessionMemoryCache`] keeps for a server.
const MAX_TICKETS: usize = 8;

impl Tickets {
    fn new() -> Self {
        Tickets {
            store: rustls::client::ClientSessionMemoryCache::new(4),
            count: Mutex::new((false, 0)),
        }
    }

    /// Whether the server gives tickets, and all were used.
    fn used_up(&self) -> bool {
        self.count.lock().is_ok_and(|c| c.0 && c.1 == 0)
    }
}

impl rustls::client::ClientSessionStore for Tickets {
    fn set_kx_hint(&self, name: ServerName<'static>, group: rustls::NamedGroup) {
        self.store.set_kx_hint(name, group)
    }

    fn kx_hint(&self, name: &ServerName<'_>) -> Option<rustls::NamedGroup> {
        self.store.kx_hint(name)
    }

    fn set_tls12_session(
        &self,
        name: ServerName<'static>,
        value: rustls::client::Tls12ClientSessionValue,
    ) {
        self.store.set_tls12_session(name, value)
    }

    fn tls12_session(
        &self,
        name: &ServerName<'_>,
    ) -> Option<rustls::client::Tls12ClientSessionValue> {
        self.store.tls12_session(name)
    }

    fn remove_tls12_session(&self, name: &ServerName<'static>) {
        self.store.remove_tls12_session(name)
    }

    fn insert_tls13_ticket(
        &self,
        name: ServerName<'static>,
        value: rustls::client::Tls13ClientSessionValue,
    ) {
        self.store.insert_tls13_ticket(name, value);
        if let Ok(mut c) = self.count.lock() {
            *c = (true, (c.1 + 1).min(MAX_TICKETS));
        }
    }

    fn take_tls13_ticket(
        &self,
        name: &ServerName<'static>,
    ) -> Option<rustls::client::Tls13ClientSessionValue> {
        let ticket = self.store.take_tls13_ticket(name);
        if ticket.is_some() {
            if let Ok(mut c) = self.count.lock() {
                c.1 = c.1.saturating_sub(1);
            }
        }
        ticket
    }
}

/// Accepts a certificate a certificate authority vouches for, or else the one the user pinned
/// for the server; otherwise remembers why not, for the handshake's error to say.
#[derive(Debug)]
struct Verifier {
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
    host: String,
    port: u16,
    refused: Arc<Mutex<Option<anyhow::Error>>>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        match self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => Ok(verified),
            Err(e) => match check_pinned_cert(&self.host, self.port, end_entity.as_ref()) {
                Ok(()) => Ok(ServerCertVerified::assertion()),
                Err(why) => {
                    if let Ok(mut refused) = self.refused.lock() {
                        *refused = Some(why);
                    }
                    Err(e)
                }
            },
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

fn tls_for(conn: &RemoteConnection) -> Result<Tls> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = Arc::new(rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    });
    let webpki =
        rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider.clone())
            .build()?;
    let refused = Arc::new(Mutex::new(None));
    let verifier = Verifier {
        webpki,
        host: conn.host.clone(),
        port: conn.port,
        refused: refused.clone(),
    };
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let mut config = match &conn.options.client_cert {
        None => builder.with_no_client_auth(),
        Some(cert) => {
            let (certs, key) = client_identity(cert, conn.options.client_key.as_deref())?;
            builder.with_client_auth_cert(certs, key).with_context(|| {
                trf(
                    "msg.ftps_client_cert_bad",
                    &[("file", cert.display().to_string())],
                )
            })?
        }
    };
    let name = ServerName::try_from(conn.host.clone())
        .with_context(|| trf("msg.sftp_lookup", &[("host", conn.host.clone())]))?;
    let tickets = Arc::new(Tickets::new());
    config.resumption = rustls::client::Resumption::store(tickets.clone());
    Ok(Tls {
        config: Arc::new(config),
        name,
        refused,
        tickets,
    })
}

/// The client certificate chain in the PEM file `cert`, and its private key, from `key` or
/// else from the same file.
fn client_identity(
    cert: &Path,
    key: Option<&Path>,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let unreadable = |file: &Path| {
        trf(
            "msg.ftps_client_cert_unreadable",
            &[("file", file.display().to_string())],
        )
    };
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|certs| certs.collect::<std::result::Result<Vec<_>, _>>())
        .with_context(|| unreadable(cert))?;
    if certs.is_empty() {
        bail!(unreadable(cert));
    }
    let key_file = key.unwrap_or(cert);
    let key = PrivateKeyDer::from_pem_file(key_file).with_context(|| unreadable(key_file))?;
    Ok((certs, key))
}

/// Secures `tcp` with TLS, finishing the handshake (and so the certificate check) before
/// anything else is sent.
fn handshake(
    tcp: TcpStream,
    tls: &Tls,
) -> Result<Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>> {
    let conn = rustls::ClientConnection::new(tls.config.clone(), tls.name.clone())?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    while s.conn.is_handshaking() {
        if let Err(e) = s.conn.complete_io(&mut s.sock) {
            if let Some(why) = tls.refused.lock().ok().and_then(|mut r| r.take()) {
                return Err(why);
            }
            return Err(anyhow::Error::from(e).context(tr("msg.ftps_handshake")));
        }
    }
    Ok(Box::new(s))
}

/// The control connection, and what was learned about the server on it.
struct Control {
    stream: BufReader<Stream>,
    tls: Option<Tls>,
    /// The address the control connection reached, that every data connection goes to (or,
    /// in active mode, comes from).
    peer: IpAddr,
    /// This end's address of the control connection, where active mode listens.
    local: IpAddr,
    /// The server's name, and the proxy the connections go through, if any: a passive data
    /// connection then goes through it too.
    host: String,
    proxy: Option<super::net::Proxy>,
    /// Whether the server connects back for each transfer.
    active: bool,
    /// Whether the server lists folders with `MLSD` and describes paths with `MLST`, rather
    /// than only the free-form `LIST`.
    mlst: bool,
    /// The folder a relative path is in.
    home: String,
    /// Replies still owed for transfers given up before they ended, read before the next
    /// command's.
    owed: usize,
    /// Whether a reply could not be read whole (it timed out, say), so that the next one read
    /// might be the late end of it rather than the answer to the next command.
    broken: bool,
}

impl Control {
    fn line(&mut self) -> Result<String> {
        let mut buf = Vec::new();
        let n = (&mut self.stream)
            .take(MAX_LINE)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            bail!(tr("msg.ftp_closed"));
        }
        if !buf.ends_with(b"\n") {
            bail!(tr("msg.ftp_bad_reply"));
        }
        Ok(String::from_utf8_lossy(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string())
    }

    fn reply(&mut self) -> Result<Reply> {
        let reply = self.read_reply();
        self.broken |= reply.is_err();
        reply
    }

    fn read_reply(&mut self) -> Result<Reply> {
        let first = self.line()?;
        let code: u16 = first
            .get(..3)
            .and_then(|c| c.parse().ok())
            .with_context(|| format!("{}: {first}", tr("msg.ftp_bad_reply")))?;
        let mut text = first.get(4..).unwrap_or("").to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            for _ in 0..MAX_LINES {
                let line = self.line()?;
                text.push('\n');
                if line.starts_with(&end) || line == code.to_string() {
                    text.push_str(line.get(4..).unwrap_or(""));
                    return Ok(Reply { code, text });
                }
                text.push_str(&line);
            }
            bail!(tr("msg.ftp_bad_reply"));
        }
        Ok(Reply { code, text })
    }

    fn cmd(&mut self, line: &str) -> Result<Reply> {
        if line.contains(['\r', '\n']) {
            bail!(tr("msg.ftp_bad_name"));
        }
        if self.broken {
            bail!(tr("msg.ftp_out_of_step"));
        }
        while self.owed > 0 {
            self.owed -= 1;
            self.reply()?;
        }
        let out = self.stream.get_mut();
        let sent = (|| {
            out.write_all(line.as_bytes())?;
            out.write_all(b"\r\n")?;
            out.flush()
        })();
        if let Err(e) = sent {
            self.broken = true;
            return Err(e.into());
        }
        self.reply()
    }

    /// Sends `line`, failing unless the reply's code is one of `ok`.
    fn expect(&mut self, line: &str, ok: &[u16]) -> Result<Reply> {
        let reply = self.cmd(line)?;
        if ok.contains(&reply.code) {
            Ok(reply)
        } else {
            bail!("{reply}")
        }
    }

    fn start_tls(&mut self) -> Result<()> {
        let tls = self.tls.clone().context("TLS")?;
        let old = std::mem::replace(&mut self.stream, BufReader::new(Stream::Gone));
        let Stream::Plain(tcp) = old.into_inner() else {
            bail!("TLS");
        };
        self.stream = BufReader::new(Stream::Tls(handshake(tcp, &tls)?));
        Ok(())
    }

    /// Opens a passive data connection, to the control connection's own address, or through
    /// the proxy to the server's name.
    fn open_data(&mut self, cancel: &AtomicBool) -> Result<TcpStream> {
        let epsv = self.cmd("EPSV")?;
        let port = if epsv.code == 229 {
            epsv_port(&epsv.text).with_context(|| epsv.to_string())?
        } else {
            let pasv = self.expect("PASV", &[227])?;
            pasv_port(&pasv.text).with_context(|| pasv.to_string())?
        };
        if let Some(proxy) = &self.proxy {
            return super::net::connect(&self.host, port, Some(proxy), cancel);
        }
        let tcp = TcpStream::connect_timeout(&SocketAddr::new(self.peer, port), CONNECT_TIMEOUT)?;
        tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        Ok(tcp)
    }

    /// Listens for the server to connect back, in active mode, and tells it where: with
    /// `EPRT`, or `PORT` for a server that does not know it.
    fn listen(&mut self) -> Result<TcpListener> {
        let listener = TcpListener::bind(SocketAddr::new(self.local, 0))?;
        let port = listener.local_addr()?.port();
        let (family, ip) = match self.local {
            IpAddr::V4(ip) => (1, ip.to_string()),
            IpAddr::V6(ip) => (2, ip.to_string()),
        };
        let eprt = self.cmd(&format!("EPRT |{family}|{ip}|{port}|"))?;
        match (eprt.code, self.local) {
            (200, _) => {}
            (500..=502, IpAddr::V4(ip)) => {
                let [a, b, c, d] = ip.octets();
                let (hi, lo) = (port >> 8, port & 0xff);
                self.expect(&format!("PORT {a},{b},{c},{d},{hi},{lo}"), &[200])?;
            }
            _ => bail!("{eprt}"),
        }
        Ok(listener)
    }

    /// The server's connection to `listener`, in active mode: only one from the address the
    /// control connection reached is taken, so another machine cannot send or take a file.
    fn accept(&self, listener: TcpListener, cancel: &AtomicBool) -> Result<TcpStream> {
        listener.set_nonblocking(true)?;
        let until = Instant::now() + CONNECT_TIMEOUT;
        loop {
            check(cancel)?;
            match listener.accept() {
                Ok((tcp, from)) if from.ip() == self.peer => {
                    tcp.set_nonblocking(false)?;
                    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
                    tcp.set_write_timeout(Some(CONNECT_TIMEOUT))?;
                    return Ok(tcp);
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > until {
                        bail!(tr("msg.ftp_active_timeout"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Starts the transfer `line` asks for over a new data connection, secured once the server
    /// has said it is ready (it starts its side of the TLS handshake only then).
    fn transfer(&mut self, line: &str, cancel: &AtomicBool) -> Result<Stream> {
        let (tcp, listener) = match self.active {
            true => (None, Some(self.listen()?)),
            false => (Some(self.open_data(cancel)?), None),
        };
        let reply = self.cmd(line)?;
        if reply.code != 125 && reply.code != 150 {
            bail!("{reply}");
        }
        let tcp = match (tcp, listener) {
            (Some(tcp), _) => tcp,
            (None, Some(listener)) => match self.accept(listener, cancel) {
                Ok(tcp) => tcp,
                Err(e) => {
                    self.owed += 1;
                    return Err(e);
                }
            },
            (None, None) => bail!("FTP"),
        };
        match self.tls.clone() {
            Some(tls) => match handshake(tcp, &tls) {
                Ok(s) => Ok(Stream::Tls(s)),
                Err(e) => {
                    self.owed += 1;
                    Err(e)
                }
            },
            None => Ok(Stream::Plain(tcp)),
        }
    }

    /// Reads the reply that ends a transfer.
    fn finish(&mut self) -> Result<()> {
        let reply = self.reply()?;
        if reply.code / 100 != 2 {
            bail!("{reply}");
        }
        Ok(())
    }

    /// `remote` described by `MLST`: `None` when the server says there is nothing there. A
    /// description not understood is an error, not "nothing there", which would let a file
    /// be written over.
    fn mlst(&mut self, remote: &str) -> Result<Option<Entry>> {
        let reply = self.cmd(&format!("MLST {remote}"))?;
        match reply.code {
            250 => match reply
                .text
                .lines()
                .find(|l| l.starts_with(' '))
                .and_then(|l| parse_mlst(l.trim_start()))
            {
                Some(entry) => Ok(Some(entry)),
                None => bail!("{}: {reply}", tr("msg.ftp_bad_reply")),
            },
            550 => Ok(None),
            _ => bail!("{reply}"),
        }
    }

    /// Whether `remote` is a folder, a file, or neither, without `MLST`.
    fn probe(&mut self, remote: &str) -> Result<Option<EntryKind>> {
        if self.cmd(&format!("CWD {remote}"))?.code == 250 {
            return Ok(Some(EntryKind::Dir));
        }
        let size = self.cmd(&format!("SIZE {remote}"))?;
        match size.code {
            213 => Ok(Some(EntryKind::File)),
            550 => Ok(None),
            _ => bail!("{size}"),
        }
    }
}

/// A file, folder or link in a listing.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    kind: Kind,
    stamp: Option<RemoteStamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
}

/// An open, signed-in FTP or FTPS connection.
pub(crate) struct Client {
    control: RefCell<Control>,
    /// To sign in again with, for fresh TLS session tickets (see [`Tickets`]).
    conn: RemoteConnection,
}

impl Client {
    /// Connects and signs in as `conn` says. Setting `cancel` stops it between steps. Fails
    /// with [`super::remote::UnknownHost`] for an FTPS server whose certificate no certificate
    /// authority vouches for and that was not trusted yet.
    pub fn connect(conn: &RemoteConnection, cancel: &AtomicBool) -> Result<Self> {
        Ok(Client {
            control: RefCell::new(Self::open(conn, cancel)?),
            conn: conn.clone(),
        })
    }

    /// Makes the control connection again before a transfer when the TLS session tickets the
    /// server gave are used up, so that the data connection can resume the session.
    fn fresh(&self, cancel: &AtomicBool) -> Result<()> {
        let used_up = self
            .control
            .borrow()
            .tls
            .as_ref()
            .is_some_and(|t| t.tickets.used_up());
        if used_up {
            let fresh = Self::open(&self.conn, cancel)?;
            let mut old = self.control.replace(fresh);
            let _ = old.stream.get_mut().write_all(b"QUIT\r\n");
        }
        Ok(())
    }

    /// A control connection signed in as `conn` says (see [`Self::connect`]).
    fn open(conn: &RemoteConnection, cancel: &AtomicBool) -> Result<Control> {
        let shown = server(&conn.host, conn.port);
        let Auth::Password(password) = &conn.auth else {
            bail!(tr("msg.ftp_password_only"));
        };
        let proxy = conn.options.proxy.clone();
        if conn.options.active && proxy.is_some() {
            bail!(tr("msg.ftp_active_proxy"));
        }
        let tls = match conn.protocol {
            Protocol::Ftps | Protocol::FtpsImplicit => Some(tls_for(conn)?),
            Protocol::Ftp | Protocol::Sftp => None,
        };
        let tcp = super::net::connect(&conn.host, conn.port, proxy.as_ref(), cancel)?;
        check(cancel)?;
        let mut c = Control {
            peer: tcp.peer_addr()?.ip(),
            local: tcp.local_addr()?.ip(),
            stream: BufReader::new(Stream::Plain(tcp)),
            tls,
            host: conn.host.clone(),
            proxy,
            active: conn.options.active,
            mlst: false,
            home: "/".to_string(),
            owed: 0,
            broken: false,
        };
        if conn.protocol == Protocol::FtpsImplicit {
            c.start_tls()?;
        }
        let hello = c.reply()?;
        if hello.code != 220 {
            bail!(trf(
                "msg.ftp_refused",
                &[("server", shown.clone()), ("reply", hello.to_string())]
            ));
        }
        if conn.protocol == Protocol::Ftps {
            let reply = c.cmd("AUTH TLS")?;
            if reply.code != 234 {
                bail!(trf(
                    "msg.ftps_unsupported",
                    &[("server", shown.clone()), ("reply", reply.to_string())]
                ));
            }
            c.start_tls()?;
        }
        check(cancel)?;
        let user = c.cmd(&format!("USER {}", conn.user))?;
        let signed_in = match user.code {
            230 => true,
            331 | 332 => {
                let mut line = format!("PASS {password}");
                let reply = c.cmd(&line);
                super::remote::wipe(&mut line);
                matches!(reply?.code, 202 | 230)
            }
            _ => false,
        };
        if !signed_in {
            bail!(trf("msg.sftp_password", &[("server", shown)]));
        }
        if c.tls.is_some() {
            c.expect("PBSZ 0", &[200])?;
            c.expect("PROT P", &[200])?;
        }
        c.expect("TYPE I", &[200])?;
        let feat = c.cmd("FEAT")?;
        let mut utf8 = false;
        if feat.code == 211 {
            for line in feat.text.lines().skip(1) {
                let line = line.trim().to_ascii_uppercase();
                c.mlst |= line.starts_with("MLST");
                utf8 |= line == "UTF8";
            }
        }
        if utf8 {
            c.cmd("OPTS UTF8 ON")?;
        }
        let pwd = c.cmd("PWD")?;
        if pwd.code == 257 {
            if let Some(home) = quoted(&pwd.text) {
                c.home = home;
            }
        }
        Ok(c)
    }

    fn listing(&self, remote: &str, cancel: &AtomicBool) -> Result<Vec<Entry>> {
        let cannot = || trf("msg.sftp_cannot_list", &[("path", remote.to_string())]);
        self.fresh(cancel).with_context(cannot)?;
        let mut c = self.control.borrow_mut();
        let mlsd = c.mlst;
        let line = if mlsd {
            format!("MLSD {remote}")
        } else {
            format!("LIST {remote}")
        };
        let mut data = c.transfer(&line, cancel).with_context(cannot)?;
        let mut text = Vec::new();
        let mut buf = vec![0u8; CHUNK];
        let mut unclean = false;
        let read = (|| -> Result<()> {
            loop {
                check(cancel)?;
                let n = data.read_data(&mut buf, &mut unclean)?;
                if n == 0 {
                    return Ok(());
                }
                if text.len() as u64 + n as u64 > MAX_LISTING {
                    bail!(tr("msg.ftp_bad_reply"));
                }
                text.extend_from_slice(&buf[..n]);
            }
        })();
        drop(data);
        if read.is_err() {
            c.owed += 1;
        } else {
            c.finish().with_context(cannot)?;
        }
        read.with_context(cannot)?;
        if unclean {
            // A line cut short could name another file: `notes` for `notes.txt`.
            text.truncate(text.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
        }
        Ok(String::from_utf8_lossy(&text)
            .lines()
            .filter_map(|line| {
                let line = line.trim_end_matches('\r');
                if mlsd {
                    parse_mlsd(line)
                } else {
                    parse_list(line)
                }
            })
            .filter(|e| !e.name.is_empty() && e.name != "." && e.name != "..")
            .collect())
    }

    fn file_stamp(c: &mut Control, remote: &str) -> Result<Option<RemoteStamp>> {
        if c.mlst {
            return Ok(c.mlst(remote)?.map(|e| e.stamp.unwrap_or_default()));
        }
        let size = c.cmd(&format!("SIZE {remote}"))?;
        let size = match size.code {
            213 => size.text.trim().parse().unwrap_or(0),
            550 => {
                return Ok((c.probe(remote)? == Some(EntryKind::Dir)).then(RemoteStamp::default));
            }
            _ => bail!("{size}"),
        };
        let mdtm = c.cmd(&format!("MDTM {remote}"))?;
        let modified = if mdtm.code == 213 {
            parse_time(mdtm.text.trim()).unwrap_or(0)
        } else {
            0
        };
        Ok(Some(RemoteStamp::new(size, modified)))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(mut c) = self.control.try_borrow_mut() {
            let _ = c.stream.get_mut().write_all(b"QUIT\r\n");
        }
    }
}

/// A file being read over a data connection; reading its end reads the transfer's reply.
struct Download<'a> {
    data: Stream,
    control: RefMut<'a, Control>,
    done: bool,
    remote: String,
    read: u64,
    unclean: bool,
}

impl Download<'_> {
    /// After a TLS data connection that ended without a `close_notify`: fails unless the
    /// server, asked, gives the size that came (or cannot say).
    fn check_size(&mut self) -> Result<()> {
        let size = self.control.cmd(&format!("SIZE {}", self.remote))?;
        match size.text.trim().parse::<u64>() {
            Ok(n) if size.code == 213 && n != self.read => bail!(tr("msg.ftp_truncated")),
            _ => Ok(()),
        }
    }
}

impl Read for Download<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        let n = self.data.read_data(buf, &mut self.unclean)?;
        if n == 0 {
            self.done = true;
            self.data = Stream::Gone;
            self.control.finish().map_err(std::io::Error::other)?;
            if self.unclean {
                self.check_size().map_err(std::io::Error::other)?;
            }
        }
        self.read += n as u64;
        Ok(n)
    }
}

impl Drop for Download<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.data = Stream::Gone;
            self.control.owed += 1;
        }
    }
}

impl RemoteFs for Client {
    fn open_read(&self, remote: &str) -> Result<Box<dyn Read + '_>> {
        let cannot = || trf("msg.sftp_cannot_read", &[("path", remote.to_string())]);
        let never = AtomicBool::new(false);
        self.fresh(&never).with_context(cannot)?;
        let mut control = self.control.borrow_mut();
        let data = control
            .transfer(&format!("RETR {remote}"), &never)
            .with_context(cannot)?;
        Ok(Box::new(Download {
            data,
            control,
            done: false,
            remote: remote.to_string(),
            read: 0,
            unclean: false,
        }))
    }

    /// A symbolic link is neither a file nor a folder here, as for SFTP.
    fn list_dir(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, bool, bool)>> {
        Ok(self
            .list_dir_stamped(remote, limit, cancel)?
            .into_iter()
            .map(|e| (e.name, e.is_dir, e.is_file))
            .collect())
    }

    fn real_path(&self, remote: &str) -> Result<String> {
        let home = self.control.borrow().home.clone();
        Ok(normalize(&join(&home, remote)))
    }

    fn kind(&self, remote: &str) -> Result<Option<EntryKind>> {
        let mut c = self.control.borrow_mut();
        if c.mlst {
            match c.mlst(remote)? {
                Some(Entry {
                    kind: Kind::Dir, ..
                }) => return Ok(Some(EntryKind::Dir)),
                Some(Entry {
                    kind: Kind::File, ..
                }) => return Ok(Some(EntryKind::File)),
                Some(_) => {}
                None => bail!(trf("msg.sftp_cannot_read", &[("path", remote.to_string())])),
            }
        }
        match c.probe(remote)? {
            Some(kind) => Ok(Some(kind)),
            None => bail!(trf("msg.sftp_cannot_read", &[("path", remote.to_string())])),
        }
    }

    fn list_entries(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<RemoteEntry>> {
        let mut lookups = 0;
        let mut out = Vec::new();
        for e in self.listing(remote, cancel)? {
            let kind = match e.kind {
                Kind::Dir => EntryKind::Dir,
                Kind::File => EntryKind::File,
                Kind::Link if lookups == MAX_LINK_LOOKUPS => EntryKind::Link,
                Kind::Link => {
                    lookups += 1;
                    check(cancel)?;
                    match self.kind(&join(remote, &e.name)) {
                        Ok(Some(kind)) => kind,
                        _ => continue,
                    }
                }
            };
            out.push(RemoteEntry {
                name: e.name,
                kind,
                size: e.stamp.map_or(0, |s| s.size),
            });
            if out.len() > limit {
                break;
            }
        }
        Ok(out)
    }

    fn stamp(&self, remote: &str) -> Result<Option<RemoteStamp>> {
        Self::file_stamp(&mut self.control.borrow_mut(), remote)
            .with_context(|| trf("msg.sftp_cannot_read", &[("path", remote.to_string())]))
    }

    fn list_dir_stamped(
        &self,
        remote: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Listed>> {
        let mut out: Vec<Listed> = self
            .listing(remote, cancel)?
            .into_iter()
            .map(|e| Listed {
                name: e.name,
                is_dir: e.kind == Kind::Dir,
                is_file: e.kind == Kind::File,
                stamp: e.stamp,
            })
            .collect();
        out.truncate(limit.saturating_add(1));
        Ok(out)
    }
}

impl RemoteWrite for Client {
    /// Writes over an existing file in place, which keeps its permissions on the server. FTP
    /// has no way to create a file only if it is not there, so a file created by someone else
    /// between the check and the write is written over. A failed or cancelled upload may leave
    /// the file partly written; its error then carries [`PartlyWritten`].
    fn put(
        &self,
        local: &Path,
        remote: &str,
        expected: Option<RemoteStamp>,
        cancel: &AtomicBool,
    ) -> Result<RemoteStamp> {
        let cannot = || trf("msg.sftp_cannot_write", &[("path", remote.to_string())]);
        let now = self.stamp(remote)?;
        if expected.is_some() && now != expected {
            bail!(trf(
                "msg.sftp_changed_on_server",
                &[("path", remote.to_string())]
            ));
        }
        if expected.is_none() && now.is_some() {
            bail!(trf(
                "msg.remote_created_meanwhile",
                &[("path", remote.to_string())]
            ));
        }
        let mut from = std::fs::File::open(local).with_context(|| {
            trf(
                "msg.sftp_cannot_read",
                &[("path", local.display().to_string())],
            )
        })?;
        check(cancel)?;
        self.fresh(cancel).with_context(cannot)?;
        let started = self
            .control
            .borrow_mut()
            .transfer(&format!("STOR {remote}"), cancel);
        let mut data = match started {
            Ok(data) => data,
            // The server may have emptied the file before the data connection failed.
            Err(e) => {
                let stamp = self.stamp(remote).ok().flatten();
                let e = e.context(cannot());
                return Err(match stamp == now {
                    true => e,
                    false => e.context(PartlyWritten {
                        path: remote.to_string(),
                        stamp,
                    }),
                });
            }
        };
        let written = {
            let mut c = self.control.borrow_mut();
            let mut buf = vec![0u8; CHUNK];
            let sent = (|| -> Result<()> {
                loop {
                    check(cancel)?;
                    let n = from.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    data.write_all(&buf[..n])?;
                }
                data.finish_upload()?;
                Ok(())
            })();
            drop(data);
            let done = c.finish();
            sent.and(done)
        };
        if let Err(e) = written {
            let stamp = self.stamp(remote).ok().flatten();
            return Err(e.context(cannot()).context(PartlyWritten {
                path: remote.to_string(),
                stamp,
            }));
        }
        Ok(self.stamp(remote)?.unwrap_or_default())
    }

    fn make_dir(&self, remote: &str) -> Result<()> {
        self.control
            .borrow_mut()
            .expect(&format!("MKD {remote}"), &[257])
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))?;
        Ok(())
    }

    fn remove_file(&self, remote: &str) -> Result<()> {
        self.control
            .borrow_mut()
            .expect(&format!("DELE {remote}"), &[250])
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))?;
        Ok(())
    }

    fn remove_dir(&self, remote: &str) -> Result<()> {
        self.control
            .borrow_mut()
            .expect(&format!("RMD {remote}"), &[250])
            .with_context(|| trf("msg.sftp_cannot_write", &[("path", remote.to_string())]))?;
        Ok(())
    }
}

/// The path a `257` reply quotes, with `""` standing for one `"`.
fn quoted(text: &str) -> Option<String> {
    let rest = &text[text.find('"')? + 1..];
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if chars.peek() == Some(&'"') {
                chars.next();
            } else {
                return Some(out);
            }
        }
        out.push(c);
    }
    None
}

/// The port of an `EPSV` reply: `(|||port|)`, with any delimiter in place of `|`.
fn epsv_port(text: &str) -> Option<u16> {
    let inner = text.get(text.find('(')? + 1..text.rfind(')')?)?;
    let delim = inner.chars().next()?;
    inner.split(delim).nth(3)?.parse().ok()
}

/// The port of a `PASV` reply's `h1,h2,h3,h4,p1,p2`; its address is not used.
fn pasv_port(text: &str) -> Option<u16> {
    let from = text.find('(').map_or(0, |i| i + 1);
    let numbers: Vec<u16> = text[from..]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(6)
        .map(|s| s.parse().ok())
        .collect::<Option<_>>()?;
    let [.., p1, p2] = numbers[..] else {
        return None;
    };
    (numbers.len() == 6 && p1 < 256 && p2 < 256).then(|| p1 * 256 + p2)
}

/// A `YYYYMMDDHHMMSS` time, with or without fractions of a second, as seconds since 1970 (UTC).
fn parse_time(s: &str) -> Option<u64> {
    let t = chrono::NaiveDateTime::parse_from_str(s.get(..14)?, "%Y%m%d%H%M%S").ok()?;
    u64::try_from(t.and_utc().timestamp()).ok()
}

/// One line of an `MLSD` listing (or of an `MLST` reply): `fact=value;...; name`.
fn parse_mlsd(line: &str) -> Option<Entry> {
    let (facts, name) = line.split_once(' ')?;
    let name = name.rsplit('/').next().unwrap_or(name).to_string();
    let (mut kind, mut size, mut modified) = (None, None, None);
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        let value_lower = value.to_ascii_lowercase();
        match key.to_ascii_lowercase().as_str() {
            "type" => {
                kind = match value_lower.as_str() {
                    "file" => Some(Kind::File),
                    "dir" => Some(Kind::Dir),
                    "cdir" | "pdir" => return None,
                    v if v.starts_with("os.unix=slink") || v.starts_with("os.unix=symlink") => {
                        Some(Kind::Link)
                    }
                    _ => None,
                }
            }
            "size" => size = value.parse().ok(),
            "modify" => modified = parse_time(value),
            _ => {}
        }
    }
    let kind = kind?;
    Some(Entry {
        name,
        kind,
        stamp: (kind == Kind::File)
            .then(|| RemoteStamp::new(size.unwrap_or(0), modified.unwrap_or(0))),
    })
}

/// The line of an `MLST` reply describing the path asked about: as a line of an `MLSD`
/// listing, but where a folder may be `type=cdir`.
fn parse_mlst(line: &str) -> Option<Entry> {
    parse_mlsd(line).or_else(|| {
        let (facts, name) = line.split_once(' ')?;
        facts
            .split(';')
            .any(|f| f.eq_ignore_ascii_case("type=cdir") || f.eq_ignore_ascii_case("type=pdir"))
            .then(|| Entry {
                name: name.rsplit('/').next().unwrap_or(name).to_string(),
                kind: Kind::Dir,
                stamp: None,
            })
    })
}

/// One line of a `LIST` listing, in the Unix `ls -l` form most servers use or the DOS form
/// of IIS. Its time is too coarse to tell a change by, so it gives no stamp.
fn parse_list(line: &str) -> Option<Entry> {
    // The start of each of the first `n` space-separated fields, then of the rest.
    fn fields(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
        let mut out = Vec::new();
        let mut rest = line.trim_start();
        for _ in 0..n {
            let end = rest.find(' ')?;
            out.push(&rest[..end]);
            rest = rest[end..].trim_start();
        }
        Some((out, rest))
    }
    let first = line.chars().next()?;
    if first.is_ascii_digit() {
        let (f, name) = fields(line, 3)?;
        let kind = if f[2].eq_ignore_ascii_case("<DIR>") {
            Kind::Dir
        } else {
            Kind::File
        };
        return Some(Entry {
            name: name.to_string(),
            kind,
            stamp: None,
        });
    }
    let kind = match first {
        'd' => Kind::Dir,
        '-' => Kind::File,
        'l' => Kind::Link,
        _ => return None,
    };
    let (_, name) = fields(line, 8)?;
    let name = match kind {
        Kind::Link => name.split(" -> ").next().unwrap_or(name),
        _ => name,
    };
    Some(Entry {
        name: name.to_string(),
        kind,
        stamp: None,
    })
}

#[cfg(test)]
pub(crate) mod test_server;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_and_listings_are_read_as_servers_write_them() {
        assert_eq!(
            quoted(r#""/home/a ""b""" is the current directory"#).as_deref(),
            Some(r#"/home/a "b""#)
        );
        assert_eq!(
            epsv_port("Entering Extended Passive Mode (|||50123|)"),
            Some(50123)
        );
        assert_eq!(epsv_port("(!!!7!)"), Some(7));
        assert_eq!(epsv_port("x) (|||1|"), None);
        assert_eq!(epsv_port("()"), None);
        assert_eq!(
            pasv_port("Entering Passive Mode (10,0,0,9,195,149)."),
            Some(195 * 256 + 149)
        );
        assert_eq!(pasv_port("Entering Passive Mode 10,0,0,9,1,2"), Some(258));
        assert_eq!(pasv_port("(1,2,3)"), None);
        assert_eq!(parse_time("19700101000100"), Some(60));
        assert_eq!(parse_time("20260101120000.123"), Some(1_767_268_800));

        let e = parse_mlsd("type=file;size=12;modify=19700101000002;perm=r; a b.txt").unwrap();
        assert_eq!(e.name, "a b.txt");
        assert_eq!(e.kind, Kind::File);
        assert_eq!(e.stamp, Some(RemoteStamp::new(12, 2)));
        assert_eq!(
            parse_mlsd("Type=dir;Modify=20260101000000; sub")
                .unwrap()
                .kind,
            Kind::Dir
        );
        assert!(parse_mlsd("type=cdir; .").is_none());
        assert_eq!(parse_mlst("type=cdir; /srv/x").unwrap().kind, Kind::Dir);
        assert!(parse_mlst("size=3; /srv/x").is_none());
        assert_eq!(
            parse_mlsd("type=OS.unix=slink:/x;size=1; ln").unwrap().kind,
            Kind::Link
        );

        let e = parse_list("-rw-r--r--   1 ann  staff  1234 Jan  1 12:00 my file.txt").unwrap();
        assert_eq!((e.name.as_str(), e.kind), ("my file.txt", Kind::File));
        let e = parse_list("drwxr-xr-x 2 ann staff 64 Mar 3 2025 sub").unwrap();
        assert_eq!((e.name.as_str(), e.kind), ("sub", Kind::Dir));
        let e = parse_list("lrwxrwxrwx 1 ann staff 3 Jan 1 12:00 ln -> sub").unwrap();
        assert_eq!((e.name.as_str(), e.kind), ("ln", Kind::Link));
        assert!(parse_list("total 8").is_none());
        let e = parse_list("01-02-26  10:15AM       <DIR>          Old Stuff").unwrap();
        assert_eq!((e.name.as_str(), e.kind), ("Old Stuff", Kind::Dir));
        let e = parse_list("01-02-26  10:15AM                  42 notes.txt").unwrap();
        assert_eq!((e.name.as_str(), e.kind), ("notes.txt", Kind::File));
    }

    use super::super::remote;
    use super::test_server::{self as ts, Options, Security};
    use std::path::Path;

    fn conn(protocol: Protocol, port: u16) -> RemoteConnection {
        RemoteConnection {
            protocol,
            host: "127.0.0.1".into(),
            port,
            user: ts::USER.into(),
            auth: Auth::Password(ts::PASSWORD.into()),
            options: Default::default(),
        }
    }

    fn seed(root: &Path) {
        std::fs::create_dir_all(root.join("data/sub")).unwrap();
        std::fs::write(root.join("data/a.txt"), "alpha").unwrap();
        std::fs::write(root.join("data/sub/b.txt"), "beta").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", root.join("data/link")).unwrap();
    }

    fn read(path: impl AsRef<Path>) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn list(conn: &RemoteConnection, path: &str) -> remote::Listing {
        let b = remote::Browser::start(conn.clone());
        b.list(path);
        let start = std::time::Instant::now();
        loop {
            if let Some(listed) = b.poll() {
                return listed.unwrap();
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(20),
                "no listing"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Fetches, lists and writes back over `conn` to a server serving `root`.
    fn round_trip(conn: &RemoteConnection, root: &Path) {
        let never = AtomicBool::new(false);
        let f = remote::fetch(conn, "/data", &never).unwrap();
        assert!(f.is_dir);
        assert_eq!(read(f.local.join("a.txt")), "alpha");
        assert_eq!(read(f.local.join("sub/b.txt")), "beta");
        assert!(!f.local.join("link").exists(), "links are left out");
        let manifest = f.manifest.clone().unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.files["a.txt"].0.size, 5);

        let l = list(conn, "/data");
        let names: Vec<_> = l
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind))
            .collect();
        assert_eq!(names[0], ("sub", EntryKind::Dir));
        assert!(names.contains(&("a.txt", EntryKind::File)));
        // A link is listed as what it links to.
        #[cfg(unix)]
        assert!(names.contains(&("link", EntryKind::File)), "{names:?}");
        assert_eq!(list(conn, "/data/a.txt").file.as_deref(), Some("a.txt"));

        std::fs::write(f.local.join("a.txt"), "alpha, edited").unwrap();
        std::fs::remove_dir_all(f.local.join("sub")).unwrap();
        std::fs::create_dir_all(f.local.join("new")).unwrap();
        std::fs::write(f.local.join("new/c.txt"), "gamma").unwrap();
        let saved =
            crate::core::remote_sync::save_folder(conn, &f.local, "/data", &manifest, &never)
                .unwrap();
        assert!(saved.errors.is_empty(), "{:?}", saved.errors);
        assert_eq!((saved.uploaded, saved.deleted), (2, 2));
        let data = root.join("data");
        assert_eq!(read(data.join("a.txt")), "alpha, edited");
        assert_eq!(read(data.join("new/c.txt")), "gamma");
        assert!(!data.join("sub").exists());

        // One file, written back while unchanged on the server, and not once it changed there.
        let f = remote::fetch(conn, "/data/new/c.txt", &never).unwrap();
        assert!(!f.is_dir);
        assert_eq!(f.stamp.unwrap().size, 5);
        std::fs::write(&f.local, "gamma 2").unwrap();
        let stamp =
            remote::upload(conn, &f.local, "/data/new/c.txt", f.stamp, None, &never).unwrap();
        assert_eq!(read(data.join("new/c.txt")), "gamma 2");
        std::fs::write(data.join("new/c.txt"), "someone else's").unwrap();
        std::fs::write(&f.local, "gamma 3").unwrap();
        assert!(
            remote::upload(conn, &f.local, "/data/new/c.txt", Some(stamp), None, &never).is_err()
        );
        assert_eq!(read(data.join("new/c.txt")), "someone else's");
    }

    #[test]
    fn plain_ftp_fetches_lists_and_writes_back_with_and_without_mlsd_and_epsv() {
        for (mlst, epsv) in [(true, true), (false, false)] {
            let server = ts::start(Options {
                security: Security::Plain,
                mlst,
                epsv,
                ..Default::default()
            });
            seed(server.root.path());
            round_trip(&conn(Protocol::Ftp, server.port), server.root.path());
        }
    }

    #[test]
    fn active_mode_transfers_with_eprt_or_port_and_never_through_a_proxy() {
        for port_only in [false, true] {
            let server = ts::start(Options {
                security: Security::Plain,
                mlst: true,
                epsv: true,
                port_only,
                ..Default::default()
            });
            seed(server.root.path());
            let mut c = conn(Protocol::Ftp, server.port);
            c.options.active = true;
            round_trip(&c, server.root.path());
        }
        let mut c = conn(Protocol::Ftp, 21);
        c.options.active = true;
        c.options.proxy = Some(super::super::net::Proxy::parse("socks5://127.0.0.1:9").unwrap());
        let never = AtomicBool::new(false);
        let err = format!("{:#}", remote::fetch(&c, "/data", &never).err().unwrap());
        assert!(err.contains(tr("msg.ftp_active_proxy")), "{err}");
    }

    #[test]
    fn a_proxy_carries_the_control_and_every_data_connection() {
        let server = ts::start(Options {
            security: Security::Plain,
            mlst: true,
            epsv: true,
            ..Default::default()
        });
        seed(server.root.path());
        let (port, carried) = super::super::net::testing::socks_relay();
        let mut c = conn(Protocol::Ftp, server.port);
        c.options.proxy =
            Some(super::super::net::Proxy::parse(&format!("socks5://127.0.0.1:{port}")).unwrap());
        round_trip(&c, server.root.path());
        // Each connection opens a control connection, and each transfer a data connection.
        assert!(
            carried.load(std::sync::atomic::Ordering::SeqCst) > 4,
            "{carried:?}"
        );
    }

    /// Tests that set where trusted FTPS certificates are kept, one at a time.
    static CERTS_TESTS: Mutex<()> = Mutex::new(());

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn a_client_certificate_is_given_to_a_server_that_asks_for_one() {
        let _one = CERTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let certs = tempfile::tempdir().unwrap();
        let file = certs.path().join("known_certs");
        *remote::CERTS_PATH.lock().unwrap() = Some(file.clone());
        let never = AtomicBool::new(false);
        let server = ts::start(Options {
            security: Security::Explicit,
            mlst: true,
            epsv: true,
            client_ca: true,
            ..Default::default()
        });
        seed(server.root.path());
        let hex: String = ts::CERT.iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(&file, format!("127.0.0.1:{} {hex}\n", server.port)).unwrap();
        let mut c = conn(Protocol::Ftps, server.port);
        c.options.client_cert = Some(fixture("ftps-client-cert.pem"));
        c.options.client_key = Some(fixture("ftps-client-key.pem"));
        round_trip(&c, server.root.path());
        // The certificate and its key in one file.
        c.options.client_cert = Some(fixture("ftps-client-both.pem"));
        c.options.client_key = None;
        assert!(remote::fetch(&c, "/data/a.txt", &never).is_ok());

        // None, or one from an authority the server does not know, is refused.
        for cert in [None, Some(fixture("ftps-client-stranger.pem"))] {
            c.options.client_cert = cert;
            c.options.client_key = Some(fixture("ftps-client-key.pem"));
            assert!(remote::fetch(&c, "/data/a.txt", &never).is_err());
        }
        c.options.client_cert = Some(fixture("ftps-client-key.pem"));
        c.options.client_key = None;
        let err = format!("{:#}", remote::fetch(&c, "/data", &never).err().unwrap());
        assert!(err.contains("ftps-client-key.pem"), "{err}");
        *remote::CERTS_PATH.lock().unwrap() = None;
    }

    #[test]
    fn ftps_asks_to_trust_an_unknown_certificate_and_refuses_a_changed_one() {
        let _one = CERTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let certs = tempfile::tempdir().unwrap();
        let file = certs.path().join("known_certs");
        *remote::CERTS_PATH.lock().unwrap() = Some(file.clone());
        let never = AtomicBool::new(false);
        for (security, protocol) in [
            (Security::Explicit, Protocol::Ftps),
            (Security::Implicit, Protocol::FtpsImplicit),
        ] {
            let server = ts::start(Options {
                security,
                mlst: true,
                epsv: true,
                ..Default::default()
            });
            seed(server.root.path());
            let c = conn(protocol, server.port);
            let err = remote::fetch(&c, "/data", &never).err().unwrap();
            let host = err.downcast::<remote::UnknownHost>().unwrap();
            assert_eq!(host.key_type, tr("label.tls_certificate"));
            assert!(
                host.fingerprint.starts_with("SHA256:"),
                "{}",
                host.fingerprint
            );
            remote::trust_host(&host).unwrap();
            round_trip(&c, server.root.path());

            // The same server with another certificate is refused without asking.
            let pinned = std::fs::read_to_string(&file).unwrap();
            let hex: String = ts::CERT.iter().map(|b| format!("{b:02x}")).collect();
            assert!(pinned.contains(&hex));
            std::fs::write(&file, pinned.replace(&hex, "00ff")).unwrap();
            let err = remote::fetch(&c, "/data", &never).err().unwrap();
            assert!(err.downcast_ref::<remote::UnknownHost>().is_none());
            assert!(
                format!("{err:#}").contains(&file.display().to_string()),
                "{err:#}"
            );
            std::fs::write(&file, "").unwrap();
        }
        *remote::CERTS_PATH.lock().unwrap() = None;
    }

    #[test]
    fn tls_data_cut_short_without_close_notify_is_caught() {
        let _one = CERTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let certs = tempfile::tempdir().unwrap();
        let file = certs.path().join("known_certs");
        *remote::CERTS_PATH.lock().unwrap() = Some(file.clone());
        let never = AtomicBool::new(false);
        let hex: String = ts::CERT.iter().map(|b| format!("{b:02x}")).collect();
        for (cut, whole) in [(3, false), (10_000, true)] {
            let server = ts::start(Options {
                security: Security::Explicit,
                mlst: true,
                epsv: true,
                cut: Some(cut),
                ..Default::default()
            });
            seed(server.root.path());
            std::fs::write(&file, format!("127.0.0.1:{} {hex}\n", server.port)).unwrap();
            let c = conn(Protocol::Ftps, server.port);
            let fetched = remote::fetch(&c, "/data/a.txt", &never);
            match whole {
                true => assert_eq!(read(fetched.unwrap().local), "alpha"),
                false => {
                    let err = format!("{:#}", fetched.err().unwrap());
                    assert!(err.contains(tr("msg.ftp_truncated")), "{err}");
                }
            }
            // A listing has no size to check by, but its line cut short is left out.
            let f = remote::fetch(&c, "/data", &never).unwrap();
            let names = std::fs::read_dir(&f.local).unwrap().count();
            assert_eq!(names, if whole { 2 } else { 0 });
        }
        *remote::CERTS_PATH.lock().unwrap() = None;
    }

    #[test]
    fn wrong_sign_ins_and_servers_without_tls_are_refused() {
        let never = AtomicBool::new(false);
        let server = ts::start(Options {
            security: Security::Plain,
            mlst: true,
            epsv: true,
            ..Default::default()
        });
        seed(server.root.path());
        let mut c = conn(Protocol::Ftp, server.port);
        c.auth = Auth::Password("wrong".into());
        let err = format!("{:#}", remote::fetch(&c, "/data", &never).err().unwrap());
        assert!(err.contains(&format!("127.0.0.1:{}", server.port)), "{err}");
        c.auth = Auth::Agent;
        assert!(remote::fetch(&c, "/data", &never).is_err());

        // Asking a server without TLS for it never sends the password in the clear.
        let c = conn(Protocol::Ftps, server.port);
        let err = format!("{:#}", remote::fetch(&c, "/data", &never).err().unwrap());
        assert!(err.contains("502"), "{err}");

        let cancelled = AtomicBool::new(true);
        let c = conn(Protocol::Ftp, server.port);
        assert!(remote::fetch(&c, "/data", &cancelled).is_err());
    }

    // Against real servers on this machine, as the Docker test environments start them
    // (`docker/servers.sh`): vsftpd, which has no MLSD, on port 2121 and ProFTPD on port 2122,
    // both also offering explicit FTPS; ProFTPD with implicit FTPS on port 2123; and vsftpd
    // asking for a client certificate signed by `tests/fixtures/ftps-client-ca.der` on port
    // 2124. vsftpd serves `/tmp/ftptest/vsftpd` and ProFTPD `/tmp/ftptest/proftpd` to the user
    // running the tests, with the password in `OXIDEMERGE_TEST_PASSWORD`, and their
    // certificate is pinned in `/tmp/ftptest/known_certs`.
    fn local(protocol: Protocol, port: u16) -> (RemoteConnection, &'static Path) {
        let root = Path::new(match port {
            2121 | 2124 => "/tmp/ftptest/vsftpd",
            _ => "/tmp/ftptest/proftpd",
        });
        let _ = std::fs::remove_dir_all(root.join("data"));
        seed(root);
        let conn = RemoteConnection {
            protocol,
            host: "127.0.0.1".into(),
            port,
            user: std::env::var("USER").unwrap(),
            auth: Auth::Password(std::env::var("OXIDEMERGE_TEST_PASSWORD").unwrap()),
            options: Default::default(),
        };
        (conn, root)
    }

    fn local_certs() -> std::sync::MutexGuard<'static, ()> {
        let one = CERTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *remote::CERTS_PATH.lock().unwrap() = Some("/tmp/ftptest/known_certs".into());
        one
    }

    #[test]
    #[ignore]
    fn ftp_ftps_and_active_mode_against_local_vsftpd_and_proftpd() {
        let _one = local_certs();
        for (protocol, port) in [
            (Protocol::Ftp, 2121),
            (Protocol::Ftps, 2121),
            (Protocol::Ftp, 2122),
            (Protocol::Ftps, 2122),
            (Protocol::FtpsImplicit, 2123),
        ] {
            for active in [false, true] {
                eprintln!("{protocol:?} on port {port}, active mode {active}");
                let (mut c, root) = local(protocol, port);
                c.options.active = active;
                round_trip(&c, root);
            }
        }
        *remote::CERTS_PATH.lock().unwrap() = None;
    }

    #[test]
    #[ignore]
    fn a_client_certificate_is_given_to_a_local_vsftpd_that_asks_for_one() {
        let _one = local_certs();
        let never = AtomicBool::new(false);
        let (mut c, root) = local(Protocol::Ftps, 2124);
        c.options.client_cert = Some(fixture("ftps-client-cert.pem"));
        c.options.client_key = Some(fixture("ftps-client-key.pem"));
        round_trip(&c, root);
        for cert in [None, Some(fixture("ftps-client-stranger.pem"))] {
            c.options.client_cert = cert;
            assert!(remote::fetch(&c, "/data/a.txt", &never).is_err());
        }
        *remote::CERTS_PATH.lock().unwrap() = None;
    }

    // Through real proxies on this machine (`net::testing::local_proxies`) to the servers
    // above, and a wrong password refused by the proxies that ask for one.
    #[test]
    #[ignore]
    fn ftp_and_ftps_through_local_socks5_and_http_proxies() {
        let _one = local_certs();
        let never = AtomicBool::new(false);
        let password = std::env::var("OXIDEMERGE_TEST_PASSWORD").unwrap();
        for proxy in super::super::net::testing::local_proxies(&password) {
            for (protocol, port) in [(Protocol::Ftp, 2121), (Protocol::Ftps, 2122)] {
                eprintln!("{protocol:?} on port {port} through {proxy:?}");
                let (mut c, root) = local(protocol, port);
                c.options.proxy = Some(proxy.clone());
                round_trip(&c, root);
            }
        }
        let wrong = super::super::net::testing::local_proxies("wrong");
        for proxy in [&wrong[1], &wrong[3]] {
            let (mut c, _) = local(Protocol::Ftp, 2121);
            c.options.proxy = Some(proxy.clone());
            let err = format!("{:#}", remote::fetch(&c, "/data", &never).err().unwrap());
            assert!(err.contains(tr("msg.proxy_sign_in")), "{proxy:?}: {err}");
        }
        *remote::CERTS_PATH.lock().unwrap() = None;
    }
}
