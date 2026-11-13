//! HTML slideshow reports, as Araxis Merge makes them: a single self-contained page that shows
//! one difference at a time, for walking an audience through a change. A title slide with the
//! compared sides and counts comes first and a closing slide last; the arrow keys, Space,
//! Page Up/Down, Home and End move between slides, F goes full screen, and each slide has its
//! own address (`#3`) to link to.

use super::diff::{DiffOptions, DiffResult};
use super::folder::{CmpStatus, DirItem};
use super::html_report::{
    block_extra, file_diffs, file_read, has_text_diffs, hunks, ignored_blocks, kind, line_range,
    row_cells, settings_dl, settings_rows, status_class, summary_reason, too_big, Extra, FileDiffs,
    FolderReportInfo, ReadAs, Redact, CONTEXT, DARK, HUNK_MAX_ROWS, LIGHT, MAX_BYTES, MAX_LINES,
};
use super::report::{esc, esc_marked, fmt_size, side_name};
use crate::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;
use std::path::PathBuf;

/// Most difference slides in one report; the rest are counted on the closing slide. Fewer in
/// tests: a file with more differences than that takes the compare of an unoptimized build past
/// its deadline, after which it reports the file as one difference.
const MAX_SLIDES: usize = if cfg!(test) { 50 } else { 2_000 };
/// Most files the closing slide of a folder slideshow lists.
const MAX_LISTED: usize = 500;

