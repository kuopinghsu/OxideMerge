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
//!
//! On macOS `register` writes Finder Quick Actions to `~/Library/Services`, and on Linux it adds
//! entries for Nautilus, Nemo and Dolphin. Those file managers pass all selected items to one
//! process, which runs with `--compare-selected`.

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
    #[cfg(windows)]
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

/// The entries offered by file managers other than Explorer: name, label key, option, and
/// whether the option takes all selected items (else only the first).
#[cfg(not(windows))]
const ENTRIES: &[(&str, &str, &str, bool)] = &[
    ("Compare", "shell.compare", "--compare-selected", true),
    ("Select Left", "shell.select_left", "--select-left", false),
    (
        "Select Middle",
        "shell.select_middle",
        "--select-middle",
        false,
    ),
];

/// Quotes `s` for a POSIX shell.
#[cfg(not(windows))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn write_executable(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Finder Quick Actions: one Automator workflow per entry in `~/Library/Services`.
#[cfg(target_os = "macos")]
mod imp {
    use super::{sh_quote, Selection, ENTRIES};
    use crate::i18n::tr;
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};

    fn services_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Services"))
    }

    fn bundle(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("OxideMerge - {name}.workflow"))
    }

    fn xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    pub fn register() -> Result<()> {
        let dir = services_dir().context("HOME is not set")?;
        register_in(&dir, &std::env::current_exe()?)?;
        // Makes the new entries show up without logging out.
        let _ = std::process::Command::new("/System/Library/CoreServices/pbs")
            .arg("-update")
            .status();
        Ok(())
    }

    pub(super) fn register_in(dir: &Path, exe: &Path) -> Result<()> {
        unregister_in(dir);
        for (i, (name, label, option, all)) in ENTRIES.iter().enumerate() {
            let contents = bundle(dir, name).join("Contents");
            std::fs::create_dir_all(&contents)?;
            let label = format!("OxideMerge: {}", tr(label));
            std::fs::write(contents.join("Info.plist"), info_plist(&label))?;
            let items = if *all { "\"$@\"" } else { "\"$1\"" };
            let command = format!(
                "nohup {} {option} {items} >/dev/null 2>&1 &",
                sh_quote(&exe.display().to_string())
            );
            std::fs::write(contents.join("document.wflow"), workflow(&command, i))?;
        }
        Ok(())
    }

    fn unregister_in(dir: &Path) {
        for (name, ..) in ENTRIES {
            let _ = std::fs::remove_dir_all(bundle(dir, name));
        }
    }

    pub fn unregister() -> Result<()> {
        if let Some(dir) = services_dir() {
            unregister_in(&dir);
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        services_dir().is_some_and(|d| bundle(&d, ENTRIES[0].0).exists())
    }

    pub fn update_labels(_: &Selection) {}

    fn info_plist(label: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSIconName</key>
			<string>NSActionTemplate</string>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>{}</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
				<string>public.item</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
"#,
            xml(label)
        )
    }

    /// A workflow with one "Run Shell Script" action that gets the selected items as arguments.
    fn workflow(command: &str, n: usize) -> String {
        let uuid = |k: usize| format!("7F3C9A10-0D2B-4E5A-9C1D-{:012X}", n * 3 + k);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>523</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{command}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/sh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>{input}</string>
				<key>Keywords</key>
				<array>
					<string>Shell</string>
					<string>Script</string>
				</array>
				<key>OutputUUID</key>
				<string>{output}</string>
				<key>UUID</key>
				<string>{id}</string>
				<key>UnlocalizedApplications</key>
				<array>
					<string>Automator</string>
				</array>
				<key>isViewVisible</key>
				<integer>1</integer>
				<key>location</key>
				<string>309.000000:253.000000</string>
				<key>nibPath</key>
				<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>applicationBundleIDsByPath</key>
		<dict/>
		<key>applicationPaths</key>
		<array/>
		<key>inputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>outputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>presentationMode</key>
		<integer>15</integer>
		<key>processesInput</key>
		<false/>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<false/>
		<key>systemImageName</key>
		<string>NSActionTemplate</string>
		<key>useAutomaticInputType</key>
		<false/>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#,
            command = xml(command),
            input = uuid(0),
            output = uuid(1),
            id = uuid(2),
        )
    }
}

/// Linux and BSD desktops: Nautilus scripts, Nemo actions and a Dolphin service menu, for the
/// file managers that are installed.
#[cfg(all(not(windows), not(target_os = "macos")))]
mod imp {
    use super::{sh_quote, write_executable, Selection, ENTRIES};
    use crate::i18n::tr;
    use anyhow::{bail, Context, Result};
    use std::path::{Path, PathBuf};

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub(super) enum Manager {
        Nautilus,
        Nemo,
        Dolphin,
    }

    const MANAGERS: &[(Manager, &str)] = &[
        (Manager::Nautilus, "nautilus"),
        (Manager::Nemo, "nemo"),
        (Manager::Dolphin, "dolphin"),
    ];

