//! Three-way structured comparison: the changes of the left and right files from the base,
//! grouped by where they are in the base. Changes of both sides at one path, or one inside a
//! value the other changed, conflict unless both made the same change.
use super::{compare_with, parse, segments, write, Change, ChangeKind, Format, Options};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// Only the left side changed it.
    Left,
    /// Only the right side changed it.
    Right,
    /// Both sides made the same change.
    Both,
    /// The sides changed it differently.
    Conflict,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeEntry {
    /// Where the outermost change is in the base.
    pub path: String,
    pub state: MergeState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<Change>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub right: Vec<Change>,
}

impl MergeEntry {
    /// The path on the left (0) or right (1) side of the node at [`Self::path`] in the base,
    /// when that side changed it.
    pub fn side_path(&self, side: usize) -> Option<String> {
        let changes = if side == 0 { &self.left } else { &self.right };
        if let Some(change) = changes.iter().find(|c| c.at[0] == self.path) {
            return Some(change.at[1].clone());
        }
        // each step of a path is one level, so the outer steps of a change inside it lead there
        let depth = segments(&self.path).len();
        let steps = segments(&changes.first()?.at[1]);
        Some(format!("${}", steps.get(..depth)?.concat()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreeWayReport {
    pub version: u32,
    pub format: Format,
    pub entries: Vec<MergeEntry>,
    pub errors: Vec<String>,
    pub policies: Vec<String>,
    #[serde(default)]
    pub options: Options,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl ThreeWayReport {
    pub fn identical(&self) -> bool {
        self.errors.is_empty() && self.entries.is_empty()
    }

    pub fn conflicts(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == MergeState::Conflict)
            .count()
    }
}

/// How a step of a path sorts: positions by number, so `[10]` comes after `[9]`.
fn step_order(step: &str) -> (u8, usize, &str) {
    let open = step.rfind('[').unwrap_or(0);
    match step[open..]
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .and_then(|s| s.parse().ok())
    {
        Some(index) if !step[open..].starts_with("[\"") => (0, index, &step[..open]),
        _ => (1, 0, step),
    }
}

/// What makes two changes the same change: what it is, where it was and what it made.
fn sameness(change: &Change) -> (String, &str, &Option<String>, &str) {
    let to = if change.kind == ChangeKind::Moved {
        change.at[1].as_str()
    } else {
        ""
    };
    (
        format!("{:?}", change.kind),
        &change.at[0],
        &change.prints[1],
        to,
    )
}

pub fn compare_three(
    left: &str,
    base: &str,
    right: &str,
    format: Format,
    options: &Options,
) -> ThreeWayReport {
    let mut report = ThreeWayReport {
        version: 1,
        format,
        entries: Vec::new(),
        errors: Vec::new(),
        policies: vec![
            "Changes of the left and right files from the base, grouped by where they are in the base. A path both sides changed, or a change on one side inside a value the other side changed, is a conflict unless both made the same change. Values compare as in a two-way semantic comparison.".into(),
        ],
        options: options.clone(),
        notes: Vec::new(),
    };
    for (name, source) in [("Left", left), ("Base", base), ("Right", right)] {
        if let Err(e) = parse(source, format) {
            report.errors.push(format!("{name}: {e:#}; use Text view"));
        }
    }
    if !report.errors.is_empty() {
        return report;
    }
    let sides = [
        compare_with(base, left, format, options),
        compare_with(base, right, format, options),
    ];
    let mut changes = Vec::new();
    for (side, r) in sides.into_iter().enumerate() {
        report.errors.extend(r.errors);
        for note in r.notes {
            if !report.notes.contains(&note) {
                report.notes.push(note);
            }
        }
        changes.extend(r.changes.into_iter().map(|c| (side, c)));
    }
    if !report.errors.is_empty() {
        return report;
    }
    changes.sort_by_cached_key(|(side, c)| {
        let order: Vec<(u8, usize, String)> = segments(&c.at[0])
            .into_iter()
            .map(|s| {
                let (a, b, c) = step_order(s);
                (a, b, c.to_owned())
            })
            .collect();
        (order, *side)
    });
    let mut outer: Vec<String> = Vec::new();
    for (side, change) in changes {
        let steps: Vec<String> = segments(&change.at[0])
            .into_iter()
            .map(str::to_owned)
            .collect();
        if report.entries.is_empty() || !steps.starts_with(&outer) {
            report.entries.push(MergeEntry {
                path: change.at[0].clone(),
                state: MergeState::Left,
                left: Vec::new(),
                right: Vec::new(),
            });
            outer = steps;
        }
        let entry = report.entries.last_mut().expect("an entry");
        if side == 0 {
            entry.left.push(change);
        } else {
            entry.right.push(change);
        }
    }
    report.entries = join_ambiguous(std::mem::take(&mut report.entries));
    for entry in &mut report.entries {
        entry.state = match (entry.left.is_empty(), entry.right.is_empty()) {
            (false, true) => MergeState::Left,
            (true, false) => MergeState::Right,
            _ => {
                let mut a: Vec<_> = entry.left.iter().map(sameness).collect();
                let mut b: Vec<_> = entry.right.iter().map(sameness).collect();
                a.sort();
                b.sort();
                if a == b {
                    MergeState::Both
                } else {
                    MergeState::Conflict
                }
            }
        };
    }
    report
}

/// The path of the outermost array holding the node at `path`, when it is in one.
fn outer_array(path: &str) -> Option<String> {
    let steps = segments(path);
    let at = steps.iter().position(|s| write::position(s).is_some())?;
    Some(format!("${}", steps[..at].concat()))
}

/// `entries` with those inside an array that both sides changed, when a change in it pairs
/// elements ambiguously, as one entry for the whole array: taking the changes of each side
/// one by one could mix two readings of the array.
fn join_ambiguous(entries: Vec<MergeEntry>) -> Vec<MergeEntry> {
    let arrays: std::collections::BTreeSet<String> = entries
        .iter()
        .flat_map(|e| e.left.iter().chain(&e.right))
        .filter(|c| c.ambiguous)
        .filter_map(|c| outer_array(&c.at[0]))
        .collect();
    if arrays.is_empty() {
        return entries;
    }
    let inside = |entry: &MergeEntry| {
        let steps = segments(&entry.path);
        arrays
            .iter()
            .map(|a| (segments(a), a))
            .filter(|(a, _)| steps.len() > a.len() && steps.starts_with(a))
            .min_by_key(|(a, _)| a.len())
            .map(|(_, a)| a.clone())
    };
    let flush = |group: Option<(String, Vec<MergeEntry>)>, out: &mut Vec<MergeEntry>| {
        let Some((path, group)) = group else { return };
        let both =
            group.iter().any(|e| !e.left.is_empty()) && group.iter().any(|e| !e.right.is_empty());
        if !both {
            out.extend(group);
            return;
        }
        let mut joined = MergeEntry {
            path,
            state: MergeState::Conflict,
            left: Vec::new(),
            right: Vec::new(),
        };
        for e in group {
            joined.left.extend(e.left);
            joined.right.extend(e.right);
        }
        out.push(joined);
    };
    let mut out = Vec::new();
    let mut group: Option<(String, Vec<MergeEntry>)> = None;
    for entry in entries {
        let array = inside(&entry);
        if group.as_ref().map(|(a, _)| a) != array.as_ref() {
            flush(group.take(), &mut out);
        }
        match array {
            Some(a) => group.get_or_insert_with(|| (a, Vec::new())).1.push(entry),
            None => out.push(entry),
        }
    }
    flush(group, &mut out);
    out
}

/// The base text with the node of `entry` taken from the left (0) or right (1) side.
pub fn take(
    entry: &MergeEntry,
    side: usize,
    [left, base, right]: [&str; 3],
    format: Format,
) -> Result<String, write::Refusal> {
    let changes = if side == 0 { &entry.left } else { &entry.right };
    if !changes.is_empty() && changes.iter().all(|c| c.kind == ChangeKind::Moved) {
        return Err(write::Refusal::Moved);
    }
    let path = entry.side_path(side).ok_or(write::Refusal::Stale)?;
    let source = if side == 0 { left } else { right };
    // a change compares the base (0) with the side (1)
    let held = changes
        .iter()
        .find(|c| c.at[0] == entry.path)
        .map_or([true, true], |c| write::held(&c.kind));
    write::copy_value(
        (source, &path),
        (base, &entry.path),
        format,
        [held[1], held[0]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(r: &ThreeWayReport) -> Vec<(String, MergeState)> {
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        r.entries
            .iter()
            .map(|e| (e.path.clone(), e.state))
            .collect()
    }

    #[test]
    fn changes_group_by_base_path_and_conflict_when_they_overlap() {
        let base = r#"{"a": 1, "b": {"x": 1, "y": 2}, "c": [1, 2], "d": 1, "e": 1}"#;
        let left = r#"{"a": 2, "b": {"x": 5, "y": 2}, "c": [1, 2], "d": 9, "e": 1, "n": 1}"#;
        let right = r#"{"a": 1, "b": "gone", "c": [1, 2, 3], "d": 9, "e": 7, "n": 2}"#;
        let r = compare_three(left, base, right, Format::Json, &Options::default());
        assert_eq!(
            states(&r),
            [
                (r#"$["a"]"#.into(), MergeState::Left),
                (r#"$["b"]"#.into(), MergeState::Conflict),
                (r#"$["c"][2]"#.into(), MergeState::Right),
                (r#"$["d"]"#.into(), MergeState::Both),
                (r#"$["e"]"#.into(), MergeState::Right),
                (r#"$["n"]"#.into(), MergeState::Conflict),
            ]
        );
        assert_eq!(r.conflicts(), 2);
        assert!(compare_three(base, base, base, Format::Json, &Options::default()).identical());
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""state":"conflict""#), "{json}");

        let bad = compare_three("{", base, right, Format::Json, &Options::default());
        assert!(bad.errors[0].starts_with("Left:"), "{:?}", bad.errors);
    }

    #[test]
    fn an_element_one_side_added_or_removed_is_taken_without_touching_its_neighbours() {
        let keyed = Options {
            array_key: Some("id".into()),
        };
        let base = r#"[{"id":1,"v":"a"},{"id":2,"v":"b"}]"#;
        let left = r#"[{"id":3,"v":"c"},{"id":1,"v":"a"},{"id":2,"v":"b"}]"#;
        let right = r#"[{"id":2,"v":"b"}]"#;
        let r = compare_three(left, base, right, Format::Json, &keyed);
        let read = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
        let added = r.entries.iter().find(|e| !e.left.is_empty()).unwrap();
        let merged = take(added, 0, [left, base, right], Format::Json).unwrap();
        assert_eq!(read(&merged), read(left));
        let removed = r.entries.iter().find(|e| !e.right.is_empty()).unwrap();
        let merged = take(removed, 1, [left, base, right], Format::Json).unwrap();
        assert_eq!(read(&merged), read(right));
    }

    #[test]
    fn an_array_both_sides_changed_by_an_ambiguous_pairing_is_one_conflict() {
        let base = r#"{"l": [1, 2, 3], "k": 1}"#;
        let left = r#"{"l": [1, 2], "k": 2}"#;
        let right = r#"{"l": [1, 3], "k": 1}"#;
        let r = compare_three(left, base, right, Format::Json, &Options::default());
        assert_eq!(
            states(&r),
            [
                (r#"$["k"]"#.into(), MergeState::Left),
                (r#"$["l"]"#.into(), MergeState::Conflict),
            ]
        );
        let conflict = &r.entries[1];
        for (side, list) in [(0, "[1,2]"), (1, "[1,3]")] {
            let out = take(conflict, side, [left, base, right], Format::Json).unwrap();
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["l"].to_string(), list, "{out}");
        }
        // one side alone keeps its entries
        let r = compare_three(base, base, right, Format::Json, &Options::default());
        assert!(r.entries.iter().all(|e| e.state == MergeState::Right));
        assert!(r.entries.len() > 1, "{:?}", states(&r));
    }

    #[test]
    fn a_side_taken_into_the_base_leaves_only_the_other_side_differing() {
        let base = "a: 1\nb:\n  x: 1\nc: 1\nd:\n  x: 1\nlist:\n  - p\n";
        let left = "a: 2\nb:\n  x: 1\n  y: 2\nc: 1\nd:\n  x: 1\n  y: 2\nlist:\n  - p\n";
        let right = "a: 1\nb: none\nc: 3\nd:\n  x: 3\nlist:\n  - p\n  - q\n";
        let texts = [left, base, right];
        let r = compare_three(left, base, right, Format::Yaml, &Options::default());
        // members of one mapping changed on each side do not conflict
        assert_eq!(
            states(&r),
            [
                (r#"$["a"]"#.into(), MergeState::Left),
                (r#"$["b"]"#.into(), MergeState::Conflict),
                (r#"$["c"]"#.into(), MergeState::Right),
                (r#"$["d"]["x"]"#.into(), MergeState::Right),
                (r#"$["d"]["y"]"#.into(), MergeState::Left),
                (r#"$["list"][1]"#.into(), MergeState::Right),
            ]
        );
        let mut merged = base.to_owned();
        for entry in &r.entries {
            let side = usize::from(entry.state == MergeState::Right);
            if entry.state == MergeState::Conflict {
                continue;
            }
            merged = take(entry, side, [left, &merged, right], Format::Yaml).unwrap();
        }
        assert_eq!(
            merged,
            "a: 2\nb:\n  x: 1\nc: 3\nd:\n  x: 3\n  y: 2\nlist:\n  - p\n  - q\n"
        );
        let conflict = r
            .entries
            .iter()
            .find(|e| e.state == MergeState::Conflict)
            .unwrap();
        assert_eq!(conflict.path, r#"$["b"]"#);
        for side in [0, 1] {
            let out = take(conflict, side, texts, Format::Yaml).unwrap();
            let theirs = [left, right][side];
            let r = super::super::compare(&out, theirs, Format::Yaml);
            assert!(
                r.changes.iter().all(|c| !c.path.starts_with(r#"$["b"]"#)),
                "{out}"
            );
        }
    }
}
