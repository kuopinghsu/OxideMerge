#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod core;
mod i18n;
mod shell;
mod ui;

use clap::{Parser, ValueEnum};
use config::Config;
use core::diff::WhitespaceMode;
use core::filter::FileFilter;
use core::folder::{CmpStatus, CompareMethod, Progress, Scanner};
use core::patch::PatchStyle;
use core::report::ReportFormat;
use core::text::FileKind;
use std::path::PathBuf;
use std::process::ExitCode;
use ui::app::{OpenRequest, OxideMergeApp};

#[derive(Clone, Copy, ValueEnum)]
enum MethodArg {
    Full,
    Quick,
    Binary,
    Date,
    Datesize,
    Size,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Text,
    Binary,
    Image,
    /// Web pages (URLs or HTML files); the console compare uses the visible text
    Web,
}

#[derive(Clone, Copy, ValueEnum)]
enum WsArg {
    Compare,
    Change,
    All,
}

#[derive(Clone, Copy, ValueEnum)]
enum PatchArg {
    Normal,
    Context,
    Unified,
}

/// OxideMerge - compare and merge files and folders on Windows, macOS and Linux.
///
/// WinMerge style switches (/r, /f, /dl, /wl, /o, /or ...) are accepted on Windows as well.
#[derive(Parser)]
#[command(
    name = "oxidemerge",
    version = concat!(env!("CARGO_PKG_VERSION"), " (build ", env!("OXIDEMERGE_BUILD_DATE"), ")"),
    about,
    long_about = None
)]
struct Cli {
    /// Files, folders or http(s) URLs to compare (2 or 3; a single conflict file is opened for resolution)
    paths: Vec<PathBuf>,
    /// Compare folders recursively
    #[arg(short = 'r', long)]
    recursive: bool,
    /// File filter: wildcard mask (e.g. "*.c;*.h;!*.bak") or "[F] <filter name>"
    #[arg(short = 'f', long)]
    filter: Option<String>,
    /// Folder compare method
    #[arg(short = 'm', long, value_enum)]
    method: Option<MethodArg>,
    /// Force the file compare type
    #[arg(short = 't', long = "compare-as", value_enum)]
    kind: Option<KindArg>,
    /// Merge output file (middle pane in 3-way, right pane in 2-way)
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,
    /// Description of the left side
    #[arg(long)]
    dl: Option<String>,
    /// Description of the middle side
    #[arg(long)]
    dm: Option<String>,
    /// Description of the right side
    #[arg(long)]
    dr: Option<String>,
    /// Open the left side read-only
    #[arg(long)]
    wl: bool,
    /// Open the middle side read-only
    #[arg(long)]
    wm: bool,
    /// Open the right side read-only
    #[arg(long)]
    wr: bool,
    /// Whitespace handling
    #[arg(long, value_enum)]
    ignore_ws: Option<WsArg>,
    #[arg(long)]
    ignore_case: bool,
    #[arg(long)]
    ignore_blank: bool,
    #[arg(long)]
    ignore_eol: bool,
    #[arg(long)]
    ignore_numbers: bool,
    /// Ignore changes in comments (for languages whose comment syntax is known)
    #[arg(long)]
    ignore_comments: bool,
    /// Reformat JSON and XML files before comparing them
    #[arg(long)]
    prettify: bool,
    /// Treat numbers at most this far apart as equal (for example 0.001)
    #[arg(long, value_name = "AMOUNT", value_parser = finite_number)]
    numeric_tolerance: Option<f64>,
    /// Compare rules file (lines to ignore, substitutions, blocks to ignore): the name of a file
    /// in the rules folder, or the path of an .oxrules file; "" for none
    #[arg(long, value_name = "NAME")]
    rules: Option<String>,
    /// Write a report without opening the GUI: folder compares as .html, .csv or .txt, text
    /// compares as side-by-side .html
    #[arg(long)]
    report: Option<PathBuf>,
    /// Add the differences of every changed text file to an HTML folder report
    #[arg(long)]
    report_diffs: bool,
    /// Compare the single path given with how it was in this Git revision (e.g. HEAD, HEAD~1,
    /// a branch, tag or commit)
    #[arg(long, value_name = "REV")]
    git_rev: Option<String>,
    /// Write a patch file without opening the GUI
    #[arg(long)]
    patch: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "unified")]
    patch_style: PatchArg,
    /// Context lines for context/unified patches
    #[arg(long, default_value_t = 3)]
    context: usize,
    /// Compare in the console only: print differences, exit code 0 = identical, 1 = different, 2 = error
    #[arg(long)]
    cli: bool,
    /// User interface language code (en, zh-TW, zh-CN, ja, ko, de, fr, es, ru, pt-BR, ...)
    #[arg(long)]
    lang: Option<String>,
    /// List available languages
    #[arg(long)]
    list_languages: bool,
    /// Add "OxideMerge" entries to the file manager's context menu (Windows Explorer, Finder
    /// Quick Actions, Nautilus, Nemo or Dolphin)
    #[arg(long)]
    register_shell: bool,
    /// Remove the file manager context menu entries
    #[arg(long)]
    unregister_shell: bool,
    /// Remember a path as the left side for a later compare (Explorer integration)
    #[arg(long)]
    select_left: Option<PathBuf>,
    /// Remember a path as the middle side for a later 3-way compare (Explorer integration)
    #[arg(long)]
    select_middle: Option<PathBuf>,
    /// Compare a path with the side(s) remembered by --select-left and --select-middle
    #[arg(long)]
    compare_to: Option<PathBuf>,
    /// Explorer's Compare entry: compares all items selected together, or one item like --compare-to
    #[arg(long, hide = true)]
    shell_compare: Option<PathBuf>,
    /// File manager entries (Finder, Nautilus, ...): compares the given items, or one item like
    /// --compare-to
    #[arg(long, hide = true)]
    compare_selected: bool,
}

