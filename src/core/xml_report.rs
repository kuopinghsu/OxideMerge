//! XML reports of text and folder compares, for other programs to read: every difference with
//! its lines, or every file with its status, dates and sizes.
//!
//! The layout (version 1):
//!
//! ```xml
//! <comparison format="1" tool="OxideMerge" version="…" type="text|folder" created="…">
//!   <side index="0" role="left" name="…"/> …
//!   <rules name="…"/>                                     (when a rules file is used)
//!   <statistics …/>
//!   text:   <difference index="1" kind="changed|added|removed|left-only|right-only|both-same|conflict"
//!                       ignored="false"><lines side="0" start="12" count="2"><line number="12">…</line>…</lines>…</difference>
//!           <bookmark side="0" line="5">comment</bookmark>
//!   folder: <item path="…" name="…" folder="…" type="file|folder" status="identical|different|unique|error|other"
//!                 description="…"><on side="0" date="…" size="…"/>…</item>
//! </comparison>
//! ```
//!
//! Line numbers start at 1. Each side's `<lines>` are its lines in the difference; a side with
//! none (added or removed text) has `count="0"`, and `start` the line it would go before.

use super::diff::{BlockKind, DiffResult};
use super::folder::{CmpStatus, DirItem};
use super::report::{fmt_size, fmt_time, status_text};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

/// `s` escaped for XML text and attribute values. Characters XML 1.0 can't hold (most control
/// characters) become U+FFFD.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || matches!(c, '\u{FFFE}' | '\u{FFFF}') => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

fn role(side: usize, panes: usize) -> &'static str {
    match (side, panes) {
        (0, _) => "left",
        (1, 3) => "middle",
        _ => "right",
    }
}

fn header(out: &mut String, kind: &str, names: &[String], rules: &str) {
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let _ = writeln!(
        out,
        "<comparison format=\"1\" tool=\"OxideMerge\" version=\"{}\" type=\"{kind}\" created=\"{now}\">",
        env!("CARGO_PKG_VERSION")
    );
    let n = names.len();
    for (i, name) in names.iter().enumerate() {
        let _ = writeln!(
            out,
            "  <side index=\"{i}\" role=\"{}\" name=\"{}\"/>",
            role(i, n),
            esc(name)
        );
    }
    if !rules.is_empty() {
        let _ = writeln!(
            out,
            "  <rules name=\"{}\"/>",
            esc(super::rules::display_name(rules))
        );
    }
}

/// The kind of difference `b`, as the report names it.
pub fn kind(result: &DiffResult, b: usize) -> &'static str {
    let blk = &result.blocks[b];
    if result.panes == 2 {
        return match (blk.ranges[0].is_empty(), blk.ranges[1].is_empty()) {
            (true, _) => "added",
            (_, true) => "removed",
            _ => "changed",
        };
    }
    match blk.kind {
        BlockKind::Conflict => "conflict",
        BlockKind::LeftOnly => "left-only",
        BlockKind::RightOnly => "right-only",
        BlockKind::BothSame => "both-same",
        BlockKind::Diff => "changed",
    }
}

/// Report of a 2-way or 3-way text compare: every difference, ignored ones marked, with the
/// lines of each side, then the bookmarks with their comments.
pub fn text_xml(
    titles: &[String],
    panes: &[&[String]],
    result: &DiffResult,
    notes: &[BTreeMap<usize, String>],
    rules: &str,
) -> String {
    let n = panes.len();
    let mut out = String::new();
    header(&mut out, "text", titles, rules);
    let significant = result.significant_blocks();
    let ignored = result.blocks.len() - significant;
    if n == 2 {
        let s = result.stats();
        let _ = writeln!(
            out,
            "  <statistics differences=\"{significant}\" ignored=\"{ignored}\" identical-lines=\"{}\" changed-lines=\"{}\" added-lines=\"{}\" removed-lines=\"{}\"/>",
            s.identical, s.changed, s.added, s.removed
        );
    } else {
        let _ = writeln!(
            out,
            "  <statistics differences=\"{significant}\" ignored=\"{ignored}\" conflicts=\"{}\"/>",
            result.conflicts()
        );
    }
    out.push_str("  <differences>\n");
    for (i, blk) in result.blocks.iter().enumerate() {
        let _ = writeln!(
            out,
            "    <difference index=\"{}\" kind=\"{}\" ignored=\"{}\">",
            i + 1,
            kind(result, i),
            blk.ignored
        );
        for (p, lines) in panes.iter().enumerate() {
            let r = &blk.ranges[p];
            let _ = write!(
                out,
                "      <lines side=\"{p}\" start=\"{}\" count=\"{}\"",
                r.start + 1,
                r.len()
            );
            if r.is_empty() {
                out.push_str("/>\n");
                continue;
            }
            out.push_str(">\n");
            for l in r.clone() {
                let _ = writeln!(
                    out,
                    "        <line number=\"{}\">{}</line>",
                    l + 1,
                    esc(&lines[l])
                );
            }
            out.push_str("      </lines>\n");
        }
        out.push_str("    </difference>\n");
    }
    out.push_str("  </differences>\n");
    let marks: Vec<(usize, usize, &str)> = notes
        .iter()
        .enumerate()
        .flat_map(|(p, m)| m.iter().map(move |(&l, note)| (p, l, note.as_str())))
        .collect();
    if !marks.is_empty() {
        out.push_str("  <bookmarks>\n");
        for (p, l, note) in marks {
            let _ = writeln!(
                out,
                "    <bookmark side=\"{p}\" line=\"{}\">{}</bookmark>",
                l + 1,
                esc(note)
            );
        }
        out.push_str("  </bookmarks>\n");
    }
    out.push_str("</comparison>\n");
    out
}

