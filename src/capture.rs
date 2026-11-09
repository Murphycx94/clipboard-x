//! Watching the system clipboard and putting items back on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use arboard::Clipboard;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use sha2::{Digest, Sha256};

use crate::db::{Database, ItemContent};

/// How often the clipboard is read.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The box thumbnails fit in, in pixels.
const THUMBNAIL_SIZE: (u32, u32) = (128, 80);

/// What the clipboard holds, read once.
#[derive(Debug, Clone, PartialEq)]
pub enum Captured {
    Text(String),
    Image(RgbaImage),
}

impl Captured {
    /// The hash items are deduplicated by: of the text, or of the pixels.
    pub fn hash(&self) -> String {
        match self {
            Captured::Text(text) => hash_bytes(text.as_bytes()),
            Captured::Image(image) => hash_bytes(image.as_raw()),
        }
    }

    /// Stores it.
    pub fn store(&self, db: &Database, hash: &str) -> Result<(), String> {
        match self {
            Captured::Text(text) => db.insert_text(text, hash).map_err(|e| e.to_string()),
            Captured::Image(image) => {
                let png = encode_png(image)?;
                let thumbnail = encode_png(&make_thumbnail(image))?;
                db.insert_image(&png, &thumbnail, hash)
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// Reads the clipboard: non-blank text first, then an image.
fn read(clipboard: &mut Clipboard) -> Option<Captured> {
    if let Ok(text) = clipboard.get_text()
        && !text.trim().is_empty()
    {
        return Some(Captured::Text(text));
    }
    let image = clipboard.get_image().ok()?;
    RgbaImage::from_raw(
        u32::try_from(image.width).ok()?,
        u32::try_from(image.height).ok()?,
        image.bytes.into_owned(),
    )
    .map(Captured::Image)
}

/// Polls the clipboard on a thread of its own, storing what changes and
/// calling `on_change` after each store, until `stop` is set.
pub fn spawn_monitor(
    db: Arc<Database>,
    stop: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("clipboard-monitor".into())
        .spawn(move || {
            let mut clipboard = match Clipboard::new() {
                Ok(clipboard) => clipboard,
                Err(e) => {
                    log::error!("cannot open the clipboard: {e}");
                    return;
                }
            };
            let mut last_hash = String::new();
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
                let Some(captured) = read(&mut clipboard) else {
                    continue;
                };
                let hash = captured.hash();
                if hash == last_hash {
                    continue;
                }
                match captured.store(&db, &hash) {
                    Ok(()) => on_change(),
                    Err(e) => log::error!("cannot store a clipboard item: {e}"),
                }
                last_hash = hash;
            }
        })
        .unwrap_or_else(|e| panic!("cannot start the clipboard monitor thread: {e}"))
}

/// Puts item `id` back on the system clipboard.
pub fn copy_item(db: &Database, id: i64) -> Result<(), String> {
    let content = db
        .item_content(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "该条目已不存在".to_owned())?;
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    match content {
        ItemContent::Text(text) => clipboard.set_text(text).map_err(|e| e.to_string()),
        ItemContent::Image(png) => {
            let rgba = image::load_from_memory(&png)
                .map_err(|e| e.to_string())?
                .to_rgba8();
            clipboard
                .set_image(arboard::ImageData {
                    width: rgba.width() as usize,
                    height: rgba.height() as usize,
                    bytes: rgba.into_raw().into(),
                })
                .map_err(|e| e.to_string())
        }
    }
}

pub fn hash_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// The image as a losslessly optimized PNG.
pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    Ok(oxipng::optimize_from_memory(&png, &oxipng::Options::default()).unwrap_or(png))
}

/// The image scaled down to fit [`THUMBNAIL_SIZE`].
pub fn make_thumbnail(image: &RgbaImage) -> RgbaImage {
    let (w, h) = THUMBNAIL_SIZE;
    image::imageops::thumbnail(image, w.min(image.width()), h.min(image.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            if (x + y) % 2 == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        })
    }

    #[test]
    fn hashes_are_stable_and_distinct() {
        assert_eq!(hash_bytes(b"a"), hash_bytes(b"a"));
        assert_ne!(hash_bytes(b"a"), hash_bytes(b"b"));
        assert_eq!(Captured::Text("a".into()).hash(), hash_bytes(b"a"));
    }

    #[test]
    fn png_round_trips() {
        let image = checker(10, 6);
        let png = encode_png(&image).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded, image);
    }

    #[test]
    fn thumbnails_fit_the_box_and_never_upscale() {
        let thumb = make_thumbnail(&checker(1280, 400));
        assert!(thumb.width() <= 128 && thumb.height() <= 80);
        let small = make_thumbnail(&checker(20, 10));
        assert!(small.width() <= 20 && small.height() <= 10);
    }

    #[test]
    fn captured_items_are_stored() {
        let db = Database::in_memory().unwrap();
        let text = Captured::Text("hello".into());
        text.store(&db, &text.hash()).unwrap();
        let image = Captured::Image(checker(4, 4));
        image.store(&db, &image.hash()).unwrap();
        let items = db.today_items().unwrap();
        assert_eq!(items.len(), 2);
        let image_item = items.iter().find(|i| i.thumbnail.is_some()).unwrap();
        match db.item_content(image_item.id).unwrap() {
            Some(ItemContent::Image(png)) => {
                assert_eq!(
                    image::load_from_memory(&png).unwrap().to_rgba8(),
                    checker(4, 4)
                );
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn copying_a_missing_item_fails() {
        let db = Database::in_memory().unwrap();
        assert!(copy_item(&db, 42).is_err());
    }
}