/// Translates WinMerge style "/switch" arguments into the equivalent long options.
fn winmerge_args() -> Vec<String> {
    let mut out = Vec::new();
    for a in std::env::args() {
        if cfg!(windows) && a.starts_with('/') && a.len() > 1 && !a[1..].contains(['/', '\\', ':'])
        {
            let sw = a[1..].to_ascii_lowercase();
            let mapped = match sw.as_str() {
                "r" => Some("--recursive"),
                "f" => Some("--filter"),
                "o" => Some("--output"),
                "or" => Some("--report"),
                "dl" => Some("--dl"),
                "dm" => Some("--dm"),
                "dr" => Some("--dr"),
                "wl" => Some("--wl"),
                "wm" => Some("--wm"),
                "wr" => Some("--wr"),
                "t" => Some("--compare-as"),
                "m" => Some("--method"),
                "e" | "u" | "x" | "s" | "minimize" | "maximize" | "noninteractive" | "noprefs"
                | "ub" | "fl" | "fm" | "fr" => None,
                _ => {
                    out.push(a);
                    continue;
                }
            };
            if let Some(m) = mapped {
                out.push(m.to_string());
            }
            continue;
        }
        out.push(a);
    }
    out
}

fn attach_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

fn apply_cli_options(cli: &Cli, cfg: &mut Config) {
    if let Some(ws) = cli.ignore_ws {
        cfg.diff.whitespace = match ws {
            WsArg::Compare => WhitespaceMode::Compare,
            WsArg::Change => WhitespaceMode::IgnoreChange,
            WsArg::All => WhitespaceMode::IgnoreAll,
        };
    }
    cfg.diff.ignore_case |= cli.ignore_case;
    cfg.diff.ignore_blank_lines |= cli.ignore_blank;
    cfg.diff.ignore_eol |= cli.ignore_eol;
    cfg.diff.ignore_numbers |= cli.ignore_numbers;
    cfg.diff.ignore_comments |= cli.ignore_comments;
    cfg.diff.prettify |= cli.prettify;
    if let Some(t) = cli.numeric_tolerance {
        cfg.diff.numeric_tolerance_enabled = true;
        cfg.diff.numeric_tolerance = t.abs();
    }
    if let Some(m) = cli.method {
        cfg.folder.method = method(m);
    }
    if cli.recursive {
        cfg.folder.recursive = true;
    }
}

