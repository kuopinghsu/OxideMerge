//! Localization. Language packs are JSON objects mapping keys to translated strings.
//! Built-in packs are embedded; extra packs can be dropped as `<code>.json` into a
//! `Languages` folder next to the executable or in the user configuration folder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::RwLock;

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
    ("it", include_str!("../lang/it.json")),
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
type Current = Option<(&'static str, &'static Table)>;

#[cfg(not(test))]
static CURRENT: RwLock<Current> = RwLock::new(None);

// Tests run in parallel; one switching the language must not change the text others check.
#[cfg(test)]
thread_local! {
    static CURRENT: std::cell::Cell<Current> = const { std::cell::Cell::new(None) };
}

fn current() -> Current {
    #[cfg(not(test))]
    return *CURRENT.read().unwrap();
    #[cfg(test)]
    return CURRENT.get();
}

fn set_current(c: Current) {
    #[cfg(not(test))]
    {
        *CURRENT.write().unwrap() = c;
    }
    #[cfg(test)]
    CURRENT.set(c);
}

fn parse(text: &str) -> Table {
    let raw: HashMap<String, String> = serde_json::from_str(text).unwrap_or_default();
    raw.into_iter()
        .map(|(k, v)| (k, &*Box::leak(v.into_boxed_str())))
        .collect()
}

/// The `{name}` placeholders in `s`, sorted.
fn placeholders(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s
        .match_indices('{')
        .filter_map(|(i, _)| {
            let rest = &s[i + 1..];
            let end = rest.find('}')?;
            let name = &rest[..end];
            (!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase()))
                .then_some(&s[i..i + end + 2])
        })
        .collect();
    v.sort_unstable();
    v
}

/// A language pack from a file: only text values of known keys, with the same placeholders
/// as in English, are used. English is shown for anything else.
fn parse_pack(text: &str) -> Table {
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(text).unwrap_or_default();
    let en = english();
    raw.into_iter()
        .filter_map(|(k, v)| {
            let v = v.as_str()?;
            let e = en.get(&k)?;
            (k == "_name" || placeholders(e) == placeholders(v))
                .then(|| (k, &*Box::leak(v.to_owned().into_boxed_str())))
        })
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
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension()
                    .map(|x| x.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
                {
                    let code = p.file_stem().unwrap().to_string_lossy().into_owned();
                    let name = std::fs::read_to_string(&p)
                        .ok()
                        .and_then(|t| name_of(&t))
                        .unwrap_or(code.clone());
                    v.retain(|l| l.code != code);
                    v.push(LanguageInfo {
                        code,
                        name,
                        source: Source::File(p),
                    });
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
        let traditional = lower.contains("tw")
            || lower.contains("hk")
            || lower.contains("mo")
            || lower.contains("hant");
        return if traditional {
            "zh-TW".into()
        } else {
            "zh-CN".into()
        };
    }
    let primary = lower.split('-').next().unwrap_or("en");
    langs
        .iter()
        .find(|l| l.code.to_ascii_lowercase().split('-').next() == Some(primary))
        .map(|l| l.code.clone())
        .unwrap_or_else(|| "en".into())
}

pub fn set_language(code: &str) {
    let code = if code.is_empty() {
        system_language()
    } else {
        code.to_string()
    };
    let info = languages().iter().find(|l| l.code == code).cloned();
    let table: &'static Table = match info.map(|i| i.source) {
        Some(Source::Embedded(text)) => Box::leak(Box::new(parse(text))),
        Some(Source::File(p)) => Box::leak(Box::new(parse_pack(
            &std::fs::read_to_string(p).unwrap_or_default(),
        ))),
        None => english(),
    };
    let code: &'static str = Box::leak(code.into_boxed_str());
    set_current(Some((code, table)));
}

pub fn current_language() -> &'static str {
    current().map(|(c, _)| c).unwrap_or("en")
}

