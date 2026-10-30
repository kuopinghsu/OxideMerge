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
