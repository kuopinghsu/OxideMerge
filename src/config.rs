//! Persistent user options.

use crate::core::diff::DiffOptions;
use crate::core::folder::FolderOptions;
use crate::core::patch::PatchStyle;
use crate::core::unpacker::Unpacker;
use crate::core::web::WebMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    pub fn key(self) -> &'static str {
        match self {
            ThemeMode::System => "theme.system",
            ThemeMode::Light => "theme.light",
            ThemeMode::Dark => "theme.dark",
        }
    }
}

/// Visual row spacing; comparison and merge rules are independent of this preference.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum TextLayout {
    #[default]
    Compact,
    Aligned,
}

impl TextLayout {
    pub const ALL: [Self; 2] = [Self::Compact, Self::Aligned];
    pub fn key(self) -> &'static str {
        match self {
            Self::Compact => "view.layout_compact",
            Self::Aligned => "view.layout_aligned",
        }
    }
}

/// A tab of the previous session: what was compared, and how.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct SessionTab {
    /// The paths as text, to show; opened through [`SessionTab::paths`].
    pub paths: Vec<String>,
    /// Where a path is not valid Unicode, which text cannot hold: for each of `paths`, the path
    /// exactly as the system has it, `None` for one `paths` holds exactly. Empty when none is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<Option<String>>,
    /// "text", "binary", "image" or "web"; none for folder compares.
    pub kind: Option<String>,
    /// Compare rules file of the tab when it has its own ("" = none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
}

impl SessionTab {
    pub fn new(paths: &[PathBuf], kind: Option<String>, rules: Option<String>) -> SessionTab {
        let raw: Vec<Option<String>> = paths
            .iter()
            .map(|p| p.to_str().is_none().then(|| encode_path(p)).flatten())
            .collect();
        SessionTab {
            paths: paths.iter().map(|p| p.display().to_string()).collect(),
            raw: if raw.iter().any(Option::is_some) {
                raw
            } else {
                Vec::new()
            },
            kind,
            rules,
        }
    }

    /// The paths compared, exact also where they are not valid Unicode. One written by another
    /// kind of system, which cannot name it, is taken as its text.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                self.raw
                    .get(i)
                    .and_then(|r| r.as_deref())
                    .and_then(decode_path)
                    .unwrap_or_else(|| PathBuf::from(p))
            })
            .collect()
    }
}

/// `path` as text: its bytes in hex after `u:` on Unix, its UTF-16 units after `w:` on Windows.
fn encode_path(path: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        Some(
            std::iter::once("u:".to_string())
                .chain(bytes.iter().map(|b| format!("{b:02x}")))
                .collect(),
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units = path.as_os_str().encode_wide();
        Some(
            std::iter::once("w:".to_string())
                .chain(units.map(|u| format!("{u:04x}")))
                .collect(),
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// The path [`encode_path`] wrote as `text` on this kind of system; `None` for another kind.
fn decode_path(text: &str) -> Option<PathBuf> {
    let units = |hex: &str, width: usize| -> Option<Vec<u32>> {
        if !hex.len().is_multiple_of(width) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        (0..hex.len())
            .step_by(width)
            .map(|i| u32::from_str_radix(&hex[i..i + width], 16).ok())
            .collect()
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bytes = units(text.strip_prefix("u:")?, 2)?;
        Some(PathBuf::from(std::ffi::OsString::from_vec(
            bytes.into_iter().map(|b| b as u8).collect(),
        )))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let wide = units(text.strip_prefix("w:")?, 4)?;
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            &wide.into_iter().map(|u| u as u16).collect::<Vec<_>>(),
        )))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (text, units);
        None
    }
}

/// A named, saved set of tabs, for `File → Workspaces`: unlike [`Config::session`] (the one,
/// unnamed set the app reopens on its own at startup), a workspace is kept until the user
/// deletes it and is only reopened when asked to by name.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Workspace {
    pub name: String,
    pub tabs: Vec<SessionTab>,
}

impl Workspace {
    /// Whether `name` names this workspace: names are told apart regardless of case.
    pub fn is_named(&self, name: &str) -> bool {
        self.name.to_lowercase() == name.trim().to_lowercase()
    }
}

