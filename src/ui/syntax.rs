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

const MAX_LINES: usize = 50_000;

pub fn highlight(lines: &[String], ext: &str, dark: bool) -> Option<Vec<LineSpans>> {
    if lines.len() > MAX_LINES {
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

pub fn spawn(lines: Vec<String>, ext: String, dark: bool) -> Receiver<Option<Vec<LineSpans>>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(highlight(&lines, &ext, dark));
    });
    rx
}
