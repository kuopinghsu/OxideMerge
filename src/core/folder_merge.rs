//! Automatic 3-way folder merge. The middle folder is the common ancestor and receives the
//! result: changes made on one side only (or the same on both) are copied into it, and text
//! files changed on both sides are merged line by line when their changes do not overlap.
//! Only the listed items are touched, so files the filter hides are left alone, and so are
//! symbolic links and names that are a file on one side and a folder on another.

use super::diff::merge3;
use super::folder::{delete_path, name_key, CmpStatus, DirItem};
use super::fsutil;
use super::text::{self, TextDoc};
use crate::i18n::tr;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

const BASE: usize = 1;

pub enum Step {
    /// Copy the file of this side over the middle one, or create the folder.
    Copy(usize),
    /// Delete the middle file: the other sides removed it without changing it.
    Delete,
    /// Remove the middle folder once its listed contents are deleted, if nothing is left in it.
    Prune,
    /// Save this merge of both sides' changes as the middle file.
    Merge(TextDoc),
    /// Left as it is: changed differently on both sides, changed on one and removed on the
    /// other, unreadable, binary or not text that saves back the same, a symbolic link, or a
    /// file on one side and a folder on another.
    Conflict,
}

/// What is at a path, without following a symbolic link there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Missing,
    File,
    Dir,
    Link,
    /// A device, pipe or socket.
    Other,
}

fn kind(p: &Path) -> Kind {
    match std::fs::symlink_metadata(p) {
        Err(_) => Kind::Missing,
        Ok(m) if m.file_type().is_symlink() => Kind::Link,
        Ok(m) if m.is_dir() => Kind::Dir,
        Ok(m) if m.is_file() => Kind::File,
        Ok(_) => Kind::Other,
    }
}

/// Size and modification time of a file on each side, or `None` where it is missing. A
/// symbolic link put in a file's place since has other ones.
type Stamp = [Option<(u64, Option<SystemTime>)>; 3];

fn stamp(it: &DirItem, roots: &[PathBuf]) -> Stamp {
    std::array::from_fn(|s| {
        let md = std::fs::symlink_metadata(it.path(&roots[s])).ok()?;
        Some((md.len(), md.modified().ok()))
    })
}

#[derive(Default)]
pub struct Plan {
    /// Item index and what to do with it, in item order.
    pub steps: Vec<(usize, Step)>,
    /// The files of the steps as they were planned, so files changed since are left alone.
    stamps: BTreeMap<usize, Stamp>,
    /// Items that are not on disk as the folder compare found them: the plan is not to be
    /// carried out before comparing again.
    pub stale: Vec<usize>,
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

