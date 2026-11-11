//! Web page compare: loads pages in the background, then shows them as text or HTML (text view),
//! Markdown shown formatted, or a downloaded resource tree (folder view). Every mode converts the
//! pages as downloaded; nothing renders them in a browser.

use crate::config::Config;
use crate::core::filter::FltFilter;
use crate::core::text::TextDoc;
use crate::core::web::{self, Page, Progress, WebMode};
use crate::core::web_markdown;
use crate::i18n::{tr, trf};
use crate::ui::app::Content;
use crate::ui::folder_view::FolderView;
use crate::ui::markdown_view::{MarkdownView, Prepared};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{self, Icon, Outcome};
use egui::Ui;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

enum Loaded {
    /// Pages downloaded for the text and HTML modes, with the text shown of each.
    Pages(Arc<Vec<Page>>, Vec<String>),
    Tree(Arc<Vec<Page>>, Vec<tempfile::TempDir>, web::TreeOutcome),
    /// Pages downloaded for the formatted mode, already lined up and compared.
    Formatted(Arc<Vec<Page>>, Box<Prepared>),
}

struct Job {
    rx: Receiver<anyhow::Result<Loaded>>,
    progress: Arc<Progress>,
    started: Instant,
}

pub struct WebView {
    pub urls: Vec<Url>,
    pub mode: WebMode,
    /// Pages fetched for the formatted, HTML, text and resource modes, reused when switching
    /// between them.
    pages: Option<Arc<Vec<Page>>>,
    pub inner: Option<Box<Content>>,
    /// The pages as Markdown, shown formatted instead of an inner view.
    pub formatted: Option<MarkdownView>,
    job: Option<Job>,
    error: Option<String>,
    note: String,
    /// Whether the pages open at their first difference (or the last) once loaded, when
    /// stepped to from another file.
    pub land: Option<bool>,
}

impl Drop for WebView {
    fn drop(&mut self) {
        if let Some(j) = &self.job {
            j.progress.abort.store(true, Ordering::Relaxed);
        }
    }
}

