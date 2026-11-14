//! The UI's state (signals, all `Copy`) and the actions that change it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use nsh_core::prelude::*;

use crate::model::{ClipboardItem, DateGroup, Settings, ThemePreference, filter_items};
use crate::services::services;
use crate::updater::Release;
use crate::vault::{EntrySummary, VaultStatus};

/// How long the "copied" check stays on a row.
const COPIED_FLASH: Duration = Duration::from_millis(1000);
/// How long a toast shows.
const TOAST_DURATION: Duration = Duration::from_millis(2200);
/// How often the UI looks for changes from the clipboard monitor and the host.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Main,
    Archive,
    Settings,
    Vault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    History,
    Favorites,
}

/// The dialog on top, if any; its data lives in the state's own signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Note,
    Image,
    Telegram,
    Logs,
    Update,
    ConfirmClear,
    VaultEditor,
    VaultConfirmDelete,
    VaultPassword,
    VaultReset,
    /// Setting a new recovery phrase (it shows the phrase).
    VaultRecoveryPhrase,
    /// Resetting a forgotten master password with the phrase.
    VaultRecover,
    SyncSetup,
    SyncSecretKey,
    SyncPair,
    SyncDevices,
    SyncNewPassword,
}

impl DialogKind {
    /// Dialogs that show entries' data, closed when the vault locks.
    pub fn is_vault(self) -> bool {
        matches!(
            self,
            DialogKind::VaultEditor | DialogKind::VaultConfirmDelete | DialogKind::SyncSecretKey
        )
    }
}

/// What the vault password dialog does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultPurpose {
    EnableTouchId,
    ChangePassword,
    Export,
    Import,
    RevealSecretKey,
}

/// Setting up sync on this device: the first one, or one joining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    First,
    Join,
}

/// The sync setup form, kept between its two steps (never the password).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncForm {
    pub url: String,
    pub code: String,
    pub device_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
}

#[derive(Clone, Copy)]
pub struct AppState {
    pub page: Signal<Page>,
    pub tab: Signal<Tab>,
    pub query: Signal<String>,
    pub images_only: Signal<bool>,
    /// The highlighted row of the main list (keyboard and hover).
    pub focused: Signal<usize>,
    /// The row under the mouse, which shows its actions.
    pub hovered: Signal<Option<i64>>,
    /// The row just copied, which shows a check.
    pub copied: Signal<Option<i64>>,
    pub today: Signal<Vec<ClipboardItem>>,
    pub favorites: Signal<Vec<ClipboardItem>>,
    pub archive: Signal<Vec<DateGroup>>,
    /// The archive's expanded days.
    pub open_dates: Signal<Vec<String>>,
    pub settings: Signal<Settings>,
    /// The rows of the main list: the tab's items, filtered.
    pub visible: Memo<Vec<ClipboardItem>>,
    pub dialog: Signal<Option<DialogKind>>,
    pub note_target: Signal<Option<i64>>,
    pub note_draft: Signal<String>,
    pub preview_src: Signal<String>,
    pub logs: Signal<String>,
    pub release: Signal<Option<Release>>,
    pub checking_update: Signal<bool>,
    pub toast: Signal<Option<Toast>>,
    pub list: ElementRef,
    pub search: ElementRef,
    pub vault_status: Signal<VaultStatus>,
    /// The entries' summaries while unlocked (never passwords).
    pub vault_items: Signal<Vec<EntrySummary>>,
    pub vault_query: Signal<String>,
    pub vault_visible: Memo<Vec<EntrySummary>>,
    pub vault_focused: Signal<usize>,
    /// The entry the editor or the delete dialog is about; `None`: a new one.
    pub vault_editing: Signal<Option<String>>,
    /// A slow vault operation (key derivation, Touch ID) is running.
    pub vault_busy: Signal<bool>,
    /// The current second, which TOTP codes follow.
    pub vault_tick: Signal<u64>,
    pub vault_purpose: Signal<VaultPurpose>,
    /// The backup file being imported.
    pub vault_import_text: Signal<String>,
    pub vault_list: ElementRef,
    pub sync_on: Signal<bool>,
    pub sync_server: Signal<String>,
    pub sync_status: Signal<crate::cloud::Status>,
    pub sync_mode: Signal<SyncMode>,
    pub sync_form: Signal<SyncForm>,
    /// The secret key shown while sync is set up or when revealed; cleared
    /// when its dialog closes.
    pub sync_secret: Signal<String>,
    /// The secret key dialog only shows the key (no setup step).
    pub sync_secret_reveal: Signal<bool>,
    pub sync_pair: Signal<Option<clipboardx_sync_proto::PairingCode>>,
    pub sync_devices: Signal<Vec<clipboardx_sync_proto::Device>>,
    /// The new-password prompt was shown for the waiting header.
    pub sync_prompted: Signal<bool>,
}

