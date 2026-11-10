//! HTML reports of compare results: one self-contained page with a summary, the compared files
//! and every difference with the lines around it, for folder and text compares.

use super::diff::{inline_diff, BlockKind, DiffOptions, DiffResult, Stats, WhitespaceMode};
use super::folder::{CmpStatus, CompareMethod, DirItem};
use super::report::{esc, esc_marked, fmt_size, fmt_time, side_name, status_text};
use super::text::TextDoc;
use crate::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Unchanged lines shown before and after each difference.
pub(crate) const CONTEXT: usize = 3;
/// Rows of one difference shown at most; the rest are summed up in a note.
pub(crate) const HUNK_MAX_ROWS: usize = 400;
/// Text files longer than this are listed without their differences.
pub(crate) const MAX_LINES: usize = 50_000;
/// Files larger than this are listed in folder reports without their differences.
pub(crate) const MAX_BYTES: u64 = 16 << 20;
/// Characters of a line shown; the rest is cut off.
pub(crate) const LINE_CHARS: usize = 2_000;
/// Rows shown in a text report, all differences together; the differences after them are only
/// counted, keeping the page small enough for a browser.
const REPORT_ROWS: usize = 20_000;
/// Bytes of the files and differences of a folder report at most, at any detail: past them,
/// the differences of the files left are only counted, so a report of many large files is not
/// built beyond what memory holds.
const REPORT_BYTES: usize = 1 << 30;

/// How much of the differences an HTML report shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportDetail {
    /// The counts and the list of differences or files, without their lines.
    Summary,
    /// Up to [`REPORT_ROWS`] rows in all, [`HUNK_MAX_ROWS`] per block and [`LINE_CHARS`]
    /// characters per line.
    #[default]
    Standard,
    /// Every row of every difference, whole lines; the page grows with the differences, a
    /// folder report up to [`REPORT_BYTES`].
    Full,
}

impl ReportDetail {
    pub const ALL: [ReportDetail; 3] = [Self::Summary, Self::Standard, Self::Full];

    /// The name used on the command line and in job files.
    pub fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    /// The name shown to people.
    pub fn label(self) -> &'static str {
        tr(match self {
            Self::Summary => "report.detail_summary",
            Self::Standard => "report.detail_standard",
            Self::Full => "report.detail_full",
        })
    }

    pub(crate) fn limits(self) -> Limits {
        match self {
            Self::Summary => Limits {
                rows: 0,
                hunk: 0,
                chars: LINE_CHARS,
                bytes: REPORT_BYTES,
            },
            Self::Standard => Limits {
                rows: REPORT_ROWS,
                hunk: HUNK_MAX_ROWS,
                chars: LINE_CHARS,
                bytes: REPORT_BYTES,
            },
            Self::Full => Limits {
                rows: usize::MAX,
                hunk: usize::MAX,
                chars: usize::MAX,
                bytes: REPORT_BYTES,
            },
        }
    }
}

/// The rows and characters an HTML report shows at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Rows of all differences together.
    pub rows: usize,
    /// Rows of one difference.
    pub hunk: usize,
    /// Characters of one line.
    pub chars: usize,
    /// Bytes of the files listed in a folder report, with their differences.
    pub bytes: usize,
}
/// Rows of a table difference read field by field at most.
const FIELD_ROWS: usize = 50;
/// Field changes listed for one difference at most.
const FIELD_LINES: usize = 12;

pub(crate) const DARK: &str = "--bg:#0f1218;--surface:#171b23;--surface2:#1e232d;--border:#2a303c;\
--text:#e6e9ef;--dim:#98a2b3;--accent:#8b96ff;--accent-bg:#232a4d;--add:#12301f;--add-m:#1f6b3e;\
--del:#3a161b;--del-m:#7a2631;--chg:#33290f;--chg-m:#6b5414;--conf:#4a1717;--ign:#1f2340;\
--gap:#14181f;--ok:#4ade80;--warn:#fbbf24;--bad:#f87171;--uniq:#c084fc;--shadow:0 1px 2px #0006;\
--ok-t:#4ade80;--warn-t:#fbbf24;--bad-t:#f87171;--acc-t:#a5adff;--acc-s:#8b96ff;--on-acc:#0f1218";

pub(crate) const LIGHT: &str = "--bg:#f6f7f9;--surface:#fff;--surface2:#f1f3f6;--border:#e3e6eb;\
--text:#1d2330;--dim:#5d6678;--accent:#5b6cff;--accent-bg:#eef0ff;--add:#e7f7ed;--add-m:#a9e5bf;\
--del:#fdecee;--del-m:#f5b8c1;--chg:#fff5dc;--chg-m:#f6d98a;--conf:#ffe0e0;--ign:#eef0fb;\
--gap:#f7f8fa;--ok:#16a34a;--warn:#d97706;--bad:#dc2626;--uniq:#7c3aed;\
--shadow:0 1px 2px #1018280f,0 1px 3px #1018281a;\
--ok-t:#166534;--warn-t:#92400e;--bad-t:#b91c1c;--acc-t:#3f4fd9;--acc-s:#3f4fd9;--on-acc:#fff";

