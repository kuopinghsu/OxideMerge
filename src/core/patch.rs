//! Patch file generation in normal, context and unified formats, and applying unified diffs.

use super::diff::{Block, DiffResult};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum PatchStyle {
    Normal,
    Context,
    Unified,
}

pub struct PatchInput<'a> {
    pub name_a: &'a str,
    pub name_b: &'a str,
    pub date_a: &'a str,
    pub date_b: &'a str,
    pub a: &'a [String],
    pub b: &'a [String],
}

pub fn make_patch(
    input: &PatchInput,
    result: &DiffResult,
    style: PatchStyle,
    context: usize,
) -> String {
    // As with `diff -B`, differences the compare options ignore stay out of the patch, unless a
    // hunk with other changes takes them in: its context lines come from one side only.
    let blocks: Vec<&Block> = result.blocks.iter().collect();
    if blocks.iter().all(|b| b.ignored) {
        return String::new();
    }
    match style {
        PatchStyle::Normal => {
            let shown: Vec<&Block> = blocks.into_iter().filter(|b| !b.ignored).collect();
            normal(input, &shown)
        }
        PatchStyle::Context => context_fmt(input, &blocks, context),
        PatchStyle::Unified => unified(input, &blocks, context),
    }
}

/// Patch of all differing or unique text files of a 2-way folder compare.
pub fn folder_patch(
    roots: &[std::path::PathBuf],
    display_roots: &[String],
    items: &[super::folder::DirItem],
    opts: &super::diff::DiffOptions,
    style: PatchStyle,
    context: usize,
) -> String {
    use super::folder::CmpStatus;
    let mut out = String::new();
    for it in items
        .iter()
        .filter(|i| !i.is_dir && matches!(i.status, CmpStatus::Different | CmpStatus::Unique))
    {
        if it.binary {
            continue;
        }
        let load = |s: usize| -> Option<(Vec<String>, String, String)> {
            let name = format!("{}/{}", display_roots[s].replace('\\', "/"), it.rel_path());
            if !it.exists[s] {
                return Some((Vec::new(), name, String::new()));
            }
            let bytes = std::fs::read(it.path(&roots[s])).ok()?;
            if super::text::looks_binary(&bytes) {
                return None;
            }
            let doc = super::text::doc_from_bytes(None, &bytes, None);
            Some((doc.lines, name, super::report::fmt_time(it.mtime[s])))
        };
        let (Some(a), Some(b)) = (load(0), load(1)) else {
            continue;
        };
        let r = super::diff::compute(&[&a.0, &b.0], opts);
        if r.blocks.is_empty() {
            continue;
        }
        let input = PatchInput {
            name_a: &a.1,
            name_b: &b.1,
            date_a: &a.2,
            date_b: &b.2,
            a: &a.0,
            b: &b.0,
        };
        out.push_str(&make_patch(&input, &r, style, context));
    }
    out
}

fn range_normal(r: &Range<usize>) -> String {
    match r.len() {
        0 => format!("{}", r.start),
        1 => format!("{}", r.start + 1),
        _ => format!("{},{}", r.start + 1, r.end),
    }
}

fn normal(input: &PatchInput, blocks: &[&Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        let (ra, rb) = (&b.ranges[0], &b.ranges[1]);
        let (cmd, la, lb) = if ra.is_empty() {
            ('a', format!("{}", ra.start), range_normal(rb))
        } else if rb.is_empty() {
            ('d', range_normal(ra), format!("{}", rb.start))
        } else {
            ('c', range_normal(ra), range_normal(rb))
        };
        let _ = writeln!(out, "{la}{cmd}{lb}");
        for l in &input.a[ra.clone()] {
            let _ = writeln!(out, "< {l}");
        }
        if cmd == 'c' {
            out.push_str("---\n");
        }
        for l in &input.b[rb.clone()] {
            let _ = writeln!(out, "> {l}");
        }
    }
    out
}

