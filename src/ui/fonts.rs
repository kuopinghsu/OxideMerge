//! Loads system fonts so that CJK and other scripts render (egui's built-in fonts are Latin/Cyrillic/Greek only).

use egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

fn candidates(lang: &str) -> Vec<&'static str> {
    let win = |f: &'static str| f;
    let mut v: Vec<&'static str> = Vec::new();
    let tw = ["C:\\Windows\\Fonts\\msjh.ttc", "C:\\Windows\\Fonts\\mingliub.ttc"];
    let cn = ["C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\simsun.ttc"];
    let ja = ["C:\\Windows\\Fonts\\YuGothM.ttc", "C:\\Windows\\Fonts\\meiryo.ttc", "C:\\Windows\\Fonts\\msgothic.ttc"];
    let ko = ["C:\\Windows\\Fonts\\malgun.ttf", "C:\\Windows\\Fonts\\gulim.ttc"];
    match lang {
        "zh-TW" => v.extend(tw),
        "ja" => v.extend(ja),
        "ko" => v.extend(ko),
        _ => v.extend(cn),
    }
    v.extend(tw);
    v.extend(cn);
    v.extend(ja);
    v.push(win("C:\\Windows\\Fonts\\seguisym.ttf"));
    v.extend([
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
    ]);
    v
}

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

fn load_first(fonts: &mut FontDefinitions, paths: &[&str]) -> Option<String> {
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("sys:{path}");
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
            return Some(name);
        }
    }
    None
}

/// Installs the UI, title and code fonts, then fallback fonts: one CJK font (preferring the
/// UI language's script variant) plus a Hangul font when available.
pub fn setup(ctx: &egui::Context, lang: &str) {
    let mut fonts = FontDefinitions::default();
    if let Some(name) = load_first(&mut fonts, UI_FONTS) {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, name);
    }
    if let Some(name) = load_first(&mut fonts, MONO_FONTS) {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, name);
    }
    let semibold = load_first(&mut fonts, UI_SEMIBOLD_FONTS);
    let mut loaded: Vec<String> = Vec::new();
    let mut have_cjk = false;
    for path in candidates(lang) {
        let is_ko = path.contains("malgun") || path.contains("gulim");
        let is_sym = path.contains("seguisym");
        if have_cjk && !is_ko && !is_sym {
            continue;
        }
        if loaded.iter().any(|l| l == path) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("sys:{path}");
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
            for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.clone());
            }
            loaded.push(path.to_string());
            if !is_ko && !is_sym {
                have_cjk = true;
            }
        }
    }
    if !have_cjk || lang == "ko" {
        for path in ["C:\\Windows\\Fonts\\malgun.ttf", "/usr/share/fonts/truetype/nanum/NanumGothic.ttf", "/System/Library/Fonts/AppleSDGothicNeo.ttc"] {
            if loaded.iter().any(|l| l == path) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(path) {
                let name = format!("sys:{path}");
                fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
                for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                    fonts.families.entry(fam).or_default().push(name.clone());
                }
                break;
            }
        }
    }
    let mut family: Vec<String> = semibold.into_iter().collect();
    family.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name(crate::ui::theme::SEMIBOLD.into()), family);
    ctx.set_fonts(fonts);
}
