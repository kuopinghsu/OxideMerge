//! File/folder filters: wildcard masks (`*.c;*.h;!*.bak;!.git\`) and WinMerge `.flt` filter files.

use regex::{Regex, RegexBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Files and folders a folder compare leaves out by their paths, relative to the compared
/// root with `/` separators, such as what Git ignores in a working copy compared with a
/// revision.
#[derive(Debug, Default)]
pub struct Skipped {
    files: HashSet<String>,
    dirs: HashSet<String>,
}

impl Skipped {
    /// From `paths`, a folder's path ending with `/`.
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let mut skipped = Skipped::default();
        for path in paths {
            match path.strip_suffix('/') {
                Some(dir) => skipped.dirs.insert(dir.to_string()),
                None => skipped.files.insert(path),
            };
        }
        skipped
    }
}

/// The only files and folders a folder compare shows, by their paths relative to the
/// compared root with `/` separators: each, what is inside a folder of them, and the folders
/// on the way to them. Where names do not tell case apart, neither does it.
#[derive(Debug, Default)]
pub struct Chosen {
    paths: Vec<String>,
}

impl Chosen {
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let paths = paths
            .into_iter()
            .map(|p| Self::key(p.trim_matches('/')))
            .filter(|p| !p.is_empty())
            .collect();
        Chosen { paths }
    }

    fn key(path: &str) -> String {
        if cfg!(any(windows, target_os = "macos")) {
            path.to_lowercase()
        } else {
            path.to_string()
        }
    }

    /// Whether `inner` is `outer` or inside it.
    fn within(inner: &str, outer: &str) -> bool {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|r| r.starts_with('/'))
    }

    fn file(&self, rel: &str) -> bool {
        let rel = Self::key(rel);
        self.paths.iter().any(|p| Self::within(&rel, p))
    }

    fn dir(&self, rel: &str) -> bool {
        let rel = Self::key(rel);
        self.paths
            .iter()
            .any(|p| Self::within(&rel, p) || Self::within(p, &rel))
    }
}

#[derive(Clone, Debug)]
pub struct FltFilter {
    pub name: String,
    pub desc: String,
    pub default_include: bool,
    pub files: Vec<Regex>,
    pub dirs: Vec<Regex>,
    pub file_except: Vec<Regex>,
    pub dir_except: Vec<Regex>,
    pub source: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum FileFilter {
    Mask {
        includes: Vec<Regex>,
        excludes: Vec<Regex>,
        dir_excludes: Vec<Regex>,
    },
    Flt(FltFilter),
    /// What the filter keeps, but for the files and folders named.
    Except(Box<FileFilter>, Arc<Skipped>),
    /// What the filter keeps of the files and folders chosen, and nothing else.
    Only(Box<FileFilter>, Arc<Chosen>),
    /// What both filters keep.
    Both(Box<FileFilter>, Box<FileFilter>),
}

pub const FLT_PREFIX: &str = "[F] ";

pub fn wildcard_to_regex(pat: &str) -> Option<Regex> {
    let mut re = String::from("^");
    for c in pat.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    RegexBuilder::new(&re).case_insensitive(true).build().ok()
}

fn ci_regex(s: &str) -> Option<Regex> {
    RegexBuilder::new(s).case_insensitive(true).build().ok()
}

impl Default for FileFilter {
    fn default() -> Self {
        FileFilter::parse_mask("*.*")
    }
}

impl FileFilter {
    /// Resolves a filter string: either a mask or `[F] name` referring to a named filter.
    pub fn resolve(spec: &str, named: &[FltFilter]) -> FileFilter {
        if let Some(name) = spec.strip_prefix(FLT_PREFIX) {
            if let Some(f) = named.iter().find(|f| f.name == name.trim()) {
                return FileFilter::Flt(f.clone());
            }
        }
        FileFilter::parse_mask(spec)
    }

