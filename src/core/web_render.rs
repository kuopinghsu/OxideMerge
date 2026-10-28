//! Renders web pages in a headless browser (over the DevTools protocol) so they are compared as
//! the user sees them: after scripts have run, as visible text laid out in blocks, as the live
//! DOM, or as screenshots of the visible area or the whole page with text differences
//! highlighted on the page itself (like WinMerge's "View Differences").

use crate::core::cdp::{self, Browser};
use crate::core::web::Progress;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use similar::{Algorithm, DiffTag, TextDiff};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use url::Url;

/// Time allowed for all pages to load before they are used as they are.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45);
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
  const blockOf = el => {
    while (el.parentElement && el !== document.body && !isBlock(el)) el = el.parentElement;
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
    if (el.tagName === 'SELECT') { const o = el.selectedOptions && el.selectedOptions[0]; return o ? o.text : ''; }
    if (el.tagName === 'TEXTAREA') return el.value;
    const t = (el.type || 'text').toLowerCase();
    if (['hidden','checkbox','radio','file','image','range','color'].includes(t)) return '';
    if (t === 'password') return el.value ? '••••' : '';
    return el.value || '';
  };
  const root = document.body || document.documentElement;
  if (!root) return { url: location.href, html: '', blocks: [] };
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
    acceptNode(n) {
      if (n.nodeType !== 1) return NodeFilter.FILTER_ACCEPT;
      if (n.tagName === 'SELECT' || n.tagName === 'TEXTAREA') return NodeFilter.FILTER_ACCEPT;
      if (SKIP.has(n.tagName)) return NodeFilter.FILTER_REJECT;
      if (n.checkVisibility && !n.checkVisibility()) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    }
  });
  for (let n = walker.currentNode; n; n = walker.nextNode()) {
    if (n.nodeType === 1) {
      if (n.tagName === 'BR') brk = true;
      else if (n.tagName === 'INPUT' || n.tagName === 'SELECT' || n.tagName === 'TEXTAREA') {
        if (n.checkVisibility && !n.checkVisibility()) continue;
        const v = control(n).replace(/\s+/g, ' ').trim();
        if (v) { start(null); cur.text = v; flush(); }
      }
      continue;
    }
    const p = n.parentElement;
    if (!p || SKIP.has(p.tagName)) continue;
    const st = style(p);
    if (st.v === 'hidden' || st.v === 'collapse') continue;
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
    html: doctype + document.documentElement.outerHTML,
    blocks: blocks.map(b => b.text),
  };
})()"#;

/// Highlights the given blocks (and character ranges within them) with the CSS Custom Highlight
/// API, which paints over the text without changing the page's layout.
const MARK_JS: &str = r#"((marks) => {
  const blocks = window.__oxidemergeBlocks || [];
  if (!window.Highlight || !window.CSS || !CSS.highlights) return 0;
  const rangeOf = (b, s, e) => {
    const ps = b.pieces.filter(p => p[3] + (p[2] - p[1]) > s && p[3] < e);
    if (!ps.length) return null;
    const f = ps[0], l = ps[ps.length - 1];
    const r = document.createRange();
    try {
      r.setStart(f[0], f[1] + Math.max(0, s - f[3]));
      r.setEnd(l[0], l[1] + Math.min(l[2] - l[1], e - l[3]));
    } catch (_) { return null; }
    return r;
  };
  const whole = new Highlight(), words = new Highlight();
  words.priority = 1;
  const seen = new Set();
  for (const [i, s, e] of marks) {
    const b = blocks[i];
    if (!b) continue;
    if (!seen.has(i)) {
      seen.add(i);
      const r = rangeOf(b, 0, b.text.length);
      if (r) whole.add(r);
    }
    if (s !== null && e !== null) { const r = rangeOf(b, s, e); if (r) words.add(r); }
  }
  CSS.highlights.set('oxidemerge-block', whole);
  CSS.highlights.set('oxidemerge-word', words);
  const st = document.createElement('style');
  st.textContent = '::highlight(oxidemerge-block){background-color:rgba(255,232,96,.8);color:#000}' +
    '::highlight(oxidemerge-word){background-color:rgba(255,140,30,.95);color:#000}';
  (document.head || document.documentElement).appendChild(st);
  return seen.size;
})"#;

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
        Some("Page.loadEventFired") | Some("Inspector.targetCrashed") => true,
        Some("Network.responseReceived") => {
            v.pointer("/params/type").and_then(Value::as_str) == Some("Document")
        }
        _ => false,
    }
}

fn session_of(v: &Value) -> Option<&str> {
    v.get("sessionId").and_then(Value::as_str)
}

