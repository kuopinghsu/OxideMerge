//! Loads system fonts so that CJK and other scripts render (egui's built-in fonts are Latin/Cyrillic/Greek only).

use egui::{FontData, FontDefinitions, FontFamily};
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
const UNIX_HANGUL: &[&str] = &["/usr/share/fonts/truetype/nanum/NanumGothic.ttf", "/System/Library/Fonts/AppleSDGothicNeo.ttc"];

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
    let out = std::process::Command::new("fc-match").args(["-s", "-f", "%{file}\t%{lang}\n", &format!("sans-serif:lang={lang}")]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let (file, langs) = l.split_once('\t')?;
        langs.split('|').any(|x| x == lang).then(|| file.to_string())
    })
}

/// Native Windows fonts, then well-known Unix paths, then fontconfig (queried lazily), then the
/// Windows fonts of a WSL host as a last resort.
fn system_fonts(win: Vec<&'static str>, unix: &'static [&'static str], fc_langs: Vec<&'static str>) -> impl Iterator<Item = String> {
    let win = windows_fonts(&win);
    let (native_win, wsl_win) = if cfg!(windows) { (win, Vec::new()) } else { (Vec::new(), win) };
    native_win
        .into_iter()
        .chain(unix.iter().map(|s| s.to_string()))
        .chain(fc_langs.into_iter().filter_map(fontconfig))
        .chain(wsl_win)
}

fn first_font(paths: impl IntoIterator<Item = String>) -> Option<(String, Vec<u8>)> {
    paths.into_iter().find_map(|p| std::fs::read(&p).ok().map(|bytes| (p, bytes)))
}

fn insert(fonts: &mut FontDefinitions, path: &str, bytes: Vec<u8>) -> String {
    let name = format!("sys:{path}");
    fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
    name
}

fn load_first(fonts: &mut FontDefinitions, paths: &[&str]) -> Option<String> {
    let (path, bytes) = first_font(paths.iter().map(|s| s.to_string()))?;
    Some(insert(fonts, &path, bytes))
}

/// Installs the UI, title and code fonts, then fallback fonts: a Han/kana font (preferring the
/// UI language's script variant), a Hangul font and a symbol font when available.
pub fn setup(ctx: &egui::Context, lang: &str) {
    let mut fonts = FontDefinitions::default();
    if let Some(name) = load_first(&mut fonts, UI_FONTS) {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, name);
    }
    if let Some(name) = load_first(&mut fonts, MONO_FONTS) {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, name);
    }
    let semibold = load_first(&mut fonts, UI_SEMIBOLD_FONTS);

    let (win_han, fc_han): (Vec<&str>, Vec<&str>) = match lang {
        "zh-TW" => ([WIN_TW, WIN_CN, WIN_JA].concat(), vec!["zh-tw", "zh-cn", "ja"]),
        "ja" => ([WIN_JA, WIN_TW, WIN_CN].concat(), vec!["ja", "zh-tw", "zh-cn"]),
        _ => ([WIN_CN, WIN_TW, WIN_JA].concat(), vec!["zh-cn", "zh-tw", "ja"]),
    };
    let han = first_font(system_fonts(win_han, UNIX_HAN, fc_han));
    let hangul = first_font(system_fonts(WIN_KO.to_vec(), UNIX_HANGUL, vec!["ko"])).filter(|(p, _)| han.as_ref().is_none_or(|(h, _)| h != p));
    let symbols = first_font(windows_fonts(&["seguisym.ttf"]));
    let ordered = if lang == "ko" { [hangul, han, symbols] } else { [han, hangul, symbols] };
    for (path, bytes) in ordered.into_iter().flatten() {
        let name = insert(&mut fonts, &path, bytes);
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push(name.clone());
        }
    }

    let mut family: Vec<String> = semibold.into_iter().collect();
    family.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name(crate::ui::theme::SEMIBOLD.into()), family);
    ctx.set_fonts(fonts);
}