    fn data_dir() -> Option<PathBuf> {
        match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
            Some(d) => Some(PathBuf::from(d)),
            None => Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/share")),
        }
    }

    fn installed() -> Vec<Manager> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        MANAGERS
            .iter()
            .filter(|(_, bin)| std::env::split_paths(&path).any(|d| d.join(bin).is_file()))
            .map(|(m, _)| *m)
            .collect()
    }

    fn nautilus_dir(data: &Path) -> PathBuf {
        data.join("nautilus/scripts/OxideMerge")
    }

    fn nemo_action(data: &Path, i: usize) -> PathBuf {
        data.join(format!("nemo/actions/oxidemerge-{i}.nemo_action"))
    }

    fn dolphin_menu(data: &Path) -> PathBuf {
        data.join("kio/servicemenus/oxidemerge.desktop")
    }

    /// Quotes an argument of a desktop entry's `Exec` key.
    fn exec_quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    pub fn register() -> Result<()> {
        let managers = installed();
        if managers.is_empty() {
            bail!("no supported file manager (Nautilus, Nemo or Dolphin) was found");
        }
        let data = data_dir().context("HOME is not set")?;
        register_in(&data, &std::env::current_exe()?, &managers)
    }

    pub(super) fn register_in(data: &Path, exe: &Path, managers: &[Manager]) -> Result<()> {
        unregister_in(data);
        let exe = exe.display().to_string();
        for m in managers {
            match m {
                Manager::Nautilus => {
                    for (_, label, option, all) in ENTRIES {
                        let items = if *all { "\"$@\"" } else { "\"$1\"" };
                        let script =
                            format!("#!/bin/sh\nexec {} {option} {items}\n", sh_quote(&exe));
                        let name = tr(label).replace('/', "-");
                        write_executable(&nautilus_dir(data).join(name), &script)?;
                    }
                }
                Manager::Nemo => {
                    for (i, (_, label, option, all)) in ENTRIES.iter().enumerate() {
                        let selection = if *all { "notnone" } else { "s" };
                        let text = format!(
                            "[Nemo Action]\nName=OxideMerge: {}\nExec={} {option} %F\n\
                             Icon-Name=oxidemerge\nSelection={selection}\nExtensions=any;\n",
                            tr(label),
                            exec_quote(&exe)
                        );
                        let path = nemo_action(data, i);
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        std::fs::write(path, text)?;
                    }
                }
                Manager::Dolphin => {
                    let ids: Vec<String> = (0..ENTRIES.len()).map(|i| format!("om{i}")).collect();
                    let mut text = format!(
                        "[Desktop Entry]\nType=Service\nMimeType=all/all;\n\
                         X-KDE-ServiceTypes=KonqPopupMenu/Plugin\nX-KDE-Submenu=OxideMerge\n\
                         Actions={};\n",
                        ids.join(";")
                    );
                    for (id, (_, label, option, all)) in ids.iter().zip(ENTRIES) {
                        let code = if *all { "%F" } else { "%f" };
                        text.push_str(&format!(
                            "\n[Desktop Action {id}]\nName={}\nIcon=oxidemerge\nExec={} {option} {code}\n",
                            tr(label),
                            exec_quote(&exe)
                        ));
                    }
                    write_executable(&dolphin_menu(data), &text)?;
                }
            }
        }
        Ok(())
    }

    fn unregister_in(data: &Path) {
        let _ = std::fs::remove_dir_all(nautilus_dir(data));
        for i in 0..ENTRIES.len() {
            let _ = std::fs::remove_file(nemo_action(data, i));
        }
        let _ = std::fs::remove_file(dolphin_menu(data));
    }

    pub fn unregister() -> Result<()> {
        if let Some(data) = data_dir() {
            unregister_in(&data);
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        data_dir().is_some_and(|d| {
            nautilus_dir(&d).exists() || nemo_action(&d, 0).exists() || dolphin_menu(&d).exists()
        })
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
        // One leader gathers them all; a busy machine may start the threads in another order.
        let mut leaders: Vec<_> = results.into_iter().flatten().collect();
        leaders.iter_mut().for_each(|paths| paths.sort());
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

    #[cfg(target_os = "macos")]
    #[test]
    fn writes_finder_quick_actions() {
        let dir = tempfile::tempdir().unwrap();
        imp::register_in(dir.path(), Path::new("/Apps/It's here/oxidemerge")).unwrap();
        let wf = dir.path().join("OxideMerge - Compare.workflow/Contents");
        let doc = std::fs::read_to_string(wf.join("document.wflow")).unwrap();
        assert!(doc.contains(
            r#"nohup '/Apps/It'\''s here/oxidemerge' --compare-selected "$@" &gt;/dev/null 2&gt;&amp;1 &amp;"#
        ));
        assert!(std::fs::read_to_string(wf.join("Info.plist"))
            .unwrap()
            .contains("com.apple.finder"));
        assert!(dir
            .path()
            .join("OxideMerge - Select Left.workflow/Contents/document.wflow")
            .exists());
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn writes_linux_file_manager_entries() {
        use imp::Manager;
        let dir = tempfile::tempdir().unwrap();
        let all = [Manager::Nautilus, Manager::Nemo, Manager::Dolphin];
        imp::register_in(dir.path(), Path::new("/opt/om/oxidemerge"), &all).unwrap();
        let d = dir.path();
        let script =
            std::fs::read_to_string(d.join("nautilus/scripts/OxideMerge/Compare")).unwrap();
        assert!(script.contains("exec '/opt/om/oxidemerge' --compare-selected \"$@\""));
        let nemo =
            std::fs::read_to_string(d.join("nemo/actions/oxidemerge-1.nemo_action")).unwrap();
        assert!(
            nemo.contains("Exec=\"/opt/om/oxidemerge\" --select-left %F")
                && nemo.contains("Selection=s")
        );
        let kde = std::fs::read_to_string(d.join("kio/servicemenus/oxidemerge.desktop")).unwrap();
        assert!(kde.contains("Actions=om0;om1;om2;") && kde.contains("--compare-selected %F"));
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