impl AppState {
    pub fn new() -> Self {
        let tab = signal(Tab::History);
        let query = signal(String::new());
        let images_only = signal(false);
        let today = signal(Vec::new());
        let favorites = signal(Vec::new());
        let vault_items = signal(Vec::<EntrySummary>::new());
        let vault_query = signal(String::new());
        let vault_visible = computed(move || {
            let query = vault_query.get();
            vault_items.with(|items| {
                items
                    .iter()
                    .filter(|i| i.matches(&query))
                    .cloned()
                    .collect()
            })
        });
        let visible = computed(move || {
            let source = match tab.get() {
                Tab::History => today,
                Tab::Favorites => favorites,
            };
            let query = query.get();
            let images_only = images_only.get();
            source.with(|items| filter_items(items, &query, images_only))
        });
        AppState {
            page: signal(Page::Main),
            tab,
            query,
            images_only,
            focused: signal(0),
            hovered: signal(None),
            copied: signal(None),
            today,
            favorites,
            archive: signal(Vec::new()),
            open_dates: signal(Vec::new()),
            settings: signal(Settings::default()),
            visible,
            dialog: signal(None),
            note_target: signal(None),
            note_draft: signal(String::new()),
            preview_src: signal(String::new()),
            logs: signal(String::new()),
            release: signal(None),
            checking_update: signal(false),
            toast: signal(None),
            list: element_ref(),
            search: element_ref(),
            vault_status: signal(services().vault().status()),
            vault_items,
            vault_query,
            vault_visible,
            vault_focused: signal(0),
            vault_editing: signal(None),
            vault_busy: signal(false),
            vault_tick: signal(0),
            vault_purpose: signal(VaultPurpose::Export),
            vault_import_text: signal(String::new()),
            vault_list: element_ref(),
            sync_on: signal(false),
            sync_server: signal(String::new()),
            sync_status: signal(crate::cloud::Status::default()),
            sync_mode: signal(SyncMode::First),
            sync_form: signal(SyncForm::default()),
            sync_secret: signal(String::new()),
            sync_secret_reveal: signal(false),
            sync_pair: signal(None),
            sync_devices: signal(Vec::new()),
            sync_prompted: signal(false),
        }
    }

    // --- Loading -------------------------------------------------------------

    /// Reads everything from the database again.
    pub fn reload(self) {
        let db = services().db();
        let report = |what: &str, e: rusqlite::Error| log::error!("cannot load {what}: {e}");
        batch(move || {
            match db.today_items() {
                Ok(items) => self.today.set(items),
                Err(e) => report("today's items", e),
            }
            match db.favorite_items() {
                Ok(items) => self.favorites.set(items),
                Err(e) => report("favorites", e),
            }
            match db.archive_groups() {
                Ok(groups) => self.archive.set(groups),
                Err(e) => report("the archive", e),
            }
            match db.settings() {
                Ok(settings) => self.settings.set(settings),
                Err(e) => report("settings", e),
            }
            let count = untracked(move || self.visible.with(Vec::len));
            if untracked(move || self.focused.get()) >= count {
                self.focused.set(count.saturating_sub(1));
            }
        });
    }

    /// Reloads when the data changed and resets the panel when the host
    /// showed it, polling the services' counters.
    pub fn watch_services(self) {
        let seen_version = Rc::new(Cell::new(services().data_version()));
        let seen_shown = Rc::new(Cell::new(services().shown_count()));
        set_interval(POLL_INTERVAL, move || {
            let version = services().data_version();
            if version != seen_version.get() {
                seen_version.set(version);
                self.reload();
            }
            let shown = services().shown_count();
            if shown != seen_shown.get() {
                seen_shown.set(shown);
                self.on_shown();
            }
            self.watch_vault();
        });
    }

