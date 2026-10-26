//! Compare rules files (`*.oxrules`): lines to ignore, substitutions and blocks to ignore,
//! kept apart from the options so that each comparison can use the set it needs.

use crate::config::Config;
use crate::core::diff::DiffOptions;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const EXT: &str = "oxrules";
/// Name given to the rules taken over from the options of earlier versions.
pub const DEFAULT_NAME: &str = "Default";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Substitution {
    pub find: String,
    pub replace: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Block {
    pub start: String,
    pub end: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct RuleSet {
    /// The file name without its extension.
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub description: String,
    /// Lines matching any of these regexes are not compared.
    pub ignore_lines: Vec<String>,
    /// Matches are replaced in both files before comparing.
    pub substitutions: Vec<Substitution>,
    /// Lines from one matching `start` to the next one matching `end` are not compared.
    pub ignore_blocks: Vec<Block>,
}

impl RuleSet {
    pub fn new(dir: &Path, name: &str) -> RuleSet {
        let name = file_name(name);
        RuleSet {
            path: dir.join(format!("{name}.{EXT}")),
            name,
            ..RuleSet::default()
        }
    }

    pub fn load(path: &Path) -> Option<RuleSet> {
        let text = std::fs::read_to_string(path).ok()?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let mut set: RuleSet = serde_json::from_str(text).ok()?;
        set.name = path.file_stem()?.to_string_lossy().into_owned();
        set.path = path.to_path_buf();
        Some(set)
    }

    /// Writes the file through a temporary one, so it is never left half written, keeping what
    /// the file was (its permissions, or a link to it). No backup is kept: the Rules folder would
    /// fill with them.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(d) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d)?;
        }
        let mut text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        text.push('\n');
        crate::core::fsutil::write_atomic_with(&self.path, text.as_bytes(), false)
            .map_err(std::io::Error::other)
    }

    pub fn is_empty(&self) -> bool {
        self.ignore_lines.is_empty()
            && self.substitutions.is_empty()
            && self.ignore_blocks.is_empty()
    }

    pub fn same_rules(&self, other: &RuleSet) -> bool {
        self.ignore_lines == other.ignore_lines
            && self.substitutions == other.substitutions
            && self.ignore_blocks == other.ignore_blocks
    }

    /// Replaces the rules of `opts` with these.
    pub fn apply(&self, opts: &mut DiffOptions) {
        opts.line_filters = self.ignore_lines.clone();
        opts.substitutions = self
            .substitutions
            .iter()
            .map(|s| (s.find.clone(), s.replace.clone()))
            .collect();
        opts.block_filters = self
            .ignore_blocks
            .iter()
            .map(|b| (b.start.clone(), b.end.clone()))
            .collect();
        opts.line_filters_enabled = !opts.line_filters.is_empty();
        opts.substitutions_enabled = !opts.substitutions.is_empty();
        opts.block_filters_enabled = !opts.block_filters.is_empty();
    }
}

/// The kinds of rules, for adding one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuleKind {
    IgnoreLine,
    Substitute,
    IgnoreBlock,
}

impl RuleKind {
    pub const ALL: [RuleKind; 3] = [
        RuleKind::IgnoreLine,
        RuleKind::Substitute,
        RuleKind::IgnoreBlock,
    ];

    pub fn key(self) -> &'static str {
        match self {
            RuleKind::IgnoreLine => "rules.ignore_line",
            RuleKind::Substitute => "rules.substitute",
            RuleKind::IgnoreBlock => "rules.ignore_block",
        }
    }
}

/// A rule about to be added: the pattern, and the replacement or block end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuleDraft {
    pub kind: RuleKind,
    pub a: String,
    pub b: String,
}

impl RuleDraft {
    /// The first pattern that is not a valid regex, or an empty one that must be given.
    pub fn problem(&self) -> Option<String> {
        let patterns: &[&str] = match self.kind {
            RuleKind::IgnoreBlock => &[&self.a, &self.b],
            _ => &[&self.a],
        };
        patterns
            .iter()
            .find(|p| p.trim().is_empty() || regex::Regex::new(p).is_err())
            .map(|p| p.to_string())
    }
}

