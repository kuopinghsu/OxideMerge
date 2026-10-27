//! Web page compare: fetching pages, extracting their visible text and linked resources, and
//! taking screenshots with an installed Chromium-based browser (Edge, Chrome, Chromium, Brave).

use crate::core::text::{self, TextEncoding};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::{Host, Url};

const PAGE_LIMIT: u64 = 50 << 20;
const RESOURCE_LIMIT: u64 = 20 << 20;
const MAX_RESOURCES: usize = 400;
const DOWNLOAD_THREADS: usize = 8;
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(90);
/// File name of the page itself in a resource tree.
pub const PAGE_FILE: &str = "index.html";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum WebMode {
    #[default]
    Screenshot,
    Html,
    Text,
    Resources,
}

impl WebMode {
    pub const ALL: [WebMode; 4] = [
        WebMode::Screenshot,
        WebMode::Html,
        WebMode::Text,
        WebMode::Resources,
    ];

    pub fn key(self) -> &'static str {
        match self {
            WebMode::Screenshot => "web.screenshot",
            WebMode::Html => "web.html",
            WebMode::Text => "web.text",
            WebMode::Resources => "web.resources",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    /// Address after redirects.
    pub url: Url,
    pub status: u16,
    pub html: String,
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub abort: AtomicBool,
}

pub fn is_url(s: &str) -> bool {
    let s = s.trim_start();
    let head = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// The address to load for a path typed by the user: web addresses as they are, local files as `file:` URLs.
pub fn location(p: &Path) -> Result<Url> {
    let s = p.to_string_lossy();
    if is_url(&s) {
        return Url::parse(s.trim()).with_context(|| format!("invalid address: {s}"));
    }
    let abs = std::path::absolute(p).with_context(|| p.display().to_string())?;
    if !abs.is_file() {
        bail!("{}: not found", p.display());
    }
    Url::from_file_path(&abs).map_err(|_| anyhow::anyhow!("{}: not a valid file path", p.display()))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        // `fetch` follows them itself, to check where each one leads.
        .max_redirects(0)
        .user_agent(concat!(
            "Mozilla/5.0 (compatible; OxideMerge/",
            env!("CARGO_PKG_VERSION"),
            ")"
        ))
        .build()
        .into()
}

/// Reads `r` to the end, failing once it gives more than `limit` bytes.
fn read_limited(r: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    r.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "larger than {} MB",
            limit >> 20
        )));
    }
    Ok(bytes)
}

struct Fetched {
    url: Url,
    status: u16,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

/// Whether an address is a public unicast one: not this computer, a private network, or a
/// range set aside (multicast, reserved, benchmarking), also when wrapped in an IPv6 address.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let embedded =
                |hi: u16, lo: u16| std::net::Ipv4Addr::from((hi as u32) << 16 | lo as u32);
            if let Some(v4) = v6.to_ipv4() {
                // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
                is_public_ip(IpAddr::V4(v4))
            } else if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                // NAT64
                is_public_ip(IpAddr::V4(embedded(s[6], s[7])))
            } else if s[0] == 0x2002 {
                // 6to4
                is_public_ip(IpAddr::V4(embedded(s[1], s[2])))
            } else {
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (s[0] & 0xfe00) == 0xfc00
                    || (s[0] & 0xffc0) == 0xfe80
                    || (s[0] & 0xffc0) == 0xfec0
                    || (s[0] == 0x64 && s[1] == 0xff9b))
            }
        }
    }
}

/// Whether `url` leads to this computer or a private network: `Some(true)` for a local file,
/// `localhost` or a private address, `Some(false)` for a public one, and `None` for a host name
/// with addresses of both kinds. A host name that cannot be looked up counts as public, as its
/// request fails anyway.
fn locality(url: &Url) -> Option<bool> {
    match url.host() {
        _ if url.scheme() == "file" => Some(true),
        Some(Host::Ipv4(ip)) => Some(!is_public_ip(ip.into())),
        Some(Host::Ipv6(ip)) => Some(!is_public_ip(ip.into())),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") {
                return Some(true);
            }
            let port = url.port_or_known_default().unwrap_or(80);
            let ips: Vec<IpAddr> = (d.as_str(), port)
                .to_socket_addrs()
                .map(|a| a.map(|a| a.ip()).collect())
                .unwrap_or_default();
            classify(&ips)
        }
        None => Some(true),
    }
}

/// [`locality`] of a host with these addresses.
fn classify(ips: &[IpAddr]) -> Option<bool> {
    match ips.iter().filter(|&&ip| !is_public_ip(ip)).count() {
        0 => Some(false),
        n if n == ips.len() => Some(true),
        _ => None,
    }
}

/// Whether `url` may lead to this computer or a private network, so that a web page may not
/// load it or be redirected there.
fn is_local(url: &Url) -> bool {
    locality(url) != Some(false)
}

