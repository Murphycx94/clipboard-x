//! The desktops' clipboard (`arboard`): polled on a thread of its own. On
//! macOS items go back on the pasteboard directly, as they are stored.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use arboard::Clipboard;
use image::RgbaImage;

use super::{
    Captured, POLL_INTERVAL, Recorder, SECRET_LIFETIME, Stored, begin_secret_copy, hash_bytes,
    is_latest_secret_copy, optimize_stored, text_capture,
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

/// How often opening the clipboard is tried while another app has it.
#[cfg(target_os = "windows")]
const OPEN_ATTEMPTS: usize = 5;

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
        match clipboard_win::raw::get(id, &mut data) {
            Ok(read) => is_opt_out(&data[..read.min(4)]),
            // There, and not readable: a secret, to be safe.
            Err(_) => true,
        }
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn clipboard_is_concealed() -> bool {
    false
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
/// it holds a secret or neither. An error when another program had the
/// clipboard at that moment: the same content is still to read.
fn read(clipboard: &mut Clipboard) -> Result<Option<Captured>, arboard::Error> {
    if clipboard_is_concealed() {
        return Ok(None);
    }
    match clipboard.get_text() {
        Ok(text) => {
            if let Some(text) = text_capture(text) {
                return Ok(Some(text));
            }
        }
        Err(busy @ arboard::Error::ClipboardOccupied) => return Err(busy),
        // No text, or none that reads: an image perhaps.
        Err(_) => {}
    }
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(busy @ arboard::Error::ClipboardOccupied) => return Err(busy),
        Err(_) => return Ok(None),
    };
    let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height)) else {
        return Ok(None);
    };
    Ok(RgbaImage::from_raw(width, height, image.bytes.into_owned()).map(Captured::Image))
}

/// A number that moves whenever the clipboard's content changes, where the
/// system keeps one: the pasteboard's change count on macOS, the
/// clipboard's sequence number on Windows.
#[cfg(target_os = "macos")]
fn change_count() -> Option<i64> {
    use objc2_app_kit::NSPasteboard;
    i64::try_from(NSPasteboard::generalPasteboard().changeCount()).ok()
}

