//! Renders web pages in a headless browser (over the DevTools protocol) so they are compared as
//! the user sees them: after scripts have run, as visible text laid out in blocks, as the live
//! DOM, or as screenshots of the visible area or the whole page with text differences
//! highlighted on the page itself (like WinMerge's "View Differences").

use crate::core::cdp::{self, Browser};
use crate::core::web::{self, Page, Progress};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use similar::{Algorithm, DiffTag, TextDiff};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use url::Url;

/// Time allowed for all pages to load before they are used as they are.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);
/// Time allowed for a page that navigated on by itself (a redirect after loading) to load.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Tallest full size screenshot; taller pages are cut (larger images exceed GPU texture limits).
pub const MAX_SHOT_HEIGHT: u32 = 16384;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capture {
    None,
    /// The visible area (the configured width x height).
    Viewport,
    /// The whole page, up to `MAX_SHOT_HEIGHT` pixels tall.
    FullPage,
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub capture: Capture,
    /// Highlight the text differences between the pages on the screenshots.
    pub highlight: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Rendered {
    /// Address after redirects.
    pub url: Option<Url>,
    pub status: u16,
    /// Visible text, one block (paragraph, heading, list item, table cell, line...) per entry.
    pub blocks: Vec<String>,
    /// The DOM after scripts have run.
    pub html: String,
    /// PNG screenshot.
    pub shot: Option<Vec<u8>>,
    /// Whether the page was taller than the full size screenshot.
    pub cut: bool,
    /// Text blocks highlighted as different on the screenshot.
    pub marked: usize,
    /// Why the page could not be shown (fully); the others are shown all the same.
    pub error: Option<String>,
}

impl Rendered {
    /// The visible text, one block per line.
    pub fn text(&self) -> String {
        let mut s = self.blocks.join("\n");
        s.push('\n');
        s
    }
}

/// Collects the visible text of the page into blocks and remembers, for each block, which text
/// nodes it came from so differences can be highlighted later. Offsets are in UTF-16 units.
const EXTRACT_JS: &str = r#"(() => {
  const SKIP = new Set(['SCRIPT','STYLE','NOSCRIPT','TEMPLATE','HEAD','IFRAME','FRAME',
    'OBJECT','EMBED','CANVAS','VIDEO','AUDIO','SELECT','TEXTAREA','OPTION']);
  // Tag names are lower case in XHTML and other XML documents.
  const tag = el => el.nodeName.toUpperCase();
  // A lone UTF-16 surrogate can't be sent as JSON; U+FFFD keeps the offsets.
  const wf = s => s.toWellFormed ? s.toWellFormed() : s;
  const styles = new Map();
  const style = el => {
    let s = styles.get(el);
    if (!s) { const c = getComputedStyle(el); s = { d: c.display, w: c.whiteSpace, v: c.visibility }; styles.set(el, s); }
    return s;
  };
  const isBlock = el => {
    const d = style(el).d;
    return !(d.startsWith('inline') || d === 'contents' || d.startsWith('ruby'));
  };
  const parentOf = n => n.assignedSlot || n.parentElement || (n.parentNode && n.parentNode.host) || null;
  const blockOf = el => {
    while (parentOf(el) && el !== document.body && !isBlock(el)) el = parentOf(el);
    return el;
  };
  const blocks = [];
  let cur = null, curEl = null, brk = false;
  const flush = () => {
    if (cur) { cur.text = cur.text.replace(/ +$/, ''); if (cur.text) blocks.push(cur); }
    cur = null; curEl = null;
  };
  const start = el => { flush(); cur = { text: '', pieces: [] }; curEl = el; brk = false; };
  const space = () => { if (cur && cur.text && !cur.text.endsWith(' ')) cur.text += ' '; };
  const control = el => {
    if (tag(el) === 'SELECT') { const o = el.selectedOptions && el.selectedOptions[0]; return o ? o.text : ''; }
    if (tag(el) === 'TEXTAREA') return el.value;
    const t = (el.type || 'text').toLowerCase();
    if (['hidden','checkbox','radio','file','image','range','color'].includes(t)) return '';
    if (t === 'password') return el.value ? '••••' : '';
    return el.value || '';
  };
  const root = document.body || document.documentElement;
  if (!root) return { url: location.href, html: '', blocks: [] };
  // The tree as rendered: open shadow roots instead of their hosts' children, and the nodes
  // assigned to a slot instead of its fallback content.
  const children = n => {
    if (n.shadowRoot) return n.shadowRoot.childNodes;
    if (tag(n) === 'SLOT') { const a = n.assignedNodes(); if (a.length) return a; }
    return n.childNodes;
  };
  // An element without a box of its own (display: contents) still shows its children.
  const shown = el => style(el).d === 'contents' || !el.checkVisibility || el.checkVisibility();
  const hiddenAbove = el => {
    for (let a = el; a && a.nodeType === 1; a = parentOf(a)) {
      if (getComputedStyle(a).contentVisibility === 'hidden') return true;
      const up = parentOf(a);
      if (up && tag(up) === 'DETAILS' && !up.open && tag(a) !== 'SUMMARY') return true;
    }
    return false;
  };
  const stack = [root];
  while (stack.length) {
    const n = stack.pop();
    if (n.nodeType === 1) {
      const t = tag(n);
      if (t === 'BR') brk = true;
      else if (t === 'INPUT' || t === 'SELECT' || t === 'TEXTAREA') {
        if (!shown(n)) continue;
        const v = control(n).replace(/\s+/g, ' ').trim();
        if (v) { start(null); cur.text = v; flush(); }
      } else if (!SKIP.has(t) && shown(n)) {
        const c = children(n);
        for (let i = c.length - 1; i >= 0; i--) stack.push(c[i]);
      }
      continue;
    }
    if (n.nodeType !== 3) continue;
    const p = parentOf(n);
    if (!p || SKIP.has(tag(p))) continue;
    const st = style(p);
    if (st.v === 'hidden' || st.v === 'collapse') continue;
    // Text right inside a display: contents element was not checked with its element (it has
    // no box to check): look for what hides content above it.
    if (st.d === 'contents' && hiddenAbove(p)) continue;
    const data = n.data;
    if (!/\S/.test(data)) { space(); continue; }
    const b = blockOf(p);
    if (!cur || b !== curEl || brk) start(b);
    const pre = /^(pre|break-spaces)/.test(st.w) || st.w === 'pre-line';
    const re = /\S+|\s+/g;
    let m;
    while ((m = re.exec(data))) {
      if (/^\s/.test(m[0])) {
        if (pre && m[0].includes('\n')) start(b); else space();
      } else {
        if (!cur) start(b);
        cur.pieces.push([n, m.index, m.index + m[0].length, cur.text.length]);
        cur.text += m[0];
      }
    }
  }
  flush();
  window.__oxidemergeBlocks = blocks;
  const dt = document.doctype;
  const doctype = dt ? '<!DOCTYPE ' + dt.name + (dt.publicId ? ' PUBLIC "' + dt.publicId + '"' : '') +
    (dt.systemId ? (dt.publicId ? ' ' : ' SYSTEM ') + '"' + dt.systemId + '"' : '') + '>\n' : '';
  return {
    url: location.href,
    html: wf(doctype + document.documentElement.outerHTML),
    blocks: blocks.map(b => wf(b.text)),
  };
})()"#;