    pub fn parse_mask(spec: &str) -> FileFilter {
        let mut includes = Vec::new();
        let mut excludes = Vec::new();
        let mut dir_excludes = Vec::new();
        for raw in spec.split([';', ',', ' ']) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let (neg, pat) = match raw.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, raw),
            };
            let is_dir = pat.ends_with('\\') || pat.ends_with('/');
            let pat = pat.trim_end_matches(['\\', '/']);
            // "*.*" should also match files without extension.
            let re = if pat == "*.*" {
                wildcard_to_regex("*")
            } else {
                wildcard_to_regex(pat)
            };
            let Some(re) = re else { continue };
            match (neg, is_dir) {
                (true, true) => dir_excludes.push(re),
                (true, false) => excludes.push(re),
                (false, true) => {}
                (false, false) => includes.push(re),
            }
        }
        FileFilter::Mask {
            includes,
            excludes,
            dir_excludes,
        }
    }

    /// Whether some file or folder could be left out, so a folder compare may not list all.
    pub fn excludes_anything(&self) -> bool {
        match self {
            FileFilter::Mask {
                includes,
                excludes,
                dir_excludes,
            } => {
                !excludes.is_empty()
                    || !dir_excludes.is_empty()
                    || !(includes.is_empty() || includes.iter().any(|r| r.as_str() == "^.*$"))
            }
            FileFilter::Flt(f) => {
                !(f.default_include
                    && f.files.is_empty()
                    && f.dirs.is_empty()
                    && f.file_except.is_empty()
                    && f.dir_except.is_empty())
            }
            FileFilter::Except(inner, skipped) => {
                inner.excludes_anything() || !(skipped.files.is_empty() && skipped.dirs.is_empty())
            }
            FileFilter::Only(..) => true,
            FileFilter::Both(a, b) => a.excludes_anything() || b.excludes_anything(),
        }
    }

    /// `rel_path` uses '/' separators and is relative to the compared root.
    pub fn include_file(&self, rel_path: &str, name: &str) -> bool {
        match self {
            FileFilter::Mask {
                includes, excludes, ..
            } => {
                (includes.is_empty() || includes.iter().any(|r| r.is_match(name)))
                    && !excludes.iter().any(|r| r.is_match(name))
            }
            FileFilter::Flt(f) => {
                let _ = rel_path;
                let hit = f.files.iter().any(|r| r.is_match(name));
                let exc = f.file_except.iter().any(|r| r.is_match(name));
                let matched = hit && !exc;
                if f.default_include {
                    !matched
                } else {
                    matched
                }
            }
            FileFilter::Except(inner, skipped) => {
                !skipped.files.contains(rel_path) && inner.include_file(rel_path, name)
            }
            FileFilter::Only(inner, chosen) => {
                chosen.file(rel_path) && inner.include_file(rel_path, name)
            }
            FileFilter::Both(a, b) => {
                a.include_file(rel_path, name) && b.include_file(rel_path, name)
            }
        }
    }

    pub fn include_dir(&self, rel_path: &str, name: &str) -> bool {
        match self {
            FileFilter::Mask { dir_excludes, .. } => !dir_excludes.iter().any(|r| r.is_match(name)),
            FileFilter::Flt(f) => {
                let win_path = format!("\\{}", rel_path.replace('/', "\\"));
                let hit = f
                    .dirs
                    .iter()
                    .any(|r| r.is_match(&win_path) || r.is_match(name));
                let exc = f
                    .dir_except
                    .iter()
                    .any(|r| r.is_match(&win_path) || r.is_match(name));
                let matched = hit && !exc;
                if f.default_include {
                    !matched
                } else {
                    // Directories are always traversed in "exclude by default" filters
                    // unless explicitly matched; files decide what is shown.
                    true
                }
            }
            FileFilter::Except(inner, skipped) => {
                !skipped.dirs.contains(rel_path) && inner.include_dir(rel_path, name)
            }
            FileFilter::Only(inner, chosen) => {
                chosen.dir(rel_path) && inner.include_dir(rel_path, name)
            }
            FileFilter::Both(a, b) => {
                a.include_dir(rel_path, name) && b.include_dir(rel_path, name)
            }
        }
    }
}

