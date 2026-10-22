//! Patch file generation in normal, context and unified formats.

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

pub fn make_patch(input: &PatchInput, result: &DiffResult, style: PatchStyle, context: usize) -> String {
    let blocks: Vec<&Block> = result.blocks.iter().collect();
    if blocks.is_empty() {
        return String::new();
    }
    match style {
        PatchStyle::Normal => normal(input, &blocks),
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
    for it in items.iter().filter(|i| !i.is_dir && matches!(i.status, CmpStatus::Different | CmpStatus::Unique)) {
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
        let (Some(a), Some(b)) = (load(0), load(1)) else { continue };
        let r = super::diff::compute(&[&a.0, &b.0], opts);
        if r.blocks.is_empty() {
            continue;
        }
        let input = PatchInput { name_a: &a.1, name_b: &b.1, date_a: &a.2, date_b: &b.2, a: &a.0, b: &b.0 };
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

/// Groups blocks into hunks whose context regions overlap.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{compute, DiffOptions};

    #[test]
    fn unified_patch() {
        let a: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["a", "x", "c"].iter().map(|s| s.to_string()).collect();
        let r = compute(&[&a, &b], &DiffOptions::default());
        let input = PatchInput { name_a: "a.txt", name_b: "b.txt", date_a: "", date_b: "", a: &a, b: &b };
        let p = make_patch(&input, &r, PatchStyle::Unified, 3);
        assert!(p.contains("@@ -1,3 +1,3 @@"));
        assert!(p.contains("-b\n+x\n"));
        let n = make_patch(&input, &r, PatchStyle::Normal, 3);
        assert!(n.starts_with("2c2\n< b\n---\n> x\n"));
    }
}
