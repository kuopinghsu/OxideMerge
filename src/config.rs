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

fn config_dir_overridden() -> bool {
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

impl Config {
    pub fn load() -> Config {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Config::parse(&s))
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

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
