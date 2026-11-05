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

/// A place in a document: a line and a byte offset in it, between two user-perceived characters
/// (grapheme clusters, UAX #29), so that an accent, an emoji sequence or a Hangul syllable written
/// as jamo is never split.
pub type Pos = (usize, usize);

/// `pos` moved to the nearest place that exists in `lines`: on the line, and at the start of the
/// character it falls in.
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
    (line, grapheme_floor(text, at))
}

/// The start of the grapheme cluster of `text` that byte `at` (a char boundary) is in.
fn grapheme_floor(text: &str, at: usize) -> usize {
    use unicode_segmentation::GraphemeCursor;
    let mut c = GraphemeCursor::new(at, text.len(), true);
    match c.is_boundary(text, 0) {
        Ok(true) => at,
        _ => c.prev_boundary(text, 0).ok().flatten().unwrap_or(0),
    }
}

/// The end of the grapheme cluster of `text` that starts at byte `at`.
fn grapheme_next(text: &str, at: usize) -> Option<usize> {
    use unicode_segmentation::GraphemeCursor;
    GraphemeCursor::new(at, text.len(), true)
        .next_boundary(text, 0)
        .ok()
        .flatten()
}

/// The start of the character (grapheme cluster) of `text` that ends at byte `at`, or 0.
pub fn char_before(text: &str, at: usize) -> usize {
    grapheme_prev(text, at).unwrap_or(0)
}

/// The start of the grapheme cluster of `text` that ends at byte `at`.
fn grapheme_prev(text: &str, at: usize) -> Option<usize> {
    use unicode_segmentation::GraphemeCursor;
    GraphemeCursor::new(at, text.len(), true)
        .prev_boundary(text, 0)
        .ok()
        .flatten()
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
    match lines
        .get(l)
        .filter(|_| b > 0)
        .and_then(|t| grapheme_prev(t, b))
    {
        Some(a) => (l, a),
        None if l > 0 => (l - 1, lines[l - 1].len()),
        None => (l, b),
    }
}

/// The place one character after `pos`, going to the start of the line below from a line end.
pub fn after(lines: &[String], pos: Pos) -> Pos {
    let (l, b) = clamp(lines, pos);
    match lines
        .get(l)
        .filter(|t| b < t.len())
        .and_then(|t| grapheme_next(t, b))
    {
        Some(c) => (l, c),
        None if l + 1 < lines.len() => (l + 1, 0),
        None => (l, b),
    }
}

/// Where a word move left from `pos` lands: the start of the word before it, skipping spaces.
/// Words follow Unicode word boundaries (UAX #29), so combining marks and emoji sequences stay
/// whole and each CJK character is its own word; letters, digits and `_` join (`snake_case` and
/// `camelCase` are one word), and a run of punctuation is a word of its own (`x.len` is three).
/// From a line start it goes to the end of the line above.
pub fn word_left(lines: &[String], pos: Pos) -> Pos {
    let (l, b) = clamp(lines, pos);
    let Some(line) = lines.get(l) else {
        return (l, b);
    };
    if b == 0 {
        return before(lines, (l, b));
    }
    let start = word_starts(line)
        .filter(|&(i, blank)| i < b && !blank)
        .last()
        .map_or(0, |(i, _)| i);
    (l, start)
}

/// Where a word move right from `pos` lands: the end of the word after it, skipping spaces (see
/// [`word_left`]). From a line end it goes to the start of the line below.
pub fn word_right(lines: &[String], pos: Pos) -> Pos {
    let (l, b) = clamp(lines, pos);
    let Some(line) = lines.get(l) else {
        return (l, b);
    };
    if b == line.len() {
        return after(lines, (l, b));
    }
    let end = word_spans(line)
        .find(|&(_, end, blank)| end > b && !blank)
        .map_or(line.len(), |(_, end, _)| end);
    (l, end)
}

