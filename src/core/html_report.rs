//! HTML reports of compare results: one self-contained page with a summary, the compared files
//! and every difference with the lines around it, for folder and text compares.

use super::diff::{inline_diff, BlockKind, DiffOptions, DiffResult, Stats};
use super::folder::{CmpStatus, DirItem};
use super::report::{esc, esc_marked, fmt_size, fmt_time, side_name, status_text};
use crate::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;
use std::path::PathBuf;

/// Unchanged lines shown before and after each difference.
const CONTEXT: usize = 3;
/// Rows of one difference shown at most; the rest are summed up in a note.
pub(crate) const HUNK_MAX_ROWS: usize = 400;
/// Text files longer than this are listed without their differences.
pub(crate) const MAX_LINES: usize = 50_000;
/// Files larger than this are listed in folder reports without their differences.
const MAX_BYTES: u64 = 16 << 20;
/// Characters of a line shown; the rest is cut off.
const LINE_CHARS: usize = 2_000;
/// Rows shown in a text report, all differences together; the differences after them are only
/// counted, keeping the page small enough for a browser.
const REPORT_ROWS: usize = 20_000;

pub(crate) const DARK: &str = "--bg:#0f1218;--surface:#171b23;--surface2:#1e232d;--border:#2a303c;\
--text:#e6e9ef;--dim:#98a2b3;--accent:#8b96ff;--accent-bg:#232a4d;--add:#12301f;--add-m:#1f6b3e;\
--del:#3a161b;--del-m:#7a2631;--chg:#33290f;--chg-m:#6b5414;--conf:#4a1717;--ign:#1f2340;\
--gap:#14181f;--ok:#4ade80;--warn:#fbbf24;--bad:#f87171;--uniq:#c084fc;--shadow:0 1px 2px #0006";

pub(crate) const LIGHT: &str = "--bg:#f6f7f9;--surface:#fff;--surface2:#f1f3f6;--border:#e3e6eb;\
--text:#1d2330;--dim:#667085;--accent:#5b6cff;--accent-bg:#eef0ff;--add:#e7f7ed;--add-m:#a9e5bf;\
--del:#fdecee;--del-m:#f5b8c1;--chg:#fff5dc;--chg-m:#f6d98a;--conf:#ffe0e0;--ign:#eef0fb;\
--gap:#f7f8fa;--ok:#16a34a;--warn:#d97706;--bad:#dc2626;--uniq:#7c3aed;\
--shadow:0 1px 2px #1018280f,0 1px 3px #1018281a";