const CSS: &str = r#"
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans","PingFang TC","Microsoft JhengHei",sans-serif;display:flex;flex-direction:column;overflow:hidden}
:root{--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}
header{display:flex;align-items:center;gap:14px;padding:10px 22px;background:var(--surface);border-bottom:1px solid var(--border)}
header nav{display:flex;align-items:center;gap:10px;flex:none}
.brand{display:flex;align-items:center;gap:10px;font-weight:650;margin-right:auto;min-width:0}
.brand span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.home{display:flex;flex:none;border-radius:7px}.home:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.logo{width:30px;height:30px;border-radius:7px;display:block;flex:none;transition:transform .3s}
.home:hover .logo{transform:rotate(-12deg) scale(1.1)}
.made{color:var(--dim);font-size:13px;margin-top:28px}.made a{color:inherit}
@media (prefers-reduced-motion:reduce){*{transition:none!important}}
button{font:inherit;font-size:14px;padding:5px 12px;border-radius:8px;border:1px solid var(--border);background:var(--surface);color:var(--text);cursor:pointer}
button:hover{border-color:var(--accent)}button:disabled{opacity:.4;cursor:default}
button:focus-visible,a:focus-visible,summary:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.count{font-variant-numeric:tabular-nums;color:var(--dim);min-width:5.5em;text-align:center}
.bar{height:3px;background:var(--surface2)}.bar div{height:100%;background:var(--accent);transition:width .2s}
main{flex:1;position:relative;overflow:hidden}
section{position:absolute;inset:0;padding:28px 40px 24px;overflow:auto;display:none}
section.on{display:block}
section.title,section.end{display:none;text-align:center}
section.title.on,section.end.on{display:grid}
section.title>div,section.end>div{margin-block:auto;width:100%}
h1{font-size:34px;margin:0 0 6px}h2{font-size:22px;margin:0 0 4px}
.sub{color:var(--dim);margin-bottom:22px}
.sides{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:12px;margin:0 auto 22px;max-width:1100px;text-align:left}
.side{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:10px 14px;min-width:0}
.side b{display:inline-block;font-size:11px;text-transform:uppercase;letter-spacing:.05em;padding:1px 8px;border-radius:99px;background:var(--accent-bg);color:var(--accent);margin-bottom:4px}
.side div{font-family:var(--mono);font-size:13px;word-break:break-all}
.stats{display:flex;gap:12px;justify-content:center;flex-wrap:wrap;margin-bottom:22px}
.stat{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:12px 20px;min-width:130px}
.stat .v{font-size:28px;font-weight:700;font-variant-numeric:tabular-nums}.stat .l{color:var(--dim);font-size:13px}
.stat.acc .v{color:var(--accent)}.stat.ok .v{color:var(--ok)}.stat.warn .v{color:var(--warn)}.stat.bad .v{color:var(--bad)}.stat.uniq .v{color:var(--uniq)}
.hint{color:var(--dim);font-size:14px}
kbd{font-family:var(--mono);font-size:12px;padding:0 6px;border:1px solid var(--border);border-bottom-width:2px;border-radius:4px;background:var(--surface)}
.sh{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap;margin-bottom:14px}
.sh .file{font-family:var(--mono);font-size:15px;color:var(--dim);word-break:break-all;width:100%}
.kind{font-weight:600;font-size:13px;padding:2px 10px;border-radius:99px}
.k-chg{background:var(--chg);color:var(--warn)}.k-add{background:var(--add);color:var(--ok)}.k-del{background:var(--del);color:var(--bad)}.k-conf{background:var(--conf);color:var(--bad)}
.pos{color:var(--dim);font-family:var(--mono);font-size:14px}.pos b{color:var(--text);font-family:system-ui,sans-serif;font-weight:600;margin:0 4px 0 10px}
.d{border:1px solid var(--border);border-radius:12px;overflow:hidden;background:var(--surface)}
table{border-collapse:collapse;width:100%;table-layout:fixed;font-family:var(--mono);font-size:15px;line-height:1.6}
th{font:600 12px system-ui,sans-serif;color:var(--dim);text-align:left;padding:6px 10px;background:var(--surface2);border-bottom:1px solid var(--border)}
td{padding:0 10px;vertical-align:top;white-space:pre-wrap;word-break:break-all}
td.n{width:4.2em;text-align:right;color:var(--dim);user-select:none;background:var(--surface2);border-right:1px solid var(--border);white-space:nowrap}
.ctx{color:var(--dim)}.del{background:var(--del)}.add{background:var(--add)}.chg{background:var(--chg)}.conf{background:var(--conf)}
.ign{background:var(--ign);color:var(--dim)}.gap{background:repeating-linear-gradient(135deg,var(--gap),var(--gap) 5px,transparent 5px,transparent 10px)}
.del .m{background:var(--del-m);border-radius:3px}.add .m{background:var(--add-m);border-radius:3px}.chg .m,.conf .m{background:var(--chg-m);border-radius:3px}
tr.more td{text-align:center;color:var(--dim);font-family:system-ui,sans-serif;padding:4px;background:var(--surface2)}
.notes{margin:14px 0 0;padding:10px 14px;border-radius:10px;background:var(--chg);font-size:15px;list-style:none}
.notes li{margin:2px 0}
.list{max-width:900px;margin:0 auto;text-align:left;columns:2;column-gap:24px;font-size:14px}
.list div{break-inside:avoid;padding:2px 0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.list div.why{white-space:normal;overflow-wrap:anywhere}
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:8px;background:var(--dim)}
.st-different .dot{background:var(--warn)}.st-unique .dot{background:var(--uniq)}.st-error .dot{background:var(--bad)}
.list a{color:inherit;text-decoration:none}.list a:hover .nm{text-decoration:underline}
.list .c{color:var(--accent);font-variant-numeric:tabular-nums;margin-left:6px}.list .so{color:var(--dim)}
.meth{font-size:13px;padding:2px 10px;border-radius:99px;background:var(--accent-bg);color:var(--accent)}
.loc{color:var(--dim);font-size:14px}
.fields{margin:12px 0 0;padding:10px 14px 10px 30px;border-radius:10px;background:var(--surface);border:1px solid var(--border);font-size:14px}
.note,.paths{color:var(--dim);font-size:13px;margin:10px 0 0}.paths{margin:-12px 0 18px}
.omit{max-width:760px;margin:0 auto 20px;text-align:left;padding:10px 16px;border-radius:12px;border:1px solid var(--border);background:var(--surface);font-size:14px}
.omit b{display:block;margin-bottom:2px}.omit ul{margin:0;padding-left:20px}
.settings{max-width:760px;margin:18px auto 0;text-align:left;font-size:14px}
.settings summary{cursor:pointer;color:var(--dim)}.settings dl{display:grid;grid-template-columns:max-content 1fr;gap:4px 14px;margin:10px 0 0}
.settings dt{color:var(--dim)}.settings dd{margin:0}.settings ul{margin:0;padding-left:18px}
.settings code{font-family:var(--mono);font-size:13px}
@media (max-width:600px){header{padding:8px 10px;gap:6px}section{padding:16px 12px}h1{font-size:26px}
.stat{min-width:0;flex:1 1 40%;padding:10px 12px}.list{columns:1}.settings dl{grid-template-columns:1fr}table{font-size:13px}}
@media print{body{display:block;overflow:visible;height:auto}header,.bar{display:none}main{overflow:visible}section,section.title,section.end{position:static;display:block!important;break-after:page;text-align:left}}
"#;

