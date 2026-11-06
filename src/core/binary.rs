//! Byte-wise (hex) comparison.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

/// Small matches in binary data are usually coincidence rather than useful blocks.
pub const MIN_MOVED_BLOCK: usize = 16;

/// An exact byte sequence found at different offsets in two panes. This is an annotation:
/// positional differences and merge ranges are deliberately unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovedBlock {
    pub from: usize,
    pub to: usize,
    pub source: Range<usize>,
    pub target: Range<usize>,
}

/// A cheap fingerprint for a 16-byte window. Exact bytes are verified before
/// extending any candidate, so collisions can only suppress a match.
fn fingerprint(window: &[u8]) -> u64 {
    let first = u64::from_le_bytes(window[..8].try_into().unwrap());
    let second = u64::from_le_bytes(window[8..].try_into().unwrap());
    (first.wrapping_mul(0x9e3779b185ebca87) ^ second.rotate_left(31))
        .wrapping_mul(0xc2b2ae3d27d4eb4f)
}

const ALIGN_BAND: usize = 32;
const ALIGN_WIDTH: usize = 2 * ALIGN_BAND + 1;
const ALIGN_CELLS: usize = 4 * 1024 * 1024;
const ALIGN_ROWS: usize = ALIGN_CELLS / ALIGN_WIDTH;
const WIDE_BAND: usize = 128;
const WIDE_WIDTH: usize = 2 * WIDE_BAND + 1;

fn align_with_budget(
    left: &[u8],
    right: &[u8],
    shift_hint: usize,
    remaining_cells: &mut usize,
    cancel: &AtomicBool,
) -> Vec<(Range<usize>, Range<usize>)> {
    let delta = left.len().abs_diff(right.len());
    let shift = if delta <= WIDE_BAND {
        shift_hint.max(delta)
    } else {
        shift_hint
    };
    let wide_cells = (left.len().min(right.len() + WIDE_BAND) + 1).saturating_mul(WIDE_WIDTH);
    let mut widen = shift > ALIGN_BAND;
    if !widen && wide_cells <= *remaining_cells {
        // A balanced insertion and deletion can have zero net shift. Look for
        // an exact resynchronization near the first change before widening.
        let at = common_prefix(left, right);
        let nearby_shift = [(left, right), (right, left)]
            .into_iter()
            .filter_map(|(a, b)| {
                let needle = a.get(at..at + MIN_MOVED_BLOCK)?;
                let haystack = b.get(at + 1..b.len().min(at + WIDE_BAND + MIN_MOVED_BLOCK))?;
                memchr::memmem::find(haystack, needle).map(|offset| offset + 1)
            })
            .min();
        widen = nearby_shift.is_some_and(|offset| offset > ALIGN_BAND);
    }
    if widen && wide_cells <= *remaining_cells {
        *remaining_cells -= wide_cells;
        return align_window::<WIDE_BAND, WIDE_WIDTH>(left, right, cancel);
    }
    let cells = (left.len().min(right.len() + ALIGN_BAND) + 1).saturating_mul(ALIGN_WIDTH);
    if cells > *remaining_cells {
        return Vec::new();
    }
    *remaining_cells -= cells;
    align_window::<ALIGN_BAND, ALIGN_WIDTH>(left, right, cancel)
}

fn common_prefix(left: &[u8], right: &[u8]) -> usize {
    const CHUNK: usize = 64;
    let len = left.len().min(right.len());
    let prefix = left[..len]
        .as_chunks::<CHUNK>()
        .0
        .iter()
        .zip(right[..len].as_chunks::<CHUNK>().0)
        .take_while(|(a, b)| a == b)
        .count()
        * CHUNK;
    prefix
        + left[prefix..]
            .iter()
            .zip(&right[prefix..])
            .take_while(|(a, b)| a == b)
            .count()
}

