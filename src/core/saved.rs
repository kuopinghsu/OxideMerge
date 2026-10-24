//! Saved comparisons (`.oxmcmp`): the text of every side with its bookmarks and their
//! comments, in one file that can be reviewed later or on another machine.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const EXT: &str = "oxmcmp";
const VERSION: u32 = 1;

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct SavedPane {
    /// What the side was called (its path or description).
    pub title: String,
    pub path: Option<String>,
    pub text: String,
    /// File extension for syntax highlighting.
    pub syntax: Option<String>,
    /// Bookmarked lines (1-based) and their comments.
    pub bookmarks: BTreeMap<usize, String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct SavedCompare {
    pub version: u32,
    pub saved: String,
    pub panes: Vec<SavedPane>,
}

pub fn is_saved_compare(path: &Path) -> bool {
    path.is_file() && super::text::extension(path) == EXT
}

impl SavedCompare {
    pub fn new(panes: Vec<SavedPane>) -> SavedCompare {
        SavedCompare {
            version: VERSION,
            saved: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            panes,
        }
    }

    pub fn load(path: &Path) -> Result<SavedCompare> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        let s: SavedCompare =
            serde_json::from_str(&text).with_context(|| path.display().to_string())?;
        if !(2..=3).contains(&s.panes.len()) {
            bail!("{}: expected 2 or 3 sides", path.display());
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        super::fsutil::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_sides_with_bookmark_comments() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("review.{EXT}"));
        let s = SavedCompare::new(vec![
            SavedPane {
                title: "old.c".into(),
                text: "a\nb\n".into(),
                bookmarks: BTreeMap::from([(2, "Why was this removed?".into())]),
                ..Default::default()
            },
            SavedPane {
                title: "new.c".into(),
                text: "a\n".into(),
                ..Default::default()
            },
        ]);
        s.save(&file).unwrap();
        assert!(is_saved_compare(&file));
        assert_eq!(SavedCompare::load(&file).unwrap(), s);
        std::fs::write(&file, r#"{"panes":[{"text":"x"}]}"#).unwrap();
        assert!(SavedCompare::load(&file).is_err());
    }
}