pub fn parse_flt(text: &str, source: Option<PathBuf>) -> Option<FltFilter> {
    let mut f = FltFilter {
        name: String::new(),
        desc: String::new(),
        default_include: true,
        files: vec![],
        dirs: vec![],
        file_except: vec![],
        dir_except: vec![],
        source,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("##") {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // strip trailing comments ("  ## comment")
        let value = match value.find(" ##") {
            Some(i) => &value[..i],
            None => value,
        }
        .trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => f.name = value.to_string(),
            "desc" => f.desc = value.to_string(),
            "def" => f.default_include = !value.eq_ignore_ascii_case("exclude") && value != "0",
            "f" => f.files.extend(ci_regex(value)),
            "d" => f.dirs.extend(ci_regex(value)),
            "xf" => f.file_except.extend(ci_regex(value)),
            "xd" => f.dir_except.extend(ci_regex(value)),
            _ => {}
        }
    }
    if f.name.is_empty() {
        f.name = f
            .source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())?;
    }
    Some(f)
}

const BUILTIN: &[&str] = &[
    r"name: Exclude Source Control
desc: Ignores files and folders of version control systems
def: include
d: \\\.git$
d: \\\.github$
d: \\\.svn$
d: \\\.hg$
d: \\\.bzr$
d: \\CVS$
d: \\_darcs$
f: ^\.git$
f: ^\.gitignore$
f: ^\.gitattributes$
f: ^\.gitmodules$
f: ^\.hgignore$",
    r"name: Exclude Build Output
desc: Ignores compiler output and dependency folders
def: include
d: \\target$
d: \\bin$
d: \\obj$
d: \\build$
d: \\node_modules$
d: \\__pycache__$
d: \\\.vs$
d: \\\.idea$
f: \.(o|obj|pdb|ilk|exp|lib|a|so|dll|exe|class|pyc|pch|ipch|ncb|suo|user|aps|tlog|log)$",
    r"name: Source Code Only
desc: Shows only source code files
def: exclude
f: \.(c|cc|cpp|cxx|h|hh|hpp|hxx|inl|rs|py|js|jsx|ts|tsx|java|kt|cs|vb|go|rb|php|swift|m|mm|scala|lua|pl|sh|bat|cmd|ps1|sql|html|htm|css|scss|xml|json|yaml|yml|toml|ini|md|txt|rc|def|cmake)$
f: ^(Makefile|CMakeLists\.txt|Dockerfile)$",
];

pub fn builtin_filters() -> Vec<FltFilter> {
    BUILTIN.iter().filter_map(|t| parse_flt(t, None)).collect()
}

pub fn filter_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("Filters"));
        }
    }
    if let Some(c) = crate::config::config_dir() {
        v.push(c.join("Filters"));
    }
    v
}

/// Built-in filters plus every `*.flt` file found in the filter directories.
pub fn load_all_filters() -> Vec<FltFilter> {
    let mut all = builtin_filters();
    for dir in filter_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .map(|x| x.eq_ignore_ascii_case("flt"))
                .unwrap_or(false)
            {
                if let Some(f) = load_flt(&p) {
                    all.retain(|x| x.name != f.name);
                    all.push(f);
                }
            }
        }
    }
    all
}

pub fn load_flt(path: &Path) -> Option<FltFilter> {
    let bytes = std::fs::read(path).ok()?;
    let (text, _, _) = super::text::decode(&bytes, None);
    parse_flt(&text, Some(path.to_path_buf()))
}

