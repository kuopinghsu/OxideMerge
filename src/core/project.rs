//! Project files: a saved comparison (paths, filter and options) to open again later.
//! OxideMerge writes `.oxmproj` files (JSON) and also reads WinMerge `.WinMerge` projects.

use super::folder::CompareMethod;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const EXT: &str = "oxmproj";
pub const WINMERGE_EXT: &str = "winmerge";

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Project {
    /// 2 or 3 files, folders or URLs. Relative paths are relative to the project file.
    pub paths: Vec<String>,
    pub descriptions: Vec<Option<String>>,
    pub read_only: Vec<bool>,
    /// "text", "binary", "image" or "web" to force the compare type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<CompareMethod>,
}

pub fn is_project(path: &Path) -> bool {
    let ext = super::text::extension(path);
    (ext == EXT || ext == WINMERGE_EXT) && path.is_file()
}

impl Project {
    pub fn load(path: &Path) -> Result<Project> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let project = if super::text::extension(path) == WINMERGE_EXT {
            parse_winmerge(&text)?
        } else {
            serde_json::from_str(&text).with_context(|| format!("{}", path.display()))?
        };
        if !(2..=3).contains(&project.paths.len()) {
            bail!("{}: a project needs 2 or 3 paths", path.display());
        }
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        super::fsutil::write_atomic(path, text.as_bytes())
    }

    /// The paths, with relative ones resolved against the folder of the project file.
    pub fn resolved_paths(&self, project_file: &Path) -> Vec<PathBuf> {
        let base = project_file.parent().unwrap_or(Path::new(""));
        self.paths
            .iter()
            .map(|p| {
                let path = PathBuf::from(p);
                if path.is_relative() && !super::web::is_url(p) {
                    base.join(path)
                } else {
                    path
                }
            })
            .collect()
    }
}

/// Reads the paths, descriptions, read-only flags, filter and subfolder setting of a WinMerge
/// project; its other settings have no counterpart here.
fn parse_winmerge(text: &str) -> Result<Project> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut sides: [Option<String>; 3] = Default::default();
    let mut descs: [Option<String>; 3] = Default::default();
    let mut read_only = [false; 3];
    let mut project = Project::default();
    let mut tag = String::new();
    let mut value = String::new();
    let mut in_first_paths = false;
    let mut seen_paths = false;
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) => {
                tag = e.name().as_ref().to_ascii_lowercase();
                value.clear();
                if tag == "paths" && !seen_paths {
                    in_first_paths = true;
                    seen_paths = true;
                }
            }
            Event::Text(t) => value.push_str(&t),
            Event::GeneralRef(r) => value.push_str(&entity(&r)),
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case("paths") {
                    in_first_paths = false;
                }
                if !in_first_paths {
                    continue;
                }
                let value = std::mem::take(&mut value).trim().to_string();
                let side = |name: &str| match name {
                    "left" => Some(0),
                    "middle" => Some(1),
                    "right" => Some(2),
                    _ => None,
                };
                if let Some(s) = side(&tag) {
                    sides[s] = Some(value);
                } else if let Some(s) = tag.strip_suffix("-desc").and_then(side) {
                    descs[s] = Some(value);
                } else if let Some(s) = tag.strip_suffix("-readonly").and_then(side) {
                    read_only[s] = value.trim() == "1";
                } else if tag == "filter" {
                    project.filter = Some(value);
                } else if tag == "subfolders" {
                    project.recursive = Some(value.trim() == "1");
                }
                tag.clear();
            }
            _ => {}
        }
    }
    for s in 0..3 {
        if let Some(p) = sides[s].take() {
            project.paths.push(p);
            project.descriptions.push(descs[s].take());
            project.read_only.push(read_only[s]);
        }
    }
    Ok(project)
}

/// The text of an XML entity or character reference, given without `&` and `;`.
fn entity(name: &str) -> String {
    let code = match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => name.strip_prefix('#').and_then(|n| {
            let n = match n.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(n)
        }),
    };
    code.map_or_else(|| format!("&{name};"), String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_loads_and_reads_winmerge_projects() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.oxmproj");
        let p = Project {
            paths: vec!["left".into(), "/abs/right".into()],
            descriptions: vec![Some("Old".into()), None],
            read_only: vec![true, false],
            filter: Some("*.rs".into()),
            recursive: Some(true),
            method: Some(CompareMethod::Size),
            ..Default::default()
        };
        p.save(&file).unwrap();
        let back = Project::load(&file).unwrap();
        assert_eq!(back, p);
        let paths = back.resolved_paths(&file);
        assert_eq!(paths[0], dir.path().join("left"));
        assert_eq!(paths[1], PathBuf::from("/abs/right"));

        let wm = dir.path().join("x.WinMerge");
        std::fs::write(
            &wm,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<project><paths>\
             <left>C:\\a</left><left-desc>Mine &amp; yours</left-desc><right>C:\\b</right>\
             <right-readonly>1</right-readonly><filter>*.c;*.h</filter><subfolders>1</subfolders>\
             </paths></project>",
        )
        .unwrap();
        assert!(is_project(&wm));
        let w = Project::load(&wm).unwrap();
        assert_eq!(w.paths, ["C:\\a", "C:\\b"]);
        assert_eq!(w.descriptions, [Some("Mine & yours".into()), None]);
        assert_eq!(w.read_only, [false, true]);
        assert_eq!(w.filter.as_deref(), Some("*.c;*.h"));
        assert_eq!(w.recursive, Some(true));
    }
}