/// Whether the page at `url` is on this computer or a private network, so that it may load
/// from there: every address of its host must be.
fn is_local_page(url: &Url) -> bool {
    locality(url) == Some(true)
}

/// Downloads `url`, following up to 10 redirects. Unless `local_ok`, a redirect to a local
/// file, this computer or a private network is refused.
fn fetch(agent: &ureq::Agent, url: &Url, limit: u64, local_ok: bool) -> Result<Fetched> {
    if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("{url}: not a local file"))?;
        if !path.is_file() {
            bail!("{}: not a file", path.display());
        }
        let file = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
        let bytes = read_limited(file, limit).with_context(|| path.display().to_string())?;
        return Ok(Fetched {
            url: url.clone(),
            status: 200,
            content_type: None,
            bytes,
        });
    }
    let mut final_url = url.clone();
    let mut resp = agent
        .get(final_url.as_str())
        .call()
        .with_context(|| url.to_string())?;
    let mut hops = 0;
    // A redirection without a usable Location is kept as the response.
    while resp.status().is_redirection() {
        let Some(next) = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .and_then(|l| final_url.join(l.trim()).ok())
        else {
            break;
        };
        if hops == 10 {
            bail!("{url}: too many redirects");
        }
        if !matches!(next.scheme(), "http" | "https") {
            bail!("{url}: redirected to {next}, which is not a web address");
        }
        if !local_ok && is_local(&next) {
            bail!("{url}: redirected to {next}, which is not followed from a public address");
        }
        hops += 1;
        final_url = next;
        resp = agent
            .get(final_url.as_str())
            .call()
            .with_context(|| final_url.to_string())?;
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let status = resp.status().as_u16();
    // The limit applies to the bytes as decompressed, which a small download can make huge.
    let reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let bytes = read_limited(reader, limit).with_context(|| url.to_string())?;
    Ok(Fetched {
        url: final_url,
        status,
        content_type,
        bytes,
    })
}

/// Charset named in a `Content-Type` header or a `<meta>` tag.
fn charset_of(s: &str) -> Option<&str> {
    let lower = s.to_ascii_lowercase();
    let i = lower.find("charset=")? + "charset=".len();
    let rest = &s[i..];
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Decodes a page as browsers do: a byte order mark first, then the charset of the
/// `Content-Type` header, then one declared in the page, then a guess from the bytes.
fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let bom = [&[0xEF, 0xBB, 0xBF][..], &[0xFF, 0xFE], &[0xFE, 0xFF]]
        .iter()
        .any(|b| bytes.starts_with(b));
    let declared = || {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
        // A page that can declare its charset in ASCII is not UTF-16.
        charset_of(&head)
            .and_then(TextEncoding::from_label)
            .map(|e| match e {
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => TextEncoding::Utf8,
                e => e,
            })
    };
    let forced = if bom {
        None
    } else {
        content_type
            .and_then(charset_of)
            .and_then(TextEncoding::from_label)
            .or_else(declared)
    };
    text::decode(bytes, forced).0
}

