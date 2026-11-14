//! What the vault page does: every call into the vault, and the slow ones
//! (key derivation, Touch ID) off the engine thread.

use nsh_core::file_dialog::{FileFilter, OpenDialog, SaveDialog};
use nsh_core::prelude::*;
use zeroize::Zeroizing;

use crate::services::services;
use crate::vault::{Field, ImportMode, VaultEntry, VaultError, VaultStatus, now_ms};

use super::super::mobile::biometric;
use super::super::state::{AppState, DialogKind, Page, VaultPurpose};

const TOUCH_ID_REASON: &str = "解锁 ClipboardX 密码本";
const BACKUP_FILE_NAME: &str = "ClipboardX.cxvault";
/// The largest backup file read.
const MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024;

fn backup_filter() -> FileFilter {
    FileFilter::new("ClipboardX 备份", ["cxvault"])
}

impl AppState {
    /// Reads the vault's status (and entries when unlocked) again; closes
    /// the dialogs that show entries once it is locked.
    pub fn refresh_vault(self) {
        let vault = services().vault();
        let status = vault.status();
        self.vault_status.set(status);
        self.refresh_sync();
        if status != VaultStatus::Unlocked {
            self.vault_items.set(Vec::new());
            if self.dialog.get().is_some_and(DialogKind::is_vault) {
                self.dialog.set(None);
            }
            return;
        }
        match vault.list() {
            Ok(items) => {
                let count = items.len();
                self.vault_items.set(items);
                if self.vault_focused.get() >= count {
                    self.vault_focused.set(count.saturating_sub(1));
                }
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Each tick: locks an idle vault, follows locks from elsewhere (the
    /// host's sleep and screen-lock observers) and moves the TOTP second.
    pub fn watch_vault(self) {
        let minutes = untracked(move || self.settings.with(|s| s.vault_lock_minutes));
        let vault = services().vault();
        vault.lock_if_idle(now_ms(), minutes);
        // While a vault operation runs it refreshes the page itself when
        // done: switching earlier would show a page that is still busy.
        let busy = untracked(move || self.vault_busy.get());
        if !busy && vault.status() != untracked(move || self.vault_status.get()) {
            self.refresh_vault();
        }
        // Another device changed the master password: ask once.
        let staged = vault.has_staged_header();
        let prompted = untracked(move || self.sync_prompted.get());
        if staged
            && !prompted
            && vault.status() == VaultStatus::Unlocked
            && untracked(move || self.dialog.get()).is_none()
        {
            self.sync_prompted.set(true);
            self.dialog.set(Some(DialogKind::SyncNewPassword));
        } else if !staged && prompted {
            self.sync_prompted.set(false);
        }
        let sync = services().sync_status();
        if untracked(move || self.sync_status.get()) != sync {
            self.sync_status.set(sync);
        }
        let second = now_ms() / 1000;
        if untracked(move || self.vault_tick.get()) != second {
            self.vault_tick.set(second);
        }
    }

    /// The vault page came into view: Touch ID when it is on.
    pub fn vault_shown(self) {
        self.refresh_vault();
        let wants_touch_id = self.settings.with(|s| s.vault_touch_id);
        if self.vault_status.get() == VaultStatus::Locked
            && wants_touch_id
            && services().vault().touch_id_available()
        {
            self.vault_unlock_touch_id();
        }
    }

    /// Runs slow vault `work` on a blocking thread, then `done` with its
    /// result here; reports errors and refreshes the vault after.
    pub(super) fn run_vault<T: Send + 'static>(
        self,
        work: impl FnOnce() -> Result<T, VaultError> + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) {
        if self.vault_busy.get() {
            return;
        }
        self.vault_busy.set(true);
        spawn(async move {
            let result = blocking(work)
                .await
                .unwrap_or_else(|e| Err(VaultError::Storage(e.to_string())));
            self.vault_busy.set(false);
            match result {
                Ok(value) => done(value),
                Err(e) => self.error(e.to_string()),
            }
            self.refresh_vault();
        });
    }

    /// Unlocks with the master password, and the secret key when this
    /// device does not have it yet.
    pub fn vault_unlock(self, password: String, account_key: Option<String>) {
        let password = Zeroizing::new(password);
        let account_key = account_key.map(Zeroizing::new);
        self.run_vault(
            move || match &account_key {
                Some(key) => services()
                    .vault()
                    .unlock_with_account_key(&password, key, now_ms()),
                None => services().vault().unlock(&password, now_ms()),
            },
            |()| services().sync_now(),
        );
    }

    /// Unlocks with Touch ID or the fingerprint; when its key is gone the
    /// setting goes off, so the lock page stops offering it.
    pub fn vault_unlock_touch_id(self) {
        self.run_vault(
            || match services().unlock_vault_with_touch_id(TOUCH_ID_REASON) {
                Err(VaultError::BiometricKeyGone(why)) => {
                    services().db().set_vault_touch_id(false)?;
                    Ok(Some(why))
                }
                unlocked => unlocked.map(|()| None),
            },
            move |gone| match gone {
                None => services().sync_now(),
                Some(why) => {
                    self.reload();
                    self.error(why);
                }
            },
        );
    }

    pub fn vault_lock(self) {
        services().lock_vault();
        self.refresh_vault();
    }

    pub fn vault_copy(self, id: &str, field: Field) {
        let what = match field {
            Field::Username => "账号",
            Field::Password => "密码",
            Field::Totp => "验证码",
        };
        if self.check(services().copy_vault_field(id, field)).is_some() {
            self.success(format!("已复制{what}，30 秒后自动清除"));
        }
    }

    pub fn vault_touch(self) {
        services().vault().touch(now_ms());
    }

    /// Opens the editor on entry `id`, or on a new entry.
    pub fn open_vault_editor(self, id: Option<String>) {
        self.vault_editing.set(id);
        self.dialog.set(Some(DialogKind::VaultEditor));
    }

    pub fn vault_save(self, id: Option<String>, entry: VaultEntry) {
        if self.vault_busy.get() {
            return;
        }
        if entry.title.trim().is_empty() {
            self.error("请填写名称");
            return;
        }
        match services().vault().save(id.as_deref(), entry) {
            Ok(_) => {
                self.dialog.set(None);
                self.refresh_vault();
                services().sync_now();
                self.success("已保存");
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    pub fn vault_delete(self) {
        if self.vault_busy.get() {
            return;
        }
        let Some(id) = self.vault_editing.get() else {
            return;
        };
        if self
            .check(services().vault().delete(&id).map_err(|e| e.to_string()))
            .is_some()
        {
            self.dialog.set(None);
            self.refresh_vault();
            services().sync_now();
            self.success("已删除");
        }
    }

    /// Whether the vault list takes the keyboard.
    pub fn vault_has_keys(self) -> bool {
        self.page.get() == Page::Vault
            && self.dialog.get().is_none()
            && self.vault_status.get() == VaultStatus::Unlocked
    }

    pub fn vault_move_focus(self, delta: isize) {
        let count = self.vault_visible.with(Vec::len);
        if count == 0 {
            return;
        }
        let next = self
            .vault_focused
            .get()
            .saturating_add_signed(delta)
            .min(count - 1);
        self.vault_focused.set(next);
        let index = u32::try_from(next).unwrap_or(u32::MAX);
        self.vault_list
            .scroll_to_index(index, percentage(0.5), px(0.0), false);
    }

    /// The id of the highlighted entry.
    pub fn vault_focused_id(self) -> Option<String> {
        let at = self.vault_focused.get();
        self.vault_visible
            .with(|items| items.get(at).filter(|i| !i.corrupt).map(|i| i.id.clone()))
    }

    // --- Settings ----------------------------------------------------------

    pub fn set_vault_lock_minutes(self, minutes: i64) {
        let saved = services()
            .db()
            .set_vault_lock_minutes(minutes)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.vault_touch();
            self.reload();
        }
    }

    pub fn set_vault_lock_on_hide(self, on: bool) {
        let saved = services()
            .db()
            .set_vault_lock_on_hide(on)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
        }
    }

    pub fn open_vault_password(self, purpose: VaultPurpose) {
        if purpose != VaultPurpose::Import {
            // A backup picked earlier and not imported goes.
            self.vault_import_text.set(String::new());
        }
        self.vault_purpose.set(purpose);
        self.dialog.set(Some(DialogKind::VaultPassword));
    }

    pub fn vault_enable_touch_id(self, password: String) {
        let password = Zeroizing::new(password);
        self.run_vault(
            move || {
                services().vault().enable_touch_id(&password)?;
                Ok(services().db().set_vault_touch_id(true)?)
            },
            move |()| {
                self.dialog.set(None);
                self.reload();
                self.success(format!("已开启{}解锁", biometric()));
            },
        );
    }

    pub fn vault_disable_touch_id(self) {
        services().vault().disable_touch_id();
        let saved = services()
            .db()
            .set_vault_touch_id(false)
            .map_err(|e| e.to_string());
        if self.check(saved).is_some() {
            self.reload();
        }
    }

    pub fn vault_change_password(self, old: String, new: String, confirm: String) {
        if new != confirm {
            self.error("两次输入的新密码不一致");
            return;
        }
        let (old, new) = (Zeroizing::new(old), Zeroizing::new(new));
        self.run_vault(
            move || services().vault().change_password(&old, &new),
            move |()| {
                self.dialog.set(None);
                services().sync_now();
                self.success("主密码已修改");
            },
        );
    }

    pub fn vault_export(self, master: String, password: String, confirm: String) {
        if password != confirm {
            self.error("两次输入的备份密码不一致");
            return;
        }
        let (master, password) = (Zeroizing::new(master), Zeroizing::new(password));
        self.run_vault(
            move || services().vault().export(&master, &password),
            move |text| {
                self.dialog.set(None);
                let text = Zeroizing::new(text);
                let picked = pick_save_path(
                    SaveDialog::new()
                        .file_name(BACKUP_FILE_NAME)
                        .filter(backup_filter())
                        .title("导出密码本备份"),
                );
                spawn(async move {
                    match picked.await {
                        Ok(Some(path)) => match std::fs::write(&path, text.as_bytes()) {
                            Ok(()) => self.success("备份已导出"),
                            Err(e) => {
                                log::error!("cannot write the backup: {e}");
                                self.error("写入备份文件失败");
                            }
                        },
                        Ok(None) => {}
                        Err(e) => self.error(e),
                    }
                });
            },
        );
    }

    /// Picks a backup file, then asks for its password.
    pub fn vault_pick_import(self) {
        if self.vault_status.get() != VaultStatus::Unlocked {
            self.error("请先解锁密码本");
            return;
        }
        let picked = pick_paths(
            OpenDialog::files()
                .filter(backup_filter())
                .title("导入密码本备份"),
        );
        spawn(async move {
            let path = match picked.await {
                Ok(Some(paths)) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(None) => return,
                Err(e) => return self.error(e),
            };
            let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BACKUP_BYTES);
            match std::fs::read_to_string(&path) {
                _ if too_big => self.error("备份文件过大"),
                Ok(text) => {
                    self.vault_import_text.set(text);
                    self.open_vault_password(VaultPurpose::Import);
                }
                Err(_) => self.error("无法读取备份文件"),
            }
        });
    }

    pub fn vault_import(self, password: String, mode: ImportMode) {
        let password = Zeroizing::new(password);
        let text = Zeroizing::new(self.vault_import_text.get());
        self.run_vault(
            move || services().vault().import(&text, &password, mode),
            move |count| {
                self.vault_import_text.set(String::new());
                self.dialog.set(None);
                services().sync_now();
                self.success(format!("已导入 {count} 条"));
            },
        );
    }

    pub fn vault_reset(self) {
        let reset = services().vault().reset().map_err(|e| e.to_string());
        if self.check(reset).is_some() {
            let _ = services().db().set_vault_touch_id(false);
            self.dialog.set(None);
            self.reload();
            self.refresh_vault();
            self.success("密码本已重置");
        }
    }
}