impl RuleSet {
    pub fn add(&mut self, rule: &RuleDraft) {
        match rule.kind {
            RuleKind::IgnoreLine => self.ignore_lines.push(rule.a.clone()),
            RuleKind::Substitute => self.substitutions.push(Substitution {
                find: rule.a.clone(),
                replace: rule.b.clone(),
            }),
            RuleKind::IgnoreBlock => self.ignore_blocks.push(Block {
                start: rule.a.clone(),
                end: rule.b.clone(),
            }),
        }
    }

    pub fn count(&self) -> usize {
        self.ignore_lines.len() + self.substitutions.len() + self.ignore_blocks.len()
    }
}

/// `opts` with no rules at all.
pub fn clear(opts: &mut DiffOptions) {
    RuleSet::default().apply(opts);
}

/// `base` with its rules replaced by those of `set` (none when there is no set).
pub fn options_with(base: &DiffOptions, set: Option<&RuleSet>) -> DiffOptions {
    let mut opts = base.clone();
    match set {
        Some(s) => s.apply(&mut opts),
        None => clear(&mut opts),
    }
    opts
}

/// A name usable as a file name.
pub fn file_name(name: &str) -> String {
    let name = name.trim();
    let name = match name.len().checked_sub(EXT.len() + 1) {
        Some(i)
            if name.is_char_boundary(i)
                && name[i..].starts_with('.')
                && name[i + 1..].eq_ignore_ascii_case(EXT) =>
        {
            &name[..i]
        }
        _ => name,
    };
    let s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let s = s.trim_matches('.').trim().to_string();
    if s.is_empty() {
        DEFAULT_NAME.into()
    } else {
        s
    }
}

/// Where new rules files are saved: next to the settings file.
pub fn dir() -> Option<PathBuf> {
    crate::config::config_path().and_then(|p| p.parent().map(|d| d.join("Rules")))
}

/// Folders searched for rules files; files of a later folder win over those of the same name.
pub fn dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("Rules")))
    {
        v.push(d);
    }
    if let Some(d) = dir() {
        if !v.contains(&d) {
            v.push(d);
        }
    }
    v
}

/// Every rules file found in `dirs`, sorted by name.
pub fn list_in(dirs: &[PathBuf]) -> Vec<RuleSet> {
    let mut all: Vec<RuleSet> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let here: Vec<RuleSet> = rd
            .flatten()
            .map(|e| e.path())
            .filter_map(|p| is_rules_file(&p).then(|| RuleSet::load(&p)).flatten())
            .collect();
        // Folders where case matters can hold names differing only in case; both are listed.
        all.retain(|x| !here.iter().any(|h| same_name(&h.name, &x.name)));
        all.extend(here);
    }
    all.sort_by_key(|s| s.name.to_lowercase());
    all
}

pub fn list() -> Vec<RuleSet> {
    list_in(&dirs())
}

/// The rules named `name` ("" = none), or those of a file when `name` is its path.
pub fn find_in(dirs: &[PathBuf], name: &str) -> Option<RuleSet> {
    if name.is_empty() {
        return None;
    }
    let p = Path::new(name);
    if is_rules_file(p) && p.is_file() {
        return RuleSet::load(p);
    }
    // "Logs.oxrules" names the rules "Logs"; a path to a missing file names nothing.
    let bare = p.parent().is_some_and(|d| d.as_os_str().is_empty());
    let name = if bare { display_name(name) } else { name };
    let all = list_in(dirs);
    let exact = all.iter().position(|s| s.name == name);
    let at = exact.or_else(|| all.iter().position(|s| same_name(&s.name, name)))?;
    all.into_iter().nth(at)
}

/// Whether two rules names are the same, letters compared regardless of case.
pub fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

pub fn find(name: &str) -> Option<RuleSet> {
    find_in(&dirs(), name)
}

/// How a rules choice (a name or the path of a file) is shown.
pub fn display_name(name: &str) -> &str {
    let p = Path::new(name);
    match p.file_stem().and_then(|s| s.to_str()) {
        Some(stem) if p.extension().is_some_and(|x| x.eq_ignore_ascii_case(EXT)) => stem,
        _ => name,
    }
}