/// Groups blocks into hunks whose context regions overlap, leaving out hunks of ignored blocks.
fn hunks<'a>(blocks: &[&'a Block], ctx: usize) -> Vec<Vec<&'a Block>> {
    let mut out: Vec<Vec<&Block>> = Vec::new();
    for b in blocks {
        if let Some(last) = out.last_mut() {
            let prev = last.last().unwrap();
            if b.ranges[0].start <= prev.ranges[0].end + 2 * ctx {
                last.push(b);
                continue;
            }
        }
        out.push(vec![b]);
    }
    out.retain(|h| h.iter().any(|b| !b.ignored));
    out
}

fn hunk_bounds(h: &[&Block], ctx: usize, na: usize, nb: usize) -> (Range<usize>, Range<usize>) {
    let first = h[0];
    let last = h[h.len() - 1];
    let a_start = first.ranges[0].start.saturating_sub(ctx);
    let lead = first.ranges[0].start - a_start;
    let b_start = first.ranges[1].start - lead.min(first.ranges[1].start);
    let a_end = (last.ranges[0].end + ctx).min(na);
    let trail = a_end - last.ranges[0].end;
    let b_end = (last.ranges[1].end + trail).min(nb);
    (a_start..a_end, b_start..b_end)
}

fn unified_range(r: &Range<usize>) -> String {
    match r.len() {
        0 => format!("{},0", r.start),
        1 => format!("{}", r.start + 1),
        n => format!("{},{}", r.start + 1, n),
    }
}

fn unified(input: &PatchInput, blocks: &[&Block], ctx: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "--- {}\t{}", input.name_a, input.date_a);
    let _ = writeln!(out, "+++ {}\t{}", input.name_b, input.date_b);
    for h in hunks(blocks, ctx) {
        let (ra, rb) = hunk_bounds(&h, ctx, input.a.len(), input.b.len());
        let _ = writeln!(out, "@@ -{} +{} @@", unified_range(&ra), unified_range(&rb));
        let mut pos = ra.start;
        for b in &h {
            for l in &input.a[pos..b.ranges[0].start] {
                let _ = writeln!(out, " {l}");
            }
            for l in &input.a[b.ranges[0].clone()] {
                let _ = writeln!(out, "-{l}");
            }
            for l in &input.b[b.ranges[1].clone()] {
                let _ = writeln!(out, "+{l}");
            }
            pos = b.ranges[0].end;
        }
        for l in &input.a[pos..ra.end] {
            let _ = writeln!(out, " {l}");
        }
    }
    out
}

fn context_range(r: &Range<usize>) -> String {
    match r.len() {
        0 => format!("{}", r.start),
        1 => format!("{}", r.start + 1),
        _ => format!("{},{}", r.start + 1, r.end),
    }
}

fn context_fmt(input: &PatchInput, blocks: &[&Block], ctx: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "*** {}\t{}", input.name_a, input.date_a);
    let _ = writeln!(out, "--- {}\t{}", input.name_b, input.date_b);
    for h in hunks(blocks, ctx) {
        let (ra, rb) = hunk_bounds(&h, ctx, input.a.len(), input.b.len());
        out.push_str("***************\n");
        let _ = writeln!(out, "*** {} ****", context_range(&ra));
        if h.iter().any(|b| !b.ranges[0].is_empty()) {
            let mut pos = ra.start;
            for b in &h {
                for l in &input.a[pos..b.ranges[0].start] {
                    let _ = writeln!(out, "  {l}");
                }
                let mark = if b.ranges[1].is_empty() { '-' } else { '!' };
                for l in &input.a[b.ranges[0].clone()] {
                    let _ = writeln!(out, "{mark} {l}");
                }
                pos = b.ranges[0].end;
            }
            for l in &input.a[pos..ra.end] {
                let _ = writeln!(out, "  {l}");
            }
        }
        let _ = writeln!(out, "--- {} ----", context_range(&rb));
        if h.iter().any(|b| !b.ranges[1].is_empty()) {
            let mut pos = rb.start;
            for b in &h {
                for l in &input.b[pos..b.ranges[1].start] {
                    let _ = writeln!(out, "  {l}");
                }
                let mark = if b.ranges[0].is_empty() { '+' } else { '!' };
                for l in &input.b[b.ranges[1].clone()] {
                    let _ = writeln!(out, "{mark} {l}");
                }
                pos = b.ranges[1].end;
            }
            for l in &input.b[pos..rb.end] {
                let _ = writeln!(out, "  {l}");
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Clone, Debug)]
pub struct Hunk {
    /// 1-based first line in the original file (0 when the hunk adds to an empty file).
    pub old_start: usize,
    /// 1-based first line in the patched file (0 when the hunk empties the file).
    pub new_start: usize,
    pub lines: Vec<HunkLine>,
}

impl Hunk {
    fn old_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect()
    }

    fn new_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Add(s) => Some(s.clone()),
                HunkLine::Remove(_) => None,
            })
            .collect()
    }
}