const JS: &str = r#"
(()=>{const s=[...document.querySelectorAll('section')],n=s.length,c=document.getElementById('count'),
b=document.querySelector('.bar div'),pv=document.getElementById('prev'),nx=document.getElementById('next');let i=0;
function go(k,push){i=Math.max(0,Math.min(n-1,k));s.forEach((x,j)=>x.classList.toggle('on',j===i));
c.textContent=(i+1)+' / '+n;b.style.width=(n>1?i/(n-1)*100:100)+'%';pv.disabled=i===0;nx.disabled=i===n-1;
s[i].scrollTop=0;if(push!==false)history.replaceState(null,'','#'+(i+1))}
pv.onclick=()=>go(i-1);nx.onclick=()=>go(i+1);
document.getElementById('full').onclick=()=>document.fullscreenElement?document.exitFullscreen():document.documentElement.requestFullscreen();
document.addEventListener('keydown',e=>{if(e.ctrlKey||e.metaKey||e.altKey)return;const k=e.key;
if((k===' '||k==='Enter')&&e.target.closest&&e.target.closest('summary,a'))return;
if(k==='ArrowRight'||k==='PageDown'||k===' '||k==='j'||k==='n'){e.preventDefault();go(i+1)}
else if(k==='ArrowLeft'||k==='PageUp'||k==='k'||k==='p'){e.preventDefault();go(i-1)}
else if(k==='Home'){e.preventDefault();go(0)}else if(k==='End'){e.preventDefault();go(n-1)}
else if(k==='f'||k==='F'){document.getElementById('full').click()}});
window.addEventListener('hashchange',()=>go(parseInt(location.hash.slice(1),10)-1||0,false));
window.addEventListener('beforeprint',()=>document.querySelectorAll('details').forEach(d=>d.open=true));
go(parseInt(location.hash.slice(1),10)-1||0,false)})();
"#;

fn page_start(out: &mut String, title: &str) {
    let _ = write!(
        out,
        "<!DOCTYPE html>\n<html lang=\"{}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"generator\" content=\"OxideMerge {}\"><title>{}</title><style>:root{{{LIGHT}}}@media (prefers-color-scheme:dark){{:root{{{DARK}}}}}{CSS}</style></head><body>\
<header><div class=\"brand\"><a class=\"home\" href=\"{}\" title=\"OxideMerge\" target=\"_blank\" rel=\"noopener\"><img class=\"logo\" alt=\"OxideMerge\" src=\"{}\"></a><span>{}</span></div>\
<nav aria-label=\"{nav}\"><button id=\"prev\" title=\"{prev} (←)\" aria-label=\"{prev}\">‹</button><span class=\"count\" id=\"count\" role=\"status\" aria-live=\"polite\"></span><button id=\"next\" title=\"{next} (→)\" aria-label=\"{next}\">›</button>\
<button id=\"full\" title=\"{full} (F)\" aria-label=\"{full}\">⛶</button></nav></header><div class=\"bar\" aria-hidden=\"true\"><div></div></div><main>\n",
        crate::i18n::current_language(),
        env!("CARGO_PKG_VERSION"),
        esc(title),
        env!("CARGO_PKG_REPOSITORY"),
        super::html_report::icon_data_uri(),
        esc(title),
        nav = esc(tr("slides.nav")),
        prev = esc(tr("slides.prev")),
        next = esc(tr("slides.next")),
        full = esc(tr("slides.full_screen")),
    );
}

fn page_end(out: &mut String) {
    let _ = writeln!(out, "</main><script>{JS}</script></body></html>");
}

fn sides_html(out: &mut String, names: &[String]) {
    let n = names.len();
    out.push_str("<div class=\"sides\">");
    for (i, name) in names.iter().enumerate() {
        let _ = write!(
            out,
            "<div class=\"side\"><b>{}</b><div>{}</div></div>",
            esc(side_name(i, n)),
            esc(name)
        );
    }
    out.push_str("</div>");
}

fn stats_html(out: &mut String, cards: &[(&str, String, &str)]) {
    out.push_str("<div class=\"stats\">");
    for (class, value, label) in cards {
        let _ = write!(
            out,
            "<div class=\"stat {class}\"><div class=\"v\">{}</div><div class=\"l\">{}</div></div>",
            esc(value),
            esc(label)
        );
    }
    out.push_str("</div>");
}

