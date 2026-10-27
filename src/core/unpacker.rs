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
        // The shell gets the path as an argument or variable, never as part of the command it
        // parses, so no file name can run commands. In quotes, nothing in a Windows name is
        // special to `cmd`, and the name can't contain a quote to end them.
        let quoted = if cfg!(windows) {
            "\"%OXIDEMERGE_FILE%\""
        } else {
            "\"$1\""
        };
        // Quotes written around the placeholder would pair with these and leave the path bare.
        let command = self
            .command
            .replace("\"${file}\"", "${file}")
            .replace("'${file}'", "${file}");
        let cmdline = if command.contains("${file}") {
            command.replace("${file}", quoted)
        } else {
            format!("{command} {quoted}")
        };
        #[cfg(windows)]
        let output = {
            use std::os::windows::process::CommandExt;
            // Passed as is: `cmd` doesn't undo the escaping other arguments get.
            Command::new("cmd")
                .raw_arg(format!("/D /V:OFF /S /C \"{cmdline}\""))
                .env("OXIDEMERGE_FILE", path)
                .output()?
        };
        #[cfg(not(windows))]
        let output = Command::new("sh")
            .args(["-c", &cmdline, "sh"])
            .arg(path)
            .output()?;
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
            // Quoting the placeholder as well changes nothing.
            for quoted in ["cat \"${file}\"", "cat '${file}'"] {
                assert_eq!(cat(quoted).run(&file).unwrap(), b"content\n", "{name}");
            }
            for ran in ["ran", "ran2", "ran3"] {
                assert!(!dir.path().join(ran).exists(), "{name} ran a command");
            }
            assert!(cat("false").run(&file).is_err());
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn file_names_are_never_run_as_commands() {
        let dir = tempfile::tempdir().unwrap();
        let unpacker = |command: &str| Unpacker {
            enabled: true,
            pattern: "*.txt".into(),
            command: command.into(),
        };
        // Any of these read as part of the command would make `type` miss the file.
        for name in [
            "plain b.txt",
            "a & exit 3 & b.txt",
            "a ^& b.txt",
            "a %PATH% b.txt",
            "a !PATH! b.txt",
            "a (b); c, d.txt",
        ] {
            let file = dir.path().join(name);
            std::fs::write(&file, "content\r\n").unwrap();
            let u = unpacker("type ${file}");
            assert!(u.matches(&file));
            assert_eq!(u.run(&file).unwrap(), b"content\r\n", "{name}");
            assert_eq!(
                unpacker("type").run(&file).unwrap(),
                b"content\r\n",
                "{name}"
            );
            assert_eq!(
                unpacker("type \"${file}\"").run(&file).unwrap(),
                b"content\r\n",
                "{name}"
            );
        }
    }
}