/// The changes to one file in a unified diff.
#[derive(Clone, Debug)]
pub struct FilePatch {
    pub old_name: String,
    pub new_name: String,
    pub hunks: Vec<Hunk>,
}

impl FilePatch {
    /// Name to show for this file: the new name unless the file is deleted.
    pub fn name(&self) -> &str {
        if self.new_name == "/dev/null" {
            &self.old_name
        } else {
            &self.new_name
        }
    }

    pub fn creates_file(&self) -> bool {
        self.old_name == "/dev/null"
    }

    /// The file this patch applies to under `base`, trying to strip 0..=3 leading path components
    /// (like `patch -p`).
    pub fn locate(&self, base: &std::path::Path) -> Option<std::path::PathBuf> {
        for strip in 0..=3 {
            for name in [&self.new_name, &self.old_name] {
                if name == "/dev/null" {
                    continue;
                }
                let parts: Vec<&str> = name.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
                if parts.len() <= strip {
                    continue;
                }
                let rel: std::path::PathBuf = parts[strip..].iter().collect();
                if rel
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    continue;
                }
                let p = base.join(&rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
}

fn header_name(rest: &str) -> String {
    let name = rest.split('\t').next().unwrap_or("").trim_end();
    let name = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);
    name.to_string()
}

fn parse_range(s: &str) -> Option<(usize, usize)> {
    let (start, len) = match s.split_once(',') {
        Some((a, b)) => (a.parse().ok()?, b.parse().ok()?),
        None => (s.parse().ok()?, 1),
    };
    Some((start, len))
}

/// Parses a unified diff (plain, `git diff` or `svn diff` output) into per-file patches.
pub fn parse_unified(text: &str) -> Result<Vec<FilePatch>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files: Vec<FilePatch> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if let (Some(old), Some(new)) = (
            l.strip_prefix("--- "),
            lines.get(i + 1).and_then(|n| n.strip_prefix("+++ ")),
        ) {
            files.push(FilePatch {
                old_name: header_name(old),
                new_name: header_name(new),
                hunks: Vec::new(),
            });
            i += 2;
            continue;
        }
        if let Some(rest) = l.strip_prefix("@@ -") {
            let Some(file) = files.last_mut() else {
                return Err(format!("line {}: hunk without a file header", i + 1));
            };
            let bad = || format!("line {}: malformed hunk header", i + 1);
            let (ranges, _) = rest.split_once(" @@").ok_or_else(bad)?;
            let (old, new) = ranges.split_once(" +").ok_or_else(bad)?;
            let (old_start, mut old_left) = parse_range(old).ok_or_else(bad)?;
            let (new_start, mut new_left) = parse_range(new).ok_or_else(bad)?;
            let mut hunk = Hunk {
                old_start,
                new_start,
                lines: Vec::new(),
            };
            i += 1;
            while i < lines.len() && (old_left > 0 || new_left > 0) {
                let h = lines[i];
                let (kind, body) = match h.chars().next() {
                    Some(' ') => (' ', &h[1..]),
                    None => (' ', ""),
                    Some('-') => ('-', &h[1..]),
                    Some('+') => ('+', &h[1..]),
                    Some('\\') => {
                        i += 1;
                        continue;
                    }
                    _ => return Err(format!("line {}: unexpected line inside a hunk", i + 1)),
                };
                match kind {
                    ' ' if old_left > 0 && new_left > 0 => {
                        old_left -= 1;
                        new_left -= 1;
                        hunk.lines.push(HunkLine::Context(body.to_string()));
                    }
                    '-' if old_left > 0 => {
                        old_left -= 1;
                        hunk.lines.push(HunkLine::Remove(body.to_string()));
                    }
                    '+' if new_left > 0 => {
                        new_left -= 1;
                        hunk.lines.push(HunkLine::Add(body.to_string()));
                    }
                    _ => {
                        return Err(format!(
                            "line {}: hunk is longer than its header says",
                            i + 1
                        ))
                    }
                }
                i += 1;
            }
            if old_left > 0 || new_left > 0 {
                return Err("the patch ends in the middle of a hunk".into());
            }
            file.hunks.push(hunk);
            continue;
        }
        i += 1;
    }
    files.retain(|f| !f.hunks.is_empty());
    if files.is_empty() {
        return Err("no unified diff hunks found".into());
    }
    Ok(files)
}

