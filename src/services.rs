//! What the UI and the desktop host share: the database, the clipboard
//! monitor, counters the UI polls (data changed, window shown) and the
//! host's window, behind [`WindowHost`].
//!
//! The UI runs on NSH's engine thread and the host on the main thread, so
//! they meet here through atomics and `Send` callbacks only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::capture;
use crate::db::Database;
use crate::telegram::{self, TelegramTarget};

/// The app's id: its data directory's name.
pub const APP_ID: &str = "com.clipboardx.app";

/// Overrides the data directory (tests, portable installs).
pub const DATA_DIR_ENV: &str = "CLIPBOARDX_DATA_DIR";
/// Set to anything to leave the clipboard alone (tests).
pub const NO_CAPTURE_ENV: &str = "CLIPBOARDX_NO_CAPTURE";

/// The window the app shows in, as the desktop host runs it.
pub trait WindowHost: Send + Sync {
    /// Hides the window.
    fn hide(&self);
    /// Registers `hotkey` as the global shortcut in place of the current one.
    fn set_hotkey(&self, hotkey: &str) -> Result<(), String>;
}

pub struct Services {
    db: Arc<Database>,
    data_dir: PathBuf,
    data_version: Arc<AtomicU64>,
    shown: AtomicU64,
    capture_started: AtomicBool,
    stop_capture: Arc<AtomicBool>,
    host: OnceLock<Box<dyn WindowHost>>,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// The app's services, opened at the first call (the database in the data
/// directory, with expired items removed).
pub fn services() -> &'static Services {
    SERVICES.get_or_init(|| Services::open(&data_dir()))
}

/// The directory the database and the log are in: `$CLIPBOARDX_DATA_DIR`,
/// else the system's application data directory (the Tauri version's).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_ID)
}

