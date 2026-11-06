//! Job files (`.oxjob`): compares, reports and folder synchronizations saved to run again with
//! `--job`. A job file is JSON with a version number. Each job becomes the command-line options
//! that do the same, so a job gives what the equivalent command gives.
//!
//! A job file names no command and no shell; paths in it are relative to its own folder and may
//! use `${env:NAME}`. Comparing a file that an unpacker of the settings reads still runs that
//! unpacker, as any comparison does. A job that changes a folder must say so (`"kind": "sync"`
//! with `"apply": true`), and even then it runs only when `--yes` is given too; so does a job
//! whose report or patch lies outside the job file's folder. A job file alone never authorizes
//! changing, deleting or overwriting anything outside its folder.

use crate::i18n::{tr, trf};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The version of the job file format; newer files are refused.
pub const JOB_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobFile {
    pub version: u32,
    /// Go on with the next job after one fails, rather than stop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_on_error: bool,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Compare two or three files or folders, and write a report or patch.
    #[default]
    Compare,
    /// Plan a synchronization of two folders and write the plan; changes nothing.
    SyncPlan,
    /// Synchronize two folders: plan, then apply at once.
    Sync,
}

impl JobKind {
    pub fn key(self) -> &'static str {
        match self {
            JobKind::Compare => "compare",
            JobKind::SyncPlan => "sync_plan",
            JobKind::Sync => "sync",
        }
    }
}

/// One job. Only the fields of its kind may be given.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: JobKind,
    // Compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    // Synchronization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// `mirror` or `update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub apply: bool,
    /// Lets a sync job delete as many files as the second confirmation of the window is for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_large_deletion: bool,
    /// Synchronize symbolic links as links (`--sync-links`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub copy_links: bool,
    // What is compared.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// `full`, `quick`, `binary`, `date`, `datesize` or `size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// `text`, `binary`, `image` or `web`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_as: Option<String>,
    /// A rules file: a name in the rules folder, or a path; `""` for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// `compare`, `change` or `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whitespace: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_case: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_blank: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_eol: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_numbers: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_comments: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prettify: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub structured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_tolerance: Option<f64>,
    // Output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub report_diffs: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slideshow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// `normal`, `context` or `unified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    /// A .pdf report: `a4` or `letter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub portrait: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unified: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whole_file: bool,
}

impl JobFile {
    /// Reads a job file, refusing a newer format, an unknown field or an empty list.
    pub fn parse(text: &str) -> Result<JobFile, String> {
        let not_a_job = |e: serde_json::Error| trf("job.err_not_job", &[("error", e.to_string())]);
        let value: serde_json::Value = serde_json::from_str(text).map_err(not_a_job)?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v > u64::from(JOB_VERSION) => {
                return Err(trf(
                    "job.err_newer",
                    &[("version", v.to_string()), ("max", JOB_VERSION.to_string())],
                ))
            }
            Some(0) => return Err(trf("job.err_bad_version", &[("version", "0".into())])),
            Some(_) => {}
            None => return Err(tr("job.err_no_version").into()),
        }
        let file: JobFile = serde_json::from_value(value).map_err(not_a_job)?;
        if file.jobs.is_empty() {
            return Err(tr("job.err_no_jobs").into());
        }
        Ok(file)
    }

    /// Reads the job file at `path`.
    pub fn read(path: &Path) -> Result<JobFile, String> {
        let bytes = super::analysis::read(path).map_err(|e| format!("{e:#}"))?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        JobFile::parse(&text)
    }

    /// The reports and patches of its jobs that lie outside `dir`, the folder of the job file,
    /// resolved as writing them would. Writing those needs the user's agreement.
    pub fn outputs_outside(&self, dir: &Path) -> Vec<PathBuf> {
        let base = resolved(dir);
        self.jobs
            .iter()
            .flat_map(|j| j.outputs(dir))
            .filter(|p| !resolved(p).starts_with(&base))
            .collect()
    }
}

impl JobFile {
    /// A job file holding `job` alone.
    pub fn single(job: Job) -> JobFile {
        JobFile {
            version: JOB_VERSION,
            continue_on_error: false,
            jobs: vec![job],
        }
    }

