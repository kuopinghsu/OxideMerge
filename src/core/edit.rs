//! Edits of a rectangular (column) selection of text lines.
//!
//! A column selection covers the same columns of several consecutive lines. Columns are counted
//! as the lines are shown: a tab reaches the next tab stop and every other character takes one
//! column. A line shorter than the selection has nothing selected past its end; text put into
//! it is padded with spaces up to the first column, so that it lines up with the others.
//!
//! The caret functions edit text at a position (line, byte) the way a text editor does: an
//! insertion may hold newlines, and a removal may join lines.
//!
//! Every function here works on whole lines and changes nothing else: line endings, encoding
//! and the byte order mark belong to the document and are kept by it.

use std::ops::Range;

/// The byte index in `line` where display column `col` starts, and how many columns the line
/// falls short of it (0 when the column is inside the line). A column inside a tab is taken to
/// start at the tab.
pub fn byte_at(line: &str, col: usize, tab: usize) -> (usize, usize) {
    let tab = tab.max(1);
    let mut c = 0;
    for (i, ch) in line.char_indices() {
        let w = if ch == '\t' { tab - c % tab } else { 1 };
        if col < c + w {
            return (i, 0);
        }
        c += w;
    }
    (line.len(), col.saturating_sub(c))
}

/// The display column where byte `at` of `line` starts.
pub fn col_at(line: &str, at: usize, tab: usize) -> usize {
    let tab = tab.max(1);
    let mut c = 0;
    for (i, ch) in line.char_indices() {
        if i >= at {
            break;
        }
        c += if ch == '\t' { tab - c % tab } else { 1 };
    }
    c
}

/// The bytes of `line` in display columns `cols`.
pub fn span(line: &str, cols: &Range<usize>, tab: usize) -> Range<usize> {
    let (a, _) = byte_at(line, cols.start, tab);
    let (b, _) = byte_at(line, cols.end.max(cols.start), tab);
    a..b
}

/// The text of each line in display columns `cols`.
pub fn take(lines: &[&str], cols: &Range<usize>, tab: usize) -> Vec<String> {
    lines
        .iter()
        .map(|l| l[span(l, cols, tab)].to_string())
        .collect()
}

/// Replaces display columns `cols` of `line` with `with`, padding a short line with spaces up
/// to the first column when `with` is not empty.
fn put(line: &mut String, cols: &Range<usize>, with: &str, tab: usize) {
    let (a, short) = byte_at(line, cols.start, tab);
    let (b, _) = byte_at(line, cols.end.max(cols.start), tab);
    if short > 0 {
        if !with.is_empty() {
            line.extend(std::iter::repeat_n(' ', short));
            line.push_str(with);
        }
        return;
    }
    line.replace_range(a..b, with);
}

/// Replaces columns `cols` of `lines[first..first + count]` with the lines of `with`, and
/// returns how many lines were changed or added.
///
/// - One line of `with` goes into every selected line, as typing does.
/// - Otherwise line `i` of `with` goes into line `first + i`. Pasted lines past the selection
///   go into the lines below it, at the same column; past the end of the document they are
///   added as new lines. Selected lines past the pasted ones only lose their selected text.
/// - An empty `with` deletes the columns.
pub fn replace(
    lines: &mut Vec<String>,
    first: usize,
    count: usize,
    cols: &Range<usize>,
    with: &[String],
    tab: usize,
) -> usize {
    let first = first.min(lines.len());
    let count = count.min(lines.len() - first);
    let one = (with.len() == 1).then(|| with[0].as_str());
    let reach = count.max(if one.is_some() { 0 } else { with.len() });
    for i in 0..reach {
        let text = one.unwrap_or_else(|| with.get(i).map_or("", String::as_str));
        let at = first + i;
        if at >= lines.len() {
            lines.push(String::new());
        }
        // Below the selection only the pasted text goes in; nothing there is selected.
        let cols = if i < count {
            cols.clone()
        } else {
            cols.start..cols.start
        };
        put(&mut lines[at], &cols, text, tab);
    }
    reach
}