    /// The items of the steps `which` picks, in item order.
    pub fn items_where(&self, which: impl Fn(&Step) -> bool) -> Vec<usize> {
        self.steps
            .iter()
            .filter(|(_, s)| which(s))
            .map(|(i, _)| *i)
            .collect()
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

/// A text file that saving would write back byte for byte: no NUL bytes outside UTF-16, one
/// kind of line ending, and characters its encoding holds.
fn load_text(path: &Path) -> Option<TextDoc> {
    let bytes = std::fs::read(path).ok()?;
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    if !utf16 && bytes.contains(&0) {
        return None;
    }
    let doc = text::doc_from_bytes(Some(path.to_path_buf()), &bytes, None);
    let same = !doc.mixed_eol && text::encode(&doc.text(), doc.encoding, doc.bom) == bytes;
    same.then_some(doc)
}

/// What the merge keeps of a setting both sides may have changed: the changed side's, or
/// nothing when they changed it differently.
fn merged<T: PartialEq + Copy>(left: T, base: T, right: T) -> Option<T> {
    if left == base || left == right {
        Some(right)
    } else if right == base {
        Some(left)
    } else {
        None
    }
}

/// The middle file with both sides' changes, when they are text and do not overlap. The
/// encoding, byte order mark, line ending and final newline are merged too.
fn merge_text(paths: &[PathBuf]) -> Option<TextDoc> {
    let docs: Vec<TextDoc> = paths.iter().map(|p| load_text(p)).collect::<Option<_>>()?;
    let [l, b, r] = [&docs[0], &docs[BASE], &docs[2]];
    let lines = merge3(&l.lines, &b.lines, &r.lines)?;
    let mut doc = b.clone();
    doc.encoding = merged(l.encoding, b.encoding, r.encoding)?;
    doc.bom = merged(l.bom, b.bom, r.bom)?;
    doc.eol = merged(l.eol, b.eol, r.eol)?;
    doc.final_newline = merged(l.final_newline, b.final_newline, r.final_newline)?;
    doc.lines = lines;
    Some(doc)
}

/// Whether what is on disk at the paths of `it` is what the folder compare listed.
fn as_listed(it: &DirItem, kinds: &[Kind; 3]) -> bool {
    kinds.iter().zip(it.exists).all(|(&k, exists)| match k {
        Kind::Missing => !exists,
        Kind::Dir => exists && it.is_dir,
        Kind::File | Kind::Link | Kind::Other => exists && !it.is_dir,
    })
}

/// Items whose name is a file in some folder and a folder in another.
fn type_clashes(items: &[DirItem]) -> HashSet<usize> {
    let mut by_name: HashMap<(Option<usize>, String), Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        by_name
            .entry((it.parent, name_key(&it.name, false).1))
            .or_default()
            .push(i);
    }
    by_name
        .into_values()
        .filter(|v| v.iter().any(|&i| items[i].is_dir) && v.iter().any(|&i| !items[i].is_dir))
        .flatten()
        .collect()
}

/// Decides what the merge does with every item. Files are compared byte for byte: the
/// folder compare may have called them identical under its ignore options. Items inside a
/// folder left as a conflict get no step of their own.
pub fn plan(items: &[DirItem], roots: &[PathBuf]) -> Plan {
    let mut steps: BTreeMap<usize, Step> = BTreeMap::new();
    if roots.len() != 3 {
        return Plan::default();
    }
    let clashes = type_clashes(items);
    let mut stale = Vec::new();
    let mut stamps = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        let covered = std::iter::successors(it.parent, |&p| items[p].parent)
            .any(|p| matches!(steps.get(&p), Some(Step::Conflict)));
        if covered {
            continue;
        }
        // A file and a folder of one name share their paths, so neither is as listed.
        if clashes.contains(&i) {
            steps.insert(i, Step::Conflict);
            continue;
        }
        // Taken before the files are read, so a change while they are is seen as one.
        if !it.is_dir {
            stamps.insert(i, stamp(it, roots));
        }
        let path = |s: usize| it.path(&roots[s]);
        let kinds: [Kind; 3] = std::array::from_fn(|s| kind(&path(s)));
        if !as_listed(it, &kinds) {
            stale.push(i);
            continue;
        }
        let odd = kinds.iter().any(|k| matches!(k, Kind::Link | Kind::Other));
        if odd || it.status == CmpStatus::Error {
            steps.insert(i, Step::Conflict);
            continue;
        }
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
            // removed on both sides: a folder goes once its listed contents went
            [false, true, false] if it.is_dir => Some(Step::Prune),
            [false, true, false] => Some(Step::Delete),
            // added on both sides: their folders' contents are merged item by item
            [true, false, true] if it.is_dir => Some(Step::Copy(0)),
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
    stamps.retain(|i, _| steps.contains_key(i));
    Plan {
        steps: steps.into_iter().collect(),
        stamps,
        stale,
    }
}

/// Whether `target`, the path of an item under `root`, would be written outside it: through
/// a name that is not a plain one, or a folder on the way replaced by a symbolic link.
fn escapes(root: &Path, it: &DirItem, target: &Path) -> bool {
    let plain = Path::new(&it.rel_path())
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !plain {
        return true;
    }
    let Ok(root) = std::fs::canonicalize(root) else {
        return true;
    };
    let mut dir = target.parent();
    while let Some(d) = dir {
        if let Ok(real) = std::fs::canonicalize(d) {
            return !real.starts_with(&root);
        }
        dir = d.parent();
    }
    true
}

/// Carries out the plan, with the setting for backups. Returns how many items changed and
/// the errors, each naming its item.
pub fn apply(
    plan: Plan,
    items: &[DirItem],
    roots: &[PathBuf],
    use_recycle_bin: bool,
) -> (usize, Vec<String>) {
    apply_with(plan, items, roots, use_recycle_bin, fsutil::backup_on())
}

/// [`apply`], keeping the middle files it replaces as `<name>.bak` with `backup`. A file whose
/// `<name>.bak` is itself listed is then left alone.
pub fn apply_with(
    plan: Plan,
    items: &[DirItem],
    roots: &[PathBuf],
    use_recycle_bin: bool,
    backup: bool,
) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut errors = Vec::new();
    if !plan.stale.is_empty() {
        errors.extend(plan.stale.iter().map(|&i| {
            format!(
                "{}: {}",
                items[i].rel_path(),
                tr("msg.changed_since_compare")
            )
        }));
        return (done, errors);
    }
    let listed: HashSet<(Option<usize>, String)> = items
        .iter()
        .map(|it| (it.parent, name_key(&it.name, false).1))
        .collect();
    let mut prune = Vec::new();
    for (i, step) in plan.steps {
        let it = &items[i];
        let target = it.path(&roots[BASE]);
        let bak = (it.parent, name_key(&format!("{}.bak", it.name), false).1);
        let replaces = matches!(step, Step::Copy(_) | Step::Merge(_)) && target.is_file();
        if backup && replaces && listed.contains(&bak) {
            errors.push(format!("{}: {}", it.rel_path(), tr("msg.backup_listed")));
            continue;
        }
        if plan.stamps.get(&i).is_some_and(|s| *s != stamp(it, roots)) {
            errors.push(format!(
                "{}: {}",
                it.rel_path(),
                tr("msg.changed_since_plan")
            ));
            continue;
        }
        if !matches!(step, Step::Conflict) && escapes(&roots[BASE], it, &target) {
            errors.push(format!("{}: {}", it.rel_path(), tr("msg.outside_folder")));
            continue;
        }
        let r = match step {
            Step::Copy(_) if it.is_dir => std::fs::create_dir_all(&target).map_err(Into::into),
            // A folder there now is not replaced: it may hold files the merge does not list.
            Step::Copy(_) if target.is_dir() => Err(anyhow::anyhow!(tr("msg.changed_since_plan"))),
            Step::Copy(from) => fsutil::copy_atomic(&it.path(&roots[from]), &target, backup),
            Step::Delete => delete_path(&target, use_recycle_bin),
            Step::Prune => {
                prune.push(i);
                continue;
            }
            Step::Merge(mut doc) => doc.save_with(backup),
            Step::Conflict => continue,
        };
        match r {
            Ok(()) => done += 1,
            Err(e) => errors.push(format!("{}: {e:#}", it.rel_path())),
        }
    }
    // innermost folders first; a folder still holding files the filter hides stays
    for i in prune.into_iter().rev() {
        match std::fs::remove_dir(items[i].path(&roots[BASE])) {
            Ok(()) => done += 1,
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
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

    /// Three empty folders: left, middle (the common ancestor) and right.
    fn folders() -> (Vec<tempfile::TempDir>, Vec<PathBuf>) {
        let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let roots = dirs.iter().map(|d| d.path().to_path_buf()).collect();
        (dirs, roots)
    }

    fn step_of<'a>(plan: &'a Plan, items: &[DirItem], rel: &str) -> Option<&'a Step> {
        plan.steps
            .iter()
            .find(|(i, _)| items[*i].rel_path() == rel)
            .map(|(_, s)| s)
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
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
        assert_eq!(read(b, "left_changed.txt"), "left\n");
        assert_eq!(read(b, "spaces.txt"), "a  b\n");
        assert_eq!(read(b, "both.txt"), "one\n2\n3\n4\nfive\n");
        assert_eq!(read(b, "conflict.txt"), "x\n");
        assert_eq!(read(b, "new/added.txt"), "new\n");
        assert!(!b.join("removed.txt").exists());
        assert!(!b.join("gone/old.txt").exists());
        assert_eq!(read(b, "gone/hidden.log"), "keep\n");
        assert_eq!(read(b, "only_left/shown.txt"), "copied\n");
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
        assert_eq!(read(b, "late.txt"), "edited meanwhile\n");
    }

