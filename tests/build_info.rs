#[path = "../build_info.rs"]
mod build_info;

#[test]
fn release_dates_and_development_revisions_are_distinct() {
    assert_eq!(
        build_info::identifier("2026.10.30", Some("abc1234"), false, true),
        "2026.10.30"
    );
    assert_eq!(
        build_info::identifier("2026.10.30", Some("abc1234"), false, false),
        "abc1234"
    );
    assert_eq!(
        build_info::identifier("2026.10.30", Some("abc1234"), true, false),
        "abc1234-dirty"
    );
    assert_eq!(
        build_info::identifier("2026.10.30", Some("abc1234"), true, true),
        "abc1234-dirty"
    );
    assert_eq!(
        build_info::identifier("2026.10.30", None, false, false),
        "unknown"
    );
}

#[test]
fn ci_hashes_are_validated_before_embedding() {
    assert_eq!(
        build_info::short_hash("ABC1234567890"),
        Some("abc1234".into())
    );
    for value in [
        "",
        "abc12",
        "abc1234\ncargo:rustc-env=BAD=yes",
        "not-a-hash",
    ] {
        assert!(build_info::short_hash(value).is_none());
    }
}