/// A place in a document: a line and a byte offset in it, on a character boundary.
pub type Pos = (usize, usize);

/// `pos` moved to the nearest place that exists in `lines`.
pub fn clamp(lines: &[String], pos: Pos) -> Pos {
    let Some(last) = lines.len().checked_sub(1) else {
        return (0, 0);
    };
    let line = pos.0.min(last);
    let text = &lines[line];
    let mut at = pos.1.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    (line, at)
}

/// The text from `a` to `b` (in either order), its lines joined by `eol`.
pub fn text_between(lines: &[String], a: Pos, b: Pos, eol: &str) -> String {
    let (a, b) = (clamp(lines, a.min(b)), clamp(lines, a.max(b)));
    if lines.is_empty() {
        return String::new();
    }
    if a.0 == b.0 {
        return lines[a.0][a.1..b.1].to_string();
    }
    let mut out = lines[a.0][a.1..].to_string();
    for l in &lines[a.0 + 1..b.0] {
        out.push_str(eol);
        out.push_str(l);
    }
    out.push_str(eol);
    out.push_str(&lines[b.0][..b.1]);
    out
}

/// Removes the text from `a` to `b` (in either order), joining their lines, and returns where
/// it was.
pub fn remove(lines: &mut Vec<String>, a: Pos, b: Pos) -> Pos {
    let (a, b) = (clamp(lines, a.min(b)), clamp(lines, a.max(b)));
    if lines.is_empty() || a == b {
        return a;
    }
    let tail = lines[b.0][b.1..].to_string();
    lines[a.0].truncate(a.1);
    lines[a.0].push_str(&tail);
    lines.drain(a.0 + 1..=b.0);
    a
}

/// Inserts `text` at `at`; its newlines (`\n`, `\r\n` or `\r`) split the line. Returns the
/// place after the inserted text.
pub fn insert(lines: &mut Vec<String>, at: Pos, text: &str) -> Pos {
    if lines.is_empty() {
        lines.push(String::new());
    }
    let at = clamp(lines, at);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parts = text.split('\n');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let line = &mut lines[at.0];
    if rest.is_empty() {
        line.insert_str(at.1, first);
        return (at.0, at.1 + first.len());
    }
    let tail = line.split_off(at.1);
    line.push_str(first);
    let last = rest.len() - 1;
    let end = (at.0 + rest.len(), rest[last].len());
    let mut new: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
    new[last].push_str(&tail);
    lines.splice(at.0 + 1..at.0 + 1, new);
    end
}

/// The place one character before `pos`, going to the end of the line above from a line start.
pub fn before(lines: &[String], pos: Pos) -> Pos {
    let (l, b) = clamp(lines, pos);
    match lines.get(l).and_then(|t| t[..b].chars().next_back()) {
        Some(c) => (l, b - c.len_utf8()),
        None if l > 0 => (l - 1, lines[l - 1].len()),
        None => (l, b),
    }
}

/// The place one character after `pos`, going to the start of the line below from a line end.
pub fn after(lines: &[String], pos: Pos) -> Pos {
    let (l, b) = clamp(lines, pos);
    match lines.get(l).and_then(|t| t[b..].chars().next()) {
        Some(c) => (l, b + c.len_utf8()),
        None if l + 1 < lines.len() => (l + 1, 0),
        None => (l, b),
    }
}

