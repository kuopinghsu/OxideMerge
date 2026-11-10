//! Syntax highlighting via syntect, computed on a background thread.

use egui::Color32;
use std::ops::Range;
use std::sync::mpsc::{channel, Receiver};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

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

/// Check before cloning input for the worker, not only after allocating its copy.
pub fn within_limits(lines: &[String], budget: Budget) -> bool {
    lines.len() <= budget.lines
        && lines
            .iter()
            .try_fold(0usize, |total, line| {
                let next = total.checked_add(line.len())?;
                (next <= budget.bytes).then_some(next)
            })
            .is_some()
}

/// Whether files named with extension `ext` have a syntax to highlight.
pub fn has_syntax(ext: &str) -> bool {
    highlighter()
        .ss
        .find_syntax_by_extension(ext)
        .is_some_and(|s| s.name != "Plain Text")
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
    if !within_limits(lines, budget) {
        return None;
    }
    let h = highlighter();
    let first = lines.first().map(|s| s.as_str()).unwrap_or("");
    let syntax =
        h.ss.find_syntax_by_extension(ext)
            .or_else(|| h.ss.find_syntax_by_first_line(first))?;
    if syntax.name == "Plain Text" {
        return None;
    }
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
        assert!(within_limits(&["let n = 1;".into()], b));
        assert!(!within_limits(&vec![String::new(); b.lines + 1], b));
        let lines = vec!["x".repeat(b.bytes + 1)];
        assert!(!within_limits(&lines, b));
        assert!(highlight(&lines, "rs", false).is_none());
        // A larger budget takes them.
        assert!(within_limits(
            &vec![String::new(); b.lines + 1],
            Budget::of_mb(17)
        ));
        assert!(within_limits(&lines, Budget::of_mb(17)));
    }
}
