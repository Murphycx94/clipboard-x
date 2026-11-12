//! The desktops' clipboard (`arboard`): polled on a thread of its own.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use arboard::Clipboard;
use image::RgbaImage;

use super::{
    Captured, POLL_INTERVAL, Recorder, SECRET_LIFETIME, begin_secret_copy, hash_bytes,
    is_latest_secret_copy, text_capture,
};
use crate::db::{Database, ItemContent};

/// Pasteboard types other apps set on secrets (nspasteboard.org).
#[cfg(target_os = "macos")]
const SKIP_TYPES: [&str; 2] = [
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.TransientType",
];

#[cfg(target_os = "macos")]
fn is_concealed_types<S: AsRef<str>>(types: &[S]) -> bool {
    types.iter().any(|t| SKIP_TYPES.contains(&t.as_ref()))
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
    let copy = begin_secret_copy(text);
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set()
        .exclude_from_history()
        .text(text.to_owned())
        .map_err(|e| e.to_string())?;
    let hash = hash_bytes(text.as_bytes());
    thread::Builder::new()
        .name("secret-clear".into())
        .spawn(move || {
            thread::sleep(SECRET_LIFETIME);
            // A later secret copy has a timer of its own.
            if !is_latest_secret_copy(copy) {
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
    if let Some(text) = clipboard.get_text().ok().and_then(text_capture) {
        return Some(text);
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
            let mut recorder = Recorder::new();
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
                let Some(captured) = read(&mut clipboard) else {
                    continue;
                };
                match recorder.observe(&db, &captured) {
                    Ok(true) => on_change(),
                    Ok(false) => {}
                    Err(e) => log::error!("cannot store a clipboard item: {e}"),
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
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
    fn copying_a_missing_item_fails() {
        let db = Database::in_memory().unwrap();
        assert!(copy_item(&db, 42).is_err());
    }
}