/// The words of `line` as (start, end, whether it is only spaces), joining runs that word
/// boundaries split but that read as one word: a run of punctuation, and of spaces. Letters,
/// digits and `_` form words (UAX #29 keeps `a_b` and `x1` whole).
fn word_spans(line: &str) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
    use unicode_segmentation::UnicodeSegmentation;
    let kind = |s: &str| {
        if s.chars().all(char::is_whitespace) {
            0
        } else if s.chars().any(|c| c.is_alphanumeric() || c == '_') {
            1
        } else {
            2
        }
    };
    // UAX #29 keeps `x.len` and `it's` whole; in code, ASCII punctuation other than `_` is a
    // word of its own
    let split = |c: char| c.is_ascii_punctuation() && c != '_';
    let segments = line.split_word_bound_indices().flat_map(|(i, seg)| {
        let mut parts = Vec::new();
        let mut from = 0;
        for (j, c) in seg.char_indices() {
            if split(c) {
                parts.extend([(from, j), (j, j + c.len_utf8())]);
                from = j + c.len_utf8();
            }
        }
        parts.push((from, seg.len()));
        parts
            .into_iter()
            .filter(|(a, b)| a < b)
            .map(move |(a, b)| (i + a, &seg[a..b]))
    });
    let mut spans: Vec<(usize, usize, u8)> = Vec::new();
    for (i, seg) in segments {
        let k = kind(seg);
        let end = i + seg.len();
        match spans.last_mut() {
            // spaces and punctuation runs join; words stay apart (two CJK characters are two
            // words)
            Some(last) if last.2 == k && k != 1 => last.1 = end,
            _ => spans.push((i, end, k)),
        }
    }
    spans.into_iter().map(|(a, b, k)| (a, b, k == 0))
}

fn word_starts(line: &str) -> impl Iterator<Item = (usize, bool)> + '_ {
    word_spans(line).map(|(a, _, blank)| (a, blank))
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

/// Puts a copy of lines `first..=last` right after them. Returns how many lines were copied
/// (0 when the range is not in `lines`).
pub fn duplicate_lines(lines: &mut Vec<String>, first: usize, last: usize) -> usize {
    if first > last || last >= lines.len() {
        return 0;
    }
    let copy: Vec<String> = lines[first..=last].to_vec();
    let n = copy.len();
    lines.splice(last + 1..last + 1, copy);
    n
}

