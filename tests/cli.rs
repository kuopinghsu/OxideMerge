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
    let config = dir.join(".config");
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(env!("CARGO_BIN_EXE_oxidemerge"))
        .args(args)
        .current_dir(dir)
        .env("OXIDEMERGE_CONFIG_DIR", &config)
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
    let run = oxidemerge(d, &["--cli", "--git-rev", "HEAD", "f.txt"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.contains("-old\n+new"), "{}", run.stdout);
}
