//! Build-label policy shared by the build script and its regression tests.

/// Only a clean optimized build of the matching version tag gets a release date label.
pub fn identifier(date: &str, hash: Option<&str>, dirty: bool, release: bool) -> String {
    if release && !dirty {
        date.into()
    } else {
        format!(
            "{}{}",
            hash.unwrap_or("unknown"),
            if dirty { "-dirty" } else { "" }
        )
    }
}

/// Validate CI-provided SHAs before embedding them in compiler directives.
pub fn short_hash(value: &str) -> Option<String> {
    (value.len() >= 7 && value.len() <= 64 && value.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| value[..7].to_ascii_lowercase())
}
