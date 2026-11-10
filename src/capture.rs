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
/// The box thumbnails are cropped to, in pixels: the list shows them at
/// 64x40 points, so this is sharp at 3x.
const THUMBNAIL_SIZE: (u32, u32) = (192, 120);
/// The way thumbnails are made; older ones are made again once.
const THUMBNAIL_VERSION: i64 = 2;

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

/// How long a copied secret stays on the clipboard.
pub const SECRET_LIFETIME: Duration = Duration::from_secs(30);

/// Pasteboard types other apps set on secrets (nspasteboard.org).
const SKIP_TYPES: [&str; 2] = [
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.TransientType",
];

/// The hash of the secret copied last and when, which the monitor does not
/// record while it is on the clipboard.
static LAST_SECRET: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// Counts secret copies: only the latest one's timer clears the clipboard.
static SECRET_COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn is_concealed_types<S: AsRef<str>>(types: &[S]) -> bool {
    types.iter().any(|t| SKIP_TYPES.contains(&t.as_ref()))
}

fn remember_secret(text: &str) {
    if let Ok(mut last) = LAST_SECRET.lock() {
        *last = Some((hash_bytes(text.as_bytes()), std::time::Instant::now()));
    }
}

/// Whether `hash` is the secret copied in the last [`SECRET_LIFETIME`]
/// (with a margin for the monitor's poll).
fn is_recent_secret(hash: &str) -> bool {
    let window = SECRET_LIFETIME + POLL_INTERVAL * 4;
    LAST_SECRET.lock().is_ok_and(|last| {
        last.as_ref()
            .is_some_and(|(h, at)| h == hash && at.elapsed() < window)
    })
}

