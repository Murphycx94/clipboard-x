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

/// Clipboard formats other apps add to secrets on Windows: their presence
/// alone keeps the content out of clipboard histories.
#[cfg(target_os = "windows")]
const SKIP_FORMATS: [&str; 2] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
];

/// Formats holding a number, 0 for content to keep out of the clipboard
/// history or the cloud clipboard (what Bitwarden sets).
#[cfg(target_os = "windows")]
const OPT_OUT_FORMATS: [&str; 2] = ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"];

/// Whether a format's data (a little-endian number) opts the content out.
#[cfg(target_os = "windows")]
fn is_opt_out(data: &[u8]) -> bool {
    <[u8; 4]>::try_from(data).is_ok_and(|number| u32::from_le_bytes(number) == 0)
}

/// Whether the clipboard holds a secret another app marked (Windows). A
/// clipboard that cannot be opened to look counts as one.
#[cfg(target_os = "windows")]
fn clipboard_is_concealed() -> bool {
    let format = |name: &str| clipboard_win::register_format(name).map(std::num::NonZeroU32::get);
    let marked = SKIP_FORMATS
        .iter()
        .filter_map(|name| format(name))
        .any(clipboard_win::is_format_avail);
    if marked {
        return true;
    }
    let opt_outs: Vec<u32> = OPT_OUT_FORMATS
        .iter()
        .filter_map(|name| format(name))
        .filter(|id| clipboard_win::is_format_avail(*id))
        .collect();
    if opt_outs.is_empty() {
        return false;
    }
    let Ok(_open) = clipboard_win::Clipboard::new_attempts(OPEN_ATTEMPTS) else {
        return true;
    };
    opt_outs.into_iter().any(|id| {
        let mut data = [0xFF_u8; 4];
        clipboard_win::raw::get(id, &mut data).is_ok_and(|read| is_opt_out(&data[..read.min(4)]))
    })
}

/// How often opening the clipboard is tried while another app has it.
#[cfg(target_os = "windows")]
const OPEN_ATTEMPTS: usize = 5;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn clipboard_is_concealed() -> bool {
    false
}

/// A number that moves whenever the clipboard changes, where the system
/// keeps one (Windows): reading the clipboard again is skipped while it
/// stays.
#[cfg(target_os = "windows")]
fn change_count() -> Option<u32> {
    clipboard_win::seq_num().map(std::num::NonZeroU32::get)
}

#[cfg(not(target_os = "windows"))]
fn change_count() -> Option<u32> {
    None
}

/// Starts writing a secret: left out of clipboard histories, and on
/// Windows of the cloud clipboard and of other clipboard monitors too.
#[cfg(target_os = "macos")]
fn secret_set(clipboard: &mut Clipboard) -> arboard::Set<'_> {
    use arboard::SetExtApple as _;
    clipboard.set().exclude_from_history()
}

#[cfg(target_os = "linux")]
fn secret_set(clipboard: &mut Clipboard) -> arboard::Set<'_> {
    use arboard::SetExtLinux as _;
    clipboard.set().exclude_from_history()
}

#[cfg(target_os = "windows")]
fn secret_set(clipboard: &mut Clipboard) -> arboard::Set<'_> {
    use arboard::SetExtWindows as _;
    clipboard
        .set()
        .exclude_from_history()
        .exclude_from_cloud()
        .exclude_from_monitoring()
}

/// Copies `text` marked as a secret (left out of clipboard histories) and
/// clears it after [`SECRET_LIFETIME`] unless something else was copied.
pub fn copy_secret(text: &str) -> Result<(), String> {
    let copy = begin_secret_copy(text);
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    secret_set(&mut clipboard)
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
            // What the clipboard's change count was when it was last read.
            let mut seen = None;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
                let count = change_count();
                if count.is_some() && count == seen {
                    continue;
                }
                let Some(captured) = read(&mut clipboard) else {
                    continue;
                };
                // Read whole: not again until the clipboard changes.
                seen = count;
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

    #[cfg(target_os = "windows")]
    #[test]
    fn a_zero_opts_content_out_of_histories() {
        assert!(is_opt_out(&0_u32.to_le_bytes()));
        assert!(!is_opt_out(&1_u32.to_le_bytes()));
        assert!(!is_opt_out(&[0, 0]));
        assert!(!is_opt_out(&[]));
    }

    #[test]
    fn copying_a_missing_item_fails() {
        let db = Database::in_memory().unwrap();
        assert!(copy_item(&db, 42).is_err());
    }
}
