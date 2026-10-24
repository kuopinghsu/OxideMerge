//! Byte-wise (hex) comparison.

use std::ops::Range;

/// Returns contiguous ranges of offsets where the buffers differ (including length differences).
pub fn diff_ranges(data: &[&[u8]]) -> Vec<Range<usize>> {
    let max = data.iter().map(|d| d.len()).max().unwrap_or(0);
    let min = data.iter().map(|d| d.len()).min().unwrap_or(0);
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut push = |o: usize| match out.last_mut() {
        Some(r) if r.end == o => r.end = o + 1,
        _ => out.push(o..o + 1),
    };
    const CHUNK: usize = 256;
    let mut off = 0;
    while off < min {
        let end = (off + CHUNK).min(min);
        if data.iter().all(|d| d[off..end] == data[0][off..end]) {
            off = end;
            continue;
        }
        for o in off..end {
            if data.iter().any(|d| d[o] != data[0][o]) {
                push(o);
            }
        }
        off = end;
    }
    if max > min {
        match out.last_mut() {
            Some(r) if r.end == min => r.end = max,
            _ => out.push(min..max),
        }
    }
    out
}

pub fn byte_differs(data: &[&[u8]], off: usize) -> bool {
    let first = data[0].get(off);
    data.iter().any(|d| d.get(off) != first)
}

/// What to search for: hex bytes such as `DE AD BE EF` or `0xDEAD`, or else the text itself;
/// text in double quotes is always searched as text.
pub fn parse_pattern(query: &str) -> Option<Vec<u8>> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Some(t) = q.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return (!t.is_empty()).then(|| t.as_bytes().to_vec());
    }
    let digits: String = q
        .split_whitespace()
        .map(|w| w.strip_prefix("0x").or(w.strip_prefix("0X")).unwrap_or(w))
        .collect();
    let hex = digits.len().is_multiple_of(2) && digits.bytes().all(|b| b.is_ascii_hexdigit());
    if hex {
        (0..digits.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
            .collect()
    } else {
        Some(q.as_bytes().to_vec())
    }
}

/// An offset typed as decimal, or as hex with a `0x` prefix, an `h` suffix or hex letters.
pub fn parse_offset(text: &str) -> Option<usize> {
    let t = text.trim().replace(['_', ' '], "");
    if let Some(h) = t
        .strip_prefix("0x")
        .or(t.strip_prefix("0X"))
        .or(t.strip_suffix('h'))
        .or(t.strip_suffix('H'))
    {
        return usize::from_str_radix(h, 16).ok();
    }
    t.parse()
        .ok()
        .or_else(|| usize::from_str_radix(&t, 16).ok())
}

/// The first occurrence of `needle` starting after `from` (forward) or before it (backward).
pub fn find(hay: &[u8], needle: &[u8], from: Option<usize>, forward: bool) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    if forward {
        let start = from.map_or(0, |f| f + 1).min(hay.len());
        memchr::memmem::find(&hay[start..], needle).map(|i| start + i)
    } else {
        let end = from
            .unwrap_or(hay.len())
            .saturating_add(needle.len() - 1)
            .min(hay.len());
        memchr::memmem::rfind(&hay[..end], needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let a = b"hello world".to_vec();
        let b = b"hallo world!!".to_vec();
        let r = diff_ranges(&[&a, &b]);
        assert_eq!(r, vec![1..2, 11..13]);
    }

    #[test]
    fn parses_patterns_and_offsets_and_finds_bytes() {
        assert_eq!(
            parse_pattern("de ad 0xBEEF"),
            Some(vec![0xDE, 0xAD, 0xBE, 0xEF])
        );
        assert_eq!(parse_pattern("cafe"), Some(vec![0xCA, 0xFE]));
        assert_eq!(parse_pattern("\"cafe\""), Some(b"cafe".to_vec()));
        assert_eq!(parse_pattern("hello"), Some(b"hello".to_vec()));
        assert_eq!(parse_pattern("abc"), Some(b"abc".to_vec()));
        assert_eq!(parse_pattern("  "), None);

        assert_eq!(parse_offset("0x10"), Some(16));
        assert_eq!(parse_offset("10h"), Some(16));
        assert_eq!(parse_offset("1F"), Some(31));
        assert_eq!(parse_offset("1_000"), Some(1000));
        assert_eq!(parse_offset("zz"), None);

        let hay = b"abcXabcXabc";
        assert_eq!(find(hay, b"abc", None, true), Some(0));
        assert_eq!(find(hay, b"abc", Some(0), true), Some(4));
        assert_eq!(find(hay, b"abc", Some(8), true), None);
        assert_eq!(find(hay, b"abc", Some(8), false), Some(4));
        assert_eq!(find(hay, b"abc", Some(4), false), Some(0));
        assert_eq!(find(hay, b"abc", None, false), Some(8));
        assert_eq!(find(hay, b"abc", Some(0), false), None);
    }
}
