//! Portable folder reviews: bounded ZIP files with data and declarative settings only.
//! Converter commands and external rules/filter dependencies are never executed on reopen.
use super::{
    diff::DiffOptions,
    folder::{DirItem, FolderOptions},
    fsutil, unpacker,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const EXT: &str = "oxmreview";
const MAX_BYTES: u64 = 256 << 20;
const MAX_ENTRIES: usize = 10_000;
const MAX_MANIFEST: u64 = 4 << 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub side: usize,
    pub path: String,
    pub directory: bool,
    pub modified: Option<SystemTime>,
    /// Text extracted at capture time instead of the original document container.
    pub converted: bool,
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub saved: String,
    pub labels: Vec<String>,
    pub options: DiffOptions,
    pub folder: FolderOptions,
    pub filter: String,
    pub notes: BTreeMap<String, String>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub workspace: super::review_state::Workspace,
}

pub struct Review {
    pub manifest: Manifest,
    pub roots: Vec<PathBuf>,
    pub temp: tempfile::TempDir,
}

pub(crate) fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && !p.ends_with(['.', ' '])
                && !p.chars().any(|c| c.is_control())
                && ![
                    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
                    "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
                    "LPT8", "LPT9",
                ]
                .contains(
                    &p.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                )
        })
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        if !(1..=2).contains(&self.version) {
            bail!(
                "Unsupported folder review version {}: it was saved by a newer OxideMerge; \
                 update OxideMerge to open it",
                self.version
            );
        }
        if !(2..=3).contains(&self.labels.len()) {
            bail!("A folder review needs 2 or 3 sides");
        }
        if self.entries.len() > MAX_ENTRIES {
            bail!("Folder review contains too many entries");
        }
        let mut seen = HashSet::new();
        for entry in &self.entries {
            if entry.side >= self.labels.len()
                || !safe_path(&entry.path)
                || !seen.insert((entry.side, entry.path.to_lowercase()))
            {
                bail!("Invalid or duplicate folder review path: {}", entry.path);
            }
        }
        self.workspace.validate()?;
        if self.notes.values().any(|n| n.len() > 16_384) {
            bail!("Review comment is too long");
        }
        if self.notes.keys().any(|p| !safe_path(p)) {
            bail!("Invalid annotation path");
        }
        Ok(())
    }
}

/// Only the supplied items are included. Their effective filter/rules have already been
/// applied; capture the resulting data, not dependencies on another machine's settings.
pub struct Capture<'a> {
    pub roots: &'a [PathBuf],
    pub labels: &'a [String],
    pub items: &'a [&'a DirItem],
    pub options: &'a DiffOptions,
    pub folder: &'a FolderOptions,
    pub filter: &'a str,
    pub notes: &'a BTreeMap<String, String>,
}

