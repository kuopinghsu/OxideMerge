//! Conservative evidence-only references. This module never runs a build or rewrites inputs.
use super::{
    analysis,
    baseline::{Capture, Category, ReleaseSummary},
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
mod code;
mod syntax;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line: usize,
    pub excerpt: String,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImpactFinding {
    pub id: String,
    pub origin_change_id: String,
    pub related_paths: Vec<String>,
    pub relation_type: String,
    pub evidence: Evidence,
    pub severity: String,
    pub confidence: String,
    pub rule_id: String,
    pub limitations: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImpactReport {
    pub version: u32,
    pub baseline_fingerprint: String,
    pub findings: Vec<ImpactFinding>,
    pub assessment: String,
    pub excluded: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}
/// Heuristic matches of changed definition names and configuration keys kept at most, so
/// that common words in a broad change leave room for the other findings.
const MAX_WORD_FINDINGS: usize = 500;
struct CacheEntry {
    lines: Vec<(usize, String)>,
    footprint: usize,
    long_lines: bool,
    /// Imports and identifiers, in each language a file holding these bytes is named in.
    code: BTreeMap<Lang, Code>,
}
struct Code {
    imports: Vec<code::Import>,
    /// Each identifier with the line it is first used on outside import statements.
    uses: HashMap<String, usize>,
    /// Why some of the file may not have been read.
    unassessed: Option<&'static str>,
}

/// The imports, identifiers and top-level definitions of `source`, a file in `lang` named
/// `path`: from its syntax tree when there is a grammar for it, otherwise from its tokens.
fn read_code(
    lang: Lang,
    path: &str,
    source: &str,
    cancel: &AtomicBool,
) -> Result<(Code, BTreeMap<String, code::Definition>)> {
    if let Some(grammar) = syntax::Grammar::of(path) {
        let read = syntax::read(grammar, source, cancel)?;
        let code = Code {
            imports: read.imports,
            uses: read.uses,
            unassessed: read.errors.then_some(
                "Code the parser could not read; imports and definitions there may be missed",
            ),
        };
        return Ok((code, read.definitions));
    }
    let (tokens, truncated) = code::tokens(lang, source);
    let imports = code::imports(lang, &tokens);
    let uses = code::uses(&tokens, &imports);
    let definitions = code::definitions(lang, &tokens);
    let code = Code {
        imports,
        uses,
        unassessed: truncated
            .then_some("More than 2,000,000 tokens; later imports were not assessed"),
    };
    Ok((code, definitions))
}
/// The lines and code of `file` for its index entry, or why it cannot be indexed, reading its
/// code in each of `langs` as the file named for that language.
fn index(
    file: &super::baseline::CapturedFile,
    langs: &BTreeMap<Lang, &str>,
    cancel: &AtomicBool,
) -> Result<std::result::Result<CacheEntry, String>> {
    if super::text::looks_binary(&file.bytes) {
        return Ok(Err("Binary content was not indexed".into()));
    }
    let source = super::text::decode(&file.bytes, None).0;
    if source.len() as u64 > analysis::MAX_FILE || super::text::lines(&source).count() > 100_000 {
        return Ok(Err(
            "Decoded text exceeds 16 MiB or 100,000 lines; references not assessed".into(),
        ));
    }
    let long_lines = super::text::lines(&source).any(|(line, _)| line.len() > 4096);
    let mut lines: Vec<_> = super::text::lines(&source)
        .enumerate()
        .filter(|(_, (line, _))| line.len() <= 4096)
        .map(|(i, (line, _))| (i + 1, line.to_owned()))
        .collect();
    lines.shrink_to_fit();
    let mut footprint = lines
        .iter()
        .map(|(_, line)| line.len() + std::mem::size_of::<(usize, String)>())
        .sum();
    let mut code = BTreeMap::new();
    for (&lang, path) in langs {
        analysis::check_cancel(cancel)?;
        let (read, _) = read_code(lang, path, &source, cancel)?;
        footprint += read
            .imports
            .iter()
            .map(|i| std::mem::size_of::<code::Import>() + i.spec.len())
            .sum::<usize>()
            + read.uses.keys().map(|k| k.len() + 48).sum::<usize>();
        code.insert(lang, read);
    }
    Ok(Ok(CacheEntry {
        lines,
        footprint,
        long_lines,
        code,
    }))
}

/// `f` of each of `items`, in order, computed on up to eight threads.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8)
        .min(items.len());
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(at) else {
                            break;
                        };
                        out.push((at, f(item)));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    });
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, r)| r).collect()
}
impl CacheEntry {
    /// The text of line `line`, unless it was too long to keep.
    fn line(&self, line: usize) -> &str {
        self.lines
            .binary_search_by_key(&line, |(n, _)| *n)
            .map_or("", |at| &self.lines[at].1)
    }
}