#[cfg(target_os = "windows")]
fn change_count() -> Option<i64> {
    clipboard_win::seq_num().map(|number| i64::from(number.get()))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn change_count() -> Option<i64> {
    None
}

/// The change count of the pasteboard as we last wrote it ourselves
/// ([`copy_item`]), 0 before: the monitor does not read that back.
#[cfg(target_os = "macos")]
static OWN_WRITE_COUNT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

#[cfg(target_os = "macos")]
fn own_write_count() -> Option<i64> {
    Some(OWN_WRITE_COUNT.load(Ordering::SeqCst)).filter(|&count| count != 0)
}

#[cfg(not(target_os = "macos"))]
fn own_write_count() -> Option<i64> {
    None
}

/// What `read` finds on the clipboard, when its change count (`count`)
/// moved since `seen`: reading costs a copy of the content (an image
/// decoded), not to pay at every poll. Nothing either when the count moved
/// again during the read (the parts read may be of two contents, a secret's
/// among them), when the read failed (the next poll reads again) or when
/// the count is that of our own write (`own`): an item the history put
/// back is not read back. Without a count, every poll reads.
fn read_if_changed<E>(
    seen: &mut Option<i64>,
    count: impl Fn() -> Option<i64>,
    own: impl Fn() -> Option<i64>,
    read: impl FnOnce() -> Result<Option<Captured>, E>,
) -> Option<Captured> {
    let before = count();
    let is_own = || before.is_some() && before == own();
    if before.is_some() && before == *seen {
        return None;
    }
    if is_own() {
        *seen = before;
        return None;
    }
    let captured = read().ok()?;
    if count() != before {
        return None;
    }
    *seen = before;
    // Our write cleared the pasteboard at this count, noted during the read.
    if is_own() {
        return None;
    }
    captured
}

/// A thread that makes the stored images whose hashes it is sent smaller,
/// one at a time, until the sender is dropped.
fn spawn_optimizer(db: Arc<Database>) -> Sender<String> {
    let (optimize, hashes) = mpsc::channel::<String>();
    let spawned = thread::Builder::new()
        .name("image-optimizer".into())
        .spawn(move || {
            for hash in hashes {
                if let Err(e) = optimize_stored(&db, &hash) {
                    log::warn!("cannot optimize a stored image: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        // Images stay as they were first encoded: larger, not wrong.
        log::error!("cannot start the image optimizer thread: {e}");
    }
    optimize
}

/// Polls the clipboard on a thread of its own, storing what changes and
/// calling `on_change` after each store, until `stop` is set. `recorder` is
/// shared with [`copy_item`]'s caller, which marks what the history puts
/// back as seen.
pub fn spawn_monitor(
    db: Arc<Database>,
    recorder: Arc<Mutex<Recorder>>,
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
            let optimize = spawn_optimizer(db.clone());
            let mut seen = None;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
                let read = || {
                    read(&mut clipboard)
                        .inspect_err(|e| log::debug!("the clipboard was not read: {e}"))
                };
                let Some(captured) =
                    read_if_changed(&mut seen, change_count, own_write_count, read)
                else {
                    continue;
                };
                let hash = captured.hash();
                // Decided under the lock, stored outside it: a copy from the
                // history does not wait for an image to be encoded. The
                // recorder holds a hash only, sound after a panic.
                let admitted = recorder
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .admit(&hash);
                if !admitted {
                    continue;
                }
                match captured.store(&db, &hash) {
                    Ok(stored) => {
                        on_change();
                        if stored == Stored::Added && matches!(captured, Captured::Image(_)) {
                            // No optimizer: the image stays as it is.
                            let _ = optimize.send(hash);
                        }
                    }
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
    write(&content)
}

/// Writes `content` on the pasteboard as it is stored: an image as its
/// PNG, neither decoded nor made a TIFF (which took long enough, on the UI
/// thread, to stall the panel). Notes the write's change count first, so
/// the monitor never reads it back.
#[cfg(target_os = "macos")]
fn write(content: &ItemContent) -> Result<(), String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString};
    use objc2_foundation::{NSData, NSString};

    let pasteboard = NSPasteboard::generalPasteboard();
    // Writing after clearing keeps the count clearing gave.
    let count = pasteboard.clearContents();
    OWN_WRITE_COUNT.store(i64::try_from(count).unwrap_or(0), Ordering::SeqCst);
    // SAFETY: AppKit's pasteboard type constants are immutable statics,
    // initialized when AppKit loads.
    let written = unsafe {
        match content {
            ItemContent::Text(text) => {
                pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
            }
            ItemContent::Image(png) => {
                pasteboard.setData_forType(Some(&NSData::with_bytes(png)), NSPasteboardTypePNG)
            }
        }
    };
    if written {
        Ok(())
    } else {
        Err("无法写入剪贴板".to_owned())
    }
}

#[cfg(not(target_os = "macos"))]
fn write(content: &ItemContent) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    match content {
        ItemContent::Text(text) => clipboard.set_text(text.as_str()).map_err(|e| e.to_string()),
        ItemContent::Image(png) => {
            let rgba = image::load_from_memory(png)
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

    fn text() -> Result<Option<Captured>, ()> {
        Ok(Some(Captured::Text("a".into())))
    }

    #[test]
    fn a_read_that_failed_is_tried_again() {
        let mut seen = None;
        assert!(read_if_changed(&mut seen, || Some(3), || None, || Err(())).is_none());
        // The same count: still to read.
        assert!(read_if_changed(&mut seen, || Some(3), || None, text).is_some());
    }

    #[test]
    fn the_clipboard_is_read_only_when_its_count_moved() {
        let count = std::cell::Cell::new(Some(7));
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            text()
        };
        let mut seen = None;
        assert!(read_if_changed(&mut seen, || count.get(), || None, read).is_some());
        assert!(read_if_changed(&mut seen, || count.get(), || None, read).is_none());
        assert_eq!(reads.get(), 1);
        count.set(Some(8));
        assert!(read_if_changed(&mut seen, || count.get(), || None, read).is_some());
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn content_that_changed_while_read_is_read_again() {
        let count = std::cell::Cell::new(Some(1));
        let mut seen = None;
        // Another copy lands between the parts of the read: what was read
        // may be of both, a secret's among them.
        let torn = read_if_changed(
            &mut seen,
            || count.get(),
            || None,
            || {
                count.set(Some(2));
                text()
            },
        );
        assert!(torn.is_none());
        assert!(read_if_changed(&mut seen, || count.get(), || None, text).is_some());
        assert!(read_if_changed(&mut seen, || count.get(), || None, text).is_none());
    }

    #[test]
    fn our_own_write_is_not_read_back() {
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            text()
        };
        let mut seen = Some(4);
        assert!(read_if_changed(&mut seen, || Some(5), || Some(5), read).is_none());
        assert_eq!(reads.get(), 0);
        assert_eq!(seen, Some(5));
        // What another app copies next is read.
        assert!(read_if_changed(&mut seen, || Some(6), || Some(5), read).is_some());
    }

    #[test]
    fn our_own_write_landing_during_a_read_is_dropped() {
        let own = std::cell::Cell::new(None);
        let mut seen = None;
        // The pasteboard was cleared for our write, its count noted only
        // while it was read.
        let captured = read_if_changed(
            &mut seen,
            || Some(9),
            || own.get(),
            || {
                own.set(Some(9));
                text()
            },
        );
        assert!(captured.is_none());
    }

    #[test]
    fn without_a_count_every_poll_reads() {
        let mut seen = None;
        assert!(read_if_changed(&mut seen, || None, || None, text).is_some());
        assert!(read_if_changed(&mut seen, || None, || None, text).is_some());
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