#[cfg(test)]
pub fn save(path: &Path, capture: Capture<'_>) -> Result<()> {
    save_with_workspace(path, capture, &super::review_state::Workspace::default())
}
pub fn save_with_workspace(
    path: &Path,
    capture: Capture<'_>,
    workspace: &super::review_state::Workspace,
) -> Result<()> {
    let Capture {
        roots,
        labels,
        items,
        options,
        folder,
        filter,
        notes,
    } = capture;
    if roots.len() != labels.len() || !(2..=3).contains(&roots.len()) {
        bail!("A folder review needs 2 or 3 sides");
    }
    let mut manifest = Manifest {
        version: 2,
        saved: chrono::Local::now().to_rfc3339(),
        labels: labels.to_vec(),
        options: options.clone(),
        folder: folder.clone(),
        filter: filter.into(),
        notes: BTreeMap::new(),
        entries: Vec::new(),
        workspace: workspace.clone(),
    };
    manifest.options.unpackers.clear();
    manifest
        .workspace
        .bind_policy(&manifest.options, &manifest.folder, &manifest.filter);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let zip_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0u64;
    let output_path = std::path::absolute(path)?;
    for item in items {
        if item.status == super::folder::CmpStatus::Error
            || item.status == super::folder::CmpStatus::Pending
        {
            bail!(
                "Finish a successful comparison before capturing {}",
                item.rel_path()
            );
        }
        let rel = item.rel_path();
        if !safe_path(&rel) {
            bail!("Path cannot be stored portably: {rel}");
        }
        if let Some(note) = notes.get(&rel) {
            manifest.notes.insert(rel.clone(), note.clone());
        }
        for (side, root) in roots.iter().enumerate() {
            if !item.exists[side] {
                continue;
            }
            if manifest.entries.len() >= MAX_ENTRIES {
                bail!("Folder review exceeds {MAX_ENTRIES} entries");
            }
            let input = item.path(root);
            if std::path::absolute(&input)? == output_path {
                bail!("Save the review outside its captured input files");
            }
            let canonical_root = std::fs::canonicalize(root)?;
            if !std::fs::canonicalize(&input)?.starts_with(&canonical_root) {
                bail!("Review input escapes its root: {rel}");
            }
            let md = std::fs::symlink_metadata(&input)?;
            if md.file_type().is_symlink() || (!md.is_file() && !md.is_dir()) {
                bail!("Cannot capture a link or special file: {rel}");
            }
            if md.is_dir() != item.is_dir
                || (!item.is_dir
                    && (item.size[side] != Some(md.len())
                        || item.mtime[side] != md.modified().ok()))
            {
                bail!("File changed since comparison; refresh before saving: {rel}");
            }
            let converted = folder.method == super::folder::CompareMethod::FullContents
                && !item.is_dir
                && unpacker::find(&options.unpackers, &input).is_some();
            let mut entry = Entry {
                side,
                path: rel.clone(),
                directory: item.is_dir,
                modified: md.modified().ok(),
                converted,
                digest: None,
            };
            let name = format!("data/{side}/{rel}");
            if item.is_dir {
                writer.add_directory(format!("{name}/"), zip_options)?;
            } else {
                let bytes = if converted {
                    unpacker::load_doc(&input, &options.unpackers, None)?
                        .text()
                        .into_bytes()
                } else {
                    if md.len() > MAX_BYTES - total {
                        bail!("Folder review exceeds 256 MiB");
                    }
                    let mut bytes = Vec::new();
                    std::fs::File::open(&input)?
                        .take(MAX_BYTES - total + 1)
                        .read_to_end(&mut bytes)?;
                    bytes
                };
                total += bytes.len() as u64;
                if total > MAX_BYTES {
                    bail!("Folder review exceeds 256 MiB");
                }
                let after = std::fs::metadata(&input)?;
                if after.len() != md.len() || after.modified().ok() != md.modified().ok() {
                    bail!("File changed while capturing: {rel}");
                }
                entry.digest = Some(super::analysis::digest(&bytes));
                writer.start_file(name, zip_options)?;
                writer.write_all(&bytes)?;
            }
            manifest.entries.push(entry);
        }
    }
    let captured: HashSet<_> = manifest
        .entries
        .iter()
        .filter(|e| !e.directory)
        .map(|e| e.path.clone())
        .collect();
    manifest
        .workspace
        .entries
        .retain(|path, _| captured.contains(path));
    if manifest
        .workspace
        .current
        .as_ref()
        .is_some_and(|p| !captured.contains(p))
    {
        manifest.workspace.current = None;
    }
    // A decision still current for the compared files holds for what the review stores, which
    // for a converted document is its text, compared without converters.
    let digests: BTreeMap<(usize, &str), &str> = manifest
        .entries
        .iter()
        .filter_map(|e| Some(((e.side, e.path.as_str()), e.digest.as_deref()?)))
        .collect();
    for (path, decision) in &mut manifest.workspace.entries {
        if decision.stale {
            continue;
        }
        let sides: Vec<_> = (0..manifest.labels.len())
            .map(|side| digests.get(&(side, path.as_str())).map(|d| d.to_string()))
            .collect();
        let prints = super::review_state::prints_of_digests(path, &sides, &manifest.options);
        decision.fingerprint = prints.fingerprint;
        decision.content = prints.content;
    }
    manifest.validate()?;
    let data = serde_json::to_vec_pretty(&manifest)?;
    if data.len() as u64 > MAX_MANIFEST {
        bail!("Review manifest is too large");
    }
    writer.start_file("manifest.json", zip_options)?;
    writer.write_all(&data)?;
    let bytes = writer.finish()?.into_inner();
    fsutil::write_atomic(path, &bytes)
}

