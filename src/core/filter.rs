//! File/folder filters: wildcard masks (`*.c;*.h;!*.bak;!.git\`) and WinMerge `.flt` filter files.

use regex::{Regex, RegexBuilder};
use std::path::{Path, PathBuf};

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
            let re = if pat == "*.*" { wildcard_to_regex("*") } else { wildcard_to_regex(pat) };
            let Some(re) = re else { continue };
            match (neg, is_dir) {
                (true, true) => dir_excludes.push(re),
                (true, false) => excludes.push(re),
                (false, true) => {}
                (false, false) => includes.push(re),
            }
        }
        FileFilter::Mask { includes, excludes, dir_excludes }
    }

    /// `rel_path` uses '/' separators and is relative to the compared root.
    pub fn include_file(&self, rel_path: &str, name: &str) -> bool {
        match self {
            FileFilter::Mask { includes, excludes, .. } => {
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
        }
    }

    pub fn include_dir(&self, rel_path: &str, name: &str) -> bool {
        match self {
            FileFilter::Mask { dir_excludes, .. } => !dir_excludes.iter().any(|r| r.is_match(name)),
            FileFilter::Flt(f) => {
                let win_path = format!("\\{}", rel_path.replace('/', "\\"));
                let hit = f.dirs.iter().any(|r| r.is_match(&win_path) || r.is_match(name));
                let exc = f.dir_except.iter().any(|r| r.is_match(&win_path) || r.is_match(name));
                let matched = hit && !exc;
                if f.default_include {
                    !matched
                } else {
                    // Directories are always traversed in "exclude by default" filters
                    // unless explicitly matched; files decide what is shown.
                    true
                }
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
        let Some((key, value)) = line.split_once(':') else { continue };
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
d: \\\.svn$
d: \\\.hg$
d: \\\.bzr$
d: \\CVS$
d: \\_darcs$
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
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("flt")).unwrap_or(false) {
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
        assert!(ff.include_dir("src", "src"));
        assert!(!ff.include_file(".gitignore", ".gitignore"));
        let src = FileFilter::Flt(builtin_filters()[2].clone());
        assert!(src.include_file("a.rs", "a.rs"));
        assert!(!src.include_file("a.png", "a.png"));
    }
}
