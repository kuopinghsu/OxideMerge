//! Persistent user options.

use crate::core::diff::DiffOptions;
use crate::core::folder::FolderOptions;
use crate::core::patch::PatchStyle;
use crate::core::unpacker::Unpacker;
use crate::core::web::WebMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Language code ("" = follow the system language).
    pub language: String,
    pub dark_mode: bool,
    pub font_size: f32,
    pub tab_size: usize,
    pub diff: DiffOptions,
    pub folder: FolderOptions,
    pub filter: String,
    pub recent_paths: Vec<String>,
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: String::new(),
            dark_mode: false,
            font_size: 14.0,
            tab_size: 4,
            diff: DiffOptions::default(),
            folder: FolderOptions::default(),
            filter: "*.*".into(),
            recent_paths: Vec::new(),
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
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("OxideMerge"))
}

/// A `oxidemerge.json` next to the executable enables portable mode.
pub fn config_path() -> Option<PathBuf> {
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
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = config_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }

    pub fn add_recent_path(&mut self, p: &str) {
        if p.trim().is_empty() {
            return;
        }
        self.recent_paths.retain(|x| x != p);
        self.recent_paths.insert(0, p.to_string());
        self.recent_paths.truncate(20);
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
