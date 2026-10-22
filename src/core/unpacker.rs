//! External "unpacker" plugins: convert files to text with a command line tool before comparing
//! (similar to WinMerge unpacker/prediffer plugins, e.g. `pandoc -t plain ${file}`).

use super::filter::wildcard_to_regex;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Unpacker {
    pub enabled: bool,
    /// Wildcard masks separated by ';', e.g. `*.docx;*.odt`.
    pub pattern: String,
    /// Command line; `${file}` is replaced by the quoted file path. Output is read from stdout.
    pub command: String,
}

impl Unpacker {
    pub fn matches(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.enabled
            && self
                .pattern
                .split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .filter_map(wildcard_to_regex)
                .any(|r| r.is_match(&name))
    }

    pub fn run(&self, path: &Path) -> Result<Vec<u8>> {
        let quoted = format!("\"{}\"", path.display());
        let cmdline = if self.command.contains("${file}") {
            self.command.replace("${file}", &quoted)
        } else {
            format!("{} {}", self.command, quoted)
        };
        let output = if cfg!(windows) {
            Command::new("cmd").args(["/C", &cmdline]).output()?
        } else {
            Command::new("sh").args(["-c", &cmdline]).output()?
        };
        if !output.status.success() && output.stdout.is_empty() {
            bail!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(output.stdout)
    }
}

pub fn find<'a>(unpackers: &'a [Unpacker], path: &Path) -> Option<&'a Unpacker> {
    unpackers.iter().find(|u| u.matches(path))
}