pub const FLT_TEMPLATE: &str = r"## This is a OxideMerge / WinMerge compatible filter file.
## Lines starting with ## are comments.
name: My Filter
desc: Description of the filter
## def: include = show everything except matches, exclude = show only matches
def: include
## f: regular expression matched against file names
f: \.bak$
## d: regular expression matched against folder paths (\sub\folder)
d: \\\.git$
## xf: / xd: exceptions to the rules above
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_paths_are_left_out_whatever_the_filter_keeps() {
        let skipped = Skipped::new(["target/".to_string(), "src/run.log".to_string()]);
        let f = FileFilter::Except(Box::new(FileFilter::parse_mask("*.*")), Arc::new(skipped));
        assert!(f.excludes_anything());
        assert!(!f.include_dir("target", "target"));
        assert!(f.include_dir("src", "src"));
        assert!(!f.include_file("src/run.log", "run.log"));
        assert!(f.include_file("run.log", "run.log"), "only the path named");
        assert!(f.include_file("src/main.rs", "main.rs"));
        // What the filter itself leaves out stays out.
        let skipped = Arc::new(Skipped::new(Vec::new()));
        let f = FileFilter::Except(Box::new(FileFilter::parse_mask("*.rs")), skipped);
        assert!(!f.include_file("a.txt", "a.txt"));
    }

    #[test]
    fn only_the_chosen_paths_what_is_in_them_and_the_way_to_them_are_kept() {
        let chosen = Chosen::new(["a.txt", "data", "deep/er/e.txt"].map(String::from));
        let f = FileFilter::Only(Box::new(FileFilter::parse_mask("*.*")), Arc::new(chosen));
        assert!(f.excludes_anything());
        assert!(f.include_file("a.txt", "a.txt"));
        assert!(!f.include_file("b.txt", "b.txt"));
        assert!(f.include_dir("data", "data"));
        assert!(f.include_dir("data/sub", "sub"));
        assert!(
            f.include_file("data/sub/d.txt", "d.txt"),
            "inside a chosen folder"
        );
        assert!(
            !f.include_dir("database", "database"),
            "a name that only starts the same"
        );
        assert!(f.include_dir("deep", "deep") && f.include_dir("deep/er", "er"));
        assert!(f.include_file("deep/er/e.txt", "e.txt"));
        assert!(
            !f.include_file("deep/er/not.txt", "not.txt"),
            "beside, not on the way"
        );
        assert!(!f.include_file("deep/x.txt", "x.txt"));
        assert_eq!(
            f.include_file("A.TXT", "A.TXT"),
            cfg!(any(windows, target_os = "macos"))
        );
        // What the filter itself leaves out stays out.
        let chosen = Arc::new(Chosen::new(["a.txt".to_string()]));
        let f = FileFilter::Only(Box::new(FileFilter::parse_mask("*.rs")), chosen);
        assert!(!f.include_file("a.txt", "a.txt"));
    }

    #[test]
    fn mask_filter() {
        let f = FileFilter::parse_mask("*.rs;*.toml;!main.rs;!target\\");
        assert!(f.include_file("src/lib.rs", "lib.rs"));
        assert!(!f.include_file("src/main.rs", "main.rs"));
        assert!(!f.include_file("a.txt", "a.txt"));
        assert!(!f.include_dir("target", "target"));
        assert!(f.include_dir("src", "src"));
        let all = FileFilter::parse_mask("*.*");
        assert!(all.include_file("Makefile", "Makefile"));
    }

    #[test]
    fn flt_filter() {
        let f = &builtin_filters()[0];
        let ff = FileFilter::Flt(f.clone());
        assert!(!ff.include_dir(".git", ".git"));
        assert!(!ff.include_dir(".github", ".github"));
        assert!(!ff.include_dir("sub/.git", ".git"));
        assert!(!ff.include_file("sub/.git", ".git"));
        assert!(ff.include_dir("src", "src"));
        assert!(ff.include_dir(".gitx", ".gitx"));
        assert!(!ff.include_file(".gitignore", ".gitignore"));
        let src = FileFilter::Flt(builtin_filters()[2].clone());
        assert!(src.include_file("a.rs", "a.rs"));
        assert!(!src.include_file("a.png", "a.png"));
    }
}
