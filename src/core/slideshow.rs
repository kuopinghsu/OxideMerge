//! HTML slideshow reports, as Araxis Merge makes them: a single self-contained page that shows
//! one difference at a time, for walking an audience through a change. A title slide with the
//! compared sides and counts comes first and a closing slide last; the arrow keys, Space,
//! Page Up/Down, Home and End move between slides, F goes full screen, and each slide has its
//! own address (`#3`) to link to.

use super::diff::{DiffOptions, DiffResult};
use super::folder::{CmpStatus, DirItem};
use super::html_report::{
    file_diffs, has_text_diffs, hunks, kind, line_range, row_cells, status_class, too_big,
    FileDiffs, DARK, HUNK_MAX_ROWS, LIGHT,
};
use super::report::{esc, esc_marked, side_name, status_text};
use crate::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;
use std::path::PathBuf;

/// Most difference slides in one report; the rest are counted on the closing slide.
const MAX_SLIDES: usize = 2_000;
/// Most files the closing slide of a folder slideshow lists.
const MAX_LISTED: usize = 500;

const CSS: &str = r#"
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans","PingFang TC","Microsoft JhengHei",sans-serif;display:flex;flex-direction:column;overflow:hidden}
:root{--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}
header{display:flex;align-items:center;gap:14px;padding:10px 22px;background:var(--surface);border-bottom:1px solid var(--border)}
.brand{display:flex;align-items:center;gap:10px;font-weight:650;margin-right:auto;min-width:0}
.brand span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.logo{width:26px;height:26px;border-radius:7px;background:linear-gradient(135deg,#ff8a3d,#5b6cff);display:grid;place-items:center;color:#fff;font-size:13px;font-weight:800;flex:none}
button{font:inherit;font-size:14px;padding:5px 12px;border-radius:8px;border:1px solid var(--border);background:var(--surface);color:var(--text);cursor:pointer}
button:hover{border-color:var(--accent)}button:disabled{opacity:.4;cursor:default}
.count{font-variant-numeric:tabular-nums;color:var(--dim);min-width:5.5em;text-align:center}
.bar{height:3px;background:var(--surface2)}.bar div{height:100%;background:var(--accent);transition:width .2s}
main{flex:1;position:relative;overflow:hidden}
section{position:absolute;inset:0;padding:28px 40px 24px;overflow:auto;display:none}
section.on{display:block}
section.title,section.end{display:none;place-content:center;text-align:center}
section.title.on,section.end.on{display:grid}
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
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:8px;background:var(--dim)}
.st-different .dot{background:var(--warn)}.st-unique .dot{background:var(--uniq)}.st-error .dot{background:var(--bad)}
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
if(k==='ArrowRight'||k==='PageDown'||k===' '||k==='j'||k==='n'){e.preventDefault();go(i+1)}
else if(k==='ArrowLeft'||k==='PageUp'||k==='k'||k==='p'){e.preventDefault();go(i-1)}
else if(k==='Home'){e.preventDefault();go(0)}else if(k==='End'){e.preventDefault();go(n-1)}
else if(k==='f'||k==='F'){document.getElementById('full').click()}});
window.addEventListener('hashchange',()=>go(parseInt(location.hash.slice(1),10)-1||0,false));
go(parseInt(location.hash.slice(1),10)-1||0,false)})();
"#;