fn common_suffix(left: &[u8], right: &[u8]) -> usize {
    const CHUNK: usize = 64;
    let len = left.len().min(right.len());
    let suffix = left
        .rchunks_exact(CHUNK)
        .zip(right.rchunks_exact(CHUNK))
        .take_while(|(a, b)| a == b)
        .count()
        * CHUNK;
    let left_end = left.len() - suffix;
    let right_end = right.len() - suffix;
    suffix
        + left[..left_end]
            .iter()
            .rev()
            .zip(right[..right_end].iter().rev())
            .take(len - suffix)
            .take_while(|(a, b)| a == b)
            .count()
}

fn aligned_runs(
    left: &[u8],
    right: &[u8],
    shift_hint: usize,
    cancel: &AtomicBool,
) -> Vec<(Range<usize>, Range<usize>)> {
    let mut remaining_cells = ALIGN_CELLS;
    if left.len().min(right.len() + ALIGN_BAND) < ALIGN_ROWS {
        return align_with_budget(left, right, shift_hint, &mut remaining_cells, cancel);
    }

    // Large files often differ in a small region. Keep enough unchanged
    // context on both sides to establish the alignment and support short runs.
    const CONTEXT: usize = ALIGN_BAND + MIN_MOVED_BLOCK;
    let prefix = common_prefix(left, right);
    let start = prefix.saturating_sub(CONTEXT);
    let suffix = common_suffix(&left[prefix..], &right[prefix..]).saturating_sub(CONTEXT);
    let left_end = left.len() - suffix;
    let right_end = right.len() - suffix;
    if left.len() == right.len() && left_end - start >= ALIGN_ROWS {
        // Equal spans at the same offsets separate independent edit regions.
        // Spend the same total traceback budget as for a single window.
        const CHUNK: usize = 2 * CONTEXT;
        let mut runs = Vec::new();
        let mut region = None;
        for (index, equal) in left
            .chunks(CHUNK)
            .zip(right.chunks(CHUNK))
            .map(|(a, b)| a == b)
            .chain(std::iter::once(true))
            .enumerate()
        {
            if index % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let offset = index * CHUNK;
            if !equal {
                region.get_or_insert(offset);
            } else if let Some(region_start) = region.take() {
                let lo = region_start.saturating_sub(CONTEXT);
                let hi = (offset + CONTEXT).min(left.len());
                runs.extend(
                    align_with_budget(
                        &left[lo..hi],
                        &right[lo..hi],
                        shift_hint,
                        &mut remaining_cells,
                        cancel,
                    )
                    .into_iter()
                    .map(|(a, b)| (a.start + lo..a.end + lo, b.start + lo..b.end + lo)),
                );
            }
        }
        return runs;
    }
    let mut runs = align_with_budget(
        &left[start..left_end],
        &right[start..right_end],
        shift_hint,
        &mut remaining_cells,
        cancel,
    );
    for (source, target) in &mut runs {
        *source = source.start + start..source.end + start;
        *target = target.start + start..target.end + start;
        if source.end == left_end && target.end == right_end {
            source.end += suffix;
            target.end += suffix;
        }
    }
    runs
}