fn method(m: MethodArg) -> CompareMethod {
    match m {
        MethodArg::Full => CompareMethod::FullContents,
        MethodArg::Quick => CompareMethod::QuickContents,
        MethodArg::Binary => CompareMethod::BinaryContents,
        MethodArg::Date => CompareMethod::Date,
        MethodArg::Datesize => CompareMethod::DateSize,
        MethodArg::Size => CompareMethod::Size,
    }
}

fn file_kind(k: KindArg) -> FileKind {
    match k {
        KindArg::Text => FileKind::Text,
        KindArg::Binary => FileKind::Binary,
        KindArg::Image => FileKind::Image,
        KindArg::Web => FileKind::Web,
    }
}

fn patch_style(p: PatchArg) -> PatchStyle {
    match p {
        PatchArg::Normal => PatchStyle::Normal,
        PatchArg::Context => PatchStyle::Context,
        PatchArg::Unified => PatchStyle::Unified,
    }
}

/// `path` as it was in Git revision `rev`, in a temporary folder that lives as long as the
/// returned guard.
fn git_copy(path: &std::path::Path, rev: &str) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    if path.is_dir() {
        return core::git::export_folder(path, rev);
    }
    let bytes = core::git::show_file(path, rev)?;
    let dir = tempfile::tempdir()?;
    let copy = dir.path().join(path.file_name().unwrap_or_default());
    std::fs::write(&copy, bytes)?;
    Ok((dir, copy))
}

/// Tells about the patterns of the rules that are not valid regular expressions: the compare
/// goes on without them.
fn warn_bad_patterns(d: &core::diff::DiffOptions) {
    let mut patterns: Vec<&str> = Vec::new();
    if d.line_filters_enabled {
        patterns.extend(d.line_filters.iter().map(String::as_str));
    }
    if d.substitutions_enabled {
        patterns.extend(d.substitutions.iter().map(|(f, _)| f.as_str()));
    }
    if d.block_filters_enabled {
        patterns.extend(
            d.block_filters
                .iter()
                .flat_map(|(s, e)| [s.as_str(), e.as_str()]),
        );
    }
    for p in patterns {
        if !p.is_empty() && regex::Regex::new(p).is_err() {
            let pattern = p.to_string();
            eprintln!(
                "note: {}",
                i18n::trf("opt.invalid_regex", &[("pattern", pattern)])
            );
        }
    }
}