/// Moves the rules of the options of earlier versions into rules files in `dir`, once: the
/// enabled ones into "Default", which becomes the selected set, and the others into
/// "Default (off)". Returns whether `cfg` changed; on an error it is left as it was, to try
/// again next time.
pub fn migrate_in(dir: &Path, cfg: &mut Config) -> std::io::Result<bool> {
    if cfg.rules_migrated {
        return Ok(false);
    }
    if !cfg.rules.is_empty() {
        cfg.rules_migrated = true;
        return Ok(true);
    }
    let d = &cfg.diff;
    let mut sets = [
        RuleSet::new(dir, DEFAULT_NAME),
        RuleSet::new(dir, &format!("{DEFAULT_NAME} (off)")),
    ];
    let at = |enabled: bool| usize::from(!enabled);
    sets[at(d.line_filters_enabled)].ignore_lines = d.line_filters.clone();
    sets[at(d.substitutions_enabled)].substitutions = d
        .substitutions
        .iter()
        .map(|(f, r)| Substitution {
            find: f.clone(),
            replace: r.clone(),
        })
        .collect();
    sets[at(d.block_filters_enabled)].ignore_blocks = d
        .block_filters
        .iter()
        .map(|(s, e)| Block {
            start: s.clone(),
            end: e.clone(),
        })
        .collect();
    let existing = list_in(&[dir.to_path_buf()]);
    let keep = |set: RuleSet| -> std::io::Result<Option<String>> {
        if set.is_empty() {
            return Ok(None);
        }
        if let Some(e) = existing.iter().find(|e| e.same_rules(&set)) {
            return Ok(Some(e.name.clone()));
        }
        let mut set = set;
        let base = set.name.clone();
        let mut n = 2;
        while set.path.exists() {
            let fresh = RuleSet::new(dir, &format!("{base} {n}"));
            set.name = fresh.name;
            set.path = fresh.path;
            n += 1;
        }
        set.save()?;
        Ok(Some(set.name))
    };
    let [on, off] = sets;
    let selected = keep(on)?;
    keep(off)?;
    cfg.rules_migrated = true;
    if let Some(name) = selected {
        cfg.rules = name;
    }
    Ok(true)
}

/// Brings the options up to date with the selected rules file. A selected file that cannot be
/// read stays selected, compared without, so a broken or unplugged one is not forgotten.
pub fn apply_selected(cfg: &mut Config) {
    cfg.diff = options_with(&cfg.diff, find(&cfg.rules).as_ref());
}

/// Whether `path` names a rules file.
/// `name` (a rules name or path) with a relative path made absolute against `base`, where it was
/// given: a bare `Logs.oxrules` stays a name unless that file is there.
pub fn absolute_spec(name: &str, base: &Path) -> String {
    let p = Path::new(name);
    let bare = p.parent().is_some_and(|d| d.as_os_str().is_empty());
    if name.is_empty() || p.is_absolute() || (bare && !is_rules_file(p)) {
        return name.to_string();
    }
    let abs = base.join(p);
    if bare && !abs.is_file() {
        return name.to_string();
    }
    abs.display().to_string()
}

pub fn is_rules_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|x| x.eq_ignore_ascii_case(EXT))
}

/// What importing rules files did, by name.
#[derive(Default, Debug)]
pub struct Imported {
    pub added: Vec<String>,
    /// Files whose rules were already there under the same name.
    pub present: Vec<String>,
    /// Files that could not be read as rules files.
    pub failed: Vec<String>,
    /// Rules that could not be saved, with the reason.
    pub unsaved: Vec<String>,
}