/// Moves lines `first..=last` one line up (`up`) or down, past the line next to them. Returns
/// false, changing nothing, at the top or bottom of `lines`.
pub fn move_lines(lines: &mut [String], first: usize, last: usize, up: bool) -> bool {
    if first > last || last >= lines.len() {
        return false;
    }
    if up && first > 0 {
        lines[first - 1..=last].rotate_left(1);
        true
    } else if !up && last + 1 < lines.len() {
        lines[first..=last + 1].rotate_right(1);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicating_and_moving_lines_keeps_every_line_once() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut l = v(&["a", "b", "c", "d"]);
        assert_eq!(duplicate_lines(&mut l, 1, 2), 2);
        assert_eq!(l, v(&["a", "b", "c", "b", "c", "d"]));
        assert_eq!(duplicate_lines(&mut l, 5, 5), 1);
        assert_eq!(l, v(&["a", "b", "c", "b", "c", "d", "d"]));
        assert_eq!(duplicate_lines(&mut l, 7, 7), 0);
        assert_eq!(duplicate_lines(&mut l, 2, 1), 0);

        let mut l = v(&["a", "b", "c", "d"]);
        assert!(move_lines(&mut l, 1, 2, true));
        assert_eq!(l, v(&["b", "c", "a", "d"]));
        assert!(!move_lines(&mut l, 0, 1, true), "already at the top");
        assert!(move_lines(&mut l, 0, 1, false));
        assert_eq!(l, v(&["a", "b", "c", "d"]));
        assert!(move_lines(&mut l, 2, 3, true));
        assert_eq!(l, v(&["a", "c", "d", "b"]));
        assert!(!move_lines(&mut l, 1, 3, false), "already at the bottom");
        assert!(!move_lines(&mut l, 3, 4, false), "past the end");
        assert_eq!(l, v(&["a", "c", "d", "b"]));
    }

    /// Every stop of repeated word moves from `from`, as (line, byte).
    fn stops(lines: &[&str], from: Pos, right: bool) -> Vec<Pos> {
        let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        let mut at = from;
        loop {
            let next = if right {
                word_right(&lines, at)
            } else {
                word_left(&lines, at)
            };
            if next == at {
                return out;
            }
            assert!(
                lines[next.0].is_char_boundary(next.1),
                "{next:?} splits a character"
            );
            out.push(next);
            at = next;
        }
    }

    #[test]
    fn word_moves_stop_at_word_ends_and_starts_and_cross_lines() {
        let lines = ["let foo_bar = x.len();", "", "  camelCase  42"];
        assert_eq!(
            stops(&lines, (0, 0), true),
            vec![
                (0, 3),  // let
                (0, 11), // foo_bar
                (0, 13), // =
                (0, 15), // x
                (0, 16), // .
                (0, 19), // len
                (0, 22), // ();
                (1, 0),  // the blank line
                (2, 0),
                (2, 11), // camelCase is one word
                (2, 15), // 42
            ]
        );
        assert_eq!(
            stops(&lines, (2, 15), false),
            vec![
                (2, 13),
                (2, 2),
                (2, 0),
                (1, 0),
                (0, 22),
                (0, 19),
                (0, 16),
                (0, 15),
                (0, 14),
                (0, 12),
                (0, 4),
                (0, 0),
            ]
        );
        // From inside a word: to its own end or start.
        let one = vec!["hello world".to_string()];
        assert_eq!(word_right(&one, (0, 2)), (0, 5));
        assert_eq!(word_left(&one, (0, 8)), (0, 6));
        // Out of range positions are clamped first.
        assert_eq!(word_right(&one, (5, 99)), (0, 11));
    }

    #[test]
    fn word_moves_keep_characters_whole() {
        // CJK characters are words of their own; a combining accent and an emoji sequence stay
        // with what they mark.
        let s = "中文 cafe\u{301} 👩\u{200d}💻 x".to_string();
        let v = vec![s.clone()];
        let mut at = (0, 0);
        let mut ends = Vec::new();
        while at.1 < s.len() {
            at = word_right(&v, at);
            ends.push(&s[..at.1]);
        }
        assert_eq!(
            ends,
            vec![
                "中",
                "中文",
                "中文 cafe\u{301}",
                "中文 cafe\u{301} 👩\u{200d}💻",
                "中文 cafe\u{301} 👩\u{200d}💻 x",
            ]
        );
        assert_eq!(word_left(&v, (0, s.len())), (0, s.len() - 1));
    }

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
        // So do characters made of several code points: an accent, a flag, an emoji sequence,
        // a Hangul syllable written as jamo.
        let marks = v(&[
            "e\u{301}x",
            "🇹🇼!",
            "👩\u{200d}💻.",
            "\u{1100}\u{1161}\u{11a8}.",
        ]);
        let ends = [3, 8, 11, 9];
        for (l, &end) in ends.iter().enumerate() {
            assert_eq!(after(&marks, (l, 0)), (l, end), "line {l}");
            assert_eq!(before(&marks, (l, end)), (l, 0), "line {l}");
            for b in 1..end {
                if marks[l].is_char_boundary(b) {
                    assert_eq!(clamp(&marks, (l, b)), (l, 0), "line {l} byte {b}");
                }
            }
            assert_eq!(clamp(&marks, (l, end)), (l, end));
        }
        let mut accent = v(&["ae\u{301}"]);
        let from = before(&accent, (0, 4));
        assert_eq!(remove(&mut accent, from, (0, 4)), (0, 1));
        assert_eq!(accent, ["a"], "Backspace takes the accent with its letter");
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
