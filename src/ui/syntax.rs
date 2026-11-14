//! Syntax highlighting via syntect, computed on a background thread. The languages syntect
//! does not come with are defined in [`langs`].

mod langs;

use egui::Color32;
use std::ops::Range;
use std::path::Path;
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
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        langs::add_to(&mut builder);
        let ss = builder.build();
        let mut ts = ThemeSet::load_defaults();
        let light = ts.themes.remove("InspiredGitHub").unwrap_or_default();
        let dark = ts.themes.remove("base16-ocean.dark").unwrap_or_default();
        Highlighter { ss, light, dark }
    })
}

/// Loads the syntaxes on a thread of its own, once, so that the first file highlighted, or
/// the first excerpt a view highlights while drawing, does not wait for them.
pub fn warm() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            highlighter();
        });
    });
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

/// What names the language of the file at `path` to the functions here: its extension in
/// lower case, or an extension of the language its whole name stands for (`Dockerfile`,
/// `CMakeLists.txt`, `Makefile`).
pub fn key(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match langs::file_name_key(&name) {
        Some(key) => key.to_string(),
        None => crate::core::text::extension(path),
    }
}

/// The syntax of a file named with extension `ext` whose first line is `first`, as a shebang
/// can name it; none for plain text.
fn find_syntax<'a>(ss: &'a SyntaxSet, ext: &str, first: &str) -> Option<&'a SyntaxReference> {
    let key = langs::by_first_line(ext, first)
        .or_else(|| langs::alias(ext))
        .unwrap_or(ext);
    ss.find_syntax_by_extension(key)
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

    /// The pieces of `text`, the lines of a file with extension `ext`, with their colours
    /// in the light theme.
    fn colours(ext: &str, text: &str) -> Vec<(String, Color32)> {
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let spans = highlight(&lines, ext, false).unwrap_or_else(|| panic!("no syntax: {ext}"));
        lines
            .iter()
            .zip(spans)
            .flat_map(|(line, spans)| {
                spans
                    .into_iter()
                    .map(move |(range, colour)| (line[range].to_string(), colour))
            })
            .collect()
    }

    fn colour_of(spans: &[(String, Color32)], text: &str) -> Color32 {
        let found = spans.iter().find(|(t, _)| t.contains(text));
        found.unwrap_or_else(|| panic!("{text} in {spans:?}")).1
    }

    /// Each language added to syntect's own is found by its extension and colours a
    /// comment, a string and a keyword, each differently from plain text.
    #[test]
    fn every_added_language_colours_comments_strings_and_keywords() {
        let plain = colour_of(&colours("rs", "x"), "x");
        for &(ext, text, keywords) in langs::tests::SAMPLES {
            assert!(has_syntax(ext, ""), "{ext}");
            let spans = colours(ext, text);
            let comment = colour_of(&spans, "note");
            let string = colour_of(&spans, "x");
            let keyword = colours(ext, keywords)[0].1;
            assert_ne!(comment, plain, "{ext}: comment");
            assert_ne!(string, plain, "{ext}: string");
            assert_ne!(keyword, plain, "{ext}: keyword in {keywords}");
            assert_ne!(comment, string, "{ext}");
            assert_ne!(comment, keyword, "{ext}");
            // The dark theme has them too.
            let lines: Vec<String> = text.lines().map(String::from).collect();
            let dark = highlight(&lines, ext, true).unwrap();
            let mut seen: Vec<Color32> = dark.iter().flatten().map(|(_, c)| *c).collect();
            seen.dedup();
            assert!(seen.len() > 1, "{ext}: {dark:?}");
        }
    }

    /// A settings file with a line padded out with spaces is highlighted in a time that
    /// grows with its length, not with the square of it; warming up loads the same syntaxes.
    #[test]
    fn a_long_padded_line_of_settings_is_highlighted() {
        warm();
        let lines = vec![
            format!("name{}", " ".repeat(200_000)),
            "key = \"x\" ".to_string(),
        ];
        let spans = highlight(&lines, "conf", false).unwrap();
        assert_eq!(spans.len(), 2);
        assert!(spans[1].len() > 1, "{:?}", spans[1]);
    }

    /// The file extensions other comparison tools highlight are covered: by an added
    /// language, or by one of syntect's under another extension.
    #[test]
    fn common_extensions_have_a_syntax() {
        let exts = "c cc cpp cxx h hh hpp hxx sma m mm cs d rc rc2 dlg idl odl as asc jsfl java \
                    js vb bas frm cls ctl pag dsr dob vbs dsm dpr dpk pas dfm inc pp html htm \
                    asp shtml htd php3 phtml php htt cfm tpl dtd hta vxml xml xsl xslt svg xul \
                    xsd axl xrc rdf vcproj wxs wxi docbook css caml ml mli fs fsi erl hrl f90 \
                    f95 f2k f for lsp lisp scm smd ss pl pm cgi pod py pyw rb rbw sbt sc scala \
                    st pac sql body sps spb sf sp md toml yaml yml ini cmake ps1 \
                    psm1 kt kts swift go lua v sv svh vhd vhdl asm s proto ts tsx mk sh json rs";
        for ext in exts.split_whitespace() {
            assert!(has_syntax(ext, ""), "{ext}");
        }
        // The same language, whichever of its extensions names it.
        let lines = ["SELECT 'x' FROM t; -- note".to_string()];
        let sql = highlight(&lines, "sql", false);
        assert_eq!(highlight(&lines, "sps", false), sql);
        assert_ne!(highlight(&lines, "c", false), sql);
    }

    /// An extension several languages share goes by the first line of the file.
    #[test]
    fn shared_extensions_go_by_the_first_line() {
        let same = |ext: &str, first: &str, as_ext: &str| {
            let lines = [first.to_string(), "x = \"a\" // b ' c".to_string()];
            let got = highlight(&lines, ext, false);
            assert!(got.is_some(), "{ext}: {first}");
            assert_eq!(got, highlight(&lines, as_ext, false), "{ext}: {first}");
        };
        same("cls", "VERSION 1.0 CLASS", "vb");
        same("cls", "\\NeedsTeXFormat{LaTeX2e}", "sty");
        same("cgi", "use strict;", "pl");
        same("cgi", "#!/usr/bin/env ruby", "rb");
        same("fs", "module Top", "fsx");
        same("fs", "#version 330 core", "c");
        same("inc", "<?php", "html");
        same("inc", "// shared by the pages", "html");
        same("inc", "typedef struct point point;", "html");
        same("fs", "in vec2 uv;", "c");
        same("inc", "{ shared constants }", "pas");
        same("inc", "; macros", "asm");
        same("spec", "CREATE OR REPLACE PACKAGE top AS", "sql");
        same("spec", "# -*- mode: python ; coding: utf-8 -*-", "py");
        // The description of an RPM package is in no language known here.
        assert!(!has_syntax("spec", "Name: top"));
        assert!(highlight(&["Name: top".to_string()], "spec", false).is_none());
        // Nor are these, which were read as Standard ML and Smalltalk.
        for ext in ["sig", "fun", "src", "pkg"] {
            assert!(!has_syntax(ext, ""), "{ext}");
        }
    }

    /// A file whose whole name tells its language is highlighted as that.
    #[test]
    fn whole_file_names_name_their_language() {
        for (name, ext) in [
            ("Dockerfile", "dockerfile"),
            ("Dockerfile.dev", "dockerfile"),
            ("app.dockerfile", "dockerfile"),
            ("Containerfile", "dockerfile"),
            ("CMakeLists.txt", "cmake"),
            ("Makefile", "makefile"),
            ("GNUmakefile", "makefile"),
            ("SConstruct", "py"),
            ("Cargo.lock", "toml"),
            (".editorconfig", "ini"),
            ("main.RS", "rs"),
            ("notes.txt", "txt"),
            ("README", ""),
        ] {
            let key = key(&Path::new("dir").join(name));
            assert_eq!(key, ext, "{name}");
            assert_eq!(has_syntax(&key, ""), !["txt", ""].contains(&ext), "{name}");
        }
    }
}