/// Highlights the given blocks (and character ranges within them) with the CSS Custom Highlight
/// API, which paints over the text without changing the page's layout.
const MARK_JS: &str = r#"((marks) => {
  const blocks = window.__oxidemergeBlocks || [];
  if (!window.Highlight || !window.CSS || !CSS.highlights) return 0;
  const roots = new Set([document]);
  // One range per text node: a block may run into a shadow tree, which a range can't span.
  const addRanges = (hl, b, s, e) => {
    const spans = [];
    for (const p of b.pieces) {
      if (p[3] + (p[2] - p[1]) <= s || p[3] >= e) continue;
      const from = p[1] + Math.max(0, s - p[3]), to = p[1] + Math.min(p[2] - p[1], e - p[3]);
      const last = spans[spans.length - 1];
      if (last && last[0] === p[0]) last[2] = to; else spans.push([p[0], from, to]);
    }
    for (const [node, from, to] of spans) {
      const r = document.createRange();
      try { r.setStart(node, from); r.setEnd(node, to); } catch (_) { continue; }
      hl.add(r);
      roots.add(node.getRootNode());
    }
  };
  const whole = new Highlight(), words = new Highlight();
  words.priority = 1;
  const seen = new Set();
  for (const [i, s, e] of marks) {
    const b = blocks[i];
    if (!b) continue;
    if (!seen.has(i)) {
      seen.add(i);
      addRanges(whole, b, 0, b.text.length);
    }
    if (s !== null && e !== null) addRanges(words, b, s, e);
  }
  CSS.highlights.set('oxidemerge-block', whole);
  CSS.highlights.set('oxidemerge-word', words);
  // Adopted rather than added as a <style>, so the page's own elements stay as they were.
  const sheet = new CSSStyleSheet();
  sheet.replaceSync('::highlight(oxidemerge-block){background-color:rgba(255,232,96,.8);color:#000}' +
    '::highlight(oxidemerge-word){background-color:rgba(255,140,30,.95);color:#000}');
  for (const r of roots) {
    if (r === document || r instanceof ShadowRoot) r.adoptedStyleSheets = [...r.adoptedStyleSheets, sheet];
  }
  return seen.size;
})"#;