    fn on_shown(self) {
        // "Today" moves at midnight even when nothing was copied since.
        self.reload();
        self.focused.set(0);
        self.hovered.set(None);
        match self.page.get() {
            Page::Main => {
                self.search.focus();
                self.scroll_to_focused();
            }
            Page::Vault => self.vault_shown(),
            Page::Archive | Page::Settings => {}
        }
    }

    pub fn apply_theme(self) {
        set_color_scheme(match self.settings.get().theme {
            ThemePreference::Light => Appearance::Light,
            ThemePreference::Dark => Appearance::Dark,
            ThemePreference::System => Appearance::System,
        });
    }

    // --- Feedback --------------------------------------------------------------

    pub fn toast(self, kind: ToastKind, message: impl Into<String>) {
        let id = self.toast.get().map_or(1, |t| t.id + 1);
        self.toast.set(Some(Toast {
            id,
            kind,
            message: message.into(),
        }));
        set_timeout(TOAST_DURATION, move || {
            if self.toast.get().is_some_and(|t| t.id == id) {
                self.toast.set(None);
            }
        });
    }

    pub fn success(self, message: impl Into<String>) {
        self.toast(ToastKind::Success, message);
    }

    pub fn error(self, message: impl Into<String>) {
        self.toast(ToastKind::Error, message);
    }

    /// Reports `result`'s error, if any; whether it succeeded.
    pub fn check<T>(self, result: Result<T, String>) -> Option<T> {
        result.map_err(|e| self.error(e)).ok()
    }

    // --- Navigation --------------------------------------------------------------

    /// Opens `page`, or goes back to the main page when it is open.
    pub fn toggle_page(self, page: Page) {
        let next = if self.page.get() == page {
            Page::Main
        } else {
            page
        };
        self.page.set(next);
        if next == Page::Vault {
            self.vault_shown();
        }
    }

    pub fn set_tab(self, tab: Tab) {
        if self.tab.get() != tab {
            self.tab.set(tab);
            self.focused.set(0);
            self.scroll_to_focused();
        }
    }

    pub fn toggle_tab(self) {
        self.set_tab(match self.tab.get() {
            Tab::History => Tab::Favorites,
            Tab::Favorites => Tab::History,
        });
    }

    /// Hides the window, back on the main page for the next time.
    pub fn close_window(self) {
        self.dialog.set(None);
        self.page.set(Page::Main);
        services().hide_window();
    }

    /// Escape: closes the dialog on top, else the window.
    pub fn escape(self) {
        if self.dialog.get().is_some() {
            self.dialog.set(None);
        } else {
            self.close_window();
        }
    }

    /// Whether the main list takes the keyboard (no dialog, main page).
    pub fn list_has_keys(self) -> bool {
        self.page.get() == Page::Main && self.dialog.get().is_none()
    }

    pub fn move_focus(self, delta: isize) {
        if !self.list_has_keys() {
            return;
        }
        let count = self.visible.with(Vec::len);
        if count == 0 {
            return;
        }
        let next = self
            .focused
            .get()
            .saturating_add_signed(delta)
            .min(count - 1);
        self.focused.set(next);
        self.scroll_to_focused();
    }

    fn scroll_to_focused(self) {
        if self.visible.with(Vec::is_empty) {
            return;
        }
        let index = u32::try_from(self.focused.get()).unwrap_or(u32::MAX);
        self.list
            .scroll_to_index(index, percentage(0.5), px(0.0), false);
    }

    // --- Items -------------------------------------------------------------------

    /// Copies the `index`-th row (from 0) and hides the window.
    pub fn copy_row(self, index: usize) {
        if !self.list_has_keys() {
            return;
        }
        if let Some(item) = self.visible.with(|items| items.get(index).cloned()) {
            self.copy(item.id, true);
        }
    }

    /// Puts item `id` on the clipboard; then hides the window, or flashes
    /// a check on the row.
    pub fn copy(self, id: i64, hide: bool) {
        if self.check(services().copy_item(id)).is_none() {
            return;
        }
        if hide {
            self.close_window();
            return;
        }
        self.copied.set(Some(id));
        set_timeout(COPIED_FLASH, move || {
            if self.copied.get() == Some(id) {
                self.copied.set(None);
            }
        });
    }