pub struct Applied {
    pub lines: Vec<String>,
    /// 1-based numbers of the hunks that could not be placed.
    pub failed: Vec<usize>,
}

fn matches_at(lines: &[String], at: usize, old: &[&str], loose: bool) -> bool {
    at + old.len() <= lines.len()
        && old.iter().zip(&lines[at..]).all(|(o, l)| {
            if loose {
                o.trim_end() == l.trim_end()
            } else {
                o == l
            }
        })
}

/// Applies the hunks of `patch` to `lines`. Each hunk is looked for near its stated position
/// (adjusted by the hunks before it), first exactly, then ignoring trailing whitespace.
pub fn apply(lines: &[String], patch: &FilePatch) -> Applied {
    let mut out = lines.to_vec();
    let mut failed = Vec::new();
    let mut delta: isize = 0;
    let mut floor = 0usize;
    for (n, h) in patch.hunks.iter().enumerate() {
        let old = h.old_lines();
        // A hunk without old lines (from `diff -U0`) goes after line `old_start`. Its new start
        // also counts hunks left out of the patch, but tells `-1,0 +1`, which some diffs write
        // for the top of the file, from a line after the first, which would start at 2.
        let at = if old.is_empty() && (h.old_start, h.new_start) == (1, 1) {
            delta
        } else if old.is_empty() {
            h.old_start as isize + delta
        } else {
            h.old_start.saturating_sub(1) as isize + delta
        };
        let expected = at.max(floor as isize) as usize;
        let last = out.len().saturating_sub(old.len());
        let found = [false, true].into_iter().find_map(|loose| {
            (0..=out.len()).find_map(|d| {
                let after = expected + d;
                if after <= last && after >= floor && matches_at(&out, after, &old, loose) {
                    return Some(after);
                }
                let before = expected.checked_sub(d)?;
                (d > 0 && before >= floor && matches_at(&out, before, &old, loose))
                    .then_some(before)
            })
        });
        match found {
            Some(found) => {
                let new = h.new_lines();
                let added = new.len();
                out.splice(found..found + old.len(), new);
                // The hunks after it are as far from their stated lines as this one was.
                delta += found as isize - at + added as isize - old.len() as isize;
                floor = found + added;
            }
            None => failed.push(n + 1),
        }
    }
    Applied { lines: out, failed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{compute, DiffOptions};

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_and_applies_unified_diffs_with_offsets() {
        let a = v(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ]);
        let b = v(&[
            "one", "TWO", "three", "four", "five", "six", "seven", "eight", "nine",
        ]);
        let r = compute(&[&a, &b], &DiffOptions::default());
        let input = PatchInput {
            name_a: "a/dir/f.txt",
            name_b: "b/dir/f.txt",
            date_a: "",
            date_b: "",
            a: &a,
            b: &b,
        };
        let text = format!(
            "diff --git a/dir/f.txt b/dir/f.txt\n{}",
            make_patch(&input, &r, PatchStyle::Unified, 1)
        );
        let files = parse_unified(&text).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name(), "b/dir/f.txt");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(apply(&a, &files[0]).lines, b);

        // The same patch still applies when lines were inserted above the hunks.
        let mut shifted = v(&["new 1", "new 2"]);
        shifted.extend(a.iter().cloned());
        let res = apply(&shifted, &files[0]);
        assert!(res.failed.is_empty());
        assert_eq!(res.lines[3], "TWO");
        assert_eq!(res.lines.last().unwrap(), "nine");

        // A hunk whose context is gone is reported, the others are still applied.
        let mut broken = a.clone();
        broken[0] = "uno".into();
        broken[2] = "tres".into();
        let res = apply(&broken, &files[0]);
        assert_eq!(res.failed, vec![1]);
        assert_eq!(res.lines.last().unwrap(), "nine");

        let dir = std::env::temp_dir().join(format!("om_patch_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("dir")).unwrap();
        std::fs::write(dir.join("dir/f.txt"), "x").unwrap();
        assert_eq!(files[0].locate(&dir), Some(dir.join("dir/f.txt")));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(parse_unified("just text").is_err());
        let new_file =
            parse_unified("--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n").unwrap();
        assert!(new_file[0].creates_file());
        assert_eq!(apply(&[], &new_file[0]).lines, v(&["a", "b"]));
    }

    #[test]
    fn unified_patch() {
        let a: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["a", "x", "c"].iter().map(|s| s.to_string()).collect();
        let r = compute(&[&a, &b], &DiffOptions::default());
        let input = PatchInput {
            name_a: "a.txt",
            name_b: "b.txt",
            date_a: "",
            date_b: "",
            a: &a,
            b: &b,
        };
        let p = make_patch(&input, &r, PatchStyle::Unified, 3);
        assert!(p.contains("@@ -1,3 +1,3 @@"));
        assert!(p.contains("-b\n+x\n"));
        let n = make_patch(&input, &r, PatchStyle::Normal, 3);
        assert!(n.starts_with("2c2\n< b\n---\n> x\n"));
    }

    #[test]
    fn ignored_changes_in_a_hunk_stay_and_the_others_go() {
        let far: Vec<String> = (1..=10).map(|i| format!("k{i}")).collect();
        let mut a = v(&["x", "", "y", "1"]);
        a.extend(far.iter().cloned());
        a.extend(v(&["", "z"]));
        let mut b = v(&["x", "y", "2"]);
        b.extend(far.iter().cloned());
        b.push("z".into());
        let opts = DiffOptions {
            ignore_blank_lines: true,
            ..DiffOptions::default()
        };
        let r = compute(&[&a, &b], &opts);
        let input = PatchInput {
            name_a: "a.txt",
            name_b: "b.txt",
            date_a: "",
            date_b: "",
            a: &a,
            b: &b,
        };
        let p = make_patch(&input, &r, PatchStyle::Unified, 3);
        assert_eq!(p.matches("@@ -").count(), 1, "{p}");
        let mut want = b.clone();
        want.insert(want.len() - 1, String::new());
        let files = parse_unified(&p).unwrap();
        assert_eq!(apply(&a, &files[0]).lines, want);
        // Every hunk holds as many lines as its header says.
        let body: Vec<&str> = p.lines().skip(3).collect();
        let old = body.iter().filter(|l| !l.starts_with('+')).count();
        let new = body.iter().filter(|l| !l.starts_with('-')).count();
        assert!(p.contains(&format!("@@ -1,{old} +1,{new} @@")), "{p}");
        let n = make_patch(&input, &r, PatchStyle::Normal, 3);
        assert_eq!(n, "4c3\n< 1\n---\n> 2\n");
    }
}
