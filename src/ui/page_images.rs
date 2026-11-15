//! The pictures of web pages shown in the Formatted compare: loaded away from the window, a few
//! at a time, as [`crate::core::web::image_bytes`] allows, and kept while the app runs.

use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{ColorImage, Context, Id};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

const ID: &str = concat!(module_path!(), "::PageImages");
/// Pictures loaded at once.
const WORKERS: usize = 4;
/// Longest side a picture is kept at: a preview needs no more, and a texture costs memory.
const MAX_SIDE: u32 = 1024;
/// Longest side a picture may have to be decoded at all.
const MAX_DECODE_SIDE: u32 = 16_384;

enum Entry {
    Pending,
    Ready(Arc<ColorImage>),
    Failed(String),
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    queue: VecDeque<String>,
    workers: usize,
}

#[derive(Default)]
pub struct PageImages {
    state: Arc<Mutex<State>>,
}

/// Whether `uri` names a picture this loader loads: one a page converted to Markdown may show.
pub fn supported(uri: &str) -> bool {
    let head = uri.get(..11).unwrap_or(uri).to_ascii_lowercase();
    ["data:image/", "file:", "http://", "https://"]
        .iter()
        .any(|s| head.starts_with(s))
}

/// Adds the loader to `ctx` once.
pub fn install(ctx: &Context) {
    let key = Id::unique(ID);
    if ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(key, true));
    if !ctx.loaders().image.lock().iter().any(|l| l.id() == ID) {
        ctx.add_image_loader(Arc::new(PageImages::default()));
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

fn work(state: Arc<Mutex<State>>, ctx: Context) {
    let never = AtomicBool::new(false);
    loop {
        let Some(uri) = state.lock().ok().and_then(|mut s| {
            let next = s.queue.pop_front();
            if next.is_none() {
                s.workers -= 1;
            }
            next
        }) else {
            return;
        };
        let entry = crate::core::web::image_bytes(&uri, &never)
            .map_err(|e| format!("{e:#}"))
            .and_then(|b| decode(&b))
            .map_or_else(Entry::Failed, |img| Entry::Ready(Arc::new(img)));
        if let Ok(mut s) = state.lock() {
            // Forgotten while it loaded: not kept.
            if s.entries.contains_key(&uri) {
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
            Some(Entry::Failed(e)) => return Err(LoadError::Loading(e.clone())),
            Some(Entry::Pending) => return Ok(ImagePoll::Pending { size: None }),
            None => {}
        }
        s.entries.insert(uri.to_string(), Entry::Pending);
        s.queue.push_back(uri.to_string());
        if s.workers < WORKERS {
            s.workers += 1;
            let (state, ctx) = (self.state.clone(), ctx.clone());
            std::thread::spawn(move || work(state, ctx));
        }
        Ok(ImagePoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.entries.remove(uri);
        }
    }

    fn forget_all(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.entries.clear();
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
    }
}
