//! A small Chrome DevTools Protocol client: starts a headless Chromium-based browser with remote
//! debugging on a local port and talks to it over a WebSocket. Only what web page compare needs:
//! open tabs, load pages, run scripts in them and capture screenshots.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Largest WebSocket message accepted (a full size screenshot is a large base64 string).
const MAX_MESSAGE: usize = 512 << 20;

/// What the reader thread of a [`Socket`] passes on.
enum Incoming {
    /// The WebSocket handshake succeeded.
    Open,
    Text(String),
    /// The connection failed or closed; the thread has ended.
    Closed(String),
}

/// A minimal WebSocket client (RFC 6455) for the browser's local DevTools endpoint.
///
/// A thread of its own reads the connection with plain blocking reads and passes messages on
/// through a channel, which is where waits time out. Socket read timeouts are never used: on
/// Windows a read that times out leaves the socket in an undefined state.
struct Socket {
    writer: Arc<Mutex<TcpStream>>,
    incoming: Receiver<Incoming>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Ends the reader thread's blocking read.
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(Shutdown::Both);
        }
    }
}

fn write_frame(w: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | opcode);
    let n = payload.len();
    if n < 126 {
        frame.push(0x80 | n as u8);
    } else if n <= 0xFFFF {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(n as u64).to_be_bytes());
    }
    // Client frames must be masked; the key need not be secret on a loopback connection.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x5a17_c3e1, |d| d.subsec_nanos());
    let key = nanos.to_le_bytes();
    frame.extend_from_slice(&key);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    w.write_all(&frame)
}

/// Reads the handshake answer, then message after message, until the connection ends.
fn read_loop(stream: TcpStream, writer: Arc<Mutex<TcpStream>>, tx: Sender<Incoming>) {
    let mut reader = BufReader::new(stream);
    let result = (|| -> Result<()> {
        let mut status = String::new();
        reader.read_line(&mut status)?;
        if !status.split_whitespace().nth(1).is_some_and(|c| c == "101") {
            bail!(
                "the browser refused the DevTools connection: {}",
                status.trim()
            );
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                bail!("the browser closed the DevTools connection");
            }
            if line.trim().is_empty() {
                break;
            }
        }
        if tx.send(Incoming::Open).is_err() {
            return Ok(());
        }
        let mut message: Vec<u8> = Vec::new();
        loop {
            let mut head = [0u8; 2];
            reader.read_exact(&mut head)?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0F;
            let masked = head[1] & 0x80 != 0;
            let mut len = (head[1] & 0x7F) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                reader.read_exact(&mut b)?;
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                reader.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if len as usize > MAX_MESSAGE || message.len() + len as usize > MAX_MESSAGE {
                bail!("DevTools message too large");
            }
            let mut mask = [0u8; 4];
            if masked {
                reader.read_exact(&mut mask)?;
            }
            let mut payload = vec![0u8; len as usize];
            reader.read_exact(&mut payload)?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match opcode {
                0x8 => bail!("the browser closed the DevTools connection"),
                0x9 => {
                    let mut w = writer.lock().map_err(|_| anyhow::anyhow!("lock"))?;
                    write_frame(&mut w, 0xA, &payload)?;
                }
                0xA => {}
                0x0..=0x2 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        let text = String::from_utf8_lossy(&message).into_owned();
                        message.clear();
                        if tx.send(Incoming::Text(text)).is_err() {
                            return Ok(());
                        }
                    }
                }
                _ => bail!("unexpected DevTools frame {opcode}"),
            }
        }
    })();
    let reason = match result {
        Ok(()) => "the DevTools connection was closed".to_string(),
        Err(e) => format!("{e:#}"),
    };
    let _ = tx.send(Incoming::Closed(reason));
}

impl Socket {
    fn connect(port: u16, path: &str, timeout: Duration) -> Result<Socket> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        let mut w = stream.try_clone()?;
        write!(
            w,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        )?;
        w.flush()?;
        let writer = Arc::new(Mutex::new(w));
        let (tx, incoming) = channel();
        let thread_writer = writer.clone();
        std::thread::Builder::new()
            .name("devtools".into())
            .spawn(move || read_loop(stream, thread_writer, tx))?;
        let socket = Socket { writer, incoming };
        match socket.incoming.recv_timeout(timeout) {
            Ok(Incoming::Open) => Ok(socket),
            Ok(Incoming::Closed(e)) => bail!("{e}"),
            Ok(Incoming::Text(_)) => bail!("unexpected DevTools message"),
            Err(_) => bail!("the browser did not answer the DevTools connection"),
        }
    }

    fn send_text(&mut self, text: &str) -> Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("DevTools connection"))?;
        write_frame(&mut w, 0x1, text.as_bytes()).context("DevTools connection")
    }
}

