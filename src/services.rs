//! What the UI and the desktop host share: the database, the clipboard
//! monitor, counters the UI polls (data changed, window shown) and the
//! host's window, behind [`WindowHost`].
//!
//! The UI runs on NSH's engine thread and the host on the main thread, so
//! they meet here through atomics and `Send` callbacks only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::capture;
use crate::db::Database;
use crate::system_prompt::{Phase, SystemPrompts};
use crate::telegram::{self, TelegramTarget};
use crate::vault::keychain::{self, KeyStore};
use crate::vault::{self, KdfParams, VaultError, VaultService};

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
    /// Keeps the window out of screenshots, recordings and screen sharing
    /// (`hidden`), or lets them show it.
    fn set_hidden_from_capture(&self, hidden: bool);
    /// Gives the keyboard back to the window if it is shown: a system
    /// prompt (Touch ID) had it.
    fn take_keyboard(&self);
}

/// What the host's window does when it loses the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blur {
    /// Hides: the user went elsewhere.
    Hide,
    /// Stays: a system prompt of ours has the keyboard, or the user keeps
    /// the window up.
    Stay,
    /// Stays and takes the keyboard back: a system prompt just closed and
    /// the system handed the keyboard to another app.
    TakeBack,
}

pub struct Services {
    db: Arc<Database>,
    data_dir: PathBuf,
    data_version: Arc<AtomicU64>,
    shown: AtomicU64,
    #[cfg_attr(target_os = "android", allow(dead_code))]
    capture_started: AtomicBool,
    #[cfg_attr(target_os = "android", allow(dead_code))]
    stop_capture: Arc<AtomicBool>,
    recorder: Mutex<capture::Recorder>,
    host: OnceLock<Box<dyn WindowHost>>,
    prompts: SystemPrompts,
    vault: VaultService,
    sync: crate::cloud::Worker,
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
    platform_data_dir()
}

#[cfg(not(target_os = "android"))]
fn platform_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_ID)
}

/// The app's private `files` directory, from the system; failing that
/// guessed, as an Android process is named after its package.
#[cfg(target_os = "android")]
fn platform_data_dir() -> PathBuf {
    match crate::android::files_dir() {
        Ok(dir) => return dir,
        Err(e) => log::error!("cannot ask Android for the files directory: {e}"),
    }
    let package = std::fs::read("/proc/self/cmdline")
        .ok()
        .and_then(|cmdline| android_package(&cmdline))
        .unwrap_or_else(|| APP_ID.to_owned());
    PathBuf::from("/data/data").join(package).join("files")
}