/// The workspaces to save, when this instance changed `base` (what it read) into `ours` while
/// another running OxideMerge may have saved `theirs` meanwhile: theirs stand, except for the
/// workspaces changed here, which are replaced, added or deleted as they were here. Names
/// match whatever their case, as [`Workspace::is_named`] does.
pub fn merge_workspaces(
    base: &[Workspace],
    ours: &[Workspace],
    theirs: &[Workspace],
) -> Vec<Workspace> {
    let find = |list: &[Workspace], name: &str| list.iter().find(|w| w.is_named(name)).cloned();
    let mut out = theirs.to_vec();
    let mut names: Vec<&str> = Vec::new();
    for w in base.iter().chain(ours) {
        if !names.iter().any(|&n| w.is_named(n)) {
            names.push(&w.name);
        }
    }
    for name in names {
        let ours = find(ours, name);
        if find(base, name) == ours {
            continue;
        }
        match (ours, out.iter().position(|w| w.is_named(name))) {
            (Some(w), Some(at)) => out[at] = w,
            (Some(w), None) => out.push(w),
            (None, Some(at)) => {
                out.remove(at);
            }
            (None, None) => {}
        }
    }
    out
}

/// The workspaces in the settings file now, as another running OxideMerge may have saved
/// them; `None` when the file can't be read, so nothing is merged from it. Unit tests share
/// one settings folder, so they never read it.
pub fn saved_workspaces() -> Option<Vec<Workspace>> {
    if cfg!(test) {
        return None;
    }
    let p = config_path()?;
    match std::fs::read(&p) {
        Ok(bytes) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(Config::parse)
            .map(|c| c.workspaces),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    }
}

/// How to reach a server connected to before over SFTP or FTP, as the connection dialog's
/// fields hold it: everything but the secrets. The password, a key's passphrase and a proxy's
/// password are typed again each time, and are never written to the settings file.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct RecentRemote {
    /// The URL scheme of the protocol: "sftp", "ftp", "ftpes" or "ftps".
    pub protocol: String,
    pub host: String,
    pub port: String,
    pub user: String,
    /// How to sign in: "password", "key" or "agent".
    pub sign_in: String,
    /// The private key file, when signing in with one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key_path: String,
    /// The proxy's URL, without its password.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy: String,
    /// FTP in active mode.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub active: bool,
    /// FTPS: a client certificate, and its key when not in the same file.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_cert: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_key: String,
}

impl RecentRemote {
    /// Whether `other` is the same user on the same server, reached the same way.
    pub fn same_server(&self, other: &RecentRemote) -> bool {
        self.protocol == other.protocol
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.user == other.user
    }