/// Whether the clipboard holds a secret another app marked (macOS).
#[cfg(target_os = "macos")]
fn clipboard_is_concealed() -> bool {
    use objc2_app_kit::NSPasteboard;
    let types = NSPasteboard::generalPasteboard().types();
    let names: Vec<String> = types
        .map(|t| t.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    is_concealed_types(&names)
}

#[cfg(not(target_os = "macos"))]
fn clipboard_is_concealed() -> bool {
    false
}

/// Copies `text` marked as a secret (left out of clipboard histories) and
/// clears it after [`SECRET_LIFETIME`] unless something else was copied.
pub fn copy_secret(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    use arboard::SetExtApple as _;
    #[cfg(target_os = "linux")]
    use arboard::SetExtLinux as _;
    #[cfg(target_os = "windows")]
    use arboard::SetExtWindows as _;
    remember_secret(text);
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set()
        .exclude_from_history()
        .text(text.to_owned())
        .map_err(|e| e.to_string())?;
    let hash = hash_bytes(text.as_bytes());
    let copy = SECRET_COPIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    thread::Builder::new()
        .name("secret-clear".into())
        .spawn(move || {
            thread::sleep(SECRET_LIFETIME);
            // A later secret copy has a timer of its own.
            if SECRET_COPIES.load(std::sync::atomic::Ordering::SeqCst) != copy {
                return;
            }
            if let Ok(mut clipboard) = Clipboard::new()
                && clipboard
                    .get_text()
                    .map(zeroize::Zeroizing::new)
                    .is_ok_and(|t| hash_bytes(t.as_bytes()) == hash)
            {
                let _ = clipboard.clear();
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Reads the clipboard: non-blank text first, then an image; nothing when
/// it holds a secret.
fn read(clipboard: &mut Clipboard) -> Option<Captured> {
    if clipboard_is_concealed() {
        return None;
    }
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
                if is_recent_secret(&hash) {
                    last_hash = hash;
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

/// The image scaled down, keeping its proportions, until it just covers
/// [`THUMBNAIL_SIZE`], then cropped to it around its center; never scaled
/// up.
pub fn make_thumbnail(image: &RgbaImage) -> RgbaImage {
    let (box_w, box_h) = THUMBNAIL_SIZE;
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return image.clone();
    }
    let scale = (f64::from(box_w) / f64::from(w))
        .max(f64::from(box_h) / f64::from(h))
        .min(1.0);
    let scaled_w = ((f64::from(w) * scale).round() as u32).max(1);
    let scaled_h = ((f64::from(h) * scale).round() as u32).max(1);
    let scaled = if scale < 1.0 {
        image::imageops::thumbnail(image, scaled_w, scaled_h)
    } else {
        image.clone()
    };
    let (crop_w, crop_h) = (box_w.min(scaled_w), box_h.min(scaled_h));
    image::imageops::crop_imm(
        &scaled,
        (scaled_w - crop_w) / 2,
        (scaled_h - crop_h) / 2,
        crop_w,
        crop_h,
    )
    .to_image()
}

/// Makes the thumbnails of stored images again from their full images when
/// they were made an older way (squeezed to the box); how many it made.
/// Once: it records the version when done.
pub fn regenerate_thumbnails(db: &Database) -> Result<usize, String> {
    if db.thumbnail_version().map_err(|e| e.to_string())? >= THUMBNAIL_VERSION {
        return Ok(0);
    }
    let mut count = 0;
    for id in db.image_ids().map_err(|e| e.to_string())? {
        let Ok(Some(ItemContent::Image(png))) = db.item_content(id) else {
            continue;
        };
        let thumbnail = image::load_from_memory(&png)
            .map_err(|e| e.to_string())
            .and_then(|image| encode_png(&make_thumbnail(&image.to_rgba8())));
        match thumbnail {
            Ok(thumbnail) => {
                db.set_thumbnail(id, &thumbnail)
                    .map_err(|e| e.to_string())?;
                count += 1;
            }
            Err(e) => log::warn!("cannot remake the thumbnail of image {id}: {e}"),
        }
    }
    db.set_thumbnail_version(THUMBNAIL_VERSION)
        .map_err(|e| e.to_string())?;
    Ok(count)
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

    /// Vertical stripes `stripe` pixels wide, alternating black and white.
    fn stripes(w: u32, h: u32, stripe: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, _| {
            if (x / stripe).is_multiple_of(2) {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        })
    }

    #[test]
    fn thumbnails_cover_the_box_without_distortion() {
        // 1000x200 (5:1) scales by 0.6 to 600x120, then crops to 192x120:
        // 100-pixel stripes become 60-pixel ones, the same both ways.
        let thumb = make_thumbnail(&stripes(1000, 200, 100));
        assert_eq!(thumb.dimensions(), THUMBNAIL_SIZE);
        let x_scale = 0.6;
        let stripe = (100.0 * x_scale) as u32;
        // The crop starts at (600 - 192) / 2 = 204: inside a white stripe
        // (180..240), which ends 36 pixels in, then 60 black.
        let dark = |x: u32| thumb.get_pixel(x, 60)[0] < 128;
        assert!(!dark(10) && dark(36 + stripe / 2) && !dark(36 + stripe + 10));
    }

    #[test]
    fn tall_images_are_cropped_not_squeezed() {
        let thumb = make_thumbnail(&checker(300, 3000));
        assert_eq!(thumb.dimensions(), THUMBNAIL_SIZE);
    }

    #[test]
    fn small_images_are_kept_as_they_are() {
        let small = make_thumbnail(&checker(20, 10));
        assert_eq!(small.dimensions(), (20, 10));
        let narrow = make_thumbnail(&checker(100, 400));
        assert_eq!(narrow.dimensions(), (100, 120));
    }

    #[test]
    fn regenerating_fixes_old_thumbnails_once() {
        let db = Database::in_memory().unwrap();
        let png = encode_png(&stripes(1000, 200, 100)).unwrap();
        let squeezed = encode_png(&checker(128, 80)).unwrap();
        db.insert_image(&png, &squeezed, "h").unwrap();
        assert_eq!(regenerate_thumbnails(&db).unwrap(), 1);
        let id = db.today_items().unwrap()[0].id;
        let thumb = db.thumbnail(id).unwrap().unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert_eq!((decoded.width(), decoded.height()), THUMBNAIL_SIZE);
        // Done: nothing to redo.
        assert_eq!(regenerate_thumbnails(&db).unwrap(), 0);
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
    fn concealed_type_names_are_recognized() {
        assert!(is_concealed_types(&[
            "public.utf8-plain-text",
            "org.nspasteboard.ConcealedType"
        ]));
        assert!(is_concealed_types(&["org.nspasteboard.TransientType"]));
        assert!(!is_concealed_types(&["public.utf8-plain-text"]));
    }

    #[test]
    fn our_own_secret_copies_are_skipped() {
        remember_secret("s3cret");
        assert!(is_recent_secret(&hash_bytes(b"s3cret")));
        assert!(!is_recent_secret(&hash_bytes(b"other")));
    }

    #[test]
    fn copying_a_missing_item_fails() {
        let db = Database::in_memory().unwrap();
        assert!(copy_item(&db, 42).is_err());
    }
}