const CSS: &str = r#"
:root{--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;color-scheme:light}
[data-theme=dark]{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans","PingFang TC","Microsoft JhengHei",sans-serif}
.vh{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap;border:0}
.nojs .js{display:none!important}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.top{position:sticky;top:0;z-index:5;display:flex;gap:12px;align-items:center;flex-wrap:wrap;padding:10px 24px;background:var(--surface);background:color-mix(in srgb,var(--surface) 88%,transparent);backdrop-filter:blur(10px);border-bottom:1px solid var(--border)}
.brand{display:flex;align-items:center;gap:10px;margin-right:auto;min-width:0}
.brand h1{margin:0;font-weight:650;font-size:15px;line-height:1.3}
.logo{width:30px;height:30px;flex:none;border-radius:7px;display:block;transition:transform .35s cubic-bezier(.3,1.6,.5,1)}
.brand:hover .logo{transform:rotate(-12deg) scale(1.1)}
.brand small{font-weight:400;color:var(--dim);font-size:12px}
.ctl{display:flex;gap:8px;align-items:center;flex-wrap:wrap}
.search{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
select.qm,input.q{padding:6px 10px;border:1px solid var(--border);border-radius:8px;background:var(--surface);color:var(--text);font:inherit;font-size:13px}
input.q{width:200px}
input.q:focus,select.qm:focus{outline:2px solid var(--accent);outline-offset:-1px}
button{font:inherit;font-size:13px;padding:5px 11px;border-radius:8px;border:1px solid var(--border);background:var(--surface);color:var(--text);cursor:pointer}
button:hover{border-color:var(--accent)}
button:disabled{opacity:.45;cursor:default}button:disabled:hover{border-color:var(--border)}
.nav{display:inline-flex;align-items:center;gap:6px}
.nav .pos{min-width:5.5em;text-align:center;color:var(--dim);font-size:13px;font-variant-numeric:tabular-nums}
.seg{display:inline-flex;border:1px solid var(--border);border-radius:8px;overflow:hidden}
.seg button{border:0;border-radius:0}.seg button.on{background:var(--accent-bg);color:var(--acc-t);font-weight:600}
.layout{display:grid;grid-template-columns:260px minmax(0,1fr);gap:24px;max-width:1600px;margin:0 auto;padding:20px 24px 48px}
.layout.single{grid-template-columns:minmax(0,1fr)}
.toc{position:sticky;top:var(--top,70px);align-self:start;max-height:calc(100vh - var(--top,70px) - 20px);overflow:auto;background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:12px;box-shadow:var(--shadow)}
.toc summary{list-style:none;cursor:pointer}.toc summary::-webkit-details-marker{display:none}
.toc summary::after{content:'›';float:right;color:var(--dim);transition:transform .15s}.toc details[open]>summary::after{transform:rotate(90deg)}
.toc h2{display:inline;margin:0;font-size:12px;text-transform:uppercase;letter-spacing:.06em;color:var(--dim)}
.toc details>div{margin-top:8px}
.toc a{display:flex;justify-content:space-between;gap:8px;padding:4px 8px;border-radius:6px;color:var(--text);text-decoration:none;font-size:13px}
.toc a:hover{background:var(--surface2)}.toc a span:first-child{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}
.toc a:hover span:first-child,.toc a:focus-visible span:first-child{white-space:normal;word-break:break-all}
.toc a .c{color:var(--dim);font-variant-numeric:tabular-nums;white-space:nowrap}
.toc a .c.so{font-size:11.5px;font-style:italic}
.sides{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:12px;margin-bottom:16px}
.side{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:10px 14px;box-shadow:var(--shadow);min-width:0}
.side b{display:inline-block;font-size:11px;text-transform:uppercase;letter-spacing:.05em;padding:1px 8px;border-radius:99px;background:var(--accent-bg);color:var(--acc-t);margin-bottom:4px}
.side div{font-family:var(--mono);font-size:12.5px;word-break:break-all}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin-bottom:16px}
.stat{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:12px 14px;box-shadow:var(--shadow)}
.stat .v{font-size:24px;font-weight:700;font-variant-numeric:tabular-nums}.stat .l{color:var(--dim);font-size:12.5px}
.stat.ok .v{color:var(--ok)}.stat.warn .v{color:var(--warn)}.stat.bad .v{color:var(--bad)}.stat.uniq .v{color:var(--uniq)}.stat.acc .v{color:var(--accent)}
.box{background:var(--surface);border:1px solid var(--border);border-radius:12px;margin-bottom:16px;box-shadow:var(--shadow);font-size:13px}
.box>summary{padding:10px 14px;cursor:pointer;font-weight:600}
.box h2{margin:0;font-size:13px;font-weight:600}
.omit{padding:10px 14px}.omit ul{margin:4px 0 0;padding-left:18px}.omit:not(.complete){border-left:4px solid var(--warn)}
.omit.complete h2{color:var(--ok-t)}.omit .pol{margin:6px 0 0;color:var(--dim)}
.sumlist{margin:0;padding:10px 14px 10px 44px;font-size:13px}.sumlist li{padding:3px 0}.sumlist .pos{color:var(--dim);font-family:var(--mono)}.sumlist .pos b{color:var(--text);font-family:system-ui,sans-serif;font-weight:600;margin-right:3px}
.box dl{display:grid;grid-template-columns:max-content minmax(0,1fr);gap:4px 16px;margin:0;padding:4px 14px 14px}
.box dt{color:var(--dim)}.box dd{margin:0;word-break:break-word}.box dd ul{margin:0;padding-left:18px}
.box code{font-family:var(--mono);font-size:12px}
.chips{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:8px}
.chip{border-radius:99px;padding:4px 12px}.chip.on{background:var(--acc-s);border-color:var(--acc-s);color:var(--on-acc)}
.chip .n{opacity:.8;margin-left:4px;font-variant-numeric:tabular-nums}
.count{color:var(--dim);font-size:13px;margin:0 0 12px}
.file{background:var(--surface);border:1px solid var(--border);border-radius:12px;margin-bottom:10px;box-shadow:var(--shadow);overflow:hidden;scroll-margin-top:var(--top,80px)}
.file>summary,.file>.sum{display:flex;flex-wrap:wrap;align-items:center;gap:6px 10px;padding:10px 14px;list-style:none;cursor:pointer}
.file>.sum{cursor:default}.file>summary::-webkit-details-marker{display:none}
.file>summary:hover{background:var(--surface2)}
.dot{width:9px;height:9px;border-radius:50%;flex:none;background:var(--dim)}
.st-identical .dot{background:var(--ok)}.st-different .dot{background:var(--warn)}.st-unique .dot{background:var(--uniq)}.st-error .dot{background:var(--bad)}
.path{min-width:0;flex:1 1 220px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.file>summary:hover .path,.file>summary:focus-visible .path,.file>.sum:hover .path{white-space:normal;word-break:break-all}
.path .dir{color:var(--dim);margin-left:8px;font-size:12.5px}
.badge{font-size:12px;padding:1px 9px;border-radius:99px;background:var(--surface2);color:var(--dim);white-space:nowrap;font-variant-numeric:tabular-nums}
.badge.add{background:var(--add);color:var(--ok-t)}.badge.del{background:var(--del);color:var(--bad-t)}.badge.chg{background:var(--chg);color:var(--warn-t)}
.badge.acc{background:var(--accent-bg);color:var(--acc-t)}.badge.ign{background:var(--ign);color:var(--dim)}
.badge.meth{background:transparent;border:1px solid var(--border);color:var(--text)}
.badge.so{background:transparent;border:1px dashed var(--dim);color:var(--text)}
.badge.lim{background:var(--chg);color:var(--warn-t)}
.why{padding:0 14px 10px 33px;color:var(--dim);font-size:13px}
.meta{display:flex;gap:8px;flex-wrap:wrap;margin:-4px 0 16px;font-size:13px;color:var(--dim)}
.meta span{padding:3px 12px;border-radius:99px;background:var(--surface);border:1px solid var(--border)}.meta b{color:var(--text)}
.chev{width:18px;margin-left:auto;text-align:center;color:var(--dim);transition:transform .15s}
details[open]>summary .chev{transform:rotate(90deg)}
.body{padding:4px 14px 14px;border-top:1px solid var(--border)}
.note{color:var(--dim);font-size:13px;padding:10px 2px}
.comments{margin:10px 0;padding:8px 12px;border-radius:10px;background:var(--chg);font-size:13px}
.comments li{margin:2px 0}
.hunk{margin-top:12px;border:1px solid var(--border);border-radius:10px;overflow:hidden;scroll-margin-top:var(--top,80px)}
.hunk.focus{outline:2px solid var(--accent);outline-offset:1px}
.hh{display:flex;gap:10px;align-items:center;flex-wrap:wrap;padding:6px 10px;background:var(--surface2);border-bottom:1px solid var(--border);font-size:12.5px}
.hh .pos{color:var(--dim);font-family:var(--mono)}.hh .pos b{color:var(--text);font-family:system-ui,sans-serif;font-weight:600;margin-right:3px}
.hh .loc{color:var(--text)}
.hh .num{margin-left:auto;color:var(--dim)}.hh a{color:inherit;text-decoration:none}
.kind{font-weight:600;font-size:11.5px;padding:1px 8px;border-radius:99px}
.k-chg{background:var(--chg);color:var(--warn-t)}.k-add{background:var(--add);color:var(--ok-t)}.k-del{background:var(--del);color:var(--bad-t)}.k-conf{background:var(--conf);color:var(--bad-t)}
ul.fields{margin:0;padding:6px 10px 6px 28px;border-bottom:1px solid var(--border);font-size:13px}ul.fields li{margin:1px 0}
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
.foot{color:var(--dim);font-size:12px;text-align:center;margin-top:24px}.foot a{color:inherit}
kbd{font-family:var(--mono);font-size:11px;padding:0 5px;border:1px solid var(--border);border-bottom-width:2px;border-radius:4px;background:var(--surface)}
@media (max-width:900px){.top{padding:8px 12px}.layout{grid-template-columns:minmax(0,1fr);gap:12px;padding:12px 12px 40px}.toc{position:static;max-height:none}input.q{width:150px}}
@media (max-width:600px){.top,.ctl{gap:6px}.ctl,.search{flex:1 1 100%;min-width:0}.search{flex-wrap:nowrap}input.q{flex:1;width:auto;min-width:0}select.qm{max-width:45%;min-width:0}.nav .lbl{display:none}.file>summary,.file>.sum{flex-wrap:wrap}.path{flex-basis:calc(100% - 30px)}.why{padding-left:14px}.box dl{grid-template-columns:minmax(0,1fr)}.box dd{margin-bottom:6px}}
[hidden]{display:none!important}
@keyframes oxm-in{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:none}}
@keyframes oxm-pop{from{opacity:0;transform:scale(.6) rotate(-20deg)}to{opacity:1;transform:none}}
@keyframes oxm-open{from{opacity:0;transform:translateY(-4px)}to{opacity:1;transform:none}}
@keyframes oxm-pulse{0%{box-shadow:0 0 0 0 color-mix(in srgb,var(--accent) 45%,transparent)}100%{box-shadow:0 0 0 10px transparent}}
@keyframes oxm-flash{0%,100%{filter:none}40%{filter:brightness(.9) saturate(1.6)}}
.logo{animation:oxm-pop .6s cubic-bezier(.3,1.6,.5,1) both}
.sides,.stats,.meta,.chips,.toc,.file,.empty,.box{animation:oxm-in .4s ease-out both}
.stats{animation-delay:.05s}.meta,.chips,.box{animation-delay:.1s}.toc{animation-delay:.12s}
.file:nth-of-type(1){animation-delay:.14s}.file:nth-of-type(2){animation-delay:.18s}.file:nth-of-type(3){animation-delay:.22s}
.file:nth-of-type(4){animation-delay:.26s}.file:nth-of-type(5){animation-delay:.3s}.file:nth-of-type(n+6){animation-delay:.34s}
.stat,.side,.file{transition:transform .18s,box-shadow .18s,border-color .18s}
.stat:hover,.side:hover{transform:translateY(-2px);border-color:var(--accent)}
.file:hover{border-color:color-mix(in srgb,var(--accent) 45%,var(--border))}
button,.toc a,.chip,.file>summary{transition:background-color .15s,border-color .15s,color .15s,transform .1s}
button:active{transform:scale(.96)}
details[open]>.body{animation:oxm-open .25s ease-out}
.hunk.focus{animation:oxm-pulse .8s ease-out}.hunk.focus .hh{animation:oxm-flash .8s ease-out}
.hunk:target{animation:oxm-pulse .8s ease-out}
@media (prefers-reduced-motion:reduce){*,*::before,*::after{animation:none!important;transition:none!important;scroll-behavior:auto!important}}
@media print{*,*::before,*::after{animation:none!important;transition:none!important}}
@media print{*{-webkit-print-color-adjust:exact;print-color-adjust:exact}.top{position:static}.top .ctl,.toc,.js,.count{display:none}.layout{grid-template-columns:1fr}.file{break-inside:avoid-page;box-shadow:none}body{background:#fff}}
"#;

const JS: &str = r##"
(function(){
const root=document.documentElement,$=s=>[...document.querySelectorAll(s)],all=(e,s)=>[...e.querySelectorAll(s)],id=x=>document.getElementById(x);
root.classList.remove('nojs');
const calm=matchMedia('(prefers-reduced-motion: reduce)').matches,how=()=>calm?'auto':'smooth';
let T={};try{T=JSON.parse(id('oxm-t').textContent)}catch(e){}
const fmt=(k,o)=>String(T[k]||'').replace(/\{(\w+)\}/g,(m,x)=>x in o?o[x]:m);
const bar=document.querySelector('.top'),top=()=>bar?bar.offsetHeight:0;
const setTop=()=>root.style.setProperty('--top',(top()+10)+'px');setTop();addEventListener('resize',setTop);
try{const t=localStorage.getItem('oxm-theme');if(t)root.dataset.theme=t}catch(e){}
function dark(){return root.dataset.theme?root.dataset.theme==='dark':matchMedia('(prefers-color-scheme: dark)').matches}
window.oxmTheme=function(){root.dataset.theme=dark()?'light':'dark';try{localStorage.setItem('oxm-theme',root.dataset.theme)}catch(e){}};
function unify(t){const u=document.createElement('table');u.className='d u';
u.innerHTML='<colgroup><col style="width:4.5em"><col style="width:4.5em"><col style="width:1.6em"><col></colgroup>';
const h=t.rows[0]&&t.rows[0].cells;if(h&&h.length===4){const r=u.createTHead().insertRow();r.innerHTML='<th>'+h[0].innerHTML+'</th><th>'+h[2].innerHTML+'</th><th></th><th>'+h[1].innerHTML+' / '+h[3].innerHTML+'</th>'}
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
let cur=null;
function setView(v){if(v==='u')$('table.s2.two').forEach(t=>{const x=t.nextElementSibling;if(!x||!x.matches('table.u'))t.after(unify(t))});
document.body.classList.toggle('unified',v==='u');$('[data-view]').forEach(b=>{const on=b.dataset.view===v;b.classList.toggle('on',on);b.setAttribute('aria-pressed',on)});
try{localStorage.setItem('oxm-view',v)}catch(e){}if(cur&&!cur.hidden)cur.scrollIntoView({block:'start'})}
window.oxmView=setView;
try{if(localStorage.getItem('oxm-view')==='u')setView('u')}catch(e){}
window.oxmOpen=function(o){$('details.file').forEach(d=>d.open=o)};
let opened=null,printing=false;const printed=()=>$('details.file,details.settings');
addEventListener('beforeprint',()=>{printing=true;$('.stat .v[data-v]').forEach(v=>v.textContent=v.dataset.v);opened=printed().map(d=>d.open);printed().forEach(d=>d.open=true)});
addEventListener('afterprint',()=>{if(opened)printed().forEach((d,i)=>d.open=opened[i]);opened=null});
let status='all',mode='path',toc;
const q=id('q'),qm=id('qm'),count=id('count'),files=$('.file[data-status]'),words=new Map();
function text(h){let t=words.get(h);if(t==null){t=all(h,'table.s2 tr:not(.more)>td:not(.n)').map(c=>c.textContent).join('\n').toLowerCase();words.set(h,t)}return t}
function tocLink(x){if(!toc)toc=new Map($('.toc a').map(a=>[a.getAttribute('href').slice(1),a]));return toc.get(x)}
function apply(){if(!files.length){nav();return}
const v=q?q.value.trim().toLowerCase():'',inText=!!v&&mode==='text';let nf=0,nb=0;
files.forEach(f=>{let ok=status==='all'||f.dataset.status===status,hits=0;const hs=all(f,'.hunk');
if(inText){hs.forEach(h=>{const m=text(h).includes(v);h.hidden=!m;if(m)hits++});if(!hits)ok=false}
else{hs.forEach(h=>h.hidden=false);if(v&&!f.dataset.name.toLowerCase().includes(v))ok=false}
f.hidden=!ok;if(ok){nf++;nb+=inText?hits:hs.length}const a=tocLink(f.id);if(a)a.hidden=!ok});
const e=id('empty');if(e)e.hidden=nf>0;const s=id('scope');if(s)s.hidden=!inText;
if(count)count.textContent=inText?fmt('blocks',{b:nb,f:nf}):fmt('files',{s:nf,n:files.length});
const c=id('qc');if(c)c.disabled=!v&&status==='all';nav()}
function pick(st){status=st;$('.chip').forEach(c=>{const on=c.dataset.status===st;c.classList.toggle('on',on);c.setAttribute('aria-pressed',on)})}
window.oxmStatus=function(b){pick(b.dataset.status);apply()};
function setMode(m){mode=m==='text'?'text':'path';if(qm){qm.value=mode;const o=qm.options[qm.selectedIndex];if(q&&o)q.setAttribute('aria-label',o.textContent)}apply()}
if(q)q.addEventListener('input',apply);if(qm)qm.addEventListener('change',()=>setMode(qm.value));
window.oxmClear=function(){if(q)q.value='';pick('all');apply();if(q)q.focus()};
const prev=id('prev'),next=id('next'),pos=id('pos');
const shown=()=>$('.hunk').filter(h=>!h.hidden&&!(h.closest('.file')||{}).hidden);
function nav(){if(!pos)return;const hs=shown(),i=cur?hs.indexOf(cur):-1;pos.textContent=(i+1)+' / '+hs.length;
pos.setAttribute('aria-label',fmt('pos',{i:i+1,n:hs.length}));if(prev)prev.disabled=!hs.length||i===0;if(next)next.disabled=!hs.length||i===hs.length-1}
function mark(h){$('.hunk.focus').forEach(x=>x.classList.remove('focus'));void h.offsetWidth;h.classList.add('focus');cur=h}
function go(h,b){const d=h.closest('details');if(d)d.open=true;mark(h);h.scrollIntoView({block:'start',behavior:b});nav();try{history.replaceState(null,'','#'+h.id)}catch(e){}}
function jump(dir){const hs=shown();if(!hs.length)return;let i=cur?hs.indexOf(cur):-1,k;
if(i>=0)k=i+dir;else{const lim=top()+2,y=h=>{const d=h.closest('details');return(d&&!d.open?d:h).getBoundingClientRect().top};let j=hs.findIndex(h=>y(h)>lim);if(j<0)j=hs.length;k=dir>0?j:j-1}
go(hs[Math.max(0,Math.min(hs.length-1,k))],how())}
window.oxmJump=jump;
function reveal(x){const t=x&&id(x);if(!t)return;const f=t.closest('.file');
if((f&&f.hidden)||t.hidden){if(q)q.value='';pick('all');apply()}
if(t.classList.contains('hunk')){go(t,'auto');return}
if(t.tagName==='DETAILS')t.open=true;const d=t.parentElement&&t.parentElement.closest('details');if(d)d.open=true;t.scrollIntoView({block:'start'})}
function fromHash(){let h=location.hash.slice(1);try{h=decodeURIComponent(h)}catch(e){}reveal(h)}
addEventListener('hashchange',fromHash);
const td=id('toc-d'),narrow=matchMedia('(max-width:900px)');if(td&&narrow.matches)td.open=false;
document.addEventListener('click',e=>{const a=e.target.closest('a[href^="#"]');if(!a)return;const x=a.getAttribute('href').slice(1);
if(td&&narrow.matches&&td.contains(a))td.open=false;if(x===location.hash.slice(1)){e.preventDefault();reveal(x)}});
document.addEventListener('keydown',e=>{const g=e.target.tagName;if(g==='INPUT'||g==='SELECT'||g==='TEXTAREA'||e.ctrlKey||e.metaKey||e.altKey)return;
const k=e.key.toLowerCase();if(k==='n'||k==='j')jump(1);else if(k==='p'||k==='k')jump(-1);else if(k==='/'&&q){e.preventDefault();q.focus()}});
if(!calm&&window.requestAnimationFrame)$('.stat .v').forEach(v=>{const t=v.textContent,m=/^(\d[\d,]*)$/.exec(t.trim());if(!m)return;
const n=+m[1].replace(/,/g,'');if(!(n>1))return;const comma=m[1].includes(','),t0=performance.now(),ms=700;v.dataset.v=t;
const f=x=>comma?x.toLocaleString('en-US'):String(x);
const step=now=>{const p=printing?1:Math.min(1,(now-t0)/ms),e=1-Math.pow(1-p,3);v.textContent=p<1?f(Math.round(n*e)):t;if(p<1)requestAnimationFrame(step)};
v.textContent=f(0);requestAnimationFrame(step)});
setMode(qm?qm.value:'path');
if(location.hash)setTimeout(fromHash,0);
})();
"##;

/// The differences to list, each with the rows shown for it: its own and up to `CONTEXT`
/// unchanged rows around it, short of the next or previous difference.
pub(crate) fn hunks(result: &DiffResult) -> Vec<(usize, Range<usize>)> {
    hunks_with(result, CONTEXT)
}

/// Like [`hunks`], with up to `context` unchanged rows around each difference.
pub(crate) fn hunks_with(result: &DiffResult, context: usize) -> Vec<(usize, Range<usize>)> {
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
            let from = blk.first_row.saturating_sub(context).max(prev);
            let to = end(blk).saturating_add(context).min(next).max(end(blk));
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
    let block = row.block().map(|b| &result.blocks[b]);
    let mut cells: Vec<(&'static str, Vec<Range<usize>>)> = (0..n)
        .map(|p| {
            let class = match block {
                None => "ctx",
                Some(b) if b.ignored => "ign",
                Some(_) if row.line(p).is_none() => "gap",
                Some(_) if n == 2 => ["del", "add"][p],
                Some(b) if b.kind == BlockKind::Conflict => "conf",
                Some(_) => "chg",
            };
            (class, Vec::new())
        })
        .collect();
    let text = |p: usize| row.line(p).map(|l| panes[p][l].as_str());
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

/// What a report reads from a file besides the text it compared, read once, when first wanted.
#[derive(Debug)]
pub(crate) struct Later<T> {
    path: Option<PathBuf>,
    value: std::sync::Arc<std::sync::OnceLock<Option<T>>>,
}

/// A copy shares what the reading has read.
impl<T> Clone for Later<T> {
    fn clone(&self) -> Later<T> {
        Later {
            path: self.path.clone(),
            value: self.value.clone(),
        }
    }
}

impl<T> Later<T> {
    fn new(path: Option<&Path>) -> Later<T> {
        Later {
            path: path.map(Path::to_path_buf),
            value: Default::default(),
        }
    }

    fn get(&self, read: impl FnOnce(&Path) -> Option<T>) -> Option<&T> {
        self.value
            .get_or_init(|| self.path.as_deref().and_then(read))
            .as_ref()
    }
}

/// Two readings are alike whatever each has read so far.
impl<T> PartialEq for Later<T> {
    fn eq(&self, _: &Later<T>) -> bool {
        true
    }
}

/// The records of a spreadsheet read the other way from its compared text, line for line.
#[derive(Debug)]
pub(crate) struct OtherCells {
    lines: Vec<String>,
    /// Whether the lines have the values as shown rather than as stored.
    shown: bool,
}

/// The line each page of a PDF file starts on in its compared text, and that text's line count.
type Pages = (Vec<usize>, usize);

/// How the text of a file was read for its comparison.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum ReadAs {
    Text,
    /// The source of a web page, compared as text.
    Html,
    /// The text of a PDF, Word, OpenDocument or RTF file, with where the pages of a PDF file
    /// start.
    Document(Later<Pages>),
    /// The records of a spreadsheet, with its values read the other way.
    Spreadsheet(Later<OtherCells>),
    /// The cells of a Jupyter notebook, without their outputs.
    Notebook,
    /// The header and sections of an ELF program.
    Elf,
    /// The output of a converter the user configured, named by its program.
    Converted(String),
    /// JSON or XML reformatted before comparing.
    Formatted,
    /// Compared by its bytes, so no text was read.
    Binary,
}

impl ReadAs {
    /// How `doc` was read, as [`super::unpacker::load_compare_doc`] does with `opts`.
    pub(crate) fn of(doc: &TextDoc, opts: &DiffOptions) -> ReadAs {
        use super::extract::Kind;
        use super::unpacker::Converter;
        let path = doc.path.as_deref();
        if !doc.compare_only {
            let ext = path.map(super::text::extension).unwrap_or_default();
            return if matches!(ext.as_str(), "html" | "htm" | "xhtml") {
                ReadAs::Html
            } else {
                ReadAs::Text
            };
        }
        match path.and_then(|p| super::unpacker::find(opts, p)) {
            Some(Converter::Command(u)) => ReadAs::Converted(program(&u.command)),
            Some(Converter::BuiltIn(k)) => match k {
                Kind::Pdf => ReadAs::Document(Later::new(path)),
                Kind::Document | Kind::Rtf => ReadAs::Document(Later::new(None)),
                Kind::Spreadsheet => ReadAs::Spreadsheet(Later::new(path)),
                Kind::Notebook => ReadAs::Notebook,
                Kind::Elf => ReadAs::Elf,
            },
            None => ReadAs::Formatted,
        }
    }

    pub(crate) fn label(&self) -> String {
        let key = match self {
            ReadAs::Text => "report.read_text",
            ReadAs::Html => "report.read_html",
            ReadAs::Document(_) => "report.read_document",
            ReadAs::Spreadsheet(_) => "report.read_sheet",
            ReadAs::Notebook => "report.read_notebook",
            ReadAs::Elf => "report.read_elf",
            ReadAs::Converted(program) => {
                return trf("report.read_converted", &[("program", program.clone())])
            }
            ReadAs::Formatted => "report.read_formatted",
            ReadAs::Binary => "report.read_binary",
        };
        tr(key).to_string()
    }

    /// Whether the lines compared are not the file's own, so their numbers are not either.
    pub(crate) fn extracted(&self) -> bool {
        !matches!(self, ReadAs::Text | ReadAs::Html | ReadAs::Binary)
    }
}

/// The file name of the program a converter's command line runs, without its folder. Settings
/// may come from another system, so both `/` and `\` end a folder.
fn program(command: &str) -> String {
    let command = command.trim_start();
    let first = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    let first = first.trim_end_matches(['/', '\\']);
    first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_string()
}

/// Where line `at` of `lines`, read as `read`, is in the file, when the reading says so: the
/// sheet and rows of a spreadsheet, the pages of a PDF file, or the kind of notebook cell.
fn location(read: &ReadAs, lines: &[String], range: &Range<usize>) -> Option<String> {
    let at = range.start.min(lines.len().checked_sub(1)?);
    match read {
        ReadAs::Document(pages) => {
            let (starts, count) = pages.get(|path| super::extract::pdf_pages(path, None).ok())?;
            if *count != lines.len() || starts.is_empty() {
                return None;
            }
            let page = |line: usize| starts.partition_point(|&s| s <= line).max(1);
            let last = (range.start + range.len().max(1) - 1).min(lines.len() - 1);
            let (a, b) = (page(at), page(last));
            Some(if a == b {
                trf("report.loc_page", &[("n", a.to_string())])
            } else {
                trf(
                    "report.loc_pages",
                    &[("a", a.to_string()), ("b", b.to_string())],
                )
            })
        }
        ReadAs::Spreadsheet(_) => {
            let heading = lines[..=at]
                .iter()
                .rposition(|l| super::extract::is_sheet_heading(l));
            // A sheet keeps its rows in place, one line each, from the line after its heading.
            let (sheet, first) = match heading {
                Some(h) if h == range.start => return Some(lines[h][2..].to_string()),
                Some(h) => (Some(lines[h][2..].to_string()), range.start - h),
                None => (None, range.start + 1),
            };
            let rows = match range.len() {
                0 | 1 => trf("report.loc_row", &[("n", first.to_string())]),
                n => trf(
                    "report.loc_rows",
                    &[("a", first.to_string()), ("b", (first + n - 1).to_string())],
                ),
            };
            Some(match sheet {
                Some(s) => format!("{s} · {rows}"),
                None => rows,
            })
        }
        ReadAs::Notebook => lines[..=at]
            .iter()
            .rev()
            .find(|l| l.starts_with("── ") && l.ends_with(" ──"))
            .map(|l| l.trim_matches(['─', ' ']).to_string()),
        _ => None,
    }
}

/// The delimiter of the records a side holds when they are a table: a CSV or TSV file, or a
/// spreadsheet read as comma separated records.
fn delimiter(read: &ReadAs, doc: &TextDoc) -> Option<char> {
    match read {
        ReadAs::Spreadsheet(_) => Some(','),
        ReadAs::Text => doc.path.as_deref().and_then(super::table::delimiter_for),
        _ => None,
    }
}

/// The changes of a two-sided difference between table rows, field by field, as
/// "row · column · old → new": for rows changed in place, whose sides have the header line of
/// their table in common and as many fields as it, a sheet's row ending in empty cells
/// included. A row is named by its first field that is not a number; a row whose name changed
/// is left to the lines shown. A sheet's column is named by both rows of a header over two
/// rows, or by its letters when it has no name, and a sheet's value is shown as the sheet shows
/// it, with what it stores when that differs.
fn field_changes(reads: &[ReadAs], docs: &[TextDoc], ranges: &[Range<usize>]) -> Vec<String> {
    let (Some(da), Some(db)) = (
        delimiter(&reads[0], &docs[0]),
        delimiter(&reads[1], &docs[1]),
    ) else {
        return Vec::new();
    };
    let (ra, rb) = (&ranges[0], &ranges[1]);
    if da != db || ra.len() != rb.len() || ra.is_empty() || ra.len() > FIELD_ROWS {
        return Vec::new();
    }
    let sheet = [0, 1].map(|p| matches!(reads[p], ReadAs::Spreadsheet(_)));
    // A sheet's header is its first row with more than one value. Below a title, a row that
    // holds numbers is taken for a row of values, as in a sheet of names and values, rather
    // than a header.
    let header = |p: usize, lines: &[String], at: usize, delimiter: char| -> Option<usize> {
        let first = lines[..at.min(lines.len())]
            .iter()
            .rposition(|l| super::extract::is_sheet_heading(l))
            .map_or(0, |h| h + 1);
        let mut h = first;
        if sheet[p] {
            let row = |h: usize| super::table::split_row(&lines[h], delimiter);
            while h < at && row(h).iter().filter(|f| !f.trim().is_empty()).count() < 2 {
                h += 1;
            }
            if h > first && h < at && row(h).iter().any(|f| f.trim().parse::<f64>().is_ok()) {
                return None;
            }
        }
        (h < at).then_some(h)
    };
    let (la, lb) = (&docs[0].lines, &docs[1].lines);
    let (Some(ha), Some(hb)) = (header(0, la, ra.start, da), header(1, lb, rb.start, db)) else {
        return Vec::new();
    };
    let names = super::table::split_row(&la[ha], da);
    if names.len() < 2 || names != super::table::split_row(&lb[hb], db) {
        return Vec::new();
    }
    // The records of a sheet leave out the empty cells they end with.
    let pad = |mut row: Vec<String>, width: usize| {
        if row.len() < width {
            row.resize(width, String::new());
        }
        row
    };
    let fields = |p: usize, line: &str, delimiter: char| -> Vec<String> {
        let row = super::table::split_row(line, delimiter);
        if sheet[p] {
            pad(row, names.len())
        } else {
            row
        }
    };
    // A sheet's header over two rows, as one whose cells span the columns of the row under
    // it: a row of names without numbers, the same on both sides, that names columns the
    // header leaves unnamed.
    let sub: Option<Vec<String>> = (sheet == [true, true]
        && ha + 1 < ra.start
        && hb + 1 < rb.start
        && la[ha + 1] == lb[hb + 1])
        .then(|| fields(0, &la[ha + 1], da))
        .filter(|sub| {
            sub.len() == names.len()
                && !sub.iter().any(|f| f.trim().parse::<f64>().is_ok())
                && (0..sub.len()).any(|c| names[c].is_empty() && !sub[c].is_empty())
        });
    let column = |c: usize| -> String {
        let below = sub.as_ref().map_or("", |s| s[c].as_str());
        let mut top = names[c].as_str();
        if top.is_empty() && !below.is_empty() {
            // A name spans the columns after it that have a name below them.
            let sub = sub.as_deref().unwrap_or_default();
            top = (0..c)
                .rev()
                .take_while(|&j| !sub[j].is_empty())
                .find(|&j| !names[j].is_empty())
                .map_or("", |j| names[j].as_str());
        }
        match (top, below) {
            ("", "") if sheet[0] => column_letters(c),
            (t, "") => t.to_string(),
            ("", b) => b.to_string(),
            (t, b) => format!("{t} {b}"),
        }
    };
    let other: Vec<Option<&OtherCells>> = (0..2)
        .map(|p| match &reads[p] {
            ReadAs::Spreadsheet(cells) => cells
                .get(|path| {
                    let o = super::extract::other_cells(path, None).ok()?;
                    Some(OtherCells {
                        lines: super::text::split_lines(&o.text).0,
                        shown: o.shown,
                    })
                })
                .filter(|o| o.lines.len() == docs[p].lines.len()),
            _ => None,
        })
        .collect();
    // A field as shown, with the value it stores when that is not what it shows.
    let value = |p: usize, line: usize, fields: &[String], c: usize| -> String {
        let read = &fields[c];
        let both = other[p]
            .and_then(|o| {
                let row = super::table::split_row(o.lines.get(line)?, ',');
                Some((pad(row, fields.len()), o.shown))
            })
            .filter(|(o, _)| o.len() == fields.len());
        let (shown, stored) = match &both {
            Some((o, true)) => (o[c].as_str(), read.as_str()),
            Some((o, false)) => (read.as_str(), o[c].as_str()),
            None => (read.as_str(), read.as_str()),
        };
        match (shown.is_empty(), stored.is_empty() || stored == shown) {
            (true, true) => "—".to_string(),
            (true, false) => stored.to_string(),
            (false, true) => shown.to_string(),
            (false, false) => format!("{shown} ({stored})"),
        }
    };
    let mut out = Vec::new();
    let mut more = 0;
    for (ia, ib) in ra.clone().zip(rb.clone()) {
        let (Some(a), Some(b)) = (la.get(ia), lb.get(ib)) else {
            continue;
        };
        let (a, b) = (fields(0, a, da), fields(1, b, db));
        if a.len() != names.len() || b.len() != names.len() {
            continue;
        }
        let name = (0..a.len())
            .find(|&j| !a[j].trim().is_empty() && a[j].trim().parse::<f64>().is_err())
            .unwrap_or(0);
        if a[name] != b[name] {
            continue;
        }
        for c in (0..a.len()).filter(|&c| c != name && a[c] != b[c]) {
            if out.len() == FIELD_LINES {
                more += 1;
                continue;
            }
            let column = column(c);
            out.push(format!(
                "{} · {column} · {} → {}",
                a[name],
                value(0, ia, &a, c),
                value(1, ib, &b, c)
            ));
        }
    }
    if more > 0 {
        out.push(trf("report.more_fields", &[("n", more.to_string())]));
    }
    out
}

/// The letters a spreadsheet names column `c` by, counting from zero: A to Z, then AA.
fn column_letters(mut c: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// A compared text: the lines of each side, their differences, the options they were found
/// with, bookmark comments, and whether the lines were read from the files rather than being
/// theirs.
struct Doc<'a> {
    limits: Limits,
    panes: &'a [&'a [String]],
    result: &'a DiffResult,
    opts: &'a DiffOptions,
    notes: &'a [BTreeMap<usize, String>],
    extracted: bool,
}

/// What a difference shows besides its lines: where it is in the file, and its changes field
/// by field.
#[derive(Default)]
pub(crate) struct Extra {
    pub(crate) location: Option<String>,
    pub(crate) fields: Vec<String>,
}

/// The extras of block `b` of the compare of `docs`, each side read as in `reads`: its location
/// on the first side it has lines on (the base of a three-way compare first), and the field
/// changes of a two-way compare.
pub(crate) fn block_extra(
    reads: &[ReadAs],
    docs: &[TextDoc],
    result: &DiffResult,
    b: usize,
) -> Extra {
    let n = docs.len();
    let ranges = &result.blocks[b].ranges;
    let order: &[usize] = if n == 3 { &[1, 0, 2] } else { &[0, 1] };
    let side = order
        .iter()
        .copied()
        .find(|&p| ranges.get(p).is_some_and(|r| !r.is_empty()))
        .unwrap_or(order[0]);
    Extra {
        location: reads
            .get(side)
            .zip(docs.get(side))
            .and_then(|(r, d)| location(r, &d.lines, &ranges[side])),
        fields: if n == 2 && reads.len() == 2 {
            field_changes(reads, docs, ranges)
        } else {
            Vec::new()
        },
    }
}

/// How a listed file was read: as on the first side it is on, as binary when it was not read,
/// or `None` when nothing says.
pub(crate) fn file_read(it: &DirItem, diffs: &FileDiffs, reads: &[ReadAs]) -> Option<ReadAs> {
    match diffs {
        FileDiffs::Diffs(..) => (0..it.exists.len())
            .find(|&s| it.exists[s])
            .and_then(|s| reads.get(s).cloned()),
        _ if it.binary && !it.is_dir => Some(ReadAs::Binary),
        FileDiffs::Note(note) if *note == tr("report.binary") => Some(ReadAs::Binary),
        _ => None,
    }
}

/// Why a changed file shows no differences, when it shows none: its note, or what kept it from
/// being compared as text. `None` for a file with differences or one that did not change.
pub(crate) fn summary_reason(
    it: &DirItem,
    n: usize,
    diffs: &FileDiffs,
    with_diffs: bool,
    redact: &Redact,
) -> Option<String> {
    let unread = (!with_diffs).then(|| tr("report.why_scope").to_string());
    reason(it, n, diffs, unread, redact)
}

/// `bytes` as a whole number of GiB where it is one, else of MiB.
fn size_label(bytes: usize) -> String {
    if bytes >= 1 << 30 && bytes.is_multiple_of(1 << 30) {
        format!("{} GiB", bytes >> 30)
    } else {
        format!("{} MiB", bytes >> 20)
    }
}

/// As [`summary_reason`]; `unread` says why the file was not read, when it was not.
fn reason(
    it: &DirItem,
    n: usize,
    diffs: &FileDiffs,
    unread: Option<String>,
    redact: &Redact,
) -> Option<String> {
    let changed = matches!(status_class(it.status), "different" | "unique" | "error");
    match diffs {
        _ if !changed => None,
        FileDiffs::Diffs(..) => None,
        FileDiffs::Note(note) => Some(redact.apply(note)),
        FileDiffs::None => Some(if it.status == CmpStatus::Error {
            redact.apply(&status_text(it, n))
        } else if let Some(unread) = unread {
            unread
        } else if it.is_dir {
            tr("report.why_folder").to_string()
        } else if it.binary {
            tr("report.binary").to_string()
        } else {
            tr("report.why_unknown").to_string()
        }),
    }
}

/// Writes one difference; whether any of its rows or lines were left out.
fn hunk_html(
    out: &mut String,
    doc: &Doc,
    id: &str,
    (index, total): (usize, usize),
    (b, rows): (usize, Range<usize>),
    extra: &Extra,
) -> bool {
    let Doc {
        limits,
        panes,
        result,
        opts,
        notes,
        extracted,
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
    out.push_str("</span>");
    if let Some(loc) = &extra.location {
        let _ = write!(out, "<span class=\"loc\">{}</span>", esc(loc));
    }
    let _ = write!(
        out,
        "<a class=\"num\" href=\"#{id}\">{}</a></div>",
        esc(&trf(
            "report.diff_n",
            &[("i", (index + 1).to_string()), ("n", total.to_string())]
        ))
    );
    if !extra.fields.is_empty() {
        out.push_str("<ul class=\"fields\">");
        for f in &extra.fields {
            let _ = write!(out, "<li>{}</li>", esc(f));
        }
        out.push_str("</ul>");
    }
    out.push_str("<div class=\"scroll\">");
    let shown = rows.start..rows.end.min(rows.start.saturating_add(limits.hunk));
    let hidden = rows.len() - shown.len();
    let mut cut_lines = false;

    // Side by side; for two sides the page builds a unified table from this one.
    let _ = write!(
        out,
        "<table class=\"d s2{}\"><colgroup>",
        if n == 2 { " two" } else { "" }
    );
    for _ in 0..n {
        out.push_str("<col style=\"width:4.5em\"><col>");
    }
    // In a <thead>, printed pages repeat the sides above the rows they carry on with.
    out.push_str("</colgroup><thead><tr>");
    let line_key = if extracted {
        "report.line_of_extracted"
    } else {
        "report.line_of"
    };
    for p in 0..n {
        let side = side_name(p, n);
        let _ = write!(
            out,
            "<th scope=\"col\"><span class=\"vh\">{}</span></th><th scope=\"col\">{}</th>",
            esc(&trf(line_key, &[("side", side.to_string())])),
            esc(side)
        );
    }
    out.push_str("</tr></thead><tbody>");
    for r in shown {
        out.push_str("<tr>");
        let cells = row_cells(result, panes, opts, r);
        for (p, (lines, (class, marks))) in panes.iter().zip(&cells).enumerate() {
            match result.rows[r].line(p) {
                Some(l) => {
                    let line = &lines[l];
                    let cut = line.char_indices().nth(limits.chars).map(|(i, _)| i);
                    cut_lines |= cut.is_some();
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
            let note = result.rows[r]
                .line(p)
                .and_then(|l| marks.get(&l).map(|t| (l, t)));
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
    out.push_str("</tbody></table>");
    out.push_str("</div></section>\n");
    hidden > 0 || cut_lines
}

pub(crate) fn ignored_blocks(result: &DiffResult) -> usize {
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
                "<span class=\"badge {class}\" title=\"{0}\" aria-label=\"{0}: {n}\">{sign}{n}</span>",
                esc(tr(key))
            );
        }
    }
}

/// The app icon the reports carry, as shipped in `images/icon.png`.
const ICON_PNG: &[u8] = include_bytes!("../../images/icon.png");

/// The app icon scaled to `size` pixels square, as RGBA.
pub(crate) fn icon_rgba(size: u32) -> Option<image::RgbaImage> {
    let img = image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png).ok()?;
    Some(
        img.resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .into_rgba8(),
    )
}

/// The icon as a `data:` URI, so a report stays one self-contained file. It is
/// shrunk to 64 px (crisp at the 30 px it is shown on a 2x screen), which keeps
/// it to a few KB instead of the 80 KB original.
pub(crate) fn icon_data_uri() -> &'static str {
    static URI: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    URI.get_or_init(|| {
        let mut png = Vec::new();
        let ok = icon_rgba(64).is_some_and(|img| {
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .is_ok()
        });
        let bytes = if ok { &png[..] } else { ICON_PNG };
        format!("data:image/png;base64,{}", base64(bytes))
    })
}

/// Standard base64 with padding.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

struct Controls {
    filter: bool,
    view: bool,
    expand: bool,
    nav: bool,
}

fn page_start(out: &mut String, title: &str, c: &Controls) {
    let _ = writeln!(
        out,
        "<!DOCTYPE html>\n<html lang=\"{}\" class=\"nojs\"><head><meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
        esc(crate::i18n::current_language())
    );
    let _ = writeln!(out, "<title>{}</title>", esc(title));
    let _ = writeln!(
        out,
        "<style>:root{{{LIGHT}}}{CSS}[data-theme=dark]{{{DARK}}}@media (prefers-color-scheme:dark){{:root:not([data-theme=light]){{{DARK};color-scheme:dark}}}}@media print{{:root:not(.oxm){{{LIGHT};color-scheme:light}}}}</style></head><body>"
    );
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let _ = write!(
        out,
        "<header class=\"top\"><div class=\"brand\"><img class=\"logo\" alt=\"\" src=\"{}\"><div><h1>{}</h1><small>{}</small></div></div><div class=\"ctl\">",
        icon_data_uri(),
        esc(title),
        esc(&trf("report.generated", &[("date", now.to_string())]))
    );
    if c.filter {
        let path = esc(tr("report.search_paths"));
        let _ = write!(
            out,
            "<div class=\"search js\" role=\"search\"><select id=\"qm\" class=\"qm\" aria-label=\"{}\"><option value=\"path\">{path}</option><option value=\"text\">{}</option></select><input id=\"q\" class=\"q\" type=\"search\" aria-label=\"{path}\"><button id=\"qc\" type=\"button\" onclick=\"oxmClear()\">{}</button></div>",
            esc(tr("report.search_mode")),
            esc(tr("report.search_text")),
            esc(tr("report.clear"))
        );
    }
    if c.nav {
        let _ = write!(
            out,
            "<div class=\"nav js\"><button id=\"prev\" type=\"button\" onclick=\"oxmJump(-1)\" aria-label=\"{0}\" title=\"{0} (P)\">‹<span class=\"lbl\"> {1}</span></button><span id=\"pos\" class=\"pos\">0 / 0</span><button id=\"next\" type=\"button\" onclick=\"oxmJump(1)\" aria-label=\"{2}\" title=\"{2} (N)\"><span class=\"lbl\">{3} </span>›</button></div>",
            esc(tr("report.prev")),
            esc(tr("report.prev_short")),
            esc(tr("report.next")),
            esc(tr("report.next_short"))
        );
    }
    if c.view {
        let _ = write!(
            out,
            "<div class=\"seg js\" role=\"group\" aria-label=\"{}\"><button type=\"button\" class=\"on\" aria-pressed=\"true\" data-view=\"s\" onclick=\"oxmView('s')\">{}</button><button type=\"button\" aria-pressed=\"false\" data-view=\"u\" onclick=\"oxmView('u')\">{}</button></div>",
            esc(tr("report.view")),
            esc(tr("report.side_by_side")),
            esc(tr("report.unified"))
        );
    }
    if c.expand {
        let _ = write!(
            out,
            "<button type=\"button\" class=\"js\" onclick=\"oxmOpen(true)\">{}</button><button type=\"button\" class=\"js\" onclick=\"oxmOpen(false)\">{}</button>",
            esc(tr("report.expand_all")),
            esc(tr("report.collapse_all"))
        );
    }
    let _ = writeln!(
        out,
        "<button type=\"button\" class=\"js\" onclick=\"oxmTheme()\" title=\"{0}\" aria-label=\"{0}\">◐</button></div></header>",
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

/// The wording the page's script fills in, as JSON safe inside a `<script>` element.
fn templates() -> String {
    let words: BTreeMap<&str, &str> = [
        ("files", tr("report.showing_files")),
        ("blocks", tr("report.matching_blocks")),
        ("pos", tr("report.diff_n")),
    ]
    .into_iter()
    .collect();
    serde_json::to_string(&words)
        .unwrap_or_default()
        .replace('<', "\\u003c")
}

fn page_end(out: &mut String, hunks: bool) {
    out.push_str("<p class=\"foot\">");
    if hunks {
        let _ = write!(out, "{} · ", tr("report.keys"));
    }
    let _ = writeln!(
        out,
        "{}</p></main></div><script type=\"application/json\" id=\"oxm-t\">{}</script><script>{JS}</script></body></html>",
        made_by_html(),
        templates()
    );
}

/// "Made by OxideMerge …", with *OxideMerge* linked to the project page.
pub(crate) fn made_by_html() -> String {
    let made_by = esc(&trf(
        "report.made_by",
        &[("version", env!("CARGO_PKG_VERSION").to_string())],
    ));
    let link = format!(
        "<a href=\"{}\">OxideMerge</a>",
        env!("CARGO_PKG_REPOSITORY")
    );
    made_by.replacen("OxideMerge", &link, 1)
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
    detail: ReportDetail,
) -> String {
    let n = panes.len();
    let limits = detail.limits();
    let list = hunks(result);
    let total = list.len();
    // The differences shown with their lines, in order, until the rows run out.
    let mut rows = 0;
    let shown = list
        .iter()
        .take_while(|(_, r)| {
            rows += r.len().min(limits.hunk);
            rows <= limits.rows
        })
        .count();
    let shown = if detail == ReportDetail::Summary {
        0
    } else {
        // One difference is always shown, however long.
        shown.max(1).min(total)
    };
    // A summary lists where each difference is, as many as the rows a standard report holds.
    let listed = if detail == ReportDetail::Summary {
        total.min(REPORT_ROWS)
    } else {
        shown
    };
    let mut out = String::new();
    let title = tr("report.text_title");
    page_start(
        &mut out,
        title,
        &Controls {
            filter: false,
            view: n == 2 && shown > 0,
            expand: false,
            nav: shown > 0,
        },
    );
    // The line of the base side, or of the first side that has lines here.
    let position = |b: usize| {
        let ranges = &result.blocks[b].ranges;
        let order: &[usize] = if n == 3 { &[1, 0, 2] } else { &[0, 1] };
        let range = order
            .iter()
            .map(|&p| &ranges[p])
            .find(|r| !r.is_empty())
            .unwrap_or(&ranges[order[0]]);
        line_range(range)
    };
    let toc = shown > 0;
    let _ = write!(
        out,
        "<div class=\"layout{}\">",
        if toc { "" } else { " single" }
    );
    if toc {
        let heading = esc(tr("report.differences"));
        let _ = write!(
            out,
            "<nav class=\"toc\" aria-label=\"{heading}\"><details id=\"toc-d\" open><summary><h2>{heading}</h2></summary><div>"
        );
        for (k, (b, _)) in list.iter().take(shown).enumerate() {
            let (_, label) = kind(result, *b);
            let _ = write!(
                out,
                "<a href=\"#d{k}\"><span>{}. {}</span><span class=\"c\">{}</span></a>",
                k + 1,
                esc(label),
                position(*b)
            );
        }
        out.push_str("</div></details></nav>");
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
    if listed < total {
        cards.push((
            "",
            (total - listed).to_string(),
            tr("report.blocks_left_out"),
        ));
    }
    stat_cards(&mut out, &cards);

    // The differences come first, so the box above them can count what they left out.
    let mut body = String::new();
    let mut cut = 0;
    if total == 0 {
        let _ = write!(
            body,
            "<div class=\"file\"><div class=\"empty\">✓ {}</div></div>",
            esc(tr("report.no_diffs"))
        );
    } else if shown == 0 {
        // A summary: where each difference is, without its lines.
        let _ = write!(
            body,
            "<div class=\"file\"><ol class=\"sumlist\" aria-label=\"{}\">",
            esc(tr("report.differences"))
        );
        for (k, (b, _)) in list.iter().take(listed).enumerate() {
            let (kclass, label) = kind(result, *b);
            let blk = &result.blocks[*b];
            let sides: Vec<String> = blk
                .ranges
                .iter()
                .enumerate()
                .take(n)
                .map(|(p, r)| format!("<b>{}</b>{}", esc(side_name(p, n)), line_range(r)))
                .collect();
            let _ = write!(
                body,
                "<li id=\"d{k}\"><span class=\"kind k-{kclass}\">{}</span> <span class=\"pos\">{}</span></li>",
                esc(label),
                sides.join(" &nbsp;")
            );
        }
        body.push_str("</ol>");
        if listed < total {
            let more = trf("report.more_diffs", &[("n", (total - listed).to_string())]);
            let _ = write!(body, "<div class=\"note\">{}</div>", esc(&more));
        }
        body.push_str("</div>");
    } else {
        body.push_str("<div class=\"file\"><div class=\"body\" style=\"border-top:0\">");
        let doc = Doc {
            limits,
            panes,
            result,
            opts,
            notes,
            extracted: false,
        };
        for (k, h) in list.into_iter().take(shown).enumerate() {
            let id = format!("d{k}");
            if hunk_html(&mut body, &doc, &id, (k, total), h, &Extra::default()) {
                cut += 1;
            }
        }
        if shown < total {
            let more = trf("report.more_diffs", &[("n", (total - shown).to_string())]);
            let _ = write!(body, "<div class=\"note\">{}</div>", esc(&more));
        }
        body.push_str("</div></div>");
    }
    let mut omissions = Vec::new();
    if detail == ReportDetail::Summary && total > 0 {
        omissions.push(trf(
            "report.omit_detail_summary",
            &[("n", listed.to_string())],
        ));
    }
    if listed < total {
        omissions.push(trf(
            "report.omit_limit",
            &[
                ("n", (total - listed).to_string()),
                ("rows", fmt_size(Some(REPORT_ROWS as u64))),
            ],
        ));
    }
    if cut > 0 {
        omissions.push(cut_text(cut, limits));
    }
    omissions_html(&mut out, &omissions, detail);

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
    out.push_str(&body);
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
        // A binary file read as text differs in bytes its text leaves out.
        FileDiffs::Diffs(docs, result) if result.is_identical() && it.binary => {
            let read = docs
                .first()
                .map(|d| ReadAs::of(d, opts).label())
                .unwrap_or_default();
            FileDiffs::Note(trf("report.same_text_bytes_differ", &[("read", read)]))
        }
        FileDiffs::Diffs(_, result) if result.is_identical() => {
            FileDiffs::Note(tr("report.no_diffs").into())
        }
        found => found,
    }
}

/// Whether `it` is a changed file whose differences [`file_diffs`] reads: a text file, or a
/// binary one that a converter or a built-in reader turns into text.
pub(crate) fn has_text_diffs(it: &DirItem, roots: &[PathBuf], opts: &DiffOptions) -> bool {
    let readable = || {
        roots
            .iter()
            .zip(&it.exists)
            .find(|(_, &e)| e)
            .is_some_and(|(r, _)| super::unpacker::find(opts, &it.path(r)).is_some())
    };
    !it.is_dir
        && matches!(it.status, CmpStatus::Different | CmpStatus::Unique)
        && (!it.binary || readable())
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
    if !has_text_diffs(it, roots, opts) {
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
        let doc = match super::unpacker::load_compare_doc(p, opts, None) {
            Ok(doc) => doc,
            Err(e) => return FileDiffs::Note(format!("{}: {e:#}", tr("status.error"))),
        };
        if !doc.read_only && doc.lines.iter().any(|l| l.contains('\0')) {
            return FileDiffs::Note(tr("report.binary").into());
        }
        docs.push(doc);
    }
    let longest = docs.iter().map(|d| d.lines.len()).max().unwrap_or(0);
    if longest > MAX_LINES {
        return FileDiffs::Note(trf("report.too_long", &[("n", longest.to_string())]));
    }
    let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
    let mut result = super::diff::compute_with(&refs, opts, super::comments::for_path(&paths[0]));
    super::diff::include_doc_formats(&mut result, &docs.iter().collect::<Vec<_>>(), opts);
    FileDiffs::Diffs(docs, result)
}

/// What a folder report says about how it was made, besides the files and the options they
/// were compared with.
#[derive(Clone, Default, Debug)]
pub struct FolderReportInfo {
    /// The method the folders were compared with, when known.
    pub method: Option<CompareMethod>,
    /// The file filter, as written; empty for none.
    pub filter: String,
    /// Which of the compared files the report has, when not all of them.
    pub scope: Option<String>,
    /// Name the compared folders by their side only, and keep their paths out of the report.
    pub hide_paths: bool,
    /// How much of the differences the report shows.
    pub detail: ReportDetail,
}

/// Replaces the compared folders' paths in a text with the names of their sides.
pub(crate) struct Redact(Vec<(String, String)>);

impl Redact {
    pub(crate) fn new(on: bool, roots: &[PathBuf], display: &[PathBuf]) -> Redact {
        let n = display.len();
        let mut pairs = Vec::new();
        if on {
            for (s, root) in roots.iter().chain(display).enumerate() {
                let label = format!("[{}]", side_name(s % n.max(1), n));
                let text = root.display().to_string();
                for form in [
                    text.clone(),
                    text.replace('\\', "/"),
                    text.replace('/', "\\"),
                ] {
                    let form = form.trim_end_matches(['/', '\\']).to_string();
                    if form.len() > 1 && !pairs.iter().any(|(f, _)| *f == form) {
                        pairs.push((form, label.clone()));
                    }
                }
            }
            // A longer path first, so that one inside another is replaced whole.
            pairs.sort_by_key(|(f, _): &(String, String)| std::cmp::Reverse(f.len()));
        }
        Redact(pairs)
    }

    pub(crate) fn apply(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (from, to) in &self.0 {
            s = s.replace(from.as_str(), to);
        }
        s
    }
}

/// The value of a setting of a folder report: text, or a list, either in the code font.
pub(crate) struct Setting {
    pub(crate) items: Vec<String>,
    pub(crate) code: bool,
    pub(crate) list: bool,
}

impl Setting {
    fn text(s: impl Into<String>) -> Setting {
        Setting {
            items: vec![s.into()],
            code: false,
            list: false,
        }
    }

    fn code(s: impl Into<String>) -> Setting {
        Setting {
            code: true,
            ..Setting::text(s)
        }
    }

    /// A list, or "None" when it is empty.
    fn list(items: Vec<String>, code: bool) -> Setting {
        if items.is_empty() {
            Setting::text(tr("report.none"))
        } else {
            Setting {
                items,
                code,
                list: true,
            }
        }
    }
}

/// The settings of a folder report: how the files were compared and what was exported, each as
/// a label and its value. `context` and `limits` name the lines shown around each difference and
/// the size limits of the report, when differences were exported.
pub(crate) fn settings_rows(
    opts: Option<&DiffOptions>,
    info: &FolderReportInfo,
    redact: &Redact,
    context: String,
    limits: String,
) -> Vec<(&'static str, Setting)> {
    let mut rows: Vec<(&'static str, Setting)> = Vec::new();
    let scope = info
        .scope
        .clone()
        .unwrap_or_else(|| tr("report.scope_all").to_string());
    rows.push((tr("report.set_scope"), Setting::text(redact.apply(&scope))));
    if let Some(m) = info.method {
        rows.push((tr("report.set_method"), Setting::text(tr(m.key()))));
    }
    let filter = info.filter.trim();
    let filter = if matches!(filter, "" | "*" | "*.*") {
        Setting::text(tr("report.all_files"))
    } else {
        Setting::code(redact.apply(filter))
    };
    rows.push((tr("report.set_filter"), filter));
    let list = |items: Vec<String>| Setting::list(items, true);
    if let Some(o) = opts {
        let ws = match o.whitespace {
            WhitespaceMode::Compare => tr("opt.ws_compare"),
            WhitespaceMode::IgnoreChange => tr("opt.ws_change"),
            WhitespaceMode::IgnoreAll => tr("opt.ws_all"),
        };
        rows.push((tr("opt.whitespace"), Setting::text(ws)));
        let ignored: Vec<&str> = [
            (o.ignore_case, "opt.ignore_case"),
            (o.ignore_blank_lines, "opt.ignore_blank"),
            (o.ignore_eol, "opt.ignore_eol"),
            (o.ignore_numbers, "opt.ignore_numbers"),
            (o.ignore_comments, "opt.ignore_comments"),
            (o.ignore_codepage, "opt.ignore_codepage"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, k)| tr(k))
        .collect();
        rows.push((
            tr("report.set_ignored"),
            Setting::text(if ignored.is_empty() {
                tr("report.none").to_string()
            } else {
                ignored.join("; ")
            }),
        ));
        if o.numeric_tolerance_enabled {
            rows.push((
                tr("opt.numeric_tolerance"),
                Setting::text(o.numeric_tolerance.to_string()),
            ));
        }
        if o.line_filters_enabled && !o.line_filters.is_empty() {
            rows.push((tr("report.set_line_filters"), list(o.line_filters.clone())));
        }
        if o.substitutions_enabled && !o.substitutions.is_empty() {
            let subs = o
                .substitutions
                .iter()
                .map(|(a, b)| format!("{a} => {b}"))
                .collect();
            rows.push((tr("report.set_substitutions"), list(subs)));
        }
        if o.block_filters_enabled && !o.block_filters.is_empty() {
            let blocks = o
                .block_filters
                .iter()
                .map(|(a, b)| format!("{a} => {b}"))
                .collect();
            rows.push((tr("report.set_block_filters"), list(blocks)));
        }
        let mut reading: Vec<String> = Vec::new();
        if o.extract_documents {
            reading.push(tr("opt.extract_documents").to_string());
        }
        if o.prettify {
            reading.push(tr("opt.prettify").to_string());
        }
        for u in o.unpackers.iter().filter(|u| u.enabled) {
            reading.push(format!("{} → {}", u.pattern, program(&u.command)));
        }
        rows.push((tr("report.set_reading"), Setting::list(reading, false)));
        rows.push((tr("report.set_context"), Setting::text(context)));
        rows.push((tr("report.set_limits"), Setting::text(limits)));
    }
    rows
}

/// The size limits of an HTML report of `detail`, as the policy it was exported with.
pub(crate) fn limits_text(detail: ReportDetail) -> String {
    let files = [
        ("size", format!("{} MiB", MAX_BYTES >> 20)),
        ("lines", fmt_size(Some(MAX_LINES as u64))),
    ];
    let limits = match detail {
        ReportDetail::Summary => tr("report.limits_summary").to_string(),
        ReportDetail::Full => trf("report.limits_full", &files),
        ReportDetail::Standard => {
            let l = detail.limits();
            let [size, lines] = files;
            trf(
                "report.limits",
                &[
                    ("rows", fmt_size(Some(l.rows as u64))),
                    ("hunk", l.hunk.to_string()),
                    ("chars", fmt_size(Some(l.chars as u64))),
                    size,
                    lines,
                ],
            )
        }
    };
    trf(
        "report.policy",
        &[("detail", detail.label().to_string()), ("limits", limits)],
    )
}

/// What a report says of the `n` blocks it shows shortened to `limits`.
fn cut_text(n: usize, limits: Limits) -> String {
    trf(
        "report.omit_cut",
        &[
            ("n", n.to_string()),
            ("rows", limits.hunk.to_string()),
            ("chars", fmt_size(Some(limits.chars as u64))),
        ],
    )
}

/// The ids of the files of a folder report, made from their paths so that a link to a file
/// keeps working in the next report of the same folders: `f-` and 12 hex digits of a hash of
/// the path, and a number after it in the rare case two paths share them.
fn file_anchors(listed: &[&DirItem]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    listed
        .iter()
        .map(|it| {
            // FNV-1a: the same everywhere, unlike the hasher of the standard library.
            let hash = it
                .rel_path()
                .bytes()
                .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                });
            let base = format!("f-{:012x}", hash >> 16);
            let mut id = base.clone();
            let mut k = 2;
            while !seen.insert(id.clone()) {
                id = format!("{base}-{k}");
                k += 1;
            }
            id
        })
        .collect()
}

/// The box that says whether a report shows everything: the `omissions` when there are any,
/// and the export policy of `detail` either way.
fn omissions_html(out: &mut String, omissions: &[String], detail: ReportDetail) {
    if omissions.is_empty() {
        let _ = write!(
            out,
            "<div class=\"box omit complete\" role=\"note\" id=\"completeness\"><h2>✓ {}</h2>",
            esc(tr("report.complete"))
        );
    } else {
        let _ = write!(
            out,
            "<div class=\"box omit\" role=\"note\" id=\"completeness\"><h2>{}</h2><ul>",
            esc(tr("report.omissions"))
        );
        for o in omissions {
            let _ = write!(out, "<li>{}</li>", esc(o));
        }
        out.push_str("</ul>");
    }
    let _ = writeln!(
        out,
        "<p class=\"pol\">{}</p></div>",
        esc(&limits_text(detail))
    );
}

/// The settings section of a folder report: how the files were compared and what was exported.
fn settings_html(
    out: &mut String,
    opts: Option<&DiffOptions>,
    info: &FolderReportInfo,
    redact: &Redact,
) {
    let _ = write!(
        out,
        "<details class=\"box settings\"><summary>{}</summary>",
        esc(tr("report.settings"))
    );
    let rows = settings_rows(
        opts,
        info,
        redact,
        CONTEXT.to_string(),
        limits_text(info.detail),
    );
    out.push_str(&settings_dl(&rows));
    out.push_str("</details>\n");
}

/// The settings of a report as a definition list.
pub(crate) fn settings_dl(rows: &[(&str, Setting)]) -> String {
    let mut out = String::from("<dl>");
    for (term, value) in rows {
        let item = |i: &String| {
            if value.code {
                format!("<code>{}</code>", esc(i))
            } else {
                esc(i)
            }
        };
        let shown = if value.list {
            let lis: String = value
                .items
                .iter()
                .map(|i| format!("<li>{}</li>", item(i)))
                .collect();
            format!("<ul>{lis}</ul>")
        } else {
            value.items.iter().map(item).collect()
        };
        let _ = write!(out, "<dt>{}</dt><dd>{shown}</dd>", esc(term));
    }
    out.push_str("</dl>");
    out
}

/// The report of a folder compare: a summary and the files listed, with the differences of every
/// changed text file when `opts` is given (reading the files from `roots`), files with
/// differences first. `display` names the compared folders, `rules` the compare rules file
/// used ("" = none), and `info` how the report was made.
pub fn folder_report(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: Option<&DiffOptions>,
    rules: &str,
    info: &FolderReportInfo,
) -> String {
    let limits = info.detail.limits();
    folder_report_within(items, roots, display, opts, rules, info, limits)
}

/// As [`folder_report`], showing at most `limits`.
fn folder_report_within(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: Option<&DiffOptions>,
    rules: &str,
    info: &FolderReportInfo,
    limits: Limits,
) -> String {
    let n = display.len();
    let redact = Redact::new(info.hide_paths, roots, display);
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
    let mut toc_len = 0;
    // Blocks shown, left out by the row limit or the byte limit, and shown with rows or lines
    // cut.
    let (mut shown_blocks, mut left_out, mut left_out_bytes, mut cut_blocks) = (0, 0, 0, 0);
    let mut summary_only = 0;
    let mut total_ignored = 0;
    let mut any_hunks = false;
    // All files share the size limit of a text report; a summary reads no file.
    let shown_opts = opts.filter(|_| info.detail != ReportDetail::Summary);
    let unread = if opts.is_none() {
        Some(tr("report.why_scope").to_string())
    } else if shown_opts.is_none() {
        Some(trf(
            "report.why_detail_summary",
            &[("detail", ReportDetail::Summary.label().to_string())],
        ))
    } else {
        None
    };
    let mut budget = limits.rows;
    let dopts = opts.cloned().unwrap_or_default();
    let anchors = file_anchors(&listed);
    let bytes_size = size_label(limits.bytes);
    for (k, it) in listed.iter().enumerate() {
        let st = status_class(it.status);
        let rel = it.rel_path();
        let anchor = &anchors[k];
        let changed = matches!(st, "different" | "unique" | "error");
        // Past the byte limit no file is read: its differences would not be shown.
        let full = files.len() >= limits.bytes;
        let diffs = match shown_opts {
            Some(o) if !full => file_diffs(it, roots, o),
            _ => FileDiffs::None,
        };
        let unread = match shown_opts {
            Some(_) if full => Some(trf("report.why_bytes", &[("size", bytes_size.clone())])),
            _ => unread.clone(),
        };
        let reads: Vec<ReadAs> = match &diffs {
            FileDiffs::Diffs(docs, _) => docs.iter().map(|d| ReadAs::of(d, &dopts)).collect(),
            _ => Vec::new(),
        };
        let read = file_read(it, &diffs, &reads);
        let mut body = String::new();
        let mut badges = String::new();
        let (mut rendered, mut total) = (0, 0);
        let why = reason(it, n, &diffs, unread, &redact);
        if let FileDiffs::Diffs(docs, result) = &diffs {
            let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
            let list = hunks(result);
            total = list.len();
            any_hunks = true;
            if n == 2 {
                stat_badges(&mut badges, &result.stats());
            } else {
                let _ = write!(
                    badges,
                    "<span class=\"badge acc\">{}</span>",
                    esc(&diff_count(total))
                );
            }
            let ignored = ignored_blocks(result);
            total_ignored += ignored;
            if ignored > 0 {
                let _ = write!(
                    badges,
                    "<span class=\"badge ign\" title=\"{}\">{}</span>",
                    esc(tr("report.ignored")),
                    esc(&trf("report.n_ignored", &[("n", ignored.to_string())]))
                );
            }
            let extracted = read.as_ref().is_some_and(ReadAs::extracted);
            if extracted {
                let _ = write!(
                    body,
                    "<div class=\"note\">{}</div>",
                    esc(tr("report.extracted_lines"))
                );
            }
            let doc = Doc {
                limits,
                panes: &refs,
                result,
                opts: &dopts,
                notes: &[],
                extracted,
            };
            let mut cut = 0;
            let mut full = false;
            for (j, h) in list.into_iter().enumerate() {
                if files.len() + body.len() >= limits.bytes {
                    full = true;
                    break;
                }
                let cost = h.1.len().min(limits.hunk);
                if cost > budget {
                    break;
                }
                budget -= cost;
                rendered += 1;
                let extra = block_extra(&reads, docs, result, h.0);
                if hunk_html(
                    &mut body,
                    &doc,
                    &format!("{anchor}-d{j}"),
                    (j, total),
                    h,
                    &extra,
                ) {
                    cut += 1;
                }
            }
            cut_blocks += cut;
            if rendered < total {
                if full {
                    left_out_bytes += total - rendered;
                } else {
                    left_out += total - rendered;
                }
                let more = trf(
                    "report.more_diffs",
                    &[("n", (total - rendered).to_string())],
                );
                let _ = write!(body, "<div class=\"note\">{}</div>", esc(&more));
                let _ = write!(
                    badges,
                    "<span class=\"badge lim\" title=\"{}\">{}</span>",
                    esc(&more),
                    esc(&trf(
                        "report.n_left_out",
                        &[("n", (total - rendered).to_string())]
                    ))
                );
            }
            if cut > 0 {
                let _ = write!(
                    badges,
                    "<span class=\"badge lim\" title=\"{}\">{}</span>",
                    esc(&trf(
                        "report.cut_note",
                        &[
                            ("rows", limits.hunk.to_string()),
                            ("chars", fmt_size(Some(limits.chars as u64)))
                        ]
                    )),
                    esc(tr("report.shortened"))
                );
            }
        }
        let open = total > 0;
        let only_summary = changed && !open;
        if only_summary {
            summary_only += 1;
            let _ = write!(
                badges,
                "<span class=\"badge so\">{}</span>",
                esc(tr("report.summary_only"))
            );
        }
        if let Some(read) = &read {
            let _ = write!(
                badges,
                "<span class=\"badge meth\" title=\"{}\">{}</span>",
                esc(tr("report.read_as")),
                esc(&read.label())
            );
        }
        shown_blocks += rendered;
        if changed {
            toc_len += 1;
            let count = if open {
                format!("<span class=\"c\">{rendered}</span>")
            } else {
                format!(
                    "<span class=\"c so\">{}</span>",
                    esc(tr("report.summary_only"))
                )
            };
            let _ = write!(
                toc,
                "<a href=\"#{anchor}\" title=\"{}\"><span>{}</span>{count}</a>",
                esc(&rel),
                esc(&it.name),
            );
        }
        let dates: Vec<String> = (0..n)
            .map(|s| format!("{}: {}", side_name(s, n), fmt_time(it.mtime[s])))
            .collect();
        let (outer, tag) = if open {
            ("details", "summary")
        } else {
            ("div", "div")
        };
        let _ = write!(
            files,
            "<{outer} class=\"file st-{st}\" id=\"{anchor}\" data-status=\"{st}\" data-name=\"{}\" data-blocks=\"{rendered}\"><{tag}{} title=\"{}\"><span class=\"dot\" aria-hidden=\"true\"></span><span class=\"path\" title=\"{}\">{}<span class=\"dir\">{}</span></span>{badges}",
            esc(&rel),
            if open { "" } else { " class=\"sum\"" },
            esc(&dates.join("\n")),
            esc(&rel),
            esc(&it.name),
            esc(&it.rel_dir),
        );
        let _ = write!(
            files,
            "<span class=\"badge\">{}</span>",
            esc(&redact.apply(&status_text(it, n)))
        );
        if !it.is_dir {
            let sizes: Vec<String> = (0..n)
                .map(|s| match it.size[s] {
                    Some(_) => fmt_size(it.size[s]),
                    None => "—".to_owned(),
                })
                .collect();
            let _ = write!(
                files,
                "<span class=\"badge\">{}</span>",
                esc(&sizes.join(" · "))
            );
        }
        if open {
            let _ = writeln!(
                files,
                "<span class=\"chev\" aria-hidden=\"true\">›</span></summary><div class=\"body\">{body}</div></details>"
            );
        } else {
            files.push_str("</div>");
            if let Some(why) = why.filter(|_| changed) {
                let _ = write!(files, "<div class=\"why\">{}</div>", esc(&why));
            }
            files.push_str("</div>\n");
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
            nav: shown_blocks > 0,
        },
    );
    let has_toc = toc_len > 0;
    let _ = write!(
        out,
        "<div class=\"layout{}\">",
        if has_toc { "" } else { " single" }
    );
    if has_toc {
        let heading = esc(tr("report.file_diffs"));
        let _ = write!(
            out,
            "<nav class=\"toc\" aria-label=\"{heading}\"><details id=\"toc-d\" open><summary><h2>{heading} ({toc_len})</h2></summary><div>{toc}</div></details></nav>"
        );
    }
    out.push_str("<main>");
    let names: Vec<String> = if info.hide_paths {
        (0..n).map(|s| side_name(s, n).to_string()).collect()
    } else {
        display.iter().map(|d| d.display().to_string()).collect()
    };
    sides(&mut out, &names);
    if info.hide_paths {
        let _ = write!(
            out,
            "<div class=\"meta\"><span>{}</span></div>",
            esc(tr("report.paths_hidden"))
        );
    }
    rules_note(&mut out, rules);
    let mut cards = vec![
        ("", listed.len().to_string(), tr("report.files")),
        ("ok", same.to_string(), tr("report.identical")),
        ("warn", diff.to_string(), tr("report.files_different")),
        ("uniq", uniq.to_string(), tr("report.files_unique")),
    ];
    if err > 0 {
        cards.push(("bad", err.to_string(), tr("report.errors")));
    }
    if other > 0 {
        cards.push(("", other.to_string(), tr("report.not_compared")));
    }
    if shown_opts.is_some() {
        cards.push((
            "acc",
            shown_blocks.to_string(),
            tr("report.blocks_exported"),
        ));
    }
    if left_out + left_out_bytes > 0 {
        cards.push((
            "",
            (left_out + left_out_bytes).to_string(),
            tr("report.blocks_left_out"),
        ));
    }
    if total_ignored > 0 {
        cards.push(("", total_ignored.to_string(), tr("report.ignored")));
    }
    stat_cards(&mut out, &cards);
    let mut omissions = Vec::new();
    if summary_only > 0 {
        omissions.push(trf(
            "report.omit_summary",
            &[("n", summary_only.to_string())],
        ));
    }
    if left_out > 0 {
        omissions.push(trf(
            "report.omit_limit",
            &[
                ("n", left_out.to_string()),
                ("rows", fmt_size(Some(limits.rows as u64))),
            ],
        ));
    }
    if left_out_bytes > 0 {
        omissions.push(trf(
            "report.omit_bytes",
            &[("n", left_out_bytes.to_string()), ("size", bytes_size)],
        ));
    }
    if cut_blocks > 0 {
        omissions.push(cut_text(cut_blocks, limits));
    }
    omissions_html(&mut out, &omissions, info.detail);
    settings_html(&mut out, opts, info, &redact);
    let _ = write!(
        out,
        "<div class=\"chips js\" role=\"group\" aria-label=\"{}\">",
        esc(tr("report.status_filter"))
    );
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
        let on = st == "all";
        let _ = write!(
            out,
            "<button type=\"button\" class=\"chip{}\" aria-pressed=\"{on}\" data-status=\"{st}\" onclick=\"oxmStatus(this)\">{}<span class=\"n\">{k}</span></button>",
            if on { " on" } else { "" },
            esc(label)
        );
    }
    out.push_str("</div>\n");
    let _ = writeln!(
        out,
        "<p id=\"count\" class=\"count\" role=\"status\" aria-live=\"polite\">{}</p>",
        esc(&trf(
            "report.showing_files",
            &[
                ("s", listed.len().to_string()),
                ("n", listed.len().to_string())
            ]
        ))
    );
    let mut scope = tr("report.search_scope").to_string();
    if !omissions.is_empty() {
        scope = format!("{scope} {}", tr("report.search_partial"));
    }
    let _ = writeln!(
        out,
        "<p id=\"scope\" class=\"count\" hidden>{}</p>",
        esc(&scope)
    );
    out.push_str(&files);
    let _ = write!(
        out,
        "<div id=\"empty\" class=\"empty\"{}>{}</div>",
        if listed.is_empty() { "" } else { " hidden" },
        esc(tr("report.no_match"))
    );
    page_end(&mut out, shown_blocks > 0);
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

    /// A reading that has `value` already.
    fn known<T>(value: T) -> Later<T> {
        Later {
            path: None,
            value: std::sync::Arc::new(std::sync::OnceLock::from(Some(value))),
        }
    }

    /// The items of a compare of `roots`.
    fn scan(roots: &[PathBuf], opts: &DiffOptions) -> Vec<DirItem> {
        Scanner {
            roots,
            filter: &FileFilter::default(),
            fopts: &FolderOptions::default(),
            dopts: opts,
            progress: &Progress::default(),
        }
        .run()
    }

    fn report(roots: &[PathBuf], opts: Option<&DiffOptions>, info: &FolderReportInfo) -> String {
        let items = scan(roots, opts.unwrap_or(&DiffOptions::default()));
        let refs: Vec<&DirItem> = items.iter().collect();
        folder_report(&refs, roots, roots, opts, "", info)
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn reports_embed_a_small_icon_and_animate_only_when_motion_is_welcome() {
        let a = v(&["x"]);
        let b = v(&["y"]);
        let opts = DiffOptions::default();
        let r = compute(&[&a, &b], &opts);
        let html = text_report(
            &["a".into(), "b".into()],
            &[&a, &b],
            &r,
            &opts,
            &[],
            "",
            ReportDetail::Standard,
        );
        let start = html
            .find("<img class=\"logo\" alt=\"\" src=\"data:image/png;base64,")
            .expect("icon");
        let uri = &html[start..];
        let len = uri.find("\">").unwrap();
        assert!(
            len < 16_000,
            "the icon is shrunk, not the 80 KB original: {len}"
        );
        let png = icon_rgba(64).unwrap();
        assert_eq!(png.dimensions(), (64, 64));
        for want in [
            "@keyframes oxm-in",
            "prefers-reduced-motion:reduce",
            "prefers-reduced-motion: reduce",
            "oxm-pulse",
        ] {
            assert!(html.contains(want), "{want}");
        }
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
            ReportDetail::Standard,
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
        assert!(html.contains("id=\"prev\"") && html.contains("id=\"next\""));
        assert!(html.trim_end().ends_with("</html>"));

        let same = text_report(
            &["a".into(), "b".into()],
            &[&a, &a],
            &compute(&[&a, &a], &opts),
            &opts,
            &[],
            "",
            ReportDetail::Standard,
        );
        assert!(!same.contains("class=\"meta\""));
        assert!(!same.contains("id=\"prev\""));
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
        let html = text_report(
            &["a".into(), "b".into()],
            &[&a, &b],
            &r,
            &opts,
            &[],
            "",
            ReportDetail::Standard,
        );
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
            ReportDetail::Standard,
        );
        assert!(added.contains("<span class=\"c\">4</span>"), "{added}");
    }

    #[test]
    fn three_sides_colour_only_the_sides_that_changed() {
        let (l, m, r) = (v(&["a", "L"]), v(&["a", "b"]), v(&["a", "b"]));
        let opts = DiffOptions::default();
        let res = compute(&[&l, &m, &r], &opts);
        let row = res.rows.iter().position(|x| x.block().is_some()).unwrap();
        let classes: Vec<&str> = row_cells(&res, &[&l, &m, &r], &opts, row)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(classes, ["chg", "ctx", "ctx"]);

        let both = v(&["a", "B"]);
        let res = compute(&[&both, &m, &both], &opts);
        let row = res.rows.iter().position(|x| x.block().is_some()).unwrap();
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
        let html = report(
            &roots,
            Some(&DiffOptions::default()),
            &FolderReportInfo::default(),
        );
        let shown = html.matches("class=\"hunk").count();
        assert!(shown > 0 && shown < 3 * 2000, "{shown}");
        assert!(html.contains("data-name=\"3.txt\""));
        let more = trf("report.more_diffs", &[("n", "2000".into())]);
        assert!(html.contains(&esc(&more)), "the last file lists none");
        // The exported count is what the page shows; the rest is counted apart, exactly.
        let card = |v: usize, key: &'static str| {
            format!(
                "<div class=\"v\">{v}</div><div class=\"l\">{}</div>",
                esc(tr(key))
            )
        };
        assert!(
            html.contains(&card(shown, "report.blocks_exported")),
            "{shown}"
        );
        assert!(html.contains(&card(3 * 2000 - shown, "report.blocks_left_out")));
        let limit = trf(
            "report.omit_limit",
            &[
                ("n", (3 * 2000 - shown).to_string()),
                ("rows", fmt_size(Some(REPORT_ROWS as u64))),
            ],
        );
        assert!(html.contains(&esc(&limit)));
        assert!(html.contains(&esc(tr("report.search_partial"))));
    }

    #[test]
    fn every_detail_says_what_it_left_out() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // A block of 1,000 changed rows, longer than a standard block, and a long line.
        let lines: Vec<String> = (0..1000).map(|i| format!("{i}\n")).collect();
        let long = "y".repeat(LINE_CHARS + 50);
        std::fs::write(a.path().join("big.txt"), lines.concat()).unwrap();
        std::fs::write(b.path().join("big.txt"), format!("{long}\n")).unwrap();
        std::fs::write(a.path().join("small.txt"), "a\n").unwrap();
        std::fs::write(b.path().join("small.txt"), "b\n").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions::default();
        let with = |detail| {
            report(
                &roots,
                Some(&opts),
                &FolderReportInfo {
                    detail,
                    ..FolderReportInfo::default()
                },
            )
        };
        let complete = esc(tr("report.complete"));
        let policy = |d: ReportDetail| esc(&limits_text(d));

        let standard = with(ReportDetail::Standard);
        let cut = cut_text(1, ReportDetail::Standard.limits());
        assert!(standard.contains(&esc(&cut)), "the long block is counted");
        assert!(!standard.contains(&complete));
        assert!(standard.contains(&policy(ReportDetail::Standard)));
        assert!(!standard.contains(&long));

        let full = with(ReportDetail::Full);
        assert!(full.contains(&complete), "nothing is left out");
        assert!(!full.contains(&esc(tr("report.omissions"))));
        assert!(full.contains(&long), "whole lines");
        assert!(full.contains("<td class=\"n\">1000</td>"), "every row");
        assert!(full.contains(&policy(ReportDetail::Full)));

        let summary = with(ReportDetail::Summary);
        assert!(!summary.contains("class=\"hunk"));
        let omitted = trf("report.omit_summary", &[("n", "2".into())]);
        assert!(summary.contains(&esc(&omitted)), "both files are counted");
        assert!(summary.contains(&policy(ReportDetail::Summary)));
        assert!(summary.len() < standard.len());
        // Differences were asked for: the files say it is the detail that left them out, and
        // no count of exported blocks says none were found.
        let why = trf(
            "report.why_detail_summary",
            &[("detail", ReportDetail::Summary.label().to_string())],
        );
        assert_eq!(
            summary
                .matches(&format!("<div class=\"why\">{}</div>", esc(&why)))
                .count(),
            2
        );
        assert!(!summary.contains(&esc(tr("report.why_scope"))));
        assert!(!summary.contains(&esc(tr("report.blocks_exported"))));
        assert!(standard.contains(&esc(tr("report.blocks_exported"))));
    }

    #[test]
    fn a_full_folder_report_stops_showing_differences_at_its_byte_limit() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // 50 blocks in each of three files.
        let side =
            |s: &str| -> String { (0..50).map(|i| format!("same {i}\n{s} {i}\n")).collect() };
        for f in ["1.txt", "2.txt", "3.txt"] {
            std::fs::write(a.path().join(f), side("left")).unwrap();
            std::fs::write(b.path().join(f), side("right")).unwrap();
        }
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions::default();
        let items = scan(&roots, &opts);
        let refs: Vec<&DirItem> = items.iter().collect();
        let info = FolderReportInfo {
            detail: ReportDetail::Full,
            ..FolderReportInfo::default()
        };
        let with = |bytes| {
            let limits = Limits {
                bytes,
                ..ReportDetail::Full.limits()
            };
            folder_report_within(&refs, &roots, &roots, Some(&opts), "", &info, limits)
        };
        let whole = with(REPORT_BYTES);
        assert_eq!(whole.matches("class=\"hunk").count(), 150);
        assert!(whole.contains(&esc(tr("report.complete"))));

        // Reached part way through the first file: the rest of its blocks are left out, and the
        // other two files are not read at all.
        let bytes = whole.len() / 6;
        let html = with(bytes);
        let shown = html.matches("class=\"hunk").count();
        assert!(shown > 0 && shown < 50, "{shown}");
        assert!(html.len() < whole.len() / 2);
        assert!(!html.contains(&esc(tr("report.complete"))));
        let omitted = trf(
            "report.omit_bytes",
            &[("n", (50 - shown).to_string()), ("size", size_label(bytes))],
        );
        assert!(html.contains(&esc(&omitted)), "{html}");
        let left = format!(
            "<div class=\"v\">{}</div><div class=\"l\">{}</div>",
            50 - shown,
            esc(tr("report.blocks_left_out"))
        );
        assert!(html.contains(&left));
        let unread = trf("report.why_bytes", &[("size", size_label(bytes))]);
        assert_eq!(
            html.matches(&format!("<div class=\"why\">{}</div>", esc(&unread)))
                .count(),
            2
        );
        let listed = trf("report.omit_summary", &[("n", "2".into())]);
        assert!(html.contains(&esc(&listed)));
        // Whole lines and every row still, for what is shown.
        assert!(!html.contains(&esc(tr("report.shortened"))));
        assert_eq!(size_label(REPORT_BYTES), "1 GiB");
        assert_eq!(size_label(16 << 20), "16 MiB");
    }

    #[test]
    fn a_summary_text_report_lists_as_many_differences_as_a_standard_one_holds_rows() {
        let total = REPORT_ROWS + 5;
        let side = |s: &str| -> Vec<String> {
            (0..total)
                .flat_map(|i| [format!("same {i}"), format!("{s} {i}")])
                .collect()
        };
        let (a, b) = (side("left"), side("right"));
        let opts = DiffOptions::default();
        let r = crate::core::diff::compute(&[&a, &b], &opts);
        assert_eq!(hunks(&r).len(), total);
        let titles = ["a".to_string(), "b".to_string()];
        let html = text_report(
            &titles,
            &[&a, &b],
            &r,
            &opts,
            &[],
            "",
            ReportDetail::Summary,
        );
        assert_eq!(html.matches("<li id=\"d").count(), REPORT_ROWS);
        assert!(!html.contains(&format!("<li id=\"d{REPORT_ROWS}\">")));
        let listed = trf(
            "report.omit_detail_summary",
            &[("n", REPORT_ROWS.to_string())],
        );
        assert!(html.contains(&esc(&listed)));
        let limit = trf(
            "report.omit_limit",
            &[
                ("n", "5".into()),
                ("rows", fmt_size(Some(REPORT_ROWS as u64))),
            ],
        );
        assert!(html.contains(&esc(&limit)));
        let more = trf("report.more_diffs", &[("n", "5".into())]);
        assert!(html.contains(&esc(&more)));
    }

    #[test]
    fn text_reports_say_whether_they_are_complete() {
        let a: Vec<String> = (0..1000).map(|i| format!("{i}")).collect();
        let b: Vec<String> = (0..1000).map(|i| format!("{i}x")).collect();
        let r = crate::core::diff::compute(&[&a, &b], &DiffOptions::default());
        let titles = ["a".to_string(), "b".to_string()];
        let opts = DiffOptions::default();
        let html = |d| text_report(&titles, &[&a, &b], &r, &opts, &[], "", d);
        let complete = esc(tr("report.complete"));

        let standard = html(ReportDetail::Standard);
        assert!(standard.contains(&esc(&cut_text(1, ReportDetail::Standard.limits()))));
        assert!(!standard.contains(&complete));
        assert!(standard.contains("id=\"completeness\""));

        let full = html(ReportDetail::Full);
        assert!(full.contains(&complete));
        assert!(full.contains("<td class=\"n\">1000</td>"));

        let summary = html(ReportDetail::Summary);
        assert!(!summary.contains("<table class=\"d"));
        assert!(summary.contains("<ol class=\"sumlist\""));
        assert!(summary.contains("<li id=\"d0\">"));
        let n = hunks(&r).len().to_string();
        assert!(summary.contains(&esc(&trf("report.omit_detail_summary", &[("n", n)]))));

        let same = text_report(
            &titles,
            &[&a, &a],
            &crate::core::diff::compute(&[&a, &a], &opts),
            &opts,
            &[],
            "",
            ReportDetail::Summary,
        );
        assert!(same.contains(&complete), "nothing to leave out");
    }

    #[test]
    fn file_anchors_follow_the_path_not_the_order() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for f in ["one.txt", "two.txt"] {
            std::fs::write(a.path().join(f), "a\n").unwrap();
            std::fs::write(b.path().join(f), "b\n").unwrap();
        }
        let items = scan(
            &[a.path().to_path_buf(), b.path().to_path_buf()],
            &DiffOptions::default(),
        );
        let mut refs: Vec<&DirItem> = items.iter().filter(|it| !it.is_dir).collect();
        let first = file_anchors(&refs);
        refs.reverse();
        let mut second = file_anchors(&refs);
        second.reverse();
        assert_eq!(first, second);
        assert_ne!(first[0], first[1]);
        assert!(
            first[0].starts_with("f-") && first[0].len() == 14,
            "{}",
            first[0]
        );
        // The same path twice still gets two ids.
        let twice = file_anchors(&[refs[0], refs[0]]);
        assert_eq!(twice[1], format!("{}-2", twice[0]));
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
        let opts = DiffOptions::default();
        let info = FolderReportInfo::default();
        let html = report(&roots, Some(&opts), &info);
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
        assert!(
            html.contains("<a href=\"https://github.com/kuopinghsu/OxideMerge\">OxideMerge</a>")
        );
        // Every changed file is in the index, those without differences shown too, and
        // their cards say why.
        for name in ["changed.txt", "new.txt", "long.txt", "new.bin", "wide.txt"] {
            assert!(
                html.contains(&format!("<span>{name}</span>")),
                "{name} in the index"
            );
        }
        assert!(!html.contains("<span>same.txt</span>"));
        let summary = format!(
            "<span class=\"badge so\">{}</span>",
            esc(tr("report.summary_only"))
        );
        assert_eq!(html.matches(&summary).count(), 2, "long.txt and new.bin");
        assert!(html.contains(&format!(
            "<div class=\"why\">{}</div>",
            esc(tr("report.binary"))
        )));
        assert!(html.contains(&format!("{}</span>", esc(&ReadAs::Binary.label()))));
        assert!(html.contains(&format!("{}</span>", esc(&ReadAs::Text.label()))));
        assert!(
            html.contains(&esc(tr("report.shortened"))),
            "wide.txt is cut"
        );
        let card = |v: usize, key: &'static str| {
            format!(
                "<div class=\"v\">{v}</div><div class=\"l\">{}</div>",
                esc(tr(key))
            )
        };
        assert!(html.contains(&card(3, "report.files_different")), "{html}");
        assert!(html.contains(&card(2, "report.files_unique")));
        assert!(html.contains(&card(3, "report.blocks_exported")));
        assert!(!html.contains(&esc(tr("report.blocks_left_out"))));

        let plain = report(&roots, None, &info);
        assert!(!plain.contains("class=\"hunk\""));
        assert!(plain.contains("data-name=\"new.txt\""));
        assert!(plain.contains(&esc(tr("report.why_scope"))));
        assert!(!plain.contains("id=\"prev\""));

        // Files not compared yet are not counted as identical.
        let mut pending = scan(&roots, &opts);
        for it in &mut pending {
            it.status = CmpStatus::Pending;
        }
        let refs: Vec<&DirItem> = pending.iter().collect();
        let html = folder_report(&refs, &roots, &roots, None, "", &info);
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
        let items = scan(&roots, &opts);
        let refs: Vec<&DirItem> = items.iter().collect();
        let info = FolderReportInfo {
            method: Some(CompareMethod::FullContents),
            filter: "*.txt".into(),
            ..FolderReportInfo::default()
        };
        let html = folder_report(&refs, &roots, &roots, Some(&opts), "/x/ucie.oxrules", &info);
        assert!(html.contains("<b>ucie</b>"), "{html}");
        let one = trf("report.n_ignored", &[("n", "1".into())]);
        assert!(html.contains(&esc(&one)), "{html}");
        // The settings say how the files were compared.
        assert!(html.contains(&format!(
            "<summary>{}</summary>",
            esc(tr("report.settings"))
        )));
        assert!(html.contains("<code>*.txt</code>"));
        assert!(html.contains("<li><code>^// </code></li>"));
        assert!(html.contains(&esc(tr("method.full"))));
        assert!(html.contains(&esc(tr("report.scope_all"))));
    }

    #[test]
    fn table_rows_are_read_field_by_field() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let left = "id,name,score\n1,Alice,90\n2,Bob,85\n3,Carol,77\n";
        let right = "id,name,score\n1,Alice,95\n2,Bob,85\n4,Dave,60\n";
        std::fs::write(a.path().join("data.csv"), left).unwrap();
        std::fs::write(b.path().join("data.csv"), right).unwrap();
        // Not a table: the same lines say nothing about fields.
        std::fs::write(a.path().join("data.txt"), left).unwrap();
        std::fs::write(b.path().join("data.txt"), right).unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let html = report(
            &roots,
            Some(&DiffOptions::default()),
            &FolderReportInfo::default(),
        );
        assert_eq!(
            html.matches("<li>Alice · score · 90 → 95</li>").count(),
            1,
            "{html}"
        );
        // Carol's row became Dave's: another row, not a change of its fields.
        assert!(!html.contains("Carol ·"), "{html}");
    }

    #[test]
    fn spreadsheet_fields_show_their_formatted_values_with_the_stored_ones() {
        let doc = |lines: &[&str]| {
            super::super::text::doc_from_bytes(None, lines.join("\n").as_bytes(), None)
        };
        let docs = [
            doc(&["item,due,share", "rent,45292,0.25"]),
            doc(&["item,due,share", "rent,45293,0.25"]),
        ];
        let shown = |lines: &[&str]| {
            ReadAs::Spreadsheet(known(OtherCells {
                lines: v(lines),
                shown: true,
            }))
        };
        let reads = [
            shown(&["item,due,share", "rent,2024-01-01,25%"]),
            shown(&["item,due,share", "rent,2024-01-02,25%"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · due · 2024-01-01 (45292) → 2024-01-02 (45293)"]
        );
        // An .ods sheet compares what it shows and stores the values apart.
        let docs = [
            doc(&["item,share", "rent,25%"]),
            doc(&["item,share", "rent,26%"]),
        ];
        let stored = |lines: &[&str]| {
            ReadAs::Spreadsheet(known(OtherCells {
                lines: v(lines),
                shown: false,
            }))
        };
        let reads = [
            stored(&["item,share", "rent,0.25"]),
            stored(&["item,share", "rent,26%"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · share · 25% (0.25) → 26%"]
        );
        // A reading of another length is not of the same lines, so it is left out.
        let reads = [shown(&["item,share"]), shown(&["item,share"])];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · share · 25% → 26%"]
        );
        // A sheet's rows leave out the empty cells they end with, and a column without a name
        // goes by its letter.
        let docs = [
            doc(&["item,,total,notes", "rent,1,2"]),
            doc(&["item,,total,notes", "rent,1,3,new"]),
        ];
        let reads = [
            shown(&["item,,total,notes", "rent,1,2"]),
            shown(&["item,,total,notes", "rent,1,3,new"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · total · 2 → 3", "rent · notes · — → new"]
        );
        let docs = [
            doc(&["item,,total", "rent,1,2"]),
            doc(&["item,,total", "rent,4,2"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · B · 1 → 4"]
        );
        // A title above a sheet's header is not its header; rows of names and numbers are not
        // a table.
        let docs = [
            doc(&["Expenses", "", "item,q1,q2", "rent,1,2"]),
            doc(&["Expenses", "", "item,q1,q2", "rent,1,3"]),
        ];
        let reads = [shown(&[]), shown(&[])];
        assert_eq!(
            field_changes(&reads, &docs, &[3..4, 3..4]),
            ["rent · q2 · 2 → 3"]
        );
        let docs = [
            doc(&["Summary", "Report date,46295", "Headcount,5"]),
            doc(&["Summary", "Report date,46295", "Headcount,6"]),
        ];
        assert!(field_changes(&reads, &docs, &[2..3, 2..3]).is_empty());
        // A header over two rows names a column by both.
        let docs = [
            doc(&["group,quarter,,,total", ",q1,q2,q3", "rent,1,2,3,6", "x"]),
            doc(&["group,quarter,,,total", ",q1,q2,q3", "rent,1,2,4,7", "x"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[2..3, 2..3]),
            ["rent · quarter q3 · 3 → 4", "rent · total · 6 → 7"]
        );
        // A row of words under a header that names every column is a row of values.
        let docs = [
            doc(&["name,team", "Amy,Sales", "Ben,Ops"]),
            doc(&["name,team", "Amy,Sales", "Ben,Data"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[2..3, 2..3]),
            ["Ben · team · Ops → Data"]
        );
        // A header in the sheet's first row may name its columns by year.
        let docs = [
            doc(&["item,2025,2026", "rent,1,2"]),
            doc(&["item,2025,2026", "rent,1,3"]),
        ];
        assert_eq!(
            field_changes(&reads, &docs, &[1..2, 1..2]),
            ["rent · 2026 · 2 → 3"]
        );
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(column_letters(702), "AAA");
    }

    #[test]
    fn binary_files_with_a_reader_show_what_it_reads() {
        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let left = std::fs::read(samples.join("left/prog/prog.elf")).unwrap();
        std::fs::copy(
            samples.join("left/prog/prog.elf"),
            a.path().join("prog.elf"),
        )
        .unwrap();
        std::fs::copy(
            samples.join("right/prog/prog.elf"),
            b.path().join("prog.elf"),
        )
        .unwrap();
        // The same program with bytes after everything its text reads.
        std::fs::write(a.path().join("padded.elf"), &left).unwrap();
        let mut padded = left.clone();
        padded.extend_from_slice(b"\0\0padding");
        std::fs::write(b.path().join("padded.elf"), padded).unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let opts = DiffOptions::default();
        let items = scan(&roots, &opts);
        let elf = |name: &str| items.iter().find(|i| i.name == name).unwrap();
        assert!(elf("prog.elf").binary && elf("padded.elf").binary);
        assert!(matches!(
            file_diffs(elf("prog.elf"), &roots, &opts),
            FileDiffs::Diffs(..)
        ));
        let note = trf(
            "report.same_text_bytes_differ",
            &[("read", ReadAs::Elf.label())],
        );
        assert!(matches!(
            file_diffs(elf("padded.elf"), &roots, &opts),
            FileDiffs::Note(n) if n == note
        ));
        let html = report(&roots, Some(&opts), &FolderReportInfo::default());
        let card = |name: &str| {
            let at = html.find(&format!("data-name=\"{name}\"")).unwrap();
            html[at..at + html[at..].find("<span class=\"badge\">").unwrap()].to_string()
        };
        assert!(
            card("prog.elf").contains(&esc(&ReadAs::Elf.label())),
            "{html}"
        );
        assert!(!card("prog.elf").contains(&esc(tr("report.summary_only"))));
        assert!(html.contains(&esc(&note)));
        // Without a reader, a binary file is still listed without its differences.
        let raw = DiffOptions {
            extract_documents: false,
            ..DiffOptions::default()
        };
        assert!(matches!(
            file_diffs(elf("prog.elf"), &roots, &raw),
            FileDiffs::None
        ));
    }

    #[test]
    fn spreadsheet_and_notebook_differences_say_where_they_are() {
        let sheet = |rows: &[&str]| -> Vec<String> {
            let mut lines = vec![format!(
                "# {}",
                trf("label.sheet", &[("name", "Plan".into())])
            )];
            lines.extend(rows.iter().map(|r| r.to_string()));
            lines
        };
        let lines = sheet(&["a,b", "", "x,y"]);
        let at = location(&ReadAs::Spreadsheet(Later::new(None)), &lines, &(3..4)).unwrap();
        assert!(
            at.starts_with(&trf("label.sheet", &[("name", "Plan".into())])),
            "{at}"
        );
        assert!(
            at.ends_with(&trf("report.loc_row", &[("n", "3".into())])),
            "{at}"
        );
        // One sheet has no heading: lines are its rows from the first.
        let one = v(&["a,b", "c,d"]);
        assert_eq!(
            location(&ReadAs::Spreadsheet(Later::new(None)), &one, &(0..2)).unwrap(),
            trf("report.loc_rows", &[("a", "1".into()), ("b", "2".into())])
        );
        let cells = v(&["── Code cell ──", "x = 1", "── Markdown cell ──", "# Title"]);
        assert_eq!(
            location(&ReadAs::Notebook, &cells, &(3..4)).as_deref(),
            Some("Markdown cell")
        );
        assert_eq!(
            location(&ReadAs::Document(Later::new(None)), &cells, &(3..4)),
            None
        );
        let pages = ReadAs::Document(known((vec![0, 2, 3], 4)));
        assert_eq!(
            location(&pages, &cells, &(1..2)).unwrap(),
            trf("report.loc_page", &[("n", "1".into())])
        );
        assert_eq!(
            location(&pages, &cells, &(1..4)).unwrap(),
            trf("report.loc_pages", &[("a", "1".into()), ("b", "3".into())])
        );
        assert_eq!(
            location(&pages, &cells, &(2..2)).unwrap(),
            trf("report.loc_page", &[("n", "2".into())])
        );
        // Pages read from text other than the one compared say nothing.
        let stale = ReadAs::Document(known((vec![0, 2], 9)));
        assert_eq!(location(&stale, &cells, &(1..2)), None);
        assert_eq!(
            program("\"C:\\Tools\\pandoc.exe\" -t plain ${file}"),
            "pandoc.exe"
        );
        assert_eq!(program("/usr/bin/pdftotext ${file} -"), "pdftotext");
        assert_eq!(program("C:\\Tools\\ ${file}"), "Tools");
    }

    #[test]
    fn hidden_paths_leave_no_trace_of_the_folders() {
        let base = tempfile::tempdir().unwrap();
        let (a, b) = (
            base.path().join("secret-left-7f3a"),
            base.path().join("secret-right-9c2e"),
        );
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("f.txt"), "one\n").unwrap();
        std::fs::write(b.join("f.txt"), "two\n").unwrap();
        std::fs::write(b.join("only.txt"), "x\n").unwrap();
        let roots = vec![a.clone(), b.clone()];
        let info = FolderReportInfo {
            filter: format!("{}/*.txt", a.display()),
            hide_paths: true,
            ..FolderReportInfo::default()
        };
        let html = report(&roots, Some(&DiffOptions::default()), &info);
        for secret in ["secret-left-7f3a", "secret-right-9c2e"] {
            assert!(!html.contains(secret), "{secret} is in the report");
        }
        assert!(html.contains(&esc(tr("report.paths_hidden"))));
        let anchor = file_anchors(&[&scan(&roots, &DiffOptions::default())
            .into_iter()
            .find(|it| it.name == "f.txt")
            .unwrap()])
        .remove(0);
        assert!(
            html.contains(&format!("id=\"{anchor}-d0\"")),
            "differences are still linked"
        );
        let shown = report(
            &roots,
            Some(&DiffOptions::default()),
            &FolderReportInfo::default(),
        );
        assert!(shown.contains("secret-left-7f3a"));
    }

    #[test]
    fn report_pages_carry_their_language_and_controls() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Names that HTML would read as markup, as far as every file system allows them.
        std::fs::write(a.path().join("a&b'.txt"), "\"quoted\" & <i>x</i>\n").unwrap();
        std::fs::write(b.path().join("a&b'.txt"), "\"quoted\" & <i>y</i>\n").unwrap();
        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let html = report(
            &roots,
            Some(&DiffOptions::default()),
            &FolderReportInfo::default(),
        );
        let lang = format!(
            "<html lang=\"{}\" class=\"nojs\">",
            crate::i18n::current_language()
        );
        assert!(html.contains(&lang));
        assert!(html.contains("<h1>"));
        assert!(html.contains("<nav class=\"toc\" aria-label="));
        assert!(html.contains("role=\"status\" aria-live=\"polite\""));
        assert!(html.contains("aria-pressed=\"true\" data-status=\"all\""));
        assert!(html.contains(&format!(">{}</option>", esc(tr("report.search_text")))));
        assert!(html.contains("<script type=\"application/json\" id=\"oxm-t\">"));
        // What the files hold stays text, wherever it is shown.
        assert!(!html.contains("<i>x</i>") && !html.contains("a&b'.txt"));
        assert!(html.contains("data-name=\"a&amp;b'.txt\""));
        assert!(html.contains("&quot;quoted&quot; &amp; &lt;i&gt;"));
        let json = templates();
        assert!(!json.contains('<') && json.starts_with('{'));
    }
}
