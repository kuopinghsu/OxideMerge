#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod core;
mod i18n;
mod prompt;
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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Web pages (URLs or HTML files, which are compared so by default); the console compare
    /// uses the pages as Markdown, or the web mode last chosen
    Web,
}

#[derive(Clone, Copy, ValueEnum)]
enum WsArg {
    Compare,
    Change,
    All,
}

#[derive(Clone, Copy, ValueEnum)]
enum SyncArg {
    /// Make the destination match the source, deleting what only it has
    Mirror,
    /// Copy new and changed files to the destination; delete nothing
    Update,
}

#[derive(Clone, Copy, ValueEnum)]
enum PageArg {
    A4,
    Letter,
}

/// How much of the differences an HTML report shows.
#[derive(Clone, Copy, ValueEnum)]
enum DetailArg {
    /// Counts and where each difference is, without lines; reads no file of a folder compare
    Summary,
    /// Up to 20,000 rows in all, 400 per difference and 2,000 characters per line
    Standard,
    /// Every row and whole lines; the report grows with the differences
    Full,
}

impl From<DetailArg> for core::report::ReportDetail {
    fn from(d: DetailArg) -> Self {
        match d {
            DetailArg::Summary => Self::Summary,
            DetailArg::Standard => Self::Standard,
            DetailArg::Full => Self::Full,
        }
    }
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
    version = concat!(env!("CARGO_PKG_VERSION"), " (build ", env!("OXIDEMERGE_BUILD_ID"), ")"),
    about,
    long_about = None
)]
struct Cli {
    /// Files, folders or http(s) URLs to compare (2 or 3; a single conflict file is opened for
    /// resolution). A file or folder on a server is written sftp://[user@]host[:port]/path,
    /// ftp://, ftpes:// (explicit FTPS) or ftps:// (implicit FTPS); /~/path is in the user's
    /// home folder. The password, or the --ssh-key passphrase, is read from
    /// OXIDEMERGE_REMOTE_PASSWORD; SFTP signs in with ssh-agent's and ~/.ssh/config's keys
    /// without either, and follows ~/.ssh/config for host aliases, users, ports and jump hosts
    paths: Vec<PathBuf>,
    /// Private key file to sign in to SFTP servers with
    #[arg(long, value_name = "FILE")]
    ssh_key: Option<PathBuf>,
    /// Trust a server not trusted yet when its key or certificate has this fingerprint
    /// (SHA256:…, as an unknown server's error shows it), and remember it
    #[arg(long, value_name = "FINGERPRINT")]
    trust_fingerprint: Option<String>,
    /// Connect to servers through this proxy: socks5://[user@]host[:port] or
    /// http://[user@]host[:port]; its password is read from OXIDEMERGE_PROXY_PASSWORD
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
    /// Use active-mode FTP, where the server connects back for each transfer
    #[arg(long)]
    ftp_active: bool,
    /// Client certificate (PEM) for FTPS servers that ask for one
    #[arg(long, value_name = "FILE")]
    client_cert: Option<PathBuf>,
    /// Private key (PEM) of --client-cert, when not in the same file
    #[arg(long, value_name = "FILE", requires = "client_cert")]
    client_key: Option<PathBuf>,
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
    /// Title of a side, given once per path in the same order (as diff -L, which Subversion and
    /// other version control systems pass); --dl, --dm and --dr take precedence
    #[arg(
        short = 'L',
        long = "label",
        value_name = "TITLE",
        allow_hyphen_values = true
    )]
    labels: Vec<String>,
    /// Accepted for diff compatibility (Subversion passes -u) and ignored
    #[allow(dead_code)]
    #[arg(short = 'u', hide = true)]
    diff_unified: bool,
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
    /// Folder compares: JSON, XML, TOML and YAML files that read as the same structure are
    /// identical (full contents method)
    #[arg(long)]
    structured: bool,
    /// Treat numbers at most this far apart as equal (for example 0.001)
    #[arg(long, value_name = "AMOUNT", value_parser = finite_number)]
    numeric_tolerance: Option<f64>,
    /// Compare rules file (lines to ignore, substitutions, blocks to ignore): the name of a file
    /// in the rules folder, or the path of an .oxrules file; "" for none
    #[arg(long, value_name = "NAME")]
    rules: Option<String>,
    /// Write a report without opening the GUI: folder compares as .html, .pdf, .csv, .txt or
    /// .xml, text compares as side-by-side .html, as .pdf or as .xml
    #[arg(long)]
    report: Option<PathBuf>,
    /// Page size of a .pdf report
    #[arg(long, value_enum, default_value = "a4")]
    page_size: PageArg,
    /// Portrait pages in a .pdf report (landscape otherwise)
    #[arg(long)]
    portrait: bool,
    /// Show the lines of a two-way .pdf report in one column, removed above added
    #[arg(long)]
    unified: bool,
    /// Show the whole files in a .pdf report, not only the differences with --context lines
    #[arg(long)]
    whole_file: bool,
    /// Add the differences of every changed text file to an HTML or PDF folder report
    #[arg(long)]
    report_diffs: bool,
    /// Name the compared folders Left, Middle and Right in a folder report, not by their paths
    #[arg(long)]
    report_hide_paths: bool,
    /// How much of the differences an .html report shows [default: standard]
    #[arg(long, value_enum, value_name = "DETAIL")]
    report_detail: Option<DetailArg>,
    /// Write the .html report as a slideshow, one difference per slide
    #[arg(long)]
    slideshow: bool,
    /// Compare the single path given with how it was in this revision of its Git repository
    /// (HEAD, HEAD~1, a branch, tag or commit), Subversion working copy (BASE, PREV, HEAD or a
    /// number) or Mercurial repository (., .^, tip, a number or tag)
    #[arg(long, visible_alias = "rev", value_name = "REV")]
    git_rev: Option<String>,
    /// Write a patch file without opening the GUI
    #[arg(long)]
    patch: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "unified")]
    patch_style: PatchArg,
    /// Context lines for context/unified patches and .pdf reports
    #[arg(long, default_value_t = 3)]
    context: usize,
    /// Compare in the console only: print differences, exit code 0 = identical, 1 = different, 2 = error
    #[arg(long)]
    cli: bool,
    /// Read-only three-way merge plan (JSON on stdout, or --report)
    #[arg(long)]
    merge_plan: bool,
    /// Validate this existing result against the three input files (left, base, right)
    #[arg(long, value_name = "RESULT")]
    validate_merge: Option<PathBuf>,
    /// Read-only plan to synchronize two folders (source, destination): JSON on stdout, or
    /// --report; exit code 0 = nothing to do, 1 = changes or conflicts, 2 = error
    #[arg(long, value_enum, value_name = "MODE")]
    sync_plan: Option<SyncArg>,
    /// With --sync-plan: synchronize symbolic links as links, never following them (Unix);
    /// otherwise links are left alone
    #[arg(long, requires = "sync_plan")]
    sync_links: bool,
    /// Apply a --sync-plan file to the folders it was made for, given in the same order, after
    /// checking every file again; needs --yes. Exit code 0 = applied, 2 = nothing changed,
    /// 4 = failed or stopped and undone, 5 = partly applied, recovery required
    #[arg(long, value_name = "PLAN", requires = "yes")]
    sync_apply: Option<PathBuf>,
    /// Confirm that --sync-apply, or the sync jobs of --job, may change and delete files in the
    /// destination folder, and that the jobs of --job may write outside the job file's folder
    #[arg(long)]
    yes: bool,
    /// Run the jobs of a .oxjob file (JSON: compares, reports, sync plans and syncs) in order,
    /// stopping at the first that fails; prints a JSON summary. Sync jobs that apply changes, and
    /// reports or patches outside the job file's folder, need --yes. Exit code: the worst of the jobs (0 identical or applied, 1 different, 2 error,
    /// 4 undone, 5 recovery required)
    #[arg(long, value_name = "FILE")]
    job: Option<PathBuf>,
    /// Undo folder synchronizations that did not finish; exit code 0 = undone, 5 = not all
    #[arg(long)]
    sync_recover: bool,
    /// Read-only JSON/XML/TOML/YAML semantic comparison of two files, or of three (left, base,
    /// right) with conflicts by path
    #[arg(long)]
    semantic: bool,
    /// Semantic format: json, xml, toml or yaml (default: from the first file's extension)
    #[arg(long, value_name = "FORMAT", requires = "semantic")]
    format: Option<String>,
    /// Pair object elements of arrays by this member's value instead of by position
    #[arg(long, value_name = "MEMBER", requires = "semantic")]
    array_key: Option<String>,
    /// Summarize annotations from this portable .oxmreview
    #[arg(long, value_name = "SNAPSHOT")]
    review_summary: Option<PathBuf>,
    /// Evidence-only potential references for the two selected folder baselines
    #[arg(long)]
    impact: bool,
    /// folder:PATH, git:REV:PATH or review:SIDE:PATH (zero-based snapshot side)
    #[arg(long, requires = "baseline_right")]
    baseline_left: Option<String>,
    #[arg(long, requires = "baseline_left")]
    baseline_right: Option<String>,
    /// Write an opt-in release baseline report (.json/.html/.xml)
    #[arg(long)]
    release_report: Option<PathBuf>,
    /// Flag changed baseline paths containing this literal substring; repeatable
    #[arg(long)]
    important_path: Vec<String>,
    /// Keep this added baseline path added rather than a suspected rename with edits; repeatable
    #[arg(long, value_name = "PATH")]
    split_rename: Vec<String>,
    /// User interface language code (en, zh-TW, zh-CN, ja, ko, de, fr, es, ru, pt-BR, it, ...)
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
    cfg.folder.structured |= cli.structured;
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

