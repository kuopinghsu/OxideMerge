//! External "unpacker" plugins: convert files to text with a command line tool before comparing
//! (similar to WinMerge unpacker/prediffer plugins, e.g. `pandoc -t plain ${file}`).

use super::filter::wildcard_to_regex;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_OUTPUT: u64 = 16 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug, Default)]
pub struct Unpacker {
    pub enabled: bool,
    /// Wildcard masks separated by ';', e.g. `*.docx;*.odt`.
    pub pattern: String,
    /// Command line; `${file}` is replaced by the quoted file path. Output is read from stdout.
    pub command: String,
}

impl Unpacker {
    pub fn presets() -> Vec<(&'static str, Self)> {
        [
            ("PDF (pdftotext)", "*.pdf", "pdftotext -layout ${file} -"),
            (
                "Office / OpenDocument (Pandoc)",
                "*.docx;*.odt;*.rtf",
                "pandoc -t plain ${file}",
            ),
            (
                "Documents (Apache Tika)",
                "*.doc;*.docx;*.xls;*.xlsx;*.ppt;*.pptx;*.pdf;*.odt;*.ods;*.odp;*.rtf",
                "tika --text ${file}",
            ),
        ]
        .into_iter()
        .map(|(name, pattern, command)| {
            (
                name,
                Self {
                    enabled: false,
                    pattern: pattern.into(),
                    command: command.into(),
                },
            )
        })
        .collect()
    }

    /// Presets use a simple executable name; custom shell commands are checked when run.
    pub fn check_dependency(&self) -> Result<()> {
        let tool = self.command.split_whitespace().next().unwrap_or("");
        if tool.is_empty() {
            bail!("Converter command is empty");
        }
        let available = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| {
                dir.join(tool).is_file()
                    || (cfg!(windows)
                        && ["exe", "cmd", "bat"]
                            .iter()
                            .any(|ext| dir.join(format!("{tool}.{ext}")).is_file()))
            })
        }) || Path::new(tool).is_file();
        if !available {
            bail!("Install {tool} and add it to PATH (or configure its full path)");
        }
        Ok(())
    }
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
        self.run_with(path, None)
    }

    pub fn run_with(&self, path: &Path, cancel: Option<&AtomicBool>) -> Result<Vec<u8>> {
        self.run_limited(path, cancel, TIMEOUT, MAX_OUTPUT)
    }

    fn run_limited(
        &self,
        path: &Path,
        cancel: Option<&AtomicBool>,
        timeout: Duration,
        limit: u64,
    ) -> Result<Vec<u8>> {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            bail!("Conversion cancelled");
        }
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
        let mut command = {
            use std::os::windows::process::CommandExt;
            // Passed as is: `cmd` doesn't undo the escaping other arguments get.
            let mut command = Command::new("cmd");
            command
                .raw_arg(format!("/D /V:OFF /S /C \"{cmdline}\""))
                .env("OXIDEMERGE_FILE", path);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", &cmdline, "sh"]).arg(path);
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            command
        };
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Starting converter for {}", path.display()))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        for (is_out, pipe) in [
            (true, Box::new(stdout) as Box<dyn Read + Send>),
            (false, Box::new(stderr) as Box<dyn Read + Send>),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe.take(limit + 1).read_to_end(&mut bytes).map(|_| bytes);
                let _ = tx.send((is_out, result));
            });
        }
        drop(tx);
        let started = Instant::now();
        let mut out = None;
        let mut err = None;
        let result = loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                break Err(anyhow::anyhow!("Conversion cancelled"));
            }
            if started.elapsed() >= timeout {
                break Err(anyhow::anyhow!(
                    "Converter timed out after {} seconds",
                    timeout.as_secs()
                ));
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok((is_out, Ok(bytes))) if bytes.len() as u64 <= limit => {
                    if is_out {
                        out = Some(bytes);
                    } else {
                        err = Some(bytes);
                    }
                }
                Ok((_, Ok(_))) => {
                    break Err(anyhow::anyhow!("Converter output exceeds {} bytes", limit))
                }
                Ok((_, Err(e))) => break Err(e.into()),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                    if out.is_none() || err.is_none() =>
                {
                    break Err(anyhow::anyhow!("Converter output unavailable"))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
            if let (Some(_), Some(stderr)) = (&out, &err) {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => break Ok(out.take().unwrap()),
                    Ok(Some(status)) => {
                        break Err(anyhow::anyhow!(
                            "Converter exited with {status}: {}",
                            String::from_utf8_lossy(stderr)
                        ))
                    }
                    Ok(None) => {}
                    Err(e) => break Err(e.into()),
                }
            }
        };
        if result.is_err() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .output();
            }
            let _ = child.kill();
        }
        let _ = child.wait();
        result.with_context(|| format!("Converting {}", path.display()))
    }
}