/// Downloads every page in parallel.
pub fn fetch_pages(urls: &[Url]) -> Result<Vec<Page>> {
    let agent = agent();
    std::thread::scope(|s| {
        let handles: Vec<_> = urls
            .iter()
            .map(|u| {
                let agent = agent.clone();
                s.spawn(move || -> Result<Page> {
                    let f = fetch(&agent, u, PAGE_LIMIT, is_local_page(u))?;
                    let html = decode_html(&f.bytes, f.content_type.as_deref());
                    Ok(Page {
                        url: f.url,
                        status: f.status,
                        html,
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| bail!("download thread panicked"))
            })
            .collect()
    })
}

// ---------- HTML scanning ----------

const SKIP_CONTENT: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "math", "head",
];
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "br",
    "caption",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "html",
    "legend",
    "li",
    "main",
    "nav",
    "ol",
    "option",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "title",
    "tr",
    "ul",
];

enum Token<'a> {
    Text(&'a str),
    /// Name (lower case), raw attribute text, closing tag.
    Tag(String, &'a str, bool),
}

/// Minimal HTML tokenizer: text runs and tags. Comments, doctypes and the contents of raw-text
/// elements (`script`, `style`, ...) and `head`/`svg` are skipped, apart from `<title>`.
fn tokens(html: &str) -> Vec<Token<'_>> {
    tokens_in(html, false)
}

/// [`tokens`]; `in_head` while scanning the contents of `<head>`, where a nested `<head>` is
/// an ordinary tag, so the scan never goes deeper.
fn tokens_in(html: &str, in_head: bool) -> Vec<Token<'_>> {
    let b = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let next = b.get(i + 1).copied().unwrap_or(0);
        if !(next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?') {
            i += 1;
            continue;
        }
        if text_start < i {
            out.push(Token::Text(&html[text_start..i]));
        }
        if lower[i..].starts_with("<!--") {
            i = lower[i + 4..]
                .find("-->")
                .map_or(b.len(), |j| i + 4 + j + 3);
            text_start = i;
            continue;
        }
        let end = tag_end(b, i + 1);
        let inner = &html[i + 1..end];
        i = (end + 1).min(b.len());
        text_start = i;
        if next == b'!' || next == b'?' {
            continue;
        }
        let closing = inner.starts_with('/');
        let body = inner.trim_start_matches('/');
        let name_len = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':'))
            .unwrap_or(body.len());
        let name = body[..name_len].to_ascii_lowercase();
        let attrs = &body[name_len..];
        let self_closing = attrs.trim_end().ends_with('/');
        out.push(Token::Tag(name.clone(), attrs, closing));
        let skip = SKIP_CONTENT.contains(&name.as_str()) && !(in_head && name == "head");
        if !closing && !self_closing && skip {
            if name == "head" {
                // Keep the title, which is visible in the browser's tab.
                let close = lower[i..].find("</head").map_or(b.len(), |j| i + j);
                if let Some(t) = lower[i..close].find("<title") {
                    let t = i + t;
                    let t_end = tag_end(b, t + 1) + 1;
                    let t_close = lower[t_end..close]
                        .find("</title")
                        .map_or(close, |j| t_end + j);
                    out.push(Token::Tag("title".into(), "", false));
                    out.push(Token::Text(&html[t_end.min(t_close)..t_close]));
                    out.push(Token::Tag("title".into(), "", true));
                }
                // Resources in <head> (stylesheets, scripts) are still reported as tags.
                for tok in tokens_in(&html[i..close], true) {
                    if let Token::Tag(n, a, false) = tok {
                        if n == "link" || n == "script" || n == "base" {
                            out.push(Token::Tag(n, a, false));
                        }
                    }
                }
                i = close;
            } else {
                let close = format!("</{name}");
                i = lower[i..].find(&close).map_or(b.len(), |j| i + j);
            }
            text_start = i;
        }
    }
    if text_start < b.len() {
        out.push(Token::Text(&html[text_start..]));
    }
    out
}

/// Index of the `>` closing the tag that starts before `from`, ignoring `>` inside quoted values.
fn tag_end(b: &[u8], from: usize) -> usize {
    let mut quote = 0u8;
    for (k, &c) in b.iter().enumerate().skip(from) {
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            return k;
        }
    }
    b.len()
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let b = attrs.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        let key = &attrs[start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            if key.is_empty() {
                i += 1;
            }
            continue;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
            let q = b[i];
            let s = i + 1;
            let e = attrs[s..].find(q as char).map_or(b.len(), |j| s + j);
            i = e + 1;
            &attrs[s..e]
        } else {
            let s = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            &attrs[s..i]
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
    }
    None
}

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" | "ensp" | "emsp" | "thinsp" => ' ',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "times" => '×',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "sect" => '§',
        "para" => '¶',
        "larr" => '←',
        "rarr" => '→',
        _ => return None,
    })
}

/// Decodes character references and turns every whitespace run into a single space.
fn push_text(out: &mut String, s: &str) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        push_collapsed(out, &rest[..amp]);
        let tail = &rest[amp + 1..];
        let semi = tail.find(';').filter(|&k| k <= 10);
        match semi.and_then(|k| entity(&tail[..k]).map(|c| (k, c))) {
            Some((k, c)) => {
                push_collapsed(out, c.encode_utf8(&mut [0; 4]));
                rest = &tail[k + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    push_collapsed(out, rest);
}

fn push_collapsed(out: &mut String, s: &str) {
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.ends_with([' ', '\n', '\t']) && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
}

/// The text a browser would show, one block element per line.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    for tok in tokens(html) {
        match tok {
            Token::Text(t) => push_text(&mut out, t),
            Token::Tag(name, _, closing) => {
                if BLOCK.contains(&name.as_str()) {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                } else if !closing && (name == "td" || name == "th") {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\t');
                }
            }
        }
    }
    let mut lines: Vec<&str> = Vec::new();
    for l in out.lines() {
        let l = l.trim_matches([' ', '\t']);
        if !l.is_empty() {
            lines.push(l);
        }
    }
    lines.join("\n")
}

/// Breaks minified HTML into one tag per line so that line-based diffs stay readable.
/// Lines that are already short are kept as they are.
pub fn format_html(html: &str) -> String {
    const LONG: usize = 300;
    let mut out = String::with_capacity(html.len() + html.len() / 16);
    for line in html.lines() {
        if line.len() <= LONG {
            out.push_str(line);
        } else {
            let mut prev = '\0';
            for c in line.chars() {
                if c == '<' && prev == '>' {
                    out.push('\n');
                }
                out.push(c);
                prev = c;
            }
        }
        out.push('\n');
    }
    out
}