/// Run in every page before its own scripts: dialogs would stop the page until answered, and
/// nobody is there to answer them.
const NO_DIALOGS_JS: &str = "window.alert = () => {}; window.confirm = () => false; \
    window.prompt = () => null; window.print = () => {};";

/// Waits for fonts and, for full size screenshots, scrolls through the page so content loaded
/// on scrolling (lazy images, infinite lists) is there, then back to the top.
const SETTLE_JS: &str = r#"(async (full) => {
  const sleep = ms => new Promise(r => setTimeout(r, ms));
  try { await Promise.race([document.fonts.ready, sleep(3000)]); } catch (_) {}
  if (full) {
    const h = () => Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0);
    for (let y = 0, i = 0; y < h() && i < 60; y += Math.max(200, innerHeight), i++) {
      scrollTo(0, y);
      await sleep(100);
    }
    scrollTo(0, 0);
  }
  await sleep(600);
  return true;
})"#;

/// A highlight on one text block: the whole block, or a range of it in UTF-16 units.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Mark {
    pub block: usize,
    pub span: Option<(usize, usize)>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn whole(block: usize) -> Mark {
    Mark { block, span: None }
}

/// Ranges of a block, in UTF-16 units.
type Spans = Vec<(usize, usize)>;

/// Character ranges that differ between two blocks, or `None` when they are too different for
/// a word level comparison to help.
fn word_ranges(a: &str, b: &str) -> Option<(Spans, Spans)> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(Duration::from_millis(200))
        .diff_words(a, b);
    if diff.ratio() < 0.3 {
        return None;
    }
    fn push(v: &mut Vec<(usize, usize)>, s: usize, e: usize) {
        match v.last_mut() {
            // Joins changed words separated by a single space.
            Some(last) if s <= last.1 + 1 => last.1 = e,
            _ => v.push((s, e)),
        }
    }
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    let (mut oa, mut ob) = (0, 0);
    for change in diff.iter_all_changes() {
        let v = change.value();
        let len = utf16_len(v);
        let blank = v.trim().is_empty();
        match change.tag() {
            similar::ChangeTag::Equal => {
                oa += len;
                ob += len;
            }
            similar::ChangeTag::Delete => {
                if !blank {
                    push(&mut ra, oa, oa + len);
                }
                oa += len;
            }
            similar::ChangeTag::Insert => {
                if !blank {
                    push(&mut rb, ob, ob + len);
                }
                ob += len;
            }
        }
    }
    if ra.is_empty() && rb.is_empty() {
        return None;
    }
    Some((ra, rb))
}

fn mark_pair(a: &[String], b: &[String], ma: &mut Vec<Mark>, mb: &mut Vec<Mark>) {
    let ops = similar::capture_diff_slices_deadline(
        Algorithm::Myers,
        a,
        b,
        Some(Instant::now() + Duration::from_secs(3)),
    );
    for op in ops {
        let (tag, ra, rb) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {}
            DiffTag::Delete => ma.extend(ra.map(whole)),
            DiffTag::Insert => mb.extend(rb.map(whole)),
            DiffTag::Replace => {
                let paired = ra.len().min(rb.len());
                for k in 0..paired {
                    let (i, j) = (ra.start + k, rb.start + k);
                    match word_ranges(&a[i], &b[j]) {
                        Some((wa, wb)) => {
                            ma.push(whole(i));
                            mb.push(whole(j));
                            ma.extend(wa.into_iter().map(|s| Mark {
                                block: i,
                                span: Some(s),
                            }));
                            mb.extend(wb.into_iter().map(|s| Mark {
                                block: j,
                                span: Some(s),
                            }));
                        }
                        None => {
                            ma.push(whole(i));
                            mb.push(whole(j));
                        }
                    }
                }
                ma.extend((ra.start + paired..ra.end).map(whole));
                mb.extend((rb.start + paired..rb.end).map(whole));
            }
        }
    }
}