fn status(st: CmpStatus) -> &'static str {
    match st {
        CmpStatus::Identical => "identical",
        CmpStatus::Different => "different",
        CmpStatus::Unique => "unique",
        CmpStatus::Error => "error",
        CmpStatus::NotCompared | CmpStatus::Pending => "other",
    }
}

/// Report of a folder compare: every item with its status, and its date and size on each side
/// it is on. `roots` names the compared folders.
pub fn folder_xml(items: &[&DirItem], roots: &[PathBuf], rules: &str) -> String {
    let n = roots.len();
    let names: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
    let mut out = String::new();
    header(&mut out, "folder", &names, rules);
    let count = |st: &str| items.iter().filter(|it| status(it.status) == st).count();
    let _ = writeln!(
        out,
        "  <statistics items=\"{}\" identical=\"{}\" different=\"{}\" unique=\"{}\" errors=\"{}\" not-compared=\"{}\"/>",
        items.len(),
        count("identical"),
        count("different"),
        count("unique"),
        count("error"),
        count("other")
    );
    out.push_str("  <items>\n");
    for it in items {
        let _ = writeln!(
            out,
            "    <item path=\"{}\" name=\"{}\" folder=\"{}\" type=\"{}\" status=\"{}\" description=\"{}\">",
            esc(&it.rel_path()),
            esc(&it.name),
            esc(&it.rel_dir),
            if it.is_dir { "folder" } else { "file" },
            status(it.status),
            esc(&status_text(it, n))
        );
        for s in 0..n {
            if !it.exists[s] {
                continue;
            }
            let _ = write!(out, "      <on side=\"{s}\"");
            if it.mtime[s].is_some() {
                let _ = write!(out, " date=\"{}\"", esc(&fmt_time(it.mtime[s])));
            }
            if let (Some(bytes), false) = (it.size[s], it.is_dir) {
                let _ = write!(
                    out,
                    " size=\"{bytes}\" size-text=\"{}\"",
                    esc(&fmt_size(it.size[s]))
                );
            }
            out.push_str("/>\n");
        }
        out.push_str("    </item>\n");
    }
    out.push_str("  </items>\n</comparison>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{compute, DiffOptions};

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn text_reports_list_each_difference_with_its_lines() {
        let a = v(&["same", "old <x> & \"y\"", "same", "gone"]);
        let b = v(&["same", "new", "same"]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        let mut notes = vec![BTreeMap::new(), BTreeMap::new()];
        notes[1].insert(1, "check".to_string());
        let x = text_xml(&["a.txt".into(), "b.txt".into()], &[&a, &b], &r, &notes, "");
        assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<comparison "));
        assert!(x.contains("type=\"text\""));
        assert!(x.contains("<side index=\"1\" role=\"right\" name=\"b.txt\"/>"));
        assert!(x.contains("<difference index=\"1\" kind=\"changed\" ignored=\"false\">"));
        assert!(x.contains("<line number=\"2\">old &lt;x&gt; &amp; &quot;y&quot;</line>"));
        assert!(x.contains("<difference index=\"2\" kind=\"removed\""));
        assert!(x.contains("<lines side=\"1\" start=\"4\" count=\"0\"/>"));
        assert!(x.contains("<bookmark side=\"1\" line=\"2\">check</bookmark>"));
        assert!(x.contains("differences=\"2\""));
        assert!(x.trim_end().ends_with("</comparison>"));
        // Well formed: every element closed, in order.
        let mut stack = Vec::new();
        for tag in x.split('<').skip(2) {
            let tag = tag.split('>').next().unwrap();
            if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(stack.pop(), Some(name.to_string()));
            } else if !tag.ends_with('/') {
                stack.push(tag.split_whitespace().next().unwrap().to_string());
            }
        }
        assert!(stack.is_empty(), "{stack:?}");
    }

    #[test]
    fn control_characters_are_replaced() {
        assert_eq!(esc("a\u{1}b\tc"), "a\u{FFFD}b\tc");
    }

    #[test]
    fn three_way_kinds_are_named() {
        let base = v(&["a", "b", "c", "d", "e"]);
        let left = v(&["a", "B", "c", "d", "e"]);
        let right = v(&["a", "b", "c", "d", "E"]);
        let r = compute(&[&left, &base, &right], &DiffOptions::default());
        let x = text_xml(
            &["l".into(), "b".into(), "r".into()],
            &[&left, &base, &right],
            &r,
            &[],
            "",
        );
        assert!(x.contains("role=\"middle\""));
        assert!(x.contains("kind=\"left-only\"") && x.contains("kind=\"right-only\""));
        assert!(x.contains("conflicts=\"0\""));
    }
}