/// The title slide; `notes` go under its counts: whether folder paths are hidden, and what the
/// slideshow leaves out.
fn title_slide(
    out: &mut String,
    title: &str,
    names: &[String],
    rules: &str,
    cards: &[(&str, String, &str)],
    notes: &str,
) {
    let _ = write!(out, "<section class=\"title\"><div><h1>{}</h1>", esc(title));
    let created = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let mut sub = created.to_string();
    if !rules.is_empty() {
        sub = format!(
            "{sub} · {}: {}",
            tr("report.rules"),
            super::rules::display_name(rules)
        );
    }
    let _ = write!(out, "<div class=\"sub\">{}</div>", esc(&sub));
    sides_html(out, names);
    stats_html(out, cards);
    out.push_str(notes);
    let _ = writeln!(
        out,
        "<div class=\"hint\">{}</div></div></section>",
        esc(tr("slides.hint"))
    );
}

/// A compared text and the differences found in it.
struct Doc<'a> {
    panes: &'a [&'a [String]],
    result: &'a DiffResult,
    opts: &'a DiffOptions,
    notes: &'a [BTreeMap<usize, String>],
    names: &'a [String],
}

/// The file of a slide in a folder compare: its path and how it was read.
struct SlideFile<'a> {
    path: &'a str,
    read: Option<&'a ReadAs>,
}

/// The slide of difference `b`, showing `rows` (it and the lines around it), with where it is in
/// the file and its field changes. `file` names the file in a folder compare. Returns whether
/// rows were left out.
fn diff_slide(
    out: &mut String,
    doc: &Doc,
    file: Option<&SlideFile>,
    (index, total): (usize, usize),
    (b, rows): (usize, Range<usize>),
    extra: &Extra,
) -> bool {
    let n = doc.panes.len();
    let blk = &doc.result.blocks[b];
    let (kclass, klabel) = kind(doc.result, b);
    out.push_str("<section><div class=\"sh\">");
    if let Some(f) = file {
        let _ = write!(out, "<div class=\"file\">{}</div>", esc(f.path));
    }
    let _ = write!(
        out,
        "<h2>{}</h2><span class=\"kind k-{kclass}\">{}</span>",
        esc(&trf(
            "slides.difference",
            &[("n", (index + 1).to_string()), ("total", total.to_string())]
        )),
        esc(klabel)
    );
    if let Some(read) = file.and_then(|f| f.read) {
        let _ = write!(
            out,
            "<span class=\"meth\" title=\"{}\">{}</span>",
            esc(tr("report.read_as")),
            esc(&read.label())
        );
    }
    out.push_str("<span class=\"pos\">");
    for (p, range) in blk.ranges.iter().enumerate().take(n) {
        let _ = write!(
            out,
            "<b>{}</b>{}",
            esc(side_name(p, n)),
            esc(&line_range(range))
        );
    }
    out.push_str("</span>");
    if let Some(at) = &extra.location {
        let _ = write!(out, "<span class=\"loc\">{}</span>", esc(at));
    }
    out.push_str("</div><div class=\"d\"><table><colgroup>");
    for _ in 0..n {
        out.push_str("<col style=\"width:4.2em\"><col>");
    }
    out.push_str("</colgroup><tr>");
    for name in doc.names.iter().take(n) {
        let _ = write!(out, "<th></th><th>{}</th>", esc(name));
    }
    out.push_str("</tr>");
    let shown = rows.start..rows.end.min(rows.start + HUNK_MAX_ROWS);
    let mut notes: Vec<(usize, usize, &str)> = Vec::new();
    for r in shown.clone() {
        let cells = row_cells(doc.result, doc.panes, doc.opts, r);
        let row = &doc.result.rows[r];
        out.push_str("<tr>");
        for (p, (class, marks)) in cells.iter().enumerate() {
            match row.line(p) {
                Some(l) => {
                    let _ = write!(
                        out,
                        "<td class=\"n\">{}</td><td class=\"{class}\">{}</td>",
                        l + 1,
                        esc_marked(&doc.panes[p][l], marks)
                    );
                    if let Some(note) = doc.notes.get(p).and_then(|m| m.get(&l)) {
                        if !note.is_empty() {
                            notes.push((p, l, note));
                        }
                    }
                }
                None => {
                    let _ = write!(out, "<td class=\"n\"></td><td class=\"{class}\"></td>");
                }
            }
        }
        out.push_str("</tr>");
    }
    if shown.end < rows.end {
        let _ = write!(
            out,
            "<tr class=\"more\"><td colspan=\"{}\">{}</td></tr>",
            n * 2,
            esc(&trf(
                "report.more_lines",
                &[("n", (rows.end - shown.end).to_string())]
            ))
        );
    }
    out.push_str("</table></div>");
    if !extra.fields.is_empty() {
        out.push_str("<ul class=\"fields\">");
        for field in &extra.fields {
            let _ = write!(out, "<li>{}</li>", esc(field));
        }
        out.push_str("</ul>");
    }
    if file.and_then(|f| f.read).is_some_and(ReadAs::extracted) {
        let _ = write!(
            out,
            "<p class=\"note\">{}</p>",
            esc(tr("report.extracted_lines"))
        );
    }
    if !notes.is_empty() {
        out.push_str("<ul class=\"notes\">");
        for (p, l, note) in notes {
            let _ = write!(
                out,
                "<li><b>{} {}:</b> {}</li>",
                esc(side_name(p, n)),
                l + 1,
                esc(note)
            );
        }
        out.push_str("</ul>");
    }
    out.push_str("</section>\n");
    shown.end < rows.end
}