fn short(u: &Url) -> String {
    if u.scheme() == "file" {
        if let Ok(p) = u.to_file_path() {
            return p
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
    }
    let s = u.as_str();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    s.trim_end_matches('/').to_string()
}

fn url_label(u: &Url) -> String {
    if u.scheme() == "file" {
        u.to_file_path()
            .map(|f| f.display().to_string())
            .unwrap_or_else(|_| u.to_string())
    } else {
        u.to_string()
    }
}

fn label(p: &Page) -> String {
    let url = url_label(&p.url);
    if (200..300).contains(&p.status) {
        url
    } else {
        format!("{url}  (HTTP {})", p.status)
    }
}

impl WebView {
    pub fn new(urls: Vec<Url>, cfg: &Config) -> Self {
        let mut v = WebView {
            urls,
            mode: cfg.web_mode,
            pages: None,
            inner: None,
            formatted: None,
            job: None,
            error: None,
            note: String::new(),
            land: None,
        };
        v.start(cfg);
        v
    }

    pub fn title(&self) -> String {
        self.urls.iter().map(short).collect::<Vec<_>>().join(" ↔ ")
    }

    /// Fetches the pages again and redraws the current mode.
    pub fn reload(&mut self, cfg: &Config) {
        self.pages = None;
        self.start(cfg);
    }

    pub fn stop(&mut self) {
        if let Some(j) = self.job.take() {
            j.progress.abort.store(true, Ordering::Relaxed);
            self.error = Some(tr("msg.aborted").into());
        }
    }

    fn start(&mut self, cfg: &Config) {
        if let Some(j) = self.job.take() {
            j.progress.abort.store(true, Ordering::Relaxed);
        }
        self.inner = None;
        self.formatted = None;
        self.error = None;
        self.note.clear();
        let (tx, rx) = channel();
        let progress = Arc::new(Progress::default());
        let urls = self.urls.clone();
        let cached = self.pages.clone();
        let fetching = progress.clone();
        let pages = move || -> anyhow::Result<Arc<Vec<Page>>> {
            match cached {
                Some(p) => Ok(p),
                None => web::fetch_pages(&urls, &fetching.abort).map(Arc::new),
            }
        };
        match self.mode {
            WebMode::Markdown => {
                // The downloaded HTML as Markdown.
                let opts = cfg.comparison_options();
                let converting = progress.clone();
                std::thread::spawn(move || {
                    let r = pages().and_then(|pages| {
                        let abort = &converting.abort;
                        let blocks = pages
                            .iter()
                            .map(|p| web_markdown::formatted_html_to_blocks(&p.html, &p.url, abort))
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| anyhow::anyhow!(tr("msg.aborted")))?;
                        Ok(Loaded::Formatted(
                            pages,
                            Box::new(Prepared::new(blocks, &opts)),
                        ))
                    });
                    let _ = tx.send(r);
                });
            }
            WebMode::Html | WebMode::Text => {
                let mode = self.mode;
                std::thread::spawn(move || {
                    let r = pages().map(|pages| {
                        let texts = pages.iter().map(|p| web::page_text(p, mode)).collect();
                        Loaded::Pages(pages, texts)
                    });
                    let _ = tx.send(r);
                });
            }
            WebMode::Resources => {
                let pr = progress.clone();
                std::thread::spawn(move || {
                    let r = (|| {
                        let pages = pages()?;
                        let temps = (0..pages.len())
                            .map(|_| tempfile::tempdir())
                            .collect::<Result<Vec<_>, _>>()?;
                        let roots: Vec<PathBuf> =
                            temps.iter().map(|t| t.path().to_path_buf()).collect();
                        let failed = web::download_trees(&pages, &roots, &pr);
                        Ok(Loaded::Tree(pages, temps, failed))
                    })();
                    let _ = tx.send(r);
                });
            }
        }
        self.job = Some(Job {
            rx,
            progress,
            started: Instant::now(),
        });
    }

    fn poll(&mut self, cfg: &Config, named: &[FltFilter]) {
        let Some(job) = &self.job else { return };
        let loaded = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("{}", tr("status.error"))),
        };
        self.job = None;
        match loaded.and_then(|l| self.show(l, cfg, named)) {
            Ok(()) => {}
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn show(&mut self, loaded: Loaded, cfg: &Config, named: &[FltFilter]) -> anyhow::Result<()> {
        let content = match loaded {
            Loaded::Formatted(pages, prepared) => {
                let labels = pages.iter().map(label).collect();
                let mut view = MarkdownView::from_prepared(*prepared, labels)
                    .with_page_urls(pages.iter().map(|page| page.url.clone()).collect());
                match self.land.take() {
                    Some(true) => view.first_diff(),
                    Some(false) => view.last_diff(),
                    None => {}
                }
                self.formatted = Some(view);
                self.pages = Some(pages);
                return Ok(());
            }
            Loaded::Pages(pages, texts) => {
                let html = self.mode == WebMode::Html;
                let docs = pages
                    .iter()
                    .zip(&texts)
                    .map(|(p, text)| {
                        let mut d = TextDoc::from_text(None, text);
                        d.read_only = true;
                        d.compare_only = true;
                        d.description = Some(label(p));
                        d.syntax = html.then(|| "html".into());
                        d
                    })
                    .collect();
                self.pages = Some(pages);
                Content::Text(TextView::new(docs, cfg))
            }
            Loaded::Tree(pages, temps, outcome) => {
                let roots = temps.iter().map(|t| t.path().to_path_buf()).collect();
                let display = pages.iter().map(label).collect();
                let mut v = FolderView::new(roots, display, temps, "*.*", named, cfg);
                v.set_recursive(true);
                v.start_compare();
                let notes = [
                    ("web.failed", outcome.failed),
                    ("web.skipped", outcome.skipped),
                ];
                self.note = notes
                    .iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(key, n)| trf(key, &[("n", n.to_string())]))
                    .collect::<Vec<_>>()
                    .join(" · ");
                self.pages = Some(pages);
                Content::Folder(v)
            }
        };
        let mut content = content;
        match (self.land.take(), &mut content) {
            (Some(true), Content::Text(v)) => v.first_diff(),
            (Some(false), Content::Text(v)) => v.last_diff(),
            _ => {}
        }
        self.inner = Some(Box::new(content));
        Ok(())
    }

    fn loading_text(&self) -> String {
        let Some(job) = &self.job else {
            return String::new();
        };
        let secs = job.started.elapsed().as_secs();
        let total = job.progress.total.load(Ordering::Relaxed);
        let msg = if total > 0 {
            trf(
                "web.downloading",
                &[
                    (
                        "done",
                        job.progress.done.load(Ordering::Relaxed).to_string(),
                    ),
                    ("total", total.to_string()),
                ],
            )
        } else {
            tr("web.loading").to_string()
        };
        format!("{msg}  ({secs} s)")
    }

    /// `inner` is the outcome of the view showing the result, when there is one.
    pub fn outcome(&self, inner: Option<Outcome>) -> Option<Outcome> {
        if self.job.is_some() {
            Some(Outcome::Busy)
        } else if self.error.is_some() {
            Some(Outcome::Error)
        } else {
            inner.or_else(|| self.formatted.as_ref().map(MarkdownView::outcome))
        }
    }

    pub fn status_line(&self, inner: Option<String>) -> String {
        if self.job.is_some() {
            return self.loading_text();
        }
        if let Some(e) = &self.error {
            return format!("{}: {e}", tr("status.error"));
        }
        // Pages are compared, never written: the text view says so per pane, the Markdown
        // view here.
        let inner = inner.or_else(|| {
            self.formatted
                .as_ref()
                .map(|m| format!("{}  |  {}", m.status_line(), tr("label.compare_only")))
        });
        match (inner, self.note.is_empty()) {
            (Some(s), true) => s,
            (Some(s), false) => format!("{s}  |  {}", self.note),
            (None, _) => self.note.clone(),
        }
    }

    /// Mode switcher and reload button, then a progress or error message while there is no inner view.
    pub fn header(&mut self, ui: &mut Ui, cfg: &Config, named: &[FltFilter]) {
        let was_loading = self.job.is_some();
        self.poll(cfg, named);
        let mut mode = self.mode;
        let mut reload = false;
        let mut stop = false;
        widgets::toolbar(ui, |ui| {
            let pal = widgets::pal(ui);
            widgets::inline_icon(ui, Icon::Globe, pal.text_dim);
            ui.add_space(4.0);
            let options: Vec<(WebMode, &str)> =
                WebMode::ALL.iter().map(|&m| (m, tr(m.key()))).collect();
            widgets::segmented(ui, &mut mode, &options);
            widgets::toolbar_sep(ui);
            if self.job.is_some() {
                if widgets::icon_button(ui, Icon::Stop, tr("btn.stop")).clicked() {
                    stop = true;
                }
            } else if widgets::icon_button(
                ui,
                Icon::Refresh,
                &widgets::with_key(tr("web.reload"), "F5"),
            )
            .clicked()
            {
                reload = true;
            }
        });
        if mode != self.mode {
            self.mode = mode;
            self.start(cfg);
        } else if reload {
            self.reload(cfg);
        } else if stop {
            self.stop();
        }
        if was_loading || self.job.is_some() {
            // Also repaints once after finishing, as the status bar was drawn before this poll.
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        if self.inner.is_some() || self.formatted.is_some() {
            return;
        }
        let pal = widgets::pal(ui);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.35).max(24.0));
            if let Some(e) = &self.error {
                ui.label(
                    egui::RichText::new(e)
                        .color(pal.danger)
                        .size(crate::ui::theme::size::BODY),
                );
            } else {
                ui.spinner();
                ui.add_space(8.0);
                ui.label(egui::RichText::new(self.loading_text()).color(pal.text_dim));
            }
        });
    }
}
