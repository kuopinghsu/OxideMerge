//! Folder compare reports (HTML, CSV, plain text), text compare HTML reports and status
//! descriptions.

use super::diff::{inline_diff, BlockKind, DiffOptions, DiffResult};
use super::folder::{CmpStatus, DirItem};
use crate::i18n::{tr, trf};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportFormat {
    Html,
    Csv,
    Text,
}

impl ReportFormat {
    pub fn from_path(p: &Path) -> ReportFormat {
        match super::text::extension(p).as_str() {
            "htm" | "html" => ReportFormat::Html,
            "csv" => ReportFormat::Csv,
            _ => ReportFormat::Text,
        }
    }
}

pub fn side_name(side: usize, panes: usize) -> &'static str {
    match (side, panes) {
        (0, _) => tr("side.left"),
        (1, 2) => tr("side.right"),
        (1, _) => tr("side.middle"),
        _ => tr("side.right"),
    }
}

pub fn status_text(it: &DirItem, panes: usize) -> String {
    match it.status {
        CmpStatus::Identical => {
            if it.is_dir {
                tr("status.folders_identical").into()
            } else if it.binary {
                tr("status.identical_binary").into()
            } else {
                tr("status.identical").into()
            }
        }
        CmpStatus::Different => {
            if panes == 3 && !it.is_dir {
                match it.diff_side {
                    Some(0) => return tr("status.left_diff").into(),
                    Some(1) => return tr("status.middle_diff").into(),
                    Some(2) => return tr("status.right_diff").into(),
                    _ => {}
                }
            }
            if it.is_dir {
                tr("status.folders_different").into()
            } else if it.binary {
                tr("status.different_binary").into()
            } else {
                tr("status.different").into()
            }
        }
        CmpStatus::Unique => {
            let present: Vec<usize> = (0..panes).filter(|&s| it.exists[s]).collect();
            if present.len() == 1 {
                trf(
                    "status.only",
                    &[("side", side_name(present[0], panes).to_string())],
                )
            } else {
                let missing: Vec<&str> = (0..panes)
                    .filter(|&s| !it.exists[s])
                    .map(|s| side_name(s, panes))
                    .collect();
                trf("status.missing", &[("side", missing.join(", "))])
            }
        }
        CmpStatus::Error => format!(
            "{}: {}",
            tr("status.error"),
            it.error.clone().unwrap_or_default()
        ),
        CmpStatus::NotCompared if it.is_dir => tr("status.not_compared").into(),
        CmpStatus::NotCompared => tr("status.file_not_compared").into(),
        CmpStatus::Pending => tr("status.pending").into(),
    }
}

pub fn fmt_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        None => String::new(),
    }
}