/// Copies the rules files `files` into `dir`. A file with the name of one already there but
/// other rules is added under a free name ("Name 2", ...); one with the same name and rules there
/// or in `others` is left out.
pub fn import_into(dir: &Path, files: &[PathBuf], others: &[PathBuf]) -> Imported {
    let mut done = Imported::default();
    for file in files {
        let shown = file.file_name().map_or_else(
            || file.display().to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        let Some(set) = RuleSet::load(file) else {
            done.failed.push(shown);
            continue;
        };
        let existing = list_in(&[dir.to_path_buf()]);
        let base = file_name(&set.name);
        let mut known = others.to_vec();
        known.push(dir.to_path_buf());
        if let Some(e) = list_in(&known)
            .iter()
            .find(|e| same_name(&e.name, &base) && e.same_rules(&set))
        {
            done.present.push(e.name.clone());
            continue;
        }
        let mut copy = RuleSet {
            path: PathBuf::new(),
            ..set
        };
        let mut n = 1;
        loop {
            let name = if n == 1 {
                base.clone()
            } else {
                format!("{base} {n}")
            };
            let fresh = RuleSet::new(dir, &name);
            let taken =
                fresh.path.exists() || existing.iter().any(|e| same_name(&e.name, &fresh.name));
            if !taken {
                copy.name = fresh.name;
                copy.path = fresh.path;
                break;
            }
            n += 1;
        }
        match copy.save() {
            Ok(()) => done.added.push(copy.name),
            Err(e) => done.unsaved.push(format!("{} ({e})", copy.name)),
        }
    }
    done
}

/// Writes `set` to `path` as a rules file.
pub fn export_to(set: &RuleSet, path: &Path) -> std::io::Result<()> {
    RuleSet {
        path: path.to_path_buf(),
        ..set.clone()
    }
    .save()
}

/// Writes every one of `sets` into the folder `dir`, as importing them there would: files already
/// there are never overwritten.
pub fn export_all(sets: &[RuleSet], dir: &Path) -> Imported {
    let files: Vec<PathBuf> = sets.iter().map(|s| s.path.clone()).collect();
    import_into(dir, &files, &[])
}

/// Takes over the rules of earlier versions, then applies the selected rules file. Returns
/// whether `cfg` has to be saved for the rules to stay taken over.
pub fn load_selected(cfg: &mut Config) -> bool {
    // Until they could be moved, the old rules stay in the options and are used from there.
    let Some(dir) = dir() else {
        return false;
    };
    let Ok(changed) = migrate_in(&dir, cfg) else {
        return false;
    };
    apply_selected(cfg);
    changed
}

/// A regex matching `line` as a whole, give or take the spaces around it.
pub fn line_pattern(line: &str) -> String {
    let t = line.trim();
    if t.is_empty() {
        r"^\s*$".into()
    } else {
        format!(r"^\s*{}\s*$", regex::escape(t))
    }
}

/// A regex matching every one of `lines` (the sides of a changed line): the text they start
/// and end with, cut at word boundaries, with anything in between. Lines with nothing in common
/// are matched each as a whole.
pub fn lines_pattern(lines: &[&str]) -> String {
    let mut lines: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    lines.dedup();
    let Some((&first, rest)) = lines.split_first() else {
        return line_pattern("");
    };
    if rest.iter().all(|l| *l == first) {
        return line_pattern(first);
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let common = |a: &str, b: &str| -> usize {
        a.chars()
            .zip(b.chars())
            .take_while(|(x, y)| x == y)
            .map(|(x, _)| x.len_utf8())
            .sum()
    };
    let mut pre = rest
        .iter()
        .fold(first.len(), |n, l| n.min(common(first, l)));
    while first[..pre].ends_with(word) {
        pre -= first[..pre].chars().next_back().map_or(0, char::len_utf8);
    }
    let rev: Vec<String> = lines
        .iter()
        .map(|l| l[pre..].chars().rev().collect())
        .collect();
    let mut suf = rev[1..]
        .iter()
        .fold(rev[0].len(), |n, l| n.min(common(&rev[0], l)));
    let tail = |n: usize| &first[first.len() - n..];
    while tail(suf).starts_with(word) {
        suf -= tail(suf).chars().next().map_or(0, char::len_utf8);
    }
    let (prefix, suffix) = (&first[..pre], tail(suf));
    // Shared punctuation alone, like the ";" ending every statement, would match far too much.
    let telling = |s: &str| s.chars().any(char::is_alphanumeric);
    if !telling(prefix) && !telling(suffix) {
        let alts: Vec<String> = lines.iter().map(|l| regex::escape(l)).collect();
        return format!(r"^\s*(?:{})\s*$", alts.join("|"));
    }
    format!(
        r"^\s*{}.*{}\s*$",
        regex::escape(prefix.trim_start()),
        regex::escape(suffix.trim_end())
    )
}

/// A substitution making `a` and `b` equal: the part where they differ, from `a` to `b`.
pub fn substitution_for(a: &str, b: &str) -> Substitution {
    let a = a.trim();
    let b = b.trim();
    let pre = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum::<usize>();
    let suf = a[pre..]
        .chars()
        .rev()
        .zip(b[pre..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum::<usize>();
    let (da, db) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    if da.is_empty() && db.is_empty() {
        return Substitution {
            find: regex::escape(a),
            replace: a.replace('$', "$$"),
        };
    }
    if da.is_empty() {
        // Text only `b` has: taking it out of `b` makes them equal.
        return Substitution {
            find: regex::escape(db),
            replace: String::new(),
        };
    }
    Substitution {
        find: regex::escape(da),
        replace: db.replace('$', "$$"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::compute;

    #[test]
    fn relative_paths_are_taken_from_where_they_were_given() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("team.oxrules"), "{}").unwrap();
        let abs = |n: &str| absolute_spec(n, dir.path());
        let team = dir.path().join("team.oxrules").display().to_string();
        assert_eq!(abs("team.oxrules"), team);
        assert_eq!(abs("Logs.oxrules"), "Logs.oxrules");
        assert_eq!(abs("Logs"), "Logs");
        assert_eq!(abs(""), "");
        let sub = dir
            .path()
            .join("rules")
            .join("a.oxrules")
            .display()
            .to_string();
        assert_eq!(
            abs(&format!("rules{}a.oxrules", std::path::MAIN_SEPARATOR)),
            sub
        );
        assert_eq!(absolute_spec(&team, Path::new("/elsewhere")), team);
    }

    #[test]
    fn save_list_find() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let mut set = RuleSet::new(&dir, "Logs: dated");
        assert_eq!(set.name, "Logs_ dated");
        set.description = "timestamps".into();
        set.ignore_lines.push(r"^\d{4}-".into());
        set.substitutions.push(Substitution {
            find: "v[0-9]+".into(),
            replace: "v".into(),
        });
        set.save().unwrap();
        RuleSet::new(&dir, "another").save().unwrap();
        std::fs::write(dir.join("broken.oxrules"), "{").unwrap();
        std::fs::write(dir.join("other.txt"), "{}").unwrap();

        let all = list_in(std::slice::from_ref(&dir));
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["another", "Logs_ dated"]);
        let found = find_in(std::slice::from_ref(&dir), "logs_ DATED").unwrap();
        assert_eq!(found, set);
        assert!(find_in(std::slice::from_ref(&dir), "").is_none());
        let by_path = find_in(&[], set.path.to_str().unwrap()).unwrap();
        assert_eq!(by_path.name, "Logs_ dated");
        let by_file_name = find_in(std::slice::from_ref(&dir), "Logs_ dated.oxrules").unwrap();
        assert_eq!(by_file_name, set);
        let gone = dir.join("sub").join("another.oxrules");
        assert!(find_in(std::slice::from_ref(&dir), gone.to_str().unwrap()).is_none());
        // Saving again replaces the file whole, leaving no temporary file behind.
        set.description = "changed".into();
        set.save().unwrap();
        assert_eq!(RuleSet::load(&set.path).unwrap().description, "changed");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4);
    }

    #[test]
    fn names_as_files_name_them() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = [tmp.path().to_path_buf()];
        std::fs::write(
            dir[0].join("Bom.oxrules"),
            "\u{feff}{\"description\":\"d\"}",
        )
        .unwrap();
        assert_eq!(find_in(&dir, "Bom").unwrap().description, "d");

        let notes = RuleSet::new(&dir[0], "notes.OXRULES");
        assert_eq!(notes.name, "notes");
        notes.save().unwrap();
        assert!(find_in(&dir, "notes.oxrules").is_some());

        RuleSet::new(&dir[0], "Ärger").save().unwrap();
        assert!(same_name("ärger", "ÄRGER"));
        assert_eq!(find_in(&dir, "ärger").unwrap().name, "Ärger");
        // Where names keep their case, both files are listed and each is found by its name.
        RuleSet::new(&dir[0], "ärger").save().unwrap();
        if std::fs::read_dir(&dir[0]).unwrap().count() == 4 {
            assert_eq!(list_in(&dir).len(), 4);
            assert_eq!(find_in(&dir, "ärger").unwrap().name, "ärger");
            assert_eq!(find_in(&dir, "Ärger").unwrap().name, "Ärger");
        }
    }

    #[test]
    fn apply_replaces_rules() {
        let mut set = RuleSet::default();
        set.ignore_blocks.push(Block {
            start: "BEGIN".into(),
            end: "END".into(),
        });
        let base = DiffOptions {
            line_filters_enabled: true,
            line_filters: vec!["x".into()],
            ignore_case: true,
            ..DiffOptions::default()
        };
        let o = options_with(&base, Some(&set));
        assert!(o.ignore_case);
        assert!(!o.line_filters_enabled && o.line_filters.is_empty());
        assert!(o.block_filters_enabled);
        assert_eq!(o.block_filters, [("BEGIN".to_string(), "END".to_string())]);
        let none = options_with(&base, None);
        assert!(!none.block_filters_enabled && none.line_filters.is_empty());
    }

    #[test]
    fn migrate_old_options() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.diff.line_filters_enabled = true;
        cfg.diff.line_filters = vec!["^#".into()];
        cfg.diff.substitutions = vec![("a".into(), "b".into())];

        // Rules that cannot be written stay in the options, to be moved next time.
        let blocked = tmp.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        assert!(migrate_in(&blocked, &mut cfg).is_err());
        assert!(!cfg.rules_migrated && cfg.rules.is_empty());

        assert!(migrate_in(tmp.path(), &mut cfg).unwrap());
        assert_eq!(cfg.rules, DEFAULT_NAME);
        let all = list_in(&[tmp.path().to_path_buf()]);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ignore_lines, ["^#"]);
        assert!(all[0].substitutions.is_empty());
        assert_eq!(all[1].name, "Default (off)");
        assert_eq!(all[1].substitutions[0].find, "a");

        // Once done, choosing no rules stays that way.
        cfg.rules.clear();
        assert!(!migrate_in(tmp.path(), &mut cfg).unwrap());
        assert!(cfg.rules.is_empty());

        // Run again because the options were not saved: the same files are picked, none added.
        cfg.rules_migrated = false;
        migrate_in(tmp.path(), &mut cfg).unwrap();
        assert_eq!(cfg.rules, DEFAULT_NAME);
        assert_eq!(list_in(&[tmp.path().to_path_buf()]).len(), 2);

        let mut fresh = Config::default();
        migrate_in(tmp.path(), &mut fresh).unwrap();
        assert!(fresh.rules.is_empty() && fresh.rules_migrated);
    }

    #[test]
    fn import_and_export() {
        let (rules_dir, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut set = RuleSet::new(rules_dir.path(), "Logs");
        set.ignore_lines = vec!["^#".into()];
        set.description = "log noise".into();
        set.save().unwrap();

        let shared = out.path().join("Logs.oxrules");
        export_to(&set, &shared).unwrap();
        let mut other = RuleSet::load(&shared).unwrap();
        other.ignore_lines.push("^;".into());
        let changed = out.path().join("sub").join("Logs.oxrules");
        export_to(&other, &changed).unwrap();
        let fresh = out.path().join("New one.oxrules");
        export_to(&RuleSet::new(out.path(), "New one"), &fresh).unwrap();
        let junk = out.path().join("junk.oxrules");
        std::fs::write(&junk, "not json").unwrap();

        let done = import_into(rules_dir.path(), &[shared, changed, fresh, junk], &[]);
        assert_eq!(done.present, ["Logs"]);
        assert_eq!(done.added, ["Logs 2", "New one"]);
        assert_eq!(done.failed, ["junk.oxrules"]);
        let all = list_in(&[rules_dir.path().to_path_buf()]);
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Logs", "Logs 2", "New one"]);
        assert_eq!(all[1].ignore_lines, ["^#", "^;"]);
        assert_eq!(all[1].description, "log noise");

        let back = tempfile::tempdir().unwrap();
        assert_eq!(export_all(&all, back.path()).added.len(), 3);
        assert!(back.path().join("Logs 2.oxrules").is_file());
        // Exporting again leaves the files there alone, even when they were changed.
        let theirs = back.path().join("Logs.oxrules");
        let mut changed = RuleSet::load(&theirs).unwrap();
        changed.ignore_lines = vec!["theirs".into()];
        changed.save().unwrap();
        let again = export_all(&all, back.path());
        assert_eq!(again.present, ["Logs 2", "New one"]);
        assert_eq!(again.added, ["Logs 3"]);
        assert_eq!(RuleSet::load(&theirs).unwrap().ignore_lines, ["theirs"]);
        assert!(is_rules_file(&back.path().join("Logs 2.OXRULES")));
    }

    #[test]
    fn pattern_for_changed_lines() {
        let a = "  * BSF: Source Tracking ID:uggc%**yifia%20000*fia+106910;";
        let b = " * BSF: Source Tracking ID:uggcf%**tvguho.pnqrapr.pbz+2009657272!qvegl;";
        let p = lines_pattern(&[a, b]);
        assert_eq!(p, r"^\s*\* BSF: Source Tracking ID:.*;\s*$");
        let re = regex::Regex::new(&p).unwrap();
        assert!(re.is_match(a) && re.is_match(b));
        assert!(!re.is_match("* BSF: Owner: me;"));

        let re = regex::Regex::new(&lines_pattern(&["Generated 2024-01", "Generated 2025-06"]));
        assert!(re.unwrap().is_match("Generated 1999-12"));
        assert_eq!(lines_pattern(&["same", "same "]), line_pattern("same"));
        let p = lines_pattern(&["alpha", "beta"]);
        let re = regex::Regex::new(&p).unwrap();
        assert!(re.is_match("alpha") && re.is_match(" beta ") && !re.is_match("gamma"));
        let re = regex::Regex::new(&lines_pattern(&["名稱：甲", "名稱：乙"])).unwrap();
        assert!(re.is_match("名稱：丙"));
        let re = regex::Regex::new(&lines_pattern(&["x = 1;", "y = 2;"])).unwrap();
        assert!(re.is_match("x = 1;") && re.is_match("y = 2;") && !re.is_match("z = 3;"));
    }

    #[test]
    fn prefilled_rules_work() {
        let line = "  Generated on 2024-01-02 (a+b) ";
        let re = regex::Regex::new(&line_pattern(line)).unwrap();
        assert!(re.is_match("Generated on 2024-01-02 (a+b)"));
        assert!(!re.is_match("Generated on 2024-01-03 (a+b)"));

        let s = substitution_for("let total = $price * 2;", "let total = $cost * 2;");
        assert_eq!(s.find, "price");
        assert_eq!(s.replace, "cost");
        let s = substitution_for("a$1", "a$1");
        assert_eq!(s.find, r"a\$1");
        assert_eq!(s.replace, "a$$1");
        let s = substitution_for("x = 1;", "x = 1; // new");
        assert_eq!((s.find.as_str(), s.replace.as_str()), (" // new", ""));

        let s = substitution_for("version = 1.2.3", "version = 1.4.0");
        let draft = RuleDraft {
            kind: RuleKind::Substitute,
            a: s.find,
            b: s.replace,
        };
        assert_eq!(draft.problem(), None);
        let mut set = RuleSet::default();
        set.add(&draft);
        assert_eq!(set.count(), 1);
        let block = RuleDraft {
            kind: RuleKind::IgnoreBlock,
            a: "^BEGIN".into(),
            b: " ".into(),
        };
        assert_eq!(block.problem().as_deref(), Some(" "));
        let opts = options_with(&DiffOptions::default(), Some(&set));
        let a = ["x".to_string(), "version = 1.2.3".into()];
        let b = ["x".to_string(), "version = 1.4.0".into()];
        assert!(compute(&[&a, &b], &opts).is_identical());
    }
}