/// The word (letters, digits and `_`) or other run of characters around byte `at` of `line`.
pub fn word_at(line: &str, at: usize) -> Range<usize> {
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let Some(k) = line[at.min(line.len())..]
        .chars()
        .next()
        .or_else(|| line[..at.min(line.len())].chars().next_back())
        .map(kind)
    else {
        return at..at;
    };
    let at = at.min(line.len());
    let start = line[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| kind(c) == k)
        .last()
        .map_or(at, |(i, _)| i);
    let end = line[at..]
        .char_indices()
        .find(|&(_, c)| kind(c) != k)
        .map_or(line.len(), |(i, _)| at + i);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn columns_follow_tabs_and_line_ends() {
        assert_eq!(byte_at("ab\tc", 0, 4), (0, 0));
        assert_eq!(byte_at("ab\tc", 3, 4), (2, 0), "inside the tab");
        assert_eq!(byte_at("ab\tc", 4, 4), (3, 0));
        assert_eq!(byte_at("ab\tc", 7, 4), (4, 2), "past the end");
        assert_eq!(col_at("ab\tc", 3, 4), 4);
        assert_eq!(byte_at("中文x", 1, 4), ("中".len(), 0));
        assert_eq!(
            take(&["abcdef", "ab", "a\tbcd"], &(1..5), 4),
            ["bcde", "b", "\tb"]
        );
    }

    #[test]
    fn typing_goes_into_every_line_and_pads_short_ones() {
        let mut lines = v(&["let a = 1;", "x", "let c = 3;"]);
        let n = replace(&mut lines, 0, 3, &(4..5), &v(&["z"]), 4);
        assert_eq!(n, 3);
        assert_eq!(lines, ["let z = 1;", "x   z", "let z = 3;"]);
    }

    #[test]
    fn a_pasted_block_goes_line_by_line_and_past_the_selection() {
        let mut lines = v(&["0123", "abcd", "wxyz"]);
        replace(&mut lines, 0, 2, &(1..3), &v(&["A", "B", "C", "D"]), 4);
        assert_eq!(lines, ["0A3", "aBd", "wCxyz", " D"]);
        // Fewer pasted lines than selected: the others only lose their selection.
        let mut lines = v(&["0123", "abcd", "wxyz"]);
        replace(&mut lines, 0, 3, &(1..3), &v(&["A", "B"]), 4);
        assert_eq!(lines, ["0A3", "aBd", "wz"]);
    }

    #[test]
    fn deleting_columns_leaves_short_lines_alone() {
        let mut lines = v(&["abcdef", "ab", "abcdef"]);
        replace(&mut lines, 0, 3, &(2..4), &[], 4);
        assert_eq!(lines, ["abef", "ab", "abef"]);
    }

    #[test]
    fn the_caret_inserts_and_removes_across_lines() {
        let mut lines = v(&["abc", "def"]);
        assert_eq!(insert(&mut lines, (0, 1), "X\nY\r\nZ"), (2, 1));
        assert_eq!(lines, ["aX", "Y", "Zbc", "def"]);
        assert_eq!(text_between(&lines, (2, 1), (0, 1), "\n"), "X\nY\nZ");
        assert_eq!(remove(&mut lines, (2, 1), (0, 1)), (0, 1));
        assert_eq!(lines, ["abc", "def"]);
        // Enter at a line end, then Backspace at the start of the new line joins them.
        let end = insert(&mut lines, (0, 3), "\n");
        assert_eq!((end, lines.clone()), ((1, 0), v(&["abc", "", "def"])));
        let back = before(&lines, end);
        assert_eq!(remove(&mut lines, back, end), (0, 3));
        assert_eq!(lines, ["abc", "def"]);
        assert_eq!(after(&lines, (0, 3)), (1, 0));
        assert_eq!(before(&lines, (0, 0)), (0, 0));
        // Multi-byte characters move as one.
        let wide = v(&["中文"]);
        assert_eq!(after(&wide, (0, 0)), (0, 3));
        assert_eq!(clamp(&wide, (5, 4)), (0, 3));
        let mut empty = Vec::new();
        assert_eq!(insert(&mut empty, (0, 0), "hi"), (0, 2));
        assert_eq!(empty, ["hi"]);
    }

    #[test]
    fn words_are_found_around_the_caret() {
        assert_eq!(word_at("let foo_1 = 2;", 5), 4..9);
        assert_eq!(word_at("let foo_1 = 2;", 9), 9..10);
        assert_eq!(word_at("abc", 3), 0..3);
        assert_eq!(word_at("", 0), 0..0);
    }
}