/// Shared by GUI, CLI, folder scanning and reports. Converted documents cannot overwrite
/// their binary originals, and output is interpreted identically in every entry point.
pub fn load_doc(
    path: &Path,
    opts: &super::diff::DiffOptions,
    cancel: Option<&AtomicBool>,
) -> Result<super::text::TextDoc> {
    let (bytes, label) = match find(opts, path) {
        Some(Converter::Command(converter)) => {
            let bytes = match cancel {
                Some(cancel) => converter.run_with(path, Some(cancel))?,
                None => converter.run(path)?,
            };
            (bytes, crate::i18n::tr("label.unpacked"))
        }
        Some(Converter::BuiltIn(kind)) => {
            // A reader is given what anyone sends it, so a mistake in one fails the file
            // rather than the window or the scan that asked for it.
            let read =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| kind.read(path, cancel)))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(crate::i18n::trf(
                            "msg.cannot_extract",
                            &[("file", path.display().to_string())],
                        )))
                    });
            // A notebook is JSON text to begin with: one this can't read (an older or broken
            // one) is still worth comparing as it is.
            let cancelled = cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
            if read.is_err() && kind == super::extract::Kind::Notebook && !cancelled {
                return super::text::load_doc(path, None);
            }
            (read?.into_bytes(), crate::i18n::tr(kind.label()))
        }
        None => return super::text::load_doc(path, None),
    };
    let mut doc = super::text::doc_from_bytes(Some(path.to_path_buf()), &bytes, None);
    // Read from the file rather than being it: what is shown cannot be written back.
    doc.read_only = true;
    doc.compare_only = true;
    doc.description = Some(format!("{} [{label}]", path.display()));
    Ok(doc)
}

pub fn load_compare_doc(
    path: &Path,
    opts: &super::diff::DiffOptions,
    cancel: Option<&AtomicBool>,
) -> Result<super::text::TextDoc> {
    let mut doc = load_doc(path, opts, cancel)?;
    if opts.prettify {
        let source = doc.text();
        if let Some(pretty) = super::prettify::detect(doc.path.as_deref(), &source)
            .and_then(|kind| super::prettify::format(&source, kind))
        {
            doc.lines = super::text::split_lines(&pretty).0;
            doc.read_only = true;
            doc.compare_only = true;
        }
    }
    Ok(doc)
}

/// How a file is made into text before it is compared.
///
/// Serialized without a tag of its own, so that a file converted by a command keeps the
/// fingerprint it had before OxideMerge read any format itself, and a saved review of one does
/// not become stale on an upgrade.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(untagged)]
pub enum Converter<'a> {
    /// A command line the user configured, which is run on the file.
    Command(&'a Unpacker),
    /// A format OxideMerge reads itself, with no other program involved.
    BuiltIn(super::extract::Kind),
}

impl Converter<'_> {
    /// What tells one converter from another, for the fingerprints that record how a result
    /// was made. A command is its own text; a built-in reader is named by its kind.
    pub fn key(&self) -> String {
        match self {
            Converter::Command(u) => u.command.clone(),
            Converter::BuiltIn(kind) => format!("{kind:?}"),
        }
    }

    /// Whether files this converter reads are the same when their text is.
    pub fn compares_by_text(&self) -> bool {
        match self {
            Converter::Command(_) => true,
            Converter::BuiltIn(kind) => kind.compares_by_text(),
        }
    }
}

/// What turns `path` into text, if anything does. A converter the user configured comes first,
/// so a better one can always be put in front of what OxideMerge reads itself.
pub fn find<'a>(opts: &'a super::diff::DiffOptions, path: &Path) -> Option<Converter<'a>> {
    if let Some(u) = opts.unpackers.iter().find(|u| u.matches(path)) {
        return Some(Converter::Command(u));
    }
    if !opts.extract_documents {
        return None;
    }
    super::extract::Kind::of(path).map(Converter::BuiltIn)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn documents_read_as_text_are_compare_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/left");
        let opts = super::super::diff::DiffOptions::default();
        for name in [
            "docs/report.docx",
            "docs/budget.xlsx",
            "docs/report.pdf",
            "docs/report.ipynb",
        ] {
            let doc = load_doc(&root.join(name), &opts, None).unwrap();
            assert!(doc.read_only && doc.compare_only, "{name}");
        }
        // A plain text file is the file itself.
        let doc = load_doc(&root.join("same.txt"), &opts, None).unwrap();
        assert!(!doc.read_only && !doc.compare_only);
    }

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

#[cfg(all(test, unix))]
mod limit_tests {
    use super::*;
    #[test]
    fn rejects_partial_failures_excess_output_timeouts_and_cancellation() {
        let run = |command: &str, limit, timeout| {
            Unpacker {
                enabled: true,
                pattern: "*".into(),
                command: command.into(),
            }
            .run_limited(Path::new("unused"), None, timeout, limit)
        };
        let err = run(
            "printf partial; exit 7 # ${file}",
            100,
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("exited"));
        let err = run("printf 12345 # ${file}", 4, Duration::from_secs(2)).unwrap_err();
        assert!(format!("{err:#}").contains("exceeds"));
        let start = Instant::now();
        let err = run("sleep 5 # ${file}", 100, Duration::from_millis(100)).unwrap_err();
        assert!(format!("{err:#}").contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let converter = Unpacker {
            enabled: true,
            pattern: "*".into(),
            command: "sleep 5 # ${file}".into(),
        };
        let err = converter
            .run_with(Path::new("unused"), Some(&cancel))
            .unwrap_err();
        thread.join().unwrap();
        assert!(format!("{err:#}").contains("cancelled"));
    }
}
