//! Automatic 3-way folder merge. The middle folder is the common ancestor and receives the
//! result: changes made on one side only (or the same on both) are copied into it, and text
//! files changed on both sides are merged line by line when their changes do not overlap.
//! Only the listed items are touched, so files the filter hides are left alone.

use super::diff::merge3;
use super::folder::{copy_recursive, delete_path, CmpStatus, DirItem};
use super::text::{self, TextDoc};
use crate::i18n::tr;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BASE: usize = 1;

pub enum Step {
    /// Copy the file of this side over the middle one, or create the folder.
    Copy(usize),
    /// Delete the middle item: the other sides removed it without changing it.
    Delete,
    /// Remove the middle folder once its listed contents are deleted, if nothing is left in it.
    Prune,
    /// Save this merge of both sides' changes as the middle file.
    Merge(TextDoc),
    /// Changed differently on both sides, or changed on one and removed on the other.
    Conflict,
}

/// Size and modification time of a file on each side, or `None` where it is missing.
type Stamp = [Option<(u64, Option<SystemTime>)>; 3];

fn stamp(it: &DirItem, roots: &[PathBuf]) -> Stamp {
    std::array::from_fn(|s| {
        let md = std::fs::metadata(it.path(&roots[s])).ok()?;
        Some((md.len(), md.modified().ok()))
    })
}

#[derive(Default)]
pub struct Plan {
    /// Item index and what to do with it, in item order.
    pub steps: Vec<(usize, Step)>,
    /// The files of the steps as they were planned, so files changed since are left alone.
    stamps: BTreeMap<usize, Stamp>,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Counts {
    pub copies: usize,
    pub deletes: usize,
    pub merges: usize,
    pub conflicts: usize,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for (_, s) in &self.steps {
            match s {
                Step::Copy(_) => c.copies += 1,
                Step::Delete | Step::Prune => c.deletes += 1,
                Step::Merge(_) => c.merges += 1,
                Step::Conflict => c.conflicts += 1,
            }
        }
        c
    }