/// Banded edit-distance alignment with bounded traceback storage.
fn align_window<const BAND: usize, const WIDTH: usize>(
    left: &[u8],
    right: &[u8],
    cancel: &AtomicBool,
) -> Vec<(Range<usize>, Range<usize>)> {
    const INF: u32 = u32::MAX / 2;
    const DIAGONAL: u8 = 1;
    const DELETE: u8 = 2;
    const INSERT: u8 = 3;
    let (n, m) = (left.len(), right.len());
    let last_row = n.min(m + BAND);
    let mut trace = vec![0; (last_row + 1) * WIDTH];
    let mut prev = [INF; WIDTH];
    let mut endpoint = (u64::MAX, 0, 0);
    for i in 0..=last_row {
        if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut row = [INF; WIDTH];
        for j in i.saturating_sub(BAND)..=m.min(i + BAND) {
            let k = j + BAND - i;
            if i == 0 && j == 0 {
                row[k] = 0;
                continue;
            }
            // Prefer a substitution over introducing gaps when the bytes do
            // not support a longer exact alignment.
            let mut best = (INF, 0);
            if i > 0 && j > 0 {
                best = (
                    prev[k] + 2 * u32::from(left[i - 1] != right[j - 1]),
                    DIAGONAL,
                );
            }
            if i > 0 && k + 1 < WIDTH && prev[k + 1] + 3 < best.0 {
                best = (prev[k + 1] + 3, DELETE);
            }
            if j > 0 && k > 0 && row[k - 1] + 3 < best.0 {
                best = (row[k - 1] + 3, INSERT);
            }
            row[k] = best.0;
            trace[i * WIDTH + k] = best.1;
            // When EOF lies outside the band, finish at the shorter file's
            // boundary and charge for the remaining trailing gap.
            if (n.abs_diff(m) > BAND && (i == n || j == m)) || (i == n && j == m) {
                let cost = u64::from(best.0) + 3 * (n - i + m - j) as u64;
                if cost <= endpoint.0 {
                    endpoint = (cost, i, j);
                }
            }
        }
        prev = row;
    }

    let mut runs: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let (_, mut i, mut j) = endpoint;
    while i != 0 || j != 0 {
        match trace[i * WIDTH + j + BAND - i] {
            DIAGONAL if left[i - 1] == right[j - 1] => {
                let (end_i, end_j) = (i, j);
                while i > 0
                    && j > 0
                    && left[i - 1] == right[j - 1]
                    && trace[i * WIDTH + j + BAND - i] == DIAGONAL
                {
                    i -= 1;
                    j -= 1;
                }
                runs.push((i..end_i, j..end_j));
            }
            DIAGONAL => {
                i -= 1;
                j -= 1;
            }
            DELETE => i -= 1,
            INSERT => j -= 1,
            _ => return Vec::new(),
        }
    }
    runs.reverse();
    // Short runs need support from the surrounding alignment, rather than
    // acting as standalone seeds. Include unshifted runs as context.
    let nearby = |a: &(Range<usize>, Range<usize>), b: &(Range<usize>, Range<usize>)| {
        (b.0.start - a.0.end) + (b.1.start - a.1.end) <= 2
    };
    runs.iter()
        .enumerate()
        .filter(|(index, (a, b))| {
            if a.start == b.start {
                return false;
            }
            let current = &runs[*index];
            let prev = index.checked_sub(1).and_then(|i| runs.get(i));
            let next = runs.get(index + 1);
            let supported = a.len() >= 8
                && (prev.is_some_and(|p| p.0.len() >= 8 && nearby(p, current))
                    || next.is_some_and(|n| n.0.len() >= 8 && nearby(current, n)));
            let short_header = a.len() >= 4
                && a.start.min(b.start) == 0
                && next.is_some_and(|n| {
                    n.0.len() >= MIN_MOVED_BLOCK && n.0.start == a.end + 1 && n.1.start == b.end + 1
                });
            (a.len() >= MIN_MOVED_BLOCK || supported || short_header)
                && right.get(a.clone()) != Some(&left[a.clone()])
                && left.get(b.clone()) != Some(&right[b.clone()])
        })
        .map(|(_, pair)| pair.clone())
        .collect()
}

/// `moved_blocks_until`, never stopped.
#[cfg(test)]
pub fn moved_blocks(data: &[&[u8]]) -> Vec<MovedBlock> {
    moved_blocks_until(data, &AtomicBool::new(false)).unwrap_or_default()
}

