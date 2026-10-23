//! Stamps the build date, and embeds the application icon from `assets/oxidemerge.ico` and the
//! version details in Windows executables, so Explorer, the taskbar and the file properties show
//! them. The window icon on every platform comes from `assets/oxidemerge.png` at runtime.
//!
//! No `rerun-if-changed` lines are printed: Cargo then reruns this script whenever a file in the
//! package changes, which keeps the build date current.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rustc-env=OXIDEMERGE_BUILD_DATE={}", build_date());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/oxidemerge.ico");
    resource.set("ProductName", "OxideMerge");
    resource.set("FileDescription", "OxideMerge");
    resource.set("OriginalFilename", "oxidemerge.exe");
    resource.set("LegalCopyright", "MIT License");
    // Cross-checking for Windows from another system has no resource compiler; the program still
    // builds, just without the icon.
    if let Err(err) = resource.compile() {
        println!("cargo:warning=The Windows icon was not embedded: {err}");
    }
}

/// UTC date as `YYYY.MM.DD`, from `SOURCE_DATE_EPOCH` for reproducible builds.
fn build_date() -> String {
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });
    let (year, month, day) = civil_date(seconds / 86_400);
    format!("{year}.{month:02}.{day:02}")
}

/// Days since 1970-01-01 to a Gregorian date (Howard Hinnant's algorithm).
fn civil_date(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
