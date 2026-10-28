//! Folder compare reports (HTML, CSV, plain text, XML), text compare reports (HTML, XML), HTML
//! slideshows of either, and status descriptions.

use super::diff::{DiffOptions, DiffResult};
use super::folder::{CmpStatus, DirItem};
use crate::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportFormat {
    Html,
    Csv,
    Text,
    Xml,
    /// An HTML page showing one difference at a time (see [`super::slideshow`]).
    Slideshow,
}

impl ReportFormat {
    /// The format a report file's extension asks for; a slideshow is asked for explicitly.
    pub fn from_path(p: &Path) -> ReportFormat {
        match super::text::extension(p).as_str() {
            "htm" | "html" => ReportFormat::Html,
            "csv" => ReportFormat::Csv,
            "xml" => ReportFormat::Xml,
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

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn csv_field(s: &str) -> String {
    // Spreadsheets run a cell starting like a formula; a leading ' keeps it text.
    let formula = s.starts_with(['=', '+', '@', '\t', '\r'])
        || (s.starts_with('-') && s.len() > 1 && s.parse::<f64>().is_err());
    let s = if formula {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// Escapes `line`, marking the byte ranges in `marks` (which must be sorted and on char boundaries).
pub(crate) fn esc_marked(line: &str, marks: &[std::ops::Range<usize>]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for r in marks {
        let (start, end) = (r.start.max(pos).min(line.len()), r.end.min(line.len()));
        if start >= end {
            continue;
        }
        out.push_str(&esc(&line[pos..start]));
        let _ = write!(out, "<span class=\"m\">{}</span>", esc(&line[start..end]));
        pos = end;
    }
    out.push_str(&esc(&line[pos..]));
    out
}

/// Report of a 2-way or 3-way text compare, listing every difference, with bookmark comments
/// and the compare rules file `rules` ("" = none) used.
pub fn text_html(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
) -> String {
    super::html_report::text_report(titles, panes, result, opts, notes, rules)
}

/// Report of a 2-way or 3-way text compare as `format`: XML, an HTML slideshow, or else the
/// HTML report (see [`text_html`]).
pub fn text_report(
    format: ReportFormat,
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    opts: &DiffOptions,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
) -> String {
    match format {
        ReportFormat::Xml => super::xml_report::text_xml(titles, panes, result, notes, rules),
        ReportFormat::Slideshow => {
            super::slideshow::text_slideshow(titles, panes, result, opts, notes, rules)
        }
        _ => text_html(titles, panes, result, opts, notes, rules),
    }
}

/// HTML slideshow of a folder compare: a slide for each difference of every changed text file.
/// `roots` are the compared folders and `display` the names to show for them.
pub fn folder_slideshow(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: &DiffOptions,
    rules: &str,
) -> String {
    super::slideshow::folder_slideshow(items, roots, display, opts, rules)
}

/// HTML folder report followed by the differences of every changed text file, including files
/// missing on some side. `roots` are the compared folders and `display` the names to show for
/// them, and `rules` the compare rules file used ("" = none).
pub fn folder_html_with_diffs(
    items: &[&DirItem],
    roots: &[PathBuf],
    display: &[PathBuf],
    opts: &DiffOptions,
    rules: &str,
) -> String {
    super::html_report::folder_report(items, roots, display, Some(opts), rules)
}

/// Makes a report; it may run on another thread.
pub type ReportJob = Box<dyn FnOnce() -> String + Send>;

/// A file name for the HTML report of a compare of `names` (paths or titles of the sides).
pub fn html_file_name(names: &[String]) -> String {
    let mut parts: Vec<String> = names
        .iter()
        .map(|n| {
            let t = n.trim().trim_end_matches(['/', '\\']);
            std::path::Path::new(t)
                .file_name()
                .map_or_else(|| t.to_string(), |f| f.to_string_lossy().into_owned())
        })
        .filter(|n| !n.is_empty())
        .collect();
    parts.dedup();
    let stem = match parts.as_slice() {
        [] => "report".to_string(),
        [one] => format!("{one} compare"),
        all => short_versus(all),
    };
    // File names are limited to 255 bytes.
    let mut bytes = 0;
    let stem: String = stem
        .chars()
        .take(80)
        .take_while(|c| {
            bytes += c.len_utf8();
            bytes <= 200
        })
        .collect();
    format!("{}.html", no_spaces(&super::rules::file_name(&stem)))
}

/// `s` with each run of white space written as `_`, or left out next to a `-` or `_`.
fn no_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() && !matches!(c, '-' | '_') && !out.ends_with(['-', '_']) {
            out.push('_');
        }
        space = false;
        out.push(c);
    }
    out
}

/// "a vs b" for names that may share a long start: the shared words are written once, cut to a
/// few words, and only the parts that differ are compared.
fn short_versus(names: &[String]) -> String {
    const HEAD: usize = 24;
    const PART: usize = 32;
    // Not '.', which also separates the parts of version numbers.
    let is_sep = |c: char| matches!(c, '_' | '-' | ' ');
    let chars: Vec<Vec<char>> = names.iter().map(|n| n.chars().collect()).collect();
    let common = (0..)
        .take_while(|&i| {
            let c = chars[0].get(i);
            c.is_some() && chars.iter().all(|n| n.get(i) == c)
        })
        .count();
    // Shared words only: stop after the last separator of the shared start.
    let cut = chars[0][..common]
        .iter()
        .rposition(|&c| is_sep(c))
        .map_or(0, |i| i + 1);
    let rests: Vec<String> = chars
        .iter()
        .map(|n| n[cut..].iter().take(PART).collect())
        .collect();
    if cut == 0 || rests.iter().any(|r| r.is_empty()) {
        return names
            .iter()
            .map(|n| n.chars().take(PART).collect::<String>())
            .collect::<Vec<_>>()
            .join(" vs ");
    }
    let shared = &chars[0][..cut];
    // Whole words that fit, or the start of a first word too long to fit.
    let end = (0..shared.len().min(HEAD + 1))
        .rev()
        .find(|&i| is_sep(shared[i]))
        .filter(|&i| i > 0)
        .unwrap_or(HEAD.min(shared.len()));
    let head: String = shared[..end].iter().collect();
    let head = head.trim_matches(is_sep);
    if head.is_empty() {
        return rests.join(" vs ");
    }
    format!("{head} {}", rests.join(" vs "))
}

/// Folder report of `items`; HTML and XML reports also name the compare rules file `rules`
/// ("" = none). A slideshow needs the files' differences: see [`folder_slideshow`].
pub fn generate(
    items: &[&DirItem],
    roots: &[PathBuf],
    format: ReportFormat,
    rules: &str,
) -> String {
    if format == ReportFormat::Xml {
        return super::xml_report::folder_xml(items, roots, rules);
    }
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
            // Spreadsheets read a CSV file as UTF-8 only when it starts with a byte order mark.
            out.push('\u{feff}');
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
        // XML returned above; a slideshow without the files' differences is the HTML report.
        ReportFormat::Html | ReportFormat::Xml | ReportFormat::Slideshow => {
            return super::html_report::folder_report(items, roots, roots, None, rules);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_fields_stay_text() {
        assert_eq!(csv_field("=cmd()"), "'=cmd()");
        assert_eq!(csv_field("-x"), "'-x");
        assert_eq!(csv_field("-5"), "-5");
        assert_eq!(csv_field("-2.5e3"), "-2.5e3");
        assert_eq!(csv_field("-2+3+cmd|x"), "'-2+3+cmd|x");
        assert_eq!(csv_field("a\rb"), "\"a\rb\"");
        assert_eq!(csv_field("a,\"b\""), "\"a,\"\"b\"\"\"");
        assert_eq!(csv_field("plain.txt"), "plain.txt");
    }

    #[test]
    fn html_file_names_from_compared_sides() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            html_file_name(&names(&["/a/L/main.rs", "/b/R/main.rs"])),
            "main.rs_compare.html"
        );
        assert_eq!(
            html_file_name(&names(&["/tmp/v1/", "/tmp/v2"])),
            "v1_vs_v2.html"
        );
        assert_eq!(
            html_file_name(&names(&["base:a.txt", "b?.txt"])),
            "base_a.txt_vs_b_.txt.html"
        );
        assert_eq!(html_file_name(&[]), "report.html");
        let long = html_file_name(&names(&[&"比較".repeat(60), &"差分".repeat(60)]));
        assert!(long.len() <= 255 && long.ends_with(".html"));
        let long = "cdn_ucie_ips_skhynix_ucie_axi_512_gearbox_strm_async_ns_2026";
        assert_eq!(
            html_file_name(&names(&[
                &format!("/hdl/{long}0903_R005v0p5"),
                &format!("/hdl/{long}1005_R005v0p5a"),
            ])),
            "cdn_ucie_ips_skhynix_20260903_R005v0p5_vs_20261005_R005v0p5a.html"
        );
        assert_eq!(
            html_file_name(&names(&["app-1.2.0", "app-1.3.0"])),
            "app_1.2.0_vs_1.3.0.html"
        );
        assert_eq!(
            html_file_name(&names(&["report_old.txt", "report_new.txt"])),
            "report_old.txt_vs_new.txt.html"
        );
        assert_eq!(
            html_file_name(&names(&["My  Project - v1", "My  Project - v2"])),
            "My_Project_v1_vs_v2.html"
        );
        assert_eq!(no_spaces("a\u{a0}b \t_ c d__e - f"), "a_b_c_d__e-f");
    }
}