impl Services {
    /// Services on the database in `dir`, or on one in memory when it
    /// cannot be opened (logged), so the window still shows.
    pub fn open(dir: &Path) -> Services {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::error!("cannot create {}: {e}", dir.display());
        }
        let db = Database::open(&dir.join("clipboardx.db")).unwrap_or_else(|e| {
            log::error!("cannot open the database in {}: {e}", dir.display());
            Database::in_memory().unwrap_or_else(|e| panic!("cannot open SQLite at all: {e}"))
        });
        let services = Services::new(db, dir.to_owned());
        services.cleanup_expired();
        services
    }

    pub fn new(db: Database, data_dir: PathBuf) -> Services {
        Services {
            db: Arc::new(db),
            data_dir,
            data_version: Arc::new(AtomicU64::new(0)),
            shown: AtomicU64::new(0),
            capture_started: AtomicBool::new(false),
            stop_capture: Arc::new(AtomicBool::new(false)),
            host: OnceLock::new(),
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    // --- Change and visibility counters -----------------------------------

    /// Counts changes to the data; the UI reloads when it moves.
    pub fn data_version(&self) -> u64 {
        self.data_version.load(Ordering::Acquire)
    }

    pub fn notify_changed(&self) {
        self.data_version.fetch_add(1, Ordering::AcqRel);
    }

    /// Counts the times the window was shown; the UI resets its selection
    /// and focuses the search when it moves.
    pub fn shown_count(&self) -> u64 {
        self.shown.load(Ordering::Acquire)
    }

    pub fn window_shown(&self) {
        self.shown.fetch_add(1, Ordering::AcqRel);
    }

    // --- The host's window ------------------------------------------------

    /// Connects the desktop host; only the first one counts.
    pub fn set_host(&self, host: Box<dyn WindowHost>) {
        if self.host.set(host).is_err() {
            log::warn!("a window host is already set");
        }
    }

    /// Whether a host can hide the window (else it is a plain window).
    pub fn has_host(&self) -> bool {
        self.host.get().is_some()
    }

    pub fn hide_window(&self) {
        if let Some(host) = self.host.get() {
            host.hide();
        }
    }

    /// Saves `hotkey` and registers it with the host when there is one.
    pub fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        crate::hotkey::parse(hotkey)?;
        if let Some(host) = self.host.get() {
            host.set_hotkey(hotkey)?;
        }
        self.db.set_hotkey(hotkey).map_err(|e| e.to_string())
    }

    // --- Capture ----------------------------------------------------------

    /// Starts watching the clipboard, once; not when `$CLIPBOARDX_NO_CAPTURE`
    /// is set.
    pub fn start_capture(&self) {
        if std::env::var_os(NO_CAPTURE_ENV).is_some()
            || self.capture_started.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let version = self.data_version.clone();
        capture::spawn_monitor(self.db.clone(), self.stop_capture.clone(), move || {
            version.fetch_add(1, Ordering::AcqRel);
        });
    }

    pub fn copy_item(&self, id: i64) -> Result<(), String> {
        capture::copy_item(&self.db, id)
    }

    // --- Items --------------------------------------------------------------

    /// Favorites an item or takes it out, then syncs the favorites.
    pub fn toggle_favorite(&self, id: i64, note: &str) -> Result<bool, String> {
        let favorite = self
            .db
            .toggle_favorite(id, note)
            .map_err(|e| e.to_string())?;
        self.notify_changed();
        self.sync_favorites();
        Ok(favorite)
    }

    pub fn update_note(&self, id: i64, note: &str) -> Result<(), String> {
        self.db.update_note(id, note).map_err(|e| e.to_string())?;
        self.notify_changed();
        Ok(())
    }

    /// Deletes an item, syncing the favorites when it was one.
    pub fn delete_item(&self, id: i64) -> Result<(), String> {
        let was_favorite = self.db.is_favorite(id).unwrap_or(false);
        self.db.delete_item(id).map_err(|e| e.to_string())?;
        self.notify_changed();
        if was_favorite {
            self.sync_favorites();
        }
        Ok(())
    }

    /// Deletes the non-favorites; how many went.
    pub fn clear_history(&self) -> Result<usize, String> {
        let count = self.db.clear_history().map_err(|e| e.to_string())?;
        self.notify_changed();
        Ok(count)
    }

    pub fn set_retention_days(&self, days: i64) -> Result<(), String> {
        self.db
            .set_retention_days(days)
            .map_err(|e| e.to_string())?;
        self.cleanup_expired();
        Ok(())
    }

    /// Removes what the retention setting no longer keeps.
    pub fn cleanup_expired(&self) {
        let removed = self
            .db
            .settings()
            .and_then(|s| self.db.cleanup_expired(s.retention_days));
        match removed {
            Ok(0) => {}
            Ok(n) => {
                log::info!("removed {n} expired items");
                self.notify_changed();
            }
            Err(e) => log::error!("cannot remove expired items: {e}"),
        }
    }

    /// Sends the favorites to Telegram on a thread of its own, when a bot
    /// is set up.
    pub fn sync_favorites(&self) {
        let settings = match self.db.settings() {
            Ok(settings) if settings.telegram_configured() => settings,
            Ok(_) => return,
            Err(e) => {
                log::error!("cannot read settings: {e}");
                return;
            }
        };
        let favorites = match self.db.favorites_with_content() {
            Ok(favorites) => favorites,
            Err(e) => {
                log::error!("cannot read favorites: {e}");
                return;
            }
        };
        let target = TelegramTarget {
            token: settings.telegram_token,
            chat_id: settings.telegram_chat_id,
        };
        let spawned = std::thread::Builder::new()
            .name("telegram-sync".into())
            .spawn(
                move || match telegram::sync_favorites(&target, &favorites) {
                    Ok(()) => log::info!("Telegram sync success ({} favorites)", favorites.len()),
                    Err(e) => log::error!("Telegram sync failed: {e}"),
                },
            );
        if let Err(e) = spawned {
            log::error!("cannot start the Telegram sync: {e}");
        }
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        self.stop_capture.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn services() -> Services {
        Services::new(Database::in_memory().unwrap(), PathBuf::from("/tmp"))
    }

    #[derive(Default)]
    struct FakeHost {
        hidden: AtomicU64,
        hotkeys: Mutex<Vec<String>>,
    }

    impl WindowHost for Arc<FakeHost> {
        fn hide(&self) {
            self.hidden.fetch_add(1, Ordering::Relaxed);
        }
        fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
            self.hotkeys.lock().unwrap().push(hotkey.to_owned());
            Ok(())
        }
    }

    #[test]
    fn changes_move_the_data_version() {
        let s = services();
        s.db().insert_text("a", "h").unwrap();
        let id = s.db().today_items().unwrap()[0].id;
        let v0 = s.data_version();
        assert!(s.toggle_favorite(id, "n").unwrap());
        s.update_note(id, "m").unwrap();
        s.delete_item(id).unwrap();
        assert_eq!(s.clear_history().unwrap(), 0);
        assert_eq!(s.data_version(), v0 + 4);
    }

    #[test]
    fn hiding_and_hotkeys_go_to_the_host() {
        let s = services();
        s.hide_window();
        assert!(!s.has_host());
        s.set_hotkey("Alt+Space").unwrap();

        let host = Arc::new(FakeHost::default());
        s.set_host(Box::new(host.clone()));
        assert!(s.has_host());
        s.hide_window();
        assert_eq!(host.hidden.load(Ordering::Relaxed), 1);
        s.set_hotkey("Ctrl+Shift+K").unwrap();
        assert_eq!(*host.hotkeys.lock().unwrap(), vec!["Ctrl+Shift+K"]);
        assert_eq!(s.db().settings().unwrap().hotkey, "Ctrl+Shift+K");
    }

    #[test]
    fn invalid_hotkeys_are_not_saved() {
        let s = services();
        assert!(s.set_hotkey("V").is_err());
        assert_eq!(
            s.db().settings().unwrap().hotkey,
            crate::model::DEFAULT_HOTKEY
        );
    }

    #[test]
    fn shown_counter_counts() {
        let s = services();
        s.window_shown();
        s.window_shown();
        assert_eq!(s.shown_count(), 2);
    }

    #[test]
    fn retention_change_cleans_up() {
        let s = services();
        s.db()
            .insert_text_at("old", "h", "2020-01-01T00:00:00")
            .unwrap();
        s.set_retention_days(0).unwrap();
        assert_eq!(s.db().archive_groups().unwrap().len(), 1);
        s.set_retention_days(30).unwrap();
        assert!(s.db().archive_groups().unwrap().is_empty());
    }

    #[test]
    fn opens_in_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let s = Services::open(dir.path());
        assert!(dir.path().join("clipboardx.db").exists());
        assert_eq!(s.data_dir(), dir.path());
    }
}