pub fn load(path: &Path) -> Result<Review> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    if zip.len() > MAX_ENTRIES + 1 {
        bail!("Folder review contains too many entries");
    }
    let mut data = Vec::new();
    zip.by_name("manifest.json")?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_MANIFEST {
        bail!("Review manifest is too large");
    }
    let mut manifest: Manifest =
        serde_json::from_slice(&data).context("Invalid folder review manifest")?;
    manifest.validate()?;
    manifest.options.unpackers.clear();
    if zip.len() != manifest.entries.len() + 1 {
        bail!("Folder review has unlisted or missing entries");
    }
    let temp = tempfile::tempdir()?;
    let roots: Vec<PathBuf> = (0..manifest.labels.len())
        .map(|s| temp.path().join(s.to_string()))
        .collect();
    for root in &roots {
        std::fs::create_dir(root)?;
    }
    let mut total = 0;
    let mut names = HashSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if !names.insert(entry.name().to_string()) {
            bail!("Duplicate ZIP entry");
        }
    }
    let mut captured_digests = BTreeMap::new();
    for entry in &manifest.entries {
        let name = format!(
            "data/{}/{}{}",
            entry.side,
            entry.path,
            if entry.directory { "/" } else { "" }
        );
        let mut file = zip
            .by_name(&name)
            .with_context(|| format!("Missing review content: {}", entry.path))?;
        if file.is_dir() != entry.directory
            || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            bail!("Invalid review entry type");
        }
        let dest = roots[entry.side].join(&entry.path);
        if entry.directory {
            std::fs::create_dir_all(&dest)?;
        } else {
            if file.size() > MAX_BYTES - total {
                bail!("Folder review exceeds 256 MiB");
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(MAX_BYTES - total + 1)
                .read_to_end(&mut bytes)?;
            total += bytes.len() as u64;
            if total > MAX_BYTES {
                bail!("Folder review exceeds 256 MiB");
            }
            let digest = super::analysis::digest(&bytes);
            if entry
                .digest
                .as_ref()
                .is_some_and(|expected| *expected != digest)
            {
                bail!("Review content fingerprint mismatch: {}", entry.path);
            }
            captured_digests.insert((entry.side, entry.path.clone()), digest);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            let mut output = std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&dest)?;
            output.write_all(&bytes)?;
            if let Some(time) = entry.modified {
                output.set_times(std::fs::FileTimes::new().set_modified(time))?;
            }
        }
    }
    let fingerprints = manifest
        .entries
        .iter()
        .filter(|e| !e.directory)
        .map(|entry| {
            let sides: Vec<_> = (0..manifest.labels.len())
                .map(|side| captured_digests.get(&(side, entry.path.clone())).cloned())
                .collect();
            (
                entry.path.clone(),
                super::review_state::prints_of_digests(&entry.path, &sides, &manifest.options),
            )
        })
        .collect();
    manifest
        .workspace
        .bind_policy(&manifest.options, &manifest.folder, &manifest.filter);
    manifest.workspace.reconcile(&fingerprints);
    Ok(Review {
        manifest,
        roots,
        temp,
    })
}

