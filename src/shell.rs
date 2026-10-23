//! Context menu integration in the style of WinMerge's shell extension.
//!
//! On Windows, `register` adds an "OxideMerge" submenu to Explorer for files, folders, drives and
//! folder backgrounds, for the current user:
//! - **Compare** compares the two or three selected items, or one item with the side or sides
//!   chosen earlier with Select Left and Select Middle. With nothing chosen, it opens the Open
//!   dialog with the item filled in.
//! - **Select Left** and **Select Middle** remember an item for a later Compare, whose label then
//!   names it.
//! - **Compare As** (files only) compares with the file type forced.
//!
//! Explorer starts one process per selected item; `gather` lets them find each other.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Paths remembered by Select Left and Select Middle. They are kept apart from the options,
/// which a window that is already open would overwrite when it closes.
#[derive(Default)]
pub struct Selection {
    pub left: Option<PathBuf>,
    pub middle: Option<PathBuf>,
}

fn selection_file() -> Option<PathBuf> {
    crate::config::config_path()?
        .parent()
        .map(|d| d.join("shell-selection.txt"))
}

impl Selection {
    pub fn load() -> Selection {
        let text = selection_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let mut lines = text
            .lines()
            .map(|l| (!l.is_empty()).then(|| PathBuf::from(l)));
        Selection {
            left: lines.next().flatten(),
            middle: lines.next().flatten(),
        }
    }

    pub fn save(&self) {
        if let Some(p) = selection_file() {
            if self.left.is_none() && self.middle.is_none() {
                let _ = std::fs::remove_file(p);
            } else {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let line = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
                let text = format!(
                    "{}\n{}\n",
                    line(&self.left).unwrap_or_default(),
                    line(&self.middle).unwrap_or_default()
                );
                let _ = std::fs::write(p, text);
            }
        }
        imp::update_labels(self);
    }

    /// Select Middle without a left side selects the left side.
    pub fn select(&mut self, path: PathBuf, middle: bool) {
        if middle && self.left.is_some() {
            self.middle = Some(path);
        } else {
            self.left = Some(path);
            self.middle = None;
        }
    }

    /// Label of the Compare entry, naming the remembered sides.
    pub fn compare_label(&self) -> String {
        use crate::i18n::{tr, trf};
        let name = |p: &PathBuf| {
            p.file_name()
                .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
        };
        match (&self.left, &self.middle) {
            (Some(l), Some(m)) => trf(
                "shell.compare_to_both",
                &[("left", name(l)), ("middle", name(m))],
            ),
            (Some(l), None) => trf("shell.compare_to", &[("name", name(l))]),
            _ => tr("shell.compare").to_string(),
        }
    }
}

/// The paths a context menu Compare opens: the selected items, or a single item after the
/// remembered left (and middle) side, which are then forgotten.
pub fn compare_paths(mut items: Vec<PathBuf>) -> Vec<PathBuf> {
    items.truncate(3);
    if items.len() != 1 {
        return items;
    }
    let selection = Selection::load();
    let Some(left) = selection.left else {
        return items;
    };
    let mut paths = vec![left];
    paths.extend(selection.middle);
    paths.extend(items);
    Selection::default().save();
    paths
}

/// Repairs a path Explorer passed as `"%1"`: for a drive root, `"C:\"` reaches the program as
/// `C:"`, because a backslash before a quote escapes it.
pub fn clean_arg(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let s = s.trim_end_matches('"');
    if s.len() == 2 && s.ends_with(':') {
        PathBuf::from(format!("{s}\\"))
    } else {
        PathBuf::from(s)
    }
}

/// Explorer starts one process per selected item. Each leaves its path in a spool folder; the
/// first becomes the leader, waits until no more paths arrive, and gets them all in the order
/// they came. The others get `None` and should exit.
pub fn gather(path: &Path) -> Option<Vec<PathBuf>> {
    gather_in(
        &std::env::temp_dir().join("oxidemerge-shell"),
        path,
        Duration::from_millis(400),
    )
}