fn end_slide(out: &mut String, text: &str, extra: &str) {
    let _ = writeln!(
        out,
        "<section class=\"end\"><div><h1>{}</h1>{extra}<p class=\"made\">{}</p></div></section>",
        esc(text),
        super::html_report::made_by_html()
    );
}

/// Slideshow of a 2-way or 3-way text compare: one slide per difference (ignored ones left out),
/// with bookmark comments on the slides of their lines.
pub fn text_slideshow(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
) -> String {
    let title = tr("slides.title");
    let list = hunks(result);
    let total = list.len();
    let mut out = String::new();
    page_start(&mut out, title);
    let mut cards = vec![("acc", total.to_string(), tr("report.differences"))];
    if panes.len() == 2 {
        let s = result.stats();
        cards.push(("warn", s.changed.to_string(), tr("report.kind_changed")));
        cards.push(("ok", s.added.to_string(), tr("report.kind_added")));
        cards.push(("bad", s.removed.to_string(), tr("report.kind_removed")));
    } else {
        cards.push((
            "bad",
            result.conflicts().to_string(),
            tr("report.kind_conflict"),
        ));
    }
    title_slide(&mut out, title, titles, rules, &cards, "");
    let doc = Doc {
        panes,
        result,
        opts,
        notes,
        names: titles,
    };
    for (i, h) in list.into_iter().take(MAX_SLIDES).enumerate() {
        diff_slide(&mut out, &doc, None, (i, total), h, &Extra::default());
    }
    let end = if total == 0 {
        tr("slides.no_diffs").to_string()
    } else {
        tr("slides.end").to_string()
    };
    let extra = if total > MAX_SLIDES {
        format!(
            "<div class=\"sub\">{}</div>",
            esc(&trf(
                "report.more_diffs",
                &[("n", (total - MAX_SLIDES).to_string())]
            ))
        )
    } else {
        String::new()
    };
    end_slide(&mut out, &end, &extra);
    page_end(&mut out);
    out
}

/// A changed file in the index of a folder slideshow.
struct Entry<'a> {
    item: &'a DirItem,
    /// The number of its first slide, when it has one.
    first: Option<usize>,
    /// Its slides, and all its differences.
    shown: usize,
    total: usize,
    read: Option<ReadAs>,
    /// Why it has no slides.
    why: Option<String>,
}