fn finite_number(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        Ok(_) => Err("expected a finite number".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Non-interactive operations (console compare, report, patch). Returns the process exit code.
fn run_batch(cli: &Cli, cfg: &Config) -> anyhow::Result<u8> {
    let mut cli_paths = cli.paths.clone();
    let mut _git_temp = None;
    let mut git_desc = None;
    if let (Some(rev), [path]) = (&cli.git_rev, &cli.paths[..]) {
        let (temp, copy) = git_copy(path, rev)?;
        git_desc = Some(format!("{} @ {rev}", path.display()));
        cli_paths = vec![copy, path.clone()];
        _git_temp = Some(temp);
    }
    let paths = &cli_paths;
    let cli_desc = |cli: &Cli, i: usize| match (i, &git_desc) {
        (0, Some(d)) if cli.dl.is_none() => Some(d.clone()),
        _ => cli_desc(cli, i),
    };
    if paths.len() < 2 {
        anyhow::bail!("two or three paths are required");
    }
    if cli.report_diffs && cli.report.is_none() {
        eprintln!("note: --report-diffs needs --report");
    }
    let patch = match &cli.patch {
        Some(_) if paths.len() != 2 => {
            eprintln!("note: --patch needs two paths; no patch written");
            None
        }
        p => p.as_ref(),
    };
    if paths.iter().all(|p| p.is_dir()) {
        let filter_spec = cli.filter.clone().unwrap_or_else(|| cfg.filter.clone());
        let filter = FileFilter::resolve(&filter_spec, &core::filter::load_all_filters());
        let progress = Progress::default();
        let mut fopts = cfg.folder.clone();
        fopts.recursive = cli.recursive || fopts.recursive;
        let items = Scanner {
            roots: paths,
            filter: &filter,
            fopts: &fopts,
            dopts: &cfg.diff,
            progress: &progress,
        }
        .run();
        let mut shown: Vec<PathBuf> = paths.clone();
        if let Some(d) = &git_desc {
            shown[0] = PathBuf::from(d);
        }
        let display: Vec<String> = shown.iter().map(|p| p.display().to_string()).collect();
        let files: Vec<&core::folder::DirItem> = items
            .iter()
            .filter(|i| !i.is_dir || i.children.is_empty())
            .collect();
        // Folders left uncompared (not recursive) say nothing about their files.
        let different = files.iter().any(|i| {
            i.status != CmpStatus::Identical && !(i.is_dir && i.status == CmpStatus::NotCompared)
        });
        let failed = files.iter().any(|i| i.status == CmpStatus::Error);
        if let Some(r) = &cli.report {
            let format = ReportFormat::from_path(r);
            if cli.report_diffs && format != ReportFormat::Html {
                eprintln!("note: --report-diffs only applies to .html reports");
            }
            let text = if cli.report_diffs && format == ReportFormat::Html {
                core::report::folder_html_with_diffs(&files, paths, &shown, &cfg.diff, &cfg.rules)
            } else {
                core::report::generate(&files, &shown, format, &cfg.rules)
            };
            std::fs::write(r, text)?;
        }
        if let Some(p) = patch {
            let text = core::patch::folder_patch(
                paths,
                &display,
                &items,
                &cfg.diff,
                patch_style(cli.patch_style),
                cli.context,
            );
            std::fs::write(p, text)?;
        }
        if cli.cli {
            let changed: Vec<&core::folder::DirItem> = files
                .iter()
                .copied()
                .filter(|i| {
                    i.status != CmpStatus::Identical
                        && !(i.is_dir && i.status == CmpStatus::NotCompared)
                })
                .collect();
            print!(
                "{}",
                core::report::generate(&changed, &shown, ReportFormat::Text, &cfg.rules)
            );
        }
        return Ok(if failed { 2 } else { u8::from(different) });
    }
    if cli.report_diffs && cli.report.is_some() {
        eprintln!("note: --report-diffs only applies to folder compares");
    }
    let kind = match cli.kind {
        Some(k) => file_kind(k),
        None if paths
            .iter()
            .any(|p| core::web::is_url(&p.to_string_lossy())) =>
        {
            FileKind::Web
        }
        None => {
            if paths
                .iter()
                .any(|p| core::text::detect_kind(p) == FileKind::Binary)
            {
                FileKind::Binary
            } else {
                FileKind::Text
            }
        }
    };
    if matches!(kind, FileKind::Binary | FileKind::Image) {
        if cli.report.is_some() {
            eprintln!("note: --report is not written for binary or image files");
        }
        let data = paths
            .iter()
            .map(std::fs::read)
            .collect::<Result<Vec<_>, _>>()?;
        let ranges = core::binary::diff_ranges(&data.iter().map(Vec::as_slice).collect::<Vec<_>>());
        if cli.cli {
            for r in ranges.iter().take(100) {
                println!("0x{:08X}-0x{:08X} differ", r.start, r.end - 1);
            }
            if ranges.is_empty() {
                println!("{}", i18n::tr("msg.binary_identical"));
            }
        }
        return Ok(u8::from(!ranges.is_empty()));
    }
    let mut docs: Vec<core::text::TextDoc> = if kind == FileKind::Web {
        let urls = paths
            .iter()
            .map(|p| core::web::location(p))
            .collect::<Result<Vec<_>, _>>()?;
        core::web::fetch_pages(&urls)?
            .iter()
            .map(|p| {
                let text = match cfg.web_mode {
                    core::web::WebMode::Html => core::web::format_html(&p.html),
                    _ => core::web::html_to_text(&p.html),
                };
                core::text::TextDoc::from_text(None, &text)
            })
            .collect()
    } else {
        paths
            .iter()
            .map(|p| core::text::load_doc(p, None))
            .collect::<Result<_, _>>()?
    };
    if cfg.diff.prettify {
        for d in &mut docs {
            let text = d.text();
            let pretty = core::prettify::detect(d.path.as_deref(), &text)
                .and_then(|f| core::prettify::format(&text, f));
            if let Some(p) = pretty {
                d.lines = core::text::split_lines(&p).0;
            }
        }
    }
    let syntax = paths.iter().find_map(|p| core::comments::for_path(p));
    let refs: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
    let result = core::diff::compute_with(&refs, &cfg.diff, syntax);
    if let Some(r) = &cli.report {
        if ReportFormat::from_path(r) != ReportFormat::Html {
            eprintln!("note: text compares are reported as HTML only");
        }
        let titles: Vec<String> = (0..docs.len())
            .map(|i| cli_desc(cli, i).unwrap_or_else(|| paths[i].display().to_string()))
            .collect();
        std::fs::write(
            r,
            core::report::text_html(&titles, &refs, &result, &cfg.diff, &[], &cfg.rules),
        )?;
    }
    if docs.len() == 2 {
        let name = |i: usize| cli_desc(cli, i).unwrap_or_else(|| paths[i].display().to_string());
        let (na, nb) = (name(0), name(1));
        let date = |i: usize| {
            std::fs::metadata(&paths[i])
                .and_then(|m| m.modified())
                .map(|t| core::report::fmt_time(Some(t)))
                .unwrap_or_default()
        };
        let (da, db) = (date(0), date(1));
        let input = core::patch::PatchInput {
            name_a: &na,
            name_b: &nb,
            date_a: &da,
            date_b: &db,
            a: &docs[0].lines,
            b: &docs[1].lines,
        };
        let text =
            core::patch::make_patch(&input, &result, patch_style(cli.patch_style), cli.context);
        if let Some(p) = patch {
            std::fs::write(p, &text)?;
        }
        if cli.cli {
            print!("{text}");
        }
    } else if cli.cli {
        for b in result.blocks.iter().filter(|b| !b.ignored) {
            println!(
                "{:?}: left {}-{}, middle {}-{}, right {}-{}",
                b.kind,
                b.ranges[0].start + 1,
                b.ranges[0].end,
                b.ranges[1].start + 1,
                b.ranges[1].end,
                b.ranges[2].start + 1,
                b.ranges[2].end
            );
        }
    }
    Ok(u8::from(!result.is_identical()))
}

fn cli_desc(cli: &Cli, i: usize) -> Option<String> {
    match (cli.paths.len(), i) {
        (_, 0) => cli.dl.clone(),
        (2, 1) => cli.dr.clone(),
        (3, 1) => cli.dm.clone(),
        _ => cli.dr.clone(),
    }
}

fn app_icon() -> egui::IconData {
    match ui::widgets::logo_image() {
        Some(img) => egui::IconData {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        },
        None => egui::IconData::default(),
    }
}

/// WSLg accelerates OpenGL only through Mesa's d3d12 driver. Left to itself, Mesa probes the
/// DRM device and Vulkan (zink) first, prints errors for each, then settles on llvmpipe.
/// An unknown `GALLIUM_DRIVER` makes Mesa fail outright, so d3d12 is only selected when installed.
#[cfg(target_os = "linux")]
fn wsl_gl_defaults() {
    use std::path::Path;
    if !Path::new("/dev/dxg").exists() {
        return;
    }
    if std::env::var_os("GALLIUM_DRIVER").is_none() {
        let arch_dir = format!("/usr/lib/{}-linux-gnu/dri", std::env::consts::ARCH);
        let mut dirs: Vec<String> = std::env::var("LIBGL_DRIVERS_PATH")
            .unwrap_or_default()
            .split(':')
            .map(String::from)
            .collect();
        dirs.extend([arch_dir, "/usr/lib64/dri".into(), "/usr/lib/dri".into()]);
        if dirs
            .iter()
            .any(|d| !d.is_empty() && Path::new(d).join("d3d12_dri.so").exists())
        {
            std::env::set_var("GALLIUM_DRIVER", "d3d12");
        }
    }
    if std::env::var_os("EGL_LOG_LEVEL").is_none() {
        std::env::set_var("EGL_LOG_LEVEL", "fatal");
    }
}

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    wsl_gl_defaults();
    attach_console();
    let cli = Cli::parse_from(winmerge_args());
    let mut cfg = Config::load();
    if core::rules::load_selected(&mut cfg) {
        cfg.save();
    }
    let stored = cfg.clone();
    if let Some(l) = &cli.lang {
        cfg.language = l.clone();
    }
    i18n::set_language(&cfg.language);

    if cli.list_languages {
        for l in i18n::languages() {
            println!("{:8} {}", l.code, l.name);
        }
        return ExitCode::SUCCESS;
    }
    if cli.register_shell {
        return match shell::register() {
            Ok(()) => {
                println!("{}", i18n::tr(ui::app::SHELL_KEYS[2]));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(2)
            }
        };
    }
    if cli.unregister_shell {
        let _ = shell::unregister();
        println!("{}", i18n::tr(ui::app::SHELL_KEYS[3]));
        return ExitCode::SUCCESS;
    }
    let selected = [(&cli.select_left, false), (&cli.select_middle, true)];
    if let Some((Some(p), middle)) = selected.into_iter().find(|(p, _)| p.is_some()) {
        let mut selection = shell::Selection::load();
        selection.select(shell::clean_arg(p), middle);
        selection.save();
        return ExitCode::SUCCESS;
    }
    let mut paths = cli.paths.clone();
    if let Some(p) = &cli.shell_compare {
        match shell::gather(&shell::clean_arg(p)) {
            Some(items) => paths = shell::compare_paths(items),
            None => return ExitCode::SUCCESS,
        }
    } else if let Some(p) = &cli.compare_to {
        paths = shell::compare_paths(vec![shell::clean_arg(p)]);
    } else if cli.compare_selected {
        paths = shell::compare_paths(paths);
    }

    apply_cli_options(&cli, &mut cfg);
    if cli.git_rev.is_some() && paths.len() != 1 {
        eprintln!("error: --git-rev compares exactly one file or folder with a revision");
        return ExitCode::from(2);
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let cli_rules = cli
        .rules
        .as_deref()
        .map(|r| core::rules::absolute_spec(r, &cwd));
    // `Some(None)` turns the rules of the options off ("--rules ''").
    let rules = match cli_rules.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(name) => match core::rules::find(name) {
            Some(set) => Some(Some(set)),
            None => {
                eprintln!("error: no compare rules named {name:?}");
                return ExitCode::from(2);
            }
        },
    };

    if cli.cli || cli.report.is_some() || cli.patch.is_some() {
        if let (Some(set), Some(name)) = (&rules, &cli_rules) {
            cfg.diff = core::rules::options_with(&cfg.diff, set.as_ref());
            cfg.rules = name.clone();
        } else if !cfg.rules.is_empty() && core::rules::find(&cfg.rules).is_none() {
            eprintln!(
                "note: the selected compare rules {:?} are missing; comparing without them",
                cfg.rules
            );
            cfg.rules.clear();
        }
        warn_bad_patterns(&cfg.diff);
        return match run_batch(&cli, &cfg) {
            Ok(code) => ExitCode::from(code),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        };
    }

    let initial = if let (Some(rev), [_]) = (&cli.git_rev, &paths[..]) {
        Some(OpenRequest {
            paths,
            git_rev: Some(rev.clone()),
            rules: cli_rules.clone(),
            ..Default::default()
        })
    } else if paths.is_empty() {
        None
    } else {
        let n = paths.len();
        let descriptions = (0..n).map(|i| cli_desc(&cli, i)).collect();
        let read_only = (0..n)
            .map(|i| match (n, i) {
                (_, 0) => cli.wl,
                (2, 1) => cli.wr,
                (3, 1) => cli.wm,
                _ => cli.wr,
            })
            .collect();
        Some(OpenRequest {
            paths,
            descriptions,
            read_only,
            kind: cli.kind.map(file_kind),
            recursive: if cli.recursive { Some(true) } else { None },
            filter: cli.filter.clone(),
            method: cli.method.map(method),
            output: cli.output.clone(),
            rules: cli_rules.clone(),
            ..Default::default()
        })
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("OxideMerge")
        .with_app_id("oxidemerge")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([640.0, 400.0])
        .with_drag_and_drop(true)
        .with_icon(std::sync::Arc::new(app_icon()));
    if cfg!(target_os = "macos") {
        // The menu bar draws into the title bar, next to the window buttons.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        "OxideMerge",
        options,
        Box::new(move |cc| Ok(Box::new(OxideMergeApp::new(cc, cfg, stored, initial)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