/// Detects exact relocations and locally aligned runs, pairwise for two or three buffers.
/// The relocation search ignores repeated seeds, including unsampled occurrences, and samples to
/// bound index memory; matches shorter than the sampling stride plus 16 bytes may be missed.
/// Gives up (`None`) soon after `cancel` is set.
pub fn moved_blocks_until(data: &[&[u8]], cancel: &AtomicBool) -> Option<Vec<MovedBlock>> {
    use std::collections::{BTreeMap, HashMap};
    if !(2..=3).contains(&data.len()) {
        return Some(Vec::new());
    }
    // Checked every so many windows: often enough to stop within a millisecond or so.
    let stopped = |i: usize| i.is_multiple_of(65_536) && cancel.load(Ordering::Relaxed);
    #[derive(Clone, Copy)]
    struct Seed {
        left: usize,
        unique: bool,
        right: Option<usize>,
        repeated: bool,
    }
    // The unused interval around an anchor, or None if it is already covered.
    fn bounds(used: &BTreeMap<usize, usize>, at: usize, len: usize) -> Option<(usize, usize)> {
        let lo = used.range(..=at).next_back().map_or(0, |(_, &end)| end);
        let hi = used.range(at..).next().map_or(len, |(&start, _)| start);
        (lo <= at && at + MIN_MOVED_BLOCK <= hi).then_some((lo, hi))
    }
    let mut out = Vec::new();
    for from in 0..data.len() {
        for to in from + 1..data.len() {
            let (left, right) = (data[from], data[to]);
            if left == right || left.len().min(right.len()) < MIN_MOVED_BLOCK {
                continue;
            }
            const MAX_SEEDS: usize = 65_536;
            let stride = (left.len() - MIN_MOVED_BLOCK + 1).div_ceil(MAX_SEEDS);
            let samples = (left.len() - MIN_MOVED_BLOCK + 1).div_ceil(stride);
            let mut seeds = HashMap::with_capacity(samples);
            // A cheap membership filter avoids most hash-table probes for
            // unrelated bytes. False positives still go through the map.
            let mut filter = vec![0u64; (samples * 16).next_power_of_two().div_ceil(64)];
            let bit = |hash: u64| ((hash >> 32) ^ hash) as usize;
            let mask = filter.len() * 64 - 1;
            let mut repeated_samples = false;
            for (i, window) in left.windows(MIN_MOVED_BLOCK).enumerate().step_by(stride) {
                if stopped(i / stride) {
                    return None;
                }
                // An exact hit at its original offset cannot also be a unique
                // relocation. Ignore it before fingerprinting large inputs.
                if stride > 1 && right.get(i..i + MIN_MOVED_BLOCK) == Some(window) {
                    continue;
                }
                let hash = fingerprint(window);
                let at = bit(hash) & mask;
                filter[at / 64] |= 1 << (at % 64);
                let seed = seeds.entry(hash).or_insert(Seed {
                    left: i,
                    unique: true,
                    right: None,
                    repeated: false,
                });
                seed.unique &= seed.left == i;
                repeated_samples |= !seed.unique;
            }
            // Padding-heavy files otherwise probe the map for every repeated
            // window even after its seed has been ruled out.
            if repeated_samples {
                seeds.retain(|_, seed| seed.unique);
                filter.fill(0);
                for &hash in seeds.keys() {
                    let at = bit(hash) & mask;
                    filter[at / 64] |= 1 << (at % 64);
                }
            }
            if !seeds.is_empty() {
                for (i, window) in right.windows(MIN_MOVED_BLOCK).enumerate() {
                    if stopped(i) {
                        return None;
                    }
                    let hash = fingerprint(window);
                    let at = bit(hash) & mask;
                    if filter[at / 64] & (1 << (at % 64)) == 0 {
                        continue;
                    }
                    if let Some(s) = seeds.get_mut(&hash).filter(|s| s.unique) {
                        if s.right.replace(i).is_some() {
                            s.repeated = true;
                        }
                    }
                }
            }
            // Only shifted, unique target hits need a second source pass.
            // For stride one, insertion into the map already checked all windows.
            seeds.retain(|_, s| s.unique && !s.repeated && s.right.is_some_and(|r| r != s.left));
            if stride > 1 && !seeds.is_empty() {
                filter.fill(0);
                for &hash in seeds.keys() {
                    let at = bit(hash) & mask;
                    filter[at / 64] |= 1 << (at % 64);
                }
                for (i, window) in left.windows(MIN_MOVED_BLOCK).enumerate() {
                    if stopped(i) {
                        return None;
                    }
                    let hash = fingerprint(window);
                    let at = bit(hash) & mask;
                    if filter[at / 64] & (1 << (at % 64)) != 0 {
                        if let Some(s) = seeds.get_mut(&hash) {
                            s.unique &= i == s.left;
                        }
                    }
                }
            }
            let mut anchors: Vec<_> = seeds
                .values()
                .filter_map(|s| {
                    let r = s.right?;
                    s.unique.then_some((s.left, r))
                })
                .collect();
            anchors.sort_unstable();
            let (mut used_left, mut used_right) = (BTreeMap::new(), BTreeMap::new());
            let mut blocks = 0;
            let pair_start = out.len();
            for (a, b) in anchors {
                let Some((lo_a, hi_a)) = bounds(&used_left, a, left.len()) else {
                    continue;
                };
                let Some((lo_b, hi_b)) = bounds(&used_right, b, right.len()) else {
                    continue;
                };
                if left[a..a + MIN_MOVED_BLOCK] != right[b..b + MIN_MOVED_BLOCK] {
                    continue;
                }
                let (mut a, mut b, mut len) = (a, b, MIN_MOVED_BLOCK);
                while a > lo_a && b > lo_b && left[a - 1] == right[b - 1] {
                    a -= 1;
                    b -= 1;
                    len += 1;
                }
                while a + len < hi_a && b + len < hi_b && left[a + len] == right[b + len] {
                    len += 1;
                }
                // Remember even rejected runs, so many anchors in one long run cannot make
                // extension quadratic. Runs never overlap within a pane pair.
                used_left.insert(a, a + len);
                used_right.insert(b, b + len);
                while len > 0 && right.get(a) == Some(&left[a]) && left.get(b) == Some(&right[b]) {
                    a += 1;
                    b += 1;
                    len -= 1;
                }
                while len > 0
                    && right.get(a + len - 1) == Some(&left[a + len - 1])
                    && left.get(b + len - 1) == Some(&right[b + len - 1])
                {
                    len -= 1;
                }
                if len >= MIN_MOVED_BLOCK
                    && right.get(a..a + len) != Some(&left[a..a + len])
                    && left.get(b..b + len) != Some(&right[b..b + len])
                {
                    out.push(MovedBlock {
                        from,
                        to,
                        source: a..a + len,
                        target: b..b + len,
                    });
                    blocks += 1;
                    if blocks == 1024 {
                        break;
                    }
                }
            }
            let shift_hint = out[pair_start..]
                .iter()
                .map(|m| m.source.start.abs_diff(m.target.start))
                .filter(|&shift| shift <= WIDE_BAND)
                .max()
                .unwrap_or(0);
            let runs = aligned_runs(left, right, shift_hint, cancel);
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            for (source, target) in runs {
                // Project conflicts from either pane onto the same run offsets.
                // A conflict at one end must not discard the rest of the run.
                let mut conflicts = Vec::new();
                for m in &out[pair_start..] {
                    for (run, used) in [(&source, &m.source), (&target, &m.target)] {
                        let start = used.start.saturating_sub(run.start);
                        let end = used.end.min(run.end).saturating_sub(run.start);
                        if start < end {
                            conflicts.push(start..end);
                        }
                    }
                }
                conflicts.sort_unstable_by_key(|r| r.start);
                let minimum = if conflicts.is_empty() {
                    1
                } else {
                    MIN_MOVED_BLOCK
                };
                let mut start = 0;
                let len = source.len();
                for conflict in conflicts.into_iter().chain(std::iter::once(len..len)) {
                    if out.len() - pair_start == 1024 {
                        break;
                    }
                    if conflict.start.saturating_sub(start) >= minimum {
                        out.push(MovedBlock {
                            from,
                            to,
                            source: source.start + start..source.start + conflict.start,
                            target: target.start + start..target.start + conflict.start,
                        });
                    }
                    start = start.max(conflict.end);
                }
                if out.len() - pair_start == 1024 {
                    break;
                }
            }
            out[pair_start..].sort_unstable_by_key(|m| m.source.start);
        }
    }
    Some(out)
}

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
    parse_hex(q).or_else(|| Some(q.as_bytes().to_vec()))
}