/// Slideshow of a folder compare, with the content of the HTML folder report: a title slide with
/// the counts and what the slideshow leaves out, a slide for each difference of every changed
/// text file (reading the files from `roots`), then a closing slide with the index of the changed
/// files, each linked to its first slide or saying why it has none, and the comparison settings.
/// `display` names the folders; `info` says how the report was made and whether their paths are
/// hidden.
pub fn folder_slideshow(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: &DiffOptions,
    rules: &str,
    info: &FolderReportInfo,
) -> String {
    let n = display.len();
    let redact = Redact::new(info.hide_paths, roots, display);
    let names: Vec<String> = if info.hide_paths {
        (0..n).map(|s| side_name(s, n).to_string()).collect()
    } else {
        display.iter().map(|d| d.display().to_string()).collect()
    };
    let files: Vec<&DirItem> = items
        .iter()
        .copied()
        .filter(|it| !it.is_dir || it.status == CmpStatus::Unique)
        .collect();
    let mut slides = String::new();
    let mut index: Vec<Entry> = Vec::new();
    let mut total = 0;
    // Differences without a slide once the slideshow is full, and those with rows left out.
    let (mut skipped, mut cut) = (0, 0);
    let mut ignored = 0;
    // Changed text files not read at all, once there are slides enough; ones known to be too
    // large are still listed, with why.
    let mut skipped_files = 0;
    let file_names: Vec<String> = (0..n).map(|s| side_name(s, n).to_string()).collect();
    for it in &files {
        if !matches!(status_class(it.status), "different" | "unique" | "error") {
            continue;
        }
        if total >= MAX_SLIDES && has_text_diffs(it, roots, opts) {
            match too_big(it, n) {
                Some(note) => index.push(Entry {
                    item: it,
                    first: None,
                    shown: 0,
                    total: 0,
                    read: None,
                    why: Some(redact.apply(&note)),
                }),
                None => skipped_files += 1,
            }
            continue;
        }
        let diffs = file_diffs(it, roots, opts);
        let reads: Vec<ReadAs> = match &diffs {
            FileDiffs::Diffs(docs, _) => docs.iter().map(|d| ReadAs::of(d, opts)).collect(),
            _ => Vec::new(),
        };
        let read = file_read(it, &diffs, &reads);
        // The title slide is the first.
        let first = total + 2;
        let (mut shown, mut count) = (0, 0);
        if let FileDiffs::Diffs(docs, result) = &diffs {
            ignored += ignored_blocks(result);
            let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
            let doc = Doc {
                panes: &refs,
                result,
                opts,
                notes: &[],
                names: &file_names,
            };
            let rel = it.rel_path();
            let file = SlideFile {
                path: &rel,
                read: read.as_ref(),
            };
            let list = hunks(result);
            count = list.len();
            for (j, h) in list.into_iter().enumerate() {
                if total >= MAX_SLIDES {
                    skipped += 1;
                    continue;
                }
                total += 1;
                shown += 1;
                let extra = block_extra(&reads, docs, result, h.0);
                if diff_slide(&mut slides, &doc, Some(&file), (j, count), h, &extra) {
                    cut += 1;
                }
            }
        }
        index.push(Entry {
            item: it,
            first: (shown > 0).then_some(first),
            shown,
            total: count,
            read,
            why: summary_reason(it, n, &diffs, true, &redact),
        });
    }
    let count = |st: &str| {
        files
            .iter()
            .filter(|it| status_class(it.status) == st)
            .count()
    };
    let title = tr("slides.title");
    let mut out = String::new();
    page_start(&mut out, title);
    let mut cards = vec![
        ("", files.len().to_string(), tr("report.files")),
        ("ok", count("identical").to_string(), tr("report.identical")),
        (
            "warn",
            count("different").to_string(),
            tr("report.files_different"),
        ),
        (
            "uniq",
            count("unique").to_string(),
            tr("report.files_unique"),
        ),
    ];
    if count("error") > 0 {
        cards.push(("bad", count("error").to_string(), tr("report.errors")));
    }
    if count("other") > 0 {
        cards.push(("", count("other").to_string(), tr("report.not_compared")));
    }
    cards.push(("acc", total.to_string(), tr("report.blocks_exported")));
    if skipped > 0 {
        cards.push(("", skipped.to_string(), tr("report.blocks_left_out")));
    }
    if ignored > 0 {
        cards.push(("", ignored.to_string(), tr("report.ignored")));
    }
    let summary_only = index.iter().filter(|e| e.total == 0).count();
    let mut omissions = Vec::new();
    if summary_only > 0 {
        omissions.push(trf(
            "report.omit_summary",
            &[("n", summary_only.to_string())],
        ));
    }
    if skipped > 0 {
        omissions.push(trf(
            "slides.omit_limit",
            &[
                ("n", skipped.to_string()),
                ("slides", fmt_size(Some(MAX_SLIDES as u64))),
            ],
        ));
    }
    if skipped_files > 0 {
        omissions.push(trf(
            "slides.more_files",
            &[("n", skipped_files.to_string())],
        ));
    }
    if cut > 0 {
        omissions.push(trf(
            "slides.omit_cut",
            &[("n", cut.to_string()), ("rows", HUNK_MAX_ROWS.to_string())],
        ));
    }
    let mut notes = String::new();
    if info.hide_paths {
        let _ = write!(
            notes,
            "<p class=\"paths\">{}</p>",
            esc(tr("report.paths_hidden"))
        );
    }
    if !omissions.is_empty() {
        let _ = write!(
            notes,
            "<div class=\"omit\" role=\"note\"><b>{}</b><ul>",
            esc(tr("report.omissions"))
        );
        for o in &omissions {
            let _ = write!(notes, "<li>{}</li>", esc(o));
        }
        notes.push_str("</ul></div>");
    }
    title_slide(&mut out, title, &names, rules, &cards, &notes);
    out.push_str(&slides);

    let mut extra = String::new();
    if !index.is_empty() {
        let _ = write!(
            extra,
            "<div class=\"sub\">{} ({})</div><div class=\"list\">",
            esc(tr("report.file_diffs")),
            index.len()
        );
        for e in index.iter().take(MAX_LISTED) {
            let rel = e.item.rel_path();
            let st = status_class(e.item.status);
            let read = e.read.as_ref().map(ReadAs::label).unwrap_or_default();
            match e.first {
                Some(first) => {
                    let mut count = e.shown.to_string();
                    if e.shown < e.total {
                        count = format!("{count}/{}", e.total);
                    }
                    let _ = write!(
                        extra,
                        "<div class=\"st-{st}\"><a href=\"#{first}\" title=\"{}\"><span class=\"dot\"></span><span class=\"nm\">{}</span><span class=\"c\">{count}</span> <span class=\"so\">{}</span></a></div>",
                        esc(&format!("{rel} · {read}")),
                        esc(&rel),
                        esc(&read)
                    );
                }
                None => {
                    let why = e.why.clone().unwrap_or_default();
                    let _ = write!(
                        extra,
                        "<div class=\"st-{st} why\" title=\"{}\"><span class=\"dot\"></span>{} — <span class=\"so\">{}{}</span></div>",
                        esc(&format!("{rel} — {why}")),
                        esc(&rel),
                        esc(tr("report.summary_only")),
                        if why.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", esc(&why))
                        }
                    );
                }
            }
        }
        extra.push_str("</div>");
        if index.len() > MAX_LISTED {
            let _ = write!(
                extra,
                "<div class=\"sub\">{}</div>",
                esc(&trf(
                    "slides.more_files",
                    &[("n", (index.len() - MAX_LISTED).to_string())]
                ))
            );
        }
    }
    let limits = trf(
        "slides.limits",
        &[
            ("slides", fmt_size(Some(MAX_SLIDES as u64))),
            ("hunk", HUNK_MAX_ROWS.to_string()),
            ("size", format!("{} MiB", MAX_BYTES >> 20)),
            ("lines", fmt_size(Some(MAX_LINES as u64))),
        ],
    );
    let rows = settings_rows(Some(opts), info, &redact, CONTEXT.to_string(), limits);
    let _ = write!(
        extra,
        "<details class=\"settings\"><summary>{}</summary>{}</details>",
        esc(tr("report.settings")),
        settings_dl(&rows)
    );
    let end = if index.is_empty() && skipped_files == 0 {
        tr("slides.no_diffs")
    } else {
        tr("slides.end")
    };
    end_slide(&mut out, end, &extra);
    page_end(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::compute;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn text_slideshows_have_a_slide_per_difference() {
        let a = v(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        ]);
        let b = v(&[
            "one", "TWO", "three", "four", "five", "six", "seven", "eight", "<nine>",
        ]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        let mut notes = vec![BTreeMap::new(), BTreeMap::new()];
        notes[0].insert(1, "renamed".to_string());
        let html = text_slideshow(
            &["a.txt".into(), "b.txt".into()],
            &[&a, &b],
            &r,
            &DiffOptions::default(),
            &notes,
            "",
        );
        // Title, two differences, end.
        assert_eq!(html.matches("<section").count(), 4);
        assert!(
            html.contains("&lt;") && !html.contains("<nine>"),
            "lines are escaped"
        );
        assert!(
            html.contains("<span class=\"m\">"),
            "changed words are marked"
        );
        assert!(html.contains("renamed"), "bookmark comments are shown");
        assert!(html.contains("a.txt") && html.contains("b.txt"));
        assert!(html.trim_end().ends_with("</html>"));
        assert!(
            html.contains("<img class=\"logo\" alt=\"OxideMerge\" src=\"data:image/png;base64,"),
            "the icon is embedded"
        );
        let home = format!("href=\"{}\"", env!("CARGO_PKG_REPOSITORY"));
        assert_eq!(
            html.matches(&home).count(),
            2,
            "the icon and the end slide link home"
        );
        assert!(!html.contains('⇄'));
    }

    #[test]
    fn identical_texts_say_so() {
        let a = v(&["same"]);
        let r = compute(&[&a, &a], &DiffOptions::default());
        let html = text_slideshow(
            &["a".into(), "b".into()],
            &[&a, &a],
            &r,
            &DiffOptions::default(),
            &[],
            "",
        );
        assert_eq!(html.matches("<section").count(), 2);
        assert!(html.contains(tr("slides.no_diffs")));
    }

    #[test]
    fn folder_slideshows_stop_reading_files_once_full() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [dir.path().join("l"), dir.path().join("r")];
        for r in &roots {
            std::fs::create_dir(r).unwrap();
        }
        // a.txt alone has one difference more than a slideshow holds.
        let side = |tag: &str| -> String {
            (0..=MAX_SLIDES)
                .map(|i| format!("same {i}\n{tag} {i}\n"))
                .collect()
        };
        std::fs::write(roots[0].join("a.txt"), side("left")).unwrap();
        std::fs::write(roots[1].join("a.txt"), side("right")).unwrap();
        std::fs::write(roots[0].join("b.txt"), "b\n").unwrap();
        std::fs::write(roots[1].join("b.txt"), "B\n").unwrap();
        let item = |name: &str| DirItem {
            rel_dir: String::new(),
            name: name.into(),
            is_dir: false,
            exists: [true, true, false],
            size: [Some(1), Some(1), None],
            mtime: [None; 3],
            status: CmpStatus::Different,
            diff_side: None,
            binary: false,
            error: None,
            depth: 0,
            parent: None,
            children: Vec::new(),
        };
        let (a, b) = (item("a.txt"), item("b.txt"));
        let html = folder_slideshow(
            &[&a, &b],
            &roots,
            &roots,
            &DiffOptions::default(),
            "",
            &FolderReportInfo::default(),
        );
        // Title, the slides it holds, end; b.txt is not read and is counted instead.
        assert_eq!(html.matches("<section").count(), MAX_SLIDES + 2);
        assert!(!html.contains(">B<"), "b.txt was not shown");
        let left_out = trf(
            "slides.omit_limit",
            &[
                ("n", "1".into()),
                ("slides", fmt_size(Some(MAX_SLIDES as u64))),
            ],
        );
        assert!(html.contains(&esc(&left_out)), "{left_out}");
        assert!(html.contains(&esc(&trf("slides.more_files", &[("n", "1".into())]))));
    }

    #[test]
    fn folder_slideshows_have_the_summary_index_and_settings_of_the_html_report() {
        use crate::core::filter::FileFilter;
        use crate::core::folder::{CompareMethod, FolderOptions, Progress, Scanner};
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("prices.csv"), "item,price\napple,1\n").unwrap();
        std::fs::write(b.path().join("prices.csv"), "item,price\napple,2\n").unwrap();
        std::fs::write(a.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
        std::fs::write(b.path().join("blob.bin"), [0u8, 1, 2, 4, 0, 9]).unwrap();
        std::fs::write(a.path().join("same.txt"), "same\n").unwrap();
        std::fs::write(b.path().join("same.txt"), "same\n").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: &opts,
            progress: &Progress::default(),
        }
        .run();
        let refs: Vec<&DirItem> = items.iter().filter(|i| !i.is_dir).collect();
        let info = FolderReportInfo {
            method: Some(CompareMethod::FullContents),
            filter: roots[0].join("*.csv").display().to_string(),
            scope: None,
            hide_paths: true,
            detail: Default::default(),
        };
        let html = folder_slideshow(&refs, &roots, &roots, &opts, "", &info);
        // Title, the one difference of prices.csv, end.
        assert_eq!(html.matches("<section").count(), 3);
        for want in [
            tr("report.files_different"),
            tr("report.files_unique"),
            tr("report.blocks_exported"),
            tr("report.paths_hidden"),
            tr("report.omissions"),
            tr("report.file_diffs"),
            tr("report.summary_only"),
            tr("report.binary"),
            tr("report.settings"),
            tr("report.set_filter"),
            tr("slides.next"),
        ] {
            assert!(html.contains(&esc(want)), "{want} missing");
        }
        assert!(
            html.contains("apple · price · 1 → 2"),
            "the fields that changed"
        );
        assert!(
            html.contains("<span class=\"meth\""),
            "how the file was read"
        );
        assert!(
            html.contains("<a href=\"#2\""),
            "the index links to the first slide of prices.csv"
        );
        assert!(html.contains("role=\"status\""));
        for root in &roots {
            let path = esc(&root.display().to_string());
            assert!(!html.contains(&path), "{path} shows");
        }
        assert!(
            !html.contains("same.txt"),
            "identical files are not indexed"
        );
    }
}