    #[test]
    fn text_merges_keep_bytes_and_settings_of_either_side() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        let put = |root: &Path, rel: &str, bytes: &[u8]| {
            std::fs::write(root.join(rel), bytes).unwrap();
        };
        // binary, as Size, Date or Quick contents compares report it: never merged as text
        put(b, "data.bin", b"a\nb\0\nc\nd\ne\n");
        put(l, "data.bin", b"A\nb\0\nc\nd\ne\n");
        put(r, "data.bin", b"a\nb\0\nc\nd\nE\n");
        // the left side's line endings with the right side's edit
        put(b, "eol.txt", b"a\r\nb\r\nc\r\nd\r\ne\r\n");
        put(l, "eol.txt", b"a\nb\nc\nd\ne\n");
        put(r, "eol.txt", b"a\r\nb\r\nc\r\nd\r\nE\r\n");
        // a final newline added on one side, a byte order mark on the other
        put(b, "end.txt", b"a\nb\nc\nd\ne");
        put(l, "end.txt", b"A\nb\nc\nd\ne\n");
        put(r, "end.txt", b"\xEF\xBB\xBFa\nb\nc\nd\nE");
        // mixed line endings would come back as one kind
        put(b, "mixed.txt", b"a\nb\r\nc\nd\ne\n");
        put(l, "mixed.txt", b"A\nb\r\nc\nd\ne\n");
        put(r, "mixed.txt", b"a\nb\r\nc\nd\nE\n");
        // line endings changed differently on both sides
        put(b, "apart.txt", b"a\nb\nc\nd\ne\n");
        put(l, "apart.txt", b"A\r\nb\r\nc\r\nd\r\ne\r\n");
        put(r, "apart.txt", b"a\rb\rc\rd\rE\r");
        // folders added empty on both sides
        for root in [l, r] {
            std::fs::create_dir(root.join("empty")).unwrap();
        }
        let mut items = scan(&roots);
        for it in &mut items {
            it.binary = false;
        }
        let plan = plan(&items, &roots);
        for name in ["data.bin", "mixed.txt", "apart.txt"] {
            assert!(
                matches!(step_of(&plan, &items, name), Some(Step::Conflict)),
                "{name}"
            );
        }
        let (_, errors) = apply_with(plan, &items, &roots, false, false);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            std::fs::read(b.join("data.bin")).unwrap(),
            b"a\nb\0\nc\nd\ne\n"
        );
        assert_eq!(read(b, "eol.txt"), "a\nb\nc\nd\nE\n");
        assert_eq!(
            std::fs::read(b.join("end.txt")).unwrap(),
            b"\xEF\xBB\xBFA\nb\nc\nd\nE\n"
        );
        assert_eq!(read(b, "mixed.txt"), "a\nb\r\nc\nd\ne\n");
        assert_eq!(read(b, "apart.txt"), "a\nb\nc\nd\ne\n");
        assert!(b.join("empty").is_dir());
    }

    #[test]
    fn backups_never_replace_compared_files() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        for root in [l, b, r] {
            write(root, "a.txt.bak", "precious\n");
        }
        write(b, "a.txt", "old\n");
        write(r, "a.txt", "old\n");
        write(l, "a.txt", "new\n");
        let items = scan(&roots);
        let (done, errors) = apply_with(plan(&items, &roots), &items, &roots, false, true);
        assert_eq!((done, errors.len()), (0, 1), "{errors:?}");
        assert_eq!(read(b, "a.txt"), "old\n");
        assert_eq!(read(b, "a.txt.bak"), "precious\n");
        let (done, errors) = apply_with(plan(&items, &roots), &items, &roots, false, false);
        assert_eq!((done, errors.len()), (1, 0), "{errors:?}");
        assert_eq!(read(b, "a.txt"), "new\n");
    }

    #[test]
    fn changes_of_each_side_alone_and_of_both() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        write(l, "added_left.txt", "L\n");
        write(r, "added_right.txt", "R\n");
        for root in [l, r] {
            write(root, "added_same.txt", "same\n");
        }
        write(l, "added_apart.txt", "left\n");
        write(r, "added_apart.txt", "right\n");
        for root in [b, r] {
            write(root, "deleted_left.txt", "base\n");
        }
        write(b, "deleted_both.txt", "base\n");
        write(b, "deleted_changed.txt", "base\n");
        write(r, "deleted_changed.txt", "changed\n");
        write(b, "changed_same.txt", "base\n");
        for root in [l, r] {
            write(root, "changed_same.txt", "new\n");
        }
        // renamed on the left, and to other names on both sides
        for root in [b, r] {
            write(root, "renamed.txt", "moved\n");
        }
        write(l, "renamed_left.txt", "moved\n");
        write(b, "renamed_apart.txt", "body\n");
        write(l, "renamed_l.txt", "body\n");
        write(r, "renamed_r.txt", "body\n");
        // the same bytes, written at other times
        for root in [l, b, r] {
            write(root, "touched.txt", "same\n");
        }
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(l.join("touched.txt"))
            .unwrap()
            .set_modified(past)
            .unwrap();

        let items = scan(&roots);
        let plan = plan(&items, &roots);
        assert!(plan.stale.is_empty());
        let conflicts: Vec<String> = plan
            .items_where(|s| matches!(s, Step::Conflict))
            .into_iter()
            .map(|i| items[i].rel_path())
            .collect();
        assert_eq!(conflicts, ["added_apart.txt", "deleted_changed.txt"]);
        assert!(step_of(&plan, &items, "touched.txt").is_none());
        let (_, errors) = apply(plan, &items, &roots, false);
        assert!(errors.is_empty(), "{errors:?}");
        let mut names: Vec<String> = std::fs::read_dir(b)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "added_left.txt",
                "added_right.txt",
                "added_same.txt",
                "changed_same.txt",
                "deleted_changed.txt",
                "renamed_l.txt",
                "renamed_left.txt",
                "renamed_r.txt",
                "touched.txt",
            ]
        );
        // a conflict keeps the middle as it was; nothing of either side is lost
        assert_eq!(read(b, "deleted_changed.txt"), "base\n");
        assert_eq!(read(l, "added_apart.txt"), "left\n");
        assert_eq!(read(r, "added_apart.txt"), "right\n");
        assert_eq!(read(b, "changed_same.txt"), "new\n");
    }

    #[test]
    fn binary_files_and_nested_and_empty_folders() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        let bin =
            |root: &Path, rel: &str, bytes: &[u8]| std::fs::write(root.join(rel), bytes).unwrap();
        bin(b, "both.bin", b"\0base");
        bin(l, "both.bin", b"\0left");
        bin(r, "both.bin", b"\0right");
        bin(b, "one.bin", b"\0base");
        bin(l, "one.bin", b"\0base");
        bin(r, "one.bin", b"\0right");
        write(r, "deep/er/still/x.txt", "x\n");
        std::fs::create_dir_all(l.join("empty")).unwrap();
        // removed on both sides: the listed file goes, the hidden one and its folder stay
        write(b, "dropped/a.txt", "a\n");
        write(b, "dropped/keep.log", "log\n");
        write(b, "dropped/inner/b.txt", "b\n");

        let items = scan(&roots);
        let plan = plan(&items, &roots);
        assert!(matches!(
            step_of(&plan, &items, "both.bin"),
            Some(Step::Conflict)
        ));
        let (_, errors) = apply(plan, &items, &roots, false);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(std::fs::read(b.join("both.bin")).unwrap(), b"\0base");
        assert_eq!(std::fs::read(b.join("one.bin")).unwrap(), b"\0right");
        assert_eq!(read(b, "deep/er/still/x.txt"), "x\n");
        assert!(b.join("empty").is_dir());
        assert!(!b.join("dropped/a.txt").exists() && !b.join("dropped/inner").exists());
        assert_eq!(read(b, "dropped/keep.log"), "log\n");
    }

    #[test]
    fn a_file_and_a_folder_of_one_name_are_left_alone() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        for root in [b, r] {
            write(root, "x", "file\n");
        }
        write(l, "x/inside.txt", "now a folder\n");
        for root in [l, b, r] {
            write(root, "y/kept.txt", "folder\n");
        }
        std::fs::remove_dir_all(r.join("y")).unwrap();
        write(r, "y", "now a file\n");

        let items = scan(&roots);
        let plan = plan(&items, &roots);
        assert!(plan.stale.is_empty());
        assert_eq!(plan.changes(), 0, "{:?}", plan.counts());
        assert_eq!(plan.counts().conflicts, 4);
        let (_, errors) = apply(plan, &items, &roots, false);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read(b, "x"), "file\n");
        assert_eq!(read(b, "y/kept.txt"), "folder\n");
    }

    #[test]
    fn results_older_than_the_files_stop_the_merge() {
        let (_dirs, roots) = folders();
        let [l, b] = [&roots[0], &roots[1]];
        write(l, "new.txt", "left\n");
        write(l, "other.txt", "left\n");
        let items = scan(&roots);
        // made in the middle after the compare: the left one is not copied over it
        write(b, "new.txt", "mine\n");
        let plan = plan(&items, &roots);
        assert_eq!(plan.stale.len(), 1);
        let (done, errors) = apply(plan, &items, &roots, false);
        assert_eq!((done, errors.len()), (0, 1), "{errors:?}");
        assert!(errors[0].starts_with("new.txt"));
        assert_eq!(read(b, "new.txt"), "mine\n");
        assert!(!b.join("other.txt").exists());
    }

    #[test]
    fn errors_of_the_compare_are_conflicts() {
        let (_dirs, roots) = folders();
        for root in &roots {
            write(root, "a.txt", "a\n");
        }
        write(&roots[0], "a.txt", "changed\n");
        let mut items = scan(&roots);
        items[0].status = CmpStatus::Error;
        let plan = plan(&items, &roots);
        assert_eq!(plan.counts().conflicts, 1);
        assert_eq!(plan.changes(), 0);
    }

    #[test]
    fn names_that_leave_the_folder_are_refused() {
        let (_dirs, roots) = folders();
        write(&roots[0], "x.txt", "x\n");
        let mut items = scan(&roots);
        let target = items[0].path(&roots[BASE]);
        assert!(!escapes(&roots[BASE], &items[0], &target));
        items[0].rel_dir = "..".into();
        let target = items[0].path(&roots[BASE]);
        assert!(escapes(&roots[BASE], &items[0], &target));
    }

    #[test]
    fn replaced_files_are_kept_as_backups() {
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        for root in [b, r] {
            write(root, "a.txt", "old\n");
        }
        write(l, "a.txt", "new\n");
        let mut perm = std::fs::metadata(b.join("a.txt")).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(b.join("a.txt"), perm).unwrap();
        let items = scan(&roots);
        let plan = plan(&items, &roots);
        let (done, errors) = apply_with(plan, &items, &roots, false, true);
        assert_eq!((done, errors.len()), (1, 0), "{errors:?}");
        assert_eq!(read(b, "a.txt"), "new\n");
        assert_eq!(read(b, "a.txt.bak"), "old\n");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed() {
        use std::os::unix::fs::symlink;
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "outside\n").unwrap();
        symlink(&secret, b.join("link.txt")).unwrap();
        symlink(&secret, r.join("link.txt")).unwrap();
        write(l, "link.txt", "replaced\n");
        for root in [l, b, r] {
            write(root, "sub/same.txt", "s\n");
        }
        write(l, "sub/new.txt", "new\n");

        let items = scan(&roots);
        let plan = plan(&items, &roots);
        assert!(matches!(
            step_of(&plan, &items, "link.txt"),
            Some(Step::Conflict)
        ));
        // a folder of the middle turned into a link to elsewhere after planning
        std::fs::remove_dir_all(b.join("sub")).unwrap();
        symlink(outside.path(), b.join("sub")).unwrap();
        let (done, errors) = apply(plan, &items, &roots, false);
        assert_eq!(done, 0);
        assert!(
            errors.iter().any(|e| e.starts_with("sub/new.txt")),
            "{errors:?}"
        );
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "outside\n");
        assert!(!outside.path().join("new.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_step_is_reported_and_the_others_go_on() {
        use std::os::unix::fs::PermissionsExt;
        let (_dirs, roots) = folders();
        let [l, b, r] = [&roots[0], &roots[1], &roots[2]];
        for root in [b, r] {
            write(root, "locked/a.txt", "old\n");
            write(root, "b.txt", "old\n");
        }
        write(l, "locked/a.txt", "old\n");
        write(l, "locked/new.txt", "new\n");
        write(l, "b.txt", "new\n");
        let items = scan(&roots);
        let plan = plan(&items, &roots);
        let locked = b.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root may write anyway; then there is nothing to test.
        let writable = std::fs::write(locked.join("probe"), "").is_ok();
        let (done, errors) = apply(plan, &items, &roots, false);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            return;
        }
        assert_eq!(done, 1);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("locked/new.txt"), "{errors:?}");
        assert!(!b.join("locked/new.txt").exists());
        assert_eq!(read(b, "b.txt"), "new\n");
    }
}