const LOCK: &str = "leader.lock";
/// Paths and locks older than this were left behind by a process that did not finish.
const STALE: Duration = Duration::from_secs(20);

fn gather_in(dir: &Path, path: &Path, quiet: Duration) -> Option<Vec<PathBuf>> {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let alone = || Some(vec![path.to_path_buf()]);
    if std::fs::create_dir_all(dir).is_err() {
        return alone();
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mine = dir.join(format!("{stamp:032}-{}-{seq}.path", std::process::id()));
    if std::fs::write(&mine, path.to_string_lossy().as_bytes()).is_err() {
        return alone();
    }
    let lock = dir.join(LOCK);
    if !take_lock(&lock) {
        return None;
    }
    let start = Instant::now();
    let mut seen = 0;
    let mut changed = Instant::now();
    while changed.elapsed() < quiet && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(40));
        let n = pending(dir).len();
        if n != seen {
            seen = n;
            changed = Instant::now();
        }
    }
    let _ = std::fs::remove_file(&lock);
    // A path that arrives after this is picked up by its own process, which finds no lock.
    // Renaming claims each path for exactly one leader.
    let mut paths = Vec::new();
    for file in pending(dir) {
        let claimed = file.with_extension(format!("claimed-{}-{seq}", std::process::id()));
        if std::fs::rename(&file, &claimed).is_ok() {
            if let Ok(text) = std::fs::read_to_string(&claimed) {
                paths.push(PathBuf::from(text));
            }
            let _ = std::fs::remove_file(&claimed);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

fn take_lock(lock: &Path) -> bool {
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock)
            .is_ok()
    };
    if create() {
        return true;
    }
    let stale = std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > STALE));
    stale && std::fs::remove_file(lock).is_ok() && create()
}

/// Waiting paths, oldest first. Stale ones are deleted.
fn pending(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "path"))
        .filter(|p| {
            let stale = std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > STALE));
            if stale {
                let _ = std::fs::remove_file(p);
            }
            !stale
        })
        .collect();
    files.sort();
    files
}

#[cfg(windows)]
mod imp {
    use super::Selection;
    use crate::i18n::tr;
    use anyhow::Result;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    const KEY: &str = "OxideMerge";
    /// Flat entries written by earlier versions.
    const LEGACY: &[&str] = &[
        "OxideMerge.SelectLeft",
        "OxideMerge.CompareTo",
        "OxideMerge.Open",
    ];
    /// Where the submenu appears, the placeholder Explorer replaces with the item, and whether
    /// Compare As is offered.
    const ROOTS: &[(&str, &str, bool)] = &[
        ("Software\\Classes\\*\\shell", "%1", true),
        ("Software\\Classes\\Directory\\shell", "%1", false),
        ("Software\\Classes\\Drive\\shell", "%1", false),
        (
            "Software\\Classes\\Directory\\Background\\shell",
            "%V",
            false,
        ),
    ];
    const KINDS: &[(&str, &str)] = &[
        ("text", "kind.text"),
        ("binary", "kind.binary"),
        ("image", "kind.image"),
        ("web", "kind.web"),
    ];
    const SEPARATOR_BEFORE: u32 = 0x20;