    /// The server as a list shows it: `sftp://ann@example.com:2222`.
    pub fn label(&self) -> String {
        let user = if self.user.is_empty() {
            String::new()
        } else {
            format!("{}@", self.user)
        };
        format!("{}://{user}{}:{}", self.protocol, self.host, self.port)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Language code ("" = follow the system language).
    pub language: String,
    pub theme: ThemeMode,
    /// Whether the dark palette is in use; follows `theme`, and the system when it is `System`.
    pub dark_mode: bool,
    pub font_size: f32,
    pub tab_size: usize,
    pub diff: DiffOptions,
    /// Compare rules file used by default ("" = none); `diff` holds its rules.
    pub rules: String,
    /// Whether the rules of the options of earlier versions were moved to rules files.
    pub rules_migrated: bool,
    /// Whether HTML reports open in the browser once saved.
    pub open_reports: bool,
    /// Whether PDF reports use US Letter pages rather than A4.
    pub pdf_letter: bool,
    /// Whether folder reports name the compared folders by their sides, not their paths.
    pub report_hide_paths: bool,
    /// How much of the differences HTML reports show.
    pub report_detail: crate::core::report::ReportDetail,
    /// Whether saving a file opened from a server writes it over in place, rather than
    /// replacing it through a staged copy (see [`crate::core::remote_stage`]).
    pub remote_in_place: bool,
    pub folder: FolderOptions,
    pub filter: String,
    pub recent_paths: Vec<String>,
    /// Recent comparisons, newest first: the 2 or 3 paths or URLs of each.
    pub recent_compares: Vec<Vec<String>>,
    /// The servers connected to over SFTP or FTP, newest first, to fill in the connection
    /// dialog with. Never a password, a passphrase or a proxy's password.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent_remotes: Vec<RecentRemote>,
    pub recent_filters: Vec<String>,
    pub syntax_highlight: bool,
    /// Files larger than this many MiB (or 3,125 lines a MiB) are shown without syntax
    /// highlighting, which says so in the status bar.
    pub highlight_limit_mb: u32,
    pub show_location_pane: bool,
    /// Diff colors told apart without red and green: blue for added, orange for removed.
    pub color_blind_colors: bool,
    pub high_contrast: bool,
    /// No animated scrolling or pulsing indicators.
    pub reduce_motion: bool,
    /// Tighter spacing and shorter rows.
    pub compact_density: bool,
    pub text_layout: TextLayout,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub show_identical: bool,
    pub show_different: bool,
    pub show_unique: [bool; 3],
    pub tree_mode: bool,
    pub use_recycle_bin: bool,
    pub confirm_operations: bool,
    pub patch_style: PatchStyle,
    pub patch_context: usize,
    pub image_threshold: u8,
    pub image_block: u32,
    pub binary_bytes_per_line: usize,
    /// Binary compares look for blocks of bytes that moved to another offset.
    pub binary_moved_blocks: bool,
    pub unpackers: Vec<Unpacker>,
    pub web_mode: WebMode,
    pub restore_session: bool,
    /// Whether open comparisons and unsaved edits are kept in a recovery snapshot while the
    /// program runs, to offer them again after it ends without closing normally (see
    /// [`crate::core::recovery`]).
    pub recovery: bool,
    /// Text compares fold long runs of unchanged lines.
    pub collapse_unchanged: bool,
    /// Text compares wrap long lines instead of scrolling sideways.
    pub word_wrap: bool,
    /// Saving over a file keeps its previous contents as `<name>.bak`.
    pub backup_on_save: bool,
    /// Text compares show the current line of every side one above the other.
    pub show_line_detail: bool,
    /// Text compares mark lines edited or copied since the files were opened.
    pub show_edit_markers: bool,
    /// Folder compares show the selected file's differences below the list.
    pub folder_file_pane: bool,
    pub session: Vec<SessionTab>,
    pub session_active: usize,
    /// Named, saved sets of tabs the user can reopen any time (see [`Workspace`]), not only
    /// the single unnamed [`Config::session`] that is restored on its own at startup.
    pub workspaces: Vec<Workspace>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: String::new(),
            theme: ThemeMode::System,
            dark_mode: false,
            font_size: 14.0,
            tab_size: 4,
            diff: DiffOptions::default(),
            rules: String::new(),
            rules_migrated: false,
            open_reports: true,
            pdf_letter: false,
            remote_in_place: false,
            report_hide_paths: false,
            report_detail: Default::default(),
            folder: FolderOptions::default(),
            filter: "*.*".into(),
            recent_paths: Vec::new(),
            recent_compares: Vec::new(),
            recent_remotes: Vec::new(),
            recent_filters: vec![
                "*.*".into(),
                "*.c;*.cpp;*.h".into(),
                "*.rs;*.toml".into(),
                "!.git\\;!target\\".into(),
            ],
            syntax_highlight: true,
            highlight_limit_mb: crate::ui::syntax::DEFAULT_LIMIT_MB,
            show_location_pane: true,
            color_blind_colors: false,
            high_contrast: false,
            reduce_motion: false,
            compact_density: false,
            text_layout: TextLayout::default(),
            show_line_numbers: true,
            show_whitespace: false,
            show_identical: true,
            show_different: true,
            show_unique: [true; 3],
            tree_mode: true,
            use_recycle_bin: true,
            confirm_operations: true,
            patch_style: PatchStyle::Unified,
            patch_context: 3,
            image_threshold: 0,
            image_block: 8,
            binary_bytes_per_line: 16,
            binary_moved_blocks: true,
            unpackers: vec![Unpacker {
                enabled: false,
                pattern: "*.docx;*.odt;*.rtf".into(),
                command: "pandoc -t plain ${file}".into(),
            }],
            web_mode: WebMode::Markdown,
            restore_session: true,
            recovery: false,
            collapse_unchanged: false,
            word_wrap: false,
            backup_on_save: false,
            show_line_detail: false,
            show_edit_markers: true,
            folder_file_pane: false,
            session: Vec::new(),
            session_active: 0,
            workspaces: Vec::new(),
        }
    }
}

