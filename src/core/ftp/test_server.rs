//! A small FTP and FTPS server over a local folder, for testing the client against: one
//! thread per connection, passive or active transfers, and only the commands the client sends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const CERT: &[u8] = include_bytes!("../../../tests/fixtures/ftps-test-cert.der");
const KEY: &[u8] = include_bytes!("../../../tests/fixtures/ftps-test-key.der");
/// The authority the client certificate `ftps-client-cert.pem` is signed by.
const CLIENT_CA: &[u8] = include_bytes!("../../../tests/fixtures/ftps-client-ca.der");

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Security {
    #[default]
    Plain,
    Explicit,
    Implicit,
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub security: Security,
    /// Whether `MLSD` and `MLST` are offered, or only `LIST`, `SIZE` and `MDTM`.
    pub mlst: bool,
    /// Whether `EPSV` is understood, or only `PASV`.
    pub epsv: bool,
    /// Whether active mode is offered only with `PORT`, not `EPRT`.
    pub port_only: bool,
    /// Whether a TLS client must present a certificate signed by [`CLIENT_CA`].
    pub client_ca: bool,
    /// Ends each download and listing after this many bytes, without a TLS `close_notify`,
    /// as someone in between could, yet still says it all went.
    pub cut: Option<usize>,
}

pub struct Server {
    pub port: u16,
    pub root: tempfile::TempDir,
}

pub const USER: &str = "ann";
pub const PASSWORD: &str = "s3cret";

pub fn start(options: Options) -> Server {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = root.path().to_path_buf();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = if options.client_ca {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CLIENT_CA.to_vec().into()).unwrap();
        let verifier =
            rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .unwrap();
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let tls = Arc::new(
        builder
            .with_single_cert(
                vec![CERT.to_vec().into()],
                rustls::pki_types::PrivateKeyDer::try_from(KEY.to_vec()).unwrap(),
            )
            .unwrap(),
    );
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let (dir, tls) = (dir.clone(), tls.clone());
            std::thread::spawn(move || {
                let _ = Session::serve(tcp, dir, options, tls);
            });
        }
    });
    Server { port, root }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
    Gone,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
            Stream::Gone => Ok(0),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
            Stream::Gone => Ok(buf.len()),
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

fn accept_tls(
    tcp: TcpStream,
    tls: &Arc<rustls::ServerConfig>,
) -> std::io::Result<Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>> {
    let conn = rustls::ServerConnection::new(tls.clone()).map_err(std::io::Error::other)?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock)?;
    }
    Ok(Box::new(s))
}

fn close(stream: Stream) -> std::io::Result<()> {
    match stream {
        Stream::Plain(s) => s.shutdown(Shutdown::Both),
        Stream::Tls(mut s) => {
            s.conn.send_close_notify();
            while s.conn.wants_write() {
                s.conn.write_tls(&mut s.sock)?;
            }
            s.sock.shutdown(Shutdown::Both)
        }
        Stream::Gone => Ok(()),
    }
}

/// Sends `bytes` over `data` and closes it, or only their first `cut` without a `close_notify`.
fn send_data(mut data: Stream, bytes: &[u8], cut: Option<usize>) -> std::io::Result<()> {
    let Some(cut) = cut else {
        data.write_all(bytes)?;
        return close(data);
    };
    data.write_all(&bytes[..cut.min(bytes.len())])?;
    data.flush()?;
    match data {
        Stream::Plain(s) => s.shutdown(Shutdown::Both),
        Stream::Tls(s) => s.sock.shutdown(Shutdown::Both),
        Stream::Gone => Ok(()),
    }
}

fn mtime(path: &Path) -> String {
    let t: chrono::DateTime<chrono::Utc> = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
        .into();
    t.format("%Y%m%d%H%M%S").to_string()
}

struct Session {
    control: BufReader<Stream>,
    root: PathBuf,
    options: Options,
    tls: Arc<rustls::ServerConfig>,
    signed_in: bool,
    user_ok: bool,
    protect: bool,
    passive: Option<TcpListener>,
    /// Where the client listens, in active mode, for the next transfer.
    active: Option<SocketAddr>,
}

impl Session {
    fn serve(
        tcp: TcpStream,
        root: PathBuf,
        options: Options,
        tls: Arc<rustls::ServerConfig>,
    ) -> std::io::Result<()> {
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        let stream = if options.security == Security::Implicit {
            Stream::Tls(accept_tls(tcp, &tls)?)
        } else {
            Stream::Plain(tcp)
        };
        let mut s = Session {
            control: BufReader::new(stream),
            root,
            options,
            tls,
            signed_in: false,
            user_ok: false,
            protect: false,
            passive: None,
            active: None,
        };
        s.send("220 test server ready")?;
        loop {
            let mut line = String::new();
            if s.control.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            if !s.handle(&cmd.to_ascii_uppercase(), arg)? {
                return Ok(());
            }
        }
    }