    pub fn toggle_favorite(self, item: &ClipboardItem) {
        // Leaving the favorites drops the note, as the Tauri version did.
        let note = if item.is_favorite {
            String::new()
        } else {
            item.note.clone().unwrap_or_default()
        };
        if self
            .check(services().toggle_favorite(item.id, &note))
            .is_some()
        {
            self.reload();
        }
    }

    pub fn delete(self, id: i64) {
        if self.check(services().delete_item(id)).is_some() {
            self.reload();
        }
    }

    pub fn open_note(self, item: &ClipboardItem) {
        self.note_target.set(Some(item.id));
        self.note_draft.set(item.note.clone().unwrap_or_default());
        self.dialog.set(Some(DialogKind::Note));
    }

    pub fn save_note(self) {
        let Some(id) = self.note_target.get() else {
            return;
        };
        let note = self.note_draft.get();
        if self
            .check(services().update_note(id, note.trim()))
            .is_some()
        {
            self.dialog.set(None);
            self.reload();
        }
    }

    /// Shows the full image of item `id`, or `thumbnail` when it has none.
    pub fn preview_image(self, id: i64, thumbnail: Option<String>) {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let full = match services().db().item_content(id) {
            Ok(Some(crate::db::ItemContent::Image(png))) => {
                Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
            }
            Ok(_) => None,
            Err(e) => {
                log::error!("cannot read image {id}: {e}");
                None
            }
        };
        let Some(src) = full.or(thumbnail) else {
            return;
        };
        self.preview_src.set(src);
        self.dialog.set(Some(DialogKind::Image));
    }

    pub fn toggle_date(self, date: &str) {
        self.open_dates.update(|dates| {
            if let Some(at) = dates.iter().position(|d| d == date) {
                dates.remove(at);
            } else {
                dates.push(date.to_owned());
            }
        });
    }

    // --- Settings ----------------------------------------------------------------

    pub fn save_hotkey(self, hotkey: &str) -> bool {
        let hotkey = hotkey.trim();
        let saved = self.check(services().set_hotkey(hotkey)).is_some();
        if saved {
            self.reload();
            self.success("快捷键已更新");
        }
        saved
    }

    pub fn set_hide_on_blur(self, hide: bool) {
        let saved = services()
            .db()
            .set_hide_on_blur(hide)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
        }
    }

    pub fn set_hide_from_capture(self, hide: bool) {
        let result = services().set_hide_from_capture(hide);
        // Saved even when the window refused: show what is stored.
        self.reload();
        self.check(result);
    }

    pub fn set_theme(self, theme: ThemePreference) {
        let saved = services().db().set_theme(theme).map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
            self.apply_theme();
        }
    }

    pub fn set_retention(self, days: i64) {
        if self.check(services().set_retention_days(days)).is_some() {
            self.reload();
            self.success("保留时间已更新");
        }
    }

    pub fn clear_history(self) {
        self.dialog.set(None);
        if let Some(count) = self.check(services().clear_history()) {
            self.reload();
            self.success(format!("已清除 {count} 条历史记录"));
        }
    }

    pub fn save_telegram_token(self, token: &str) {
        let token = token.trim();
        let saved = services()
            .db()
            .set_telegram_token(token)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
            self.success("Token 已保存");
        }
    }

    pub fn save_telegram_chat_id(self, chat_id: &str) {
        let chat_id = chat_id.trim();
        let saved = services()
            .db()
            .set_telegram_chat_id(chat_id)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
            self.success("Chat ID 已保存");
        }
    }

    pub fn show_logs(self) {
        self.logs.set(crate::logger::contents());
        self.dialog.set(Some(DialogKind::Logs));
    }

    pub fn check_update(self) {
        if self.checking_update.get() {
            return;
        }
        self.checking_update.set(true);
        spawn(async move {
            let result = blocking(crate::updater::check)
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            self.checking_update.set(false);
            match result {
                Ok(Some(release)) => {
                    self.release.set(Some(release));
                    self.dialog.set(Some(DialogKind::Update));
                }
                Ok(None) => self.success("当前已是最新版本"),
                Err(e) => {
                    log::warn!("update check failed: {e}");
                    self.error("检查更新失败，请稍后重试");
                }
            }
        });
    }
}