/// The text blocks to highlight on each page: each page is compared with its neighbours, so in
/// a three-way compare the middle page shows its differences from both sides.
pub fn diff_marks(pages: &[Vec<String>]) -> Vec<Vec<Mark>> {
    let mut marks = vec![Vec::new(); pages.len()];
    for i in 1..pages.len() {
        let (left, right) = marks.split_at_mut(i);
        mark_pair(&pages[i - 1], &pages[i], &mut left[i - 1], &mut right[0]);
    }
    for m in &mut marks {
        m.sort();
        m.dedup();
    }
    marks
}

fn keep_event(v: &Value) -> bool {
    match v.get("method").and_then(Value::as_str) {
        Some("Page.loadEventFired")
        | Some("Inspector.targetCrashed")
        | Some("Target.detachedFromTarget") => true,
        Some("Network.responseReceived") => {
            v.pointer("/params/type").and_then(Value::as_str) == Some("Document")
        }
        _ => false,
    }
}

fn session_of(v: &Value) -> Option<&str> {
    v.get("sessionId").and_then(Value::as_str)
}

/// Pages for the text and HTML modes.
pub enum TextSource {
    /// As the browser shows them, after their scripts have run.
    Rendered(Vec<Rendered>),
    /// As downloaded, without running their scripts; with why the browser was not used, when
    /// it failed.
    Downloaded(Vec<Page>, Option<String>),
}

impl TextSource {
    /// The visible text, or the HTML, of each page.
    pub fn texts(&self, html: bool) -> Vec<String> {
        match self {
            TextSource::Rendered(r) => r
                .iter()
                .map(|r| {
                    if html {
                        web::format_html(&r.html)
                    } else {
                        r.text()
                    }
                })
                .collect(),
            TextSource::Downloaded(pages, _) => pages
                .iter()
                .map(|p| {
                    if html {
                        web::format_html(&p.html)
                    } else {
                        web::html_to_text(&p.html)
                    }
                })
                .collect(),
        }
    }
}

/// The pages for the text and HTML modes: rendered by `browser` when there is one, otherwise,
/// or when rendering fails, as downloaded. Stopping (`progress.abort`) is an error either way.
pub fn load_text(
    browser: Option<&Path>,
    urls: &[Url],
    width: u32,
    height: u32,
    progress: &Progress,
) -> Result<TextSource> {
    let failed = match browser {
        None => None,
        Some(exe) => {
            let opts = RenderOptions {
                width,
                height,
                capture: Capture::None,
                highlight: false,
            };
            match render(exe, urls, &opts, progress) {
                Ok(r) => match r.iter().find_map(|r| r.error.clone()) {
                    None => return Ok(TextSource::Rendered(r)),
                    Some(e) => Some(e),
                },
                Err(_) if progress.abort.load(Ordering::Relaxed) => {
                    bail!("{}", crate::i18n::tr("msg.aborted"))
                }
                Err(e) => Some(format!("{e:#}")),
            }
        }
    };
    Ok(TextSource::Downloaded(web::fetch_pages(urls)?, failed))
}

