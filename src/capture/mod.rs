//! Watching the system clipboard and putting items back on it.
//!
//! What every platform shares lives here: hashing, deduplicating against
//! what was seen last ([`Recorder`]), storing, thumbnails and leaving our
//! own secret copies out. Reading and writing the clipboard is the
//! platform's: `desktop` polls it on a thread of its own (`arboard`); a
//! phone reads it when the app comes to the foreground
//! ([`read_foreground`], NSH's clipboard on the UI thread) and `android`
//! writes it.

use std::time::Duration;

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::{copy_item, copy_secret, spawn_monitor};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{copy_item, copy_secret};

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

/// The hash of the secret copied last and when, which the monitor does not
/// record while it is on the clipboard.
static LAST_SECRET: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// Counts secret copies: only the latest one's timer clears the clipboard.
static SECRET_COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

/// Starts a secret copy of `text`: remembers it so the history leaves it
/// out, and gives the copy's number, which [`is_latest_secret_copy`]
/// checks before clearing the clipboard.
pub(crate) fn begin_secret_copy(text: &str) -> u64 {
    remember_secret(text);
    SECRET_COPIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

/// Whether secret copy `copy` is the latest: an older one's timer leaves
/// the clipboard alone.
#[cfg_attr(target_os = "android", allow(dead_code))] // The Android host clears it (step 4).
pub(crate) fn is_latest_secret_copy(copy: u64) -> bool {
    SECRET_COPIES.load(std::sync::atomic::Ordering::SeqCst) == copy
}

/// Turns what the clipboard holds into history items: stores what differs
/// from what it saw last, except secrets we copied ourselves.
#[derive(Debug, Default)]
pub struct Recorder {
    last_hash: String,
}

impl Recorder {
    pub fn new() -> Recorder {
        Recorder::default()
    }

    /// Stores `captured` unless it is what was seen last or a secret we
    /// copied; whether it stored it.
    pub fn observe(&mut self, db: &Database, captured: &Captured) -> Result<bool, String> {
        let hash = captured.hash();
        if hash == self.last_hash {
            return Ok(false);
        }
        let is_secret = is_recent_secret(&hash);
        // Seen either way: a failed store is not retried on every poll.
        self.last_hash = hash.clone();
        if is_secret {
            return Ok(false);
        }
        captured.store(db, &hash).map(|()| true)
    }
}

/// What the clipboard holds now, read through NSH on the UI thread: its
/// text, unless blank. Phones read it so, when the app comes forward
/// (`ui::foreground`); NSH reads no images on Android.
pub async fn read_foreground() -> Option<Captured> {
    text_capture(nsh_core::prelude::clipboard_string().await)
}

/// Non-blank text as a [`Captured`], else nothing.
pub fn text_capture(text: String) -> Option<Captured> {
    (!text.trim().is_empty()).then_some(Captured::Text(text))
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

    /// The secret copied last is one slot for the process: tests that set
    /// it take turns.
    static SECRET_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn secret_slot() -> std::sync::MutexGuard<'static, ()> {
        SECRET_SLOT.lock().unwrap_or_else(|e| e.into_inner())
    }

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
    fn our_own_secret_copies_are_skipped() {
        let _slot = secret_slot();
        remember_secret("s3cret");
        assert!(is_recent_secret(&hash_bytes(b"s3cret")));
        assert!(!is_recent_secret(&hash_bytes(b"other")));
    }

    #[test]
    fn recorder_stores_new_content_once() {
        let db = Database::in_memory().unwrap();
        let mut recorder = Recorder::new();
        let first = Captured::Text("recorder-first".into());
        assert!(recorder.observe(&db, &first).unwrap());
        assert!(!recorder.observe(&db, &first).unwrap());
        let second = Captured::Text("recorder-second".into());
        assert!(recorder.observe(&db, &second).unwrap());
        assert_eq!(db.today_items().unwrap().len(), 2);
    }

    #[test]
    fn recorder_skips_our_own_secret_copies() {
        let _slot = secret_slot();
        let db = Database::in_memory().unwrap();
        let mut recorder = Recorder::new();
        begin_secret_copy("recorder-secret");
        let secret = Captured::Text("recorder-secret".into());
        assert!(!recorder.observe(&db, &secret).unwrap());
        assert!(db.today_items().unwrap().is_empty());
    }

    #[test]
    fn only_the_latest_secret_copy_clears() {
        let _slot = secret_slot();
        let older = begin_secret_copy("a");
        let newer = begin_secret_copy("b");
        assert!(!is_latest_secret_copy(older));
        assert!(is_latest_secret_copy(newer));
    }

    #[test]
    fn blank_text_is_not_captured() {
        assert_eq!(text_capture("  \n".into()), None);
        assert_eq!(text_capture("x".into()), Some(Captured::Text("x".into())));
    }
}