fn page_start(out: &mut String, title: &str) {
    let _ = write!(
        out,
        "<!DOCTYPE html>\n<html lang=\"{}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"generator\" content=\"OxideMerge {}\"><title>{}</title><style>:root{{{LIGHT}}}@media (prefers-color-scheme:dark){{:root{{{DARK}}}}}{CSS}</style></head><body>\
<header><div class=\"brand\"><div class=\"logo\">⇄</div><span>{}</span></div>\
<button id=\"prev\" title=\"←\">‹</button><span class=\"count\" id=\"count\"></span><button id=\"next\" title=\"→\">›</button>\
<button id=\"full\" title=\"F\">⛶</button></header><div class=\"bar\"><div></div></div><main>\n",
        crate::i18n::current_language(),
        env!("CARGO_PKG_VERSION"),
        esc(title),
        esc(title)
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

fn title_slide(
    out: &mut String,
    title: &str,
    names: &[String],
    rules: &str,
    cards: &[(&str, String, &str)],
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

/// The slide of difference `b`, showing `rows` (it and the lines around it). `file` names the
/// file in a folder compare.
fn diff_slide(
    out: &mut String,
    doc: &Doc,
    file: Option<&str>,
    (index, total): (usize, usize),
    (b, rows): (usize, Range<usize>),
) {
    let n = doc.panes.len();
    let blk = &doc.result.blocks[b];
    let (kclass, klabel) = kind(doc.result, b);
    out.push_str("<section><div class=\"sh\">");
    if let Some(f) = file {
        let _ = write!(out, "<div class=\"file\">{}</div>", esc(f));
    }
    let _ = write!(
        out,
        "<h2>{}</h2><span class=\"kind k-{kclass}\">{}</span><span class=\"pos\">",
        esc(&trf(
            "slides.difference",
            &[("n", (index + 1).to_string()), ("total", total.to_string())]
        )),
        esc(klabel)
    );
    for p in 0..n {
        let _ = write!(
            out,
            "<b>{}</b>{}",
            esc(side_name(p, n)),
            esc(&line_range(&blk.ranges[p]))
        );
    }
    out.push_str("</span></div><div class=\"d\"><table><colgroup>");
    for _ in 0..n {
        out.push_str("<col style=\"width:4.2em\"><col>");
    }
    out.push_str("</colgroup><tr>");
    for p in 0..n {
        let _ = write!(out, "<th></th><th>{}</th>", esc(&doc.names[p]));
    }
    out.push_str("</tr>");
    let shown = rows.start..rows.end.min(rows.start + HUNK_MAX_ROWS);
    let mut notes: Vec<(usize, usize, &str)> = Vec::new();
    for r in shown.clone() {
        let cells = row_cells(doc.result, doc.panes, doc.opts, r);
        let row = &doc.result.rows[r];
        out.push_str("<tr>");
        for (p, (class, marks)) in cells.iter().enumerate() {
            match row.line[p] {
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
}

fn end_slide(out: &mut String, text: &str, extra: &str) {
    let _ = writeln!(
        out,
        "<section class=\"end\"><div><h1>{}</h1>{extra}</div></section>",
        esc(text)
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
    title_slide(&mut out, title, titles, rules, &cards);
    let doc = Doc {
        panes,
        result,
        opts,
        notes,
        names: titles,
    };
    for (i, h) in list.into_iter().take(MAX_SLIDES).enumerate() {
        diff_slide(&mut out, &doc, None, (i, total), h);
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

/// Slideshow of a folder compare: a slide for each difference of every changed text file
/// (reading the files from `roots`), then a closing slide listing the files that differ in
/// other ways, such as binary files or files on one side only. `display` names the folders.
pub fn folder_slideshow(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: &DiffOptions,
    rules: &str,
) -> String {
    let n = display.len();
    let names: Vec<String> = display.iter().map(|d| d.display().to_string()).collect();
    let files: Vec<&DirItem> = items
        .iter()
        .copied()
        .filter(|it| !it.is_dir || it.status == CmpStatus::Unique)
        .collect();
    let mut slides = String::new();
    let mut others: Vec<(&DirItem, String)> = Vec::new();
    let mut total = 0;
    let mut skipped = 0;
    // Changed text files not read at all, once there are slides enough; ones known to be too
    // large are still listed, with why.
    let mut skipped_files = 0;
    for it in &files {
        if total >= MAX_SLIDES && has_text_diffs(it) {
            match too_big(it, n) {
                Some(note) => others.push((it, note)),
                None => skipped_files += 1,
            }
            continue;
        }
        match file_diffs(it, roots, opts) {
            FileDiffs::Diffs(docs, result) => {
                let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
                let file_names: Vec<String> = (0..n).map(|s| side_name(s, n).to_string()).collect();
                let doc = Doc {
                    panes: &refs,
                    result: &result,
                    opts,
                    notes: &[],
                    names: &file_names,
                };
                let list = hunks(&result);
                let count = list.len();
                for (j, h) in list.into_iter().enumerate() {
                    if total >= MAX_SLIDES {
                        skipped += 1;
                        continue;
                    }
                    total += 1;
                    diff_slide(&mut slides, &doc, Some(&it.rel_path()), (j, count), h);
                }
            }
            FileDiffs::Note(note) => others.push((it, note)),
            FileDiffs::None if it.status != CmpStatus::Identical => {
                others.push((it, status_text(it, n)))
            }
            FileDiffs::None => {}
        }
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
    let cards = vec![
        (
            "acc",
            (total + skipped).to_string(),
            tr("report.differences"),
        ),
        (
            "warn",
            count("different").to_string(),
            tr("report.different"),
        ),
        ("uniq", count("unique").to_string(), tr("report.unique")),
        ("ok", count("identical").to_string(), tr("report.identical")),
    ];
    title_slide(&mut out, title, &names, rules, &cards);
    out.push_str(&slides);
    let mut extra = String::new();
    if skipped > 0 {
        let _ = write!(
            extra,
            "<div class=\"sub\">{}</div>",
            esc(&trf("report.more_diffs", &[("n", skipped.to_string())]))
        );
    }
    if skipped_files > 0 {
        let _ = write!(
            extra,
            "<div class=\"sub\">{}</div>",
            esc(&trf(
                "slides.more_files",
                &[("n", skipped_files.to_string())]
            ))
        );
    }
    if !others.is_empty() {
        let _ = write!(
            extra,
            "<div class=\"sub\">{}</div><div class=\"list\">",
            esc(tr("slides.other_files"))
        );
        for (it, why) in others.iter().take(MAX_LISTED) {
            let _ = write!(
                extra,
                "<div class=\"st-{}\" title=\"{}\"><span class=\"dot\"></span>{} — {}</div>",
                status_class(it.status),
                esc(&it.rel_path()),
                esc(&it.rel_path()),
                esc(why)
            );
        }
        extra.push_str("</div>");
        if others.len() > MAX_LISTED {
            let _ = write!(
                extra,
                "<div class=\"sub\">{}</div>",
                esc(&trf(
                    "slides.more_files",
                    &[("n", (others.len() - MAX_LISTED).to_string())]
                ))
            );
        }
    }
    let end = if total + skipped + skipped_files == 0 {
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
        let html = folder_slideshow(&[&a, &b], &roots, &roots, &DiffOptions::default(), "");
        // Title, the slides it holds, end; b.txt is not read and is counted instead.
        assert_eq!(html.matches("<section").count(), MAX_SLIDES + 2);
        assert!(!html.contains(">B<"), "b.txt was not shown");
        assert!(html.contains(&esc(&trf("report.more_diffs", &[("n", "1".into())]))));
        assert!(html.contains(&esc(&trf("slides.more_files", &[("n", "1".into())]))));
    }
}