/// Loads every page in its own tab of one headless browser and extracts, marks and captures
/// them. `progress.done` counts finished pages.
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
    let mut b = Browser::launch(browser_exe, keep_event)?;
    let (w, h) = (opts.width.clamp(320, 7680), opts.height.clamp(240, 16384));
    let mut tabs = Vec::new();
    for url in urls {
        let s = b.new_tab()?;
        b.call(Some(&s), "Page.enable", json!({}))?;
        b.call(Some(&s), "Network.enable", json!({}))?;
        b.call(
            Some(&s),
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": w, "height": h, "deviceScaleFactor": 1, "mobile": false }),
        )?;
        b.clear_events(&s);
        let nav = b
            .call(Some(&s), "Page.navigate", json!({ "url": url.as_str() }))
            .with_context(|| url.to_string())?;
        if let Some(e) = nav.get("errorText").and_then(Value::as_str) {
            if !e.is_empty() {
                bail!("{url}: {e}");
            }
        }
        let frame = nav["frameId"].as_str().unwrap_or_default().to_string();
        tabs.push((s, frame));
    }
    // The pages load at the same time; wait for each (or give up waiting and take them as is).
    let deadline = Instant::now() + LOAD_TIMEOUT;
    for (s, _) in &tabs {
        let s = s.clone();
        let ev = b.wait_event(deadline, &aborted, &|v| {
            session_of(v) == Some(&s)
                && matches!(
                    v.get("method").and_then(Value::as_str),
                    Some("Page.loadEventFired") | Some("Inspector.targetCrashed")
                )
        })?;
        check()?;
        if ev
            .as_ref()
            .and_then(|v| v.get("method"))
            .and_then(Value::as_str)
            == Some("Inspector.targetCrashed")
        {
            bail!("the browser tab crashed");
        }
    }
    let full = opts.capture == Capture::FullPage;
    let mut out = Vec::new();
    for (s, frame) in &tabs {
        check()?;
        b.eval(s, &format!("{SETTLE_JS}({full})"))?;
        let v = b.eval(s, EXTRACT_JS)?;
        let status = b
            .events()
            .filter(|e| {
                session_of(e) == Some(s.as_str())
                    && e.pointer("/params/frameId").and_then(Value::as_str) == Some(frame.as_str())
            })
            .filter_map(|e| e.pointer("/params/response/status").and_then(Value::as_f64))
            .last()
            .map_or(200, |n| n as u16);
        out.push(Rendered {
            url: v["url"].as_str().and_then(|u| Url::parse(u).ok()),
            status: if status == 0 { 200 } else { status },
            blocks: v["blocks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            html: v["html"].as_str().unwrap_or_default().to_string(),
            ..Default::default()
        });
    }
    if opts.capture == Capture::None {
        progress.done.store(urls.len(), Ordering::Relaxed);
        return Ok(out);
    }
    if opts.highlight && out.len() > 1 {
        let blocks: Vec<Vec<String>> = out.iter().map(|r| r.blocks.clone()).collect();
        for (i, marks) in diff_marks(&blocks).into_iter().enumerate() {
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
            let n = b.eval(&tabs[i].0, &format!("{MARK_JS}({})", Value::Array(list)))?;
            out[i].marked = n.as_u64().unwrap_or(0) as usize;
        }
    }
    for (i, (s, _)) in tabs.iter().enumerate() {
        check()?;
        let mut params = json!({ "format": "png" });
        if full {
            let m = b.call(Some(s), "Page.getLayoutMetrics", json!({}))?;
            let size = m.get("cssContentSize").or_else(|| m.get("contentSize"));
            let page_h = size
                .and_then(|c| c["height"].as_f64())
                .unwrap_or(h as f64)
                .ceil()
                .max(1.0) as u32;
            let shot_h = page_h.min(MAX_SHOT_HEIGHT);
            out[i].cut = page_h > MAX_SHOT_HEIGHT;
            params = json!({
                "format": "png",
                "captureBeyondViewport": true,
                "clip": { "x": 0, "y": 0, "width": w, "height": shot_h, "scale": 1 },
            });
        }
        let shot = b.call(Some(s), "Page.captureScreenshot", params)?;
        let data = shot["data"].as_str().context("Page.captureScreenshot")?;
        out[i].shot = Some(cdp::base64_decode(data)?);
        progress.done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(out)
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

    /// Renders two pages in a real browser when one is installed (skipped otherwise).
    #[test]
    fn renders_pages_in_a_browser() {
        let Some(exe) = crate::core::web::find_browser(
            &std::env::var("OXIDEMERGE_TEST_BROWSER").unwrap_or_default(),
        ) else {
            eprintln!("no browser; skipped");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let page = |name: &str, body: &str| {
            let p = d.path().join(name);
            std::fs::write(
                &p,
                format!(
                    "<!doctype html><html><head><title>T</title></head><body>{body}\
                     <script>document.body.insertAdjacentHTML('beforeend','<p>From script</p>')</script>\
                     </body></html>"
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
            v(&["Hello", "The quick brown fox", "tall", "end", "From script"])
        );
        assert!(text[1].html.contains("<p>From script</p>"));
        let shots = render(
            &exe,
            &[a, b],
            &RenderOptions {
                width: 800,
                height: 600,
                capture: Capture::FullPage,
                highlight: true,
            },
            &progress,
        )
        .unwrap();
        // Width and height from the PNG header.
        let png = shots[0].shot.as_ref().unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!(width, 800);
        assert!(height > 3000, "full page, not just the first screen");
        assert_eq!(shots[0].marked, 1);
        assert_eq!(shots[1].marked, 1);
    }
}
