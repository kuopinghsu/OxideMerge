//! The pictures of web pages shown in the Formatted compare: loaded away from the window, a few
//! at a time, as [`crate::core::web::image_bytes`] allows, and kept while the app runs.
//!
//! Pictures on this computer and in the page load apart from those on the web, so that slow
//! web servers never hold up the others; a web picture that failed is tried again after a
//! while, and forgetting the pictures (a page's Reload) stops the downloads under way.

use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{ColorImage, Context, Id};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const ID: &str = concat!(module_path!(), "::PageImages");
/// Pictures loaded at once: from this computer or the page, and from the web.
const WORKERS: [usize; 2] = [2, 4];
/// How long a picture that failed stays failed before it is tried again: on this computer or
/// in the page, and on the web.
const RETRY: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(60)];
/// Longest side a picture is kept at: a preview needs no more, and a texture costs memory.
const MAX_SIDE: u32 = 1024;
/// Longest side a picture may have to be decoded at all.
const MAX_DECODE_SIDE: u32 = 16_384;

enum Entry {
    Pending,
    Ready(Arc<ColorImage>),
    /// Why it failed, and when it may be tried again.
    Failed(String, Instant),
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Pictures waiting, local ones first and web ones second.
    queues: [VecDeque<String>; 2],
    workers: [usize; 2],
    /// Set to stop the downloads under way; replaced by a new one for those after.
    abort: Arc<AtomicBool>,
    /// How many pictures have failed, so that what is laid out by them is worked out again.
    failures: usize,
}

/// The state of the loader [`install`] adds, which [`failed`] and [`failures`] read.
fn shared() -> &'static Arc<Mutex<State>> {
    static SHARED: OnceLock<Arc<Mutex<State>>> = OnceLock::new();
    SHARED.get_or_init(Arc::default)
}

/// What a web picture's address starts with when it may load from this computer or a private
/// network, as a picture of a page that is there itself: the permission goes with the address
/// the view asks for, so another page naming the same picture does not have it.
const LOCAL: &str = "local+";

/// The address that loads the web picture `uri` of a page on this computer or a private
/// network, where the picture may be too.
pub fn local_uri(uri: &str) -> String {
    format!("{LOCAL}{uri}")
}

/// The picture's own address, and whether it may load from this computer or a private network.
pub fn split_local(uri: &str) -> (&str, bool) {
    match uri.strip_prefix(LOCAL) {
        Some(rest) => (rest, true),
        None => (uri, false),
    }
}

/// How many of the pictures `uris` failed to load the last time they were tried.
pub fn failed_among<'a>(uris: impl Iterator<Item = &'a String>) -> usize {
    shared().lock().map_or(0, |s| {
        uris.filter(|u| matches!(s.entries.get(u.as_str()), Some(Entry::Failed(..))))
            .count()
    })
}

/// Whether the picture `uri` failed to load the last time it was tried.
pub fn failed(uri: &str) -> bool {
    shared()
        .lock()
        .is_ok_and(|s| matches!(s.entries.get(uri), Some(Entry::Failed(..))))
}

/// How many pictures have failed to load so far.
pub fn failures() -> usize {
    shared().lock().map_or(0, |s| s.failures)
}

#[derive(Default)]
pub struct PageImages {
    state: Arc<Mutex<State>>,
}

/// Whether `uri` names a picture this loader loads: one a page converted to Markdown may show.
pub fn supported(uri: &str) -> bool {
    let uri = split_local(uri).0;
    let head = uri.get(..11).unwrap_or(uri).to_ascii_lowercase();
    ["data:image/", "file:", "http://", "https://"]
        .iter()
        .any(|s| head.starts_with(s))
}

/// Whether `uri` is a picture on the web, which loading contacts the server it is on for.
pub fn is_remote(uri: &str) -> bool {
    let uri = split_local(uri).0;
    let head = uri.get(..8).unwrap_or(uri).to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// Adds the loader to `ctx` once.
pub fn install(ctx: &Context) {
    let key = Id::unique(ID);
    if ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(key, true));
    if !ctx.loaders().image.lock().iter().any(|l| l.id() == ID) {
        ctx.add_image_loader(Arc::new(PageImages {
            state: shared().clone(),
        }));
    }
}

/// Decodes `bytes` into a picture no larger than [`MAX_SIDE`] across or down.
fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let mut img = reader.decode().map_err(|e| e.to_string())?;
    if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img = img.thumbnail(MAX_SIDE, MAX_SIDE);
    }
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Loads the pictures of queue `pool` until it is empty.
fn work(state: Arc<Mutex<State>>, ctx: Context, pool: usize) {
    loop {
        let Some((uri, abort)) = state.lock().ok().and_then(|mut s| {
            let next = s.queues[pool].pop_front();
            if next.is_none() {
                s.workers[pool] -= 1;
            }
            next.map(|uri| (uri, s.abort.clone()))
        }) else {
            return;
        };
        let (address, local_ok) = split_local(&uri);
        let entry = crate::core::web::image_bytes(address, local_ok, &abort)
            .map_err(|e| format!("{e:#}"))
            .and_then(|b| decode(&b))
            .map_or_else(
                |e| Entry::Failed(e, Instant::now() + RETRY[pool]),
                |img| Entry::Ready(Arc::new(img)),
            );
        if let Ok(mut s) = state.lock() {
            // Forgotten while it loaded: not kept.
            if !abort.load(Ordering::Relaxed) && s.entries.contains_key(&uri) {
                if matches!(entry, Entry::Failed(..)) {
                    s.failures += 1;
                }
                s.entries.insert(uri, entry);
            }
        }
        ctx.request_repaint();
    }
}