/// Bytes written in hexadecimal, such as `DE AD BE EF`, `deadbeef` or `0xDE 0xAD`; `None` for
/// anything else, including an odd number of digits.
pub fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let digits: String = text
        .split_whitespace()
        .map(|w| w.strip_prefix("0x").or(w.strip_prefix("0X")).unwrap_or(w))
        .collect();
    let hex = !digits.is_empty()
        && digits.len().is_multiple_of(2)
        && digits.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
        .collect()
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
    fn detects_relocations_above_the_previous_size_limit() {
        let mut left = vec![0; 16 * 1024 * 1024];
        let a = 1024 * 1024;
        let b = 12 * 1024 * 1024;
        let len = 4096;
        let mut seed = 42u32;
        for byte in &mut left[a..a + len] {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *byte = (seed as u8).max(1);
        }
        let mut right = left.clone();
        right.copy_within(a..a + len, b);
        right[a..a + len].fill(0);
        assert!(moved_blocks(&[&left, &right])
            .iter()
            .any(|m| { m.source == (a..a + len) && m.target == (b..b + len) }));
    }

    #[test]
    fn aligns_wide_shift_through_repeated_data_in_a_large_file() {
        let right: Vec<_> = (0..251u8).cycle().take(1024 * 1024).collect();
        let mut left = right.clone();
        let at = right.len() / 2;
        left.splice(at..at, [255; 96]);
        let moves = moved_blocks(&[&left, &right]);
        assert!(moves
            .iter()
            .any(|m| { m.source == (at + 96..left.len()) && m.target == (at..right.len()) }));
        let reversed = moved_blocks(&[&right, &left]);
        assert!(reversed
            .iter()
            .any(|m| { m.source == (at..right.len()) && m.target == (at + 96..left.len()) }));
    }

    #[test]
    fn aligns_wide_balanced_edits_without_unique_seeds() {
        let right: Vec<_> = (0..251u8).cycle().take(1024 * 1024).collect();
        let mut left = right.clone();
        let at = right.len() / 2;
        let end = at + 96 + 4096;
        left.splice(at..at, [255; 96]);
        left.drain(end..end + 96);
        let moves = moved_blocks(&[&left, &right]);
        assert!(moves
            .iter()
            .any(|m| { m.source == (at + 96..end) && m.target == (at..end - 96) }));
    }

    #[test]
    fn aligns_short_shifts_in_separate_regions_of_large_files() {
        let mut state = 4u32;
        let right: Vec<_> = (0..256 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let mut left = right.clone();
        for at in [4096, 192 * 1024] {
            left.insert(at, right[at] ^ 0xff);
            left.remove(at + 16);
        }
        let moves = moved_blocks(&[&left, &right]);
        for at in [4096, 192 * 1024] {
            assert!(moves
                .iter()
                .any(|m| { m.source == (at + 1..at + 16) && m.target == (at..at + 15) }));
        }
    }

    #[test]
    fn overlapping_seed_keeps_aligned_firmware_tail() {
        let right = include_bytes!("../../samples/right/bin/firmware.bin");
        let mut left = right.to_vec();
        left.drain(0x447..0x44c);
        left.drain(0xa0d..0xa11);
        let moves = moved_blocks(&[&left, right]);
        assert!(
            moves
                .iter()
                .any(|m| { m.source == (0xa0d..0xfe7) && m.target == (0xa16..0xff0) }),
            "{moves:?}"
        );
        for (index, m) in moves.iter().enumerate() {
            assert_eq!(left[m.source.clone()], right[m.target.clone()]);
            for other in &moves[..index] {
                assert!(other.source.end <= m.source.start);
                assert!(other.target.end <= m.target.start || m.target.end <= other.target.start);
            }
        }
    }

    #[test]
    fn trailing_bytes_do_not_hide_earlier_firmware_alignment() {
        let base = include_bytes!("../../samples/right/bin/firmware.bin");
        let (mut left, mut right) = (base.to_vec(), base.to_vec());
        left.insert(0x20, 0xa6);
        right.remove(0x2f);
        right.extend([0; 64]);
        for (a, b, source, target) in [
            (&left, &right, 0x21..0x30, 0x20..0x2f),
            (&right, &left, 0x20..0x2f, 0x21..0x30),
        ] {
            assert!(moved_blocks(&[a, b])
                .iter()
                .any(|m| { m.source == source && m.target == target }));
        }
    }

    #[test]
    fn moved_blocks_find_unaligned_swaps_without_changing_positional_differences() {
        let prefix = vec![250; 7];
        let (a, b, c): (Vec<u8>, Vec<u8>, Vec<u8>) =
            ((0..64).collect(), (64..128).collect(), (128..192).collect());
        let left = [prefix.clone(), a.clone(), b.clone(), c.clone()].concat();
        let right = [prefix, a, c, b].concat();
        let refs = [left.as_slice(), right.as_slice()];
        let before = diff_ranges(&refs);
        let moved = moved_blocks(&refs);
        assert_eq!(
            moved,
            vec![
                MovedBlock {
                    from: 0,
                    to: 1,
                    source: 71..135,
                    target: 135..199
                },
                MovedBlock {
                    from: 0,
                    to: 1,
                    source: 135..199,
                    target: 71..135
                },
            ]
        );
        assert_eq!(diff_ranges(&refs), before);
        assert_eq!(moved_blocks(&refs), moved, "deterministic pairing");
    }

    #[test]
    fn moved_blocks_ignore_short_identical_copied_and_repeated_data() {
        let data: Vec<u8> = (0..100).collect();
        assert!(moved_blocks(&[&data, &data]).is_empty());
        let copied = [data.clone(), data[20..60].to_vec()].concat();
        assert!(
            moved_blocks(&[&data, &copied]).is_empty(),
            "copy is ambiguous with retained bytes"
        );
        let short: Vec<u8> = (0..30).collect();
        let swapped = [&short[15..], &short[..15]].concat();
        assert!(moved_blocks(&[&short, &swapped]).is_empty());
        assert!(moved_blocks(&[&[], &data]).is_empty());
        assert!(moved_blocks(&[&vec![0; 100_000], &vec![0; 100_010]]).is_empty());
    }

    #[test]
    fn moved_blocks_check_unsampled_duplicates_and_all_three_pairs() {
        let mut seed = 17u64;
        let mut left: Vec<u8> = (0..200_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let block: Vec<u8> = (0..32).collect();
        left[100..132].copy_from_slice(&block);
        left[150_003..150_035].copy_from_slice(&block);
        let mut right = vec![0; left.len()];
        right[50_000..50_032].copy_from_slice(&block);
        assert!(moved_blocks(&[&left, &right]).is_empty());

        let (a, b, c): (Vec<u8>, Vec<u8>, Vec<u8>) =
            ((0..64).collect(), (64..128).collect(), (128..192).collect());
        let docs = [
            [a.clone(), b.clone(), c.clone()].concat(),
            [c.clone(), a.clone(), b.clone()].concat(),
            [b, c, a].concat(),
        ];
        let refs: Vec<_> = docs.iter().map(Vec::as_slice).collect();
        let moved = moved_blocks(&refs);
        for pair in [(0, 1), (0, 2), (1, 2)] {
            assert!(moved.iter().any(|m| (m.from, m.to) == pair));
            let matches: Vec<_> = moved.iter().filter(|m| (m.from, m.to) == pair).collect();
            for (i, m) in matches.iter().enumerate() {
                assert_eq!(refs[m.from][m.source.clone()], refs[m.to][m.target.clone()]);
                assert!(m.source.len() >= MIN_MOVED_BLOCK);
                assert_ne!(m.source.start, m.target.start);
                for other in &matches[i + 1..] {
                    assert!(
                        m.source.end <= other.source.start || other.source.end <= m.source.start
                    );
                    assert!(
                        m.target.end <= other.target.start || other.target.end <= m.target.start
                    );
                }
            }
        }
    }

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
        assert_eq!(parse_hex(" de AD\n0xbe "), Some(vec![0xDE, 0xAD, 0xBE]));
        assert_eq!(parse_hex("abc"), None);
        assert_eq!(parse_hex("hello"), None);
        assert_eq!(parse_hex(""), None);

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
