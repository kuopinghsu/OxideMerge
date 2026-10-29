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
    pub final_newline_a: bool,
    pub final_newline_b: bool,
}

pub fn make_patch(
    input: &PatchInput,
    result: &DiffResult,
    style: PatchStyle,
    context: usize,
) -> String {
    let mut formatted;
    let result = if input.final_newline_a != input.final_newline_b {
        formatted = result.clone();
        let mut a = super::text::TextDoc::empty(None);
        let mut b = super::text::TextDoc::empty(None);
        a.lines = input.a.to_vec();
        b.lines = input.b.to_vec();
        a.final_newline = input.final_newline_a;
        b.final_newline = input.final_newline_b;
        super::diff::include_doc_formats(
            &mut formatted,
            &[&a, &b],
            &super::diff::DiffOptions::default(),
        );
        &formatted
    } else {
        result
    };
    // As with `diff -B`, differences the compare options ignore stay out of the patch, unless a
    // hunk with other changes takes them in: its context lines come from one side only.
    // Metadata can split one replacement into adjacent blocks. Coalesce those before
    // emitting removals/additions so a hunk has the same ordering as a native diff.
    let mut merged: Vec<Block> = Vec::new();
    for block in &result.blocks {
        if let Some(last) = merged.last_mut() {
            if last.ignored == block.ignored
                && (0..2).all(|p| last.ranges[p].end == block.ranges[p].start)
            {
                for p in 0..2 {
                    last.ranges[p].end = block.ranges[p].end;
                }
                last.rows += block.rows;
                continue;
            }
        }
        merged.push(block.clone());
    }
    let blocks: Vec<&Block> = merged.iter().collect();
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

/// Raw UTF-8 patch lines, retaining CRs and BOMs as bytes represented in the patch.
/// Unified patches are line oriented; other encodings and lone-CR terminators cannot
/// safely be written as UTF-8 patch text.
pub fn document_lines(doc: &super::text::TextDoc) -> Result<(Vec<String>, bool), String> {
    raw_lines(doc, None, doc.bom).map(|text| (text.lines, text.final_newline))
}

fn raw_lines(
    doc: &super::text::TextDoc,
    eols: Option<&super::text::TextDoc>,
    bom: bool,
) -> Result<PhysicalText, String> {
    if doc.encoding != super::text::TextEncoding::Utf8 {
        return Err(format!(
            "Cannot export/apply a text patch for {}: {} encoding is not supported; use UTF-8.",
            doc.title(),
            doc.encoding.name()
        ));
    }
    let mut text = String::new();
    if bom {
        text.push('\u{feff}');
    }
    for (i, line) in doc.lines.iter().enumerate() {
        text.push_str(line);
        if let Some(eol) = doc.line_eol(i) {
            let eol = eols.map_or(eol, |source| source.line_eol(i).unwrap_or(source.eol));
            if eol == super::text::Eol::Cr {
                return Err(format!("Cannot export/apply a text patch for {}: lone-CR line endings are not supported; use LF or CRLF.", doc.title()));
            }
            text.push_str(eol.as_str());
        }
    }
    let final_newline = text.is_empty() || text.ends_with('\n');
    Ok(PhysicalText {
        lines: text.split_terminator('\n').map(str::to_string).collect(),
        final_newline,
        bom,
    })
}

/// Bytes used only for emission/application, never for normalization or ignore rules.
struct PhysicalText {
    lines: Vec<String>,
    final_newline: bool,
    bom: bool,
}

struct PatchDocument {
    decoded: super::text::TextDoc,
    physical: PhysicalText,
}

impl PatchDocument {
    fn new(
        doc: &super::text::TextDoc,
        eols: Option<&super::text::TextDoc>,
        bom: bool,
    ) -> Result<Self, String> {
        let physical = raw_lines(doc, eols, bom)?;
        let mut decoded = doc.clone();
        // A BOM-only file occupies a physical line without any decoded characters.
        // Give that line an empty comparison key; filters never see BOM/CRLF bytes.
        decoded.lines.resize(physical.lines.len(), String::new());
        decoded.final_newline = physical.final_newline;
        decoded.bom = bom;
        Ok(Self { decoded, physical })
    }
}

/// A decoded comparison plus independent byte data for the patch writers.
struct PatchPlan {
    old: PatchDocument,
    new: PatchDocument,
    result: DiffResult,
}

impl PatchPlan {
    fn new(
        old: PatchDocument,
        new: PatchDocument,
        opts: &super::diff::DiffOptions,
        syntax: Option<&super::comments::CommentSyntax>,
    ) -> Self {
        let mut result =
            super::diff::compute_with(&[&old.decoded.lines, &new.decoded.lines], opts, syntax);
        super::diff::include_doc_formats(&mut result, &[&old.decoded, &new.decoded], opts);
        let mut plan = Self { old, new, result };
        plan.include_bom_boundary();
        plan
    }

    fn include_bom_boundary(&mut self) {
        let mut forced = vec![false; self.result.rows.len()];
        for (index, row) in self.result.rows.iter().enumerate() {
            let (Some(a), Some(b)) = (row.line[0], row.line[1]) else {
                continue;
            };
            let old_prefix = a == 0 && self.old.physical.bom;
            let new_prefix = b == 0 && self.new.physical.bom;
            if old_prefix == new_prefix {
                continue;
            }
            // A decoded context line moved across the physical BOM boundary. It must
            // be replaced, but ignored text changes on that line must remain ignored.
            if row.block.is_none_or(|i| self.result.blocks[i].ignored) {
                let old = &self.old.physical.lines[a];
                let body = if old_prefix {
                    old.strip_prefix('\u{feff}').unwrap()
                } else {
                    old.as_str()
                };
                self.new.physical.lines[b] =
                    format!("{}{body}", if new_prefix { "\u{feff}" } else { "" });
            }
            forced[index] = true;
        }
        if !forced.contains(&true) {
            return;
        }
        let labels: Vec<_> = self
            .result
            .rows
            .iter()
            .zip(forced)
            .map(|(row, force)| {
                if force {
                    Some((None, false))
                } else {
                    row.block.map(|i| (Some(i), self.result.blocks[i].ignored))
                }
            })
            .collect();
        self.result.blocks.clear();
        let mut positions = [0usize; 3];
        let mut previous = None;
        for (index, (row, label)) in self.result.rows.iter_mut().zip(labels).enumerate() {
            let start = positions;
            for (p, line) in row.line.iter().enumerate() {
                if line.is_some() {
                    positions[p] += 1;
                }
            }
            row.block = label.map(|(_, ignored)| {
                if previous != Some(label) {
                    self.result.blocks.push(Block {
                        ranges: std::array::from_fn(|p| start[p]..positions[p]),
                        kind: super::diff::BlockKind::Diff,
                        ignored,
                        first_row: index,
                        rows: 1,
                    });
                } else {
                    let last = self.result.blocks.last_mut().unwrap();
                    last.rows += 1;
                    for (p, range) in last.ranges.iter_mut().enumerate() {
                        range.end = positions[p];
                    }
                }
                self.result.blocks.len() - 1
            });
            previous = Some(label);
        }
    }

    fn emit(&self, names: &PatchInput, style: PatchStyle, context: usize) -> String {
        let input = PatchInput {
            a: &self.old.physical.lines,
            b: &self.new.physical.lines,
            final_newline_a: self.old.physical.final_newline,
            final_newline_b: self.new.physical.final_newline,
            ..*names
        };
        make_patch(&input, &self.result, style, context)
    }
}

/// Produces a byte-faithful patch subject to decoded-text comparison options.
/// Unsupported byte formats return an error before an output file is created.
pub fn make_doc_patch(
    input: &PatchInput,
    docs: &[&super::text::TextDoc],
    opts: &super::diff::DiffOptions,
    style: PatchStyle,
    context: usize,
) -> Result<String, String> {
    let syntax = docs
        .iter()
        .find_map(|d| d.path.as_deref().and_then(super::comments::for_path));
    let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
    let mut visible = super::diff::compute_with(&refs, opts, syntax);
    super::diff::include_doc_formats(&mut visible, docs, opts);
    if visible.is_identical() {
        return Ok(String::new());
    }
    let old = PatchDocument::new(docs[0], None, docs[0].bom)?;
    let new = PatchDocument::new(
        docs[1],
        opts.ignore_eol.then_some(docs[0]),
        if opts.ignore_codepage {
            docs[0].bom
        } else {
            docs[1].bom
        },
    )?;
    Ok(PatchPlan::new(old, new, opts, syntax).emit(input, style, context))
}

/// Patch of all differing or unique text files of a 2-way folder compare.
pub fn folder_patch(
    roots: &[std::path::PathBuf],
    display_roots: &[String],
    items: &[super::folder::DirItem],
    opts: &super::diff::DiffOptions,
    style: PatchStyle,
    context: usize,
) -> Result<String, String> {
    use super::folder::CmpStatus;
    let mut out = String::new();
    for it in items
        .iter()
        .filter(|i| !i.is_dir && matches!(i.status, CmpStatus::Different | CmpStatus::Unique))
    {
        if it.binary {
            continue;
        }
        let load = |side: usize| -> Result<super::text::TextDoc, String> {
            if !it.exists[side] {
                return Ok(super::text::TextDoc::empty(None));
            }
            super::text::load_doc(&it.path(&roots[side]), None).map_err(|e| e.to_string())
        };
        let (a, b) = (load(0)?, load(1)?);
        let input = PatchInput {
            name_a: &format!("{}/{}", display_roots[0].replace('\\', "/"), it.rel_path()),
            name_b: &format!("{}/{}", display_roots[1].replace('\\', "/"), it.rel_path()),
            date_a: &super::report::fmt_time(it.mtime[0]),
            date_b: &super::report::fmt_time(it.mtime[1]),
            a: &a.lines,
            b: &b.lines,
            final_newline_a: a.final_newline,
            final_newline_b: b.final_newline,
        };
        out.push_str(&make_doc_patch(&input, &[&a, &b], opts, style, context)?);
    }
    Ok(out)
}

fn write_lines(
    out: &mut String,
    prefix: &str,
    lines: &[String],
    range: Range<usize>,
    final_newline: bool,
) {
    for i in range {
        let _ = writeln!(out, "{prefix}{}", lines[i]);
        if i + 1 == lines.len() && !final_newline {
            out.push_str("\\ No newline at end of file\n");
        }
    }
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
        write_lines(&mut out, "< ", input.a, ra.clone(), input.final_newline_a);
        if cmd == 'c' {
            out.push_str("---\n");
        }
        write_lines(&mut out, "> ", input.b, rb.clone(), input.final_newline_b);
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
            write_lines(
                &mut out,
                " ",
                input.a,
                pos..b.ranges[0].start,
                input.final_newline_a,
            );
            write_lines(
                &mut out,
                "-",
                input.a,
                b.ranges[0].clone(),
                input.final_newline_a,
            );
            write_lines(
                &mut out,
                "+",
                input.b,
                b.ranges[1].clone(),
                input.final_newline_b,
            );
            pos = b.ranges[0].end;
        }
        write_lines(&mut out, " ", input.a, pos..ra.end, input.final_newline_a);
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
                write_lines(
                    &mut out,
                    "  ",
                    input.a,
                    pos..b.ranges[0].start,
                    input.final_newline_a,
                );
                let mark = if b.ranges[1].is_empty() { '-' } else { '!' };
                write_lines(
                    &mut out,
                    &format!("{mark} "),
                    input.a,
                    b.ranges[0].clone(),
                    input.final_newline_a,
                );
                pos = b.ranges[0].end;
            }
            write_lines(&mut out, "  ", input.a, pos..ra.end, input.final_newline_a);
        }
        let _ = writeln!(out, "--- {} ----", context_range(&rb));
        if h.iter().any(|b| !b.ranges[1].is_empty()) {
            let mut pos = rb.start;
            for b in &h {
                write_lines(
                    &mut out,
                    "  ",
                    input.b,
                    pos..b.ranges[1].start,
                    input.final_newline_b,
                );
                let mark = if b.ranges[0].is_empty() { '+' } else { '!' };
                write_lines(
                    &mut out,
                    &format!("{mark} "),
                    input.b,
                    b.ranges[1].clone(),
                    input.final_newline_b,
                );
                pos = b.ranges[1].end;
            }
            write_lines(&mut out, "  ", input.b, pos..rb.end, input.final_newline_b);
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
    pub old_no_newline: bool,
    pub new_no_newline: bool,
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
    // A CR at the end of a hunk body is file data (a CRLF terminator), not patch framing.
    let lines: Vec<&str> = text.split_terminator('\n').collect();
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
                old_no_newline: false,
                new_no_newline: false,
            };
            i += 1;
            while i < lines.len()
                && (old_left > 0 || new_left > 0 || lines[i] == "\\ No newline at end of file")
            {
                let h = lines[i];
                if h == "\\ No newline at end of file" {
                    match hunk.lines.last() {
                        Some(HunkLine::Context(_)) => {
                            hunk.old_no_newline = true;
                            hunk.new_no_newline = true;
                        }
                        Some(HunkLine::Remove(_)) => hunk.old_no_newline = true,
                        Some(HunkLine::Add(_)) => hunk.new_no_newline = true,
                        None => {
                            return Err(format!("line {}: newline marker without a line", i + 1))
                        }
                    }
                    i += 1;
                    continue;
                }
                let (kind, body) = match h.chars().next() {
                    Some(' ') => (' ', &h[1..]),
                    None => (' ', ""),
                    Some('-') => ('-', &h[1..]),
                    Some('+') => ('+', &h[1..]),
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
    /// EOF state when an applied hunk reaches the final line; otherwise preserve the input.
    pub final_newline: Option<bool>,
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
    let mut final_newline = None;
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
                if after <= last
                    && after >= floor
                    && (!(h.old_no_newline || h.new_no_newline) || after + old.len() == out.len())
                    && matches_at(&out, after, &old, loose)
                {
                    return Some(after);
                }
                let before = expected.checked_sub(d)?;
                (d > 0
                    && before >= floor
                    && (!(h.old_no_newline || h.new_no_newline) || before + old.len() == out.len())
                    && matches_at(&out, before, &old, loose))
                .then_some(before)
            })
        });
        match found {
            Some(found) => {
                let new = h.new_lines();
                let added = new.len();
                if found + old.len() == out.len() {
                    final_newline = Some(!h.new_no_newline);
                }
                out.splice(found..found + old.len(), new);
                // The hunks after it are as far from their stated lines as this one was.
                delta += found as isize - at + added as isize - old.len() as isize;
                floor = found + added;
            }
            None => failed.push(n + 1),
        }
    }
    Applied {
        lines: out,
        final_newline,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{compute, DiffOptions};

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn format_regression_anchored_rules_use_decoded_text() {
        use crate::core::text::{self, TextEncoding};
        let filter = DiffOptions {
            line_filters_enabled: true,
            line_filters: vec!["^ignored".into()],
            ..Default::default()
        };
        let ending_filter = DiffOptions {
            line_filters_enabled: true,
            line_filters: vec!["ignored$".into()],
            ..Default::default()
        };
        let substitution = DiffOptions {
            substitutions_enabled: true,
            substitutions: vec![("^version=.*$".into(), "version".into())],
            ..Default::default()
        };
        let block = DiffOptions {
            block_filters_enabled: true,
            block_filters: vec![("^BEGIN$".into(), "^END$".into())],
            ..Default::default()
        };
        for (first_a, first_b, eol, opts) in [
            ("ignored old", "ignored new", "\n", filter),
            ("old ignored", "new ignored", "\r\n", ending_filter),
            ("version=old", "version=new", "\n", substitution),
            (
                "BEGIN\nold block\nEND",
                "BEGIN\nnew block\nEND",
                "\n",
                block,
            ),
        ] {
            let gap = (0..20)
                .map(|i| format!("same {i}{eol}"))
                .collect::<String>();
            let old = format!("\u{feff}{first_a}{eol}{gap}old{eol}");
            let new = format!("\u{feff}{first_b}{eol}{gap}new{eol}");
            let a = text::doc_from_bytes(None, old.as_bytes(), Some(TextEncoding::Utf8));
            let b = text::doc_from_bytes(None, new.as_bytes(), Some(TextEncoding::Utf8));
            let input = PatchInput {
                name_a: "a/f",
                name_b: "b/f",
                date_a: "",
                date_b: "",
                a: &a.lines,
                b: &b.lines,
                final_newline_a: true,
                final_newline_b: true,
            };
            for context in [0, 3] {
                let patch =
                    make_doc_patch(&input, &[&a, &b], &opts, PatchStyle::Unified, context).unwrap();
                assert_eq!(patch.matches("@@ -").count(), 1, "{patch}");
                let parsed = parse_unified(&patch).unwrap();
                let (raw, nl) = document_lines(&a).unwrap();
                let result = apply(&raw, &parsed[0]);
                assert!(result.failed.is_empty(), "{patch}");
                assert_eq!(
                    text::join_lines(
                        &result.lines,
                        text::Eol::Lf,
                        result.final_newline.unwrap_or(nl)
                    ),
                    format!("\u{feff}{first_a}{eol}{gap}new{eol}"),
                    "{patch}"
                );
            }
        }
    }

    #[test]
    fn patches_round_trip_moving_eof_and_utf8_formats() {
        use crate::core::text::{self, TextDoc};
        let texts = [
            "",
            "foo",
            "foo\n",
            "foo\nbar",
            "foo\nbar\n",
            "foo\nbar\nend",
            "\u{feff}",
            "\u{feff}foo\r\nbar\n",
            "foo\r\nbar\r\n",
        ];
        let opts = DiffOptions {
            ignore_eol: false,
            ignore_codepage: false,
            ..Default::default()
        };
        for old in texts {
            for new in texts {
                let a = text::doc_from_bytes(None, old.as_bytes(), Some(text::TextEncoding::Utf8));
                let b = text::doc_from_bytes(None, new.as_bytes(), Some(text::TextEncoding::Utf8));
                let input = PatchInput {
                    name_a: "a/f",
                    name_b: "b/f",
                    date_a: "",
                    date_b: "",
                    a: &a.lines,
                    b: &b.lines,
                    final_newline_a: a.final_newline,
                    final_newline_b: b.final_newline,
                };
                for context in [0, 1, 3] {
                    let patch =
                        make_doc_patch(&input, &[&a, &b], &opts, PatchStyle::Unified, context)
                            .unwrap();
                    if old == new {
                        assert!(patch.is_empty());
                        continue;
                    }
                    assert!(!patch.is_empty(), "{old:?} -> {new:?}");
                    let parsed = parse_unified(&patch).unwrap();
                    let (lines, nl) = document_lines(&a).unwrap();
                    let result = apply(&lines, &parsed[0]);
                    assert!(result.failed.is_empty(), "{old:?} -> {new:?}: {patch}");
                    assert_eq!(
                        text::join_lines(
                            &result.lines,
                            text::Eol::Lf,
                            result.final_newline.unwrap_or(nl)
                        ),
                        new,
                        "{old:?} -> {new:?}: {patch}"
                    );
                }
            }
        }
        let mut a = TextDoc::from_text(None, "old\n");
        let b = TextDoc::from_text(None, "new\n");
        let empty = Vec::new();
        let input = PatchInput {
            name_a: "a",
            name_b: "b",
            date_a: "",
            date_b: "",
            a: &empty,
            b: &empty,
            final_newline_a: true,
            final_newline_b: true,
        };
        a.encoding = text::TextEncoding::Utf16Le;
        assert!(
            make_doc_patch(&input, &[&a, &b], &opts, PatchStyle::Unified, 3)
                .unwrap_err()
                .contains("UTF-8")
        );
    }

    #[test]
    fn patches_preserve_final_newlines_in_both_directions() {
        for (old, new) in [
            ("old", "new"),
            ("old\n", "new"),
            ("old", "new\n"),
            ("one\n", "one"),
            ("one", "one\n"),
            ("head\nold", "HEAD\nold"),
            ("", "new"),
            ("old", ""),
        ] {
            let a = super::super::text::TextDoc::from_text(None, old);
            let b = super::super::text::TextDoc::from_text(None, new);
            let input = PatchInput {
                name_a: "a/f.txt",
                name_b: "b/f.txt",
                date_a: "",
                date_b: "",
                a: &a.lines,
                b: &b.lines,
                final_newline_a: a.final_newline,
                final_newline_b: b.final_newline,
            };
            let result = compute(&[&a.lines, &b.lines], &DiffOptions::default());
            for context in [0, 3] {
                let text = make_patch(&input, &result, PatchStyle::Unified, context);
                if context > 0 || old != "head\nold" {
                    assert!(
                        text.contains("\\ No newline at end of file"),
                        "{old:?} -> {new:?}: {text}"
                    );
                }
                let files = parse_unified(&text).unwrap();
                let patched = apply(&a.lines, &files[0]);
                assert!(patched.failed.is_empty(), "{text}");
                assert_eq!(
                    super::super::text::join_lines(
                        &patched.lines,
                        super::super::text::Eol::Lf,
                        patched.final_newline.unwrap_or(a.final_newline)
                    ),
                    new,
                    "{text}"
                );
            }
            for style in [PatchStyle::Normal, PatchStyle::Context] {
                if old == "head\nold" && style == PatchStyle::Normal {
                    continue;
                }
                assert!(
                    make_patch(&input, &result, style, 3).contains("\\ No newline at end of file")
                );
            }
        }
    }

    #[test]
    fn a_patch_away_from_eof_preserves_the_original_newline_state() {
        let files = parse_unified("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
        let result = apply(&v(&["old", "tail"]), &files[0]);
        assert!(result.failed.is_empty());
        assert_eq!(result.final_newline, None);
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
            final_newline_a: true,
            final_newline_b: true,
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
            final_newline_a: true,
            final_newline_b: true,
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
            final_newline_a: true,
            final_newline_b: true,
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

#[cfg(test)]
mod format_regression {
    use super::*;
    use crate::core::diff::DiffOptions;
    use crate::core::text::{self, TextEncoding};

    fn patch(old: &str, new: &str, opts: &DiffOptions, context: usize) -> String {
        let a = text::doc_from_bytes(None, old.as_bytes(), Some(TextEncoding::Utf8));
        let b = text::doc_from_bytes(None, new.as_bytes(), Some(TextEncoding::Utf8));
        let input = PatchInput {
            name_a: "a/target.txt",
            name_b: "b/target.txt",
            date_a: "",
            date_b: "",
            a: &a.lines,
            b: &b.lines,
            final_newline_a: a.final_newline,
            final_newline_b: b.final_newline,
        };
        make_doc_patch(&input, &[&a, &b], opts, PatchStyle::Unified, context).unwrap()
    }

    fn apply_bytes(old: &str, patch: &str) -> String {
        if patch.is_empty() {
            return old.to_string();
        }
        let a = text::doc_from_bytes(None, old.as_bytes(), Some(TextEncoding::Utf8));
        let (lines, nl) = document_lines(&a).unwrap();
        let parsed = parse_unified(patch).unwrap();
        let result = apply(&lines, &parsed[0]);
        assert!(result.failed.is_empty(), "old={old:?}, patch={patch:?}");
        text::join_lines(
            &result.lines,
            text::Eol::Lf,
            result.final_newline.unwrap_or(nl),
        )
    }

    #[test]
    fn byte_round_trip_matrix() {
        let shapes: &[&[&str]] = &[&[], &["a"], &["a", "b"], &["x", "a", "b"], &["a", "x", "b"]];
        let mut cases = Vec::new();
        for lines in shapes {
            for bom in [false, true] {
                for mode in 0..3 {
                    for newline in [false, true] {
                        let mut text = if bom {
                            "\u{feff}".to_string()
                        } else {
                            String::new()
                        };
                        for (i, line) in lines.iter().enumerate() {
                            text.push_str(line);
                            if i + 1 < lines.len() || newline {
                                text.push_str(match mode {
                                    0 => "\n",
                                    1 => "\r\n",
                                    _ if i % 2 == 0 => "\r\n",
                                    _ => "\n",
                                });
                            }
                        }
                        cases.push(text);
                    }
                }
            }
        }
        cases.sort();
        cases.dedup();
        let opts = DiffOptions {
            ignore_eol: false,
            ignore_codepage: false,
            ..Default::default()
        };
        for old in &cases {
            for new in &cases {
                for context in [0, 1, 3] {
                    let p = patch(old, new, &opts, context);
                    assert_eq!(
                        apply_bytes(old, &p),
                        *new,
                        "old={old:?}, target={new:?}, context={context}, patch={p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn moving_bom_boundary_preserves_ignored_text() {
        let opts = DiffOptions {
            substitutions_enabled: true,
            substitutions: vec![("^version=.*$".into(), "version".into())],
            ..Default::default()
        };
        for (old, new, expected) in [
            (
                "\u{feff}version=old\na\nold\n",
                "\u{feff}x\nversion=new\na\nnew\n",
                "\u{feff}x\nversion=old\na\nnew\n",
            ),
            (
                "\u{feff}x\nversion=old\na\nold\n",
                "\u{feff}version=new\na\nnew\n",
                "\u{feff}version=old\na\nnew\n",
            ),
        ] {
            for context in [0, 3] {
                let p = patch(old, new, &opts, context);
                assert_eq!(apply_bytes(old, &p), expected, "patch={p:?}");
            }
        }
    }

    #[test]
    fn ignored_differences_outside_context_do_not_leak() {
        let gap = (0..20).map(|i| format!("same {i}\n")).collect::<String>();
        for opts in [
            DiffOptions {
                ignore_case: true,
                ..Default::default()
            },
            DiffOptions {
                line_filters_enabled: true,
                line_filters: vec!["^keep".into()],
                ..Default::default()
            },
            DiffOptions {
                substitutions_enabled: true,
                substitutions: vec![("^keep .*".into(), "keep".into())],
                ..Default::default()
            },
        ] {
            let old = format!("\u{feff}keep OLD\n{gap}old\n");
            let new = format!("\u{feff}keep old\n{gap}changed\n");
            let expected = format!("\u{feff}keep OLD\n{gap}changed\n");
            let p = patch(&old, &new, &opts, 3);
            assert_eq!(apply_bytes(&old, &p), expected, "patch={p:?}");
        }
    }
}
