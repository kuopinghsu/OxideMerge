//! Localization. Language packs are JSON objects mapping keys to translated strings.
//! Built-in packs are embedded; extra packs can be dropped as `<code>.json` into a
//! `Languages` folder next to the executable or in the user configuration folder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

type Table = HashMap<String, &'static str>;

const EMBEDDED: &[(&str, &str)] = &[
    ("en", include_str!("../lang/en.json")),
    ("zh-TW", include_str!("../lang/zh-TW.json")),
    ("zh-CN", include_str!("../lang/zh-CN.json")),
    ("ja", include_str!("../lang/ja.json")),
    ("ko", include_str!("../lang/ko.json")),
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("es", include_str!("../lang/es.json")),
    ("ru", include_str!("../lang/ru.json")),
    ("pt-BR", include_str!("../lang/pt-BR.json")),
];

#[derive(Clone, Debug)]
pub struct LanguageInfo {
    pub code: String,
    pub name: String,
    source: Source,
}

#[derive(Clone, Debug)]
enum Source {
    Embedded(&'static str),
    File(PathBuf),
}

static LANGS: OnceLock<Vec<LanguageInfo>> = OnceLock::new();
static ENGLISH: OnceLock<Table> = OnceLock::new();
static CURRENT: RwLock<Option<(&'static str, &'static Table)>> = RwLock::new(None);

fn parse(text: &str) -> Table {
    let raw: HashMap<String, String> = serde_json::from_str(text).unwrap_or_default();
    raw.into_iter()
        .map(|(k, v)| (k, &*Box::leak(v.into_boxed_str())))
        .collect()
}

fn english() -> &'static Table {
    ENGLISH.get_or_init(|| parse(EMBEDDED[0].1))
}

fn lang_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("Languages"));
        }
    }
    if let Some(c) = crate::config::config_dir() {
        v.push(c.join("Languages"));
    }
    v
}

fn name_of(text: &str) -> Option<String> {
    let raw: HashMap<String, String> = serde_json::from_str(text).ok()?;
    raw.get("_name").cloned()
}

pub fn languages() -> &'static [LanguageInfo] {
    LANGS.get_or_init(|| {
        let mut v: Vec<LanguageInfo> = EMBEDDED
            .iter()
            .map(|(code, text)| LanguageInfo {
                code: code.to_string(),
                name: name_of(text).unwrap_or_else(|| code.to_string()),
                source: Source::Embedded(text),
            })
            .collect();
        for dir in lang_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x.eq_ignore_ascii_case("json")).unwrap_or(false) {
                    let code = p.file_stem().unwrap().to_string_lossy().into_owned();
                    let name = std::fs::read_to_string(&p).ok().and_then(|t| name_of(&t)).unwrap_or(code.clone());
                    v.retain(|l| l.code != code);
                    v.push(LanguageInfo { code, name, source: Source::File(p) });
                }
            }
        }
        v
    })
}

/// Best matching language code for the operating system locale.
pub fn system_language() -> String {
    let loc = sys_locale::get_locale().unwrap_or_else(|| "en".into());
    let loc = loc.replace('_', "-");
    let langs = languages();
    if let Some(l) = langs.iter().find(|l| l.code.eq_ignore_ascii_case(&loc)) {
        return l.code.clone();
    }
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("zh") {
        let traditional = lower.contains("tw") || lower.contains("hk") || lower.contains("mo") || lower.contains("hant");
        return if traditional { "zh-TW".into() } else { "zh-CN".into() };
    }
    let primary = lower.split('-').next().unwrap_or("en");
    langs
        .iter()
        .find(|l| l.code.to_ascii_lowercase().split('-').next() == Some(primary))
        .map(|l| l.code.clone())
        .unwrap_or_else(|| "en".into())
}

pub fn set_language(code: &str) {
    let code = if code.is_empty() { system_language() } else { code.to_string() };
    let info = languages().iter().find(|l| l.code == code).cloned();
    let table: &'static Table = match info.map(|i| i.source) {
        Some(Source::Embedded(text)) => Box::leak(Box::new(parse(text))),
        Some(Source::File(p)) => Box::leak(Box::new(parse(&std::fs::read_to_string(p).unwrap_or_default()))),
        None => english(),
    };
    let code: &'static str = Box::leak(code.into_boxed_str());
    *CURRENT.write().unwrap() = Some((code, table));
}

pub fn current_language() -> &'static str {
    CURRENT.read().unwrap().map(|(c, _)| c).unwrap_or("en")
}

pub fn tr(key: &'static str) -> &'static str {
    if let Some((_, t)) = *CURRENT.read().unwrap() {
        if let Some(v) = t.get(key) {
            return v;
        }
    }
    english().get(key).copied().unwrap_or(key)
}

/// Translates and substitutes `{name}` placeholders.
pub fn trf(key: &'static str, args: &[(&str, String)]) -> String {
    let mut s = tr(key).to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_languages_have_all_keys() {
        let en = english();
        for (code, text) in EMBEDDED {
            let t = parse(text);
            assert!(!t.is_empty(), "{code} failed to parse");
            let missing: Vec<&String> = en.keys().filter(|k| !t.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{code} is missing {missing:?}");
        }
    }

    #[test]
    fn translations_keep_placeholders() {
        let re = regex::Regex::new(r"\{[a-z]+\}").unwrap();
        let names = |s: &str| {
            let mut v: Vec<String> = re.find_iter(s).map(|m| m.as_str().to_string()).collect();
            v.sort();
            v
        };
        let en = english();
        for (code, text) in EMBEDDED {
            for (k, v) in parse(text) {
                if let Some(e) = en.get(&k) {
                    assert_eq!(names(e), names(v), "{code}: placeholders differ in {k}");
                }
            }
        }
    }
}