    pub fn register() -> Result<()> {
        unregister()?;
        let exe = std::env::current_exe()?.display().to_string();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let selection = Selection::load();
        for (root, item, kinds) in ROOTS {
            let (menu, _) = hkcu.create_subkey(format!("{root}\\{KEY}"))?;
            submenu(&menu, "OxideMerge", 0)?;
            menu.set_value("Icon", &exe)?;
            let verb = |parent: &RegKey, name: &str, label: &str, args: &str, flags: u32| {
                let (k, _) = parent.create_subkey(format!("shell\\{name}"))?;
                k.set_value("MUIVerb", &label)?;
                if flags != 0 {
                    k.set_value("CommandFlags", &flags)?;
                }
                let (cmd, _) = k.create_subkey("command")?;
                cmd.set_value("", &format!("\"{exe}\" {args} \"{item}\""))?;
                anyhow::Ok(())
            };
            verb(
                &menu,
                "1Compare",
                &selection.compare_label(),
                "--shell-compare",
                0,
            )?;
            verb(
                &menu,
                "2SelectLeft",
                tr("shell.select_left"),
                "--select-left",
                SEPARATOR_BEFORE,
            )?;
            verb(
                &menu,
                "3SelectMiddle",
                tr("shell.select_middle"),
                "--select-middle",
                0,
            )?;
            if *kinds {
                let (sub, _) = menu.create_subkey("shell\\4CompareAs")?;
                submenu(&sub, tr("cmd.compare_as"), SEPARATOR_BEFORE)?;
                for (i, (kind, label)) in KINDS.iter().enumerate() {
                    let name = format!("{}{kind}", i + 1);
                    let args = format!("--compare-as {kind} --shell-compare");
                    verb(&sub, &name, tr(label), &args, 0)?;
                }
            }
        }
        Ok(())
    }

    /// Makes `key` a cascading menu entry whose items are in its `shell` subkey.
    fn submenu(key: &RegKey, label: &str, flags: u32) -> Result<()> {
        key.set_value("MUIVerb", &label)?;
        key.set_value("SubCommands", &"")?;
        if flags != 0 {
            key.set_value("CommandFlags", &flags)?;
        }
        Ok(())
    }

    pub fn unregister() -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (root, _, _) in ROOTS {
            let _ = hkcu.delete_subkey_all(format!("{root}\\{KEY}"));
            for key in LEGACY {
                let _ = hkcu.delete_subkey_all(format!("{root}\\{key}"));
            }
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!("{}\\{KEY}", ROOTS[0].0))
            .is_ok()
    }

    /// Renames the Compare entries after the remembered sides, when the menu is registered.
    pub fn update_labels(selection: &Selection) {
        let label = selection.compare_label();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for (root, _, _) in ROOTS {
            if let Ok(k) = hkcu
                .open_subkey_with_flags(format!("{root}\\{KEY}\\shell\\1Compare"), KEY_SET_VALUE)
            {
                let _ = k.set_value("MUIVerb", &label);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Selection;
    use anyhow::{bail, Result};

    pub fn register() -> Result<()> {
        bail!("shell integration is only available on Windows")
    }

    pub fn unregister() -> Result<()> {
        Ok(())
    }

    pub fn is_registered() -> bool {
        false
    }

    pub fn update_labels(_: &Selection) {}
}

pub use imp::{is_registered, register, unregister};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gathers_the_items_of_one_selection() {
        let dir = tempfile::tempdir().unwrap();
        let quiet = Duration::from_millis(300);
        let results: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = ["a.txt", "b.txt", "c.txt"]
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let dir = dir.path();
                    s.spawn(move || {
                        std::thread::sleep(Duration::from_millis(30 * i as u64));
                        gather_in(dir, Path::new(name), quiet)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let leaders: Vec<_> = results.into_iter().flatten().collect();
        assert_eq!(
            leaders,
            vec![vec![
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("c.txt")
            ]]
        );
        // A later selection starts afresh.
        assert_eq!(
            gather_in(dir.path(), Path::new("d.txt"), quiet),
            Some(vec![PathBuf::from("d.txt")])
        );
    }

    #[test]
    fn repairs_drive_roots() {
        assert_eq!(clean_arg(Path::new("C:\"")), PathBuf::from("C:\\"));
        assert_eq!(
            clean_arg(Path::new("C:\\dir\\file.txt")),
            PathBuf::from("C:\\dir\\file.txt")
        );
    }

    #[test]
    fn select_middle_needs_a_left_side() {
        let mut s = Selection::default();
        s.select(PathBuf::from("m"), true);
        assert_eq!(s.left, Some(PathBuf::from("m")));
        s.select(PathBuf::from("n"), true);
        assert_eq!(s.middle, Some(PathBuf::from("n")));
        s.select(PathBuf::from("l"), false);
        assert_eq!((s.left, s.middle), (Some(PathBuf::from("l")), None));
    }
}
