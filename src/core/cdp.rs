//! A small Chrome DevTools Protocol client: starts a headless Chromium-based browser with remote
//! debugging on a local port and talks to it over a WebSocket. Only what web page compare needs:
//! open tabs, load pages, run scripts in them and capture screenshots.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Largest WebSocket message accepted (a full size screenshot is a large base64 string).
const MAX_MESSAGE: usize = 512 << 20;

/// A minimal WebSocket client (RFC 6455) for the browser's local DevTools endpoint.
struct Socket {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Socket {
    fn connect(port: u16, path: &str, timeout: Duration) -> Result<Socket> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        let mut writer = stream.try_clone()?;
        write!(
            writer,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        )?;
        writer.flush()?;
        let mut reader = BufReader::new(stream);
        reader.get_ref().set_read_timeout(Some(timeout))?;
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
        Ok(Socket { reader, writer })
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
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
        self.writer.write_all(&frame)?;
        Ok(())
    }

    fn send_text(&mut self, text: &str) -> Result<()> {
        self.send_frame(0x1, text.as_bytes())
    }

    /// Waits up to `timeout` for the next text message. `Ok(None)` when none arrived in time.
    fn recv_text(&mut self, timeout: Duration) -> Result<Option<String>> {
        // Wait for the first byte with the caller's timeout; once a frame has started, its rest
        // is read with a generous timeout, so a timeout never leaves a half-read frame behind.
        let mut message: Vec<u8> = Vec::new();
        let mut first = true;
        loop {
            let wait = if first {
                timeout
            } else {
                Duration::from_secs(60)
            };
            self.reader
                .get_ref()
                .set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
            let mut head = [0u8; 2];
            match self.reader.read_exact(&mut head[..1]) {
                Ok(()) => {}
                Err(e)
                    if first
                        && matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(e).context("DevTools connection"),
            }
            self.reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(60)))?;
            self.reader.read_exact(&mut head[1..])?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0F;
            let masked = head[1] & 0x80 != 0;
            let mut len = (head[1] & 0x7F) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                self.reader.read_exact(&mut b)?;
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                self.reader.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if len as usize > MAX_MESSAGE || message.len() + len as usize > MAX_MESSAGE {
                bail!("DevTools message too large");
            }
            let mut mask = [0u8; 4];
            if masked {
                self.reader.read_exact(&mut mask)?;
            }
            let mut payload = vec![0u8; len as usize];
            self.reader.read_exact(&mut payload)?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match opcode {
                0x8 => bail!("the browser closed the DevTools connection"),
                0x9 => {
                    self.send_frame(0xA, &payload)?;
                    continue;
                }
                0xA => continue,
                0x0..=0x2 => {
                    first = false;
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(Some(String::from_utf8_lossy(&message).into_owned()));
                    }
                }
                _ => bail!("unexpected DevTools frame {opcode}"),
            }
        }
    }
}

/// A headless browser process and the DevTools connection to it. The process is ended on drop.
pub struct Browser {
    child: Child,
    socket: Socket,
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
        let mut child = cmd
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
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
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| exe.display().to_string())?;
        // The browser writes its port and WebSocket path to this file once it listens.
        let port_file = profile.path().join("DevToolsActivePort");
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
        let socket = match Socket::connect(port, &path, Duration::from_secs(10)) {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill();
                return Err(e);
            }
        };
        Ok(Browser {
            child,
            socket,
            next_id: 0,
            events: VecDeque::new(),
            keep,
            _profile: profile,
        })
    }

    fn send(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let mut msg = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.socket.send_text(&msg.to_string())?;
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
            let Some(text) = self.socket.recv_text(left)? else {
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
            if let Some(text) = self
                .socket
                .recv_text(left.min(Duration::from_millis(200)))?
            {
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