/// Addresses of the scripts, stylesheets, icons, images, media and frames a page loads, in document order.
pub fn resources(html: &str, page: &Url) -> Vec<Url> {
    let mut base = page.clone();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut add = |base: &Url, raw: &str, out: &mut Vec<Url>| {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            return;
        }
        let Ok(mut u) = base.join(raw) else { return };
        if !matches!(u.scheme(), "http" | "https" | "file") {
            return;
        }
        u.set_fragment(None);
        if out.len() < MAX_RESOURCES && seen.insert(u.to_string()) {
            out.push(u);
        }
    };
    for tok in tokens(html) {
        let Token::Tag(name, attrs, false) = tok else {
            continue;
        };
        let a = |n| attr(attrs, n);
        match name.as_str() {
            "base" => {
                if let Some(u) = a("href").and_then(|h| page.join(h.trim()).ok()) {
                    base = u;
                }
            }
            "link" => {
                let rel = a("rel").unwrap_or_default().to_ascii_lowercase();
                if ["stylesheet", "icon", "preload", "manifest"]
                    .iter()
                    .any(|r| rel.contains(r))
                {
                    if let Some(h) = a("href") {
                        add(&base, h, &mut out);
                    }
                }
            }
            "script" | "img" | "iframe" | "frame" | "embed" | "audio" | "video" | "source"
            | "track" => {
                if let Some(s) = a("src") {
                    add(&base, s, &mut out);
                }
                if let Some(p) = a("poster") {
                    add(&base, p, &mut out);
                }
                if let Some(set) = a("srcset") {
                    for cand in set.split(',') {
                        if let Some(u) = cand.split_whitespace().next() {
                            add(&base, u, &mut out);
                        }
                    }
                }
            }
            "object" => {
                if let Some(d) = a("data") {
                    add(&base, d, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

fn sanitize(seg: &str) -> String {
    let mut s: String = seg
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"|?*\/"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.len() > 100 {
        let mut cut = 100;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4);
    if s.is_empty() || s == "." || s == ".." || reserved || s.ends_with(['.', ' ']) {
        s.insert(0, '_');
    }
    s
}

fn segments(u: &Url) -> Vec<String> {
    u.path_segments()
        .map(|s| s.map(str::to_string).collect())
        .unwrap_or_default()
}

/// The folder of each page that its resource tree is rooted at. Pages at the same depth use their
/// own folder, so `/v1/` and `/v2/` or two hosts serving the same paths line up. Web pages at
/// different depths use the site root instead, so `/` and `/docs/` share `/static/...`.
pub fn tree_bases(pages: &[Url]) -> Vec<Vec<String>> {
    let dirs: Vec<Vec<String>> = pages
        .iter()
        .map(|u| {
            let mut d = segments(u);
            d.pop();
            d
        })
        .collect();
    let same_depth = dirs.windows(2).all(|w| w[0].len() == w[1].len());
    dirs.into_iter()
        .zip(pages)
        .map(|(d, u)| {
            if same_depth || u.scheme() == "file" {
                d
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// Where a resource is stored in the resource tree. Same-site resources below `base` (see
/// [`tree_bases`]) keep their relative path; other same-site resources go to `_site/`, and
/// resources from other hosts to `_external/<host>/`.
pub fn resource_path(page: &Url, base: &[String], res: &Url) -> PathBuf {
    let mut res_segs = segments(res);
    let same_site = res.scheme() == page.scheme()
        && res.host_str() == page.host_str()
        && res.port() == page.port();
    let mut out = PathBuf::new();
    if same_site {
        if res_segs.len() > base.len() && res_segs.starts_with(base) {
            res_segs.drain(..base.len());
        } else {
            out.push("_site");
        }
    } else {
        out.push("_external");
        out.push(sanitize(res.host_str().unwrap_or("local")));
    }
    if res_segs.last().is_none_or(|s| s.is_empty()) {
        res_segs.pop();
        res_segs.push("index".into());
    }
    for s in res_segs.iter().filter(|s| !s.is_empty()) {
        let decoded = percent_decode(s);
        out.push(sanitize(&decoded));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What happened to the resources of the pages.
#[derive(Debug, Default, PartialEq)]
pub struct TreeOutcome {
    /// Resources that could not be downloaded or saved.
    pub failed: usize,
    /// Resources on this computer or a private network, named by a page that is not.
    pub skipped: usize,
}

/// Whether a page at `page` may load the resource at `res`: a web page loads no local files,
/// and only one on this computer or a private network (`page_local`) loads from there.
fn may_load(page: &Url, page_local: bool, res: &Url, local: &mut impl FnMut(&Url) -> bool) -> bool {
    match (page.scheme(), res.scheme()) {
        ("file", _) => true,
        (_, "file") => false,
        _ => page_local || !local(res),
    }
}

/// Saves each page with its resources into the matching root folder.
pub fn download_trees(pages: &[Page], roots: &[PathBuf], progress: &Progress) -> TreeOutcome {
    struct Job {
        side: usize,
        url: Url,
        path: PathBuf,
        local_ok: bool,
    }
    let mut jobs = Vec::new();
    let mut outcome = TreeOutcome::default();
    let mut hosts: HashMap<(String, String, Option<u16>), Option<bool>> = HashMap::new();
    let mut lookup = |u: &Url| {
        let key = (
            u.scheme().to_string(),
            u.host_str().unwrap_or_default().to_string(),
            u.port_or_known_default(),
        );
        *hosts.entry(key).or_insert_with(|| locality(u))
    };
    let bases = tree_bases(&pages.iter().map(|p| p.url.clone()).collect::<Vec<_>>());
    for (side, ((page, root), base)) in pages.iter().zip(roots).zip(&bases).enumerate() {
        if progress.abort.load(Ordering::Relaxed) {
            break;
        }
        if std::fs::write(root.join(PAGE_FILE), &page.html).is_err() {
            outcome.failed += 1;
        }
        let local_ok = lookup(&page.url) == Some(true);
        let mut used: HashSet<PathBuf> = HashSet::from([PathBuf::from(PAGE_FILE)]);
        for url in resources(&page.html, &page.url) {
            if progress.abort.load(Ordering::Relaxed) {
                break;
            }
            let mut local = |u: &Url| lookup(u) != Some(false);
            if !may_load(&page.url, local_ok, &url, &mut local) {
                outcome.skipped += 1;
                continue;
            }
            let mut path = resource_path(&page.url, base, &url);
            if !path.components().all(|c| matches!(c, Component::Normal(_))) {
                outcome.failed += 1;
                continue;
            }
            let mut k = 2;
            while used.contains(&path) {
                let name = path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                path.set_file_name(format!("{name}~{k}"));
                k += 1;
            }
            used.insert(path.clone());
            jobs.push(Job {
                side,
                url,
                path,
                local_ok,
            });
        }
    }
    progress.total.store(jobs.len(), Ordering::Relaxed);
    let threads = DOWNLOAD_THREADS.min(jobs.len());
    let failed = AtomicUsize::new(0);
    let agent = agent();
    let queue = Mutex::new(jobs.into_iter());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                if progress.abort.load(Ordering::Relaxed) {
                    break;
                }
                let Some(job) = queue.lock().ok().and_then(|mut q| q.next()) else {
                    break;
                };
                let dest = roots[job.side].join(&job.path);
                let ok = fetch(&agent, &job.url, RESOURCE_LIMIT, job.local_ok)
                    .ok()
                    .is_some_and(|f| {
                        dest.parent()
                            .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
                            && f.status < 400
                            && std::fs::write(&dest, &f.bytes).is_ok()
                    });
                if !ok {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
                progress.done.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    outcome.failed += failed.into_inner();
    outcome
}

// ---------- screenshots ----------

/// The configured browser, or the first Chromium-based browser found on this system.
pub fn find_browser(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        return Some(PathBuf::from(configured))
            .filter(|p| p.is_file())
            .or_else(|| which(configured));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(dir) = std::env::var_os(var).map(PathBuf::from) {
                candidates.push(dir.join(r"Microsoft\Edge\Application\msedge.exe"));
                candidates.push(dir.join(r"Google\Chrome\Application\chrome.exe"));
                candidates.push(dir.join(r"Chromium\Application\chrome.exe"));
                candidates.push(dir.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome",
            "Microsoft Edge",
            "Chromium",
            "Brave Browser",
        ] {
            candidates.push(PathBuf::from(format!(
                "/Applications/{app}.app/Contents/MacOS/{app}"
            )));
        }
    }
    candidates.into_iter().find(|p| p.is_file()).or_else(|| {
        [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "microsoft-edge-stable",
            "brave-browser",
        ]
        .iter()
        .find_map(|n| which(n))
    })
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// Renders `url` headlessly and saves a PNG of the top `width` x `height` pixels to `out`.
pub fn screenshot(browser: &Path, url: &Url, out: &Path, width: u32, height: u32) -> Result<()> {
    let profile = tempfile::tempdir()?;
    let mut child = std::process::Command::new(browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--mute-audio",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--virtual-time-budget=5000",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(format!("--window-size={width},{height}"))
        .arg(format!("--screenshot={}", out.display()))
        .arg(url.as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| browser.display().to_string())?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > SCREENSHOT_TIMEOUT {
            let _ = child.kill();
            bail!(
                "{url}: the browser did not finish within {} seconds",
                SCREENSHOT_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !out.is_file() {
        bail!("{url}: the browser did not produce a screenshot");
    }
    Ok(())
}

/// Takes all screenshots in parallel, saving them as `0.png`, `1.png`, ... in `dir`.
pub fn screenshots(
    browser: &Path,
    urls: &[Url],
    dir: &Path,
    width: u32,
    height: u32,
) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = (0..urls.len())
        .map(|i| dir.join(format!("{i}.png")))
        .collect();
    let results: Vec<Result<()>> = std::thread::scope(|s| {
        let handles: Vec<_> = urls
            .iter()
            .zip(&paths)
            .map(|(u, p)| s.spawn(move || screenshot(browser, u, p, width, height)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| bail!("screenshot thread panicked"))
            })
            .collect()
    });
    for r in results {
        r?;
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_urls() {
        assert!(is_url("https://example.com"));
        assert!(is_url("  HTTP://example.com/a"));
        assert!(!is_url("C:\\pages\\a.html"));
        assert!(!is_url("httpfoo"));
    }

    #[test]
    fn extracts_visible_text() {
        let html = r#"<!DOCTYPE html><html><head><title>Hi &amp; bye</title>
            <style>p { color: red }</style><script>var x = "<p>no</p>";</script></head>
            <body><!-- hidden --><h1>Head&nbsp;line</h1><p>One
            two</p><ul><li>A</li><li>B &#x263A;</li></ul>
            <table><tr><td>x</td><td>y</td></tr></table>a < b</body></html>"#;
        assert_eq!(
            html_to_text(html),
            "Hi & bye\nHead line\nOne two\nA\nB ☺\nx\ty\na < b"
        );
    }

    #[test]
    fn finds_resources() {
        let page = Url::parse("https://example.com/docs/page.html").unwrap();
        let html = r#"<head><link rel="stylesheet" href="css/site.css"><link rel=canonical href="/x">
            <script src='/js/app.js?v=2'></script></head>
            <body><img src="img/a.png" srcset="img/a@2x.png 2x, img/a@3x.png 3x">
            <img src="data:image/png;base64,AAAA"><a href="other.html">x</a>
            <script src="https://cdn.example.net/lib.js"></script><img src="img/a.png"></body>"#;
        let found: Vec<String> = resources(html, &page)
            .iter()
            .map(|u| u.to_string())
            .collect();
        assert_eq!(
            found,
            [
                "https://example.com/docs/css/site.css",
                "https://example.com/js/app.js?v=2",
                "https://example.com/docs/img/a.png",
                "https://example.com/docs/img/a@2x.png",
                "https://example.com/docs/img/a@3x.png",
                "https://cdn.example.net/lib.js",
            ]
        );
    }

    #[test]
    fn maps_resources_to_paths() {
        let page = Url::parse("https://example.com/docs/page.html").unwrap();
        let base = vec!["docs".to_string()];
        let p = |u: &str| resource_path(&page, &base, &Url::parse(u).unwrap());
        assert_eq!(
            p("https://example.com/docs/img/a.png"),
            Path::new("img").join("a.png")
        );
        assert_eq!(
            p("https://example.com/js/app.js?v=2"),
            Path::new("_site").join("js").join("app.js")
        );
        assert_eq!(
            p("https://cdn.example.net/lib/"),
            Path::new("_external")
                .join("cdn.example.net")
                .join("lib")
                .join("index")
        );
        assert_eq!(
            p("https://example.com/docs/a%20b/con.txt"),
            Path::new("a b").join("_con.txt")
        );
        // Decoded slashes and dots never lead out of the tree.
        for u in [
            "https://example.com/docs/%2Fetc%2Fpasswd",
            "https://example.com/docs/..%2F..%2Fx",
            "https://example.com/docs/%2E%2E/x",
            "https://example.com/docs/a%5C..%5C..%5Cx",
            "https://evil.example/%2F%2Fhost%2Fshare",
        ] {
            let path = p(u);
            assert!(
                path.components().all(|c| matches!(c, Component::Normal(_))),
                "{u}: {path:?}"
            );
        }
    }

    #[test]
    fn nested_heads_are_read_without_going_deeper() {
        let html = format!("{}<title>t</title>", "<head>".repeat(50_000));
        assert_eq!(html_to_text(&html).trim(), "t");
    }

    #[test]
    fn picks_tree_bases() {
        let urls =
            |list: &[&str]| -> Vec<Url> { list.iter().map(|u| Url::parse(u).unwrap()).collect() };
        let s = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
        assert_eq!(
            tree_bases(&urls(&["https://a.com/v1/", "https://a.com/v2/x.html"])),
            vec![s(&["v1"]), s(&["v2"])]
        );
        assert_eq!(
            tree_bases(&urls(&["https://a.com/", "https://a.com/learn/"])),
            vec![s(&[]), s(&[])]
        );
    }

    #[test]
    fn splits_minified_html() {
        let long = format!("<div>{}</div><p>x</p>", "a".repeat(400));
        assert_eq!(
            format_html(&long),
            format!("<div>{}</div>\n<p>x</p>\n", "a".repeat(400))
        );
        assert_eq!(format_html("<a></a>"), "<a></a>\n");
    }

    #[test]
    fn reads_charset() {
        assert_eq!(
            charset_of("text/html; charset=Shift_JIS"),
            Some("Shift_JIS")
        );
        assert_eq!(charset_of(r#"<meta charset="big5">"#), Some("big5"));
        assert_eq!(charset_of("text/html"), None);
    }

    #[test]
    fn decodes_pages_by_bom_header_declaration_then_content() {
        let latin1 = b"<p>caf\xe9</p>";
        let meta = b"<meta charset=\"iso-8859-1\"><p>caf\xe9</p>";
        let big5 = b"<meta charset=big5><p>\xa4\xa4\xa4\xe5</p>";
        let utf8 = "<p>café</p>".as_bytes();
        let bom = [&[0xEF, 0xBB, 0xBF], utf8].concat();
        let html = "text/html; charset=ISO-8859-1";
        assert_eq!(decode_html(utf8, None), "<p>café</p>");
        assert_eq!(decode_html(latin1, Some(html)), "<p>café</p>");
        assert!(decode_html(meta, None).ends_with("<p>café</p>"));
        assert!(decode_html(big5, None).ends_with("<p>中文</p>"));
        // The header outranks the page, and a byte order mark outranks both.
        assert!(decode_html(meta, Some("text/html; charset=utf-8")).ends_with("caf\u{FFFD}</p>"));
        assert_eq!(decode_html(&bom, Some(html)), "<p>café</p>");
        let claims_utf16 = "<meta charset=utf-16><p>café</p>";
        assert!(decode_html(claims_utf16.as_bytes(), None).ends_with("<p>café</p>"));
        // Unknown labels are ignored.
        assert_eq!(
            decode_html(utf8, Some("text/html; charset=nonsense")),
            "<p>café</p>"
        );
    }

    #[test]
    fn tells_local_and_private_addresses_from_public_ones() {
        let local = |s: &str| is_local(&Url::parse(s).unwrap());
        for s in [
            "http://127.0.0.1/",
            "http://10.1.2.3:8080/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://[::127.0.0.1]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[2002:7f00:1::]/",
            "http://[fec0::1]/",
            "http://[ff02::1]/",
            "http://224.0.0.1/",
            "http://240.0.0.1/",
            "http://198.18.0.1/",
            "http://192.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://localhost:3000/",
            "http://app.localhost./",
            "file:///tmp/a.html",
        ] {
            assert!(local(s), "{s}");
        }
        for s in [
            "http://93.184.215.14/",
            "https://8.8.8.8/",
            "http://[2606:4700::1111]/",
            "http://172.32.0.1/",
            "http://[64:ff9b::808:808]/",
            "http://[2002:808:808::]/",
        ] {
            assert!(!local(s), "{s}");
        }
        // a page is local only when every address of its host is; a resource when any is
        let page = |s: &str| is_local_page(&Url::parse(s).unwrap());
        assert!(page("http://localhost/") && page("file:///tmp/a.html"));
        assert!(page("http://192.168.1.1/") && !page("http://8.8.8.8/"));
        assert_eq!(
            locality(&Url::parse("http://8.8.8.8/").unwrap()),
            Some(false)
        );
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(classify(&[ip("1.2.3.4"), ip("10.0.0.1")]), None);
        assert_eq!(classify(&[ip("10.0.0.1"), ip("::1")]), Some(true));
        assert_eq!(classify(&[ip("1.2.3.4")]), Some(false));
        assert_eq!(classify(&[]), Some(false));
    }

    #[test]
    fn web_pages_load_no_local_files_nor_private_addresses() {
        let u = |s: &str| Url::parse(s).unwrap();
        let public = u("http://93.184.215.14/index.html");
        let lan = u("http://192.168.1.10/index.html");
        let file = u("file:///tmp/page.html");
        let may =
            |page: &Url, res: &str| may_load(page, is_local_page(page), &u(res), &mut is_local);
        assert!(may(&public, "http://8.8.8.8/a.js"));
        assert!(!may(&public, "http://127.0.0.1:631/"));
        assert!(!may(&public, "http://169.254.169.254/"));
        assert!(!may(&public, "file:///etc/passwd"));
        assert!(may(&lan, "http://192.168.1.11/a.css"));
        assert!(may(&lan, "http://8.8.8.8/a.js"));
        assert!(!may(&lan, "file:///etc/passwd"));
        assert!(may(&file, "file:///tmp/a.css"));
        assert!(may(&file, "http://127.0.0.1/a.css"));

        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let roots: Vec<PathBuf> = dirs.iter().map(|d| d.path().to_path_buf()).collect();
        let html = r#"<link rel=stylesheet href="http://127.0.0.1:9/a.css">
            <img src="file:///etc/hosts"><script src="http://[::1]:9/b.js"></script>"#;
        let page = |url: &Url| Page {
            url: url.clone(),
            status: 200,
            html: html.into(),
        };
        let outcome = download_trees(
            &[page(&public), page(&public)],
            &roots,
            &Progress::default(),
        );
        assert_eq!(
            outcome,
            TreeOutcome {
                failed: 0,
                skipped: 6
            }
        );
        assert!(roots[0].join(PAGE_FILE).is_file());
        assert_eq!(std::fs::read_dir(&roots[0]).unwrap().count(), 1);
    }

    /// Answers each request on a local port with `reply(path)`, one connection per request.
    fn serve(reply: fn(&str, u16) -> Vec<u8>) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap_or_default();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                    header.clear();
                }
                let _ = reader.get_mut().write_all(&reply(&path, port));
            }
        });
        port
    }

    /// 100,000 `0` characters, gzipped.
    fn gzip_of_zeros() -> Vec<u8> {
        let hex = "1f8b08000000000000ffedc13101000000c2a04aeb9fce1a1e4001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000af0685c24dc5a0860100";
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    #[test]
    fn fetches_follow_safe_redirects_and_report_failures() {
        let port = serve(|path, port| match path {
            "/page" => response(
                "200 OK",
                "Content-Type: text/html; charset=iso-8859-1\r\n",
                b"<p>caf\xe9</p>",
            ),
            "/moved" => response("301 Moved", "Location: /page\r\n", b""),
            "/loop" => response("302 Found", "Location: /loop\r\n", b""),
            "/nowhere" => response("302 Found", "", b"moved"),
            "/to-file" => response("302 Found", "Location: file:///etc/passwd\r\n", b""),
            "/to-self" => response(
                "302 Found",
                &format!("Location: http://127.0.0.1:{port}/page\r\n"),
                b"",
            ),
            "/missing" => response("404 Not Found", "", b"<h1>gone</h1>"),
            "/big" => response("200 OK", "", &[b'x'; 4096]),
            "/bomb" => response("200 OK", "Content-Encoding: gzip\r\n", &gzip_of_zeros()),
            _ => b"not http at all\r\n\r\n".to_vec(),
        });
        let agent = agent();
        let url = |p: &str| Url::parse(&format!("http://127.0.0.1:{port}{p}")).unwrap();
        let get = |p: &str, local_ok| fetch(&agent, &url(p), 1 << 20, local_ok);

        let f = get("/moved", true).unwrap();
        assert_eq!((f.url, f.status), (url("/page"), 200));
        assert_eq!(
            decode_html(&f.bytes, f.content_type.as_deref()),
            "<p>café</p>"
        );
        let f = get("/nowhere", true).unwrap();
        assert_eq!((f.status, f.bytes), (302, b"moved".to_vec()));
        let f = get("/missing", true).unwrap();
        assert_eq!((f.status, f.bytes), (404, b"<h1>gone</h1>".to_vec()));

        let err = |r: Result<Fetched>| format!("{:#}", r.err().expect("an error"));
        assert!(err(get("/loop", true)).contains("too many redirects"));
        assert!(err(get("/to-file", true)).contains("not a web address"));
        assert!(err(get("/to-self", false)).contains("not followed"));
        assert!(get("/to-self", true).is_ok());
        assert!(err(get("/garbage", true)).contains(url("/garbage").as_str()));
        assert!(err(fetch(&agent, &url("/big"), 1024, true)).contains(url("/big").as_str()));
        // 133 bytes that decompress to 100,000
        assert!(err(fetch(&agent, &url("/bomb"), 1024, true)).contains("larger than"));
        assert_eq!(
            fetch(&agent, &url("/bomb"), 1 << 20, true)
                .unwrap()
                .bytes
                .len(),
            100_000
        );

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let gone = Url::parse(&format!("http://{}/", closed.local_addr().unwrap())).unwrap();
        drop(closed);
        assert!(err(fetch(&agent, &gone, 1024, true)).contains(gone.as_str()));
    }

    #[test]
    fn local_pages_download_their_resources() {
        let port = serve(|path, _| match path {
            "/index.html" => response(
                "200 OK",
                "",
                b"<link rel=stylesheet href=site.css><img src=gone.png>",
            ),
            "/site.css" => response("200 OK", "", b"p {}"),
            _ => response("404 Not Found", "", b""),
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/index.html")).unwrap();
        let pages = fetch_pages(std::slice::from_ref(&url)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let roots = [dir.path().to_path_buf()];
        let outcome = download_trees(&pages, &roots, &Progress::default());
        assert_eq!(
            outcome,
            TreeOutcome {
                failed: 1,
                skipped: 0
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("site.css")).unwrap(),
            "p {}"
        );
    }
}
