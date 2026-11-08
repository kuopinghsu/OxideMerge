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
fn structured_files_compare_by_structure_in_folders_and_three_ways() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[
            ("left/conf.json", "{\"a\": 1, \"b\": 2}"),
            ("right/conf.json", "{\n  \"b\": 2,\n  \"a\": 1\n}\n"),
            ("l.yaml", "port: 8080\nmode: fast\n"),
            ("base.yaml", "port: 80\nmode: slow\n"),
            ("r.yaml", "port: 80\nmode: safe\n"),
            ("broken.yaml", "port: [80\n"),
        ],
    );
    assert_eq!(oxidemerge(d, &["--cli", "-r", "left", "right"]).code, 1);
    let run = oxidemerge(d, &["--cli", "-r", "--structured", "left", "right"]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);

    let run = oxidemerge(d, &["--semantic", "l.yaml", "base.yaml", "r.yaml"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let report: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let states: Vec<_> = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["path"].as_str().unwrap(), e["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        states,
        [("$[\"mode\"]", "conflict"), ("$[\"port\"]", "left")]
    );
    let same = oxidemerge(d, &["--semantic", "base.yaml", "base.yaml", "base.yaml"]);
    assert_eq!(same.code, 0, "{}", same.stderr);
    let bad = oxidemerge(d, &["--semantic", "l.yaml", "base.yaml", "broken.yaml"]);
    assert_eq!(bad.code, 2, "{}", bad.stdout);
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
    let run = oxidemerge(d, &["--cli", "--compare-as", "web", "a.html", "b.html"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("-world") && run.stdout.contains("+there"));
    assert!(!run.stdout.contains("p{}"), "styles are not visible text");
}

/// The text mode compares the pages as downloaded, without what they hide.
#[test]
fn web_pages_compare_as_text_without_a_browser() {
    let d = tmp();
    let d = d.path();
    let page = |word: &str| {
        format!(
            "<html><head><title>{word}</title></head><body><p>Hello</p><p>{word}</p>\
             <p hidden>hidden {word}</p><span style='display:none'>none {word}</span></body></html>"
        )
    };
    let (a, b) = (page("one"), page("two"));
    files(d, &[("a.html", a.as_str()), ("b.html", b.as_str())]);
    std::fs::create_dir_all(d.join(".config")).unwrap();
    std::fs::write(d.join(".config/config.json"), r#"{"web_mode": "Text"}"#).unwrap();
    let run = oxidemerge(d, &["--cli", "--compare-as", "web", "a.html", "b.html"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-one") && run.stdout.contains("+two"),
        "{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("hidden") && !run.stdout.contains("none"),
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
fn format_regression_git_revision_checkout_modes() {
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

#[test]
fn format_regression_cli_patches_apply_with_git() {
    let dir = tmp();
    let d = dir.path();
    std::fs::create_dir(d.join(".config")).unwrap();
    std::fs::write(
        d.join(".config/config.json"),
        r#"{"diff":{"ignore_eol":false,"ignore_codepage":false}}"#,
    )
    .unwrap();
    for (old, new) in [
        ("\u{feff}a\nb\n", "\u{feff}x\na\nb\n"),
        ("\u{feff}x\na\nb\n", "\u{feff}a\nb\n"),
        ("\u{feff}a\r\nb", "\u{feff}x\r\na\r\nb"),
        ("", "\u{feff}"),
        ("\u{feff}", ""),
        ("foo\nbar\n", "foo"),
        ("one\r\ntwo\n", "ONE\ntwo\r\n"),
    ] {
        files(d, &[("a.txt", old), ("b.txt", new), ("target.txt", old)]);
        let run = oxidemerge(
            d,
            &[
                "--cli",
                "--dl",
                "a/target.txt",
                "--dr",
                "b/target.txt",
                "--patch",
                "fix.patch",
                "a.txt",
                "b.txt",
            ],
        );
        assert_eq!(
            run.code, 1,
            "old={old:?}, target={new:?}, stderr={:?}",
            run.stderr
        );
        let applied = Command::new("git")
            .current_dir(d)
            .args([
                "-c",
                "core.autocrlf=false",
                "apply",
                "--whitespace=nowarn",
                "fix.patch",
            ])
            .env("GIT_CONFIG_GLOBAL", d.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git is required for format regressions");
        assert!(
            applied.status.success(),
            "old={old:?}, target={new:?}, patch={:?}, stderr={:?}",
            read(d.join("fix.patch")),
            String::from_utf8_lossy(&applied.stderr)
        );
        assert_eq!(
            read(d.join("target.txt")),
            new,
            "old={old:?}, target={new:?}, patch={:?}",
            read(d.join("fix.patch"))
        );
    }
}

#[cfg(unix)]
#[test]
fn converters_agree_for_files_folder_reports_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(
        d,
        &[
            ("left/a.docx", "\0metadata-one\nvisible text\n"),
            ("right/a.docx", "\0metadata-two\nvisible text\n"),
        ],
    );
    std::fs::create_dir_all(d.join(".config")).unwrap();
    let config = |command: &str| {
        serde_json::json!({"unpackers":[{"enabled":true,"pattern":"*.docx","command":command}]})
            .to_string()
    };
    std::fs::write(d.join(".config/config.json"), config("tail -n +2 ${file}")).unwrap();
    let file = oxidemerge(d, &["--cli", "left/a.docx", "right/a.docx"]);
    assert_eq!(file.code, 0, "{}", file.stderr);
    let folder = oxidemerge(d, &["--cli", "-r", "left", "right"]);
    assert_eq!(folder.code, 0, "{}", folder.stderr);
    std::fs::write(d.join("right/a.docx"), "\0metadata-three\nchanged text\n").unwrap();
    let report = oxidemerge(
        d,
        &[
            "-r",
            "left",
            "right",
            "--report",
            "review.html",
            "--report-diffs",
        ],
    );
    assert_eq!(report.code, 1, "{}", report.stderr);
    let html = read(d.join("review.html"));
    assert!(html.contains("visible") && html.contains("changed"));
    assert!(!html.contains("metadata-three"));
    std::fs::write(
        d.join(".config/config.json"),
        config("printf partial; exit 9 # ${file}"),
    )
    .unwrap();
    assert_eq!(
        oxidemerge(d, &["--cli", "left/a.docx", "right/a.docx"]).code,
        2
    );
    assert_eq!(oxidemerge(d, &["--cli", "-r", "left", "right"]).code, 2);
}

#[test]
fn semantic_merge_validation_and_release_analysis_are_opt_in_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(
        d,
        &[
            ("a.json", r#"{"b":2,"a":1}"#),
            ("b.json", r#"{"a":1,"b":2}"#),
            ("a.yaml", "db:\n  port: 5432\n  host: x\n"),
            ("b.yml", "db:\n  host: x\n  port: 6543\n"),
            ("alias.yaml", "base: &b {port: 1}\ndb: *b\n"),
            ("a.toml", "[db]\nport = 5_432\n"),
            ("b.toml", "# same value\n[db]\nport = 5432\n"),
            ("list1.json", r#"[{"id":1,"v":1},{"id":2,"v":2}]"#),
            (
                "list2.json",
                r#"[{"id":3,"v":3},{"id":2,"v":2},{"id":1,"v":1}]"#,
            ),
            ("left", "a\nLEFT\nc\nd\ne\nf\n"),
            ("base", "a\nb\nc\nd\ne\nf\n"),
            ("right", "a\nb\nc\nd\ne\nRIGHT\n"),
            ("result", "a\nLEFT\nc\nd\ne\nRIGHT\n"),
            ("close", "a\nb\nc\nd\nCLOSE\nf\n"),
            ("old/old.txt", "same\n"),
            ("new/new.txt", "same\n"),
            ("old/app.ini", "k1=1\nk2=2\nk3=3\nk4=4\n"),
            ("new/main.ini", "k1=1\nk2=2\nk3=3\nk4=40\n"),
        ],
    );
    let r = oxidemerge(d, &["--semantic", "a.json", "b.json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("policies"));
    assert_eq!(oxidemerge(d, &["--cli", "a.json", "b.json"]).code, 1);
    let keyed = oxidemerge(
        d,
        &[
            "--semantic",
            "--array-key",
            "id",
            "list1.json",
            "list2.json",
        ],
    );
    assert_eq!(keyed.code, 1, "{}", keyed.stderr);
    assert!(keyed.stdout.contains("\"kind\": \"moved\""));
    assert!(keyed.stdout.contains("\"from\": \"$[0]\""));
    assert!(keyed.stdout.contains("\"array_key\": \"id\""));
    assert_eq!(
        oxidemerge(d, &["--semantic", "--format", "ini", "a.json", "b.json"]).code,
        2
    );
    assert_eq!(
        oxidemerge(d, &["--semantic", "--format", "yaml", "a.json", "b.json"]).code,
        0
    );
    let yaml = oxidemerge(d, &["--semantic", "a.yaml", "b.yml"]);
    assert_eq!(yaml.code, 1, "{}", yaml.stderr);
    assert!(yaml.stdout.contains("\"format\": \"yaml\""));
    assert!(yaml.stdout.contains(r#"$[\"db\"][\"port\"]"#));
    let toml = oxidemerge(d, &["--semantic", "a.toml", "b.toml"]);
    assert_eq!(toml.code, 0, "{}", toml.stderr);
    assert_eq!(
        oxidemerge(d, &["--semantic", "a.yaml", "alias.yaml"]).code,
        2
    );
    let plan = oxidemerge(d, &["--merge-plan", "left", "base", "right"]);
    assert_eq!(plan.code, 0, "{}", plan.stderr);
    assert!(plan.stdout.contains("LEFT"));
    assert_eq!(read(d.join("base")), "a\nb\nc\nd\ne\nf\n");
    let close = oxidemerge(d, &["--merge-plan", "left", "base", "close"]);
    assert_eq!(close.code, 1, "{}", close.stderr);
    assert!(close.stdout.contains("\"nearby_edits\""));
    assert!(close.stdout.contains("\"mechanical\": false"));
    let validated = oxidemerge(d, &["--validate-merge", "result", "left", "base", "right"]);
    assert_eq!(validated.code, 0, "{}", validated.stderr);
    std::fs::write(d.join("result"), "CORRUPT\n").unwrap();
    assert_eq!(
        oxidemerge(d, &["--validate-merge", "result", "left", "base", "right"]).code,
        3
    );
    let release = oxidemerge(
        d,
        &[
            "--baseline-left",
            "folder:old",
            "--baseline-right",
            "folder:new",
            "--release-report",
            "release.json",
        ],
    );
    assert_eq!(release.code, 1, "{}", release.stderr);
    let json = read(d.join("release.json"));
    assert!(json.contains("\"moved\""));
    assert!(json.contains("\"renamed_modified\""));
    assert!(json.contains("\"similarity\": 75"));
    assert!(json.contains("\"captured_at\""));
    let split = oxidemerge(
        d,
        &[
            "--baseline-left",
            "folder:old",
            "--baseline-right",
            "folder:new",
            "--split-rename",
            "./main.ini",
        ],
    );
    assert_eq!(split.code, 1, "{}", split.stderr);
    assert!(!split.stdout.contains("\"renamed_modified\""));
    assert!(split.stdout.contains("\"split_renames\""));
    assert!(split.stderr.is_empty(), "{}", split.stderr);
    let unmatched = oxidemerge(
        d,
        &[
            "--baseline-left",
            "folder:old",
            "--baseline-right",
            "folder:new",
            "--split-rename",
            "nowhere.ini",
        ],
    );
    assert_eq!(unmatched.code, 1, "{}", unmatched.stderr);
    assert!(
        unmatched.stderr.contains("nowhere.ini: not an added path"),
        "{}",
        unmatched.stderr
    );
    let alone = oxidemerge(d, &["--split-rename", "main.ini", "old", "new"]);
    assert_eq!(alone.code, 2, "{}", alone.stderr);
    assert!(alone.stderr.contains("--split-rename"), "{}", alone.stderr);
    let scoped = oxidemerge(d, &["--impact", "--filter", "*.rs", "old", "new"]);
    assert_eq!(scoped.code, 0, "{}", scoped.stderr);
    assert!(scoped.stdout.contains("File excluded by scope"));
    assert_eq!(
        oxidemerge(
            d,
            &[
                "--impact",
                "--filter",
                "[F] missing-analysis-filter",
                "old",
                "new"
            ]
        )
        .code,
        2
    );
    let bad = oxidemerge(d, &["--semantic", "--report", "a.json", "a.json", "b.json"]);
    assert_eq!(bad.code, 2);
    assert_eq!(read(d.join("a.json")), r#"{"b":2,"a":1}"#);
}

#[test]
fn semantic_ambiguities_report_errors_instead_of_identical() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(
        d,
        &[
            ("a.json", r#"{"x":1,"x":2}"#),
            ("b.json", r#"{"x":2}"#),
            (
                "a.xml",
                "<!DOCTYPE a [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><a>&x;</a>",
            ),
            ("b.xml", "<a/>"),
        ],
    );
    for (a, b) in [("a.json", "b.json"), ("a.xml", "b.xml")] {
        let r = oxidemerge(d, &["--semantic", a, b]);
        assert_eq!(r.code, 2);
        assert!(r.stdout.contains("Text view"));
    }
}

#[test]
fn review_summary_accepts_version_one_archives_without_original_sources() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let f = std::fs::File::create(d.join("legacy.oxmreview")).unwrap();
    let mut writer = zip::ZipWriter::new(f);
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("manifest.json", options).unwrap();
    writer.write_all(br#"{"version":1,"saved":"baseline","labels":["left","right"],"options":{},"folder":{},"filter":"*","notes":{},"entries":[{"side":0,"path":"a.txt","directory":false,"modified":null,"converted":false},{"side":1,"path":"a.txt","directory":false,"modified":null,"converted":false}]}"#).unwrap();
    for (path, content) in [("data/0/a.txt", b"old"), ("data/1/a.txt", b"new")] {
        writer.start_file(path, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap();
    let r = oxidemerge(d, &["--review-summary", "legacy.oxmreview"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let summary: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["unreviewed"], 1);
    assert_eq!(
        oxidemerge(
            d,
            &[
                "--review-summary",
                "legacy.oxmreview",
                "--report",
                "legacy.oxmreview"
            ]
        )
        .code,
        2
    );
}

#[test]
fn release_git_sources_record_immutable_commits_and_leave_the_working_tree_alone() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(d)
            .env("GIT_CONFIG_GLOBAL", d.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    files(d, &[("file.txt", "first\n")]);
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "first"
    ])
    .status
    .success());
    let first = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    files(d, &[("file.txt", "second\n")]);
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "second"
    ])
    .status
    .success());
    let second = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    files(d, &[("file.txt", "uncommitted\n")]);
    let report_dir = tempfile::tempdir().unwrap();
    let report_path = report_dir.path().join("release.json");
    let r = oxidemerge(
        d,
        &[
            "--baseline-left",
            "git:HEAD~1:.",
            "--baseline-right",
            "git:HEAD:.",
            "--release-report",
            report_path.to_str().unwrap(),
        ],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    let report: serde_json::Value = serde_json::from_str(&read(&report_path)).unwrap();
    assert_eq!(report["left"]["resolved_commit"], first);
    assert_eq!(report["right"]["resolved_commit"], second);
    assert_eq!(read(d.join("file.txt")), "uncommitted\n");
    assert!(git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-qm",
        "advance"
    ])
    .status
    .success());
    let saved: serde_json::Value = serde_json::from_str(&read(&report_path)).unwrap();
    assert_eq!(saved, report);
}

#[test]
fn folder_merge_validation_checks_written_results_without_merging_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(
        d,
        &[
            ("left/a.txt", "a\nLEFT\nc\nd\n"),
            ("base/a.txt", "a\nb\nc\nd\n"),
            ("right/a.txt", "a\nb\nc\nRIGHT\n"),
            ("merged/a.txt", "a\nLEFT\nc\nRIGHT\n"),
        ],
    );
    let good = oxidemerge(d, &["--validate-merge", "merged", "left", "base", "right"]);
    assert_eq!(good.code, 0, "{}", good.stderr);
    assert_eq!(read(d.join("base/a.txt")), "a\nb\nc\nd\n");
    files(d, &[("merged/a.txt", "incorrect\n")]);
    let bad = oxidemerge(d, &["--validate-merge", "merged", "left", "base", "right"]);
    assert_eq!(bad.code, 3, "{}", bad.stderr);
}

#[test]
fn merge_validation_rejects_eol_conflicts_even_when_output_matches_the_base() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(d, &[("left", "a\r\n"), ("base", "a\n"), ("right", "a\r")]);
    std::fs::create_dir(d.join(".config")).unwrap();
    std::fs::write(
        d.join(".config/config.json"),
        r#"{"diff":{"ignore_eol":false,"ignore_codepage":false}}"#,
    )
    .unwrap();
    let run = oxidemerge(d, &["--validate-merge", "base", "left", "base", "right"]);
    assert_eq!(run.code, 3, "{} {}", run.stdout, run.stderr);
    assert!(run.stdout.contains("unresolved_conflict"));
    assert_eq!(read(d.join("base")), "a\n");
}

/// The capture times of `report`, the only part of a report that differs between runs, blanked.
fn without_capture_times(report: &str) -> String {
    let mut out = String::new();
    let mut rest = report;
    while let Some(at) = rest.find("captured_at") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        // "captured_at": "2026-11-01T13:38:33Z", quoted with `"` or `&quot;`
        let time = tail
            .char_indices()
            .find(|(_, c)| c.is_ascii_digit())
            .map(|(i, _)| i)
            .filter(|&i| i < 40 && tail.get(i..i + 20).is_some_and(|t| t.ends_with('Z')))
            .unwrap_or_else(|| panic!("a capture time after {}", &tail[..40.min(tail.len())]));
        out.push_str(&tail[..time]);
        out.push_str("CAPTURE-TIME");
        rest = &tail[time + 20..];
    }
    out.push_str(rest);
    out
}

/// Semantic, release summary and impact reports of fixed inputs match the reports saved under
/// `tests/fixtures/reports`, so any change to their content or layout is a reviewed change.
/// After an intended change, `OXIDEMERGE_UPDATE_REPORTS=1 cargo test --test cli reports_match`
/// saves the new reports to review in the diff.
#[test]
fn reports_match_the_saved_reference_reports() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut inputs = vec![
        (
            "a.json",
            r#"{"name":"svc","db":{"host":"x","port":5432},"items":[{"id":1,"v":1},{"id":2,"v":2}]}"#,
        ),
        (
            "b.json",
            r#"{"name":"svc","db":{"host":"x","port":6543},"items":[{"id":2,"v":2},{"id":1,"v":10}]}"#,
        ),
    ];
    let project = [
        ("src/lib.rs", "mod parser;\npub use parser::parse;\n"),
        ("src/main.rs", "use crate::parser::{parse, Token};\n\nfn main() {\n    let _t = Token;\n    println!(\"{}\", parse(\"x\"));\n}\n"),
        ("web/app.js", "import { format as fmt } from \"./util.js\";\n\nconsole.log(fmt(1));\n"),
        ("py/main.py", "from .helpers import normalize\n\nprint(normalize(\" a \"))\n"),
        ("c/main.c", "#include \"limits.h\"\n\nint main(void) { return LIMIT; }\n"),
        ("README.md", "Set db.timeout in config.json.\n"),
        ("notes.txt", "unchanged\n"),
    ];
    for side in ["old", "new"] {
        for (name, text) in project {
            inputs.push((Box::leak(format!("{side}/{name}").into_boxed_str()), text));
        }
    }
    inputs.extend([
        (
            "old/src/parser.rs",
            "pub fn parse(s: &str) -> usize {\n    s.len()\n}\n\npub struct Token;\n",
        ),
        (
            "new/src/parser.rs",
            "pub fn parse(s: &str) -> usize {\n    s.trim().len()\n}\n\npub struct Token;\n",
        ),
        (
            "old/web/util.js",
            "export function format(x) {\n  return String(x);\n}\n",
        ),
        (
            "new/web/util.js",
            "export function format(x, digits) {\n  return Number(x).toFixed(digits);\n}\n",
        ),
        (
            "old/py/helpers.py",
            "def normalize(text):\n    return text.strip()\n",
        ),
        (
            "new/py/helpers.py",
            "def normalize(text):\n    return text.strip().lower()\n",
        ),
        ("old/c/limits.h", "#define LIMIT 3\n"),
        ("new/c/limits.h", "#define LIMIT 4\n"),
        (
            "old/config.json",
            "{\"db\": {\"timeout\": 30, \"host\": \"x\"}}\n",
        ),
        (
            "new/config.json",
            "{\"db\": {\"timeout\": 60, \"host\": \"x\"}}\n",
        ),
        ("old/docs/guide.txt", "moved file\n"),
        ("new/guide.txt", "moved file\n"),
        ("old/app.ini", "k1=1\nk2=2\nk3=3\nk4=4\n"),
        ("new/main.ini", "k1=1\nk2=2\nk3=3\nk4=40\n"),
    ]);
    files(d, &inputs);
    let saved = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reports");
    let update = std::env::var_os("OXIDEMERGE_UPDATE_REPORTS").is_some();
    let mut differing = Vec::new();
    for ext in ["json", "xml"] {
        let runs: [(&str, Vec<&str>); 3] = [
            (
                "semantic",
                vec![
                    "--semantic",
                    "--array-key",
                    "id",
                    "a.json",
                    "b.json",
                    "--report",
                ],
            ),
            (
                "release-summary",
                vec![
                    "--baseline-left",
                    "folder:old",
                    "--baseline-right",
                    "folder:new",
                    "--release-report",
                ],
            ),
            ("impact", vec!["--impact", "old", "new", "--report"]),
        ];
        for (name, mut args) in runs {
            let file = format!("{name}.{ext}");
            args.push(&file);
            let run = oxidemerge(d, &args);
            assert_eq!(run.code, 1, "{file}: {}", run.stderr);
            let report = without_capture_times(&read(d.join(&file)));
            if update {
                std::fs::create_dir_all(&saved).unwrap();
                std::fs::write(saved.join(&file), &report).unwrap();
            } else if report != read(saved.join(&file)) {
                differing.push(file);
            }
        }
    }
    assert!(
        differing.is_empty(),
        "{differing:?} differ from tests/fixtures/reports; after an intended change, rerun with \
         OXIDEMERGE_UPDATE_REPORTS=1 and review the diff"
    );
}

#[test]
fn command_line_version_displays_the_same_build_identifier_as_about() {
    let dir = tempfile::tempdir().unwrap();
    let run = oxidemerge(dir.path(), &["--version"]);
    assert_eq!(run.code, 0);
    assert!(run.stdout.contains(env!("CARGO_PKG_VERSION")));
    assert!(run
        .stdout
        .contains(&format!("(build {})", env!("OXIDEMERGE_BUILD_ID"))));
}

#[test]
fn folders_are_synchronized_from_a_plan_checked_again_before_applying() {
    let d = tmp();
    let d = d.path();
    files(
        d,
        &[
            ("src/a.txt", "new\n"),
            ("src/sub/b.txt", "b\n"),
            ("dst/a.txt", "old\n"),
            ("dst/extra.txt", "extra\n"),
        ],
    );
    let plan = oxidemerge(
        d,
        &[
            "--sync-plan",
            "mirror",
            "src",
            "dst",
            "--report",
            "plan.json",
        ],
    );
    assert_eq!(plan.code, 1, "{}", plan.stderr);
    let json = read(d.join("plan.json"));
    assert!(json.contains("\"replace\"") && json.contains("\"delete\""));
    assert!(!json.contains(d.to_str().unwrap()), "no absolute paths");
    assert_eq!(
        read(d.join("dst/a.txt")),
        "old\n",
        "planning changes nothing"
    );

    let unconfirmed = oxidemerge(d, &["--sync-apply", "plan.json", "src", "dst"]);
    assert_eq!(unconfirmed.code, 2);
    assert_eq!(read(d.join("dst/extra.txt")), "extra\n");
    let filtered = oxidemerge(
        d,
        &[
            "--sync-apply",
            "plan.json",
            "--yes",
            "--filter",
            "*.md",
            "src",
            "dst",
        ],
    );
    assert_eq!(filtered.code, 2);
    assert!(filtered.stderr.contains("filter"), "{}", filtered.stderr);
    assert_eq!(read(d.join("dst/extra.txt")), "extra\n");

    files(d, &[("dst/a.txt", "OLD\n")]);
    let stale = oxidemerge(d, &["--sync-apply", "plan.json", "--yes", "src", "dst"]);
    assert_eq!(stale.code, 2, "{}", stale.stdout);
    assert_eq!(read(d.join("dst/a.txt")), "OLD\n");

    let plan = oxidemerge(
        d,
        &[
            "--sync-plan",
            "mirror",
            "src",
            "dst",
            "--report",
            "plan.json",
        ],
    );
    assert_eq!(plan.code, 1);
    // A file touched without changing its contents still fits the plan.
    std::fs::File::options()
        .write(true)
        .open(d.join("src/a.txt"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    let applied = oxidemerge(d, &["--sync-apply", "plan.json", "--yes", "src", "dst"]);
    assert_eq!(applied.code, 0, "{}{}", applied.stdout, applied.stderr);
    assert!(applied.stdout.contains("\"applied\""));
    assert_eq!(read(d.join("dst/a.txt")), "new\n");
    assert_eq!(read(d.join("dst/sub/b.txt")), "b\n");
    assert!(!d.join("dst/extra.txt").exists());
    let again = oxidemerge(d, &["--sync-plan", "update", "src", "dst"]);
    assert_eq!(again.code, 0, "{}", again.stdout);

    std::fs::write(
        d.join("future.json"),
        json.replacen("\"version\": 1", "\"version\": 99", 1),
    )
    .unwrap();
    let future = oxidemerge(d, &["--sync-apply", "future.json", "--yes", "src", "dst"]);
    assert_eq!(future.code, 2);
    assert!(future.stderr.contains("99"), "{}", future.stderr);
    let nested = oxidemerge(d, &["--sync-plan", "mirror", "src", "src/sub"]);
    assert_eq!(nested.code, 2);
    let recover = oxidemerge(d, &["--sync-recover"]);
    assert_eq!(recover.code, 0, "{}", recover.stdout);
    let yes_alone = oxidemerge(d, &["--yes", "src", "dst"]);
    assert_eq!(yes_alone.code, 2, "{}", yes_alone.stdout);
}

#[test]
fn a_compare_job_writes_what_its_command_line_writes() {
    let t = tmp();
    let d = t.path();
    folders(d);
    files(
        d,
        &[(
            "jobs/nightly.oxjob",
            r#"{"version": 1, "jobs": [
                {"name": "folders", "kind": "compare", "left": "../left", "right": "../right",
                 "recursive": true, "report": "../job.csv"}]}"#,
        )],
    );
    // A report outside the job file's folder is written only when allowed.
    let refused = oxidemerge(d, &["--job", "jobs/nightly.oxjob"]);
    assert_eq!(refused.code, 2, "{}{}", refused.stdout, refused.stderr);
    assert!(refused.stderr.contains("--yes"), "{}", refused.stderr);
    assert!(!d.join("job.csv").exists());
    let job = oxidemerge(d, &["--job", "jobs/nightly.oxjob", "--yes"]);
    assert_eq!(job.code, 1, "{}{}", job.stdout, job.stderr);
    let summary: serde_json::Value = serde_json::from_str(&job.stdout).unwrap();
    assert_eq!(summary["exit_code"], 1);
    assert_eq!(summary["jobs"][0]["name"], "folders");
    assert_eq!(summary["jobs"][0]["exit_code"], 1);
    let direct = oxidemerge(d, &["-r", "left", "right", "--report", "direct.csv"]);
    assert_eq!(direct.code, 1);
    // The same rows; the header names the folders as the job file's folder reaches them.
    let rows = |p: &str| {
        read(d.join(p))
            .lines()
            .skip(2)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(rows("job.csv"), rows("direct.csv"));
    let header = read(d.join("job.csv"));
    let first = header.lines().next().unwrap();
    assert!(
        first.ends_with(&format!("jobs{}../left", std::path::MAIN_SEPARATOR)),
        "{first}"
    );
}

#[test]
fn a_sync_job_changes_nothing_without_yes_and_stops_the_jobs_after_it() {
    let t = tmp();
    let d = t.path();
    files(
        d,
        &[
            ("src/a.txt", "new\n"),
            ("dst/a.txt", "old\n"),
            ("dst/extra.txt", "extra\n"),
            (
                "sync.oxjob",
                r#"{"version": 1, "jobs": [
                    {"kind": "sync_plan", "source": "src", "destination": "dst",
                     "mode": "mirror", "report": "plan.json"},
                    {"name": "mirror", "kind": "sync", "source": "src", "destination": "dst",
                     "mode": "mirror", "apply": true},
                    {"name": "after", "kind": "compare", "left": "src", "right": "dst"}]}"#,
            ),
        ],
    );
    let refused = oxidemerge(d, &["--job", "sync.oxjob"]);
    assert_eq!(refused.code, 2, "{}{}", refused.stdout, refused.stderr);
    let summary: serde_json::Value = serde_json::from_str(&refused.stdout).unwrap();
    assert_eq!(summary["jobs"][0]["exit_code"], 1, "the plan has changes");
    assert!(summary["jobs"][1]["error"]
        .as_str()
        .unwrap()
        .contains("--yes"));
    assert_eq!(summary["not_run"][0], "after");
    assert!(read(d.join("plan.json")).contains("\"delete\""));
    assert_eq!(read(d.join("dst/a.txt")), "old\n");
    assert_eq!(read(d.join("dst/extra.txt")), "extra\n");

    let applied = oxidemerge(d, &["--job", "sync.oxjob", "--yes"]);
    assert_eq!(applied.code, 1, "{}{}", applied.stdout, applied.stderr);
    let summary: serde_json::Value = serde_json::from_str(&applied.stdout).unwrap();
    assert_eq!(summary["jobs"][1]["sync"]["status"], "applied");
    assert_eq!(
        summary["jobs"][2]["exit_code"], 0,
        "identical after the sync"
    );
    assert_eq!(read(d.join("dst/a.txt")), "new\n");
    assert!(!d.join("dst/extra.txt").exists());
}

#[test]
fn jobs_go_on_after_a_failure_only_when_asked_and_large_deletions_need_leave() {
    let t = tmp();
    let d = t.path();
    std::fs::create_dir_all(d.join("src")).unwrap();
    for i in 0..60 {
        files(d, &[(&format!("dst/f{i}.txt"), "x\n")]);
    }
    files(
        d,
        &[
            ("a.txt", "a\n"),
            (
                "jobs.oxjob",
                r#"{"version": 1, "continue_on_error": true, "jobs": [
                    {"kind": "compare", "left": "missing.txt", "right": "a.txt"},
                    {"kind": "sync", "source": "src", "destination": "dst",
                     "mode": "mirror", "apply": true},
                    {"kind": "compare", "left": "a.txt", "right": "a.txt"}]}"#,
            ),
        ],
    );
    let run = oxidemerge(d, &["--job", "jobs.oxjob", "--yes"]);
    assert_eq!(run.code, 2);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(summary["jobs"][0]["exit_code"], 2);
    assert!(summary["jobs"][1]["error"]
        .as_str()
        .unwrap()
        .contains("allow_large_deletion"));
    assert_eq!(summary["jobs"][2]["exit_code"], 0);
    assert_eq!(std::fs::read_dir(d.join("dst")).unwrap().count(), 60);

    let with_paths = oxidemerge(d, &["--job", "jobs.oxjob", "a.txt", "a.txt"]);
    assert_eq!(with_paths.code, 2);
    for option in ["-r", "--ignore-case", "--filter=*.c", "--method=date"] {
        let with_option = oxidemerge(d, &["--job", "jobs.oxjob", option]);
        assert_eq!(with_option.code, 2, "{option}");
        assert!(with_option.stdout.is_empty(), "{option}: no job runs");
    }
    files(d, &[("future.oxjob", r#"{"version": 9, "jobs": []}"#)]);
    let future = oxidemerge(d, &["--job", "future.oxjob", "--lang", "en"]);
    assert_eq!(future.code, 2);
    assert!(future.stderr.contains("version 9"), "{}", future.stderr);
}

#[test]
fn pdf_reports_are_written_for_text_and_folder_compares() {
    let dir = tmp();
    let d = dir.path();
    files(d, &[("a.txt", "one\ntwo\n"), ("b.txt", "one\nthree\n")]);
    let run = oxidemerge(d, &["a.txt", "b.txt", "--report", "t.pdf", "--unified"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    let pdf = std::fs::read(d.join("t.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"), "a PDF file");
    assert!(!run.stderr.contains("note:"), "{}", run.stderr);

    folders(d);
    let run = oxidemerge(
        d,
        &[
            "left",
            "right",
            "-r",
            "--report",
            "f.pdf",
            "--report-diffs",
            "--page-size",
            "letter",
            "--whole-file",
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let pdf = std::fs::read(d.join("f.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(
        String::from_utf8_lossy(&pdf).contains("612"),
        "Letter pages"
    );

    // PDF options without a PDF report are pointed out.
    let run = oxidemerge(d, &["a.txt", "b.txt", "--report", "t.html", "--portrait"]);
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.contains("--portrait only applies to .pdf"),
        "{}",
        run.stderr
    );
}

#[cfg(unix)]
#[test]
fn sync_links_copies_symbolic_links_as_links_from_a_plan_and_a_job() {
    let dir = tmp();
    let d = dir.path();
    files(d, &[("src/a.txt", "a\n"), ("dst/a.txt", "a\n")]);
    std::os::unix::fs::symlink("a.txt", d.join("src/to-a")).unwrap();
    std::os::unix::fs::symlink("gone", d.join("dst/old-link")).unwrap();

    // Without --sync-links a link is left alone.
    let plain = oxidemerge(d, &["--sync-plan", "mirror", "src", "dst"]);
    assert!(plain.stdout.contains("\"conflict\""), "{}", plain.stdout);

    let plan = oxidemerge(
        d,
        &[
            "--sync-plan",
            "mirror",
            "--sync-links",
            "src",
            "dst",
            "--report",
            "plan.json",
        ],
    );
    assert_eq!(plan.code, 1, "{}", plan.stderr);
    assert!(read(d.join("plan.json")).contains("\"copy_links\": true"));
    let applied = oxidemerge(d, &["--sync-apply", "plan.json", "--yes", "src", "dst"]);
    assert_eq!(applied.code, 0, "{}{}", applied.stdout, applied.stderr);
    assert_eq!(
        std::fs::read_link(d.join("dst/to-a")).unwrap(),
        Path::new("a.txt")
    );
    assert!(std::fs::symlink_metadata(d.join("dst/old-link")).is_err());

    // A job takes it as "copy_links", only for synchronizations.
    std::os::unix::fs::symlink("a.txt", d.join("src/second")).unwrap();
    std::fs::write(
        d.join("links.oxjob"),
        r#"{"version": 1, "jobs": [{"kind": "sync", "source": "src", "destination": "dst",
            "mode": "mirror", "apply": true, "copy_links": true}]}"#,
    )
    .unwrap();
    let job = oxidemerge(d, &["--job", "links.oxjob", "--yes"]);
    assert_eq!(job.code, 0, "{}{}", job.stdout, job.stderr);
    assert!(std::fs::symlink_metadata(d.join("dst/second")).is_ok_and(|m| m.is_symlink()));
    std::fs::write(
        d.join("bad.oxjob"),
        r#"{"version": 1, "jobs": [{"kind": "compare", "left": "src", "right": "dst",
            "copy_links": true}]}"#,
    )
    .unwrap();
    let bad = oxidemerge(d, &["--job", "bad.oxjob"]);
    assert_eq!(bad.code, 2);
    assert!(bad.stdout.contains("copy_links"), "{}", bad.stdout);
}

#[test]
fn diff_style_labels_title_the_sides_and_descriptions_win_over_them() {
    let d = tmp();
    let d = d.path();
    files(d, &[("old.txt", "a\nb\n"), ("new.txt", "a\nB\n")]);
    // As Subversion's --diff-cmd calls it: -u, then one -L per file, then the files.
    let run = oxidemerge(
        d,
        &[
            "--report",
            "labels.html",
            "-u",
            "-L",
            "f.txt\t(revision 1)",
            "-L",
            "f.txt\t(working copy)",
            "old.txt",
            "new.txt",
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let html = read(d.join("labels.html"));
    assert!(html.contains("f.txt\t(revision 1)"), "left label");
    assert!(html.contains("f.txt\t(working copy)"), "right label");

    // A file named with a leading dash still has its title taken as one.
    let run = oxidemerge(
        d,
        &[
            "--report",
            "dash.html",
            "-L",
            "-notes.txt\t(revision 1)",
            "-L",
            "-notes.txt\t(working copy)",
            "old.txt",
            "new.txt",
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(read(d.join("dash.html")).contains("-notes.txt\t(revision 1)"));

    let run = oxidemerge(
        d,
        &[
            "--report",
            "described.html",
            "--label",
            "Left label",
            "--label",
            "Right label",
            "--dr",
            "Mine",
            "old.txt",
            "new.txt",
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let html = read(d.join("described.html"));
    assert!(
        html.contains("Left label"),
        "a label fills a side with no description"
    );
    assert!(
        html.contains("Mine") && !html.contains("Right label"),
        "--dr wins"
    );
}

#[test]
fn subversion_working_copies_compare_with_their_revisions() {
    let d = tmp();
    let d = d.path();
    let svn = |dir: &Path, program: &str, args: &[&str]| {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(dir);
        // A UTF-8 locale is how `svn` is given names on Unix. Windows uses the
        // ANSI code page instead, and a UTF-8 locale does not change that.
        if !cfg!(windows) {
            cmd.env(
                "LC_CTYPE",
                if cfg!(target_os = "macos") {
                    "UTF-8"
                } else {
                    "C.UTF-8"
                },
            );
        }
        cmd.output()
    };
    let repo = d.join("repo");
    if svn(d, "svnadmin", &["create", repo.to_str().unwrap()]).is_err() {
        eprintln!("Subversion is not installed; skipped");
        return;
    }
    let url = format!(
        "file:///{}",
        repo.to_str()
            .unwrap()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let wc = d.join("wc");
    let ok = |dir: &Path, program: &str, args: &[&str]| {
        let out = svn(dir, program, args).unwrap();
        assert!(
            out.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    ok(d, "svn", &["checkout", "-q", &url, wc.to_str().unwrap()]);
    // A name with an @ (a peg revision to svn) and letters that are not ASCII.
    files(&wc, &[("f.txt", "one\n"), ("a @b/é.txt", "x\n")]);
    ok(&wc, "svn", &["add", "-q", "f.txt", "a @b@"]);
    ok(&wc, "svn", &["commit", "-q", "-m", "first"]);
    files(&wc, &[("f.txt", "two\n"), ("later.txt", "new\n")]);
    ok(&wc, "svn", &["add", "-q", "later.txt"]);
    ok(&wc, "svn", &["commit", "-q", "-m", "second"]);
    ok(&wc, "svn", &["update", "-q"]);

    let run = oxidemerge(&wc, &["--cli", "--rev", "BASE", "f.txt"]);
    assert_eq!(run.code, 0, "unchanged since BASE: {}", run.stderr);
    files(&wc, &[("f.txt", "three\n"), ("a @b/é.txt", "y\n")]);
    let run = oxidemerge(&wc, &["--cli", "--rev", "BASE", "f.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-two") && run.stdout.contains("+three"),
        "{}",
        run.stdout
    );
    // r1, as svn log prints it, is revision 1.
    let run = oxidemerge(&wc, &["--cli", "--git-rev", "r1", "f.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-one") && run.stdout.contains("+three"),
        "{}",
        run.stdout
    );
    let run = oxidemerge(&wc, &["--cli", "--rev", "BASE", "a @b/é.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-x") && run.stdout.contains("+y"),
        "{}",
        run.stdout
    );

    // A folder: later.txt was added after revision 1.
    let run = oxidemerge(
        &wc,
        &[
            "--lang", "en", "--cli", "--rev", "1", "-f", "*.txt", "-r", ".",
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let later = run.stdout.lines().find(|l| l.starts_with("later.txt"));
    assert!(
        later.is_some_and(|l| l.contains("Right only")),
        "{}",
        run.stdout
    );
    let run = oxidemerge(&wc, &["--cli", "--rev", "BASE", "a @b"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("é.txt"), "{}", run.stdout);

    // A folder that was not there in revision 1 compares as empty, as in Git.
    files(&wc, &[("added/n.txt", "n\n")]);
    ok(&wc, "svn", &["add", "-q", "added"]);
    ok(&wc, "svn", &["commit", "-q", "-m", "third"]);
    let run = oxidemerge(&wc, &["--lang", "en", "--cli", "--rev", "1", "-r", "added"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("Right only"), "{}", run.stdout);

    let run = oxidemerge(&wc, &["--cli", "--rev", "99", "f.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("E160006"), "{}", run.stderr);
    let run = oxidemerge(&wc, &["--cli", "--rev=-r1", "f.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("invalid revision"), "{}", run.stderr);
}

/// A letter Mercurial can store here. On Windows it has to survive the ANSI code page:
/// `é` does on 1252, and `中` does on 950. A page that has neither still runs the ASCII cases.
fn mercurial_letter() -> &'static str {
    for letter in ["é", "中", "я", "ä"] {
        if ansi_round_trips(letter) {
            return letter;
        }
    }
    "x"
}

fn ansi_round_trips(text: &str) -> bool {
    #[cfg(not(windows))]
    {
        let _ = text;
        return true;
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetACP() -> u32;
            fn WideCharToMultiByte(
                code_page: u32,
                flags: u32,
                wide: *const u16,
                wide_len: i32,
                multi: *mut u8,
                multi_len: i32,
                default_char: *const u8,
                used_default: *mut i32,
            ) -> i32;
            fn MultiByteToWideChar(
                code_page: u32,
                flags: u32,
                multi: *const u8,
                multi_len: i32,
                wide: *mut u16,
                wide_len: i32,
            ) -> i32;
        }
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let cp = unsafe { GetACP() };
        if cp == 65001 {
            return true;
        }
        let bytes_len = unsafe {
            WideCharToMultiByte(
                cp,
                0,
                wide.as_ptr(),
                -1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        if bytes_len <= 1 {
            return false;
        }
        let mut bytes = vec![0u8; bytes_len as usize];
        let wrote = unsafe {
            WideCharToMultiByte(
                cp,
                0,
                wide.as_ptr(),
                -1,
                bytes.as_mut_ptr(),
                bytes_len,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        if wrote <= 1 {
            return false;
        }
        let wide_len =
            unsafe { MultiByteToWideChar(cp, 0, bytes.as_ptr(), -1, std::ptr::null_mut(), 0) };
        if wide_len <= 1 {
            return false;
        }
        let mut back = vec![0u16; wide_len as usize];
        unsafe {
            MultiByteToWideChar(cp, 0, bytes.as_ptr(), -1, back.as_mut_ptr(), wide_len);
        }
        back.pop();
        String::from_utf16(&back).ok().as_deref() == Some(text)
    }
}

#[test]
fn mercurial_repositories_compare_with_their_revisions() {
    let d = tmp();
    let d = d.path();
    let repo = d.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let hgrc = d.join("hgrc");
    std::fs::write(&hgrc, "").unwrap();
    // Windows Mercurial reads names in the ANSI code page. Forcing UTF-8 makes a name
    // that code page can spell fail to add. Unix builds already use UTF-8.
    let hg = |args: &[&str]| {
        let mut cmd = Command::new("hg");
        cmd.args(args)
            .current_dir(&repo)
            .env("HGRCPATH", &hgrc)
            .env("HGUSER", "test")
            .env("HGPLAIN", "1");
        if cfg!(windows) {
            cmd.env_remove("HGENCODING");
        } else {
            cmd.env("HGENCODING", "utf-8");
        }
        cmd.output()
    };
    if hg(&["init"]).is_err() {
        eprintln!("Mercurial is not installed; skipped");
        return;
    }
    let ok = |args: &[&str]| {
        let out = hg(args).unwrap();
        assert!(
            out.status.success(),
            "hg {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    // Brackets would be a pattern to hg. The letter is one this system's ANSI code page
    // can spell: Mercurial on Windows cannot open a name outside that page.
    let extra = mercurial_letter();
    let nested = format!("a b[x]/{extra}.txt");
    files(&repo, &[("f.txt", "one\n"), (nested.as_str(), "x\n")]);
    ok(&["add", "-q"]);
    ok(&["commit", "-q", "-m", "first"]);
    files(&repo, &[("f.txt", "two\n"), ("later.txt", "new\n")]);
    ok(&["add", "-q", "later.txt"]);
    ok(&["commit", "-q", "-m", "second"]);
    let env = [("HGRCPATH", hgrc.to_str().unwrap())];

    let run = oxidemerge_with_env(&repo, &["--cli", "--rev", ".", "f.txt"], &env);
    assert_eq!(run.code, 0, "unchanged since the parent: {}", run.stderr);
    files(&repo, &[("f.txt", "three\n"), (nested.as_str(), "y\n")]);
    // No revision is the working copy's parent, and the title says which.
    let run = oxidemerge_with_env(&repo, &["--cli", "--rev", "", "f.txt"], &env);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-two") && run.stdout.contains("+three"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("f.txt @ ."), "{}", run.stdout);
    let run = oxidemerge_with_env(&repo, &["--cli", "--rev", "0", "f.txt"], &env);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("-one"), "{}", run.stdout);
    let run = oxidemerge_with_env(&repo, &["--cli", "--rev", ".", &nested], &env);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stdout.contains("-x") && run.stdout.contains("+y"),
        "{}",
        run.stdout
    );

    let run = oxidemerge_with_env(
        &repo,
        &[
            "--lang", "en", "--cli", "--rev", "0", "-f", "*.txt", "-r", ".",
        ],
        &env,
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    let later = run.stdout.lines().find(|l| l.starts_with("later.txt"));
    assert!(
        later.is_some_and(|l| l.contains("Right only")),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains(".hg_archival"), "{}", run.stdout);
    // A folder with no files in that revision compares as empty.
    files(&repo, &[("new/n.txt", "n\n")]);
    let run = oxidemerge_with_env(
        &repo,
        &["--lang", "en", "--cli", "--rev", "0", "-r", "new"],
        &env,
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("n.txt"), "{}", run.stdout);

    // Mercurial counts back from the tip with negative numbers.
    let run = oxidemerge_with_env(&repo, &["--cli", "--rev=-2", "f.txt"], &env);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("-one"), "{}", run.stdout);

    let run = oxidemerge_with_env(&repo, &["--cli", "--rev", "nosuch", "f.txt"], &env);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("unknown revision"), "{}", run.stderr);
}

#[test]
fn a_path_in_no_working_copy_names_every_system_looked_for() {
    let d = tmp();
    let d = d.path();
    files(d, &[("f.txt", "a\n")]);
    let run = oxidemerge_with_env(
        d,
        &["--cli", "--rev", "HEAD", "f.txt"],
        &[(
            "GIT_CEILING_DIRECTORIES",
            d.parent().unwrap().to_str().unwrap(),
        )],
    );
    assert_eq!(run.code, 2);
    assert!(
        run.stderr
            .contains("is not in a Git, Subversion or Mercurial working copy"),
        "{}",
        run.stderr
    );
}

#[test]
fn paths_on_a_server_are_checked_before_anything_connects() {
    let t = tmp();
    let d = t.path();
    files(d, &[("a.txt", "a\n")]);
    let no_password = [
        ("OXIDEMERGE_REMOTE_PASSWORD", ""),
        ("OXIDEMERGE_PROXY_PASSWORD", ""),
    ];
    let run = |args: &[&str]| {
        let mut all = vec!["--cli", "--lang", "en"];
        all.extend(args);
        oxidemerge_with_env(d, &all, &no_password)
    };

    let bad = run(&["sftp://", "a.txt"]);
    assert_eq!(bad.code, 2);
    assert!(bad.stderr.contains("Not a server path"), "{}", bad.stderr);

    // A password in the address is refused, and not repeated where others may see it.
    let password = run(&["sftp://ann:hunter2@127.0.0.1/x", "a.txt"]);
    assert_eq!(password.code, 2);
    assert!(
        password.stderr.contains("OXIDEMERGE_REMOTE_PASSWORD"),
        "{}",
        password.stderr
    );
    assert!(!password.stderr.contains("hunter2"), "{}", password.stderr);

    let ftp = run(&["ftp://ann@127.0.0.1:1/x", "a.txt"]);
    assert_eq!(ftp.code, 2);
    assert!(
        ftp.stderr.contains("OXIDEMERGE_REMOTE_PASSWORD"),
        "{}",
        ftp.stderr
    );

    let key = run(&["--ssh-key", "a.txt", "ftps://ann@127.0.0.1:1/x", "a.txt"]);
    assert_eq!(key.code, 2);
    assert!(!key.stderr.is_empty());

    // So is a password in a proxy's address, and an address that is not a proxy's.
    let proxy = |url: &str| run(&["--proxy", url, "ftp://127.0.0.1:1/x", "a.txt"]);
    let password = proxy("socks5://bob:hunter2@127.0.0.1:1");
    assert_eq!(password.code, 2);
    assert!(
        password.stderr.contains("OXIDEMERGE_PROXY_PASSWORD"),
        "{}",
        password.stderr
    );
    assert!(!password.stderr.contains("hunter2"), "{}", password.stderr);
    let bad = proxy("ftp://127.0.0.1:1");
    assert_eq!(bad.code, 2);
    assert!(bad.stderr.contains("Not a proxy address"), "{}", bad.stderr);

    // A client certificate is read before anything connects.
    let cert = run(&["--client-cert", "a.txt", "ftps://127.0.0.1:1/x", "a.txt"]);
    assert_eq!(cert.code, 2);
    assert!(
        cert.stderr.contains("Cannot read a certificate"),
        "{}",
        cert.stderr
    );

    // Nothing listens on port 1: the error names the server.
    let anonymous = run(&["ftp://127.0.0.1:1/x", "a.txt"]);
    assert_eq!(anonymous.code, 2);
    assert!(
        anonymous.stderr.contains("127.0.0.1"),
        "{}",
        anonymous.stderr
    );

    files(
        d,
        &[(
            "remote.oxjob",
            r#"{"version": 1, "jobs": [
                {"name": "site", "kind": "compare", "left": "a.txt",
                 "right": "ftp://ann@127.0.0.1:1/a.txt"}]}"#,
        )],
    );
    let job = oxidemerge_with_env(d, &["--job", "remote.oxjob", "--lang", "en"], &no_password);
    assert_eq!(job.code, 2, "{}{}", job.stdout, job.stderr);
    let summary: serde_json::Value = serde_json::from_str(&job.stdout).unwrap();
    assert!(summary["jobs"][0]["error"]
        .as_str()
        .unwrap()
        .contains("OXIDEMERGE_REMOTE_PASSWORD"));
}
