//! Loads system fonts so that CJK and other scripts render (egui's built-in fonts are Latin/Cyrillic/Greek only).

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use std::sync::Arc;

const WIN_TW: &[&str] = &["msjh.ttc", "mingliub.ttc"];
const WIN_CN: &[&str] = &["msyh.ttc", "simsun.ttc"];
const WIN_JA: &[&str] = &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"];
const WIN_KO: &[&str] = &["malgun.ttf", "gulim.ttc"];

const UNIX_HAN: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
];
const UNIX_HANGUL: &[&str] = &[
    "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
];

/// Fonts for the scripts a PDF report may need beyond the fallbacks of [`fallbacks`]: Windows
/// files, Unix paths and a fontconfig language for Hebrew, Arabic (with its presentation forms)
/// and Thai.
const DOCUMENT_SCRIPTS: &[(&[&str], &[&str], &str)] = &[
    (
        &["segoeui.ttf", "tahoma.ttf"],
        &[
            "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
            "/System/Library/Fonts/SFHebrew.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ],
        "he",
    ),
    (
        &["segoeui.ttf", "tahoma.ttf"],
        &[
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/System/Library/Fonts/GeezaPro.ttc",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ],
        "ar",
    ),
    (
        &["leelawui.ttf", "tahoma.ttf"],
        &[
            "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
            "/usr/share/fonts/truetype/tlwg/Garuda.ttf",
            "/System/Library/Fonts/Supplemental/Tahoma.ttf",
        ],
        "th",
    ),
];

const UI_FONTS: &[&str] = &[
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "/System/Library/Fonts/SFNS.ttf",
    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];
const UI_SEMIBOLD_FONTS: &[&str] = &[
    "C:\\Windows\\Fonts\\seguisb.ttf",
    "/usr/share/fonts/truetype/inter/Inter-SemiBold.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
    "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
];
const MONO_FONTS: &[&str] = &[
    "C:\\Windows\\Fonts\\CascadiaMono.ttf",
    "C:\\Windows\\Fonts\\consola.ttf",
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];

/// Paths of Windows font files; under WSL they are reachable through the mounted C: drive.
fn windows_fonts(names: &[&str]) -> Vec<String> {
    let dir = if cfg!(windows) {
        "C:\\Windows\\Fonts\\"
    } else if cfg!(target_os = "linux") {
        "/mnt/c/Windows/Fonts/"
    } else {
        return Vec::new();
    };
    names.iter().map(|n| format!("{dir}{n}")).collect()
}

/// Asks fontconfig for the preferred sans-serif font that supports `lang` (e.g. "zh-tw").
fn fontconfig(lang: &str) -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    let out = std::process::Command::new("fc-match")
        .args([
            "-s",
            "-f",
            "%{file}\t%{lang}\n",
            &format!("sans-serif:lang={lang}"),
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let (file, langs) = l.split_once('\t')?;
        langs
            .split('|')
            .any(|x| x == lang)
            .then(|| file.to_string())
    })
}

/// Native Windows fonts, then well-known Unix paths, then fontconfig (queried lazily), then the
/// Windows fonts of a WSL host as a last resort.
fn system_fonts(
    win: Vec<&'static str>,
    unix: Vec<String>,
    fc_langs: Vec<&'static str>,
) -> impl Iterator<Item = String> {
    let win = windows_fonts(&win);
    let (native_win, wsl_win) = if cfg!(windows) {
        (win, Vec::new())
    } else {
        (Vec::new(), win)
    };
    native_win
        .into_iter()
        .chain(unix)
        .chain(fc_langs.into_iter().filter_map(fontconfig))
        .chain(wsl_win)
}

/// Ascent and row height as a fraction of the em square (hhea metrics, same source egui uses).
#[derive(Clone, Copy)]
struct VMetrics {
    asc: f32,
    row: f32,
}

fn vertical_metrics(bytes: &[u8], index: u32) -> Option<VMetrics> {
    let face = ttf_parser::Face::parse(bytes, index).ok()?;
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    let asc = f32::from(face.ascender()) / upem;
    let desc = f32::from(face.descender()) / upem;
    let gap = f32::from(face.line_gap()) / upem;
    Some(VMetrics {
        asc,
        row: asc - desc + gap,
    })
}

/// `FontTweak::y_offset_factor` that puts `fallback` on `primary`'s baseline.
///
/// egui centers a fallback font's em box on the primary font
/// (`ascent + 0.5 * (primary_row - fallback_row)`). San Francisco and the macOS CJK fonts
/// do not share an ascent or line gap — Hiragino's line gap is half an em — so Chinese and
/// Latin glyphs on one line sit at different heights. This cancels that shift.
fn baseline_offset_factor(primary: VMetrics, fallback: VMetrics) -> f32 {
    primary.asc - fallback.asc - 0.5 * (primary.row - fallback.row)
}

fn postscript_name(face: &ttf_parser::Face<'_>) -> Option<String> {
    face.names().into_iter().find_map(|n| {
        (n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .then(|| n.to_string())
            .flatten()
    })
}

/// Face whose PostScript name is one of `prefs`, or face 0 when none match.
fn face_index(bytes: &[u8], prefs: &[&str]) -> u32 {
    for pref in prefs {
        let mut index = 0u32;
        while let Ok(face) = ttf_parser::Face::parse(bytes, index) {
            if postscript_name(&face).as_deref() == Some(*pref) {
                return index;
            }
            index += 1;
            if index >= 64 {
                break;
            }
        }
    }
    0
}

fn han_face_prefs(lang: &str) -> &'static [&'static str] {
    match lang {
        "zh-TW" => &[
            "PingFangTC-Regular",
            "PingFangSC-Regular",
            "PingFangHK-Regular",
        ],
        "zh-HK" => &[
            "PingFangHK-Regular",
            "PingFangTC-Regular",
            "PingFangSC-Regular",
        ],
        _ => &[
            "PingFangSC-Regular",
            "PingFangTC-Regular",
            "PingFangHK-Regular",
        ],
    }
}

/// PingFang.ttc shipped inside a MobileAsset download. Newer macOS releases no longer keep a copy at
/// `/System/Library/Fonts/PingFang.ttc`, and the next hardcoded face (Hiragino Sans GB) does not
/// share San Francisco's vertical metrics.
fn asset_fonts(file_name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(root) = std::fs::read_dir("/System/Library/AssetsV2") else {
        return found;
    };
    for dir in root.flatten() {
        if !dir
            .file_name()
            .to_string_lossy()
            .starts_with("com_apple_MobileAsset_Font")
        {
            continue;
        }
        let Ok(assets) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for asset in assets.flatten() {
            let path = asset.path().join("AssetData").join(file_name);
            if path.is_file() {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    found
}

fn unix_han_paths(lang: &str) -> Vec<String> {
    let mut paths = Vec::new();
    // Japanese keeps Hiragino (the next face in `UNIX_HAN` once PingFang's old path is absent).
    // Every other UI language prefers the system Chinese font, which is metric-compatible with San Francisco.
    if cfg!(target_os = "macos") && lang != "ja" {
        paths.extend(asset_fonts("PingFang.ttc"));
    }
    paths.extend(UNIX_HAN.iter().copied().map(str::to_string));
    paths
}

struct LoadedFont {
    path: String,
    bytes: Vec<u8>,
    index: u32,
}

fn first_font(paths: impl IntoIterator<Item = String>) -> Option<(String, Vec<u8>)> {
    paths
        .into_iter()
        .find_map(|p| std::fs::read(&p).ok().map(|bytes| (p, bytes)))
}

fn first_face(paths: impl IntoIterator<Item = String>, prefs: &[&str]) -> Option<LoadedFont> {
    paths.into_iter().find_map(|path| {
        let bytes = std::fs::read(&path).ok()?;
        let index = face_index(&bytes, prefs);
        Some(LoadedFont { path, bytes, index })
    })
}

fn insert(
    fonts: &mut FontDefinitions,
    path: &str,
    bytes: Vec<u8>,
    index: u32,
    tweak: FontTweak,
) -> String {
    let name = if index == 0 {
        format!("sys:{path}")
    } else {
        format!("sys:{path}#{index}")
    };
    let mut data = FontData::from_owned(bytes).tweak(tweak);
    data.index = index;
    fonts.font_data.insert(name.clone(), Arc::new(data));
    name
}

fn load_first(fonts: &mut FontDefinitions, paths: &[&str]) -> Option<(String, Option<VMetrics>)> {
    let (path, bytes) = first_font(paths.iter().map(|s| s.to_string()))?;
    let metrics = vertical_metrics(&bytes, 0);
    Some((
        insert(fonts, &path, bytes, 0, FontTweak::default()),
        metrics,
    ))
}

fn fallback_tweak(primary: Option<VMetrics>, bytes: &[u8], index: u32) -> FontTweak {
    let Some(primary) = primary else {
        return FontTweak::default();
    };
    let Some(fallback) = vertical_metrics(bytes, index) else {
        return FontTweak::default();
    };
    FontTweak {
        y_offset_factor: baseline_offset_factor(primary, fallback),
        ..Default::default()
    }
}

/// The fallback fonts for scripts the UI and code fonts lack, in the order they are tried: a
/// Han/kana font (preferring the script variant of `lang`), a Hangul font and a symbol font,
/// each when installed.
fn fallbacks(lang: &str) -> Vec<LoadedFont> {
    let (win_han, fc_han): (Vec<&str>, Vec<&str>) = match lang {
        "zh-TW" => (
            [WIN_TW, WIN_CN, WIN_JA].concat(),
            vec!["zh-tw", "zh-cn", "ja"],
        ),
        "ja" => (
            [WIN_JA, WIN_TW, WIN_CN].concat(),
            vec!["ja", "zh-tw", "zh-cn"],
        ),
        _ => (
            [WIN_CN, WIN_TW, WIN_JA].concat(),
            vec!["zh-cn", "zh-tw", "ja"],
        ),
    };
    let han = first_face(
        system_fonts(win_han, unix_han_paths(lang), fc_han),
        han_face_prefs(lang),
    );
    let hangul = first_face(
        system_fonts(
            WIN_KO.to_vec(),
            UNIX_HANGUL.iter().copied().map(str::to_string).collect(),
            vec!["ko"],
        ),
        &[],
    )
    .filter(|f| han.as_ref().is_none_or(|h| h.path != f.path));
    let symbols = first_font(windows_fonts(&["seguisym.ttf"])).map(|(path, bytes)| LoadedFont {
        path,
        bytes,
        index: 0,
    });
    let ordered = if lang == "ko" {
        [hangul, han, symbols]
    } else {
        [han, hangul, symbols]
    };
    ordered.into_iter().flatten().collect()
}

/// Installs the UI, title and code fonts, then fallback fonts: a Han/kana font (preferring the
/// UI language's script variant), a Hangul font and a symbol font when available.
pub fn setup(ctx: &egui::Context, lang: &str) {
    let mut fonts = FontDefinitions::default();
    let proportional = load_first(&mut fonts, UI_FONTS);
    if let Some((name, _)) = &proportional {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name.clone());
    }
    if let Some((name, _)) = load_first(&mut fonts, MONO_FONTS) {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name);
    }
    let semibold = load_first(&mut fonts, UI_SEMIBOLD_FONTS).map(|(name, _)| name);
    // San Francisco and SF Mono share vertical metrics, so one shift fits both families.
    // Windows and Linux UI fonts already share a baseline with their CJK fallbacks.
    let primary_metrics = if cfg!(target_os = "macos") {
        proportional.and_then(|(_, metrics)| metrics)
    } else {
        None
    };

    for loaded in fallbacks(lang) {
        let tweak = fallback_tweak(primary_metrics, &loaded.bytes, loaded.index);
        let name = insert(&mut fonts, &loaded.path, loaded.bytes, loaded.index, tweak);
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push(name.clone());
        }
    }

    let mut family: Vec<String> = semibold.into_iter().collect();
    family.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(FontFamily::Name(crate::ui::theme::SEMIBOLD.into()), family);
    ctx.set_fonts(fonts);
}

/// Whether the font's licence lets a PDF embed it as a report does: a subset of its outlines,
/// in a document that can at least be viewed and printed. A font that doesn't say is left out.
fn embeddable(bytes: &[u8], index: u32) -> bool {
    use ttf_parser::Permissions::{Editable, Installable, PreviewAndPrint};
    ttf_parser::Face::parse(bytes, index).is_ok_and(|face| {
        matches!(
            face.permissions(),
            Some(Installable | Editable | PreviewAndPrint)
        ) && face.is_subsetting_allowed()
            && face.is_outline_embedding_allowed()
    })
}

/// The fonts a PDF report embeds: the code and UI fonts, then the fallbacks of [`fallbacks`] and
/// fonts for Hebrew, Arabic and Thai, each only if it allows embedding, and last the fonts built
/// into the program, so that every report has a font for Latin text even where none is
/// installed. Read once per language.
pub fn document_fonts(lang: &str) -> crate::core::pdf_report::Fonts {
    use crate::core::pdf_report::Fonts;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Fonts>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(fonts) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(lang) {
        return fonts.clone();
    }
    let fonts = load_document_fonts(lang);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(lang.to_string(), fonts.clone());
    fonts
}

fn load_document_fonts(lang: &str) -> crate::core::pdf_report::Fonts {
    use crate::core::pdf_report::FontFile;
    let file = |bytes: Vec<u8>, index: u32| FontFile {
        data: Arc::new(bytes),
        index,
    };
    let read = |path: &str| {
        let bytes = std::fs::read(path).ok()?;
        embeddable(&bytes, 0).then(|| file(bytes, 0))
    };
    let first = |paths: &[&str]| {
        paths
            .iter()
            .find_map(|&path| Some((path.to_string(), read(path)?)))
    };
    let mono = first(MONO_FONTS);
    let sans = first(UI_FONTS);
    let primaries: Vec<(String, FontFile)> = mono.iter().chain(&sans).cloned().collect();
    let mut seen = std::collections::HashSet::new();
    let mut rest: Vec<FontFile> = Vec::new();
    for f in fallbacks(lang) {
        if embeddable(&f.bytes, f.index) {
            let (bytes, index) = (f.bytes, f.index);
            add_first(vec![f.path], &primaries, &mut seen, &mut rest, |_| {
                Some(file(bytes.clone(), index))
            });
        }
    }
    for &(win, unix, fc) in DOCUMENT_SCRIPTS {
        let paths = system_fonts(
            win.to_vec(),
            unix.iter().copied().map(str::to_string).collect(),
            vec![fc],
        );
        add_first(paths, &primaries, &mut seen, &mut rest, read);
    }
    let built_in = FontDefinitions::default();
    let builtin = |names: &[&str]| -> Vec<FontFile> {
        names
            .iter()
            .filter_map(|name| {
                built_in.font_data.get(*name).map(|d| FontFile {
                    data: d.font.clone(),
                    index: d.index,
                })
            })
            .collect()
    };
    crate::core::pdf_report::Fonts {
        mono: chain(
            mono.map(|(_, f)| f),
            &rest,
            builtin(&["Hack", "Ubuntu-Light", "NotoEmoji-Regular"]),
        ),
        sans: chain(
            sans.map(|(_, f)| f),
            &rest,
            builtin(&["Ubuntu-Light", "NotoEmoji-Regular"]),
        ),
    }
}

/// Adds to `rest` the first of `paths` there is a font for, unless an earlier one of them is
/// there already. A path that is one of the `primaries` (the code or UI font) is added too,
/// sharing its bytes: the other primary may lack the script it was looked up for.
fn add_first(
    paths: impl IntoIterator<Item = String>,
    primaries: &[(String, crate::core::pdf_report::FontFile)],
    seen: &mut std::collections::HashSet<String>,
    rest: &mut Vec<crate::core::pdf_report::FontFile>,
    read: impl Fn(&str) -> Option<crate::core::pdf_report::FontFile>,
) {
    for path in paths {
        if seen.contains(&path) {
            return;
        }
        let shared = primaries.iter().find(|(p, _)| *p == path);
        if let Some(font) = shared.map(|(_, f)| f.clone()).or_else(|| read(&path)) {
            seen.insert(path);
            rest.push(font);
            return;
        }
    }
}

/// `primary`, then the fonts of `rest` other than `primary` itself, then `last`. Without a
/// `primary`, the first of `last` takes its place: a font found for another script would
/// otherwise draw the digits and spaces it also has, between letters in the built-in font.
fn chain(
    primary: Option<crate::core::pdf_report::FontFile>,
    rest: &[crate::core::pdf_report::FontFile],
    mut last: Vec<crate::core::pdf_report::FontFile>,
) -> Vec<crate::core::pdf_report::FontFile> {
    let primary = primary.or_else(|| (!last.is_empty()).then(|| last.remove(0)));
    let others = rest
        .iter()
        .filter(|f| {
            !primary
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(&p.data, &f.data) && p.index == f.index)
        })
        .cloned()
        .collect::<Vec<_>>();
    primary.into_iter().chain(others).chain(last).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `font` with the embedding flags of its OS/2 table set to `fs_type`.
    fn with_fs_type(font: &[u8], fs_type: u16) -> Vec<u8> {
        let mut out = font.to_vec();
        let tables = u16::from_be_bytes([font[4], font[5]]) as usize;
        for t in 0..tables {
            let rec = 12 + 16 * t;
            if &font[rec..rec + 4] == b"OS/2" {
                let at = u32::from_be_bytes(font[rec + 8..rec + 12].try_into().unwrap()) as usize;
                out[at + 8..at + 10].copy_from_slice(&fs_type.to_be_bytes());
                return out;
            }
        }
        panic!("no OS/2 table");
    }

    /// A font for Hebrew or Arabic that is also the UI font (Segoe UI on Windows) is still a
    /// fallback for the code font, which lacks those scripts, and appears once in each list.
    #[test]
    fn a_ui_font_that_covers_a_script_is_a_fallback_for_the_code_font() {
        use crate::core::pdf_report::FontFile;
        let font = |byte: u8| FontFile {
            data: Arc::new(vec![byte]),
            index: 0,
        };
        let bytes = |list: &[FontFile]| {
            list.iter()
                .map(|f| (*f.data).as_ref()[0])
                .collect::<Vec<_>>()
        };
        let (mono, sans) = (font(1), font(2));
        let primaries = vec![
            ("mono".to_string(), mono.clone()),
            ("ui".to_string(), sans.clone()),
        ];
        let mut seen = std::collections::HashSet::new();
        let mut rest = Vec::new();
        let read = |path: &str| (path == "thai").then(|| font(3));
        for paths in [
            vec!["ui".to_string()],
            vec!["missing".to_string(), "ui".to_string()],
            vec!["missing".to_string(), "thai".to_string()],
        ] {
            add_first(paths, &primaries, &mut seen, &mut rest, read);
        }
        assert_eq!(bytes(&rest), [2, 3]);
        assert_eq!(
            bytes(&chain(Some(mono), &rest, vec![font(9)])),
            [1, 2, 3, 9]
        );
        assert_eq!(bytes(&chain(Some(sans), &rest, vec![font(9)])), [2, 3, 9]);
        assert_eq!(bytes(&chain(None, &rest, Vec::new())), [2, 3]);
        // With no code font installed, the built-in one comes before the fonts for other
        // scripts.
        assert_eq!(
            bytes(&chain(None, &rest, vec![font(8), font(9)])),
            [8, 2, 3, 9]
        );
    }

    #[test]
    fn only_fonts_that_allow_embedding_a_subset_of_outlines_are_embedded() {
        let defs = FontDefinitions::default();
        let hack = (*defs.font_data["Hack"].font).as_ref().to_vec();
        for (fs_type, ok) in [
            (0x0000, true),  // installable
            (0x0004, true),  // preview and print
            (0x0008, true),  // editable
            (0x0002, false), // restricted
            (0x0100, false), // no subsetting
            (0x0200, false), // bitmaps only
            (0x0104, false),
        ] {
            assert_eq!(
                embeddable(&with_fs_type(&hack, fs_type), 0),
                ok,
                "fsType {fs_type:#06x}"
            );
        }
        assert!(!embeddable(b"not a font", 0));
    }

    #[test]
    fn report_fonts_are_read_once_and_not_twice() {
        let a = document_fonts("en");
        let b = document_fonts("en");
        assert!(a
            .sans
            .iter()
            .zip(&b.sans)
            .all(|(x, y)| Arc::ptr_eq(&x.data, &y.data)));
        let mut seen = std::collections::HashSet::new();
        for f in &a.sans {
            assert!(
                seen.insert((Arc::as_ptr(&f.data) as *const () as usize, f.index)),
                "a font is listed twice"
            );
            let same_bytes = a
                .sans
                .iter()
                .filter(|g| g.index == f.index && (*g.data).as_ref() == (*f.data).as_ref())
                .count();
            assert_eq!(same_bytes, 1, "a font file is embedded twice");
        }
    }

    #[test]
    fn baseline_offset_shares_primary_baseline() {
        // San Francisco (hhea 1980 / -432) vs Hiragino Sans GB (880 / -120, line gap 500).
        let primary = VMetrics {
            asc: 1980.0 / 2048.0,
            row: (1980.0 + 432.0) / 2048.0,
        };
        let hiragino = VMetrics {
            asc: 880.0 / 1000.0,
            row: (880.0 + 120.0 + 500.0) / 1000.0,
        };
        let factor = baseline_offset_factor(primary, hiragino);
        let y_fallback = hiragino.asc + factor + 0.5 * (primary.row - hiragino.row);
        assert!((primary.asc - y_fallback).abs() < 1e-5);
        assert!(
            factor > 0.2,
            "Hiragino has to move down onto the San Francisco baseline, factor={factor}"
        );
    }

    #[test]
    fn macos_chinese_uses_pingfang_matching_sf() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let Some(sf) = std::fs::read("/System/Library/Fonts/SFNS.ttf").ok() else {
            return;
        };
        let Some(primary) = vertical_metrics(&sf, 0) else {
            return;
        };
        let paths = unix_han_paths("zh-TW");
        let pingfang_installed = paths
            .iter()
            .any(|p| p.contains("PingFang") && std::path::Path::new(p).is_file());
        let Some(han) = first_face(paths, han_face_prefs("zh-TW")) else {
            return;
        };
        if !pingfang_installed {
            return;
        }
        assert!(
            han.path.contains("PingFang"),
            "expected PingFang, got {}",
            han.path
        );
        let face = ttf_parser::Face::parse(&han.bytes, han.index).expect("pingfang face");
        assert_eq!(
            postscript_name(&face).as_deref(),
            Some("PingFangTC-Regular")
        );
        let fallback = vertical_metrics(&han.bytes, han.index).expect("metrics");
        let factor = baseline_offset_factor(primary, fallback);
        assert!(
            factor.abs() < 0.05,
            "PingFang should already sit near the SF baseline, factor={factor}"
        );
    }
}