    pub fn changes(&self) -> usize {
        let c = self.counts();
        c.copies + c.deletes + c.merges
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let Ok(n) = fa.read(&mut ba) else {
            return false;
        };
        if n == 0 {
            return true;
        }
        if fb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

/// The middle file with both sides' changes, when they are text and do not overlap.
fn merge_text(paths: &[PathBuf]) -> Option<TextDoc> {
    let docs: Vec<TextDoc> = paths
        .iter()
        .map(|p| text::load_doc(p, None).ok())
        .collect::<Option<_>>()?;
    let lines = merge3(&docs[0].lines, &docs[BASE].lines, &docs[2].lines)?;
    let mut doc = docs[BASE].clone();
    doc.lines = lines;
    Some(doc)
}

/// Decides what the merge does with every item. Items inside a folder removed on both sides
/// get no step of their own. Files are compared byte for byte: the folder compare may have
/// called them identical under its ignore options.
pub fn plan(items: &[DirItem], roots: &[PathBuf]) -> Plan {
    let mut steps: BTreeMap<usize, Step> = BTreeMap::new();
    if roots.len() != 3 {
        return Plan::default();
    }
    for (i, it) in items.iter().enumerate() {
        let covered = std::iter::successors(it.parent, |&p| items[p].parent)
            .any(|p| matches!(steps.get(&p), Some(Step::Delete)));
        if covered || it.status == CmpStatus::Error {
            continue;
        }
        let path = |s: usize| it.path(&roots[s]);
        let step = match it.exists {
            [true, true, true] if it.is_dir => None,
            [true, true, true] => {
                let paths: Vec<PathBuf> = (0..3).map(path).collect();
                let left_same = same_file(&paths[0], &paths[BASE]);
                let right_same = same_file(&paths[2], &paths[BASE]);
                match (left_same, right_same) {
                    (true, true) => None,
                    (false, true) => Some(Step::Copy(0)),
                    (true, false) => Some(Step::Copy(2)),
                    (false, false) if same_file(&paths[0], &paths[2]) => Some(Step::Copy(0)),
                    (false, false) if it.binary => Some(Step::Conflict),
                    (false, false) => Some(merge_text(&paths).map_or(Step::Conflict, Step::Merge)),
                }
            }
            [true, false, false] => Some(Step::Copy(0)),
            [false, false, true] => Some(Step::Copy(2)),
            [false, true, false] => Some(Step::Delete),
            // added on both sides: their folders' contents are merged item by item
            [true, false, true] if it.is_dir => None,
            [true, false, true] if same_file(&path(0), &path(2)) => Some(Step::Copy(0)),
            [true, false, true] => Some(Step::Conflict),
            // removed on one side: deleted when the other side did not change it
            [l, true, r] if it.is_dir && l != r => None,
            [l, true, r] if l != r => {
                let kept = if l { 0 } else { 2 };
                Some(if same_file(&path(kept), &path(BASE)) {
                    Step::Delete
                } else {
                    Step::Conflict
                })
            }
            _ => None,
        };
        if let Some(step) = step {
            steps.insert(i, step);
        }
    }
    // A folder removed on one side whose listed contents all go is removed after them.
    for (i, it) in items.iter().enumerate().rev() {
        let removed_on_one_side = it.is_dir && it.exists[BASE] && it.exists[0] != it.exists[2];
        if removed_on_one_side
            && !steps.contains_key(&i)
            && it
                .children
                .iter()
                .all(|c| matches!(steps.get(c), Some(Step::Delete | Step::Prune)))
        {
            steps.insert(i, Step::Prune);
        }
    }
    let stamps = steps
        .keys()
        .filter(|&&i| !items[i].is_dir)
        .map(|&i| (i, stamp(&items[i], roots)))
        .collect();
    Plan {
        steps: steps.into_iter().collect(),
        stamps,
    }
}

/// Carries out the plan. Returns how many items changed and the errors.
pub fn apply(
    plan: Plan,
    items: &[DirItem],
    roots: &[PathBuf],
    use_recycle_bin: bool,
) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut errors = Vec::new();
    let mut prune = Vec::new();
    for (i, step) in plan.steps {
        let it = &items[i];
        let target = it.path(&roots[BASE]);
        if plan.stamps.get(&i).is_some_and(|s| *s != stamp(it, roots)) {
            errors.push(format!(
                "{}: {}",
                it.rel_path(),
                tr("msg.changed_since_plan")
            ));
            continue;
        }
        let r = match step {
            Step::Copy(_) if it.is_dir => std::fs::create_dir_all(&target).map_err(Into::into),
            Step::Copy(from) => {
                if target.is_dir() {
                    delete_path(&target, use_recycle_bin)
                        .and_then(|_| copy_recursive(&it.path(&roots[from]), &target))
                } else {
                    copy_recursive(&it.path(&roots[from]), &target)
                }
            }
            Step::Delete => delete_path(&target, use_recycle_bin),
            Step::Prune => {
                prune.push(i);
                continue;
            }
            Step::Merge(mut doc) => doc.save(),
            Step::Conflict => continue,
        };
        match r {
            Ok(()) => done += 1,
            Err(e) => errors.push(format!("{}: {e}", it.rel_path())),
        }
    }
    // innermost folders first; a folder still holding files the filter hides stays
    for i in prune.into_iter().rev() {
        match std::fs::remove_dir(items[i].path(&roots[BASE])) {
            Ok(()) => done += 1,
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(e) => errors.push(format!("{}: {e}", items[i].rel_path())),
        }
    }
    (done, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::DiffOptions;
    use crate::core::filter::FileFilter;
    use crate::core::folder::{FolderOptions, Progress, Scanner};

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn scan(roots: &[PathBuf]) -> Vec<DirItem> {
        Scanner {
            roots,
            filter: &FileFilter::parse_mask("!*.log"),
            fopts: &FolderOptions::default(),
            dopts: &DiffOptions::default(),
            progress: &Progress::default(),
        }
        .run()
    }

    #[test]
    fn merges_folders_into_the_middle() {
        let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let [l, b, r] = [dirs[0].path(), dirs[1].path(), dirs[2].path()];
        for root in [l, b, r] {
            write(root, "same.txt", "same\n");
            write(root, "gone/old.txt", "old\n");
        }
        write(b, "left_changed.txt", "base\n");
        write(l, "left_changed.txt", "left\n");
        write(r, "left_changed.txt", "base\n");
        write(b, "both.txt", "1\n2\n3\n4\n5\n");
        write(l, "both.txt", "one\n2\n3\n4\n5\n");
        write(r, "both.txt", "1\n2\n3\n4\nfive\n");
        write(b, "conflict.txt", "x\n");
        write(l, "conflict.txt", "left\n");
        write(r, "conflict.txt", "right\n");
        write(r, "new/added.txt", "new\n");
        write(b, "removed.txt", "keep?\n");
        write(l, "removed.txt", "keep?\n");
        for root in [b, r] {
            write(root, "spaces.txt", "a b\n");
        }
        write(l, "spaces.txt", "a  b\n");
        std::fs::remove_dir_all(r.join("gone")).unwrap();
        // hidden by the filter: left alone
        write(b, "gone/hidden.log", "keep\n");
        write(l, "only_left/hidden.log", "not copied\n");
        write(l, "only_left/shown.txt", "copied\n");

        let roots: Vec<PathBuf> = [l, b, r].iter().map(|p| p.to_path_buf()).collect();
        let mut items = scan(&roots);
        // as a compare ignoring whitespace would report it
        let spaces = items.iter().position(|i| i.name == "spaces.txt").unwrap();
        items[spaces].status = CmpStatus::Identical;
        assert!(items.iter().all(|i| !i.name.ends_with(".log")));
        let plan = plan(&items, &roots);
        assert_eq!(
            plan.counts(),
            Counts {
                copies: 6,
                deletes: 3,
                merges: 1,
                conflicts: 1
            }
        );
        let (done, errors) = apply(plan, &items, &roots, false);
        // every step but the pruned folder, which still holds the hidden file
        assert_eq!((done, errors.len()), (9, 0), "{errors:?}");
        let read = |rel: &str| std::fs::read_to_string(b.join(rel)).unwrap();
        assert_eq!(read("left_changed.txt"), "left\n");
        assert_eq!(read("spaces.txt"), "a  b\n");
        assert_eq!(read("both.txt"), "one\n2\n3\n4\nfive\n");
        assert_eq!(read("conflict.txt"), "x\n");
        assert_eq!(read("new/added.txt"), "new\n");
        assert!(!b.join("removed.txt").exists());
        assert!(!b.join("gone/old.txt").exists());
        assert_eq!(read("gone/hidden.log"), "keep\n");
        assert_eq!(read("only_left/shown.txt"), "copied\n");
        assert!(!b.join("only_left/hidden.log").exists());

        // a folder whose listed contents all go is removed once empty
        let fresh: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let fresh_roots: Vec<PathBuf> = fresh.iter().map(|d| d.path().to_path_buf()).collect();
        for root in &fresh_roots[..2] {
            write(root, "d/x.txt", "x\n");
        }
        let items = scan(&fresh_roots);
        let p = super::plan(&items, &fresh_roots);
        assert_eq!(apply(p, &items, &fresh_roots, false), (2, Vec::new()));
        assert!(!fresh_roots[BASE].join("d").exists());

        // a file changed after planning is not overwritten
        write(l, "late.txt", "left\n");
        let items = scan(&roots);
        let p = super::plan(&items, &roots);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(b, "late.txt", "edited meanwhile\n");
        let (_, errors) = apply(p, &items, &roots, false);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(read("late.txt"), "edited meanwhile\n");
    }
}