/// The settings folder: `OXIDEMERGE_CONFIG_DIR` when set (for scripts and tests that must not
/// see the user's settings), else the platform's. Unit tests use a folder of their own.
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join("oxidemerge-test-config"));
    }
    if let Some(d) = std::env::var_os("OXIDEMERGE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    dirs::config_dir().map(|d| d.join("OxideMerge"))
}

pub fn config_dir_overridden() -> bool {
    cfg!(test) || std::env::var_os("OXIDEMERGE_CONFIG_DIR").is_some_and(|d| !d.is_empty())
}

/// A `oxidemerge.json` next to the executable enables portable mode.
pub fn config_path() -> Option<PathBuf> {
    if config_dir_overridden() {
        return config_dir().map(|d| d.join("config.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("oxidemerge.json");
            if p.exists() {
                return Some(p);
            }
        }
    }
    config_dir().map(|d| d.join("config.json"))
}

/// A settings file that could not be read: `Ok` with the copy of it kept, or `Err` with the
/// file itself when no copy could be made, which saving then leaves alone.
pub type Unreadable = Result<PathBuf, PathBuf>;

static UNREADABLE: std::sync::OnceLock<Unreadable> = std::sync::OnceLock::new();

/// The settings file [`Config::load`] could not read, if any.
pub fn unreadable() -> Option<&'static Unreadable> {
    UNREADABLE.get()
}

impl Config {
    /// Comparison settings with the trusted local converter configuration attached.
    pub fn comparison_options(&self) -> crate::core::diff::DiffOptions {
        let mut options = self.diff.clone();
        options.unpackers = self.unpackers.clone();
        options
    }

    pub fn load() -> Config {
        let Some(p) = config_path() else {
            return Config::default();
        };
        let (cfg, unreadable) = Config::load_from(&p);
        if let Some(u) = unreadable {
            let _ = UNREADABLE.set(u);
        }
        cfg
    }

    /// The settings in `p`. Ones that are there but can't be read are copied to
    /// `<name>.bad` first, as saving the defaults would replace them. An empty file, as made
    /// for portable use, holds the defaults.
    fn load_from(p: &std::path::Path) -> (Config, Option<Unreadable>) {
        let bytes = match std::fs::read(p) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
            Err(_) => return (Config::default(), Some(Err(p.to_path_buf()))),
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return (Config::default(), None);
        }
        if let Some(cfg) = std::str::from_utf8(&bytes).ok().and_then(Config::parse) {
            return (cfg, None);
        }
        let mut kept = p.as_os_str().to_owned();
        kept.push(".bad");
        let kept = PathBuf::from(kept);
        let copied = crate::core::fsutil::write_atomic_with(&kept, &bytes, false);
        let unreadable = match copied {
            Ok(()) => Ok(kept),
            Err(_) => Err(p.to_path_buf()),
        };
        (Config::default(), Some(unreadable))
    }

    fn parse(s: &str) -> Option<Config> {
        let mut value: serde_json::Value = serde_json::from_str(s).ok()?;
        // Settings from when web pages could be compared as screenshots: those modes are gone,
        // so open the pages formatted instead of dropping all the settings as unreadable.
        if let Some(mode) = value.get_mut("web_mode") {
            if matches!(mode.as_str(), Some("Screenshot" | "FullScreenshot")) {
                *mode = serde_json::Value::from("Markdown");
            }
        }
        let has_theme = value.get("theme").is_some();
        let mut cfg: Config = serde_json::from_value(value).ok()?;
        // Settings from before the theme choice existed: keep an explicit dark mode.
        if !has_theme && cfg.dark_mode {
            cfg.theme = ThemeMode::Dark;
        }
        Some(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(p) = config_path() else {
            anyhow::bail!("{}", crate::i18n::tr("msg.no_config"));
        };
        if let Some(Err(left)) = unreadable() {
            anyhow::bail!("{}", unreadable_message(left, None));
        }
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| anyhow::anyhow!("{}: {e}", d.display()))?;
        }
        let s = serde_json::to_string_pretty(self)?;
        // One save at a time: on Windows, a file replaced while another save replaces it too
        // can be refused ("Access is denied"), as unit tests running side by side found.
        static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _saving = SAVING.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::fsutil::write_atomic_with(&p, s.as_bytes(), false)
    }

    pub fn add_recent_path(&mut self, p: &str) {
        if p.trim().is_empty() {
            return;
        }
        self.recent_paths.retain(|x| x != p);
        self.recent_paths.insert(0, p.to_string());
        self.recent_paths.truncate(20);
    }

    pub fn add_recent_compare(&mut self, paths: &[String]) {
        if paths.len() < 2 || paths.iter().any(|p| p.trim().is_empty()) {
            return;
        }
        self.recent_compares.retain(|x| x != paths);
        self.recent_compares.insert(0, paths.to_vec());
        self.recent_compares.truncate(20);
    }

    /// Puts back recent comparisons that were cleared, after any made since.
    pub fn restore_recent_compares(&mut self, cleared: Vec<Vec<String>>) {
        for paths in cleared {
            if !self.recent_compares.contains(&paths) {
                self.recent_compares.push(paths);
            }
        }
        self.recent_compares.truncate(20);
    }

    pub fn remove_recent_compare(&mut self, paths: &[String]) {
        self.recent_compares.retain(|x| x != paths);
    }

    /// Remembers a server connected to, ahead of the others, in place of what was kept for
    /// the same user on it before.
    pub fn add_recent_remote(&mut self, remote: RecentRemote) {
        if remote.host.trim().is_empty() {
            return;
        }
        self.recent_remotes.retain(|r| !r.same_server(&remote));
        self.recent_remotes.insert(0, remote);
        self.recent_remotes.truncate(10);
    }

    pub fn add_recent_filter(&mut self, f: &str) {
        if f.trim().is_empty() {
            return;
        }
        self.recent_filters.retain(|x| x != f);
        self.recent_filters.insert(0, f.to_string());
        self.recent_filters.truncate(20);
    }
}