/// `path` as it was in revision `rev` of its Git repository, Subversion working copy or
/// Mercurial repository, in a temporary folder that lives as long as the returned guard.
fn git_copy(path: &std::path::Path, rev: &str) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    core::vcs::copy_of(path, rev, &AtomicBool::new(false))
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

thread_local! {
    /// Set while a job runs: its reports and patches never replace a link or special file.
    static JOB_OUTPUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Writes a report or patch whole or not at all, so a failed run leaves no file that looks done.
fn export(path: &std::path::Path, text: &str) -> anyhow::Result<()> {
    write_output(path, text.as_bytes())
}

fn write_output(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    if JOB_OUTPUT.get() {
        core::fsutil::write_atomic_report(path, bytes)
    } else {
        core::fsutil::write_atomic_with(path, bytes, false)
    }
}

/// How much of the differences an HTML report shows, from the command line.
fn report_detail(cli: &Cli) -> core::report::ReportDetail {
    cli.report_detail.map(Into::into).unwrap_or_default()
}

/// Whether a report file is a PDF.
fn is_pdf(path: &std::path::Path) -> bool {
    core::text::extension(path) == "pdf"
}

/// How a .pdf report is laid out, from the command line and the color-blind setting.
fn pdf_options(cli: &Cli, cfg: &Config) -> core::pdf_report::PdfOptions {
    use core::pdf_report::{Layout, PageSize, PdfOptions};
    PdfOptions {
        page: match cli.page_size {
            PageArg::A4 => PageSize::A4,
            PageArg::Letter => PageSize::Letter,
        },
        portrait: cli.portrait,
        layout: if cli.unified {
            Layout::Unified
        } else {
            Layout::SideBySide
        },
        context: (!cli.whole_file).then_some(cli.context),
        color_blind: cfg.color_blind_colors,
    }
}

/// Notes options that only apply to a .pdf report when no such report is written.
fn note_pdf_options(cli: &Cli) {
    if cli.report.as_deref().is_some_and(is_pdf) {
        return;
    }
    let set: Vec<&str> = [
        (cli.portrait, "--portrait"),
        (cli.unified, "--unified"),
        (cli.whole_file, "--whole-file"),
        (!matches!(cli.page_size, PageArg::A4), "--page-size"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if !set.is_empty() {
        let verb = if set.len() == 1 { "applies" } else { "apply" };
        eprintln!("note: {} only {verb} to .pdf reports", set.join(", "));
    }
}

/// Non-interactive operations (console compare, report, patch). Returns the process exit code.
fn run_batch(cli: &Cli, cfg: &Config) -> anyhow::Result<u8> {
    if analysis_mode(cli) {
        return run_analysis(cli, cfg);
    }
    let mut cli_paths = cli.paths.clone();
    let mut _git_temp = None;
    let mut git_desc = None;
    if let (Some(rev), [path]) = (&cli.git_rev, &cli.paths[..]) {
        let (temp, copy) = git_copy(path, rev)?;
        let rev = core::vcs::rev_or_default(path, rev);
        git_desc = Some(format!("{} @ {rev}", path.display()));
        cli_paths = vec![copy, path.clone()];
        _git_temp = Some(temp);
    }
    let paths = &cli_paths;
    let cli_desc = |cli: &Cli, i: usize| match (i, &git_desc) {
        (0, Some(d)) if cli.dl.is_none() => Some(d.clone()),
        _ => cli_desc(cli, i),
    };
    if !(2..=3).contains(&paths.len()) {
        anyhow::bail!(
            "two or three paths are required, {} were given",
            paths.len()
        );
    }
    if cli.report_diffs && cli.report.is_none() {
        eprintln!("note: --report-diffs needs --report");
    }
    if cli.report_hide_paths && cli.report.is_none() {
        eprintln!("note: --report-hide-paths needs --report");
    }
    if cli.report_detail.is_some() {
        let html = cli.report.as_deref().map(ReportFormat::from_path) == Some(ReportFormat::Html);
        if cli.report.is_none() {
            eprintln!("note: --report-detail needs --report");
        } else if !html || cli.slideshow {
            eprintln!("note: --report-detail only applies to .html reports");
        }
    }
    if cli.slideshow && cli.report.is_none() {
        eprintln!("note: --slideshow needs --report");
    }
    note_pdf_options(cli);
    // A slideshow is an HTML report.
    let report_format = |r: &std::path::Path| match ReportFormat::from_path(r) {
        ReportFormat::Html if cli.slideshow => ReportFormat::Slideshow,
        f => {
            if cli.slideshow {
                eprintln!("note: --slideshow only applies to .html reports");
            }
            f
        }
    };
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
        // The names reports give the folders.
        let named = core::report::shown_roots(&shown, cli.report_hide_paths);
        let info = core::report::FolderReportInfo {
            method: Some(fopts.method),
            filter: filter_spec.clone(),
            scope: None,
            hide_paths: cli.report_hide_paths,
            detail: report_detail(cli),
        };
        if let Some(r) = cli.report.as_deref().filter(|r| is_pdf(r)) {
            if cli.slideshow {
                eprintln!("note: --slideshow only applies to .html reports");
            }
            let fonts = ui::fonts::document_fonts(i18n::current_language());
            let pdf = core::pdf_report::folder_pdf(
                &files,
                paths,
                &shown,
                cli.report_diffs.then_some(&cfg.diff),
                &cfg.rules,
                &info,
                &[],
                &fonts,
                &pdf_options(cli, cfg),
            )
            .map_err(anyhow::Error::msg)?;
            write_output(r, &pdf)?;
        } else if let Some(r) = &cli.report {
            let format = report_format(r);
            if cli.report_diffs {
                match format {
                    ReportFormat::Html => {}
                    ReportFormat::Slideshow => {
                        eprintln!("note: a slideshow shows the differences; --report-diffs ignored")
                    }
                    _ => eprintln!("note: --report-diffs only applies to .html and .pdf reports"),
                }
            }
            let text = match format {
                ReportFormat::Slideshow => core::report::folder_slideshow(
                    &files, paths, &shown, &cfg.diff, &cfg.rules, &info,
                ),
                ReportFormat::Html => core::report::folder_html(
                    &files,
                    paths,
                    &shown,
                    cli.report_diffs.then_some(&cfg.diff),
                    &cfg.rules,
                    &info,
                ),
                f => core::report::generate(&files, &named, f, &cfg.rules),
            };
            export(r, &text)?;
        }
        if let Some(p) = patch {
            let text = core::patch::folder_patch(
                paths,
                &display,
                &items,
                &cfg.diff,
                patch_style(cli.patch_style),
                cli.context,
            )
            .map_err(anyhow::Error::msg)?;
            export(p, &text)?;
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
    if cli.report_hide_paths && cli.report.is_some() {
        eprintln!("note: --report-hide-paths only applies to folder compares");
    }
    let kind = match cli.kind {
        Some(k) => file_kind(k),
        None if paths
            .iter()
            .any(|p| core::web::is_url(&p.to_string_lossy()))
            || (cli.output.is_none()
                && cli.patch.is_none()
                && core::web::are_html_files(paths)) =>
        {
            FileKind::Web
        }
        None => {
            if paths.iter().any(|p| {
                core::unpacker::find(&cfg.comparison_options(), p).is_none()
                    && core::text::detect_kind(p) == FileKind::Binary
            }) {
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
        core::web::fetch_pages(&urls, &Default::default())?
            .iter()
            .map(|p| core::web::page_text(p, cfg.web_mode))
            .map(|text| core::text::TextDoc::from_text(None, &text))
            .collect()
    } else {
        paths
            .iter()
            .map(|p| core::unpacker::load_doc(p, &cfg.comparison_options(), None))
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
    let mut result = core::diff::compute_with(&refs, &cfg.diff, syntax);
    core::diff::include_doc_formats(&mut result, &docs.iter().collect::<Vec<_>>(), &cfg.diff);
    if let Some(r) = cli.report.as_deref().filter(|r| is_pdf(r)) {
        if cli.slideshow {
            eprintln!("note: --slideshow only applies to .html reports");
        }
        let titles: Vec<String> = (0..docs.len())
            .map(|i| cli_desc(cli, i).unwrap_or_else(|| paths[i].display().to_string()))
            .collect();
        let fonts = ui::fonts::document_fonts(i18n::current_language());
        let pdf = core::pdf_report::text_pdf(
            &titles,
            &refs,
            &result,
            &cfg.diff,
            &[],
            &cfg.rules,
            &[],
            &fonts,
            &pdf_options(cli, cfg),
        )
        .map_err(anyhow::Error::msg)?;
        write_output(r, &pdf)?;
    } else if let Some(r) = &cli.report {
        // One note per problem: another type is written as HTML, a slideshow if asked for.
        let format = match ReportFormat::from_path(r) {
            ReportFormat::Html | ReportFormat::Xml => report_format(r),
            _ if cli.slideshow => {
                eprintln!(
                    "note: text compares are reported as .html or .xml; writing an HTML slideshow"
                );
                ReportFormat::Slideshow
            }
            _ => {
                eprintln!("note: text compares are reported as .html or .xml; writing HTML");
                ReportFormat::Html
            }
        };
        let titles: Vec<String> = (0..docs.len())
            .map(|i| cli_desc(cli, i).unwrap_or_else(|| paths[i].display().to_string()))
            .collect();
        export(
            r,
            &core::report::text_report(
                format,
                &titles,
                &refs,
                &result,
                &cfg.diff,
                &[],
                &cfg.rules,
                report_detail(cli),
            ),
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
            final_newline_a: docs[0].final_newline,
            final_newline_b: docs[1].final_newline,
        };
        let text = core::patch::make_doc_patch(
            &input,
            &[&docs[0], &docs[1]],
            &cfg.diff,
            patch_style(cli.patch_style),
            cli.context,
        );
        match text {
            Ok(text) => {
                if let Some(p) = patch {
                    export(p, &text)?;
                }
                if cli.cli {
                    print!("{text}");
                }
            }
            Err(e) if patch.is_some() => return Err(anyhow::anyhow!(e)),
            Err(e) if cli.cli => println!(
                "{}\n{e}",
                if result.is_identical() {
                    "Identical"
                } else {
                    "Different"
                }
            ),
            Err(_) => {}
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

fn analysis_mode(cli: &Cli) -> bool {
    cli.merge_plan
        || cli.sync_plan.is_some()
        || cli.sync_apply.is_some()
        || cli.sync_recover
        || cli.validate_merge.is_some()
        || cli.semantic
        || cli.review_summary.is_some()
        || cli.impact
        || cli.baseline_left.is_some()
        || cli.release_report.is_some()
        || !cli.important_path.is_empty()
        || !cli.split_rename.is_empty()
}
fn analysis_output<T: serde::Serialize>(
    cli: &Cli,
    value: &T,
    mut inputs: Vec<PathBuf>,
) -> anyhow::Result<()> {
    inputs.extend(cli.paths.clone());
    if let Some(path) = cli.release_report.as_ref().or(cli.report.as_ref()) {
        core::analysis::report(value, path, &inputs)
    } else {
        println!("{}", core::analysis::json(value)?);
        Ok(())
    }
}
/// Plans the synchronization of the two folders of the command line, as of a fresh compare.
fn sync_plan(
    cli: &Cli,
    cfg: &Config,
    policy: core::folder_sync::SyncPolicy,
    copy_links: bool,
) -> anyhow::Result<core::folder_sync::SyncPlan> {
    use std::sync::atomic::AtomicBool;
    anyhow::ensure!(
        cli.paths.len() == 2 && cli.paths.iter().all(|p| p.is_dir()),
        "Folder synchronization requires two folders: source, then destination"
    );
    let progress = Progress::default();
    let mut folder = cfg.folder.clone();
    folder.recursive = true;
    folder.include_unique_subfolders = true;
    let spec = cli.filter.as_deref().unwrap_or(&cfg.filter);
    let mask = FileFilter::resolve(spec, &core::filter::load_all_filters());
    let items = Scanner {
        roots: &cli.paths,
        filter: &mask,
        fopts: &folder,
        dopts: &cfg.diff,
        progress: &progress,
    }
    .run();
    let scope = core::folder_sync::Scope {
        complete: true,
        excluded: mask.excludes_anything() || folder.skip_hidden,
        filter: spec.to_string(),
        skip_hidden: folder.skip_hidden,
        copy_links,
    };
    core::folder_sync::plan(&items, &cli.paths, policy, scope, &AtomicBool::new(false))
        .map_err(anyhow::Error::msg)
}

/// How `--sync-apply` and `--sync-recover` ended, printed as JSON.
#[derive(serde::Serialize)]
pub(crate) struct SyncReport {
    pub(crate) status: &'static str,
    /// Why it did not simply succeed, in the shared taxonomy.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<core::problem::Problem>,
    pub(crate) changes: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) messages: Vec<String>,
    /// Recovery journals kept because something could not be undone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    journals: Vec<PathBuf>,
}

fn run_sync(cli: &Cli, cfg: &Config) -> anyhow::Result<u8> {
    use core::folder_sync::{self, Side, SyncMode, SyncPlan, SyncPolicy};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    if let Some(mode) = cli.sync_plan {
        let mode = match mode {
            SyncArg::Mirror => SyncMode::Mirror,
            SyncArg::Update => SyncMode::Update,
        };
        let plan = sync_plan(
            cli,
            cfg,
            SyncPolicy {
                mode,
                source: Side::Left,
            },
            cli.sync_links,
        )?;
        let code = u8::from(plan.changes() > 0 || plan.estimate.conflicts > 0);
        let json = plan.to_json()?;
        match &cli.report {
            Some(path) => core::fsutil::write_atomic_report(path, json.as_bytes())?,
            None => println!("{json}"),
        }
        return Ok(code);
    }
    let journal_root =
        folder_sync::journal_root().ok_or_else(|| anyhow::anyhow!("No settings folder"))?;
    if cli.sync_recover {
        anyhow::ensure!(cli.paths.is_empty(), "--sync-recover takes no paths");
        let mut report = SyncReport {
            status: "recovered",
            problem: None,
            changes: 0,
            messages: Vec::new(),
            journals: Vec::new(),
        };
        for u in folder_sync::unfinished(&journal_root) {
            let r = folder_sync::recover(&u);
            report.changes += r.restored;
            if !r.complete() {
                report.status = "recovery_required";
                report.problem = Some(core::problem::Problem::RecoveryNeeded);
                report.journals.extend(r.journal);
                report
                    .messages
                    .extend(r.failed.into_iter().map(|(p, e)| format!("{p}: {e}")));
            }
        }
        println!("{}", core::analysis::json(&report)?);
        return Ok(if report.status == "recovered" { 0 } else { 5 });
    }
    let Some(path) = &cli.sync_apply else {
        unreachable!("a synchronization mode")
    };
    let text = String::from_utf8(core::analysis::read(path)?)?;
    let planned = SyncPlan::from_json(&text).map_err(anyhow::Error::msg)?;
    // Planned again as the plan was: copying links or not is the plan's, not the command's.
    let fresh = sync_plan(cli, cfg, planned.policy, planned.copy_links)?;
    anyhow::ensure!(
        (&planned.filter, planned.skip_hidden) == (&fresh.filter, fresh.skip_hidden),
        "The plan was made with another filter or hidden-file setting; use the same --filter and settings, or make a new plan with --sync-plan"
    );
    anyhow::ensure!(
        planned.agrees_with(&fresh),
        "The folders are no longer as planned; make a new plan with --sync-plan"
    );
    let (stop, done) = (AtomicBool::new(false), AtomicUsize::new(0));
    let outcome = folder_sync::apply(&planned, &cli.paths, &journal_root, &stop, &done);
    let (code, report) = sync_report(outcome);
    println!("{}", core::analysis::json(&report)?);
    Ok(code)
}

fn run_analysis(cli: &Cli, cfg: &Config) -> anyhow::Result<u8> {
    use core::{analysis, baseline, merge_validation, review, semantic};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    anyhow::ensure!(
        cli.patch.is_none()
            && cli.output.is_none()
            && cli.git_rev.is_none()
            && !cli.slideshow
            && !cli.report_diffs,
        "Analysis is read-only; do not combine it with output/patch/git-rev/slideshow/report-diffs"
    );
    let modes = usize::from(cli.merge_plan || cli.validate_merge.is_some())
        + usize::from(cli.sync_plan.is_some())
        + usize::from(cli.sync_apply.is_some())
        + usize::from(cli.sync_recover)
        + usize::from(cli.semantic)
        + usize::from(cli.review_summary.is_some())
        + usize::from(cli.impact || cli.baseline_left.is_some() || cli.release_report.is_some());
    anyhow::ensure!(
        cli.impact
            || cli.baseline_left.is_some()
            || cli.release_report.is_some()
            || (cli.important_path.is_empty() && cli.split_rename.is_empty()),
        "--important-path and --split-rename need --impact, --release-report or --baseline-left/--baseline-right"
    );
    anyhow::ensure!(modes == 1, "Choose one analysis mode per invocation");
    if cli.sync_plan.is_some() || cli.sync_apply.is_some() || cli.sync_recover {
        return run_sync(cli, cfg);
    }
    if let Some(path) = &cli.review_summary {
        anyhow::ensure!(
            cli.paths.is_empty(),
            "--review-summary takes its snapshot as its flag argument"
        );
        let r = review::load(path)?;
        let paths = r
            .manifest
            .entries
            .iter()
            .filter(|e| !e.directory)
            .map(|e| e.path.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let summary = r.manifest.workspace.summary(paths);
        analysis_output(cli, &summary, vec![path.clone()])?;
        return Ok(u8::from(summary.stale > 0));
    }
    if cli.semantic {
        anyhow::ensure!(
            matches!(cli.paths.len(), 2 | 3),
            "Semantic comparison requires two files, or three: left, base, right"
        );
        let format = match cli.format.as_deref() {
            Some(name) => semantic::Format::of_extension(name).ok_or_else(|| {
                anyhow::anyhow!("Unsupported semantic format {name}; use json, xml, toml or yaml")
            })?,
            None => semantic::Format::of_extension(&core::text::extension(&cli.paths[0]))
                .unwrap_or(semantic::Format::Json),
        };
        let docs: Vec<_> = cli
            .paths
            .iter()
            .map(|p| analysis::read(p).map(|b| core::text::decode(&b, None).0))
            .collect::<anyhow::Result<_>>()?;
        let options = semantic::Options {
            array_key: cli.array_key.clone().filter(|k| !k.is_empty()),
        };
        if let [left, base, right] = &docs[..] {
            let report = semantic::three_way::compare_three(left, base, right, format, &options);
            let code = if !report.errors.is_empty() {
                2
            } else {
                u8::from(!report.identical())
            };
            analysis_output(cli, &report, Vec::new())?;
            return Ok(code);
        }
        let report = semantic::compare_with(&docs[0], &docs[1], format, &options);
        let code = if !report.errors.is_empty() {
            2
        } else {
            u8::from(!report.identical())
        };
        analysis_output(cli, &report, Vec::new())?;
        return Ok(code);
    }
    if cli.merge_plan || cli.validate_merge.is_some() {
        anyhow::ensure!(
            cli.paths.len() == 3,
            "Merge analysis requires left, base, right paths"
        );
        if cli.paths.iter().all(|p| p.is_dir()) {
            let progress = Progress::default();
            let mut folder = cfg.folder.clone();
            folder.recursive = true;
            let mask = FileFilter::resolve(
                cli.filter.as_deref().unwrap_or(&cfg.filter),
                &core::filter::load_all_filters(),
            );
            let items = Scanner {
                roots: &cli.paths,
                filter: &mask,
                fopts: &folder,
                dopts: &cfg.diff,
                progress: &progress,
            }
            .run();
            let report = merge_validation::folder_plan(&items, &cli.paths, &cfg.diff);
            if let Some(destination) = &cli.validate_merge {
                let validated = merge_validation::validate_folder(&report, destination)?;
                let code = match validated.status {
                    merge_validation::ValidationStatus::Failed => 3,
                    merge_validation::ValidationStatus::Warning => 1,
                    _ => 0,
                };
                analysis_output(cli, &validated, vec![destination.clone()])?;
                return Ok(code);
            }
            let code = u8::from(
                report.actions.iter().any(|a| a.operation == "conflict")
                    || !report.findings.is_empty(),
            );
            analysis_output(cli, &report, Vec::new())?;
            return Ok(code);
        }
        let docs: Vec<_> = cli
            .paths
            .iter()
            .map(|p| {
                analysis::read(p).map(|b| core::text::doc_from_bytes(Some(p.clone()), &b, None))
            })
            .collect::<anyhow::Result<_>>()?;
        let view = ui::text_view::TextView::new(docs, cfg);
        let plan = view.preview_merge(cfg)?;
        if let Some(path) = &cli.validate_merge {
            let report = merge_validation::validate_file(&plan, path)?;
            let code = match report.status {
                merge_validation::ValidationStatus::Failed => 3,
                merge_validation::ValidationStatus::Warning => 1,
                _ => 0,
            };
            analysis_output(cli, &report, vec![path.clone()])?;
            return Ok(code);
        }
        let code = u8::from(
            plan.actions.iter().any(|a| a.operation == "conflict")
                || plan
                    .findings
                    .iter()
                    .any(|f| f.severity != merge_validation::ValidationSeverity::Info),
        );
        analysis_output(cli, &plan, Vec::new())?;
        return Ok(code);
    }
    let specs = match (&cli.baseline_left, &cli.baseline_right) {
        (Some(a), Some(b)) => {
            anyhow::ensure!(
                cli.paths.is_empty(),
                "Do not combine positional paths with baseline specifications"
            );
            [
                baseline::BaselineSpec::parse(a)?,
                baseline::BaselineSpec::parse(b)?,
            ]
        }
        _ => {
            anyhow::ensure!(
                cli.paths.len() == 2,
                "Baseline/impact analysis requires two folders or --baseline-left/--baseline-right"
            );
            [
                baseline::BaselineSpec::Folder {
                    path: cli.paths[0].clone(),
                },
                baseline::BaselineSpec::Folder {
                    path: cli.paths[1].clone(),
                },
            ]
        }
    };
    let cancel = AtomicBool::new(false);
    let progress = AtomicUsize::new(0);
    let filter = cli.filter.as_deref().unwrap_or(&cfg.filter);
    let left = baseline::capture(&specs[0], &cfg.diff, filter, &cancel, &progress)?;
    let right = baseline::capture(&specs[1], &cfg.diff, filter, &cancel, &progress)?;
    // written as baseline paths are: with `/` between folders, from the top of the folder
    let baseline_path = |p: &String| {
        let p = p.replace('\\', "/");
        let mut rest = p.as_str();
        while let Some(r) = rest.strip_prefix("./") {
            rest = r;
        }
        rest.to_owned()
    };
    let split = cli.split_rename.iter().map(baseline_path).collect();
    let important: Vec<String> = cli.important_path.iter().map(baseline_path).collect();
    let summary = baseline::compare_with(&left, &right, &important, &split, &cancel)?;
    for path in &split {
        if !summary
            .changes
            .iter()
            .any(|c| &c.path == path && matches!(c.category, baseline::Category::Added))
        {
            eprintln!("--split-rename {path}: not an added path of this comparison");
        }
    }
    let inputs: Vec<PathBuf> = specs
        .iter()
        .map(|s| match s {
            baseline::BaselineSpec::Folder { path }
            | baseline::BaselineSpec::GitRef { path, .. }
            | baseline::BaselineSpec::ReviewSnapshot { path, .. } => path.clone(),
        })
        .collect();
    if cli.impact {
        let report = core::impact::Index::default().analyze(&summary, &left, &right, &cancel)?;
        analysis_output(cli, &report, inputs)?;
    } else if let Some(path) = cli
        .release_report
        .as_ref()
        .or(cli.report.as_ref())
        .filter(|p| ["html", "htm"].contains(&core::text::extension(p).as_str()))
    {
        let html = baseline::html_report(&summary, &left, &right)?;
        analysis::write_report_text(path, &html, &inputs)?;
    } else {
        analysis_output(cli, &summary, inputs)?;
    }
    Ok(
        if summary
            .excluded
            .iter()
            .any(|(_, _, reason)| !baseline::is_scope_exclusion(reason))
        {
            2
        } else {
            u8::from(
                summary
                    .changes
                    .iter()
                    .any(|c| !matches!(c.category, baseline::Category::Unchanged)),
            )
        },
    )
}

/// True when the command line named paths and no option. Defaults such as the page size do
/// not count: only an option the user typed does.
fn command_line_only_paths(matches: &clap::ArgMatches) -> bool {
    <Cli as clap::CommandFactory>::command()
        .get_arguments()
        .all(|arg| {
            let id = arg.get_id().as_str();
            id == "paths"
                || matches.value_source(id) != Some(clap::parser::ValueSource::CommandLine)
        })
}

fn cli_desc(cli: &Cli, i: usize) -> Option<String> {
    let desc = match (cli.paths.len(), i) {
        (_, 0) => cli.dl.clone(),
        (2, 1) => cli.dr.clone(),
        (3, 1) => cli.dm.clone(),
        _ => cli.dr.clone(),
    };
    desc.or_else(|| cli.labels.get(i).cloned())
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

/// The compare rules `--rules` names, relative to `base`: the spec, and the rules
/// (`Some(None)` for `--rules ''`, which turns the rules of the options off).
type CliRules = (Option<String>, Option<Option<core::rules::RuleSet>>);

fn resolve_rules(cli: &Cli, base: &std::path::Path) -> Result<CliRules, String> {
    let spec = cli
        .rules
        .as_deref()
        .map(|r| core::rules::absolute_spec(r, base));
    let rules = match spec.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(name) => match core::rules::find(name) {
            Some(set) => Some(Some(set)),
            None => return Err(format!("no compare rules named {name:?}")),
        },
    };
    Ok((spec, rules))
}

/// Puts the rules of the command line, or else those of the settings while they exist, into
/// the options a console run compares with.
fn use_rules(
    cfg: &mut Config,
    spec: &Option<String>,
    rules: &Option<Option<core::rules::RuleSet>>,
) {
    if let (Some(set), Some(name)) = (rules, spec) {
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
}

/// How one job of `--job` ended, in its summary.
#[derive(serde::Serialize)]
pub(crate) struct JobReport {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// For a sync job, what applying did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sync: Option<SyncReport>,
}

#[derive(serde::Serialize)]
pub(crate) struct JobsReport {
    pub(crate) exit_code: u8,
    pub(crate) jobs: Vec<JobReport>,
    /// Jobs not run because one before failed, or because the run was stopped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) not_run: Vec<String>,
    /// Whether the run was stopped before its last job.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stopped: bool,
}

/// Runs the jobs of `file` one after another, as their command lines would, and prints a JSON
/// summary. The exit code is the worst of the jobs'.
fn run_jobs(file: &std::path::Path, yes: bool, base: &Config) -> anyhow::Result<u8> {
    let jobs = core::job::JobFile::read(file).map_err(anyhow::Error::msg)?;
    core::remote::set_asker(Some(std::sync::Arc::new(prompt::ask)));
    let summary = job_summary(&jobs, &job_dir(file)?, yes, base, &AtomicBool::new(false));
    println!("{}", core::analysis::json(&summary)?);
    Ok(summary.exit_code)
}

/// Fetches the paths of `cli` that are on a server (see [`core::remote_url`]) to temporary
/// copies, which it then compares instead.
fn stage_remote(
    cli: &mut Cli,
    cancel: &AtomicBool,
) -> anyhow::Result<Vec<core::remote_url::Staged>> {
    let remote = |p: &PathBuf| p.to_str().is_some_and(core::remote_url::is_remote_url);
    let sides: Vec<usize> = (0..cli.paths.len())
        .filter(|&i| remote(&cli.paths[i]))
        .collect();
    if sides.is_empty() {
        return Ok(Vec::new());
    }
    let mut creds =
        core::remote_url::Credentials::from_env(cli.ssh_key.clone(), cli.trust_fingerprint.clone());
    creds.options = core::remote::ConnOptions {
        proxy: core::remote_url::proxy(cli.proxy.as_deref())?,
        active: cli.ftp_active,
        client_cert: cli.client_cert.clone(),
        client_key: cli.client_key.clone(),
        in_place: false,
        port_given: false,
    };
    let staged = core::remote_url::stage(&mut cli.paths, &creds, cancel)?;
    // A side is described by its address on the server, not by its temporary copy.
    for (&i, s) in sides.iter().zip(&staged) {
        if s.fetched.is_dir {
            core::folder_sync::name_folder(&s.fetched.local, &s.label());
        }
        if cli_desc(cli, i).is_none() {
            let desc = Some(s.label());
            match (cli.paths.len(), i) {
                (_, 0) => cli.dl = desc,
                (3, 1) => cli.dm = desc,
                _ => cli.dr = desc,
            }
        }
    }
    Ok(staged)
}

/// The folder of a job file, which its relative paths are in.
pub(crate) fn job_dir(file: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(std::path::absolute(file)?
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default())
}

/// Runs `jobs`, read from a job file in `dir`, one after another, as their command lines would,
/// and says how each ended. Sync jobs, and jobs writing outside `dir`, run only when `yes` is
/// given. `stop` stops a synchronization being applied and the jobs after it. Used by `--job` and
/// by *File → Projects and jobs → Run job…*.
pub(crate) fn job_summary(
    jobs: &core::job::JobFile,
    dir: &std::path::Path,
    yes: bool,
    base: &Config,
    stop: &AtomicBool,
) -> JobsReport {
    use core::job::{JobFile, JobKind};
    struct Strict(bool);
    impl Drop for Strict {
        fn drop(&mut self) {
            JOB_OUTPUT.set(self.0);
        }
    }
    let _strict = Strict(JOB_OUTPUT.replace(true));
    let mut summary = JobsReport {
        exit_code: 0,
        jobs: Vec::new(),
        not_run: Vec::new(),
        stopped: false,
    };
    for (i, job) in jobs.jobs.iter().enumerate() {
        let name = job.label(i);
        if stop.load(Ordering::Relaxed) {
            summary.stopped = true;
        }
        if summary.stopped || (summary.exit_code >= 2 && !jobs.continue_on_error) {
            summary.not_run.push(name);
            continue;
        }
        let mut report = JobReport {
            name,
            kind: job.kind.key(),
            exit_code: 2,
            error: None,
            sync: None,
        };
        type Ran = anyhow::Result<(u8, Option<SyncReport>)>;
        // What the job did, and what could not be written back to its servers afterwards.
        let run = || -> anyhow::Result<(Ran, Vec<String>)> {
            let prepared = job.prepare(dir).map_err(anyhow::Error::msg)?;
            let alone = JobFile::single(job.clone());
            if let (Some(out), false) = (alone.outputs_outside(dir).first(), yes) {
                anyhow::bail!(
                    "{}",
                    i18n::trf("job.err_outside", &[("path", out.display().to_string())])
                );
            }
            let argv = std::iter::once(std::ffi::OsString::from("oxidemerge")).chain(prepared.args);
            let mut cli = Cli::try_parse_from(argv).map_err(|e| {
                let text = e.to_string();
                anyhow::anyhow!("{}", text.lines().next().unwrap_or_default())
            })?;
            let mut cfg = base.clone();
            apply_cli_options(&cli, &mut cfg);
            let (spec, rules) = resolve_rules(&cli, dir).map_err(anyhow::Error::msg)?;
            use_rules(&mut cfg, &spec, &rules);
            anyhow::ensure!(
                yes || job.kind != JobKind::Sync,
                "{}",
                i18n::trf(
                    "job.err_needs_yes",
                    &[(
                        "path",
                        core::remote_url::shown(&cli.paths[1].display().to_string())
                    )]
                )
            );
            let staged = stage_remote(&mut cli, stop)?;
            let result = if job.kind == JobKind::Sync {
                sync_now(&cli, &cfg, prepared.allow_large_deletion, stop)
                    .map(|(code, report)| (code, Some(report)))
            } else {
                run_batch(&cli, &cfg).map(|code| (code, None))
            };
            let code = result.as_ref().map_or(2, |(code, _)| *code);
            let journals = core::folder_sync::journal_root();
            let failed = core::remote_url::finish(&staged, code, journals.as_deref(), stop);
            Ok((result, failed))
        };
        let mut errors = Vec::new();
        match run() {
            Ok((result, failed)) => {
                match result {
                    Ok((code, sync)) => {
                        report.exit_code = code;
                        report.sync = sync;
                    }
                    Err(e) => errors.push(format!("{e:#}")),
                }
                if !failed.is_empty() {
                    report.exit_code = 2;
                    errors.extend(failed);
                }
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
        if !errors.is_empty() {
            let text = errors.join("\n");
            eprintln!("error: {}: {text}", report.name);
            report.error = Some(text);
        }
        summary.exit_code = summary.exit_code.max(report.exit_code);
        summary.jobs.push(report);
    }
    summary
}

/// Plans the synchronization of the two folders of `cli` and applies it at once, as a sync job
/// does: refused when it would delete many files unless the job allows it.
fn sync_now(
    cli: &Cli,
    cfg: &Config,
    allow_large: bool,
    stop: &AtomicBool,
) -> anyhow::Result<(u8, SyncReport)> {
    use core::folder_sync::{self, Side, SyncMode, SyncPolicy};
    use std::sync::atomic::AtomicUsize;
    let mode = match cli.sync_plan {
        Some(SyncArg::Update) => SyncMode::Update,
        _ => SyncMode::Mirror,
    };
    let plan = sync_plan(
        cli,
        cfg,
        SyncPolicy {
            mode,
            source: Side::Left,
        },
        cli.sync_links,
    )?;
    anyhow::ensure!(
        allow_large || !plan.large_deletion(),
        "{}",
        i18n::trf(
            "job.err_large_deletion",
            &[("n", plan.deletions().to_string())]
        )
    );
    let journal_root =
        folder_sync::journal_root().ok_or_else(|| anyhow::anyhow!("No settings folder"))?;
    let done = AtomicUsize::new(0);
    let outcome = folder_sync::apply(&plan, &cli.paths, &journal_root, stop, &done);
    Ok(sync_report(outcome))
}

/// The exit code and JSON report of a synchronization's outcome.
fn sync_report(outcome: core::folder_sync::SyncOutcome) -> (u8, SyncReport) {
    use core::folder_sync::SyncOutcome;
    let problem = outcome.problem();
    match outcome {
        SyncOutcome::Applied { changes, kept } => (
            0,
            SyncReport {
                problem,
                status: "applied",
                changes,
                messages: kept,
                journals: Vec::new(),
            },
        ),
        SyncOutcome::NotApplied(_, messages) => (
            2,
            SyncReport {
                problem,
                status: "not_applied",
                changes: 0,
                messages,
                journals: Vec::new(),
            },
        ),
        SyncOutcome::Stopped {
            reason, rollback, ..
        } => (
            if rollback.complete() { 4 } else { 5 },
            SyncReport {
                problem,
                status: if rollback.complete() {
                    "rolled_back"
                } else {
                    "recovery_required"
                },
                changes: rollback.restored,
                messages: reason
                    .into_iter()
                    .chain(
                        rollback
                            .failed
                            .into_iter()
                            .map(|(p, e)| format!("{p}: {e}")),
                    )
                    .collect(),
                journals: rollback.journal.into_iter().collect(),
            },
        ),
    }
}

fn main() -> ExitCode {
    core::remote_url::take_password_env();
    #[cfg(target_os = "linux")]
    wsl_gl_defaults();
    attach_console();
    let matches = <Cli as clap::CommandFactory>::command().get_matches_from(winmerge_args());
    let mut cli =
        <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let mut cfg = Config::load();
    cfg.diff = cfg.comparison_options();
    if core::rules::load_selected(&mut cfg) {
        if let Err(e) = cfg.save() {
            eprintln!("note: {e:#}");
        }
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

    // Subversion's merge-tool-cmd and Perforce's P4MERGE pass only files, in their own order.
    // Any other option means the arguments were written out, and stay as they are.
    let mut merge_result = None;
    if command_line_only_paths(&matches) {
        if let Some(merge) = core::vcs::external_merge(&paths) {
            cli.dl = Some(merge.labels[0].clone());
            cli.dm = Some(merge.labels[1].clone());
            cli.dr = Some(merge.labels[2].clone());
            cli.wl = merge.read_only[0];
            cli.wm = merge.read_only[1];
            cli.wr = merge.read_only[2];
            cli.output = Some(merge.output.clone());
            merge_result = Some(merge.output);
            paths = merge.paths;
        }
    }

    // What each job of --job starts from: the settings, without this command line's options.
    let base_cfg = cfg.clone();
    apply_cli_options(&cli, &mut cfg);
    if let Some(file) = &cli.job {
        let command = <Cli as clap::CommandFactory>::command();
        let others = command.get_arguments().any(|arg| {
            let id = arg.get_id().as_str();
            !matches!(id, "job" | "yes" | "lang")
                && matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine)
        });
        if others {
            eprintln!(
                "error: --job takes no paths and no options but --yes and --lang; the options of \
                 each job are in the file"
            );
            return ExitCode::from(2);
        }
        return match run_jobs(file, cli.yes, &base_cfg) {
            Ok(code) => ExitCode::from(code),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        };
    }
    if cli.git_rev.is_some() && paths.len() != 1 {
        eprintln!("error: --git-rev compares exactly one file or folder with a revision");
        return ExitCode::from(2);
    }

    let never = AtomicBool::new(false);
    cli.paths = paths;
    core::remote::set_asker(Some(std::sync::Arc::new(prompt::ask)));
    let staged = match stage_remote(&mut cli, &never) {
        Ok(staged) => staged,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let paths = cli.paths.clone();

    let cwd = std::env::current_dir().unwrap_or_default();
    let (cli_rules, rules) = match resolve_rules(&cli, &cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    if cli.yes && cli.sync_apply.is_none() {
        eprintln!("error: --yes only confirms --sync-apply or --job");
        return ExitCode::from(2);
    }

    if cli.cli || cli.report.is_some() || cli.patch.is_some() || analysis_mode(&cli) {
        use_rules(&mut cfg, &cli_rules, &rules);
        let mut code = match run_batch(&cli, &cfg) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        };
        // What the command changed in a copy of a path on a server goes back to the server.
        let journals = core::folder_sync::journal_root();
        for e in core::remote_url::finish(&staged, code, journals.as_deref(), &never) {
            eprintln!("error: {e}");
            code = code.max(2);
        }
        return ExitCode::from(code);
    }
    let fetched: Vec<_> = staged
        .into_iter()
        .map(core::remote_url::Staged::into_parts)
        .collect();

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
        // Automated window checks leave the user's window size and place alone.
        persist_window: std::env::var_os("OXIDEMERGE_SMOKE_REPORT").is_none(),
        // A settings folder of its own keeps the window state there too.
        persistence_path: config::config_dir_overridden()
            .then(config::config_dir)
            .flatten()
            .map(|d| d.join("app.ron")),
        ..Default::default()
    };
    // An unchanged result leaves the conflict unresolved. Subversion and Perforce treat an
    // exit code of 1 that way; saving the merge changes the file, and creating it does too.
    let merge_digest = merge_result
        .as_ref()
        .map(|path| core::vcs::file_digest(path));
    let result = eframe::run_native(
        "OxideMerge",
        options,
        Box::new(move |cc| {
            Ok(Box::new(OxideMergeApp::with_fetched(
                cc, cfg, stored, initial, fetched,
            )))
        }),
    );
    match result {
        Ok(()) if ui::app::screenshot_failed() => ExitCode::from(2),
        Ok(())
            if merge_result.as_ref().is_some_and(|path| {
                core::vcs::file_digest(path) == merge_digest.unwrap_or(None)
            }) =>
        {
            ExitCode::from(1)
        }
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_alone_are_the_only_command_a_merge_tool_is_recognized_in() {
        let plain = <Cli as clap::CommandFactory>::command().get_matches_from([
            "oxidemerge",
            "base",
            "theirs",
            "mine",
            "merged",
        ]);
        assert!(command_line_only_paths(&plain));
        let flagged = <Cli as clap::CommandFactory>::command().get_matches_from([
            "oxidemerge",
            "--wl",
            "base",
            "theirs",
            "mine",
            "merged",
        ]);
        assert!(!command_line_only_paths(&flagged));
    }
}
