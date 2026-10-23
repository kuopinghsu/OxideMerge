//! Web page compare: fetching pages, extracting their visible text and linked resources, and
//! taking screenshots with an installed Chromium-based browser (Edge, Chrome, Chromium, Brave).

use crate::core::text::{self, TextEncoding};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

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
        .user_agent(concat!(
            "Mozilla/5.0 (compatible; OxideMerge/",
            env!("CARGO_PKG_VERSION"),
            ")"
        ))
        .build()
        .into()
}

struct Fetched {
    url: Url,
    status: u16,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

fn fetch(agent: &ureq::Agent, url: &Url, limit: u64) -> Result<Fetched> {
    if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("{url}: not a local file"))?;
        let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
        return Ok(Fetched {
            url: url.clone(),
            status: 200,
            content_type: None,
            bytes,
        });
    }
    use ureq::ResponseExt;
    let mut resp = agent
        .get(url.as_str())
        .call()
        .with_context(|| url.to_string())?;
    let final_url = Url::parse(&resp.get_uri().to_string()).unwrap_or_else(|_| url.clone());
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let status = resp.status().as_u16();
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .with_context(|| url.to_string())?;
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

fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let label = content_type
        .and_then(charset_of)
        .or_else(|| charset_of(&head));
    text::decode(bytes, label.and_then(TextEncoding::from_label)).0
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
                    let f = fetch(&agent, u, PAGE_LIMIT)?;
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
        if !closing && !self_closing && SKIP_CONTENT.contains(&name.as_str()) {
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
                for tok in tokens(&html[i..close]) {
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
            if c.is_control() || r#"<>:"|?*\"#.contains(c) {
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

/// Saves each page with its resources into the matching root folder. Returns how many resources failed.
pub fn download_trees(pages: &[Page], roots: &[PathBuf], progress: &Progress) -> usize {
    struct Job {
        side: usize,
        url: Url,
        path: PathBuf,
    }
    let mut jobs = Vec::new();
    let bases = tree_bases(&pages.iter().map(|p| p.url.clone()).collect::<Vec<_>>());
    for (side, ((page, root), base)) in pages.iter().zip(roots).zip(&bases).enumerate() {
        let _ = std::fs::write(root.join(PAGE_FILE), &page.html);
        let mut used: HashSet<PathBuf> = HashSet::from([PathBuf::from(PAGE_FILE)]);
        for url in resources(&page.html, &page.url) {
            let mut path = resource_path(&page.url, base, &url);
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
            jobs.push(Job { side, url, path });
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
                let ok = fetch(&agent, &job.url, RESOURCE_LIMIT)
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
    failed.into_inner()
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
}
