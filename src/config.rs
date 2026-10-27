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

/// A tab of the previous session: what was compared, and how.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct SessionTab {
    pub paths: Vec<String>,
    /// "text", "binary", "image" or "web"; none for folder compares.
    pub kind: Option<String>,
    /// Compare rules file of the tab when it has its own ("" = none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
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
    pub folder: FolderOptions,
    pub filter: String,
    pub recent_paths: Vec<String>,
    /// Recent comparisons, newest first: the 2 or 3 paths or URLs of each.
    pub recent_compares: Vec<Vec<String>>,
    pub recent_filters: Vec<String>,
    pub syntax_highlight: bool,
    pub show_location_pane: bool,
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
    pub unpackers: Vec<Unpacker>,
    /// Browser used for web page screenshots ("" = detect Edge, Chrome or Chromium).
    pub web_browser: String,
    pub web_width: u32,
    pub web_height: u32,
    pub web_mode: WebMode,
    pub restore_session: bool,
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
            folder: FolderOptions::default(),
            filter: "*.*".into(),
            recent_paths: Vec::new(),
            recent_compares: Vec::new(),
            recent_filters: vec![
                "*.*".into(),
                "*.c;*.cpp;*.h".into(),
                "*.rs;*.toml".into(),
                "!.git\\;!target\\".into(),
            ],
            syntax_highlight: true,
            show_location_pane: true,
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
            unpackers: vec![Unpacker {
                enabled: false,
                pattern: "*.docx;*.odt;*.rtf".into(),
                command: "pandoc -t plain ${file}".into(),
            }],
            web_browser: String::new(),
            web_width: 1280,
            web_height: 1600,
            web_mode: WebMode::Screenshot,
            restore_session: true,
            collapse_unchanged: false,
            word_wrap: false,
            backup_on_save: false,
            show_line_detail: false,
            show_edit_markers: true,
            folder_file_pane: false,
            session: Vec::new(),
            session_active: 0,
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
        let value: serde_json::Value = serde_json::from_str(s).ok()?;
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

    pub fn remove_recent_compare(&mut self, paths: &[String]) {
        self.recent_compares.retain(|x| x != paths);
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
    fn old_dark_mode_becomes_explicit_theme() {
        let cfg = Config::parse(r#"{"dark_mode": true}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::Dark);
        let cfg = Config::parse(r#"{"dark_mode": false}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::System);
        let cfg = Config::parse(r#"{"dark_mode": true, "theme": "System"}"#).unwrap();
        assert_eq!(cfg.theme, ThemeMode::System);
    }
}
