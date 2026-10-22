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
}