/// Loads every page in its own tab of one headless browser and extracts, marks and captures
/// them. `progress.done` counts finished pages. A page that fails keeps its error in
/// [`Rendered::error`] (with a blank screenshot); the others are shown all the same.
pub fn render(
    browser_exe: &Path,
    urls: &[Url],
    opts: &RenderOptions,
    progress: &Progress,
) -> Result<Vec<Rendered>> {
    let aborted = || progress.abort.load(Ordering::Relaxed);
    let check = || -> Result<()> {
        if aborted() {
            bail!("{}", crate::i18n::tr("msg.aborted"));
        }
        Ok(())
    };
    // Stopping is never one page's error.
    let page_error = |e: anyhow::Error| -> Result<String> {
        check()?;
        Ok(format!("{e:#}"))
    };
    let mut b = Browser::launch(browser_exe, keep_event, Some(&progress.abort))?;
    let (w, h) = (opts.width.clamp(320, 7680), opts.height.clamp(240, 16384));
    let mut tabs = Vec::new();
    let mut out: Vec<Rendered> = Vec::new();
    for url in urls {
        let (s, target) = b.new_tab()?;
        b.call(Some(&s), "Page.enable", json!({}))?;
        b.call(Some(&s), "Network.enable", json!({}))?;
        b.call(
            Some(&s),
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": w, "height": h, "deviceScaleFactor": 1, "mobile": false }),
        )?;
        b.call(
            Some(&s),
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": NO_DIALOGS_JS }),
        )?;
        b.clear_events(&s);
        // Answered once the response headers arrive: all the pages are asked for first, so
        // they load at the same time.
        let id = b.send(Some(&s), "Page.navigate", json!({ "url": url.as_str() }))?;
        tabs.push((s, target, id));
        out.push(Rendered::default());
    }
    let deadline = Instant::now() + LOAD_TIMEOUT;
    for (i, (s, frame, id)) in tabs.iter_mut().enumerate() {
        match b.reply(*id, Some(s), "Page.navigate", deadline) {
            Ok(Some(nav)) => {
                if let Some(f) = nav["frameId"].as_str() {
                    *frame = f.to_string();
                }
                match nav.get("errorText").and_then(Value::as_str) {
                    // An error status with an empty body: the status tells what happened.
                    None | Some("") | Some("net::ERR_HTTP_RESPONSE_CODE_FAILURE") => {}
                    Some(e) => out[i].error = Some(format!("{}: {e}", urls[i])),
                }
            }
            Ok(None) => {
                out[i].error = Some(format!("{}: {}", urls[i], crate::i18n::tr("web.timeout")))
            }
            Err(e) => out[i].error = Some(page_error(e)?),
        }
    }
    // Wait for each page to load (or give up waiting and take them as they are). A crashed tab
    // would not answer again.
    let mut crashed = vec![false; tabs.len()];
    for (i, (s, _, _)) in tabs.iter().enumerate() {
        let ev = b.wait_event(deadline, &|v| {
            session_of(v) == Some(s.as_str())
                && matches!(
                    v.get("method").and_then(Value::as_str),
                    Some("Page.loadEventFired") | Some("Inspector.targetCrashed")
                )
        })?;
        if ev
            .as_ref()
            .and_then(|v| v.get("method"))
            .and_then(Value::as_str)
            == Some("Inspector.targetCrashed")
        {
            out[i].error = Some(format!("{}: the browser tab crashed", urls[i]));
            crashed[i] = true;
        }
    }
    let full = opts.capture == Capture::FullPage;
    let mut worlds = vec![None; tabs.len()];
    for (i, (s, frame, _)) in tabs.iter().enumerate() {
        check()?;
        if crashed[i] {
            continue;
        }
        // Only the last tab created is active. Activate each page before scrolling it so
        // Chromium also produces frames for pages that were loaded in the background.
        let read = (|| -> Result<(i64, Value)> {
            b.call(Some(s), "Page.bringToFront", json!({}))?;
            settle_and_extract(&mut b, s, frame, full)
        })();
        let (world, v) = match read {
            Ok(r) => r,
            Err(e) => {
                let e = page_error(e)?;
                out[i].error.get_or_insert(e);
                continue;
            }
        };
        worlds[i] = Some(world);
        let status = b
            .events()
            .filter(|e| {
                session_of(e) == Some(s.as_str())
                    && e.pointer("/params/frameId").and_then(Value::as_str) == Some(frame.as_str())
            })
            .filter_map(|e| e.pointer("/params/response/status").and_then(Value::as_f64))
            .last()
            .map_or(200, |n| n as u16);
        let r = &mut out[i];
        r.url = v["url"].as_str().and_then(|u| Url::parse(u).ok());
        r.status = if status == 0 { 200 } else { status };
        r.blocks = v["blocks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        r.html = v["html"].as_str().unwrap_or_default().to_string();
    }
    if opts.capture == Capture::None {
        progress.done.store(urls.len(), Ordering::Relaxed);
        return Ok(out);
    }
    if opts.highlight && out.len() > 1 {
        let blocks: Vec<Vec<String>> = out.iter().map(|r| r.blocks.clone()).collect();
        for (i, marks) in diff_marks(&blocks).into_iter().enumerate() {
            let Some(world) = worlds[i] else { continue };
            if marks.is_empty() {
                continue;
            }
            let list: Vec<Value> = marks
                .iter()
                .map(|m| match m.span {
                    Some((s, e)) => json!([m.block, s, e]),
                    None => json!([m.block, null, null]),
                })
                .collect();
            let js = format!("{MARK_JS}({})", Value::Array(list));
            // Without highlights the screenshot still shows the page.
            match b.eval(&tabs[i].0, Some(world), &js) {
                Ok(n) => out[i].marked = n.as_u64().unwrap_or(0) as usize,
                Err(e) => {
                    page_error(e)?;
                }
            }
        }
    }
    for (i, (s, _, _)) in tabs.iter().enumerate() {
        check()?;
        if crashed[i] {
            out[i].shot = Some(blank_png(w, h)?);
            progress.done.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let shot = (|| -> Result<(Vec<u8>, bool)> {
            // captureScreenshot can wait indefinitely for a compositor frame on an inactive
            // tab (notably Edge on Windows), even with background throttling disabled.
            b.call(Some(s), "Page.bringToFront", json!({}))?;
            let mut params = json!({ "format": "png" });
            let mut cut = false;
            if full {
                let m = b.call(Some(s), "Page.getLayoutMetrics", json!({}))?;
                let size = m.get("cssContentSize").or_else(|| m.get("contentSize"));
                let page_h = size
                    .and_then(|c| c["height"].as_f64())
                    .unwrap_or(h as f64)
                    .ceil()
                    .max(1.0) as u32;
                let shot_h = page_h.min(MAX_SHOT_HEIGHT);
                cut = page_h > MAX_SHOT_HEIGHT;
                params = json!({
                    "format": "png",
                    "captureBeyondViewport": true,
                    "clip": { "x": 0, "y": 0, "width": w, "height": shot_h, "scale": 1 },
                });
            }
            let shot = b.call(Some(s), "Page.captureScreenshot", params)?;
            let data = shot["data"].as_str().context("Page.captureScreenshot")?;
            Ok((cdp::base64_decode(data)?, cut))
        })();
        match shot {
            Ok((png, cut)) => {
                out[i].shot = Some(png);
                out[i].cut = cut;
            }
            Err(e) => {
                let e = page_error(e)?;
                out[i].error.get_or_insert(e);
                out[i].shot = Some(blank_png(w, h)?);
            }
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(out)
}

/// Lets the page settle, then reads its text in a JavaScript context of its own (returned for
/// marking it later). A page that navigates on by itself meanwhile (a redirect after loading)
/// is read once the next page has loaded, a few times at most.
fn settle_and_extract(b: &mut Browser, s: &str, frame: &str, full: bool) -> Result<(i64, Value)> {
    let mut tries = 0;
    loop {
        let read = (|| -> Result<(i64, Value)> {
            let world = b.isolated_world(s, frame)?;
            b.eval(s, Some(world), &format!("{SETTLE_JS}({full})"))?;
            Ok((world, b.eval(s, Some(world), EXTRACT_JS)?))
        })();
        let e = match read {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let msg = format!("{e:#}");
        let navigated = [
            "navigated or closed",
            "context was destroyed",
            "Cannot find context",
        ]
        .iter()
        .any(|m| msg.contains(m));
        tries += 1;
        if !navigated || tries == 3 {
            return Err(e);
        }
        b.wait_event(Instant::now() + RELOAD_TIMEOUT, &|v| {
            session_of(v) == Some(s)
                && v.get("method").and_then(Value::as_str) == Some("Page.loadEventFired")
        })?;
    }
}

/// A white PNG in place of a screenshot that could not be taken.
fn blank_png(w: u32, h: u32) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn marks_changed_words_and_blocks() {
        let a = v(&["Title", "The quick brown fox", "Gone"]);
        let b = v(&["Title", "The quick red fox", "New one", "Another"]);
        let m = diff_marks(&[a, b]);
        // "brown" (offsets 10..15) vs "red" (10..13), plus the whole blocks.
        assert!(m[0].contains(&Mark {
            block: 1,
            span: Some((10, 15))
        }));
        assert!(m[1].contains(&Mark {
            block: 1,
            span: Some((10, 13))
        }));
        assert!(m[0].contains(&whole(1)) && m[0].contains(&whole(2)));
        assert!(m[1].contains(&whole(2)) && m[1].contains(&whole(3)));
        assert!(!m[0].iter().any(|x| x.block == 0));
        assert!(!m[1].iter().any(|x| x.block == 0));
    }

    #[test]
    fn identical_pages_have_no_marks() {
        let a = v(&["One", "Two"]);
        let m = diff_marks(&[a.clone(), a.clone(), a]);
        assert!(m.iter().all(|x| x.is_empty()));
    }

    #[test]
    fn middle_page_of_three_shows_both_sides() {
        let m = diff_marks(&[v(&["a", "x"]), v(&["a", "b"]), v(&["c", "b"])]);
        assert_eq!(m[0], vec![whole(1)]);
        assert_eq!(m[1], vec![whole(0), whole(1)]);
        assert_eq!(m[2], vec![whole(0)]);
    }

    #[test]
    fn offsets_are_utf16() {
        let (a, b) = word_ranges("日本 😀 alpha beta", "日本 😀 alpha gamma").unwrap();
        // "日本 " is 3 units, "😀 " is 3 units, "alpha " is 6.
        assert_eq!(a, vec![(12, 16)]);
        assert_eq!(b, vec![(12, 17)]);
    }

    /// The browser to test with: `OXIDEMERGE_TEST_BROWSER` names it, or is `auto` to use the
    /// one found. Unset or empty, tests needing a browser are skipped, so they run only where
    /// asked for.
    fn test_browser() -> Option<std::path::PathBuf> {
        let name = std::env::var("OXIDEMERGE_TEST_BROWSER")
            .ok()
            .filter(|n| !n.is_empty())?;
        let found = crate::core::web::find_browser(if name == "auto" { "" } else { &name });
        assert!(
            found.is_some(),
            "OXIDEMERGE_TEST_BROWSER={name}: no such browser"
        );
        found
    }

    #[test]
    fn text_falls_back_to_the_pages_as_downloaded() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.html");
        std::fs::write(&p, "<p>Hello</p>").unwrap();
        let urls = [
            Url::from_file_path(&p).unwrap(),
            Url::from_file_path(&p).unwrap(),
        ];
        let progress = Progress::default();
        // No browser: downloaded, and nothing to report.
        let source = load_text(None, &urls, 800, 600, &progress).unwrap();
        assert!(matches!(source, TextSource::Downloaded(_, None)));
        // A browser that can't start: downloaded, saying why.
        let missing = d.path().join("no-such-browser");
        let source = load_text(Some(&missing), &urls, 800, 600, &progress).unwrap();
        let TextSource::Downloaded(pages, Some(why)) = source else {
            panic!("expected the pages as downloaded");
        };
        assert_eq!(pages.len(), 2);
        assert!(why.contains("no-such-browser"), "{why}");
        // Stopped: an error, not the downloaded pages.
        progress.abort.store(true, Ordering::Relaxed);
        assert!(load_text(Some(&missing), &urls, 800, 600, &progress).is_err());
    }

    /// Renders two pages in a real browser (see [`test_browser`]).
    #[test]
    fn renders_pages_in_a_browser() {
        let Some(exe) = test_browser() else {
            eprintln!("OXIDEMERGE_TEST_BROWSER not set; skipped");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let page = |name: &str, body: &str| {
            let p = d.path().join(name);
            std::fs::write(
                &p,
                format!(
                    "<!doctype html><html><head><title>T</title></head><body>{body}\
                     <div style='display:contents'><p>In contents</p></div>\
                     <x-card><b>slotted</b></x-card>\
                     <script>document.body.insertAdjacentHTML('beforeend','<p>From script</p>');\
                     customElements.define('x-card', class extends HTMLElement {{ constructor() {{ super();\
                     this.attachShadow({{mode:'open'}}).innerHTML='<p>Shadow <slot>fallback</slot></p>'; }} }});\
                     </script></body></html>"
                ),
            )
            .unwrap();
            Url::from_file_path(&p).unwrap()
        };
        let a = page(
            "a.html",
            "<h1>Hello</h1><p>The <b>quick</b> brown fox</p><p style='display:none'>hidden</p>\
             <div style='height:3000px'>tall</div><p>end</p>",
        );
        let b = page(
            "b.html",
            "<h1>Hello</h1><p>The <b>quick</b> red fox</p><div style='height:3000px'>tall</div><p>end</p>",
        );
        let progress = Progress::default();
        let text = render(
            &exe,
            &[a.clone(), b.clone()],
            &RenderOptions {
                width: 800,
                height: 600,
                capture: Capture::None,
                highlight: false,
            },
            &progress,
        )
        .unwrap();
        assert_eq!(
            text[0].blocks,
            v(&[
                "Hello",
                "The quick brown fox",
                "tall",
                "end",
                "In contents",
                "Shadow slotted",
                "From script"
            ])
        );
        assert!(text[1].html.contains("<p>From script</p>"));
        for capture in [Capture::Viewport, Capture::FullPage] {
            let shots = render(
                &exe,
                &[a.clone(), b.clone()],
                &RenderOptions {
                    width: 800,
                    height: 600,
                    capture,
                    highlight: true,
                },
                &progress,
            )
            .unwrap();
            // Both tabs must produce screenshots, including the first, inactive tab.
            for shot in &shots {
                let png = shot.shot.as_ref().unwrap();
                let image = image::load_from_memory(png).unwrap();
                assert_eq!(image.width(), 800);
                if capture == Capture::FullPage {
                    assert!(
                        image.height() > 3000,
                        "full page, not just the first screen"
                    );
                } else {
                    assert_eq!(image.height(), 600);
                    // The changed paragraph, near the top, is painted with the highlight.
                    let painted = image
                        .to_rgb8()
                        .pixels()
                        .filter(|p| p[0] >= 245 && (120..=245).contains(&p[1]) && p[2] <= 150)
                        .count();
                    assert!(painted > 50, "{painted} highlighted pixels");
                }
                assert_eq!(shot.marked, 1);
            }
        }
    }

    /// Pages that redirect, open dialogs, hold odd text or fail don't fail the others.
    #[test]
    fn pages_that_misbehave_are_shown_on_their_own() {
        let Some(exe) = test_browser() else {
            eprintln!("OXIDEMERGE_TEST_BROWSER not set; skipped");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let page = |name: &str, body: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, body).unwrap();
            Url::from_file_path(&p).unwrap()
        };
        page("target.html", "<p>Target</p>");
        let urls = [
            page(
                "redirect.html",
                "<meta http-equiv=refresh content='0;url=target.html'><p>Before</p>",
            ),
            page("alert.html", "<script>alert('hi')</script><p>After alert</p>"),
            page(
                "surrogate.html",
                "<p id=x></p><script>x.textContent = 'Hi \u{1F600}'.slice(0, 4)</script>",
            ),
            page(
                "page.xhtml",
                "<html xmlns='http://www.w3.org/1999/xhtml'><body><p>line one<br/>line two</p>\
                 <input value='typed'/></body></html>",
            ),
            page(
                "details.html",
                "<details><summary>Summary</summary><div style='display:contents'>secret</div></details>",
            ),
            Url::parse("http://127.0.0.1:9/").unwrap(),
        ];
        let opts = RenderOptions {
            width: 800,
            height: 600,
            capture: Capture::None,
            highlight: false,
        };
        let r = render(&exe, &urls, &opts, &Progress::default()).unwrap();
        let blocks: Vec<&[String]> = r.iter().map(|r| r.blocks.as_slice()).collect();
        assert_eq!(blocks[0], v(&["Target"]));
        assert_eq!(blocks[1], v(&["After alert"]));
        assert_eq!(blocks[2], v(&["Hi \u{FFFD}"]));
        assert_eq!(blocks[3], v(&["line one", "line two", "typed"]));
        assert_eq!(blocks[4], v(&["Summary"]));
        assert!(r[..5].iter().all(|r| r.error.is_none()), "{r:?}");
        assert!(r[5].error.is_some());

        // Replaced globals don't reach the scripts, and a failed page still gets a screenshot.
        let globals = |name: &str, word: &str| {
            page(
                name,
                &format!(
                    "<script>var Highlight = function () {{}}; window.Set = function () {{}};</script>\
                     <p>The {word} fox</p>"
                ),
            )
        };
        let urls = [
            globals("g1.html", "brown"),
            globals("g2.html", "red"),
            Url::parse("http://127.0.0.1:9/").unwrap(),
        ];
        let opts = RenderOptions {
            capture: Capture::Viewport,
            highlight: true,
            ..opts
        };
        let r = render(&exe, &urls, &opts, &Progress::default()).unwrap();
        assert_eq!(
            (r[0].marked, r[1].marked),
            (1, 1),
            "{:?}",
            (&r[0].error, &r[1].error)
        );
        assert!(r[2].error.is_some() && r[2].shot.is_some());
    }

    /// An error status without a body is shown as that status, and Stop ends a page that never
    /// answers.
    #[test]
    fn empty_error_pages_show_their_status_and_stop_ends_waiting() {
        let Some(exe) = test_browser() else {
            eprintln!("OXIDEMERGE_TEST_BROWSER not set; skipped");
            return;
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 2048];
                let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                if String::from_utf8_lossy(&buf[..n]).starts_with("GET /gone") {
                    let _ = std::io::Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                } else {
                    held.push(stream);
                }
            }
        });
        let opts = RenderOptions {
            width: 800,
            height: 600,
            capture: Capture::None,
            highlight: false,
        };
        let gone = Url::parse(&format!("http://127.0.0.1:{port}/gone")).unwrap();
        let r = render(&exe, &[gone.clone(), gone], &opts, &Progress::default()).unwrap();
        assert_eq!(r[0].status, 404, "{:?}", r[0].error);
        assert!(r[0].error.is_none());

        let progress = std::sync::Arc::new(Progress::default());
        let stop = progress.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            stop.abort.store(true, Ordering::Relaxed);
        });
        let hang = Url::parse(&format!("http://127.0.0.1:{port}/hang")).unwrap();
        let started = Instant::now();
        assert!(render(&exe, &[hang.clone(), hang], &opts, &progress).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
    }
}