    fn send(&mut self, line: &str) -> std::io::Result<()> {
        let out = self.control.get_mut();
        out.write_all(line.as_bytes())?;
        out.write_all(b"\r\n")?;
        out.flush()
    }

    fn path(&self, arg: &str) -> Option<PathBuf> {
        let rel = arg.trim_start_matches('/');
        if rel.split('/').any(|p| p == "..") {
            return None;
        }
        Some(if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        })
    }

    fn data(&mut self) -> std::io::Result<Stream> {
        let tcp = match self.active.take() {
            Some(at) => TcpStream::connect_timeout(&at, Duration::from_secs(10))?,
            None => self.accept_passive()?,
        };
        tcp.set_nonblocking(false)?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(if self.protect {
            Stream::Tls(accept_tls(tcp, &self.tls)?)
        } else {
            Stream::Plain(tcp)
        })
    }

    fn accept_passive(&mut self) -> std::io::Result<TcpStream> {
        let listener = self
            .passive
            .take()
            .ok_or_else(|| std::io::Error::other("no PASV"))?;
        listener.set_nonblocking(true)?;
        let start = Instant::now();
        loop {
            match listener.accept() {
                Ok((tcp, _)) => return Ok(tcp),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(10) {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Where an `EPRT` (`|1|127.0.0.1|port|`) or `PORT` (`h1,h2,h3,h4,p1,p2`) argument says the
    /// client listens.
    fn active_address(cmd: &str, arg: &str) -> Option<SocketAddr> {
        if cmd == "EPRT" {
            let mut parts = arg.split('|');
            let (_, _, ip, port) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            return Some(SocketAddr::new(ip.parse().ok()?, port.parse().ok()?));
        }
        let n: Vec<u8> = arg
            .split(',')
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        let [a, b, c, d, hi, lo] = n[..] else {
            return None;
        };
        Some(SocketAddr::new(
            IpAddr::from([a, b, c, d]),
            u16::from(hi) << 8 | u16::from(lo),
        ))
    }

    fn fact_line(path: &Path, name: &str) -> Option<String> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let kind = if meta.file_type().is_symlink() {
            "OS.unix=slink:x".to_string()
        } else if meta.is_dir() {
            "dir".to_string()
        } else {
            "file".to_string()
        };
        Some(format!(
            "type={kind};size={};modify={}; {name}",
            meta.len(),
            mtime(path)
        ))
    }

    fn list_line(path: &Path, name: &str) -> Option<String> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let (first, size, suffix) = if meta.file_type().is_symlink() {
            ('l', 1, " -> somewhere".to_string())
        } else if meta.is_dir() {
            ('d', 0, String::new())
        } else {
            ('-', meta.len(), String::new())
        };
        Some(format!(
            "{first}rw-r--r--   1 ann  staff  {size} Jan  1 12:00 {name}{suffix}"
        ))
    }

    /// Handles one command; `false` once the session is over.
    fn handle(&mut self, cmd: &str, arg: &str) -> std::io::Result<bool> {
        let open = matches!(
            cmd,
            "USER" | "PASS" | "AUTH" | "QUIT" | "FEAT" | "PBSZ" | "PROT"
        );
        if !self.signed_in && !open {
            self.send("530 Please sign in")?;
            return Ok(true);
        }
        match cmd {
            "AUTH" if self.options.security == Security::Explicit => {
                self.send("234 Go ahead")?;
                let old = std::mem::replace(&mut self.control, BufReader::new(Stream::Gone));
                let Stream::Plain(tcp) = old.into_inner() else {
                    return Ok(false);
                };
                self.control = BufReader::new(Stream::Tls(accept_tls(tcp, &self.tls)?));
            }
            "AUTH" => self.send("502 No TLS here")?,
            "USER" => {
                self.user_ok = arg == USER;
                self.send("331 Password, please")?;
            }
            "PASS" => {
                if self.user_ok && arg == PASSWORD {
                    self.signed_in = true;
                    self.send("230 Signed in")?;
                } else {
                    self.send("530 Wrong user name or password")?;
                }
            }
            "PBSZ" => self.send("200 PBSZ=0")?,
            "PROT" => {
                self.protect = arg.eq_ignore_ascii_case("P");
                self.send("200 OK")?;
            }
            "TYPE" | "OPTS" => self.send("200 OK")?,
            "FEAT" => {
                let mut lines = vec!["211-Features:".to_string(), " UTF8".to_string()];
                if self.options.mlst {
                    lines.push(" MLST type*;size*;modify*;".to_string());
                }
                if self.options.epsv {
                    lines.push(" EPSV".to_string());
                }
                lines.push("211 End".to_string());
                self.send(&lines.join("\r\n"))?;
            }
            "PWD" => self.send("257 \"/\" is the current folder")?,
            "EPSV" if self.options.epsv => {
                let listener = TcpListener::bind("127.0.0.1:0")?;
                let port = listener.local_addr()?.port();
                self.passive = Some(listener);
                self.active = None;
                self.send(&format!("229 Entering Extended Passive Mode (|||{port}|)"))?;
            }
            "EPRT" | "PORT" if cmd == "PORT" || !self.options.port_only => {
                match Self::active_address(cmd, arg) {
                    Some(at) => {
                        self.passive = None;
                        self.active = Some(at);
                        self.send("200 OK")?;
                    }
                    None => self.send("501 Bad address")?,
                }
            }
            "PASV" => {
                let listener = TcpListener::bind("127.0.0.1:0")?;
                let port = listener.local_addr()?.port();
                self.passive = Some(listener);
                self.active = None;
                // A wrong address, as a server behind NAT names: the client must not use it.
                self.send(&format!(
                    "227 Entering Passive Mode (10,255,255,1,{},{})",
                    port / 256,
                    port % 256
                ))?;
            }
            "MLST" if self.options.mlst => match self.path(arg) {
                Some(p) if std::fs::symlink_metadata(&p).is_ok() => {
                    let line = Self::fact_line(&p, arg).unwrap_or_default();
                    self.send(&format!("250-Listing {arg}\r\n {line}\r\n250 End"))?;
                }
                _ => self.send("550 No such file or folder")?,
            },
            "MLSD" | "LIST" if cmd == "LIST" || self.options.mlst => {
                let Some(dir) = self.path(arg).filter(|p| p.is_dir()) else {
                    self.send("550 Not a folder")?;
                    return Ok(true);
                };
                let mut names: Vec<String> = std::fs::read_dir(&dir)?
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                let lines: Vec<String> = names
                    .iter()
                    .filter_map(|n| {
                        if cmd == "MLSD" {
                            Self::fact_line(&dir.join(n), n)
                        } else {
                            Self::list_line(&dir.join(n), n)
                        }
                    })
                    .collect();
                self.send("150 Here it comes")?;
                let data = self.data()?;
                let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
                send_data(data, text.as_bytes(), self.options.cut)?;
                self.send("226 Done")?;
            }
            "SIZE" => match self.path(arg).filter(|p| p.is_file()) {
                Some(p) => {
                    let n = std::fs::metadata(p)?.len();
                    self.send(&format!("213 {n}"))?;
                }
                None => self.send("550 Not a file")?,
            },
            "MDTM" => match self.path(arg).filter(|p| p.is_file()) {
                Some(p) => {
                    let t = mtime(&p);
                    self.send(&format!("213 {t}"))?;
                }
                None => self.send("550 Not a file")?,
            },
            "CWD" => match self.path(arg).filter(|p| p.is_dir()) {
                Some(_) => self.send("250 OK")?,
                None => self.send("550 Not a folder")?,
            },
            "RETR" => match self.path(arg).filter(|p| p.is_file()) {
                Some(p) => {
                    let bytes = std::fs::read(p)?;
                    self.send("150 Here it comes")?;
                    let data = self.data()?;
                    send_data(data, &bytes, self.options.cut)?;
                    self.send("226 Done")?;
                }
                None => self.send("550 Not a file")?,
            },
            "STOR" => match self.path(arg) {
                Some(p) => {
                    self.send("150 Send it")?;
                    let mut data = self.data()?;
                    let mut bytes = Vec::new();
                    match data.read_to_end(&mut bytes) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
                        Err(e) => return Err(e),
                    }
                    let _ = close(data);
                    std::fs::write(p, bytes)?;
                    self.send("226 Stored")?;
                }
                None => self.send("553 Bad name")?,
            },
            "MKD" => match self.path(arg).map(std::fs::create_dir) {
                Some(Ok(())) => self.send(&format!("257 \"{arg}\" created"))?,
                _ => self.send("550 Cannot create it")?,
            },
            "DELE" => match self.path(arg).map(std::fs::remove_file) {
                Some(Ok(())) => self.send("250 Deleted")?,
                _ => self.send("550 Cannot delete it")?,
            },
            "RMD" => match self.path(arg).map(std::fs::remove_dir) {
                Some(Ok(())) => self.send("250 Removed")?,
                _ => self.send("550 Cannot remove it")?,
            },
            "QUIT" => {
                self.send("221 Bye")?;
                return Ok(false);
            }
            _ => self.send("502 Not implemented")?,
        }
        Ok(true)
    }
}
