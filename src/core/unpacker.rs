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
        // The shell gets the path as an argument, never as part of the command it parses, so
        // no file name can run commands. Windows file names can't contain quotes.
        let quoted = if cfg!(windows) {
            format!("\"{}\"", path.display())
        } else {
            "\"$1\"".to_string()
        };
        let cmdline = if self.command.contains("${file}") {
            self.command.replace("${file}", &quoted)
        } else {
            format!("{} {}", self.command, quoted)
        };
        let output = if cfg!(windows) {
            Command::new("cmd").args(["/C", &cmdline]).output()?
        } else {
            Command::new("sh")
                .args(["-c", &cmdline, "sh"])
                .arg(path)
                .output()?
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_never_run_as_commands() {
        let dir = tempfile::tempdir().unwrap();
        let cat = |command: &str| Unpacker {
            enabled: true,
            pattern: "*.txt".into(),
            command: command.into(),
        };
        // An unbalanced quote would only make an injected command a syntax error, so names
        // without one are tried too.
        for name in [
            "a \"$(touch ran)\" `touch ran2` $HOME ' b.txt",
            "a $(touch ran) `touch ran2`; touch ran3 b.txt",
            "a \"; touch ran3; \" b.txt",
        ] {
            let file = dir.path().join(name);
            std::fs::write(&file, "content\n").unwrap();
            let u = cat(&format!("cd '{}' && cat ${{file}}", dir.path().display()));
            assert!(u.matches(&file));
            assert_eq!(u.run(&file).unwrap(), b"content\n", "{name}");
            assert_eq!(cat("cat").run(&file).unwrap(), b"content\n", "{name}");
            for ran in ["ran", "ran2", "ran3"] {
                assert!(!dir.path().join(ran).exists(), "{name} ran a command");
            }
            assert!(cat("false").run(&file).is_err());
        }
    }
}