/// What the user is told about a settings file [`Config::load`] could not read.
pub fn unreadable_message(file: &std::path::Path, kept: Option<&std::path::Path>) -> String {
    let file = ("file", file.display().to_string());
    match kept {
        Some(k) => crate::i18n::trf(
            "msg.config_kept",
            &[file, ("kept", k.display().to_string())],
        ),
        None => crate::i18n::trf("msg.config_left", &[file]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, n: usize) -> Workspace {
        Workspace {
            name: name.into(),
            tabs: vec![SessionTab::default(); n],
        }
    }

    #[test]
    fn workspaces_saved_by_another_instance_are_kept() {
        let base = vec![ws("a", 1), ws("b", 1), ws("gone", 1)];
        // Here: "b" replaced, "gone" deleted, "new" added.
        let ours = vec![ws("a", 1), ws("b", 2), ws("new", 1)];
        // Meanwhile, elsewhere: "a" replaced, "other" added.
        let theirs = vec![ws("a", 3), ws("b", 1), ws("gone", 1), ws("other", 1)];
        assert_eq!(
            merge_workspaces(&base, &ours, &theirs),
            vec![ws("a", 3), ws("b", 2), ws("other", 1), ws("new", 1)]
        );
        // Nothing changed here: whatever was saved elsewhere stands as it is.
        assert_eq!(merge_workspaces(&base, &base, &theirs), theirs);
        // Names match whatever their case: renaming "a" to "A" here replaces it, once.
        let ours = vec![ws("A", 1), ws("b", 1), ws("gone", 1)];
        assert_eq!(
            merge_workspaces(&base, &ours, &theirs),
            vec![ws("A", 1), ws("b", 1), ws("gone", 1), ws("other", 1)]
        );
    }

    #[test]
    fn session_paths_come_back_exactly_even_when_not_valid_unicode() {
        #[cfg(unix)]
        let odd = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"/data/caf\xe9.txt".to_vec()))
        };
        #[cfg(windows)]
        let odd = {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = r"C:\data\caf".encode_utf16().collect();
            wide.extend([0xd800, u16::from(b'.'), u16::from(b't')]);
            PathBuf::from(std::ffi::OsString::from_wide(&wide))
        };
        let plain = PathBuf::from("/data/plain ✓.txt");
        assert!(odd.to_str().is_none());

        let tab = SessionTab::new(&[odd.clone(), plain.clone()], Some("text".into()), None);
        let json = serde_json::to_string(&tab).unwrap();
        let back: SessionTab = serde_json::from_str(&json).unwrap();
        assert_eq!(back.paths(), [odd, plain.clone()]);
        assert_eq!(back.paths[1], plain.display().to_string());

        let tab = SessionTab::new(std::slice::from_ref(&plain), None, None);
        let json = serde_json::to_string(&tab).unwrap();
        assert!(!json.contains("raw"), "{json}");

        // Written before paths were kept exactly, or by another kind of system.
        let old: SessionTab =
            serde_json::from_str(r#"{"paths": ["/a.txt"], "kind": null}"#).unwrap();
        assert_eq!(old.paths(), [PathBuf::from("/a.txt")]);
        for other in ["x:00", "u:zz", "u:c", "u:+0", "w:d80", "w:+0d8"] {
            let tab = SessionTab {
                paths: vec!["/shown.txt".into()],
                raw: vec![Some(other.into())],
                ..Default::default()
            };
            assert_eq!(tab.paths(), [PathBuf::from("/shown.txt")], "{other}");
        }
    }

    #[test]
    fn a_hand_edited_workspace_missing_a_field_keeps_the_settings() {
        let cfg = Config::parse(r#"{"language": "ja", "workspaces": [{"name": "x"}]}"#).unwrap();
        assert_eq!(cfg.language, "ja");
        assert_eq!(cfg.workspaces, vec![ws("x", 0)]);
    }

    #[test]
    fn unreadable_settings_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        assert_eq!(Config::load_from(&p), (Config::default(), None));
        std::fs::write(&p, r#"{"language": "ja"}"#).unwrap();
        assert_eq!(Config::load_from(&p).0.language, "ja");
        for bad in [&br#"{"language": 3}"#[..], b"{", b"\xff\xfe"] {
            std::fs::write(&p, bad).unwrap();
            let kept = dir.path().join("config.json.bad");
            assert_eq!(
                Config::load_from(&p),
                (Config::default(), Some(Ok(kept.clone())))
            );
            assert_eq!(std::fs::read(&kept).unwrap(), bad);
            assert_eq!(std::fs::read(&p).unwrap(), bad);
        }
        // An empty file is how portable use starts.
        std::fs::remove_file(dir.path().join("config.json.bad")).unwrap();
        for empty in [&b""[..], b" \n"] {
            std::fs::write(&p, empty).unwrap();
            assert_eq!(Config::load_from(&p), (Config::default(), None));
        }
        assert!(!dir.path().join("config.json.bad").exists());
        // A file that is there but can't be read is never saved over.
        let unreadable = dir.path().join("folder.json");
        std::fs::create_dir(&unreadable).unwrap();
        assert_eq!(
            Config::load_from(&unreadable),
            (Config::default(), Some(Err(unreadable.clone())))
        );
    }

    #[test]
    fn text_layout_defaults_and_roundtrips_without_changing_comparison_rules() {
        let old = Config::parse("{}").unwrap();
        assert_eq!(old.text_layout, TextLayout::Compact);
        for text_layout in TextLayout::ALL {
            let cfg = Config {
                text_layout,
                ..old.clone()
            };
            let reopened = Config::parse(&serde_json::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(reopened, cfg);
            assert_eq!(reopened.diff, old.diff);
        }
    }

    #[test]
    fn removed_screenshot_web_modes_open_formatted() {
        for old in ["Screenshot", "FullScreenshot"] {
            let cfg =
                Config::parse(&format!(r#"{{"web_mode": "{old}", "web_browser": "x"}}"#)).unwrap();
            assert_eq!(cfg.web_mode, WebMode::Markdown);
        }
        let cfg = Config::parse(r#"{"web_mode": "Html"}"#).unwrap();
        assert_eq!(cfg.web_mode, WebMode::Html);
    }

    #[test]
    fn old_dark_mode_becomes_explicit_theme() {
        let cfg = Config::parse(r#"{"dark_mode": true}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::Dark);
        let cfg = Config::parse(r#"{"dark_mode": false}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::System);
        let cfg = Config::parse(r#"{"dark_mode": true, "theme": "System"}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::System);
    }
}
