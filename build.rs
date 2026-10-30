//! Stamps the build date, and embeds the application icon from `assets/oxidemerge.ico` and the
//! version details in Windows executables, so Explorer, the taskbar and the file properties show
//! them. The window icon on every platform comes from `assets/oxidemerge.png` at runtime.
//!
//! Tracks package files and Git metadata so committing, staging or editing source refreshes
//! the identifier even when the calendar date and application version stay the same.

mod build_info;

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    for key in ["SOURCE_DATE_EPOCH", "GITHUB_SHA"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    watch_git_inputs();
    let date = build_date();
    let hash = git(&["rev-parse", "--short=7", "HEAD"])
        .or_else(|| std::env::var("GITHUB_SHA").ok())
        .and_then(|value| build_info::short_hash(&value));
    let status = git(&[
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=no",
    ]);
    let dirty = status.as_ref().is_some_and(|status| !status.is_empty());
    let tag = format!(
        "refs/tags/v{}^{{commit}}",
        std::env::var("CARGO_PKG_VERSION").unwrap()
    );
    let head = git(&["rev-parse", "HEAD"]);
    let release = std::env::var("PROFILE").as_deref() == Ok("release")
        && status.as_deref() == Some("")
        && head.is_some()
        && git(&["rev-parse", "--verify", "--end-of-options", &tag]) == head;
    let id = build_info::identifier(&date, hash.as_deref(), dirty, release);
    println!("cargo:rustc-env=OXIDEMERGE_BUILD_DATE={date}");
    println!("cargo:rustc-env=OXIDEMERGE_BUILD_ID={id}");

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

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(std::env::var_os("CARGO_MANIFEST_DIR")?)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn watch_git_inputs() {
    // Keep Cargo's default package-file tracking when building a source archive without Git.
    let Some(files) = git(&["ls-files", "-z"]) else {
        return;
    };
    for path in files.split('\0').filter(|path| !path.is_empty()) {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in ["HEAD", "index", "refs", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            // Missing paths make Cargo rerun on every invocation. Creating packed refs
            // also changes the watched loose refs directory.
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
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