/// Include folder annotations in exported reviews without treating note text as markup.
pub fn annotate_report(
    mut content: String,
    format: super::report::ReportFormat,
    notes: &BTreeMap<String, String>,
) -> String {
    if notes.is_empty() {
        return content;
    }
    use super::report::{esc, ReportFormat};
    match format {
        ReportFormat::Html | ReportFormat::Slideshow => {
            let mut section = format!(
                "<aside><h2>{}</h2><dl>",
                esc(crate::i18n::tr("review.notes"))
            );
            for (path, note) in notes {
                section.push_str(&format!("<dt>{}</dt><dd>{}</dd>", esc(path), esc(note)));
            }
            section.push_str("</dl></aside>");
            let at = content.rfind("</body>").unwrap_or(content.len());
            content.insert_str(at, &section);
        }
        ReportFormat::Xml => {
            let mut section = String::from("<annotations>");
            for (path, note) in notes {
                section.push_str(&format!(
                    "<annotation path=\"{}\">{}</annotation>",
                    esc(path),
                    esc(note)
                ));
            }
            section.push_str("</annotations>");
            let at = content.rfind("</comparison>").unwrap_or(content.len());
            content.insert_str(at, &section);
        }
        _ => {
            // Folder CSV stays rectangular; comments are carried by HTML/XML and bundles.
            if format == ReportFormat::Text {
                content.push_str(&format!("\n{}\n", crate::i18n::tr("review.notes")));
                for (path, note) in notes {
                    content.push_str(&format!("{path}: {note}\n"));
                }
            }
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        filter::FileFilter,
        folder::{Progress, Scanner},
    };
    #[test]
    fn portable_folder_review_carries_bytes_options_and_comments() {
        let original = tempfile::tempdir().unwrap();
        let roots: Vec<PathBuf> = ["left", "right"]
            .iter()
            .map(|p| original.path().join(p))
            .collect();
        for root in &roots {
            std::fs::create_dir_all(root.join("empty")).unwrap();
        }
        std::fs::write(roots[0].join("a.txt"), b"one\r\n").unwrap();
        std::fs::write(roots[1].join("a.txt"), b"TWO\n").unwrap();
        std::fs::write(roots[0].join("binary.bin"), [0, 255, 1]).unwrap();
        let opts = DiffOptions {
            ignore_case: true,
            ..Default::default()
        };
        let folder = FolderOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &folder,
            dopts: &opts,
            progress: &Progress::default(),
        }
        .run();
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("review.oxmreview");
        save(
            &path,
            Capture {
                roots: &roots,
                labels: &["Left".into(), "Right".into()],
                items: &items.iter().collect::<Vec<_>>(),
                options: &opts,
                folder: &folder,
                filter: "*",
                notes: &BTreeMap::from([("a.txt".into(), "Check this change".into())]),
            },
        )
        .unwrap();
        drop(original);
        let review = load(&path).unwrap();
        assert_eq!(
            std::fs::read(review.roots[0].join("a.txt")).unwrap(),
            b"one\r\n"
        );
        assert_eq!(
            std::fs::read(review.roots[0].join("binary.bin")).unwrap(),
            [0, 255, 1]
        );
        assert!(review.roots[1].join("empty").is_dir());
        assert!(!review.roots[1].join("binary.bin").exists());
        assert!(review.manifest.options.ignore_case);
        assert_eq!(review.manifest.notes["a.txt"], "Check this change");
        assert!(review.manifest.options.unpackers.is_empty());
    }
    #[test]
    fn persistent_review_states_round_trip_and_corrupt_content_is_rejected() {
        use crate::core::review_state::{Decision, State, Workspace};
        let original = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|p| original.path().join(p))
            .collect();
        for (side, root) in roots.iter().enumerate() {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("a.txt"), format!("side {side}\n")).unwrap();
        }
        let opts = DiffOptions::default();
        let folder = FolderOptions::default();
        let items = Scanner {
            roots: &roots,
            filter: &FileFilter::default(),
            fopts: &folder,
            dopts: &opts,
            progress: &Progress::default(),
        }
        .run();
        let fingerprint = crate::core::review_state::prints("a.txt", &roots, &opts)
            .unwrap()
            .fingerprint;
        let mut workspace = Workspace {
            current: Some("a.txt".into()),
            ..Default::default()
        };
        workspace.policy = crate::core::review_state::policy_digest(&opts, &folder, "*");
        workspace.entries.insert(
            "a.txt".into(),
            Decision {
                id: fingerprint.clone(),
                fingerprint,
                state: State::Accepted,
                comment: "Review <safe> & complete".into(),
                tags: vec!["release".into()],
                hunks: BTreeMap::from([(
                    "hunk-id".into(),
                    Box::new(Decision {
                        state: State::NeedsAction,
                        comment: "Check this hunk".into(),
                        ..Default::default()
                    }),
                )]),
                ..Default::default()
            },
        );
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("review.oxmreview");
        save_with_workspace(
            &path,
            Capture {
                roots: &roots,
                labels: &["Left".into(), "Right".into()],
                items: &items.iter().collect::<Vec<_>>(),
                options: &opts,
                folder: &folder,
                filter: "*",
                notes: &BTreeMap::new(),
            },
            &workspace,
        )
        .unwrap();
        drop(original);
        let review = load(&path).unwrap();
        assert_eq!(review.manifest.version, 2);
        assert_eq!(review.manifest.workspace.current.as_deref(), Some("a.txt"));
        assert_eq!(
            review.manifest.workspace.entries["a.txt"].state,
            State::Accepted
        );
        assert!(!review.manifest.workspace.entries["a.txt"].stale);
        assert_eq!(
            review.manifest.workspace.summary(["a.txt".into()]).accepted,
            1
        );

        assert_eq!(
            review.manifest.workspace.entries["a.txt"].hunks["hunk-id"].state,
            State::NeedsAction
        );
        assert!(review.manifest.entries.iter().all(|e| e.digest.is_some()));
        // Repack a well-formed ZIP with one changed payload; CRCs are valid, captured SHA-256 is not.
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let name = entry.name().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "data/0/a.txt" {
                bytes = b"tampered\n".to_vec();
            }
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let corrupted = out.path().join("corrupt.oxmreview");
        std::fs::write(&corrupted, writer.finish().unwrap().into_inner()).unwrap();
        assert!(load(&corrupted)
            .err()
            .unwrap()
            .to_string()
            .contains("fingerprint mismatch"));
    }

    #[test]
    fn rejects_unsafe_and_duplicate_paths_and_future_versions() {
        for path in [
            "../escape",
            "/absolute",
            "a/../b",
            "C:/drive",
            "a\\b",
            "a//b",
            "a/CON.txt",
            "a.",
        ] {
            assert!(!safe_path(path), "{path}");
        }
        let mut manifest = Manifest {
            version: 1,
            saved: String::new(),
            labels: vec!["a".into(), "b".into()],
            options: DiffOptions::default(),
            folder: FolderOptions::default(),
            filter: "*".into(),
            notes: BTreeMap::new(),
            entries: vec![],
            workspace: Default::default(),
        };
        manifest.entries.push(Entry {
            side: 0,
            path: "../escape".into(),
            directory: false,
            modified: None,
            converted: false,
            digest: None,
        });
        assert!(manifest.validate().is_err());
        manifest.entries[0].path = "A.txt".into();
        manifest.entries.push(Entry {
            path: "a.txt".into(),
            ..manifest.entries[0].clone()
        });
        assert!(manifest.validate().is_err());
        manifest.entries.clear();
        manifest.version = 3;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn hostile_review_archives_are_rejected_without_writing_outside_the_review() {
        let dir = tempfile::tempdir().unwrap();
        let entry = |path: &str| {
            serde_json::json!({
                "side": 0, "path": path, "directory": false, "modified": null, "converted": false
            })
        };
        let manifest = |entries: Vec<serde_json::Value>| {
            serde_json::json!({
                "version": 2,
                "saved": "",
                "labels": ["Left", "Right"],
                "options": DiffOptions::default(),
                "folder": FolderOptions::default(),
                "filter": "*",
                "notes": {},
                "entries": entries,
            })
        };
        enum Item<'a> {
            File(&'a str, &'a [u8]),
            Link(&'a str, &'a str),
        }
        let archive = |name: &str, manifest: &[u8], items: &[Item]| {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("manifest.json", options).unwrap();
            writer.write_all(manifest).unwrap();
            for item in items {
                match item {
                    Item::File(path, bytes) => {
                        writer.start_file(*path, options).unwrap();
                        writer.write_all(bytes).unwrap();
                    }
                    Item::Link(path, target) => {
                        writer.add_symlink(*path, *target, options).unwrap()
                    }
                }
            }
            let path = dir.path().join(name);
            std::fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
            path
        };
        let rejected = |path: &Path, expected: &str| {
            let error = load(path).err().map(|e| format!("{e:#}"));
            assert!(
                error.as_deref().is_some_and(|e| e.contains(expected)),
                "{}: {error:?}, expected {expected:?}",
                path.display()
            );
        };
        let json = |v: serde_json::Value| serde_json::to_vec(&v).unwrap();

        let fine = archive(
            "fine.oxmreview",
            &json(manifest(vec![entry("a.txt")])),
            &[Item::File("data/0/a.txt", b"a\n")],
        );
        assert!(load(&fine).is_ok());

        let slip = archive(
            "slip.oxmreview",
            &json(manifest(vec![entry("../../evil.txt")])),
            &[Item::File("data/0/../../evil.txt", b"evil\n")],
        );
        rejected(&slip, "Invalid or duplicate folder review path");
        let hidden = archive(
            "hidden.oxmreview",
            &json(manifest(vec![entry("a.txt")])),
            &[
                Item::File("data/0/a.txt", b"a\n"),
                Item::File("data/0/../../evil.txt", b"evil\n"),
            ],
        );
        rejected(&hidden, "unlisted or missing entries");
        let link = archive(
            "link.oxmreview",
            &json(manifest(vec![entry("a.txt")])),
            &[Item::Link("data/0/a.txt", "/etc/passwd")],
        );
        rejected(&link, "Invalid review entry type");
        let missing = archive(
            "missing.oxmreview",
            &json(manifest(vec![entry("a.txt"), entry("b.txt")])),
            &[
                Item::File("data/0/a.txt", b"a\n"),
                Item::File("data/0/c.txt", b"c\n"),
            ],
        );
        rejected(&missing, "Missing review content: b.txt");
        let mut typed = manifest(vec![entry("a.txt")]);
        typed["entries"][0]["side"] = "zero".into();
        let typed = archive(
            "typed.oxmreview",
            &json(typed),
            &[Item::File("data/0/a.txt", b"a\n")],
        );
        rejected(&typed, "Invalid folder review manifest");
        let mut newer = manifest(vec![]);
        newer["version"] = 3.into();
        let newer = archive("newer.oxmreview", &json(newer), &[]);
        rejected(&newer, "update OxideMerge to open it");
        let mut huge = json(manifest(vec![]));
        huge.splice(1..1, std::iter::repeat_n(b' ', MAX_MANIFEST as usize));
        let huge = archive("huge.oxmreview", &huge, &[]);
        rejected(&huge, "Review manifest is too large");
        let mut long = manifest(vec![]);
        long["notes"] = serde_json::json!({ "a.txt": "x".repeat(16_385) });
        let long = archive("long.oxmreview", &json(long), &[]);
        rejected(&long, "Review comment is too long");
        let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("data/0/{i}")).collect();
        let many: Vec<Item> = names.iter().map(|n| Item::File(n, b"")).collect();
        let many = archive("many.oxmreview", &json(manifest(vec![])), &many);
        rejected(&many, "too many entries");

        assert!(!dir.path().join("evil.txt").exists());
        assert!(!dir.path().parent().unwrap().join("evil.txt").exists());
    }

    /// Saved by OxideMerge 0.9.6 itself: two sides compared ignoring case, with notes and
    /// decisions on a file and on one in a subfolder.
    const REVIEW_0_9_6: &[u8] = include_bytes!("../../tests/fixtures/review-0.9.6.oxmreview");

    #[test]
    fn reviews_saved_by_0_9_6_open_with_their_decisions_current() {
        use crate::core::review_state::State;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.oxmreview");
        std::fs::write(&path, REVIEW_0_9_6).unwrap();
        let review = load(&path).unwrap();
        let manifest = &review.manifest;
        assert_eq!(manifest.version, 2);
        assert_eq!(manifest.labels, ["Release", "Main"]);
        assert!(manifest.options.ignore_case);
        assert_eq!(manifest.filter, "*");
        assert_eq!(manifest.notes["a.txt"], "Note from 0.9.6");
        let read = |side: usize, rel: &str| std::fs::read(review.roots[side].join(rel)).unwrap();
        assert_eq!(read(0, "a.txt"), b"one\r\ntwo\r\n");
        assert_eq!(read(1, "a.txt"), b"ONE\nthree\n");
        assert_eq!(read(0, "sub/c.txt"), "caf\u{e9}\n".as_bytes());
        assert_eq!(read(0, "only_left.bin"), [0, 255, 1]);
        assert!(!review.roots[1].join("only_left.bin").exists());
        assert!(review.roots[1].join("empty").is_dir());

        let workspace = &manifest.workspace;
        assert_eq!(workspace.current.as_deref(), Some("sub/c.txt"));
        let a = &workspace.entries["a.txt"];
        assert_eq!(a.state, State::Accepted);
        assert_eq!(a.reviewer, "Kim");
        assert_eq!(a.comment, "Line endings only <ok> & fine");
        assert_eq!(a.rationale, "Checked by hand");
        assert_eq!(a.tags, ["release"]);
        assert_eq!(a.hunks["hunk-1"].state, State::NeedsAction);
        assert_eq!(a.hunks["hunk-1"].comment, "Check this hunk");
        let c = &workspace.entries["sub/c.txt"];
        assert_eq!(c.state, State::InReview);
        assert_eq!(c.comment, "Accent dropped");
        // what the folder view computes again from the files it opens
        for (rel, decision) in [("a.txt", a), ("sub/c.txt", c)] {
            assert!(!decision.stale, "{rel}");
            let now = crate::core::review_state::prints(rel, &review.roots, &manifest.options);
            assert_eq!(now.unwrap().fingerprint, decision.fingerprint, "{rel}");
        }
    }
}