impl ImageLoader for PageImages {
    fn id(&self) -> &str {
        ID
    }

    fn load(&self, ctx: &Context, uri: &str, _: SizeHint) -> ImageLoadResult {
        if !supported(uri) {
            return Err(LoadError::NotSupported);
        }
        let mut s = self
            .state
            .lock()
            .map_err(|e| LoadError::Loading(e.to_string()))?;
        match s.entries.get(uri) {
            Some(Entry::Ready(img)) => return Ok(ImagePoll::Ready { image: img.clone() }),
            Some(Entry::Failed(e, retry)) if Instant::now() < *retry => {
                return Err(LoadError::Loading(e.clone()));
            }
            Some(Entry::Pending) => return Ok(ImagePoll::Pending { size: None }),
            _ => {}
        }
        s.entries.insert(uri.to_string(), Entry::Pending);
        let pool = usize::from(is_remote(uri));
        s.queues[pool].push_back(uri.to_string());
        if s.workers[pool] < WORKERS[pool] {
            s.workers[pool] += 1;
            let (state, ctx) = (self.state.clone(), ctx.clone());
            std::thread::spawn(move || work(state, ctx, pool));
        }
        Ok(ImagePoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.entries.remove(uri);
            for q in &mut s.queues {
                q.retain(|u| u != uri);
            }
        }
    }

    fn forget_all(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.entries.clear();
            s.queues.iter_mut().for_each(VecDeque::clear);
            s.abort.store(true, Ordering::Relaxed);
            s.abort = Arc::default();
        }
    }

    fn byte_size(&self) -> usize {
        self.state.lock().map_or(0, |s| {
            s.entries
                .values()
                .map(|e| match e {
                    Entry::Ready(img) => img.pixels.len() * 4,
                    _ => 0,
                })
                .sum()
        })
    }

    fn has_pending(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|s| s.entries.values().any(|e| matches!(e, Entry::Pending)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_web_pictures_are_tried_again_and_forgetting_stops_loads() {
        let ctx = Context::default();
        let loader = PageImages::default();
        let uri = "https://example.invalid/a.png";
        let past = Instant::now() - Duration::from_secs(1);
        let later = Instant::now() + RETRY[1];
        let set = |e: Entry| {
            loader.state.lock().unwrap().entries.insert(uri.into(), e);
        };
        set(Entry::Failed("down".into(), later));
        assert!(loader.load(&ctx, uri, SizeHint::default()).is_err());
        set(Entry::Failed("down".into(), past));
        assert!(matches!(
            loader.load(&ctx, uri, SizeHint::default()),
            Ok(ImagePoll::Pending { .. })
        ));
        // Forgetting one picture takes it out of its queue too.
        loader.state.lock().unwrap().queues[1].push_back("https://x/b.png".into());
        loader.forget("https://x/b.png");
        assert!(!loader.state.lock().unwrap().queues[1].contains(&"https://x/b.png".to_string()));
        let abort = loader.state.lock().unwrap().abort.clone();
        loader.forget_all();
        assert!(abort.load(Ordering::Relaxed));
        let s = loader.state.lock().unwrap();
        assert!(s.entries.is_empty() && s.queues.iter().all(VecDeque::is_empty));
        assert!(!s.abort.load(Ordering::Relaxed));
        assert!(is_remote("HTTPS://x") && !is_remote("file:///a.png"));
        // Leave to load from a private network goes with the address asked for.
        let local = local_uri("http://intranet/a.png");
        assert_eq!(split_local(&local), ("http://intranet/a.png", true));
        assert_eq!(
            split_local("http://intranet/a.png"),
            ("http://intranet/a.png", false)
        );
        assert!(supported(&local) && is_remote(&local));
    }

    #[test]
    fn only_page_pictures_are_loaded() {
        assert!(supported("data:image/png;base64,AA"));
        assert!(supported("file:///D:/a.png"));
        assert!(supported("HTTPS://example.com/a.png"));
        assert!(!supported("data:text/html,x"));
        assert!(!supported("bytes://a.png"));
        assert!(!supported("javascript:x"));
    }

    #[test]
    fn large_pictures_are_kept_small() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2048, 512, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode(&png).unwrap();
        assert_eq!(img.size, [1024, 256]);
        assert!(decode(b"not a picture").is_err());
    }

    #[test]
    fn a_picture_loads_in_the_background() {
        let ctx = Context::default();
        let loader = PageImages::default();
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([9, 9, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, &png).unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let gone = url::Url::from_file_path(dir.path().join("gone.png"))
            .unwrap()
            .to_string();
        let mut ready = None;
        let mut failed = false;
        for _ in 0..500 {
            if ready.is_none() {
                if let Ok(ImagePoll::Ready { image }) = loader.load(&ctx, &uri, SizeHint::default())
                {
                    ready = Some(image.size);
                }
            }
            failed |= loader.load(&ctx, &gone, SizeHint::default()).is_err();
            if ready.is_some() && failed {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ready, Some([3, 2]));
        assert!(failed);
        assert!(loader.byte_size() >= 24);
        // A missing file is tried again a few seconds later, so one added beside the page shows.
        let s = loader.state.lock().unwrap();
        let Some(Entry::Failed(_, at)) = s.entries.get(&gone) else {
            panic!("not failed");
        };
        assert!(*at <= Instant::now() + RETRY[0]);
    }
}
