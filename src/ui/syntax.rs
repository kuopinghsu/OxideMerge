//! Syntax highlighting via syntect, computed on a background thread.

use egui::Color32;
use std::ops::Range;
use std::sync::mpsc::{channel, Receiver};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub type LineSpans = Vec<(Range<usize>, Color32)>;

struct Highlighter {
    ss: SyntaxSet,
    light: Theme,
    dark: Theme,
}

fn highlighter() -> &'static Highlighter {
    static H: OnceLock<Highlighter> = OnceLock::new();
    H.get_or_init(|| {
        let ss = SyntaxSet::load_defaults_newlines();
        let mut ts = ThemeSet::load_defaults();
        let light = ts.themes.remove("InspiredGitHub").unwrap_or_default();
        let dark = ts.themes.remove("base16-ocean.dark").unwrap_or_default();
        Highlighter { ss, light, dark }
    })
}

/// The default size of a file that is highlighted, in MiB ([`crate::config::Config::highlight_limit_mb`]).
pub const DEFAULT_LIMIT_MB: u32 = 16;

/// How much a file may hold to be highlighted: `mb` MiB of text, and 3,125 lines for each
/// MiB (50,000 lines at the default 16 MiB), as highlighting takes time for each line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub lines: usize,
    pub bytes: usize,
}

impl Budget {
    pub fn of_mb(mb: u32) -> Budget {
        let mb = mb.max(1) as usize;
        Budget {
            lines: mb * 3125,
            bytes: mb << 20,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Budget::of_mb(DEFAULT_LIMIT_MB)
    }
}

/// Which part of a [`Budget`] a file goes over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Over {
    Lines,
    Bytes,
}

/// Check before cloning input for the worker, not only after allocating its copy.
pub fn over_budget(lines: &[String], budget: Budget) -> Option<Over> {
    if lines.len() > budget.lines {
        return Some(Over::Lines);
    }
    lines
        .iter()
        .try_fold(0usize, |total, line| {
            let next = total.checked_add(line.len())?;
            (next <= budget.bytes).then_some(next)
        })
        .is_none()
        .then_some(Over::Bytes)
}

/// The syntax of a file named with extension `ext` whose first line is `first`, as a shebang
/// can name it; none for plain text.
fn find_syntax<'a>(ss: &'a SyntaxSet, ext: &str, first: &str) -> Option<&'a SyntaxReference> {
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_first_line(first))
        .filter(|s| s.name != "Plain Text")
}

/// Whether a file named with extension `ext` whose first line is `first` has a syntax to
/// highlight.
pub fn has_syntax(ext: &str, first: &str) -> bool {
    find_syntax(&highlighter().ss, ext, first).is_some()
}

/// [`has_syntax`] on a thread of its own: the first look loads syntect's syntaxes, which
/// takes too long for a frame.
pub fn spawn_has_syntax(ext: String, first: String) -> Receiver<bool> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(has_syntax(&ext, &first));
    });
    rx
}

pub fn highlight(lines: &[String], ext: &str, dark: bool) -> Option<Vec<LineSpans>> {
    highlight_within(lines, ext, dark, Budget::default())
}

fn highlight_within(
    lines: &[String],
    ext: &str,
    dark: bool,
    budget: Budget,
) -> Option<Vec<LineSpans>> {
    if over_budget(lines, budget).is_some() {
        return None;
    }
    let h = highlighter();
    let first = lines.first().map(|s| s.as_str()).unwrap_or("");
    let syntax = find_syntax(&h.ss, ext, first)?;
    let theme = if dark { &h.dark } else { &h.light };
    let mut hl = HighlightLines::new(syntax, theme);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let with_nl = format!("{line}\n");
        let regions = hl.highlight_line(&with_nl, &h.ss).ok()?;
        let mut spans = Vec::with_capacity(regions.len());
        let mut pos = 0;
        for (style, text) in regions {
            let end = (pos + text.len()).min(line.len());
            if pos < end {
                let c = style.foreground;
                spans.push((pos..end, Color32::from_rgb(c.r, c.g, c.b)));
            }
            pos += text.len();
        }
        out.push(spans);
    }
    Some(out)
}

pub fn spawn(
    lines: Vec<String>,
    ext: String,
    dark: bool,
    budget: Budget,
) -> Receiver<Option<Vec<LineSpans>>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(highlight_within(&lines, &ext, dark, budget));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_large_line_counts_and_single_giant_lines_before_worker_copy() {
        let b = Budget::default();
        assert_eq!(
            b,
            Budget {
                lines: 50_000,
                bytes: 16 << 20
            }
        );
        assert_eq!(over_budget(&["let n = 1;".into()], b), None);
        assert_eq!(
            over_budget(&vec![String::new(); b.lines + 1], b),
            Some(Over::Lines)
        );
        let lines = vec!["x".repeat(b.bytes + 1)];
        assert_eq!(over_budget(&lines, b), Some(Over::Bytes));
        assert!(highlight(&lines, "rs", false).is_none());
        // A larger budget takes them.
        assert_eq!(
            over_budget(&vec![String::new(); b.lines + 1], Budget::of_mb(17)),
            None
        );
        assert_eq!(over_budget(&lines, Budget::of_mb(17)), None);
    }

    /// A file without a known extension has the syntax its shebang names, wherever it is
    /// looked up.
    #[test]
    fn a_shebang_names_the_syntax_for_the_check_and_the_highlighter_alike() {
        let script = vec!["#!/bin/bash".to_string(), "echo hi".into()];
        assert!(has_syntax("", &script[0]));
        assert!(spawn_has_syntax(String::new(), script[0].clone())
            .recv()
            .unwrap());
        assert!(highlight(&script, "", false).is_some());
        assert!(has_syntax("rs", ""));
        assert!(!has_syntax("", "just words"));
        assert!(highlight(&["just words".to_string()], "", false).is_none());
        // A known extension of plain text wins over a first line.
        assert!(!has_syntax("txt", &script[0]));
    }
}