/// The package of an Android process's `cmdline`: its first argument, up
/// to a `:` (a process of the app's other than its main one).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_package(cmdline: &[u8]) -> Option<String> {
    let first = cmdline.split(|&b| b == 0).next()?;
    let name = std::str::from_utf8(first).ok()?;
    let package = name.split(':').next()?.trim();
    (!package.is_empty() && !package.contains('/')).then(|| package.to_owned())
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
        services.upgrade_thumbnails();
        services
    }

    /// Remakes thumbnails made an older way, on a thread of its own; the UI
    /// reloads when they are done.
    fn upgrade_thumbnails(&self) {
        let db = self.db.clone();
        let version = self.data_version.clone();
        let spawned = std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || match capture::regenerate_thumbnails(&db) {
                Ok(0) => {}
                Ok(n) => {
                    log::info!("remade {n} thumbnails");
                    version.fetch_add(1, Ordering::AcqRel);
                }
                Err(e) => log::error!("cannot remake thumbnails: {e}"),
            });
        if let Err(e) = spawned {
            log::error!("cannot start remaking thumbnails: {e}");
        }
    }

    pub fn new(db: Database, data_dir: PathBuf) -> Services {
        let keys = keychain::platform(&data_dir);
        Services::with_vault(db, data_dir, keys, KdfParams::DEFAULT)
    }

    /// Services whose vault keeps its Touch ID key in `keys` and derives
    /// keys with `params` (tests use cheap ones).
    pub fn with_vault(
        db: Database,
        data_dir: PathBuf,
        keys: Box<dyn KeyStore>,
        params: KdfParams,
    ) -> Services {
        let db = Arc::new(db);
        Services {
            vault: VaultService::with_params(db.clone(), keys, params),
            db,
            data_dir,
            data_version: Arc::new(AtomicU64::new(0)),
            shown: AtomicU64::new(0),
            capture_started: AtomicBool::new(false),
            stop_capture: Arc::new(AtomicBool::new(false)),
            recorder: Mutex::new(capture::Recorder::new()),
            host: OnceLock::new(),
            prompts: SystemPrompts::default(),
            sync: crate::cloud::Worker::default(),
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

    /// What the host's window does now that it lost the keyboard: hides
    /// if the user wants (`hide_on_blur`), unless a system prompt of ours
    /// took the keyboard.
    pub fn window_blurred(&self) -> Blur {
        self.window_blurred_at(Instant::now())
    }

    fn window_blurred_at(&self, now: Instant) -> Blur {
        match self.prompts.phase(now) {
            Phase::Open => Blur::Stay,
            Phase::Settling => Blur::TakeBack,
            Phase::Idle => {
                let hide = self.db.settings().map(|s| s.hide_on_blur).unwrap_or(true);
                if hide { Blur::Hide } else { Blur::Stay }
            }
        }
    }

    /// A system prompt of ours is about to take the keyboard, until the
    /// returned value is dropped: the window stays up meanwhile and takes
    /// the keyboard back after.
    fn system_prompt(&self) -> SystemPrompt<'_> {
        self.prompts.begin();
        SystemPrompt { services: self }
    }

    /// Saves `hotkey` and registers it with the host when there is one.
    #[cfg(not(target_os = "android"))]
    pub fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        crate::hotkey::parse(hotkey)?;
        if let Some(host) = self.host.get() {
            host.set_hotkey(hotkey)?;
        }
        self.db.set_hotkey(hotkey).map_err(|e| e.to_string())
    }

    /// Android has no global shortcuts.
    #[cfg(target_os = "android")]
    pub fn set_hotkey(&self, _hotkey: &str) -> Result<(), String> {
        Err("安卓不支持全局快捷键".to_owned())
    }

    // --- Screen captures ----------------------------------------------------

    /// Whether the window can be kept out of screenshots, recordings and
    /// screen sharing: Android's activity, or a desktop host's own window
    /// (NSH's standard window cannot).
    pub fn can_hide_from_capture(&self) -> bool {
        cfg!(target_os = "android") || self.has_host()
    }

    /// Saves whether the window is kept out of screen captures, and does it.
    pub fn set_hide_from_capture(&self, hide: bool) -> Result<(), String> {
        self.db
            .set_hide_from_capture(hide)
            .map_err(|e| e.to_string())?;
        self.apply_capture_protection()
    }

    /// Keeps the window out of screen captures or lets them show it, as the
    /// settings say; out of them when the settings cannot be read.
    pub fn apply_capture_protection(&self) -> Result<(), String> {
        let hide = self
            .db
            .settings()
            .map(|s| s.hide_from_capture)
            .unwrap_or_else(|e| {
                log::error!("cannot read the settings, hiding from screen captures: {e}");
                true
            });
        #[cfg(target_os = "android")]
        crate::android::set_secure(hide)?;
        #[cfg(not(target_os = "android"))]
        if let Some(host) = self.host.get() {
            host.set_hidden_from_capture(hide);
        }
        Ok(())
    }

    // --- The vault ----------------------------------------------------------

    pub fn vault(&self) -> &VaultService {
        &self.vault
    }

    /// Starts syncing the vault in the background (while it is unlocked and
    /// sync is on); once.
    pub fn start_sync_worker(&'static self) {
        self.sync
            .start(move || &self.vault, move || self.notify_changed());
    }

    /// Syncs the vault soon.
    pub fn sync_now(&self) {
        self.sync.sync_now();
    }

    pub fn sync_status(&self) -> crate::cloud::Status {
        self.sync.status()
    }

    /// Copies a field of a vault entry as a secret: left out of clipboard
    /// histories and cleared after 30 seconds.
    pub fn copy_vault_field(&self, id: &str, field: vault::Field) -> Result<(), String> {
        let text = self.vault.secret(id, field).map_err(|e| e.to_string())?;
        capture::copy_secret(&text)
    }

    /// Unlocks the vault with Touch ID or the fingerprint. Blocking: the
    /// system asks the user, and its prompt has the keyboard meanwhile.
    pub fn unlock_vault_with_touch_id(&self, reason: &str) -> Result<(), VaultError> {
        let _prompt = self.system_prompt();
        self.vault.unlock_with_touch_id(reason)
    }

    /// The host hid the window: locks the vault when the user wants it.
    pub fn window_hidden(&self) {
        if self.db.settings().is_ok_and(|s| s.vault_lock_on_hide) {
            self.vault.lock();
        }
    }

    /// Locks the vault (sleep, screen lock).
    pub fn lock_vault(&self) {
        self.vault.lock();
    }

    // --- Capture ----------------------------------------------------------

    /// Starts watching the clipboard, once; not when `$CLIPBOARDX_NO_CAPTURE`
    /// is set. On Android nothing watches it: the UI reads it when the app
    /// comes forward and hands it to [`Services::record_clipboard`].
    pub fn start_capture(&self) {
        #[cfg(not(target_os = "android"))]
        if self.captures() && !self.capture_started.swap(true, Ordering::AcqRel) {
            let version = self.data_version.clone();
            capture::spawn_monitor(self.db.clone(), self.stop_capture.clone(), move || {
                version.fetch_add(1, Ordering::AcqRel);
            });
        }
    }

    /// Whether the app records the clipboard: not when
    /// `$CLIPBOARDX_NO_CAPTURE` is set.
    pub fn captures(&self) -> bool {
        std::env::var_os(NO_CAPTURE_ENV).is_none()
    }

    /// Stores what the clipboard holds unless it is what was recorded last
    /// (or a secret we copied); whether it stored it. The UI tells when the
    /// item list changed.
    pub fn record_clipboard(&self, captured: &capture::Captured) -> Result<bool, String> {
        let mut recorder = self
            .recorder
            .lock()
            .map_err(|_| "剪贴板记录器不可用".to_owned())?;
        let stored = recorder.observe(&self.db, captured)?;
        if stored {
            self.notify_changed();
        }
        Ok(stored)
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

/// A system prompt of ours, up until dropped.
struct SystemPrompt<'a> {
    services: &'a Services,
}