const CSS: &str = r#"
:root{--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;color-scheme:light}
[data-theme=dark]{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans","PingFang TC","Microsoft JhengHei",sans-serif}
.top{position:sticky;top:0;z-index:5;display:flex;gap:12px;align-items:center;flex-wrap:wrap;padding:10px 24px;background:var(--surface);background:color-mix(in srgb,var(--surface) 88%,transparent);backdrop-filter:blur(10px);border-bottom:1px solid var(--border)}
.brand{display:flex;align-items:center;gap:10px;font-weight:650;font-size:15px;margin-right:auto}
.logo{width:26px;height:26px;border-radius:7px;background:linear-gradient(135deg,#ff8a3d,#5b6cff);display:grid;place-items:center;color:#fff;font-size:13px;font-weight:800}
.brand small{font-weight:400;color:var(--dim);font-size:12px}
.ctl{display:flex;gap:8px;align-items:center;flex-wrap:wrap}
input.q{width:220px;padding:6px 10px;border:1px solid var(--border);border-radius:8px;background:var(--surface);color:var(--text);font:inherit}
input.q:focus{outline:2px solid var(--accent);outline-offset:-1px}
button{font:inherit;font-size:13px;padding:5px 11px;border-radius:8px;border:1px solid var(--border);background:var(--surface);color:var(--text);cursor:pointer}
button:hover{border-color:var(--accent)}
.seg{display:inline-flex;border:1px solid var(--border);border-radius:8px;overflow:hidden}
.seg button{border:0;border-radius:0}.seg button.on{background:var(--accent-bg);color:var(--accent);font-weight:600}
.layout{display:grid;grid-template-columns:260px minmax(0,1fr);gap:24px;max-width:1600px;margin:0 auto;padding:20px 24px 48px}
.layout.single{grid-template-columns:minmax(0,1fr)}
.toc{position:sticky;top:70px;align-self:start;max-height:calc(100vh - 90px);overflow:auto;background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:12px;box-shadow:var(--shadow)}
.toc h4{margin:0 0 8px;font-size:12px;text-transform:uppercase;letter-spacing:.06em;color:var(--dim)}
.toc a{display:flex;justify-content:space-between;gap:8px;padding:4px 8px;border-radius:6px;color:var(--text);text-decoration:none;font-size:13px}
.toc a:hover{background:var(--surface2)}.toc a span:first-child{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.toc a .c{color:var(--dim);font-variant-numeric:tabular-nums}
.sides{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:12px;margin-bottom:16px}
.side{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:10px 14px;box-shadow:var(--shadow);min-width:0}
.side b{display:inline-block;font-size:11px;text-transform:uppercase;letter-spacing:.05em;padding:1px 8px;border-radius:99px;background:var(--accent-bg);color:var(--accent);margin-bottom:4px}
.side div{font-family:var(--mono);font-size:12.5px;word-break:break-all}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin-bottom:16px}
.stat{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:12px 14px;box-shadow:var(--shadow)}
.stat .v{font-size:24px;font-weight:700;font-variant-numeric:tabular-nums}.stat .l{color:var(--dim);font-size:12.5px}
.stat.ok .v{color:var(--ok)}.stat.warn .v{color:var(--warn)}.stat.bad .v{color:var(--bad)}.stat.uniq .v{color:var(--uniq)}.stat.acc .v{color:var(--accent)}
.chips{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px}
.chip{border-radius:99px;padding:4px 12px}.chip.on{background:var(--accent);border-color:var(--accent);color:#fff}
.chip .n{opacity:.75;margin-left:4px;font-variant-numeric:tabular-nums}
.file{background:var(--surface);border:1px solid var(--border);border-radius:12px;margin-bottom:10px;box-shadow:var(--shadow);overflow:hidden}
.file>summary,.file>.sum{display:flex;align-items:center;gap:10px;padding:10px 14px;list-style:none;cursor:pointer}
.file>.sum{cursor:default}.file>summary::-webkit-details-marker{display:none}
.file>summary:hover{background:var(--surface2)}
.dot{width:9px;height:9px;border-radius:50%;flex:none;background:var(--dim)}
.st-identical .dot{background:var(--ok)}.st-different .dot{background:var(--warn)}.st-unique .dot{background:var(--uniq)}.st-error .dot{background:var(--bad)}
.path{min-width:0;flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.path .dir{color:var(--dim);margin-left:8px;font-size:12.5px}
.badge{font-size:12px;padding:1px 9px;border-radius:99px;background:var(--surface2);color:var(--dim);white-space:nowrap;font-variant-numeric:tabular-nums}
.badge.add{background:var(--add);color:var(--ok)}.badge.del{background:var(--del);color:var(--bad)}.badge.chg{background:var(--chg);color:var(--warn)}
.badge.acc{background:var(--accent-bg);color:var(--accent)}.badge.ign{background:var(--ign);color:var(--dim)}
.meta{display:flex;gap:8px;flex-wrap:wrap;margin:-4px 0 16px;font-size:13px;color:var(--dim)}
.meta span{padding:3px 12px;border-radius:99px;background:var(--surface);border:1px solid var(--border)}.meta b{color:var(--text)}
.chev{width:18px;text-align:center;color:var(--dim);transition:transform .15s}
details[open]>summary .chev{transform:rotate(90deg)}
.body{padding:4px 14px 14px;border-top:1px solid var(--border)}
.note{color:var(--dim);font-size:13px;padding:10px 2px}
.comments{margin:10px 0;padding:8px 12px;border-radius:10px;background:var(--chg);font-size:13px}
.comments li{margin:2px 0}
.hunk{margin-top:12px;border:1px solid var(--border);border-radius:10px;overflow:hidden;scroll-margin-top:80px}
.hunk.focus{outline:2px solid var(--accent);outline-offset:1px}
.hh{display:flex;gap:10px;align-items:center;flex-wrap:wrap;padding:6px 10px;background:var(--surface2);border-bottom:1px solid var(--border);font-size:12.5px}
.hh .pos{color:var(--dim);font-family:var(--mono)}.hh .pos b{color:var(--text);font-family:system-ui,sans-serif;font-weight:600;margin-right:3px}
.hh .num{margin-left:auto;color:var(--dim)}.hh a{color:inherit;text-decoration:none}
.kind{font-weight:600;font-size:11.5px;padding:1px 8px;border-radius:99px}
.k-chg{background:var(--chg);color:var(--warn)}.k-add{background:var(--add);color:var(--ok)}.k-del{background:var(--del);color:var(--bad)}.k-conf{background:var(--conf);color:var(--bad)}
.scroll{overflow-x:auto}
table.d{border-collapse:collapse;width:100%;table-layout:fixed;font-family:var(--mono);font-size:12.5px;line-height:1.55}
table.d th{font:600 11.5px system-ui,sans-serif;color:var(--dim);text-align:left;padding:4px 8px;background:var(--surface);border-bottom:1px solid var(--border)}
table.d td{padding:0 8px;vertical-align:top;white-space:pre-wrap;word-break:break-all}
table.d td.n{width:4.5em;text-align:right;color:var(--dim);user-select:none;background:var(--surface2);border-right:1px solid var(--border);white-space:nowrap}
table.d td.s{width:1.6em;text-align:center;color:var(--dim);user-select:none;padding:0}
.ctx{color:var(--dim)}.del{background:var(--del)}.add{background:var(--add)}.chg{background:var(--chg)}.conf{background:var(--conf)}
.ign{background:var(--ign);color:var(--dim)}.gap{background:repeating-linear-gradient(135deg,var(--gap),var(--gap) 5px,transparent 5px,transparent 10px)}
.del .m{background:var(--del-m);border-radius:3px}.add .m{background:var(--add-m);border-radius:3px}.chg .m,.conf .m{background:var(--chg-m);border-radius:3px}
tr.cm td{background:var(--chg);font-family:system-ui,sans-serif;font-style:italic;padding:2px 10px}
tr.more td{text-align:center;color:var(--dim);font-family:system-ui,sans-serif;padding:4px;background:var(--surface2)}
table.u{display:none}body.unified table.u{display:table}body.unified table.s2.two{display:none}
.empty{text-align:center;color:var(--dim);padding:40px}
.foot{color:var(--dim);font-size:12px;text-align:center;margin-top:24px}
kbd{font-family:var(--mono);font-size:11px;padding:0 5px;border:1px solid var(--border);border-bottom-width:2px;border-radius:4px;background:var(--surface)}
@media (max-width:900px){.layout{grid-template-columns:minmax(0,1fr)}.toc{display:none}input.q{width:150px}}
[hidden]{display:none!important}
@media print{*{-webkit-print-color-adjust:exact;print-color-adjust:exact}.top .ctl,.toc{display:none}.layout{grid-template-columns:1fr}.file{break-inside:avoid-page;box-shadow:none}body{background:#fff}}
"#;

const JS: &str = r##"
(function(){
const root=document.documentElement,$=s=>[...document.querySelectorAll(s)];
try{const t=localStorage.getItem('oxm-theme');if(t)root.dataset.theme=t;const v=localStorage.getItem('oxm-view');if(v==='u')setView('u')}catch(e){}
function dark(){return root.dataset.theme?root.dataset.theme==='dark':matchMedia('(prefers-color-scheme: dark)').matches}
window.oxmTheme=function(){root.dataset.theme=dark()?'light':'dark';try{localStorage.setItem('oxm-theme',root.dataset.theme)}catch(e){}};
function unify(t){const u=document.createElement('table');u.className='d u';
u.innerHTML='<colgroup><col style="width:4.5em"><col style="width:4.5em"><col style="width:1.6em"><col></colgroup>';
const tb=u.createTBody(),D=['del','add','chg','conf','gap'],isd=c=>D.includes(c.className);
const line=(a,b,k,s,h)=>{tb.insertRow().innerHTML='<td class="n">'+a+'</td><td class="n">'+b+'</td><td class="s '+k+'">'+s+'</td><td class="'+k+'">'+h+'</td>'};
const note=r=>{const c=r.cloneNode(true);c.cells[0].colSpan=4;tb.appendChild(c)};
const rs=[...t.rows].filter(r=>r.cells[0].tagName==='TD');
for(let i=0;i<rs.length;){const c=rs[i].cells;
if(c.length<4){note(rs[i]);i++;continue}
const n0=c[0].textContent,n1=c[2].textContent;
if(!isd(c[1])&&!isd(c[3])){if(n0&&n1&&c[1].innerHTML!==c[3].innerHTML){line(n0,'',c[1].className,'−',c[1].innerHTML);line('',n1,c[3].className,'+',c[3].innerHTML)}
else line(n0,n1,c[1].className,'',n0?c[1].innerHTML:c[3].innerHTML);i++;continue}
let j=i;while(j<rs.length&&(rs[j].classList.contains('cm')||rs[j].cells.length===4&&(isd(rs[j].cells[1])||isd(rs[j].cells[3]))))j++;
const run=rs.slice(i,j),rows=run.filter(r=>r.cells.length===4);
rows.forEach(r=>{if(r.cells[0].textContent)line(r.cells[0].textContent,'',r.cells[1].className,'−',r.cells[1].innerHTML)});
rows.forEach(r=>{if(r.cells[2].textContent)line('',r.cells[2].textContent,r.cells[3].className,'+',r.cells[3].innerHTML)});
run.filter(r=>r.cells.length<4).forEach(note);i=j}
return u}
function setView(v){if(v==='u')$('table.s2.two').forEach(t=>{const x=t.nextElementSibling;if(!x||!x.matches('table.u'))t.after(unify(t))});
document.body.classList.toggle('unified',v==='u');$('[data-view]').forEach(b=>b.classList.toggle('on',b.dataset.view===v));try{localStorage.setItem('oxm-view',v)}catch(e){}}
window.oxmView=setView;
window.oxmOpen=function(o){$('details.file').forEach(d=>d.open=o)};
let opened=null;
addEventListener('beforeprint',()=>{opened=$('details.file').map(d=>d.open);oxmOpen(true)});
addEventListener('afterprint',()=>{if(opened)$('details.file').forEach((d,i)=>d.open=opened[i]);opened=null});
let status='all',toc;
function tocLink(id){if(!toc)toc=new Map($('.toc a').map(a=>[a.getAttribute('href').slice(1),a]));return toc.get(id)}
function apply(){const q=(document.getElementById('q')||{value:''}).value.trim().toLowerCase();let shown=0;
$('.file').forEach(f=>{const ok=(status==='all'||f.dataset.status===status)&&(!q||f.dataset.name.toLowerCase().includes(q));f.hidden=!ok;if(ok)shown++;
const a=tocLink(f.id);if(a)a.hidden=!ok});
const e=document.getElementById('empty');if(e)e.hidden=shown>0}
window.oxmStatus=function(b){status=b.dataset.status;$('.chip').forEach(c=>c.classList.toggle('on',c===b));apply()};
const q=document.getElementById('q');if(q)q.addEventListener('input',apply);
document.addEventListener('click',e=>{const a=e.target.closest('a[href^="#"]');if(!a)return;const t=document.getElementById(a.getAttribute('href').slice(1));if(t){const d=t.closest('details')||t;if(d.tagName==='DETAILS')d.open=true}});
function jump(dir){const hs=$('.hunk').filter(h=>!h.closest('.file').hidden);if(!hs.length)return;
const c=hs.indexOf(document.querySelector('.hunk.focus'));let k;
if(c>=0)k=c+dir;else{const top=h=>{const d=h.closest('details');return(d&&!d.open?d:h).getBoundingClientRect().top};let i=hs.findIndex(h=>top(h)>82);if(i<0)i=hs.length;k=dir>0?i:i-1}
k=Math.max(0,Math.min(hs.length-1,k));const h=hs[k];const d=h.closest('details');if(d)d.open=true;
$('.hunk.focus').forEach(x=>x.classList.remove('focus'));h.classList.add('focus');h.scrollIntoView({block:'start',behavior:'smooth'})}
document.addEventListener('keydown',e=>{if(e.target.tagName==='INPUT'||e.ctrlKey||e.metaKey||e.altKey)return;
const key=e.key.toLowerCase();if(key==='n'||key==='j')jump(1);else if(key==='p'||key==='k')jump(-1);else if(key==='/'&&q){e.preventDefault();q.focus()}});
})();
"##;

/// The differences to list, each with the rows shown for it: its own and up to `CONTEXT`
/// unchanged rows around it, short of the next or previous difference.
pub(crate) fn hunks(result: &DiffResult) -> Vec<(usize, Range<usize>)> {
    let end = |b: &super::diff::Block| b.first_row + b.rows;
    let listed: Vec<usize> = (0..result.blocks.len())
        .filter(|&b| !result.blocks[b].ignored)
        .collect();
    listed
        .iter()
        .enumerate()
        .map(|(k, &b)| {
            let blk = &result.blocks[b];
            let prev = k
                .checked_sub(1)
                .map_or(0, |j| end(&result.blocks[listed[j]]));
            let next = listed
                .get(k + 1)
                .map_or(result.rows.len(), |&j| result.blocks[j].first_row);
            let from = blk.first_row.saturating_sub(CONTEXT).max(prev);
            let to = (end(blk) + CONTEXT).min(next).max(end(blk));
            (b, from.min(blk.first_row)..to)
        })
        .collect()
}

/// The kind of a difference, as a CSS class and a label.
pub(crate) fn kind(result: &DiffResult, b: usize) -> (&'static str, &'static str) {
    let blk = &result.blocks[b];
    if result.panes == 2 {
        return match (blk.ranges[0].is_empty(), blk.ranges[1].is_empty()) {
            (true, _) => ("add", tr("report.kind_added")),
            (_, true) => ("del", tr("report.kind_removed")),
            _ => ("chg", tr("report.kind_changed")),
        };
    }
    match blk.kind {
        BlockKind::Conflict => ("conf", tr("report.kind_conflict")),
        BlockKind::LeftOnly => ("chg", tr("report.kind_left")),
        BlockKind::RightOnly => ("chg", tr("report.kind_right")),
        BlockKind::BothSame | BlockKind::Compatible => ("chg", tr("report.kind_both")),
        BlockKind::Diff => ("chg", tr("report.kind_changed")),
    }
}

pub(crate) fn line_range(r: &Range<usize>) -> String {
    match r.len() {
        0 => "—".into(),
        1 => (r.start + 1).to_string(),
        _ => format!("{}–{}", r.start + 1, r.end),
    }
}

/// How the line of each pane in row `r` is shown, and the parts that differ from the other
/// side (from the middle one with three sides).
pub(crate) fn row_cells(
    result: &DiffResult,
    panes: &[&[String]],
    opts: &DiffOptions,
    r: usize,
) -> Vec<(&'static str, Vec<Range<usize>>)> {
    let n = panes.len();
    let row = &result.rows[r];
    let block = row.block.map(|b| &result.blocks[b]);
    let mut cells: Vec<(&'static str, Vec<Range<usize>>)> = (0..n)
        .map(|p| {
            let class = match block {
                None => "ctx",
                Some(b) if b.ignored => "ign",
                Some(_) if row.line[p].is_none() => "gap",
                Some(_) if n == 2 => ["del", "add"][p],
                Some(b) if b.kind == BlockKind::Conflict => "conf",
                Some(_) => "chg",
            };
            (class, Vec::new())
        })
        .collect();
    let text = |p: usize| row.line[p].map(|l| panes[p][l].as_str());
    let changed = |c: &str| matches!(c, "del" | "add" | "chg" | "conf");
    // Three sides: a side still as the middle one is unchanged; the middle is changed only when
    // both sides moved away from it.
    if n == 3 && cells.iter().any(|c| c.0 == "chg") {
        let same = |p: usize| text(p).is_some() && text(p) == text(1);
        let (left, right) = (same(0), same(2));
        for (p, kept) in [(0, left), (2, right), (1, left || right)] {
            if kept && cells[p].0 == "chg" {
                cells[p].0 = "ctx";
            }
        }
    }
    if n == 2 {
        if let (Some(a), Some(b)) = (text(0), text(1)) {
            if changed(cells[0].0) {
                let (ma, mb) = inline_diff(a, b, opts);
                cells[0].1 = ma;
                cells[1].1 = mb;
            }
        }
    } else {
        for p in [0, 2] {
            if let (Some(a), Some(m)) = (text(p), text(1)) {
                if changed(cells[p].0) {
                    cells[p].1 = inline_diff(a, m, opts).0;
                }
            }
        }
    }
    cells
}

/// A compared text: the lines of each side, their differences, the options they were found
/// with, and bookmark comments.
struct Doc<'a> {
    panes: &'a [&'a [String]],
    result: &'a DiffResult,
    opts: &'a DiffOptions,
    notes: &'a [BTreeMap<usize, String>],
}

fn hunk_html(
    out: &mut String,
    doc: &Doc,
    id: &str,
    (index, total): (usize, usize),
    (b, rows): (usize, Range<usize>),
) {
    let Doc {
        panes,
        result,
        opts,
        notes,
    } = *doc;
    let n = panes.len();
    let blk = &result.blocks[b];
    let (kclass, klabel) = kind(result, b);
    let _ = write!(
        out,
        "<section class=\"hunk\" id=\"{id}\"><div class=\"hh\"><span class=\"kind k-{kclass}\">{}</span><span class=\"pos\">",
        esc(klabel)
    );
    for (p, range) in blk.ranges.iter().enumerate().take(n) {
        let _ = write!(
            out,
            "<b>{}</b>{} &nbsp;",
            esc(side_name(p, n)),
            line_range(range)
        );
    }
    let _ = write!(
        out,
        "</span><a class=\"num\" href=\"#{id}\">{}</a></div><div class=\"scroll\">",
        esc(&trf(
            "report.diff_n",
            &[("i", (index + 1).to_string()), ("n", total.to_string())]
        ))
    );
    let shown = rows.start..rows.end.min(rows.start + HUNK_MAX_ROWS);
    let hidden = rows.len() - shown.len();

    // Side by side; for two sides the page builds a unified table from this one.
    let _ = write!(
        out,
        "<table class=\"d s2{}\"><colgroup>",
        if n == 2 { " two" } else { "" }
    );
    for _ in 0..n {
        out.push_str("<col style=\"width:4.5em\"><col>");
    }
    out.push_str("</colgroup><tr>");
    for p in 0..n {
        let _ = write!(out, "<th colspan=\"2\">{}</th>", esc(side_name(p, n)));
    }
    out.push_str("</tr>");
    for r in shown {
        out.push_str("<tr>");
        let cells = row_cells(result, panes, opts, r);
        for (p, (lines, (class, marks))) in panes.iter().zip(&cells).enumerate() {
            match result.rows[r].line[p] {
                Some(l) => {
                    let line = &lines[l];
                    let cut = line.char_indices().nth(LINE_CHARS).map(|(i, _)| i);
                    let more = match cut {
                        Some(c) if marks.iter().any(|m| m.end > c) => "<span class=\"m\">…</span>",
                        Some(_) => "…",
                        None => "",
                    };
                    let _ = write!(
                        out,
                        "<td class=\"n\">{}</td><td class=\"{class}\">{}{more}</td>",
                        l + 1,
                        esc_marked(&line[..cut.unwrap_or(line.len())], marks),
                    );
                }
                None => {
                    let _ = write!(out, "<td class=\"n\"></td><td class=\"{class}\"></td>");
                }
            }
        }
        out.push_str("</tr>");
        for (p, marks) in notes.iter().enumerate().take(n) {
            let note = result.rows[r].line[p].and_then(|l| marks.get(&l).map(|t| (l, t)));
            if let Some((l, text)) = note.filter(|(_, t)| !t.trim().is_empty()) {
                let _ = write!(
                    out,
                    "<tr class=\"cm\"><td colspan=\"{}\">💬 {} {}: {}</td></tr>",
                    2 * n,
                    esc(side_name(p, n)),
                    l + 1,
                    esc(text)
                );
            }
        }
    }
    if hidden > 0 {
        let _ = write!(
            out,
            "<tr class=\"more\"><td colspan=\"{}\">{}</td></tr>",
            2 * n,
            esc(&trf("report.more_lines", &[("n", hidden.to_string())]))
        );
    }
    out.push_str("</table>");
    out.push_str("</div></section>\n");
}

fn ignored_blocks(result: &DiffResult) -> usize {
    result.blocks.iter().filter(|b| b.ignored).count()
}

/// The compare rules file used, when there is one.
fn rules_note(out: &mut String, rules: &str) {
    if !rules.is_empty() {
        let _ = write!(
            out,
            "<div class=\"meta\"><span>{}: <b>{}</b></span></div>",
            esc(tr("report.rules")),
            esc(super::rules::display_name(rules))
        );
    }
}

/// "{n} differences", or the wording for one.
pub fn diff_count(n: usize) -> String {
    let key = if n == 1 {
        "report.text_summary_one"
    } else {
        "report.text_summary"
    };
    trf(key, &[("n", n.to_string())])
}

/// Line count badges of a 2-way result.
fn stat_badges(out: &mut String, s: &Stats) {
    let _ = write!(
        out,
        "<span class=\"badge acc\">{}</span>",
        esc(&diff_count(s.differences))
    );
    for (class, sign, n, key) in [
        ("add", "+", s.added, "report.added"),
        ("del", "−", s.removed, "report.removed"),
        ("chg", "~", s.changed, "report.changed"),
    ] {
        if n > 0 {
            let _ = write!(
                out,
                "<span class=\"badge {class}\" title=\"{}\">{sign}{n}</span>",
                esc(tr(key))
            );
        }
    }
}

struct Controls {
    filter: bool,
    view: bool,
    expand: bool,
}

fn page_start(out: &mut String, title: &str, c: &Controls) {
    out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    let _ = writeln!(out, "<title>{}</title>", esc(title));
    let _ = writeln!(
        out,
        "<style>:root{{{LIGHT}}}{CSS}[data-theme=dark]{{{DARK}}}@media (prefers-color-scheme:dark){{:root:not([data-theme=light]){{{DARK};color-scheme:dark}}}}@media print{{:root:not(.oxm){{{LIGHT};color-scheme:light}}}}</style></head><body>"
    );
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let _ = write!(
        out,
        "<header class=\"top\"><div class=\"brand\"><div class=\"logo\">⇄</div><div>{}<br><small>{}</small></div></div><div class=\"ctl\">",
        esc(title),
        esc(&trf("report.generated", &[("date", now.to_string())]))
    );
    if c.filter {
        let _ = write!(
            out,
            "<input id=\"q\" class=\"q\" type=\"search\" placeholder=\"{}\">",
            esc(tr("report.search"))
        );
    }
    if c.view {
        let _ = write!(
            out,
            "<div class=\"seg\"><button class=\"on\" data-view=\"s\" onclick=\"oxmView('s')\">{}</button><button data-view=\"u\" onclick=\"oxmView('u')\">{}</button></div>",
            esc(tr("report.side_by_side")),
            esc(tr("report.unified"))
        );
    }
    if c.expand {
        let _ = write!(
            out,
            "<button onclick=\"oxmOpen(true)\">{}</button><button onclick=\"oxmOpen(false)\">{}</button>",
            esc(tr("report.expand_all")),
            esc(tr("report.collapse_all"))
        );
    }
    let _ = writeln!(
        out,
        "<button onclick=\"oxmTheme()\" title=\"{0}\" aria-label=\"{0}\">◐</button></div></header>",
        esc(tr("report.theme"))
    );
}

fn sides(out: &mut String, names: &[String]) {
    out.push_str("<div class=\"sides\">");
    for (s, name) in names.iter().enumerate() {
        let _ = write!(
            out,
            "<div class=\"side\"><b>{}</b><div>{}</div></div>",
            esc(side_name(s, names.len())),
            esc(name)
        );
    }
    out.push_str("</div>\n");
}

fn stat_cards(out: &mut String, cards: &[(&str, String, &str)]) {
    out.push_str("<div class=\"stats\">");
    for (class, value, label) in cards {
        let _ = write!(
            out,
            "<div class=\"stat {class}\"><div class=\"v\">{value}</div><div class=\"l\">{}</div></div>",
            esc(label)
        );
    }
    out.push_str("</div>\n");
}

fn page_end(out: &mut String, hunks: bool) {
    out.push_str("<p class=\"foot\">");
    if hunks {
        let _ = write!(out, "{} · ", tr("report.keys"));
    }
    let _ = writeln!(
        out,
        "{}</p></main></div><script>{JS}</script></body></html>",
        esc(&trf(
            "report.made_by",
            &[("version", env!("CARGO_PKG_VERSION").to_string())]
        ))
    );
}

/// The report of a 2-way or 3-way text compare: its differences, each with the lines around
/// it, the bookmark comments in `notes`, and the compare rules file `rules` ("" = none) used.
pub fn text_report(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
) -> String {
    let n = panes.len();
    let mut list = hunks(result);
    let mut rows = 0;
    let fits = list
        .iter()
        .take_while(|(_, r)| {
            rows += r.len().min(HUNK_MAX_ROWS);
            rows <= REPORT_ROWS
        })
        .count()
        .max(1);
    let total = list.len();
    list.truncate(fits);
    let mut out = String::new();
    let title = tr("report.text_title");
    page_start(
        &mut out,
        title,
        &Controls {
            filter: false,
            view: n == 2 && !list.is_empty(),
            expand: false,
        },
    );
    let toc = !list.is_empty();
    let _ = write!(
        out,
        "<div class=\"layout{}\">",
        if toc { "" } else { " single" }
    );
    if toc {
        let _ = write!(
            out,
            "<nav class=\"toc\"><h4>{}</h4>",
            esc(tr("report.differences"))
        );
        for (k, (b, _)) in list.iter().enumerate() {
            let (_, label) = kind(result, *b);
            // The line of the base side, or of the first side that has lines here.
            let ranges = &result.blocks[*b].ranges;
            let order: &[usize] = if n == 3 { &[1, 0, 2] } else { &[0, 1] };
            let range = order
                .iter()
                .map(|&p| &ranges[p])
                .find(|r| !r.is_empty())
                .unwrap_or(&ranges[order[0]]);
            let pos = line_range(range);
            let _ = write!(
                out,
                "<a href=\"#d{k}\"><span>{}. {}</span><span class=\"c\">{pos}</span></a>",
                k + 1,
                esc(label)
            );
        }
        out.push_str("</nav>");
    }
    out.push_str("<main>");
    sides(&mut out, titles);
    rules_note(&mut out, rules);
    let s = result.stats();
    let mut cards = vec![("acc", total.to_string(), tr("report.differences"))];
    if n == 2 {
        cards.push(("ok", s.added.to_string(), tr("report.added")));
        cards.push(("bad", s.removed.to_string(), tr("report.removed")));
        cards.push(("warn", s.changed.to_string(), tr("report.changed")));
    } else {
        cards.push((
            "bad",
            result.conflicts().to_string(),
            tr("report.kind_conflict"),
        ));
    }
    let ignored = ignored_blocks(result);
    if ignored > 0 {
        cards.push(("", ignored.to_string(), tr("report.ignored")));
    }
    stat_cards(&mut out, &cards);

    let comments: Vec<String> = notes
        .iter()
        .enumerate()
        .take(n)
        .flat_map(|(p, m)| {
            m.iter()
                .filter(|(_, t)| !t.trim().is_empty())
                .map(move |(l, t)| {
                    format!(
                        "<li><b>{} {}</b>: {}</li>",
                        esc(side_name(p, n)),
                        l + 1,
                        esc(t)
                    )
                })
        })
        .collect();
    if !comments.is_empty() {
        let _ = write!(
            out,
            "<div class=\"comments\">💬 {}<ul>{}</ul></div>",
            esc(tr("report.comments")),
            comments.concat()
        );
    }
    if list.is_empty() {
        let _ = write!(
            out,
            "<div class=\"file\"><div class=\"empty\">✓ {}</div></div>",
            esc(tr("report.no_diffs"))
        );
    } else {
        out.push_str("<div class=\"file\"><div class=\"body\" style=\"border-top:0\">");
        let shown = list.len();
        for (k, h) in list.into_iter().enumerate() {
            let doc = Doc {
                panes,
                result,
                opts,
                notes,
            };
            hunk_html(&mut out, &doc, &format!("d{k}"), (k, total), h);
        }
        if shown < total {
            let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
            let _ = write!(out, "<div class=\"note\">{}</div>", esc(&more));
        }
        out.push_str("</div></div>");
    }
    page_end(&mut out, toc);
    out
}

pub(crate) fn status_class(st: CmpStatus) -> &'static str {
    match st {
        CmpStatus::Different => "different",
        CmpStatus::Unique => "unique",
        CmpStatus::Error => "error",
        CmpStatus::Identical => "identical",
        CmpStatus::NotCompared | CmpStatus::Pending => "other",
    }
}

/// What a folder report shows of a file besides its status: its differences, or why not. A side
/// the file is missing from is an empty document named where the file would be.
pub(crate) enum FileDiffs {
    None,
    Note(String),
    Diffs(Vec<super::text::TextDoc>, DiffResult),
}

pub(crate) fn file_diffs(it: &DirItem, roots: &[PathBuf], opts: &DiffOptions) -> FileDiffs {
    match file_docs(it, roots, opts) {
        FileDiffs::Diffs(_, result) if result.is_identical() => {
            FileDiffs::Note(tr("report.no_diffs").into())
        }
        found => found,
    }
}

/// Whether `it` is a changed text file, whose differences [`file_diffs`] reads.
pub(crate) fn has_text_diffs(it: &DirItem) -> bool {
    !it.is_dir && !it.binary && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
}

/// Why `it` is listed without its differences, when the sizes found by the compare of its
/// `panes` sides already tell: one is too large.
pub(crate) fn too_big(it: &DirItem, panes: usize) -> Option<String> {
    let largest = it.size.iter().take(panes).flatten().max().copied();
    largest
        .filter(|&s| s > MAX_BYTES)
        .map(|size| trf("report.too_big", &[("size", fmt_size(Some(size)))]))
}

/// Like [`file_diffs`], but files whose lines compare equal are compared too.
pub(crate) fn file_docs(it: &DirItem, roots: &[PathBuf], opts: &DiffOptions) -> FileDiffs {
    if !has_text_diffs(it) {
        return FileDiffs::None;
    }
    let paths: Vec<PathBuf> = roots.iter().map(|r| it.path(r)).collect();
    if let Some(note) = too_big(it, paths.len()) {
        return FileDiffs::Note(note);
    }
    let mut docs = Vec::with_capacity(paths.len());
    for (p, &exists) in paths.iter().zip(&it.exists) {
        if !exists {
            docs.push(super::text::TextDoc::empty(Some(p.clone())));
            continue;
        }
        let bytes = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => return FileDiffs::Note(format!("{}: {e}", tr("status.error"))),
        };
        if bytes.len() as u64 > MAX_BYTES {
            return FileDiffs::Note(trf(
                "report.too_big",
                &[("size", fmt_size(Some(bytes.len() as u64)))],
            ));
        }
        if super::text::looks_binary(&bytes) {
            return FileDiffs::Note(tr("report.binary").into());
        }
        docs.push(super::text::doc_from_bytes(Some(p.clone()), &bytes, None));
    }
    let longest = docs.iter().map(|d| d.lines.len()).max().unwrap_or(0);
    if longest > MAX_LINES {
        return FileDiffs::Note(trf("report.too_long", &[("n", longest.to_string())]));
    }
    let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
    let result = super::diff::compute_with(&refs, opts, super::comments::for_path(&paths[0]));
    FileDiffs::Diffs(docs, result)
}

/// The report of a folder compare: a summary and the files listed, with the differences of every
/// changed text file when `opts` is given (reading the files from `roots`), files with
/// differences first. `display` names the compared folders, and `rules` the compare rules file
/// used ("" = none).
pub fn folder_report(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: Option<&DiffOptions>,
    rules: &str,
) -> String {
    let n = display.len();
    let mut listed: Vec<&DirItem> = items
        .iter()
        .copied()
        .filter(|it| !it.is_dir || it.status == CmpStatus::Unique)
        .collect();
    if opts.is_some() {
        listed.sort_by_key(|it| status_class(it.status) == "identical");
    }
    let count = |st: &str| {
        listed
            .iter()
            .filter(|it| status_class(it.status) == st)
            .count()
    };
    let (same, diff, uniq, err, other) = (
        count("identical"),
        count("different"),
        count("unique"),
        count("error"),
        count("other"),
    );

    let mut files = String::new();
    let mut toc = String::new();
    let mut total_diffs = 0;
    let mut total_ignored = 0;
    let mut any_hunks = false;
    // All files share the size limit of a text report.
    let mut budget = REPORT_ROWS;
    let dopts = opts.cloned().unwrap_or_default();
    for (k, it) in listed.iter().enumerate() {
        let st = status_class(it.status);
        let rel = it.rel_path();
        let diffs = opts.map_or(FileDiffs::None, |o| file_diffs(it, roots, o));
        let open = matches!(diffs, FileDiffs::Diffs(..) | FileDiffs::Note(_));
        let tag = if open { "summary" } else { "div" };
        let dates: Vec<String> = (0..n)
            .map(|s| format!("{}: {}", side_name(s, n), fmt_time(it.mtime[s])))
            .collect();
        let _ = write!(
            files,
            "<{} class=\"file st-{st}\" id=\"f{k}\" data-status=\"{st}\" data-name=\"{}\"><{tag}{} title=\"{}\"><span class=\"dot\"></span><span class=\"path\">{}<span class=\"dir\">{}</span></span>",
            if open { "details" } else { "div" },
            esc(&rel),
            if open { "" } else { " class=\"sum\"" },
            esc(&dates.join("\n")),
            esc(&it.name),
            esc(&it.rel_dir),
        );
        let mut body = String::new();
        match &diffs {
            FileDiffs::Diffs(docs, result) => {
                let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
                let list = hunks(result);
                total_diffs += list.len();
                any_hunks = true;
                if n == 2 {
                    stat_badges(&mut files, &result.stats());
                } else {
                    let _ = write!(
                        files,
                        "<span class=\"badge acc\">{}</span>",
                        esc(&diff_count(list.len()))
                    );
                }
                let ignored = ignored_blocks(result);
                total_ignored += ignored;
                if ignored > 0 {
                    let _ = write!(
                        files,
                        "<span class=\"badge ign\" title=\"{}\">{}</span>",
                        esc(tr("report.ignored")),
                        esc(&trf("report.n_ignored", &[("n", ignored.to_string())]))
                    );
                }
                let _ = write!(
                    toc,
                    "<a href=\"#f{k}\" title=\"{}\"><span>{}</span><span class=\"c\">{}</span></a>",
                    esc(&rel),
                    esc(&it.name),
                    list.len()
                );
                let doc = Doc {
                    panes: &refs,
                    result,
                    opts: &dopts,
                    notes: &[],
                };
                let total = list.len();
                let mut shown = 0;
                for (j, h) in list.into_iter().enumerate() {
                    let cost = h.1.len().min(HUNK_MAX_ROWS);
                    if cost > budget {
                        break;
                    }
                    budget -= cost;
                    shown += 1;
                    hunk_html(&mut body, &doc, &format!("f{k}d{j}"), (j, total), h);
                }
                if shown < total {
                    let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
                    let _ = write!(body, "<div class=\"note\">{}</div>", esc(&more));
                }
            }
            FileDiffs::Note(note) => {
                let _ = write!(body, "<div class=\"note\">{}</div>", esc(note));
            }
            FileDiffs::None => {}
        }
        let _ = write!(
            files,
            "<span class=\"badge\">{}</span>",
            esc(&status_text(it, n))
        );
        let sizes: Vec<String> = (0..n)
            .map(|s| match it.size[s] {
                Some(_) => fmt_size(it.size[s]),
                None => "—".to_owned(),
            })
            .collect();
        if !it.is_dir {
            let _ = write!(
                files,
                "<span class=\"badge\">{}</span>",
                esc(&sizes.join(" · "))
            );
        }
        if open {
            let _ = writeln!(
                files,
                "<span class=\"chev\">›</span></summary><div class=\"body\">{body}</div></details>"
            );
        } else {
            files.push_str("</div></div>\n");
        }
    }

    let mut out = String::new();
    let title = tr("report.title");
    page_start(
        &mut out,
        title,
        &Controls {
            filter: true,
            view: n == 2 && any_hunks,
            expand: any_hunks,
        },
    );
    let has_toc = !toc.is_empty();
    let _ = write!(
        out,
        "<div class=\"layout{}\">",
        if has_toc { "" } else { " single" }
    );
    if has_toc {
        let _ = write!(
            out,
            "<nav class=\"toc\"><h4>{}</h4>{toc}</nav>",
            esc(tr("report.file_diffs"))
        );
    }
    out.push_str("<main>");
    let names: Vec<String> = display.iter().map(|d| d.display().to_string()).collect();
    sides(&mut out, &names);
    rules_note(&mut out, rules);
    let mut cards = vec![
        ("", listed.len().to_string(), tr("report.files")),
        ("ok", same.to_string(), tr("report.identical")),
        ("warn", diff.to_string(), tr("report.different")),
        ("uniq", uniq.to_string(), tr("report.unique")),
    ];
    if err > 0 {
        cards.push(("bad", err.to_string(), tr("report.errors")));
    }
    if other > 0 {
        cards.push(("", other.to_string(), tr("report.not_compared")));
    }
    if any_hunks {
        cards.push(("acc", total_diffs.to_string(), tr("report.differences")));
    }
    if total_ignored > 0 {
        cards.push(("", total_ignored.to_string(), tr("report.ignored")));
    }
    stat_cards(&mut out, &cards);
    out.push_str("<div class=\"chips\">");
    for (st, label, k) in [
        ("all", tr("report.all"), listed.len()),
        ("different", tr("report.different"), diff),
        ("unique", tr("report.unique"), uniq),
        ("identical", tr("report.identical"), same),
        ("error", tr("report.errors"), err),
        ("other", tr("report.not_compared"), other),
    ] {
        if k == 0 && st != "all" {
            continue;
        }
        let _ = write!(
            out,
            "<button class=\"chip{}\" data-status=\"{st}\" onclick=\"oxmStatus(this)\">{}<span class=\"n\">{k}</span></button>",
            if st == "all" { " on" } else { "" },
            esc(label)
        );
    }
    out.push_str("</div>\n");
    out.push_str(&files);
    let _ = write!(
        out,
        "<div id=\"empty\" class=\"empty\"{}>{}</div>",
        if listed.is_empty() { "" } else { " hidden" },
        esc(tr("report.no_match"))
    );
    page_end(&mut out, any_hunks);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::compute;
    use crate::core::filter::FileFilter;
    use crate::core::folder::{FolderOptions, Progress, Scanner};

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hunks_keep_context_short_of_neighbours() {
        let a = v(&[
            "1", "2", "3", "4", "x", "6", "7", "y", "9", "10", "11", "12", "13",
        ]);
        let b = v(&[
            "1", "2", "3", "4", "X", "6", "7", "Y", "9", "10", "11", "12", "13",
        ]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        let h = hunks(&r);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].1, 1..7);
        assert_eq!(h[1].1, 5..11);
    }

    #[test]
    fn text_report_lists_each_difference() {
        let a = v(&["same", "old <word>", "gone", "tail"]);
        let b = v(&["same", "new <word>", "tail", "added"]);
        let opts = DiffOptions::default();
        let r = compute(&[&a, &b], &opts);
        let notes = vec![BTreeMap::from([(2, "why <removed>?".to_string())])];
        let html = text_report(
            &["a.txt".into(), "b.txt".into()],
            &[&a, &b],
            &r,
            &opts,
            &notes,
            "My rules",
        );
        let total = hunks(&r).len();
        assert!(total >= 2, "{html}");
        for k in 0..total {
            assert!(html.contains(&format!("id=\"d{k}\"")));
        }
        assert!(html.contains("why &lt;removed&gt;?"));
        assert!(
            html.contains("<span class=\"m\">old</span> &lt;word&gt;"),
            "{html}"
        );
        assert!(
            html.contains("class=\"d s2 two\"") && html.contains("data-view=\"u\""),
            "two sides have a unified view"
        );
        assert!(html.contains("<b>My rules</b>"));
        assert!(html.trim_end().ends_with("</html>"));

        let same = text_report(
            &["a".into(), "b".into()],
            &[&a, &a],
            &compute(&[&a, &a], &opts),
            &opts,
            &[],
            "",
        );
        assert!(!same.contains("class=\"meta\""));
        assert!(same.contains(&esc(tr("report.no_diffs"))));
    }

    #[test]
    fn text_report_stops_listing_after_its_rows() {
        let a: Vec<String> = (0..60_000).map(|i| i.to_string()).collect();
        let b: Vec<String> = a
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i % 10 == 5 {
                    format!("{l}x")
                } else {
                    l.clone()
                }
            })
            .collect();
        let opts = DiffOptions::default();
        let r = compute(&[&a, &b], &opts);
        let total = hunks(&r).len();
        let html = text_report(&["a".into(), "b".into()], &[&a, &b], &r, &opts, &[], "");
        let shown = html.matches(" id=\"d").count();
        assert!(shown > 0 && shown < total, "{shown} of {total}");
        let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
        assert!(html.contains(&esc(&more)));

        let added = text_report(
            &["a".into(), "b".into()],
            &[&a[..3], &a[..4]],
            &compute(&[&a[..3], &a[..4]], &opts),
            &opts,
            &[],
            "",
        );
        assert!(added.contains("<span class=\"c\">4</span>"), "{added}");
    }

    #[test]
    fn three_sides_colour_only_the_sides_that_changed() {
        let (l, m, r) = (v(&["a", "L"]), v(&["a", "b"]), v(&["a", "b"]));
        let opts = DiffOptions::default();
        let res = compute(&[&l, &m, &r], &opts);
        let row = res.rows.iter().position(|x| x.block.is_some()).unwrap();
        let classes: Vec<&str> = row_cells(&res, &[&l, &m, &r], &opts, row)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(classes, ["chg", "ctx", "ctx"]);

        let both = v(&["a", "B"]);
        let res = compute(&[&both, &m, &both], &opts);
        let row = res.rows.iter().position(|x| x.block.is_some()).unwrap();
        let classes: Vec<&str> = row_cells(&res, &[&both, &m, &both], &opts, row)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(classes, ["chg", "chg", "chg"]);
    }

    #[test]
    fn folder_reports_share_one_size_limit() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let lines: Vec<String> = (0..20_000).map(|i| format!("{i}\n")).collect();
        let changed: String = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i % 10 == 5 {
                    format!("x{l}")
                } else {
                    l.clone()
                }
            })
            .collect();
        for f in ["1.txt", "2.txt", "3.txt"] {
            std::fs::write(a.path().join(f), lines.concat()).unwrap();
            std::fs::write(b.path().join(f), &changed).unwrap();
        }
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: &DiffOptions::default(),
            progress: &Progress::default(),
        }
        .run();
        let refs: Vec<&DirItem> = items.iter().collect();
        let html = folder_report(&refs, &roots, &roots, Some(&DiffOptions::default()), "");
        let shown = html.matches("class=\"hunk").count();
        assert!(shown > 0 && shown < 3 * 2000, "{shown}");
        assert!(html.contains("data-name=\"3.txt\""));
        let more = trf("report.more_diffs", &[("n", "2000".into())]);
        assert!(html.contains(&esc(&more)), "the last file lists none");
    }

    #[test]
    fn folder_report_with_and_without_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("same.txt"), "x\n").unwrap();
        std::fs::write(b.path().join("same.txt"), "x\n").unwrap();
        std::fs::write(a.path().join("changed.txt"), "one\ntwo\n").unwrap();
        std::fs::write(b.path().join("changed.txt"), "one\n2\n").unwrap();
        std::fs::write(b.path().join("new.txt"), "fresh\n").unwrap();
        let long: String = (0..=MAX_LINES).map(|i| format!("{i}\n")).collect();
        std::fs::write(a.path().join("long.txt"), &long).unwrap();
        std::fs::write(b.path().join("long.txt"), long + "end\n").unwrap();
        std::fs::write(b.path().join("new.bin"), b"PNG\0\x01\x02").unwrap();
        // Only the last word differs, after the cut; short enough to be compared by words.
        let wide = "w ".repeat(LINE_CHARS / 2 + 100);
        std::fs::write(a.path().join("wide.txt"), format!("{wide}a\n")).unwrap();
        std::fs::write(b.path().join("wide.txt"), format!("{wide}b\n")).unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: &DiffOptions::default(),
            progress: &Progress::default(),
        }
        .run();
        let refs: Vec<&DirItem> = items.iter().collect();
        let opts = DiffOptions::default();
        let html = folder_report(&refs, &roots, &roots, Some(&opts), "");
        assert!(html.contains("data-name=\"changed.txt\""), "{html}");
        assert!(
            html.contains("<td class=\"del\"><span class=\"m\">two</span></td>"),
            "{html}"
        );
        assert!(html.contains("data-status=\"identical\" data-name=\"same.txt\""));
        assert!(html.contains("fresh"));
        let too_long = trf("report.too_long", &[("n", (MAX_LINES + 2).to_string())]);
        assert!(html.contains(&esc(&too_long)));
        assert!(html.contains(&esc(tr("report.binary"))), "{html}");
        assert!(html.contains("<span class=\"m\">…</span></td>"));
        assert!(!html.contains(&"w ".repeat(LINE_CHARS / 2 + 1)));
        assert!(html.contains("class=\"toc\""));

        let plain = folder_report(&refs, &roots, &roots, None, "");
        assert!(!plain.contains("class=\"hunk\"") && !plain.contains("class=\"toc\""));
        assert!(plain.contains("data-name=\"new.txt\""));

        // Files not compared yet are not counted as identical.
        let mut pending = items.clone();
        for it in &mut pending {
            it.status = CmpStatus::Pending;
        }
        let refs: Vec<&DirItem> = pending.iter().collect();
        let html = folder_report(&refs, &roots, &roots, None, "");
        assert!(!html.contains("data-status=\"identical\""));
        assert!(html.contains("data-status=\"other\""));
        assert!(html.contains(&esc(tr("report.not_compared"))));
    }

    #[test]
    fn reports_name_rules_and_count_ignored_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("f.txt"), "keep\n// v1\nmid\nold\n").unwrap();
        std::fs::write(b.path().join("f.txt"), "keep\n// v2\nmid\nnew\n").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions {
            line_filters_enabled: true,
            line_filters: vec!["^// ".into()],
            ..DiffOptions::default()
        };
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: &opts,
            progress: &Progress::default(),
        }
        .run();
        let refs: Vec<&DirItem> = items.iter().collect();
        let html = folder_report(&refs, &roots, &roots, Some(&opts), "/x/ucie.oxrules");
        assert!(html.contains("<b>ucie</b>"), "{html}");
        let one = trf("report.n_ignored", &[("n", "1".into())]);
        assert!(html.contains(&esc(&one)), "{html}");
    }
}