pub fn fmt_size(s: Option<u64>) -> String {
    match s {
        Some(s) => {
            let mut out = String::new();
            let digits = s.to_string();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(c);
            }
            out
        }
        None => String::new(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Escapes `line`, marking the byte ranges in `marks` (which must be sorted and on char boundaries).
fn esc_marked(line: &str, marks: &[std::ops::Range<usize>]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for r in marks {
        let (start, end) = (r.start.max(pos).min(line.len()), r.end.min(line.len()));
        if start >= end {
            continue;
        }
        out.push_str(&esc(&line[pos..start]));
        let _ = write!(out, "<span class=\"w\">{}</span>", esc(&line[start..end]));
        pos = end;
    }
    out.push_str(&esc(&line[pos..]));
    out
}

/// Side-by-side HTML report of a 2-way or 3-way text compare.
pub fn text_html(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
) -> String {
    let n = panes.len();
    let mut out = String::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n");
    let _ = writeln!(out, "<title>{}</title>", esc(tr("report.text_title")));
    out.push_str(
        "<style>body{font-family:Segoe UI,Arial,sans-serif;font-size:13px}\
         table{border-collapse:collapse;width:100%;table-layout:fixed}\
         td,th{padding:0 4px;vertical-align:top}th{background:#ddd;text-align:left;border:1px solid #bbb}\
         td.n{width:4em;color:#888;text-align:right;user-select:none;background:#f4f4f4}\
         td.t{font-family:Consolas,Menlo,monospace;white-space:pre-wrap;word-break:break-all}\
         .chg{background:#efcbd0}.conf{background:#ffb3b3}.same{background:#fff3b0}\
         .gap{background:#e4e4e4}.ign{background:#ececf8}.w{background:#e48a96}</style></head><body>\n",
    );
    let _ = writeln!(
        out,
        "<h2>{}</h2><p>{}<br>{}</p>",
        esc(tr("report.text_title")),
        now,
        esc(&trf(
            "report.text_summary",
            &[("n", result.significant_blocks().to_string())]
        ))
    );
    out.push_str("<table><tr>");
    for (p, t) in titles.iter().enumerate().take(n) {
        let _ = write!(
            out,
            "<th class=\"n\"></th><th>{}: {}</th>",
            esc(side_name(p, n)),
            esc(t)
        );
    }
    out.push_str("</tr>\n");
    for row in &result.rows {
        let block = row.block.map(|b| &result.blocks[b]);
        let class = match block {
            None => "",
            Some(b) if b.ignored => "ign",
            Some(b) => match b.kind {
                BlockKind::Conflict => "conf",
                BlockKind::BothSame => "same",
                _ => "chg",
            },
        };
        let inline = |p: usize| -> Vec<std::ops::Range<usize>> {
            let other = if n == 3 { 1 } else { 1 - p };
            if class.is_empty() || class == "ign" || (n == 3 && p == 1) {
                return Vec::new();
            }
            match (row.line[p], row.line[other]) {
                (Some(a), Some(b)) => inline_diff(&panes[p][a], &panes[other][b], opts).0,
                _ => Vec::new(),
            }
        };
        out.push_str("<tr>");
        for (p, lines) in panes.iter().enumerate() {
            match row.line[p] {
                Some(l) => {
                    let _ = write!(
                        out,
                        "<td class=\"n\">{}</td><td class=\"t {class}\">{}</td>",
                        l + 1,
                        esc_marked(&lines[l], &inline(p))
                    );
                }
                None => {
                    let gap = if class.is_empty() { "" } else { "gap" };
                    let _ = write!(out, "<td class=\"n\"></td><td class=\"t {gap}\"></td>");
                }
            }
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table></body></html>\n");
    out
}

pub fn generate(items: &[&DirItem], roots: &[PathBuf], format: ReportFormat) -> String {
    let panes = roots.len();
    let mut header: Vec<String> = vec![
        tr("col.name").into(),
        tr("col.folder").into(),
        tr("col.result").into(),
    ];
    for s in 0..panes {
        header.push(format!("{} {}", side_name(s, panes), tr("col.date")));
    }
    for s in 0..panes {
        header.push(format!("{} {}", side_name(s, panes), tr("col.size")));
    }
    header.push(tr("col.ext").into());
    let rows: Vec<(CmpStatus, Vec<String>)> = items
        .iter()
        .map(|it| {
            let mut r = vec![it.name.clone(), it.rel_dir.clone(), status_text(it, panes)];
            for s in 0..panes {
                r.push(fmt_time(it.mtime[s]));
            }
            for s in 0..panes {
                r.push(fmt_size(it.size[s]));
            }
            r.push(it.ext());
            (it.status, r)
        })
        .collect();
    let mut out = String::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    match format {
        ReportFormat::Csv => {
            for (s, r) in roots.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{},{}",
                    csv_field(side_name(s, panes)),
                    csv_field(&r.display().to_string())
                );
            }
            out.push('\n');
            let _ = writeln!(
                out,
                "{}",
                header
                    .iter()
                    .map(|h| csv_field(h))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            for (_, r) in &rows {
                let _ = writeln!(
                    out,
                    "{}",
                    r.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(",")
                );
            }
        }
        ReportFormat::Text => {
            let _ = writeln!(out, "{} - {}", tr("report.title"), now);
            for (s, r) in roots.iter().enumerate() {
                let _ = writeln!(out, "{}: {}", side_name(s, panes), r.display());
            }
            out.push('\n');
            let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
            for (_, r) in &rows {
                for (i, c) in r.iter().enumerate() {
                    widths[i] = widths[i].max(c.chars().count());
                }
            }
            let line = |cells: &[String]| -> String {
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{:w$}", c, w = widths[i]))
                    .collect::<Vec<_>>()
                    .join("  ")
                    .trim_end()
                    .to_string()
            };
            let _ = writeln!(out, "{}", line(&header));
            let _ = writeln!(
                out,
                "{}",
                "-".repeat(widths.iter().sum::<usize>() + 2 * widths.len())
            );
            for (_, r) in &rows {
                let _ = writeln!(out, "{}", line(r));
            }
        }
        ReportFormat::Html => {
            out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n");
            let _ = writeln!(out, "<title>{}</title>", esc(tr("report.title")));
            out.push_str(
                "<style>body{font-family:Segoe UI,Arial,sans-serif;font-size:13px}table{border-collapse:collapse}\
                 th,td{border:1px solid #bbb;padding:2px 6px;text-align:left}th{background:#ddd}\
                 .identical{background:#fff}.different{background:#efcbd0}.unique{background:#c0c0c0}\
                 .error{background:#ffa0a0}</style></head><body>\n",
            );
            let _ = writeln!(
                out,
                "<h2>{}</h2><p>{}</p><ul>",
                esc(tr("report.title")),
                now
            );
            for (s, r) in roots.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "<li><b>{}</b>: {}</li>",
                    esc(side_name(s, panes)),
                    esc(&r.display().to_string())
                );
            }
            out.push_str("</ul><table><tr>");
            for h in &header {
                let _ = write!(out, "<th>{}</th>", esc(h));
            }
            out.push_str("</tr>\n");
            for (st, r) in &rows {
                let class = match st {
                    CmpStatus::Identical | CmpStatus::NotCompared | CmpStatus::Pending => {
                        "identical"
                    }
                    CmpStatus::Different => "different",
                    CmpStatus::Unique => "unique",
                    CmpStatus::Error => "error",
                };
                let _ = write!(out, "<tr class=\"{class}\">");
                for c in r {
                    let _ = write!(out, "<td>{}</td>", esc(c));
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</table></body></html>\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::compute;

    #[test]
    fn text_report_marks_changed_rows_and_words() {
        let a: Vec<String> = ["same", "old <word>", "gone"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: Vec<String> = ["same", "new <word>"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = DiffOptions::default();
        let r = compute(&[&a, &b], &opts);
        let html = text_html(&["a.txt".into(), "b.txt".into()], &[&a, &b], &r, &opts);
        assert!(html.contains("<td class=\"t \">same</td>"));
        assert!(html.contains("<span class=\"w\">old</span> &lt;word&gt;"));
        assert!(html.contains(
            "<td class=\"t chg\">gone</td><td class=\"n\"></td><td class=\"t gap\"></td>"
        ));
    }
}
