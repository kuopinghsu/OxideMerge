//! The command line end to end: the built program compares, writes reports and patches, and
//! exits with 0 (identical), 1 (different) or 2 (error). Every run gets settings of its own, so
//! the options of whoever runs the tests do not change the results.

use std::path::Path;
use std::process::{Command, Output};

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn oxidemerge(dir: &Path, args: &[&str]) -> Run {
    oxidemerge_with_env(dir, args, &[])
}

fn oxidemerge_with_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let config = dir.join(".config");
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(env!("CARGO_BIN_EXE_oxidemerge"))
        .args(args)
        .current_dir(dir)
        .env("OXIDEMERGE_CONFIG_DIR", &config)
        .envs(env.iter().copied())
        .output()
        .expect("the program runs");
    Run {
        code: status.code().expect("the program exits by itself"),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

fn files(dir: &Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

fn read(p: impl AsRef<Path>) -> String {
    let p = p.as_ref();
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn text_compares_exit_with_whether_files_differ() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[
            ("a.txt", "one\ntwo\nthree\n"),
            ("same.txt", "one\ntwo\nthree\n"),
            ("b.txt", "one\n2\nthree\n"),
            ("spaced.txt", "one\ntwo  \nthree\n"),
        ],
    );
    let same = oxidemerge(d, &["--cli", "a.txt", "same.txt"]);
    assert_eq!(
        (same.code, same.stdout.as_str()),
        (0, ""),
        "{}",
        same.stderr
    );

    let diff = oxidemerge(d, &["--cli", "a.txt", "b.txt"]);
    assert_eq!(diff.code, 1, "{}", diff.stderr);
    assert!(diff.stdout.contains("@@ -1,3 +1,3 @@"), "{}", diff.stdout);
    assert!(diff.stdout.contains("\n-two\n+2\n"), "{}", diff.stdout);

    assert_eq!(oxidemerge(d, &["--cli", "a.txt", "spaced.txt"]).code, 1);
    let ignored = oxidemerge(d, &["--cli", "--ignore-ws", "all", "a.txt", "spaced.txt"]);
    assert_eq!(ignored.code, 0, "{}", ignored.stdout);
}

#[test]
fn errors_exit_with_2_and_name_the_file() {
    let d = tmp();
    let d = d.path();
    files(d, &[("a.txt", "one\n")]);
    let run = oxidemerge(d, &["--cli", "a.txt", "nowhere.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("nowhere.txt"), "{}", run.stderr);

    let run = oxidemerge(d, &["--cli", "a.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.starts_with("error:"), "{}", run.stderr);

    let run = oxidemerge(d, &["--cli", "--rules", "no such rules", "a.txt", "a.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("no such rules"), "{}", run.stderr);

    // Two or three sides at most, for files and folders alike.
    std::fs::create_dir_all(d.join("f")).unwrap();
    for args in [["a.txt"; 4], ["f"; 4]] {
        let mut all = vec!["--cli"];
        all.extend(args);
        let run = oxidemerge(d, &all);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(run.stderr.contains("4 were given"), "{}", run.stderr);
    }
}

#[test]
fn patches_are_written_in_each_style() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "one\n2\nthree\n")],
    );
    let styles = [
        ("unified", "\n-two\n+2\n"),
        ("context", "\n! two\n"),
        ("normal", "2c2\n< two\n---\n> 2\n"),
    ];
    for (style, expected) in styles {
        let out = format!("{style}.patch");
        let run = oxidemerge(
            d,
            &["--patch", &out, "--patch-style", style, "a.txt", "b.txt"],
        );
        assert_eq!(run.code, 1, "{style}: {}", run.stderr);
        let patch = read(d.join(&out));
        assert!(patch.contains(expected), "{style}:\n{patch}");
    }
    let run = oxidemerge(
        d,
        &["--patch", "zero.patch", "--context", "0", "a.txt", "b.txt"],
    );
    assert_eq!(run.code, 1);
    assert!(read(d.join("zero.patch")).contains("@@ -2 +2 @@\n-two\n+2\n"));
}

#[test]
fn outputs_that_cannot_be_written_leave_no_file() {
    let d = tmp();
    let d = d.path();
    files(d, &[("a.txt", "one\n"), ("b.txt", "two\n")]);
    for flag in ["--patch", "--report"] {
        let out = d.join("missing").join("out.html");
        let run = oxidemerge(d, &[flag, out.to_str().unwrap(), "a.txt", "b.txt"]);
        assert_eq!(run.code, 2, "{flag}");
        assert!(run.stderr.contains("out.html"), "{flag}: {}", run.stderr);
        assert!(!out.exists());
    }
}

fn folders(d: &Path) {
    files(
        d,
        &[
            ("left/same.txt", "same\n"),
            ("left/changed.txt", "left <b>\n"),
            ("left/only-left.txt", "x\n"),
            ("left/sub/deep.txt", "deep\n"),
            ("right/same.txt", "same\n"),
            ("right/changed.txt", "right\n"),
            ("right/sub/deep.txt", "deeper\n"),
        ],
    );
}

#[test]
fn folder_compares_list_and_report_the_differences() {
    let d = tmp();
    let d = d.path();
    folders(d);
    let run = oxidemerge(d, &["--cli", "-r", "left", "right"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    for name in ["changed.txt", "only-left.txt", "deep.txt"] {
        assert!(run.stdout.contains(name), "{name}:\n{}", run.stdout);
    }
    assert!(!run.stdout.contains("same.txt"), "{}", run.stdout);

    let same = oxidemerge(d, &["--cli", "-r", "left", "left"]);
    assert_eq!(same.code, 0, "{}", same.stdout);

    for (out, marker) in [
        ("report.html", "<html"),
        ("report.csv", "changed.txt,"),
        ("report.txt", "only-left.txt"),
    ] {
        let run = oxidemerge(d, &["--report", out, "-r", "left", "right"]);
        assert_eq!(run.code, 1, "{out}: {}", run.stderr);
        let text = read(d.join(out));
        assert!(text.contains(marker), "{out}:\n{text}");
    }

    let run = oxidemerge(
        d,
        &[
            "--report",
            "diffs.html",
            "--report-diffs",
            "-r",
            "left",
            "right",
        ],
    );
    assert_eq!(run.code, 1);
    let html = read(d.join("diffs.html"));
    assert!(html.contains("left &lt;b&gt;"), "file text is not escaped");
    assert!(!html.contains("left <b>"));
}

#[test]
fn reports_are_written_as_xml_and_slideshows() {
    let d = tmp();
    let d = d.path();
    folders(d);
    // Folder compares.
    let run = oxidemerge(d, &["--report", "f.xml", "-r", "left", "right"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let xml = read(d.join("f.xml"));
    assert!(
        xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
        "{xml}"
    );
    assert!(xml.contains("type=\"folder\""), "{xml}");
    assert!(xml.contains("path=\"only-left.txt\"") && xml.contains("status=\"unique\""));
    let run = oxidemerge(
        d,
        &["--report", "f.html", "--slideshow", "-r", "left", "right"],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let slides = read(d.join("f.html"));
    assert!(
        slides.contains("<section class=\"title\">"),
        "a title slide"
    );
    assert!(
        slides.contains("changed.txt"),
        "a slide for the changed file"
    );

    // Text compares.
    files(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\nTWO\n")]);
    let run = oxidemerge(d, &["--report", "t.xml", "a.txt", "b.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let xml = read(d.join("t.xml"));
    assert!(
        xml.contains("type=\"text\"") && xml.contains("kind=\"changed\""),
        "{xml}"
    );
    assert!(xml.contains("<line number=\"2\">TWO</line>"), "{xml}");
    let run = oxidemerge(d, &["--report", "t.html", "--slideshow", "a.txt", "b.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    // Title, one difference, end.
    assert_eq!(read(d.join("t.html")).matches("<section").count(), 3);

    // A slideshow is HTML: other report types say so and are written as asked.
    let run = oxidemerge(d, &["--report", "t2.xml", "--slideshow", "a.txt", "b.txt"]);
    assert!(
        run.stderr.contains("--slideshow only applies"),
        "{}",
        run.stderr
    );
    assert!(read(d.join("t2.xml")).contains("<comparison "));
}

#[cfg(unix)]
#[test]
fn file_names_in_reports_are_escaped() {
    let d = tmp();
    let d = d.path();
    let name = "<img src=x onerror=alert(1)>.txt";
    files(
        d,
        &[
            (&format!("left/{name}"), "a\n"),
            (&format!("right/{name}"), "b\n"),
        ],
    );
    for args in [
        &[
            "--report",
            "folder.html",
            "--report-diffs",
            "-r",
            "left",
            "right",
        ][..],
        &[
            "--report",
            "file.html",
            &format!("left/{name}"),
            &format!("right/{name}"),
        ],
    ] {
        let run = oxidemerge(d, args);
        assert_eq!(run.code, 1, "{}", run.stderr);
        let html = read(d.join(args[1]));
        assert!(
            html.contains("&lt;img src=x onerror=alert(1)&gt;"),
            "{}",
            args[1]
        );
        assert!(!html.contains("<img src=x"), "{}", args[1]);
    }
}

#[test]
fn folder_patches_cover_every_changed_text_file() {
    let d = tmp();
    let d = d.path();
    folders(d);
    let run = oxidemerge(d, &["--patch", "all.patch", "-r", "left", "right"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let patch = read(d.join("all.patch"));
    for part in [
        "changed.txt",
        "-left <b>",
        "+right",
        "sub/deep.txt",
        "only-left.txt",
    ] {
        assert!(patch.contains(part), "{part}:\n{patch}");
    }
}

#[test]
fn three_way_compares_list_their_blocks() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[
            ("left.txt", "a\nleft\nc\n"),
            ("base.txt", "a\nb\nc\n"),
            ("right.txt", "a\nright\nc\n"),
        ],
    );
    let run = oxidemerge(d, &["--cli", "left.txt", "base.txt", "right.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("Conflict: left 2-2, middle 2-2, right 2-2"),
        "{}",
        run.stdout
    );
}

#[test]
fn binary_files_report_the_ranges_that_differ() {
    let d = tmp();
    let d = d.path();
    std::fs::write(d.join("a.bin"), [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();
    std::fs::write(d.join("b.bin"), [0u8, 1, 9, 9, 4, 5, 6, 7]).unwrap();
    let run = oxidemerge(d, &["--cli", "a.bin", "b.bin"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("0x00000002-0x00000003 differ"),
        "{}",
        run.stdout
    );
    let same = oxidemerge(d, &["--cli", "a.bin", "a.bin"]);
    assert_eq!(same.code, 0);
}

#[test]
fn web_pages_compare_their_visible_text() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[
            (
                "a.html",
                "<html><head><title>T</title><style>p{}</style></head><body><p>Hello</p><p>world</p></body></html>",
            ),
            (
                "b.html",
                "<html><head><title>T</title><style>q{}</style></head><body><p>Hello</p><p>there</p></body></html>",
            ),
        ],
    );
    // The pages as downloaded, whether or not a browser is installed.
    std::fs::create_dir_all(d.join(".config")).unwrap();
    std::fs::write(d.join(".config/config.json"), r#"{"web_render": false}"#).unwrap();
    let run = oxidemerge(d, &["--cli", "--compare-as", "web", "a.html", "b.html"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("-world") && run.stdout.contains("+there"));
    assert!(!run.stdout.contains("p{}"), "styles are not visible text");
}

/// With a browser (`OXIDEMERGE_TEST_BROWSER`, see `web_render`), the console compare sees what
/// the app's text mode shows: the pages after their scripts have run.
#[test]
fn web_pages_compare_as_rendered_with_a_browser() {
    let Some(browser) = std::env::var("OXIDEMERGE_TEST_BROWSER")
        .ok()
        .filter(|b| !b.is_empty())
    else {
        return;
    };
    let d = tmp();
    let d = d.path();
    let page = |word: &str| {
        format!(
            "<html><body><p>Hello</p><script>document.body.insertAdjacentHTML(\
             'beforeend', '<p>{word} from a script</p>')</script></body></html>"
        )
    };
    let (a, b) = (page("one"), page("two"));
    files(d, &[("a.html", a.as_str()), ("b.html", b.as_str())]);
    if browser != "auto" {
        let config = serde_json::json!({ "web_browser": browser }).to_string();
        std::fs::create_dir_all(d.join(".config")).unwrap();
        std::fs::write(d.join(".config/config.json"), config).unwrap();
    }
    let run = oxidemerge(d, &["--cli", "--compare-as", "web", "a.html", "b.html"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-one from a script") && run.stdout.contains("+two from a script"),
        "{}",
        run.stdout
    );
}

#[test]
fn messages_follow_the_language_chosen() {
    let d = tmp();
    let d = d.path();
    std::fs::write(d.join("a.bin"), [0u8, 1, 2]).unwrap();
    let english = oxidemerge(d, &["--cli", "--lang", "en", "a.bin", "a.bin"]);
    let german = oxidemerge(d, &["--cli", "--lang", "de", "a.bin", "a.bin"]);
    assert_eq!((english.code, german.code), (0, 0));
    assert!(!english.stdout.trim().is_empty());
    assert_ne!(english.stdout, german.stdout);

    let list = oxidemerge(d, &["--list-languages"]);
    assert_eq!(list.code, 0);
    for code in ["en", "zh-TW", "de"] {
        assert!(
            list.stdout.lines().any(|l| l.starts_with(code)),
            "{}",
            list.stdout
        );
    }
}

#[test]
fn files_compare_with_their_git_revision() {
    let d = tmp();
    let d = d.path();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(d)
            .env("GIT_CONFIG_GLOBAL", d.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
    };
    if git(&["init", "-q"]).is_err() {
        eprintln!("git is not installed; skipped");
        return;
    }
    files(d, &[("f.txt", "old\n")]);
    for args in [
        &["add", "f.txt"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "first",
        ],
    ] {
        assert!(git(args).unwrap().status.success());
    }
    files(d, &[("f.txt", "new\n")]);
    // Git revisions use checkout filters. Windows runners normally enable autocrlf,
    // so assert the actual bytes in both modes instead of assuming LF on every OS.
    // Set configuration only on the child process: tests run concurrently.
    let gitconfig = d.join(".gitconfig");
    let gitconfig = gitconfig.to_str().unwrap();
    for (autocrlf, eol) in [("false", "\n"), ("true", "\r\n")] {
        let run = oxidemerge_with_env(
            d,
            &["--cli", "--git-rev", "HEAD", "f.txt"],
            &[
                ("GIT_CONFIG_GLOBAL", gitconfig),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "core.autocrlf"),
                ("GIT_CONFIG_VALUE_0", autocrlf),
            ],
        );
        assert_eq!(run.code, 1, "autocrlf={autocrlf}: {}", run.stderr);
        assert!(
            run.stdout.contains(&format!("-old{eol}+new{eol}")),
            "autocrlf={autocrlf}: {:?}",
            run.stdout
        );
    }
}

#[test]
fn text_file_format_settings_affect_cli_exit_status() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[("a.txt", "one\r\n"), ("b.txt", "one\n"), ("end.txt", "one")],
    );
    assert_eq!(oxidemerge(d, &["--cli", "a.txt", "b.txt"]).code, 0);
    std::fs::create_dir_all(d.join(".config")).unwrap();
    std::fs::write(
        d.join(".config/config.json"),
        r#"{"diff":{"ignore_eol":false,"ignore_codepage":false}}"#,
    )
    .unwrap();
    assert_eq!(oxidemerge(d, &["--cli", "a.txt", "b.txt"]).code, 1);
    assert_eq!(oxidemerge(d, &["--cli", "b.txt", "end.txt"]).code, 1);
    std::fs::write(
        d.join("utf16.txt"),
        [0xff, 0xfe, b'o', 0, b'n', 0, b'e', 0, 10, 0],
    )
    .unwrap();
    assert_eq!(oxidemerge(d, &["--cli", "b.txt", "utf16.txt"]).code, 1);
}

#[cfg(unix)]
#[test]
fn generated_patches_apply_with_the_system_patch_utility() {
    for (old, new) in [
        ("old", "new"),
        ("old\n", "new"),
        ("old", "new\n"),
        ("one\n", "one"),
        ("one", "one\n"),
        ("head\nold", "HEAD\nold"),
        ("foo\nbar\n", "foo"),
        ("foo", "foo\nbar\n"),
    ] {
        let dir = tmp();
        let d = dir.path();
        files(d, &[("a.txt", old), ("b.txt", new), ("target.txt", old)]);
        for style in ["unified", "context", "normal"] {
            std::fs::write(d.join("target.txt"), old).unwrap();
            let run = oxidemerge(
                d,
                &[
                    "--cli",
                    "--patch-style",
                    style,
                    "--patch",
                    "fix.patch",
                    "a.txt",
                    "b.txt",
                ],
            );
            assert_eq!(run.code, 1, "{}", run.stderr);
            let mut native = Command::new("diff");
            native.current_dir(d);
            match style {
                "unified" => {
                    native.arg("-u");
                }
                "context" => {
                    native.arg("-c");
                }
                _ => {}
            }
            let native = native.args(["a.txt", "b.txt"]).output().unwrap();
            assert_eq!(native.status.code(), Some(1));
            let generated = read(d.join("fix.patch"));
            let native = String::from_utf8(native.stdout).unwrap();
            let skip = if style == "normal" { 0 } else { 2 };
            assert_eq!(
                generated.lines().skip(skip).collect::<Vec<_>>(),
                native.lines().skip(skip).collect::<Vec<_>>(),
                "{style}"
            );
            // Apple's legacy patch does not support EOF markers in normal/context patches,
            // including patches produced by its own diff. Unified patches are portable.
            if style != "unified" {
                continue;
            }
            let applied = Command::new("patch")
                .current_dir(d)
                .args(["target.txt", "fix.patch"])
                .output()
                .unwrap();
            assert!(
                applied.status.success(),
                "{old:?} -> {new:?} ({style}): {} {}",
                String::from_utf8_lossy(&applied.stdout),
                String::from_utf8_lossy(&applied.stderr)
            );
            assert_eq!(
                std::fs::read(d.join("target.txt")).unwrap(),
                new.as_bytes(),
                "{style}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn format_patches_apply_as_bytes_and_unsupported_encodings_leave_no_output() {
    let dir = tmp();
    let d = dir.path();
    std::fs::create_dir(d.join(".config")).unwrap();
    std::fs::write(
        d.join(".config/config.json"),
        r#"{"diff":{"ignore_eol":false,"ignore_codepage":false}}"#,
    )
    .unwrap();
    for (a, b) in [
        ("one\r\n", "one\n"),
        ("", "\u{feff}"),
        ("\u{feff}old\r\n", "new\n"),
        ("one\r\ntwo\n", "one\ntwo\r\n"),
    ] {
        files(d, &[("a.txt", a), ("b.txt", b), ("target.txt", a)]);
        let run = oxidemerge(d, &["--cli", "--patch", "fix.patch", "a.txt", "b.txt"]);
        assert_eq!(run.code, 1, "{}", run.stderr);
        let applied = Command::new("patch")
            .current_dir(d)
            .args(["target.txt", "fix.patch"])
            .output()
            .unwrap();
        assert!(
            applied.status.success(),
            "{} {}",
            String::from_utf8_lossy(&applied.stdout),
            String::from_utf8_lossy(&applied.stderr)
        );
        assert_eq!(std::fs::read(d.join("target.txt")).unwrap(), b.as_bytes());
    }
    std::fs::write(
        d.join("a.txt"),
        [0xff, 0xfe, b'o', 0, b'l', 0, b'd', 0, 10, 0],
    )
    .unwrap();
    files(d, &[("b.txt", "new\n")]);
    let run = oxidemerge(
        d,
        &[
            "--cli",
            "-t",
            "text",
            "--patch",
            "unsupported.patch",
            "a.txt",
            "b.txt",
        ],
    );
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("UTF-8"), "{}", run.stderr);
    assert!(!d.join("unsupported.patch").exists());
    let run = oxidemerge(d, &["--cli", "-t", "text", "a.txt", "b.txt"]);
    assert_eq!(run.code, 1);
}

#[test]
fn bom_files_keep_anchored_ignore_rules_during_patch_export() {
    let dir = tmp();
    let d = dir.path();
    std::fs::create_dir(d.join(".config")).unwrap();
    std::fs::write(
        d.join(".config/config.json"),
        r#"{"diff":{"line_filters_enabled":true,"line_filters":["^ignored"]}}"#,
    )
    .unwrap();
    let gap = (0..20).map(|i| format!("same {i}\n")).collect::<String>();
    let a = format!("\u{feff}ignored old\n{gap}old\n");
    let b = format!("\u{feff}ignored new\n{gap}new\n");
    files(d, &[("a.txt", &a), ("b.txt", &b)]);
    let run = oxidemerge(d, &["--cli", "--patch", "fix.patch", "a.txt", "b.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let patch = read(d.join("fix.patch"));
    assert_eq!(patch.matches("@@ -").count(), 1, "{patch}");
    assert!(!patch.contains("ignored"), "{patch}");
}
