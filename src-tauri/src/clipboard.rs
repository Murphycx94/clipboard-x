use arboard::Clipboard;
use image::{ImageBuffer, RgbaImage, ImageEncoder};
use image::codecs::png::PngEncoder;
use sha2::{Sha256, Digest};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use crate::database::Database;

pub fn start_clipboard_monitor(db: Arc<Database>) {
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to init clipboard: {e}");
                return;
            }
        };
        let mut last_hash = String::new();

        loop {
            thread::sleep(Duration::from_millis(500));

            // Try text first
            if let Ok(text) = clipboard.get_text() {
                if !text.trim().is_empty() {
                    let hash = hash_bytes(text.as_bytes());
                    if hash != last_hash {
                        last_hash = hash.clone();
                        let _ = db.insert_text(&text, &hash);
                    }
                    continue;
                }
            }

            // Try image
            if let Ok(img) = clipboard.get_image() {
                let hash = hash_bytes(&img.bytes);
                if hash != last_hash {
                    last_hash = hash.clone();
                    if let Some(rgba) = ImageBuffer::from_raw(
                        img.width as u32,
                        img.height as u32,
                        img.bytes.into_owned(),
                    ) as Option<RgbaImage> {
                        let image_data = encode_png(&rgba);
                        let thumbnail = make_thumbnail(&rgba, 128, 80);
                        let _ = db.insert_image(&image_data, &thumbnail, &hash);
                    }
                }
            }
        }
    });
}

fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(&mut buf);
    let _ = encoder.write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    );
    buf
}

fn make_thumbnail(img: &RgbaImage, w: u32, h: u32) -> Vec<u8> {
    let thumb = image::imageops::thumbnail(img, w, h);
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(&mut buf);
    let _ = encoder.write_image(
        thumb.as_raw(),
        thumb.width(),
        thumb.height(),
        image::ExtendedColorType::Rgba8,
    );
    buf
}