pub fn tr(key: &'static str) -> &'static str {
    if let Some((_, t)) = current() {
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
    fn analysis_strings_have_explicit_english_fallback_outside_traditional_chinese() {
        let en = english();
        for (code, text) in EMBEDDED {
            if *code == "en" || *code == "zh-TW" {
                continue;
            }
            let table = parse(text);
            for (key, value) in en.iter().filter(|(k, _)| {
                ["analysis.", "workspace.", "baseline.", "impact."]
                    .iter()
                    .any(|p| k.starts_with(p))
            }) {
                assert_eq!(
                    table.get(key),
                    Some(value),
                    "Intentional English fallback for {code}: {key}"
                );
            }
        }
    }

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
        let en = english();
        for (code, text) in EMBEDDED {
            let t = parse(text);
            let extra: Vec<&String> = t.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(extra.is_empty(), "{code} has unknown keys {extra:?}");
            for (k, v) in t {
                assert!(!v.trim().is_empty(), "{code}: {k} is empty");
                assert_eq!(
                    placeholders(en[&k]),
                    placeholders(v),
                    "{code}: placeholders differ in {k}"
                );
            }
        }
        assert_eq!(placeholders("{b} of {a}, {} {X}"), ["{a}", "{b}"]);
    }

    /// The keys a message names (`Ctrl+Shift+V`, `F5`), with the modifier names some languages
    /// translate (German *Strg*, *Umschalt*) read as the English ones, sorted.
    fn shortcuts(s: &str) -> Vec<String> {
        let re = regex::Regex::new(
            r"\b(?:(?:Ctrl|Strg|Alt|Shift|Umschalt|Cmd)\+)+[A-Za-z0-9]+|\bF\d{1,2}\b",
        )
        .unwrap();
        let mut v: Vec<String> = re
            .find_iter(s)
            .map(|m| {
                m.as_str()
                    .split('+')
                    .map(|part| match part {
                        "Strg" => "Ctrl",
                        "Umschalt" => "Shift",
                        part => part,
                    })
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn translations_name_the_same_keys() {
        let en = english();
        for (code, text) in EMBEDDED {
            for (k, v) in parse(text) {
                assert_eq!(
                    shortcuts(en[&k]),
                    shortcuts(v),
                    "{code}: {k} names other keys than English: {v}"
                );
            }
        }
        assert_eq!(shortcuts("Mit Strg+V oder F5, Alto"), ["Ctrl+V", "F5"]);
        assert_eq!(
            shortcuts("Strg+Umschalt+V, Ctrl+Alt+Shift+F12, Shift+F3"),
            ["Ctrl+Alt+Shift+F12", "Ctrl+Shift+V", "Shift+F3"]
        );
    }

    /// JSON lets a key appear twice, and the last one silently wins.
    #[test]
    fn language_files_name_each_key_once() {
        for (code, text) in EMBEDDED {
            let mut seen = std::collections::HashSet::new();
            for line in text.lines() {
                let Some(rest) = line.trim_start().strip_prefix('"') else {
                    continue;
                };
                let Some(end) = rest.find("\":") else {
                    continue;
                };
                assert!(
                    seen.insert(&rest[..end]),
                    "{code}: {} is given twice",
                    &rest[..end]
                );
            }
        }
    }

    /// Warnings before something is deleted, replaced or lost are translated in every
    /// language, apart from the areas deliberately left in English.
    #[test]
    fn warnings_of_lost_work_are_translated() {
        let en = english();
        let risky =
            regex::Regex::new(r"(?i)delet|overwrit|discard|replac|undone|\blos[et]").unwrap();
        let english_on_purpose = ["analysis.", "workspace.", "baseline.", "impact."];
        for (code, text) in EMBEDDED.iter().filter(|(c, _)| *c != "en") {
            let t = parse(text);
            let left: Vec<&String> = en
                .iter()
                .filter(|(k, v)| {
                    risky.is_match(v)
                        && !english_on_purpose.iter().any(|p| k.starts_with(p))
                        && t.get(*k) == Some(*v)
                })
                .map(|(k, _)| k)
                .collect();
            assert!(
                left.is_empty(),
                "{code} leaves these warnings in English: {left:?}"
            );
        }
    }

    /// The structure of a document: its headings' levels and its anchors, in order.
    fn outline(path: &str) -> Vec<String> {
        let text =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut code = false;
        text.lines()
            .filter_map(|l| {
                if l.starts_with("```") || l == "----" {
                    code = !code;
                }
                if code {
                    return None;
                }
                let level = l.bytes().take_while(|&b| b == b'=' || b == b'#').count();
                if level > 0 && l[level..].starts_with(' ') {
                    return Some(format!("h{level}"));
                }
                l.strip_prefix("[[")
                    .and_then(|a| a.strip_suffix("]]"))
                    .map(|a| format!("[[{a}]]"))
            })
            .collect()
    }

    /// The English and Traditional Chinese documents have the same sections, at the same
    /// levels, with the same anchors, and every cross reference in a user guide leads to one.
    #[test]
    fn english_and_chinese_documents_keep_the_same_structure() {
        for (en, zh) in [
            ("docs/user-guide.adoc", "docs/user-guide.zh-TW.adoc"),
            ("docs/development.md", "docs/development.zh-TW.md"),
            ("README.md", "README_zh-TW.md"),
        ] {
            let (a, b) = (outline(en), outline(zh));
            assert!(a.len() > 10, "{en} has an outline");
            if let Some(i) = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i)) {
                panic!(
                    "{zh} differs from {en} at item {i}: {:?} against {:?}",
                    b.get(i.saturating_sub(1)..(i + 2).min(b.len())),
                    a.get(i.saturating_sub(1)..(i + 2).min(a.len()))
                );
            }
        }
        let xref = regex::Regex::new(r"<<([A-Za-z0-9_-]+)[,>]").unwrap();
        for guide in ["docs/user-guide.adoc", "docs/user-guide.zh-TW.adoc"] {
            let anchors = outline(guide);
            let text = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(guide),
            )
            .unwrap();
            for c in xref.captures_iter(&text) {
                let target = format!("[[{}]]", &c[1]);
                assert!(
                    anchors.contains(&target),
                    "{guide}: <<{}>> leads nowhere",
                    &c[1]
                );
            }
        }
    }

    #[test]
    fn chinese_packs_keep_to_their_script() {
        // Simplified and Traditional forms of common characters in the interface.
        let pairs = [
            "们們", "这這", "设設", "页頁", "时時", "读讀", "写寫", "过過", "发發", "开開", "关關",
            "选選", "项項", "错錯", "误誤", "输輸", "档檔", "网網", "络絡", "图圖", "应應", "则則",
            "体體", "说說", "认認", "显顯", "无無", "区區", "资資", "转轉", "换換", "复複", "删刪",
            "贴貼", "件件", "夹夾", "较較", "实實", "务務", "并並", "后後", "从從", "个個", "为為",
        ];
        let pairs: Vec<(char, char)> = pairs
            .iter()
            .map(|p| {
                let mut c = p.chars();
                (c.next().unwrap(), c.next().unwrap())
            })
            .filter(|(s, t)| s != t)
            .collect();
        let pack = |code: &str| parse(EMBEDDED.iter().find(|(c, _)| *c == code).unwrap().1);
        for (k, v) in pack("zh-TW") {
            let wrong: String = pairs
                .iter()
                .map(|p| p.0)
                .filter(|c| v.contains(*c))
                .collect();
            assert!(wrong.is_empty(), "zh-TW {k} has Simplified {wrong}: {v}");
        }
        for (k, v) in pack("zh-CN") {
            let wrong: String = pairs
                .iter()
                .map(|p| p.1)
                .filter(|c| v.contains(*c))
                .collect();
            assert!(wrong.is_empty(), "zh-CN {k} has Traditional {wrong}: {v}");
        }
    }

    #[test]
    fn language_files_fall_back_to_english_for_broken_entries() {
        let pack = parse_pack(
            r#"{"_name": "Test", "status.error": "Fehler", "msg.saved": "Gespeichert",
                "msg.op_done": "{n} erledigt", "menu.file": 3, "no.such.key": "x"}"#,
        );
        assert_eq!(pack.get("status.error"), Some(&"Fehler"));
        assert_eq!(pack.get("msg.op_done"), Some(&"{n} erledigt"));
        assert!(!pack.contains_key("msg.saved"), "drops {{file}}");
        assert!(!pack.contains_key("menu.file") && !pack.contains_key("no.such.key"));
        assert!(parse_pack("not json").is_empty());
    }
}