/// Languages whose import statements are recognized, by file extension.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Lang {
    Script,
    Python,
    C,
    Css,
    Rust,
    Json,
}
impl Lang {
    fn of(path: &str) -> Option<Lang> {
        Some(
            match super::text::extension(std::path::Path::new(path)).as_str() {
                "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "vue" | "svelte" => {
                    Lang::Script
                }
                "py" | "pyi" => Lang::Python,
                "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "m" | "mm" => Lang::C,
                "css" | "scss" | "less" => Lang::Css,
                "rs" => Lang::Rust,
                "json" => Lang::Json,
                _ => return None,
            },
        )
    }
    fn name(self) -> &'static str {
        match self {
            Lang::Script => "script",
            Lang::Python => "python",
            Lang::C => "c",
            Lang::Css => "css",
            Lang::Rust => "rust",
            Lang::Json => "json",
        }
    }
}

/// `path` with `.` and `..` steps resolved; `None` when it leaves the root.
fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for step in path.split('/') {
        match step {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            step => out.push(step),
        }
    }
    Some(out.join("/"))
}

fn join(base: &str, rest: &str) -> Option<String> {
    normalize(&if base.is_empty() {
        rest.to_owned()
    } else {
        format!("{base}/{rest}")
    })
}

fn parent(path: &str) -> Option<&str> {
    (!path.is_empty()).then(|| path.rsplit_once('/').map_or("", |(dir, _)| dir))
}

/// The files of the Rust module `segments` (`crate`, `self` or `super` first) seen from `from`.
fn rust_module_files(from: &str, segments: &[&str], exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let dir = parent(from).unwrap_or("");
    let name = from.rsplit('/').next().unwrap_or(from);
    // where the modules of the module of `from` are
    let own = if ["mod.rs", "lib.rs", "main.rs"].contains(&name) {
        dir.to_owned()
    } else {
        from.strip_suffix(".rs").unwrap_or(from).to_owned()
    };
    // the nearest folder up with a lib.rs or main.rs
    let mut at = Some(dir);
    let root = loop {
        let Some(d) = at else { break None };
        let has = |file: &str| join(d, file).is_some_and(|p| exists(&p));
        if has("lib.rs") || has("main.rs") {
            break Some(d.to_owned());
        }
        at = parent(d);
    };
    let (base, rest) = match segments.first() {
        Some(&"crate") => {
            let Some(root) = root else {
                return Vec::new();
            };
            (root, &segments[1..])
        }
        Some(&"self") => (own, &segments[1..]),
        Some(&"super") => {
            let ups = segments.iter().take_while(|s| **s == "super").count();
            let mut base = own.as_str();
            for _ in 0..ups {
                // never above the crate root
                if root.as_deref() == Some(base) {
                    return Vec::new();
                }
                let Some(up) = parent(base) else {
                    return Vec::new();
                };
                base = up;
            }
            (base.to_owned(), &segments[ups..])
        }
        _ => return Vec::new(),
    };
    if rest.is_empty() {
        let mut out: Vec<String> = ["mod.rs", "lib.rs", "main.rs"]
            .iter()
            .filter_map(|file| join(&base, file))
            .collect();
        if !base.is_empty() {
            out.insert(0, format!("{base}.rs"));
        }
        return out;
    }
    let module = rest.join("/");
    [format!("{module}.rs"), format!("{module}/mod.rs")]
        .iter()
        .filter_map(|tail| join(&base, tail))
        .collect()
}

/// The file `import` in `from` names, with the names it brings in from that file.
fn resolve(
    lang: Lang,
    from: &str,
    import: &code::Import,
    exists: &dyn Fn(&str) -> bool,
) -> Option<(String, Vec<(String, String)>)> {
    if let Some(path) = import.spec.strip_prefix("use:") {
        let segments: Vec<&str> = path.split("::").collect();
        // a module itself, or a name in its parent module
        if let Some(file) = rust_module_files(from, &segments, exists)
            .into_iter()
            .find(|p| exists(p))
        {
            return Some((file, Vec::new()));
        }
        let (_, module) = segments.split_last()?;
        let file = rust_module_files(from, module, exists)
            .into_iter()
            .find(|p| exists(p))?;
        return Some((file, import.names.clone()));
    }
    let file = candidates(lang, from, &import.spec)
        .into_iter()
        .find(|p| exists(p))?;
    Some((file, import.names.clone()))
}