    /// The file's text, as saved: pretty JSON ending in a newline.
    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default() + "\n"
    }

    /// Whether running the file may change a folder: whether it holds a sync job.
    pub fn changes_folders(&self) -> bool {
        self.jobs.iter().any(|j| j.kind == JobKind::Sync)
    }
}

/// What a job does once read: the command-line options that do it, and for a sync job the
/// parts its apply step needs.
#[derive(Debug, PartialEq)]
pub struct Prepared {
    /// Without the program name.
    pub args: Vec<OsString>,
    /// For a sync job: whether it may delete many files.
    pub allow_large_deletion: bool,
}

impl Job {
    /// The job's name, or what it compares.
    pub fn label(&self, index: usize) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n.clone(),
            _ => trf("job.unnamed", &[("n", (index + 1).to_string())]),
        }
    }

    /// Checks the job, as running it would, without running it.
    pub fn validate(&self) -> Result<(), String> {
        self.check()
    }

    /// Checks the job and turns it into command-line options, with its paths made relative
    /// to `dir`, the folder of the job file.
    pub fn prepare(&self, dir: &Path) -> Result<Prepared, String> {
        self.check()?;
        let path = |field: &str, v: &str| -> Result<OsString, String> {
            let text = expand(v).map_err(|e| format!("{field}: {e}"))?;
            if super::web::is_url(&text) {
                return Ok(text.into());
            }
            let p = PathBuf::from(&text);
            Ok(if p.is_absolute() { p } else { dir.join(p) }.into_os_string())
        };
        let mut args: Vec<OsString> = Vec::new();
        let mut opt = |name: &str, value: Option<OsString>| {
            let mut a = OsString::from(format!("--{name}"));
            if let Some(v) = value {
                a.push("=");
                a.push(v);
            }
            args.push(a);
        };
        let text = |v: &str| Some(OsString::from(v));
        let mut paths = Vec::new();
        match self.kind {
            JobKind::Compare => {
                paths.push(path("left", self.left.as_deref().unwrap_or_default())?);
                if let Some(b) = &self.base {
                    paths.push(path("base", b)?);
                }
                paths.push(path("right", self.right.as_deref().unwrap_or_default())?);
            }
            JobKind::SyncPlan | JobKind::Sync => {
                opt("sync-plan", self.mode.as_deref().and_then(text));
                paths.push(path("source", self.source.as_deref().unwrap_or_default())?);
                paths.push(path(
                    "destination",
                    self.destination.as_deref().unwrap_or_default(),
                )?);
            }
        }
        if self.recursive {
            opt("recursive", None);
        }
        if let Some(f) = &self.filter {
            opt("filter", text(f));
        }
        if let Some(m) = &self.method {
            opt("method", text(m));
        }
        if let Some(k) = &self.compare_as {
            opt("compare-as", text(k));
        }
        if let Some(r) = &self.rules {
            let spec = expand(r).map_err(|e| format!("rules: {e}"))?;
            opt("rules", text(&super::rules::absolute_spec(&spec, dir)));
        }
        if let Some(w) = &self.whitespace {
            opt("ignore-ws", text(w));
        }
        for (on, name) in [
            (self.ignore_case, "ignore-case"),
            (self.ignore_blank, "ignore-blank"),
            (self.ignore_eol, "ignore-eol"),
            (self.ignore_numbers, "ignore-numbers"),
            (self.ignore_comments, "ignore-comments"),
            (self.prettify, "prettify"),
            (self.structured, "structured"),
            (self.report_diffs, "report-diffs"),
            (self.copy_links, "sync-links"),
            (self.slideshow, "slideshow"),
            (self.portrait, "portrait"),
            (self.unified, "unified"),
            (self.whole_file, "whole-file"),
        ] {
            if on {
                opt(name, None);
            }
        }
        if let Some(t) = self.numeric_tolerance {
            opt("numeric-tolerance", text(&t.to_string()));
        }
        if let Some(r) = &self.report {
            opt("report", Some(path("report", r)?));
        }
        if let Some(p) = &self.patch {
            opt("patch", Some(path("patch", p)?));
        }
        if let Some(s) = &self.patch_style {
            opt("patch-style", text(s));
        }
        if let Some(c) = self.context {
            opt("context", text(&c.to_string()));
        }
        if let Some(p) = &self.page_size {
            opt("page-size", text(p));
        }
        // Paths after `--`, so one starting with `-` is not read as an option.
        args.push("--".into());
        args.extend(paths);
        Ok(Prepared {
            args,
            allow_large_deletion: self.allow_large_deletion,
        })
    }

    /// The report and patch the job writes, resolved against `dir`, the folder of the job file.
    pub fn outputs(&self, dir: &Path) -> Vec<PathBuf> {
        [&self.report, &self.patch]
            .into_iter()
            .flatten()
            .filter_map(|v| expand(v).ok())
            .filter(|t| !t.trim().is_empty())
            .map(|t| {
                let p = PathBuf::from(t);
                if p.is_absolute() {
                    p
                } else {
                    dir.join(p)
                }
            })
            .collect()
    }

    /// The paths the job names, resolved against `dir`: left, base and right of a compare, or
    /// source and destination of a synchronization.
    pub fn paths(&self, dir: &Path) -> Vec<PathBuf> {
        let fields = match self.kind {
            JobKind::Compare => [&self.left, &self.base, &self.right],
            JobKind::SyncPlan | JobKind::Sync => [&self.source, &None, &self.destination],
        };
        fields
            .into_iter()
            .flatten()
            .filter_map(|v| expand(v).ok())
            .map(|t| {
                let p = PathBuf::from(t);
                if p.is_absolute() {
                    p
                } else {
                    dir.join(p)
                }
            })
            .collect()
    }

    /// A copy whose paths are relative to the folder of `job_file`, so the job and its folders
    /// can move together. URLs, rule names and `${env:…}` paths stay as they are.
    pub fn relative_to(&self, job_file: &Path) -> std::io::Result<Job> {
        let file = std::path::absolute(job_file)?;
        let base = file.parent().unwrap_or(Path::new(""));
        let rel = |v: &Option<String>| -> std::io::Result<Option<String>> {
            match v {
                Some(t) if !t.contains("${") && Path::new(t).is_absolute() => {
                    super::project::relative_path(t, base)
                        .map(Some)
                        .map_err(std::io::Error::other)
                }
                other => Ok(other.clone()),
            }
        };
        let mut j = self.clone();
        j.left = rel(&self.left)?;
        j.base = rel(&self.base)?;
        j.right = rel(&self.right)?;
        j.source = rel(&self.source)?;
        j.destination = rel(&self.destination)?;
        j.report = rel(&self.report)?;
        j.patch = rel(&self.patch)?;
        j.rules = rel(&self.rules)?;
        Ok(j)
    }

    /// Refuses a job missing what its kind needs, or giving what it does not take.
    fn check(&self) -> Result<(), String> {
        let kind = self.kind.key();
        let given = |on: bool, field: &str| -> Result<(), String> {
            if on {
                Err(trf(
                    "job.err_not_for_kind",
                    &[("field", field.into()), ("kind", kind.into())],
                ))
            } else {
                Ok(())
            }
        };
        let need = |v: &Option<String>, field: &str| -> Result<(), String> {
            match v {
                Some(s) if !s.trim().is_empty() => Ok(()),
                _ => Err(trf(
                    "job.err_needs",
                    &[("kind", kind.into()), ("field", field.into())],
                )),
            }
        };
        match self.kind {
            JobKind::Compare => {
                need(&self.left, "left")?;
                need(&self.right, "right")?;
                given(self.source.is_some(), "source")?;
                given(self.destination.is_some(), "destination")?;
                given(self.mode.is_some(), "mode")?;
                given(self.apply, "apply")?;
                given(self.allow_large_deletion, "allow_large_deletion")?;
                given(self.copy_links, "copy_links")?;
            }
            JobKind::SyncPlan | JobKind::Sync => {
                need(&self.source, "source")?;
                need(&self.destination, "destination")?;
                need(&self.mode, "mode")?;
                for (on, field) in [
                    (self.left.is_some(), "left"),
                    (self.right.is_some(), "right"),
                    (self.base.is_some(), "base"),
                    (self.compare_as.is_some(), "compare_as"),
                    (self.patch.is_some(), "patch"),
                    (self.patch_style.is_some(), "patch_style"),
                    (self.context.is_some(), "context"),
                    (self.report_diffs, "report_diffs"),
                    (self.slideshow, "slideshow"),
                    (self.page_size.is_some(), "page_size"),
                    (self.portrait, "portrait"),
                    (self.unified, "unified"),
                    (self.whole_file, "whole_file"),
                    // A synchronization always compares every subfolder.
                    (self.recursive, "recursive"),
                ] {
                    given(on, field)?;
                }
                if self.kind == JobKind::SyncPlan {
                    need(&self.report, "report")?;
                    given(self.apply, "apply")?;
                    given(self.allow_large_deletion, "allow_large_deletion")?;
                } else {
                    given(self.report.is_some(), "report")?;
                    if !self.apply {
                        return Err(tr("job.err_apply").into());
                    }
                }
            }
        }
        Ok(())
    }
}