/// The DevTools connection over the pipe the browser is given on file descriptors 3 (what it
/// reads) and 4 (what it writes), messages ending in a NUL byte (`--remote-debugging-pipe`).
/// Unlike a port, nothing else on the computer can reach it.
#[cfg(unix)]
struct Pipe {
    writer: std::io::PipeWriter,
    incoming: Receiver<Incoming>,
}

#[cfg(unix)]
impl Pipe {
    /// Has `cmd` start with the pipe, and starts it.
    fn spawn(cmd: &mut Command) -> Result<(Child, Pipe)> {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let (browser_reads, writer) = std::io::pipe()?;
        let (reader, browser_writes) = std::io::pipe()?;
        let (r, w) = (browser_reads.as_raw_fd(), browser_writes.as_raw_fd());
        // SAFETY: between fork and exec only async-signal-safe calls (fcntl, dup2) are made.
        // The pipe ends are first copied above 4, so placing one on 3 can't overwrite the other.
        unsafe {
            cmd.pre_exec(move || {
                let high = |fd| match libc::fcntl(fd, libc::F_DUPFD, 10) {
                    -1 => Err(std::io::Error::last_os_error()),
                    fd => Ok(fd),
                };
                let (r, w) = (high(r)?, high(w)?);
                for (from, to) in [(r, 3), (w, 4)] {
                    // dup2 leaves the new descriptor open across exec.
                    if libc::dup2(from, to) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = cmd.arg("--remote-debugging-pipe").spawn()?;
        // The browser's ends are its own now; closing ours lets a read end when it exits.
        drop((browser_reads, browser_writes));
        let (tx, incoming) = channel();
        std::thread::Builder::new()
            .name("devtools".into())
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                let reason = loop {
                    let mut message = Vec::new();
                    match reader.read_until(0, &mut message) {
                        Ok(0) => break "the browser closed the DevTools connection".to_string(),
                        Ok(_) => {
                            if message.last() == Some(&0) {
                                message.pop();
                            }
                            let text = String::from_utf8_lossy(&message).into_owned();
                            if tx.send(Incoming::Text(text)).is_err() {
                                return;
                            }
                        }
                        Err(e) => break format!("DevTools connection: {e}"),
                    }
                };
                let _ = tx.send(Incoming::Closed(reason));
            })?;
        Ok((child, Pipe { writer, incoming }))
    }
}

/// How the DevTools messages travel.
enum Conn {
    Socket(Socket),
    #[cfg(unix)]
    Pipe(Pipe),
}

impl Conn {
    fn send_text(&mut self, text: &str) -> Result<()> {
        match self {
            Conn::Socket(s) => s.send_text(text),
            #[cfg(unix)]
            Conn::Pipe(p) => {
                p.writer.write_all(text.as_bytes())?;
                p.writer.write_all(&[0])?;
                Ok(())
            }
        }
    }

    /// Waits up to `timeout` for the next message. `Ok(None)` when none arrived in time.
    fn recv_text(&mut self, timeout: Duration) -> Result<Option<String>> {
        let incoming = match self {
            Conn::Socket(s) => &s.incoming,
            #[cfg(unix)]
            Conn::Pipe(p) => &p.incoming,
        };
        match incoming.recv_timeout(timeout) {
            Ok(Incoming::Text(text)) => Ok(Some(text)),
            Ok(Incoming::Closed(e)) => bail!("{e}"),
            Ok(Incoming::Open) | Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                bail!("the browser closed the DevTools connection")
            }
        }
    }
}

/// A headless browser process and the DevTools connection to it. The process is ended on drop.
pub struct Browser {
    child: Child,
    conn: Conn,
    next_id: u64,
    /// Events received while waiting for a reply, kept for `wait_event`.
    events: VecDeque<Value>,
    /// Which events are worth keeping; the rest (there can be very many) are dropped.
    keep: fn(&Value) -> bool,
    _profile: tempfile::TempDir,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.send(None, "Browser.close", json!({}));
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Browser {
    /// Starts `exe` headless, with a throwaway profile, and connects to it.
    pub fn launch(exe: &Path, keep: fn(&Value) -> bool) -> Result<Browser> {
        let profile = tempfile::tempdir()?;
        let mut cmd = Command::new(exe);
        if running_as_root() {
            // Chromium refuses to start its sandbox as root (as in containers).
            cmd.arg("--no-sandbox");
        }
        cmd.args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--mute-audio",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-background-networking",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
            "--disable-features=Translate,MediaRouter",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let (child, conn) = Browser::connect(exe, &mut cmd, profile.path())?;
        Ok(Browser {
            child,
            conn,
            next_id: 0,
            events: VecDeque::new(),
            keep,
            _profile: profile,
        })
    }

    /// Starts the browser and connects to it: over a pipe where there are pipes to hand it
    /// (macOS, Linux), else over a port on this computer only (Windows). Setting
    /// `OXIDEMERGE_DEVTOOLS_PORT` uses the port everywhere, to test it.
    fn connect(exe: &Path, cmd: &mut Command, profile: &Path) -> Result<(Child, Conn)> {
        #[cfg(unix)]
        if std::env::var_os("OXIDEMERGE_DEVTOOLS_PORT").is_none() {
            let (child, pipe) = Pipe::spawn(cmd).with_context(|| exe.display().to_string())?;
            return Ok((child, Conn::Pipe(pipe)));
        }
        let mut child = cmd
            .arg("--remote-debugging-port=0")
            .spawn()
            .with_context(|| exe.display().to_string())?;
        // The browser writes its port and WebSocket path to this file once it listens.
        let port_file = profile.join("DevToolsActivePort");
        let started = Instant::now();
        let (port, path) = loop {
            if let Ok(s) = std::fs::read_to_string(&port_file) {
                let mut lines = s.lines();
                if let (Some(Ok(port)), Some(path)) =
                    (lines.next().map(|p| p.trim().parse::<u16>()), lines.next())
                {
                    break (port, path.trim().to_string());
                }
            }
            if let Ok(Some(status)) = child.try_wait() {
                bail!("{}: the browser exited ({status})", exe.display());
            }
            if started.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                bail!("{}: the browser did not start", exe.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        match Socket::connect(port, &path, Duration::from_secs(10)) {
            Ok(s) => Ok((child, Conn::Socket(s))),
            Err(e) => {
                let _ = child.kill();
                Err(e)
            }
        }
    }

    fn send(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let mut msg = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.conn.send_text(&msg.to_string())?;
        Ok(self.next_id)
    }

    fn stash(&mut self, v: Value) {
        if v.get("method").is_some() && (self.keep)(&v) {
            self.events.push_back(v);
        }
    }

    /// Sends a command and waits for its result.
    pub fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.send(session, method, params)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{method}: the browser did not answer");
            }
            let Some(text) = self.conn.recv_text(left)? else {
                continue;
            };
            let v: Value = serde_json::from_str(&text).context("DevTools message")?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!(
                        "{method}: {}",
                        e.get("message").and_then(Value::as_str).unwrap_or("error")
                    );
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            self.stash(v);
        }
    }