impl Drop for SystemPrompt<'_> {
    fn drop(&mut self) {
        self.services.prompts.end(Instant::now());
        if let Some(host) = self.services.host.get() {
            host.take_keyboard();
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
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::system_prompt::SETTLE;

    #[test]
    fn android_package_is_the_process_name_up_to_a_colon() {
        assert_eq!(
            android_package(b"com.clipboardx.app\0").as_deref(),
            Some("com.clipboardx.app")
        );
        assert_eq!(
            android_package(b"com.clipboardx.app:sync\0x\0").as_deref(),
            Some("com.clipboardx.app")
        );
        assert_eq!(android_package(b"/system/bin/app_process\0"), None);
        assert_eq!(android_package(b""), None);
    }

    #[test]
    fn recording_the_clipboard_stores_new_text_and_notifies() {
        let s = services();
        let before = s.data_version();
        let text = capture::Captured::Text("from the foreground".into());
        assert!(s.record_clipboard(&text).unwrap());
        assert!(s.data_version() > before);
        // The same content again, as when the app comes forward twice.
        let after = s.data_version();
        assert!(!s.record_clipboard(&text).unwrap());
        assert_eq!(s.data_version(), after);
        assert_eq!(s.db().today_items().unwrap().len(), 1);
    }

    fn services() -> Services {
        Services::with_vault(
            Database::in_memory().unwrap(),
            PathBuf::from("/tmp"),
            Box::new(keychain::MemoryKeyStore::default()),
            KdfParams::INSECURE_TEST,
        )
    }

    #[test]
    fn hiding_the_window_locks_the_vault_when_wanted() {
        let s = services();
        s.vault().setup("correct horse battery").unwrap();
        s.db().set_vault_lock_on_hide(false).unwrap();
        s.window_hidden();
        assert_eq!(s.vault().status(), vault::VaultStatus::Unlocked);
        s.db().set_vault_lock_on_hide(true).unwrap();
        s.window_hidden();
        assert_eq!(s.vault().status(), vault::VaultStatus::Locked);
        s.vault()
            .unlock("correct horse battery", vault::now_ms())
            .unwrap();
        s.lock_vault();
        assert_eq!(s.vault().status(), vault::VaultStatus::Locked);
    }

    #[derive(Default)]
    struct FakeHost {
        hidden: AtomicU64,
        hotkeys: Mutex<Vec<String>>,
        capture_hidden: Mutex<Vec<bool>>,
        keyboard_taken: AtomicU64,
    }

    impl WindowHost for Arc<FakeHost> {
        fn hide(&self) {
            self.hidden.fetch_add(1, Ordering::Relaxed);
        }
        fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
            self.hotkeys.lock().unwrap().push(hotkey.to_owned());
            Ok(())
        }
        fn set_hidden_from_capture(&self, hidden: bool) {
            self.capture_hidden.lock().unwrap().push(hidden);
        }
        fn take_keyboard(&self) {
            self.keyboard_taken.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A key store whose Touch ID prompt stays up until the test answers.
    struct PromptingKeys {
        items: keychain::MemoryKeyStore,
        shown: Mutex<mpsc::Sender<()>>,
        answer: Mutex<mpsc::Receiver<()>>,
    }

    impl KeyStore for PromptingKeys {
        fn available(&self) -> bool {
            true
        }
        fn put(&self, item: keychain::KeyItem, bytes: &[u8]) -> Result<(), String> {
            self.items.put(item, bytes)
        }
        fn get(&self, item: keychain::KeyItem) -> Option<zeroize::Zeroizing<Vec<u8>>> {
            self.items.get(item)
        }
        fn get_with_auth(
            &self,
            item: keychain::KeyItem,
            reason: &str,
        ) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
            self.shown.lock().unwrap().send(()).unwrap();
            self.answer
                .lock()
                .unwrap()
                .recv_timeout(PROMPT_WAIT)
                .map_err(|e| e.to_string())?;
            self.items.get_with_auth(item, reason)
        }
        fn delete(&self, item: keychain::KeyItem) {
            self.items.delete(item);
        }
    }

    /// The most a test waits for the other thread.
    const PROMPT_WAIT: Duration = Duration::from_secs(30);

    #[test]
    fn losing_the_keyboard_hides_the_window_when_wanted() {
        let s = services();
        assert_eq!(s.window_blurred(), Blur::Hide);
        s.db().set_hide_on_blur(false).unwrap();
        assert_eq!(s.window_blurred(), Blur::Stay);
    }

    #[test]
    fn the_touch_id_prompt_keeps_the_window_up_and_gives_the_keyboard_back() {
        const MASTER: &str = "correct horse battery";
        let (shown, prompt_shown) = mpsc::channel();
        let (answer_prompt, answer) = mpsc::channel();
        let s = Services::with_vault(
            Database::in_memory().unwrap(),
            PathBuf::from("/tmp"),
            Box::new(PromptingKeys {
                items: keychain::MemoryKeyStore::with_auth(),
                shown: Mutex::new(shown),
                answer: Mutex::new(answer),
            }),
            KdfParams::INSECURE_TEST,
        );
        let host = Arc::new(FakeHost::default());
        s.set_host(Box::new(host.clone()));
        s.vault().setup(MASTER).unwrap();
        s.vault().enable_touch_id(MASTER).unwrap();
        s.lock_vault();
        let before = Instant::now();

        std::thread::scope(|scope| {
            let unlocking = scope.spawn(|| s.unlock_vault_with_touch_id("解锁"));
            prompt_shown.recv_timeout(PROMPT_WAIT).unwrap();
            // The prompt has the keyboard: the window stays, so the vault
            // is not locked under the unlock (`vault_lock_on_hide`).
            assert_eq!(s.window_blurred(), Blur::Stay);
            assert_eq!(host.keyboard_taken.load(Ordering::Relaxed), 0);
            answer_prompt.send(()).unwrap();
            unlocking.join().unwrap().unwrap();
        });

        assert_eq!(s.vault().status(), vault::VaultStatus::Unlocked);
        assert_eq!(host.keyboard_taken.load(Ordering::Relaxed), 1);
        // As the prompt closes the system moves the keyboard once more.
        let settling = before + SETTLE - Duration::from_millis(1);
        assert_eq!(s.window_blurred_at(settling), Blur::TakeBack);
        // Later the user going elsewhere hides the window again.
        assert_eq!(s.window_blurred_at(Instant::now() + SETTLE), Blur::Hide);
    }

    #[test]
    fn a_canceled_touch_id_prompt_gives_the_keyboard_back_too() {
        let s = Services::with_vault(
            Database::in_memory().unwrap(),
            PathBuf::from("/tmp"),
            Box::new(keychain::MemoryKeyStore::without_auth()),
            KdfParams::INSECURE_TEST,
        );
        let host = Arc::new(FakeHost::default());
        s.set_host(Box::new(host.clone()));
        s.vault().setup("correct horse battery").unwrap();
        s.lock_vault();

        assert!(s.unlock_vault_with_touch_id("解锁").is_err());

        assert_eq!(host.keyboard_taken.load(Ordering::Relaxed), 1);
        assert_eq!(s.window_blurred_at(Instant::now() + SETTLE), Blur::Hide);
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
    fn hiding_from_capture_is_saved_and_goes_to_the_host() {
        let s = services();
        // No window of ours yet: saved for the host to come.
        assert!(!s.can_hide_from_capture());
        s.set_hide_from_capture(false).unwrap();
        assert!(!s.db().settings().unwrap().hide_from_capture);

        let host = Arc::new(FakeHost::default());
        s.set_host(Box::new(host.clone()));
        assert!(s.can_hide_from_capture());
        s.apply_capture_protection().unwrap();
        s.set_hide_from_capture(true).unwrap();
        assert_eq!(*host.capture_hidden.lock().unwrap(), vec![false, true]);
        assert!(s.db().settings().unwrap().hide_from_capture);
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