/// `text` with each `${env:NAME}` replaced by that environment variable.
fn expand(text: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| trf("job.err_env_unclosed", &[("text", format!("{text:?}"))]))?;
        let name = after[..end]
            .strip_prefix("env:")
            .filter(|n| !n.is_empty())
            .ok_or_else(|| trf("job.err_env_only", &[("name", after[..end].to_string())]))?;
        let value = std::env::var(name)
            .map_err(|_| trf("job.err_env_unset", &[("name", name.to_string())]))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `path` as writing to it reaches it: through the links of the part that exists, with `.` and
/// `..` of the rest taken away.
fn resolved(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut at = path.as_path();
    loop {
        if let Ok(real) = at.canonicalize() {
            let mut out = real;
            for part in rest.into_iter().rev() {
                match part {
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => out.push(other),
                }
            }
            return out;
        }
        match (at.parent(), at.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                at = parent;
            }
            _ => return path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(json: &str) -> Result<Job, String> {
        let file = JobFile::parse(&format!(r#"{{"version": 1, "jobs": [{json}]}}"#))?;
        Ok(file.jobs[0].clone())
    }

    fn args(p: &Prepared) -> Vec<String> {
        p.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_compare_job_becomes_its_command_line_with_paths_beside_the_file() {
        let dir = Path::new("/jobs");
        let abs = std::env::temp_dir().join("b");
        let j = job(&format!(
            r#"{{"kind": "compare", "left": "a", "right": {}, "recursive": true,
                "filter": "*.c;!*.bak", "whitespace": "all", "ignore_case": true,
                "report": "out/r.html", "report_diffs": true}}"#,
            serde_json::to_string(&abs).unwrap()
        ))
        .unwrap();
        let p = j.prepare(dir).unwrap();
        let join = |s: &str| dir.join(s).to_string_lossy().into_owned();
        assert_eq!(
            args(&p),
            [
                "--recursive".to_string(),
                "--filter=*.c;!*.bak".into(),
                "--ignore-ws=all".into(),
                "--ignore-case".into(),
                "--report-diffs".into(),
                format!("--report={}", join("out/r.html")),
                "--".into(),
                join("a"),
                abs.to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn urls_are_left_as_they_are() {
        let p = job(r#"{"kind": "compare", "left": "https://example.com/a", "right": "b.html"}"#)
            .unwrap()
            .prepare(Path::new("/d"))
            .unwrap();
        assert!(args(&p).contains(&"https://example.com/a".to_string()));
    }

    #[test]
    fn environment_variables_are_expanded_and_nothing_else() {
        std::env::set_var("OXIDEMERGE_JOB_TEST_DIR", "/from/env");
        assert_eq!(
            expand("${env:OXIDEMERGE_JOB_TEST_DIR}/x").unwrap(),
            "/from/env/x"
        );
        assert!(expand("${env:OXIDEMERGE_JOB_TEST_UNSET_42}").is_err());
        assert!(expand("${HOME}").is_err());
        assert!(expand("${env:A").is_err());
        assert_eq!(expand("$HOME and $(ls)").unwrap(), "$HOME and $(ls)");
    }

    #[test]
    fn a_sync_job_must_say_it_applies() {
        let e = job(r#"{"kind": "sync", "source": "a", "destination": "b", "mode": "mirror"}"#)
            .unwrap()
            .prepare(Path::new("/d"))
            .unwrap_err();
        assert!(e.contains("\"apply\": true"), "{e}");
        let p = job(
            r#"{"kind": "sync", "source": "a", "destination": "b", "mode": "mirror", "apply": true}"#,
        )
        .unwrap()
        .prepare(Path::new("/d"))
        .unwrap();
        assert_eq!(args(&p)[0], "--sync-plan=mirror");
    }

    #[test]
    fn fields_of_another_kind_or_unknown_fields_are_refused() {
        let e = job(r#"{"kind": "compare", "left": "a", "right": "b", "apply": true}"#)
            .unwrap()
            .prepare(Path::new("/d"))
            .unwrap_err();
        assert!(e.contains("apply"), "{e}");
        let e =
            job(r#"{"kind": "sync_plan", "source": "a", "destination": "b", "mode": "update"}"#)
                .unwrap()
                .prepare(Path::new("/d"))
                .unwrap_err();
        assert!(e.contains("report"), "{e}");
        assert!(job(r#"{"kind": "compare", "left": "a", "right": "b", "comand": "rm"}"#).is_err());
        assert!(job(r#"{"kind": "delete_everything"}"#).is_err());
    }

    #[test]
    fn a_newer_or_empty_job_file_is_refused_and_one_round_trips() {
        assert!(JobFile::parse(r#"{"version": 2, "jobs": []}"#)
            .unwrap_err()
            .contains("version 2"));
        assert!(JobFile::parse(r#"{"version": 1, "jobs": []}"#).is_err());
        assert!(JobFile::parse(r#"{"jobs": []}"#).is_err());
        let file = JobFile::parse(
            r#"{"version": 1, "continue_on_error": true, "jobs": [
                {"name": "docs", "kind": "compare", "left": "a", "right": "b"}]}"#,
        )
        .unwrap();
        let json = serde_json::to_string_pretty(&file).unwrap();
        assert_eq!(JobFile::parse(&json).unwrap(), file);
        assert_eq!(file.jobs[0].label(0), "docs");
        assert!(
            JobFile::parse(r#"{"version": 0, "jobs": [{"left": "a", "right": "b"}]}"#).is_err()
        );
    }

    #[test]
    fn a_job_without_a_kind_compares_as_the_readme_example_shows() {
        let readme = include_str!("../../README.md");
        let start = readme.find("```json\n{\n  \"version\": 1,").unwrap() + "```json\n".len();
        let end = start + readme[start..].find("```").unwrap();
        let file = JobFile::parse(&readme[start..end]).unwrap();
        assert_eq!(file.jobs[0].kind, JobKind::Compare);
        assert!(file.jobs[0].prepare(Path::new("/d")).is_ok());
        assert_eq!(file.jobs[1].kind, JobKind::Sync);
        assert_eq!(
            file.jobs[1].label(1),
            trf("job.unnamed", &[("n", "2".into())])
        );
    }

    #[test]
    fn outputs_outside_the_job_folder_are_found_also_through_links() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = dir.path().join("jobs");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(jobs.join("out")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let file = |report: &str, patch: Option<&str>| JobFile {
            version: 1,
            continue_on_error: false,
            jobs: vec![Job {
                left: Some("a".into()),
                right: Some("b".into()),
                report: Some(report.into()),
                patch: patch.map(Into::into),
                ..Default::default()
            }],
        };
        assert!(file("out/r.html", Some("new/p.diff"))
            .outputs_outside(&jobs)
            .is_empty());
        let outside = file("../elsewhere/r.html", None).outputs_outside(&jobs);
        assert_eq!(outside, [jobs.join("../elsewhere/r.html")]);
        let abs = elsewhere.join("r.html").display().to_string();
        assert_eq!(file("r.html", Some(&abs)).outputs_outside(&jobs).len(), 1);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&elsewhere, jobs.join("link")).unwrap();
            assert_eq!(file("link/r.html", None).outputs_outside(&jobs).len(), 1);
        }
    }
}