    /// Removes and returns the first kept event matching `pred`, waiting until `deadline`.
    /// `Ok(None)` on timeout or when `abort` returns true.
    pub fn wait_event(
        &mut self,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
        pred: &dyn Fn(&Value) -> bool,
    ) -> Result<Option<Value>> {
        loop {
            if let Some(i) = self.events.iter().position(pred) {
                return Ok(self.events.remove(i));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || abort() {
                return Ok(None);
            }
            if let Some(text) = self.conn.recv_text(left.min(Duration::from_millis(200)))? {
                let v: Value = serde_json::from_str(&text).context("DevTools message")?;
                self.stash(v);
            }
        }
    }

    /// Forgets the kept events of one tab.
    pub fn clear_events(&mut self, session: &str) {
        self.events
            .retain(|e| e.get("sessionId").and_then(Value::as_str) != Some(session));
    }

    /// Kept events received so far, oldest first.
    pub fn events(&self) -> impl Iterator<Item = &Value> {
        self.events.iter()
    }

    /// Opens a new tab and returns its session id.
    pub fn new_tab(&mut self) -> Result<String> {
        let target = self.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
        let target_id = target["targetId"]
            .as_str()
            .context("Target.createTarget")?
            .to_string();
        let attached = self.call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        )?;
        Ok(attached["sessionId"]
            .as_str()
            .context("Target.attachToTarget")?
            .to_string())
    }

    /// Evaluates `expression` in the tab, awaiting a returned promise, and returns its value.
    pub fn eval(&mut self, session: &str, expression: &str) -> Result<Value> {
        let r = self.call(
            Some(session),
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("script error");
            bail!("{msg}");
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }
}

fn running_as_root() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2))
                == Some("0")
        })
}

/// Decodes standard base64 (as used for screenshots), ignoring whitespace.
pub fn base64_decode(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c).context("invalid base64")?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");
        assert!(base64_decode("a$b").is_err());
    }
}