/// The paths an import of `spec` written in `from` can name, most likely first.
fn candidates(lang: Lang, from: &str, spec: &str) -> Vec<String> {
    let dir = from.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut out = Vec::new();
    match lang {
        Lang::Script => {
            if let Some(p) = join(dir, spec) {
                out.push(p.clone());
                for ext in [
                    "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "vue", "svelte",
                ] {
                    out.push(format!("{p}.{ext}"));
                }
                for ext in ["ts", "tsx", "js", "jsx", "mjs"] {
                    out.push(format!("{p}/index.{ext}"));
                }
            }
        }
        Lang::Python => {
            let dots = spec.chars().take_while(|&c| c == '.').count();
            let module = spec[dots..].replace('.', "/");
            let bases: Vec<String> = if dots == 0 {
                vec![String::new(), "src".into()]
            } else {
                let mut base = Some(dir.to_owned());
                for _ in 1..dots {
                    base = base.and_then(|b| normalize(&format!("{b}/..")));
                }
                base.into_iter().collect()
            };
            for base in bases {
                for tail in [format!("{module}.py"), format!("{module}/__init__.py")] {
                    out.extend(join(&base, &tail));
                }
            }
        }
        Lang::C | Lang::Css | Lang::Json => out.extend(join(dir, spec)),
        Lang::Rust => {
            let name = from.rsplit('/').next().unwrap_or(from);
            let base = if ["mod.rs", "lib.rs", "main.rs"].contains(&name) {
                dir.to_owned()
            } else {
                from.strip_suffix(".rs").unwrap_or(from).to_owned()
            };
            out.extend(join(&base, &format!("{spec}.rs")));
            out.extend(join(&base, &format!("{spec}/mod.rs")));
        }
    }
    out
}
#[derive(Default)]
pub struct Index {
    version: &'static str,
    cache: BTreeMap<String, CacheEntry>,
}
impl Index {
    /// The references in `target`, the right capture of `summary`, to what changed since
    /// `left`, its left capture.
    pub fn analyze(
        &mut self,
        summary: &ReleaseSummary,
        left: &Capture,
        target: &Capture,
        cancel: &AtomicBool,
    ) -> Result<ImpactReport> {
        let result = self.analyze_inner(summary, left, target, cancel);
        if result.is_err() {
            self.cache.clear();
        }
        result
    }
    fn analyze_inner(
        &mut self,
        summary: &ReleaseSummary,
        left: &Capture,
        target: &Capture,
        cancel: &AtomicBool,
    ) -> Result<ImpactReport> {
        anyhow::ensure!(
            summary.right.fingerprint == target.metadata.fingerprint
                && summary.right.rules == target.metadata.rules
                && summary.right.filter == target.metadata.filter
                && summary.left.fingerprint == left.metadata.fingerprint,
            "Impact baseline changed; regenerate the release summary first"
        );
        const VERSION: &str = "syntax-v1";
        if self.version != VERSION {
            self.cache.clear();
            self.version = VERSION;
        }
        let active: BTreeSet<_> = target.files.values().map(|f| f.digest.clone()).collect();
        self.cache.retain(|digest, _| active.contains(digest));
        let mut patterns: BTreeMap<String, Vec<(&super::baseline::ReleaseChange, &'static str)>> =
            BTreeMap::new();
        // the top-level functions, types and values each changed file changed or removed
        let mut symbols: BTreeMap<&str, BTreeMap<String, (code::Kind, bool)>> = BTreeMap::new();
        let read = parallel(&summary.changes, |change| -> Result<_> {
            let Some(lang) = Lang::of(&change.path).filter(|_| {
                !change.ignored_by_text_rules
                    && matches!(
                        change.category,
                        Category::Modified | Category::RenamedModified | Category::Deleted
                    )
            }) else {
                return Ok(None);
            };
            let definitions = |path: &str, file: Option<&super::baseline::CapturedFile>| match file
                .filter(|f| !super::text::looks_binary(&f.bytes))
            {
                Some(f) => {
                    let source = super::text::decode(&f.bytes, None).0;
                    read_code(lang, path, &source, cancel).map(|(_, d)| d)
                }
                None => Ok(BTreeMap::new()),
            };
            let old_path = change.old_path.as_deref().unwrap_or(&change.path);
            let before = definitions(old_path, left.files.get(old_path))?;
            let after = definitions(&change.path, target.files.get(&change.path))?;
            Ok(Some(code::changed(&before, &after)))
        });
        for (change, changed_definitions) in summary.changes.iter().zip(read) {
            analysis::check_cancel(cancel)?;
            if matches!(change.category, Category::Unchanged) || change.ignored_by_text_rules {
                continue;
            }
            let mut local = BTreeMap::new();
            if let Some(changed) = changed_definitions? {
                for name in changed.keys().filter(|n| n.chars().count() >= 4) {
                    local.insert(name.clone(), "symbol_name_reference");
                }
                symbols.insert(&change.path, changed);
            }
            local.insert(change.path.clone(), "literal_path_reference");
            if let Some(name) = std::path::Path::new(&change.path)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| s.len() >= 4)
            {
                local.insert(name.into(), "filename_reference");
            }
            if let Some(semantic) = &change.semantic {
                for key in &semantic.changes {
                    let segments = super::semantic::segments(&key.path);
                    let keys: Vec<String> = segments
                        .iter()
                        .filter_map(|s| s.strip_prefix('[')?.strip_suffix(']'))
                        .filter_map(|raw| serde_json::from_str::<String>(raw).ok())
                        .collect();
                    if let Some(leaf) = keys.last().filter(|k| k.len() >= 3) {
                        local.insert(leaf.clone(), "config_key_occurrence");
                    }
                    // A dotted path cannot name an array element.
                    if keys.len() >= 2
                        && keys.len() == segments.len()
                        && keys.iter().all(|k| !k.contains('.'))
                    {
                        local.insert(keys.join("."), "config_key_path");
                    }
                }
            }
            for (needle, relation) in local {
                patterns.entry(needle).or_default().push((change, relation));
            }
        }
        let mut changed = BTreeMap::new();
        for change in &summary.changes {
            if matches!(change.category, Category::Unchanged) || change.ignored_by_text_rules {
                continue;
            }
            changed.insert(change.path.clone(), change);
            if let Some(old) = &change.old_path {
                changed.insert(old.clone(), change);
            }
        }
        if patterns.keys().map(String::len).sum::<usize>() > 1 << 20 {
            bail!("Impact pattern budget exceeds 1 MiB; narrow the scope");
        }
        let mut excluded = target.excluded.clone();
        let mut findings = Vec::new();
        let mut word_findings = Vec::new();
        let mut words_left_out = 0usize;
        if !patterns.is_empty() {
            // Multi-pattern search keeps runtime linear in indexed text instead of rescanning
            // the entire project for each changed path. Literal boundary checks stay identical.
            let rules: Vec<_> = patterns.into_iter().collect();
            let matcher = aho_corasick::AhoCorasick::builder()
                .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
                .build(rules.iter().map(|(pattern, _)| pattern))?;
            if matcher.memory_usage() > 32 << 20 {
                bail!("Impact matcher exceeds 32 MiB; narrow the scope");
            }
            // the languages each content is read in, from the names of the files holding it,
            // with the first such name
            let mut by_digest: BTreeMap<
                &str,
                (&super::baseline::CapturedFile, BTreeMap<Lang, &str>),
            > = BTreeMap::new();
            for (path, file) in &target.files {
                let entry = by_digest
                    .entry(file.digest.as_str())
                    .or_insert_with(|| (file, BTreeMap::new()));
                if let Some(lang) = Lang::of(path) {
                    entry.1.entry(lang).or_insert(path);
                }
            }
            let mut selected = BTreeSet::new();
            let mut unsupported = BTreeMap::new();
            let (mut line_count, mut footprint) = (0usize, 0usize);
            let by_digest: Vec<_> = by_digest.into_iter().collect();
            // Files are read in parallel a batch at a time, so only one batch of new entries is
            // held before the index budget is checked.
            for batch in by_digest.chunks(64) {
                analysis::check_cancel(cancel)?;
                let todo: Vec<_> = batch
                    .iter()
                    .filter(|(digest, (_, langs))| {
                        !self
                            .cache
                            .get(*digest)
                            .is_some_and(|e| langs.keys().all(|l| e.code.contains_key(l)))
                    })
                    .collect();
                let read = parallel(&todo, |(digest, (file, langs))| {
                    (*digest, index(file, langs, cancel))
                });
                let mut read = read
                    .into_iter()
                    .map(|(digest, entry)| Ok((digest, entry?)))
                    .collect::<Result<BTreeMap<_, _>>>()?;
                for digest in batch.iter().map(|(digest, _)| *digest) {
                    let candidate = match read.remove(digest) {
                        Some(Ok(entry)) => Some(entry),
                        Some(Err(reason)) => {
                            unsupported.insert(digest.to_owned(), reason);
                            continue;
                        }
                        None => None,
                    };
                    let entry = candidate.as_ref().or(self.cache.get(digest)).unwrap();
                    if line_count + entry.lines.len() > 1_000_000
                        || footprint + entry.footprint > analysis::MAX_TOTAL as usize
                    {
                        unsupported.insert(
                            digest.to_owned(),
                            "Index exceeds 1,000,000 lines or 256 MiB; references not assessed"
                                .into(),
                        );
                        continue;
                    }
                    line_count += entry.lines.len();
                    footprint += entry.footprint;
                    if entry.long_lines {
                        unsupported.insert(
                            digest.to_owned(),
                            "Lines longer than 4 KiB were not assessed".into(),
                        );
                    }
                    if let Some(reason) = entry.code.values().find_map(|c| c.unassessed) {
                        unsupported.insert(digest.to_owned(), reason.into());
                    }
                    selected.insert(digest.to_owned());
                    if let Some(entry) = candidate {
                        self.cache.insert(digest.to_owned(), entry);
                    }
                }
            }
            self.cache.retain(|digest, _| selected.contains(digest));
            let exists = |p: &str| target.files.contains_key(p) || changed.contains_key(p);
            // (change, file, name) of each changed name a file imports by name
            let mut imported = BTreeSet::new();
            let mut statements = BTreeSet::new();
            let mut match_steps = 0usize;
            for (path, file) in &target.files {
                analysis::check_cancel(cancel)?;
                if let Some(reason) = unsupported.get(&file.digest) {
                    excluded.insert(path.clone(), reason.clone());
                }
                let Some(entry) = self.cache.get(&file.digest) else {
                    continue;
                };
                let lang = Lang::of(path);
                let code = lang.and_then(|lang| Some((lang, entry.code.get(&lang)?)));
                for (lang, import) in code
                    .iter()
                    .flat_map(|(l, c)| c.imports.iter().map(move |i| (*l, i)))
                {
                    let Some((named, names)) = resolve(lang, path, import, &exists) else {
                        continue;
                    };
                    let Some(&change) = changed.get(&named) else {
                        continue;
                    };
                    if path == &change.path {
                        continue;
                    }
                    let line = import.line;
                    // one finding for the paths a `use` tree imports from the changed file
                    if statements.insert((change.id.as_str(), path.as_str(), line)) {
                        if findings.len() >= 2000 {
                            bail!("Impact exceeds 2,000 findings; narrow the scope");
                        }
                        let spec = &import.spec;
                        let rule_id = format!("import-reference-v3:{}", lang.name());
                        findings.push(ImpactFinding {
                            id: analysis::identity(&(&change.id, path, line, &file.digest, spec, &rule_id)),
                            origin_change_id: change.id.clone(),
                            related_paths: vec![path.clone(), named.clone()],
                            relation_type: if lang == Lang::Json { "json_ref" } else { "import" }.into(),
                            evidence: Evidence {
                                path: path.clone(),
                                line,
                                excerpt: entry.line(line).chars().take(240).collect(),
                                digest: file.digest.clone(),
                            },
                            severity: "review".into(),
                            confidence: "exact".into(),
                            rule_id,
                            limitations: "This import/include/$ref statement resolves to exactly the changed path. Statements are read from the syntax tree (Tree-sitter) of JavaScript/TypeScript, Python, C/C++ and Rust files and from the tokens of others, so comments and strings are not imports; conditional, generated or macro-built imports can be missed. It is not proof of a build or runtime failure.".into(),
                        });
                    }
                    let Some(changed_names) = symbols.get(change.path.as_str()) else {
                        continue;
                    };
                    for (name, local) in &names {
                        let Some(&(kind, gone)) = changed_names.get(name) else {
                            continue;
                        };
                        if !imported.insert((change.id.as_str(), path.as_str(), name.clone())) {
                            continue;
                        }
                        if findings.len() >= 2000 {
                            bail!("Impact exceeds 2,000 findings; narrow the scope");
                        }
                        let (_, code) = code.expect("a file with imports");
                        let line = code.uses.get(local).copied().unwrap_or(line);
                        let rule_id = format!("symbol-reference-v2:{}", lang.name());
                        findings.push(ImpactFinding {
                            id: analysis::identity(&(&change.id, path, line, &file.digest, name, &rule_id)),
                            origin_change_id: change.id.clone(),
                            related_paths: vec![path.clone(), named.clone()],
                            relation_type: format!("{}_reference", kind.name()),
                            evidence: Evidence {
                                path: path.clone(),
                                line,
                                excerpt: entry.line(line).chars().take(240).collect(),
                                digest: file.digest.clone(),
                            },
                            severity: "review".into(),
                            confidence: "exact".into(),
                            rule_id,
                            limitations: format!(
                                "Uses {name}, a {} {} {}, imported from it by name. Whether this use still builds and behaves the same is not checked.",
                                kind.name(),
                                if gone { "removed from" } else { "changed in" },
                                change.path
                            ),
                        });
                    }
                }
                for (line, source) in &entry.lines {
                    analysis::check_cancel(cancel)?;
                    let mut seen = BTreeSet::new();
                    for matched in matcher.find_overlapping_iter(source) {
                        match_steps += 1;
                        if match_steps.is_multiple_of(1024) {
                            analysis::check_cancel(cancel)?;
                        }
                        if match_steps > 10_000_000 {
                            bail!("Impact reference matching budget exceeded; narrow the scope");
                        }
                        let boundary =
                            |c: char| !(c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'));
                        if !source[..matched.start()]
                            .chars()
                            .next_back()
                            .is_none_or(|c| c == '/' || boundary(c))
                            || !source[matched.end()..].chars().next().is_none_or(boundary)
                        {
                            continue;
                        }
                        let (needle, origins) = &rules[matched.pattern().as_usize()];
                        for &(change, relation) in origins {
                            // a name of a definition, only where the same language could use it
                            if relation == "symbol_name_reference"
                                && (lang.is_none()
                                    || lang != Lang::of(&change.path)
                                    || imported.contains(&(
                                        change.id.as_str(),
                                        path.as_str(),
                                        needle.clone(),
                                    )))
                            {
                                continue;
                            }
                            let name = relation == "symbol_name_reference";
                            // the first use as a name, outside comments, strings and imports
                            if path == &change.path
                                || !seen.insert((change.id.as_str(), relation, needle.as_str()))
                                || name && code.and_then(|(_, c)| c.uses.get(needle)) != Some(line)
                            {
                                continue;
                            }
                            let word = name || relation == "config_key_occurrence";
                            if word && word_findings.len() >= MAX_WORD_FINDINGS {
                                words_left_out += 1;
                                continue;
                            }
                            if !word && findings.len() >= 2000 {
                                bail!("Impact exceeds 2,000 findings; narrow the scope");
                            }
                            let rule_id = format!("literal-reference-v1:{relation}");
                            let limitations = if name {
                                format!("{needle} names a definition changed or removed in {}; this file does not import it by name from there, so the word may name something else. Inspect or dismiss it manually.", change.path)
                            } else {
                                "A delimited name/path occurrence is evidence of text only; it does not prove a dependency or runtime/build/test failure. Inspect or dismiss it manually.".into()
                            };
                            let finding = ImpactFinding {
                                id: analysis::identity(&(
                                    &change.id,
                                    path,
                                    line,
                                    &file.digest,
                                    needle,
                                    &rule_id,
                                )),
                                origin_change_id: change.id.clone(),
                                related_paths: vec![path.clone()],
                                relation_type: relation.into(),
                                evidence: Evidence {
                                    path: path.clone(),
                                    line: *line,
                                    excerpt: source[source[..matched.start()]
                                        .char_indices()
                                        .rev()
                                        .nth(80)
                                        .map_or(0, |(at, _)| at)..]
                                        .chars()
                                        .take(240)
                                        .collect(),
                                    digest: file.digest.clone(),
                                },
                                severity: "review".into(),
                                confidence: "heuristic".into(),
                                rule_id,
                                limitations,
                            };
                            if word {
                                word_findings.push(finding);
                            } else {
                                findings.push(finding);
                            }
                        }
                    }
                }
            }
        }
        let room = 2000 - findings.len();
        if word_findings.len() > room {
            words_left_out += word_findings.len() - room;
            word_findings.truncate(room);
        }
        let mut notes = Vec::new();
        if words_left_out > 0 {
            notes.push(format!(
                "{words_left_out} more occurrences of changed definition names and configuration keys were left out after the first {}; the other findings are complete.",
                word_findings.len()
            ));
        }
        findings.append(&mut word_findings);
        let exact: BTreeSet<_> = findings
            .iter()
            .filter(|f| f.confidence == "exact")
            .map(|f| {
                (
                    f.origin_change_id.clone(),
                    f.evidence.path.clone(),
                    f.evidence.line,
                )
            })
            .collect();
        findings.retain(|f| {
            f.confidence == "exact"
                || !exact.contains(&(
                    f.origin_change_id.clone(),
                    f.evidence.path.clone(),
                    f.evidence.line,
                ))
        });
        findings.sort_by(|a, b| {
            (
                &a.origin_change_id,
                &a.evidence.path,
                a.evidence.line,
                &a.rule_id,
            )
                .cmp(&(
                    &b.origin_change_id,
                    &b.evidence.path,
                    b.evidence.line,
                    &b.rule_id,
                ))
        });
        let assessment = if findings.is_empty() {
            "No supported references found; impact is not assessed"
        } else {
            "Potential references found; all findings require review"
        }
        .into();
        Ok(ImpactReport {
            version: 1,
            baseline_fingerprint: target.metadata.fingerprint.clone(),
            findings,
            assessment,
            excluded,
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        baseline::{capture, compare, BaselineSpec},
        diff::DiffOptions,
    };
    #[test]
    fn imports_resolving_to_changed_files_are_exact_and_text_matches_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let write = |root: &std::path::Path, files: &[(&str, &str)]| {
            for (path, text) in files {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
        };
        write(
            &a,
            &[
                ("src/util.ts", "export const x = 1;\n"),
                ("src/gone.js", "module.exports = 1;\n"),
                ("pkg/helpers.py", "def f(): return 1\n"),
                ("lib/x.h", "int x(void);\n"),
                ("src/parser.rs", "pub fn parse() {}\n"),
                ("schema/defs.json", r#"{"a":{"type":"string"}}"#),
                ("config.json", r#"{"db":{"timeout":30}}"#),
                ("hosts.json", r#"{"servers":[{"host":"alpha"}]}"#),
                ("src/same.ts", "export const same = 1;\n"),
            ],
        );
        write(
            &b,
            &[
                ("src/util.ts", "export const x = 2;\n"),
                ("pkg/helpers.py", "def f(): return 2\n"),
                ("lib/x.h", "int x(int);\n"),
                ("src/parser.rs", "pub fn parse(input: &str) {}\n"),
                ("schema/defs.json", r#"{"a":{"type":"number"}}"#),
                ("config.json", r#"{"db":{"timeout":60}}"#),
                ("hosts.json", r#"{"servers":[{"host":"beta"}]}"#),
                ("deploy.py", "HOST = get(\"servers.host\")\n"),
                ("src/same.ts", "export const same = 1;\n"),
                (
                    "src/app.ts",
                    "import { x } from \"./util\";\nimport { same } from './same';\n",
                ),
                ("src/use.js", "const g = require(\"./gone\");\n"),
                ("src/other.ts", "// util.ts holds the constant\n"),
                ("main.py", "from pkg.helpers import f\n"),
                ("pkg/user.py", "from .helpers import f\n"),
                ("lib/x.c", "#include \"x.h\"\n"),
                ("src/lib.rs", "pub mod parser;\n"),
                ("schema/root.json", r#"{"$ref": "defs.json#/a"}"#),
                ("settings.py", "TIMEOUT = get(\"db.timeout\")\n"),
            ],
        );
        let cancel = AtomicBool::new(false);
        let take = |root: &std::path::Path| {
            capture(
                &BaselineSpec::Folder {
                    path: root.to_owned(),
                },
                &DiffOptions::default(),
                "*",
                &cancel,
                &AtomicUsize::new(0),
            )
            .unwrap()
        };
        let (left, right) = (take(&a), take(&b));
        let summary = compare(&left, &right, &[], &cancel).unwrap();
        let report = Index::default()
            .analyze(&summary, &left, &right, &cancel)
            .unwrap();
        let found: BTreeSet<_> = report
            .findings
            .iter()
            .map(|f| {
                (
                    f.evidence.path.as_str(),
                    f.evidence.line,
                    f.relation_type.as_str(),
                    f.confidence.as_str(),
                )
            })
            .collect();
        for expected in [
            ("src/app.ts", 1, "import", "exact"),
            ("src/use.js", 1, "import", "exact"),
            ("main.py", 1, "import", "exact"),
            ("pkg/user.py", 1, "import", "exact"),
            ("lib/x.c", 1, "import", "exact"),
            ("src/lib.rs", 1, "import", "exact"),
            ("schema/root.json", 1, "json_ref", "exact"),
            ("src/other.ts", 1, "filename_reference", "heuristic"),
            ("settings.py", 1, "config_key_path", "heuristic"),
        ] {
            assert!(found.contains(&expected), "{expected:?} not in {found:#?}");
        }
        assert!(!found.iter().any(|f| f.0 == "src/app.ts" && f.1 == 2));
        assert!(
            !found
                .iter()
                .any(|f| f.0 == "deploy.py" && f.2 == "config_key_path"),
            "a dotted path skips the array index: {found:#?}"
        );
        assert_eq!(
            found.iter().filter(|f| f.0 == "schema/root.json").count(),
            1,
            "an exact finding replaces text matches on its line: {found:#?}"
        );
        let user = report
            .findings
            .iter()
            .find(|f| f.evidence.path == "pkg/user.py")
            .unwrap();
        assert_eq!(user.related_paths, ["pkg/user.py", "pkg/helpers.py"]);
        assert_eq!(user.rule_id, "import-reference-v3:python");
    }

    #[test]
    fn changed_functions_and_types_imported_by_name_are_exact_and_other_uses_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let write = |root: &std::path::Path, files: &[(&str, &str)]| {
            for (path, text) in files {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
        };
        let shared: &[(&str, &str)] = &[
            (
                "src/app.ts",
                "import {\n  parse as p,\n  LIMIT,\n  gone,\n} from \"./parser\";\n\nconst x = p(LIMIT);\ngone();\n",
            ),
            (
                "src/commented.ts",
                "// import { parse } from \"./parser\";\nparse(\"x\");\n",
            ),
            ("notes.md", "call parse() here\n"),
            ("src/other.py", "parse = 1\n"),
            ("pkg/user.py", "from .helpers import (\n    shape,\n)\n\nshape()\n"),
            (
                "rs/main.rs",
                "mod parser;\nuse crate::parser::{self, Config as C};\nfn main() { let c: C = parser::make(); }\n",
            ),
        ];
        write(&a, shared);
        write(&b, shared);
        write(
            &a,
            &[
                (
                    "src/parser.ts",
                    "export const LIMIT = 3;\nexport function parse(a) { return a; }\nexport function gone() {}\n",
                ),
                ("pkg/helpers.py", "def shape():\n    return 1\n"),
                ("rs/parser.rs", "pub struct Config { a: u8 }\npub fn make() -> Config { Config { a: 1 } }\n"),
            ],
        );
        write(
            &b,
            &[
                (
                    "src/parser.ts",
                    "export const LIMIT = 3;\nexport function parse(a, b) { return a + b; }\n",
                ),
                ("pkg/helpers.py", "def shape(size):\n    return size\n"),
                (
                    "rs/parser.rs",
                    "pub struct Config { a: u16 }\npub fn make() -> Config { Config { a: 1 } }\n",
                ),
            ],
        );
        let cancel = AtomicBool::new(false);
        let take = |root: &std::path::Path| {
            capture(
                &BaselineSpec::Folder {
                    path: root.to_owned(),
                },
                &DiffOptions::default(),
                "*",
                &cancel,
                &AtomicUsize::new(0),
            )
            .unwrap()
        };
        let (left, right) = (take(&a), take(&b));
        let summary = compare(&left, &right, &[], &cancel).unwrap();
        let report = Index::default()
            .analyze(&summary, &left, &right, &cancel)
            .unwrap();
        let found: BTreeSet<_> = report
            .findings
            .iter()
            .map(|f| {
                (
                    f.evidence.path.as_str(),
                    f.evidence.line,
                    f.relation_type.as_str(),
                    f.confidence.as_str(),
                )
            })
            .collect();
        for expected in [
            ("src/app.ts", 1, "import", "exact"),
            ("src/app.ts", 7, "function_reference", "exact"),
            ("src/app.ts", 8, "function_reference", "exact"),
            ("pkg/user.py", 1, "import", "exact"),
            ("pkg/user.py", 5, "function_reference", "exact"),
            ("rs/main.rs", 1, "import", "exact"),
            ("rs/main.rs", 3, "type_reference", "exact"),
            ("src/commented.ts", 2, "symbol_name_reference", "heuristic"),
        ] {
            assert!(found.contains(&expected), "{expected:?} not in {found:#?}");
        }
        assert!(
            !found
                .iter()
                .any(|f| f.0 == "src/commented.ts" && f.3 == "exact"),
            "an import in a comment is not an import: {found:#?}"
        );
        assert!(
            !found.contains(&("src/commented.ts", 1, "symbol_name_reference", "heuristic")),
            "a name in a comment is not a use: {found:#?}"
        );
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.evidence.path == "rs/main.rs"
                    && f.evidence.line == 2
                    && f.relation_type == "import")
                .count(),
            1,
            "one finding for a use tree: {found:#?}"
        );
        assert!(
            !found
                .iter()
                .any(|f| (f.0 == "notes.md" || f.0 == "src/other.py")
                    && f.2 == "symbol_name_reference"),
            "names are only looked for in the same language: {found:#?}"
        );
        assert!(
            !found
                .iter()
                .any(|f| f.0 == "src/app.ts" && f.2 == "symbol_name_reference"),
            "an exact use replaces name matches in the file: {found:#?}"
        );
        assert!(
            !found
                .iter()
                .any(|f| f.0 == "src/app.ts" && f.2 == "value_reference"),
            "LIMIT did not change: {found:#?}"
        );
        let gone = report
            .findings
            .iter()
            .find(|f| f.evidence.path == "src/app.ts" && f.evidence.line == 8)
            .unwrap();
        assert!(gone
            .limitations
            .contains("gone, a function removed from src/parser.ts"));
        assert_eq!(gone.related_paths, ["src/app.ts", "src/parser.ts"]);
        assert_eq!(gone.evidence.excerpt, "gone();");
    }

    #[test]
    fn a_broad_refactor_lists_the_first_name_matches_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let names: Vec<String> = (0..30).map(|i| format!("helper{i:02}")).collect();
        let lib = |body: &str| {
            names
                .iter()
                .map(|n| format!("export function {n}() {{ return {body}; }}\n"))
                .collect::<String>()
        };
        let calls: String = names.iter().map(|n| format!("{n}();\n{n}();\n")).collect();
        for (root, body) in [(&a, "0"), (&b, "1")] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("lib.ts"), lib(body)).unwrap();
            for i in 0..30 {
                std::fs::write(root.join(format!("use{i:02}.ts")), &calls).unwrap();
            }
            std::fs::write(root.join("imports.ts"), "import \"./lib\";\n").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let take = |root: &std::path::Path| {
            capture(
                &BaselineSpec::Folder {
                    path: root.to_owned(),
                },
                &DiffOptions::default(),
                "*",
                &cancel,
                &AtomicUsize::new(0),
            )
            .unwrap()
        };
        let (left, right) = (take(&a), take(&b));
        let summary = compare(&left, &right, &[], &cancel).unwrap();
        let report = Index::default()
            .analyze(&summary, &left, &right, &cancel)
            .unwrap();
        let named = |f: &&ImpactFinding| f.relation_type == "symbol_name_reference";
        assert_eq!(
            report.findings.iter().filter(named).count(),
            MAX_WORD_FINDINGS
        );
        assert!(report
            .findings
            .iter()
            .filter(named)
            .all(|f| f.evidence.line % 2 == 1));
        assert!(report
            .findings
            .iter()
            .any(|f| f.evidence.path == "imports.ts" && f.relation_type == "import"));
        assert_eq!(report.notes.len(), 1);
        assert!(
            report.notes[0].starts_with("400 more occurrences"),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn import_specifiers_resolve_inside_the_root_only() {
        assert!(candidates(Lang::C, "a.c", "../../etc/passwd").is_empty());
        assert_eq!(
            candidates(Lang::Rust, "src/a.rs", "b"),
            ["src/a/b.rs", "src/a/b/mod.rs"]
        );
        assert_eq!(
            candidates(Lang::Python, "p/q/m.py", "..r"),
            ["p/r.py", "p/r/__init__.py"]
        );
        let files = [
            "app/src/main.rs",
            "app/src/a/b.rs",
            "app/src/a/c.rs",
            "app/src/a.rs",
        ];
        let exists = |p: &str| files.contains(&p);
        let module = |from: &str, path: &str| {
            rust_module_files(from, &path.split("::").collect::<Vec<_>>(), &exists)
        };
        assert_eq!(
            module("app/src/a/b.rs", "crate::a::c"),
            ["app/src/a/c.rs", "app/src/a/c/mod.rs"]
        );
        assert_eq!(module("app/src/a/b.rs", "super::c")[0], "app/src/a/c.rs");
        assert_eq!(module("app/src/a/b.rs", "super")[0], "app/src/a.rs");
        assert_eq!(module("app/src/a.rs", "self::b")[0], "app/src/a/b.rs");
        assert_eq!(module("app/src/main.rs", "super::x"), Vec::<String>::new());
        assert!(module("lib/x.rs", "crate::a").is_empty(), "no crate root");
        let import = |spec: &str, names: &[&str]| code::Import {
            spec: spec.into(),
            line: 1,
            names: names
                .iter()
                .map(|n| (n.to_string(), n.to_string()))
                .collect(),
            tokens: 0..0,
        };
        assert_eq!(
            resolve(
                Lang::Rust,
                "app/src/main.rs",
                &import("use:crate::a::c::run", &["run"]),
                &exists
            ),
            Some(("app/src/a/c.rs".into(), vec![("run".into(), "run".into())]))
        );
        assert_eq!(
            resolve(
                Lang::Rust,
                "app/src/main.rs",
                &import("use:crate::a::c", &["c"]),
                &exists
            ),
            Some(("app/src/a/c.rs".into(), vec![]))
        );
    }
}
