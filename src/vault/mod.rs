//! The password vault: everything secret lives under this module.

pub mod backup;
pub mod crypto;
pub mod entry;
pub mod error;
pub mod generator;
pub mod keychain;
pub mod store;
pub mod totp;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::db::Database;
pub use crypto::KdfParams;
use crypto::{
    SALT_LEN, Sealed, SecretKey, derive_key, open, random_bytes, seal, unwrap_key, wrap_key,
};
pub use entry::{EntrySummary, VaultEntry};
pub use error::VaultError;
use keychain::KeyStore;
use store::{EntryRow, MetaRow};

pub const MIN_PASSWORD_CHARS: usize = 10;
const FORMAT_VERSION: i64 = 1;
/// Failures before waiting starts.
const FREE_TRIES: u32 = 3;
const MAX_WAIT_MS: u64 = 60_000;
/// What the key check seals, and its associated data.
const KEY_CHECK: &[u8] = b"clipboardx/vault/key-check/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    NotSetUp,
    Locked,
    Unlocked,
}

/// What an import does with the vault's entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Adds new entries; of two copies of one entry keeps the newer.
    Merge,
    /// Replaces every entry with the backup's.
    Replace,
}

/// Which secret to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Username,
    Password,
    Totp,
}

/// How long to wait after `failed_tries` wrong passwords.
pub fn backoff_ms(failed_tries: u32) -> u64 {
    if failed_tries < FREE_TRIES {
        return 0;
    }
    1_000_u64
        .checked_shl(failed_tries - FREE_TRIES)
        .unwrap_or(MAX_WAIT_MS)
        .min(MAX_WAIT_MS)
}

/// Unix time in ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}

pub struct VaultService {
    db: Arc<Database>,
    keys: Box<dyn KeyStore>,
    params: KdfParams,
    /// The data key while unlocked.
    dek: Mutex<Option<SecretKey>>,
    last_activity_ms: AtomicU64,
    /// Moves on every lock: an unlock that started before a lock and ends
    /// after it (a slow key derivation, a Touch ID prompt) must not unlock.
    lock_generation: AtomicU64,
}

impl VaultService {
    pub fn new(db: Arc<Database>, keys: Box<dyn KeyStore>) -> VaultService {
        VaultService::with_params(db, keys, KdfParams::DEFAULT)
    }

    pub fn with_params(
        db: Arc<Database>,
        keys: Box<dyn KeyStore>,
        params: KdfParams,
    ) -> VaultService {
        VaultService {
            db,
            keys,
            params,
            dek: Mutex::new(None),
            last_activity_ms: AtomicU64::new(0),
            lock_generation: AtomicU64::new(0),
        }
    }

    fn key(&self) -> MutexGuard<'_, Option<SecretKey>> {
        self.dek.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn meta(&self) -> Result<Option<MetaRow>, VaultError> {
        Ok(self.db.with_conn(store::load_meta)?)
    }

    fn generation(&self) -> u64 {
        self.lock_generation.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> VaultStatus {
        match self.meta() {
            Ok(None) => VaultStatus::NotSetUp,
            _ if self.key().is_some() => VaultStatus::Unlocked,
            _ => VaultStatus::Locked,
        }
    }

    /// Creates the vault with `password` and leaves it unlocked.
    pub fn setup(&self, password: &str) -> Result<(), VaultError> {
        if self.meta()?.is_some() {
            return Err(VaultError::AlreadySetUp);
        }
        check_strength(password)?;
        let generation = self.generation();
        // A Touch ID key of an earlier vault must not outlive it (the key
        // check would refuse it anyway).
        self.keys.delete();
        let dek = SecretKey::random()?;
        let meta = self.new_meta(password, &dek)?;
        self.db.with_conn(|c| store::save_meta(c, &meta))?;
        self.open_session(dek, now_ms(), generation)
    }

    /// The vault's metadata for `password` and data key `dek`.
    fn new_meta(&self, password: &str, dek: &SecretKey) -> Result<MetaRow, VaultError> {
        let salt = random_bytes::<SALT_LEN>()?;
        let kek = derive_key(password, &salt, self.params)?;
        Ok(MetaRow {
            version: FORMAT_VERSION,
            params: self.params,
            salt: salt.to_vec(),
            wrapped_dek: wrap_key(&kek, dek)?,
            key_check: key_check(dek)?,
            failed_tries: 0,
            locked_until_ms: 0,
        })
    }

    /// Unlocks with `dek` unless the vault was locked since `generation`.
    fn open_session(&self, dek: SecretKey, now: u64, generation: u64) -> Result<(), VaultError> {
        let mut key = self.key();
        if self.generation() != generation {
            return Err(VaultError::Interrupted);
        }
        *key = Some(dek);
        drop(key);
        self.touch(now);
        Ok(())
    }

    /// The data key `password` unwraps, behind the backoff: every attempt
    /// counts (recorded before the slow check, so quitting mid-check does
    /// not skip it) until one succeeds.
    fn verify(&self, password: &str, now: u64) -> Result<(MetaRow, SecretKey), VaultError> {
        let mut meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        if now < meta.locked_until_ms {
            return Err(VaultError::TooManyAttempts {
                wait_ms: meta.locked_until_ms - now,
            });
        }
        let tries = meta.failed_tries.saturating_add(1);
        self.db
            .with_conn(|c| store::set_failures(c, tries, now + backoff_ms(tries)))?;
        let kek = derive_key(password, &meta.salt, meta.params)?;
        let dek = unwrap_key(&kek, &meta.wrapped_dek).map_err(|_| VaultError::WrongPassword)?;
        meta.failed_tries = 0;
        meta.locked_until_ms = 0;
        if meta.key_check.is_empty() {
            // A vault made before the key check had one.
            meta.key_check = key_check(&dek)?;
        }
        self.db.with_conn(|c| store::save_meta(c, &meta))?;
        Ok((meta, dek))
    }

    pub fn unlock(&self, password: &str, now: u64) -> Result<(), VaultError> {
        let generation = self.generation();
        let (_, dek) = self.verify(password, now)?;
        self.open_session(dek, now, generation)
    }

    pub fn lock(&self) {
        let mut key = self.key();
        self.lock_generation.fetch_add(1, Ordering::SeqCst);
        *key = None;
    }

    /// Records activity (resets the idle timer).
    pub fn touch(&self, now: u64) {
        self.last_activity_ms.store(now, Ordering::Relaxed);
    }

    /// Locks after `minutes` without activity; whether it locked now.
    pub fn lock_if_idle(&self, now: u64, minutes: i64) -> bool {
        let idle = now.saturating_sub(self.last_activity_ms.load(Ordering::Relaxed));
        let limit = u64::try_from(minutes.max(1)).unwrap_or(1) * 60_000;
        if idle < limit || self.key().is_none() {
            return false;
        }
        self.lock();
        true
    }

    /// Runs `f` with the unlocked key, counting as activity.
    fn with_key<R>(
        &self,
        f: impl FnOnce(&SecretKey) -> Result<R, VaultError>,
    ) -> Result<R, VaultError> {
        self.touch(now_ms());
        self.with_key_passive(f)
    }

    /// Runs `f` with the unlocked key without resetting the idle timer
    /// (what the UI reads by itself, such as TOTP codes ticking).
    fn with_key_passive<R>(
        &self,
        f: impl FnOnce(&SecretKey) -> Result<R, VaultError>,
    ) -> Result<R, VaultError> {
        let key = self.key();
        f(key.as_ref().ok_or(VaultError::Locked)?)
    }

    fn load(&self, key: &SecretKey, id: &str) -> Result<VaultEntry, VaultError> {
        let row = self
            .db
            .with_conn(|c| store::load_entry(c, id))?
            .ok_or(VaultError::NotFound)?;
        Self::decrypt(key, &row)
    }

    fn decrypt(key: &SecretKey, row: &EntryRow) -> Result<VaultEntry, VaultError> {
        let sealed =
            Sealed::from_parts(&row.nonce, &row.ciphertext).map_err(|_| VaultError::Corrupt)?;
        let plain = open(key, &sealed, row.id.as_bytes()).map_err(|_| VaultError::Corrupt)?;
        serde_json::from_slice(&plain).map_err(|_| VaultError::Corrupt)
    }

    fn encrypt(key: &SecretKey, id: &str, entry: &VaultEntry) -> Result<EntryRow, VaultError> {
        let json =
            zeroize::Zeroizing::new(serde_json::to_vec(entry).map_err(|_| VaultError::Crypto)?);
        let sealed = seal(key, &json, id.as_bytes())?;
        Ok(EntryRow {
            id: id.to_owned(),
            nonce: sealed.nonce.to_vec(),
            ciphertext: sealed.ciphertext,
            updated_at: entry.updated_at.clone(),
        })
    }

    /// Every entry's summary, sorted by title.
    pub fn list(&self) -> Result<Vec<EntrySummary>, VaultError> {
        self.with_key(|key| {
            let rows = self.db.with_conn(store::load_entries)?;
            let mut list: Vec<EntrySummary> = rows
                .iter()
                .map(|row| {
                    Self::decrypt(key, row)
                        .map_or_else(|_| EntrySummary::corrupt(&row.id), |e| e.summary(&row.id))
                })
                .collect();
            list.sort_by_key(|s| s.title.to_lowercase());
            Ok(list)
        })
    }

    pub fn get(&self, id: &str) -> Result<VaultEntry, VaultError> {
        self.with_key(|key| self.load(key, id))
    }

    /// Saves `entry` as a new entry (`id` `None`) or over `id`; its id.
    pub fn save(&self, id: Option<&str>, mut entry: VaultEntry) -> Result<String, VaultError> {
        if !entry.totp.trim().is_empty() {
            totp::validate(&entry.totp).map_err(VaultError::InvalidTotp)?;
        }
        self.with_key(|key| {
            let id = id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
            let now = timestamp();
            if entry.created_at.is_empty() {
                entry.created_at = now.clone();
            }
            entry.updated_at = now;
            let row = Self::encrypt(key, &id, &entry)?;
            self.db.with_conn(|c| store::upsert_entry(c, &row))?;
            Ok(id)
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), VaultError> {
        self.with_key(|_| Ok(self.db.with_conn(|c| store::delete_entry(c, id))?))
    }

    /// The text of `field` of entry `id`, for the clipboard.
    pub fn secret(&self, id: &str, field: Field) -> Result<zeroize::Zeroizing<String>, VaultError> {
        let entry = self.get(id)?;
        let text = match field {
            Field::Username => entry.username.clone(),
            Field::Password => entry.password.clone(),
            Field::Totp => {
                totp::current(&entry.totp)
                    .map_err(VaultError::InvalidTotp)?
                    .code
            }
        };
        Ok(zeroize::Zeroizing::new(text))
    }

    /// The current code of entry `id`'s TOTP; not activity.
    pub fn totp_code(&self, id: &str) -> Result<totp::TotpCode, VaultError> {
        let entry = self.with_key_passive(|key| self.load(key, id))?;
        totp::current(&entry.totp).map_err(VaultError::InvalidTotp)
    }

    /// Sets a new master password and a new data key: every entry is
    /// encrypted again, so an old copy of the database and the old password
    /// open none of them. The vault is left unlocked.
    pub fn change_password(&self, old: &str, new: &str) -> Result<(), VaultError> {
        check_strength(new)?;
        let generation = self.generation();
        let (_, old_dek) = self.verify(old, now_ms())?;
        let new_dek = SecretKey::random()?;
        let rows = self
            .db
            .with_conn(store::load_entries)?
            .into_iter()
            .map(|row| match Self::decrypt(&old_dek, &row) {
                Ok(entry) => Self::encrypt(&new_dek, &row.id, &entry),
                // A corrupt entry stays as it was.
                Err(_) => Ok(row),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let meta = self.new_meta(new, &new_dek)?;
        self.db
            .with_conn(|c| store::save_meta_and_entries(c, &meta, &rows))?;
        // A Touch ID copy follows the new key, or goes.
        if self.keys.has_key() && self.keys.save(&new_dek).is_err() {
            self.keys.delete();
        }
        // Locked meanwhile: the password changed all the same.
        let _ = self.open_session(new_dek, now_ms(), generation);
        Ok(())
    }

    /// Every entry, encrypted with `backup_password`; needs the master
    /// password again.
    pub fn export(&self, master: &str, backup_password: &str) -> Result<String, VaultError> {
        check_strength(backup_password)?;
        let (_, dek) = self.verify(master, now_ms())?;
        let rows = self.db.with_conn(store::load_entries)?;
        let entries = rows
            .iter()
            .map(|row| {
                let entry = Self::decrypt(&dek, row)?;
                Ok(backup::BackupEntry {
                    id: row.id.clone(),
                    updated_at: entry.updated_at.clone(),
                    entry,
                })
            })
            .collect::<Result<Vec<_>, VaultError>>()?;
        backup::encode(&entries, backup_password, self.params)
    }

    /// Imports a backup, atomically; how many entries it added or changed.
    /// Merge keeps, of two copies of an entry, the one changed last.
    pub fn import(
        &self,
        text: &str,
        password: &str,
        mode: ImportMode,
    ) -> Result<usize, VaultError> {
        if self.key().is_none() {
            return Err(VaultError::Locked);
        }
        let entries = backup::decode(text, password)?;
        if mode == ImportMode::Replace && entries.is_empty() {
            return Err(VaultError::InvalidBackup(
                "备份中没有条目，未做任何更改".into(),
            ));
        }
        self.with_key(|key| {
            self.db.with_conn(|c| {
                let tx = c.unchecked_transaction()?;
                let existing = store::load_entries(&tx)?;
                let rows: Vec<EntryRow> = entries
                    .iter()
                    .filter(|b| match mode {
                        ImportMode::Replace => true,
                        ImportMode::Merge => existing
                            .iter()
                            .find(|r| r.id == b.id)
                            .is_none_or(|r| r.updated_at < b.entry.updated_at),
                    })
                    .map(|b| Self::encrypt(key, &b.id, &b.entry))
                    .collect::<Result<_, _>>()?;
                if mode == ImportMode::Replace {
                    tx.execute("DELETE FROM vault_entries", [])?;
                }
                for row in &rows {
                    store::upsert_entry(&tx, row)?;
                }
                tx.commit()?;
                Ok(rows.len())
            })
        })
    }

    pub fn touch_id_available(&self) -> bool {
        self.keys.available()
    }

    /// Keeps a copy of the data key for Touch ID; needs the password.
    pub fn enable_touch_id(&self, password: &str) -> Result<(), VaultError> {
        let (_, dek) = self.verify(password, now_ms())?;
        self.keys.save(&dek).map_err(VaultError::Keychain)
    }

    pub fn disable_touch_id(&self) {
        self.keys.delete();
    }

    /// Unlocks with the stored key after the system asks the user; only
    /// this vault's key (its key check) opens it. Blocking.
    pub fn unlock_with_touch_id(&self, reason: &str) -> Result<(), VaultError> {
        let generation = self.generation();
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        if meta.key_check.is_empty() {
            return Err(VaultError::Keychain("请先用主密码解锁一次".into()));
        }
        let dek = self.keys.load(reason).map_err(VaultError::Keychain)?;
        if !matches_check(&dek, &meta.key_check) {
            return Err(VaultError::Keychain(
                "Touch ID 密钥已失效，请用主密码解锁".into(),
            ));
        }
        self.open_session(dek, now_ms(), generation)
    }

    /// Deletes the vault and its Touch ID key.
    pub fn reset(&self) -> Result<(), VaultError> {
        self.lock();
        self.keys.delete();
        Ok(self.db.with_conn(store::wipe)?)
    }
}

/// A known value sealed under the data key: tells this vault's key from
/// any other.
fn key_check(dek: &SecretKey) -> Result<Vec<u8>, VaultError> {
    Ok(seal(dek, KEY_CHECK, KEY_CHECK)?.to_bytes())
}

fn matches_check(dek: &SecretKey, check: &[u8]) -> bool {
    Sealed::from_bytes(check)
        .and_then(|sealed| open(dek, &sealed, KEY_CHECK))
        .is_ok_and(|plain| plain.as_slice() == KEY_CHECK)
}

fn check_strength(password: &str) -> Result<(), VaultError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(VaultError::WeakPassword);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::vault::keychain::MemoryKeyStore;

    pub(crate) fn service() -> VaultService {
        VaultService::with_params(
            Arc::new(Database::in_memory().unwrap()),
            Box::new(MemoryKeyStore::default()),
            KdfParams::INSECURE_TEST,
        )
    }

    pub(crate) fn entry(title: &str, password: &str) -> VaultEntry {
        let mut e = VaultEntry::default();
        e.title = title.into();
        e.username = "me".into();
        e.password = password.into();
        e
    }

    #[test]
    fn setup_unlock_lock() {
        let v = service();
        assert_eq!(v.status(), VaultStatus::NotSetUp);
        assert_eq!(v.setup("short").unwrap_err(), VaultError::WeakPassword);
        v.setup("correct horse battery").unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
        assert_eq!(
            v.setup("correct horse battery").unwrap_err(),
            VaultError::AlreadySetUp
        );
        v.lock();
        assert_eq!(v.status(), VaultStatus::Locked);
        assert_eq!(v.list().unwrap_err(), VaultError::Locked);
        v.unlock("correct horse battery", 0).unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
    }

    #[test]
    fn crud_and_no_plaintext_on_disk() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("GitHub", "hunter2-PLAIN")).unwrap();
        let other = v.save(None, entry("Mail", "x")).unwrap();
        assert_eq!(v.list().unwrap().len(), 2);
        let mut e = v.get(&id).unwrap();
        e.password = "changed-PLAIN".into();
        v.save(Some(&id), e).unwrap();
        assert_eq!(v.get(&id).unwrap().password, "changed-PLAIN");
        v.delete(&other).unwrap();
        assert_eq!(v.list().unwrap().len(), 1);
        let raw: Vec<Vec<u8>> = v.db.with_conn(|c| {
            let mut s = c.prepare("SELECT ciphertext FROM vault_entries").unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        });
        for blob in raw {
            let text = String::from_utf8_lossy(&blob);
            assert!(!text.contains("PLAIN") && !text.contains("GitHub"));
        }
    }

    #[test]
    fn wrong_passwords_back_off_and_persist() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.lock();
        assert_eq!(
            v.unlock("nope-nope-1", 1_000).unwrap_err(),
            VaultError::WrongPassword
        );
        assert_eq!(
            v.unlock("nope-nope-2", 1_000).unwrap_err(),
            VaultError::WrongPassword
        );
        assert_eq!(
            v.unlock("nope-nope-3", 1_000).unwrap_err(),
            VaultError::WrongPassword
        );
        // Third failure: wait 1 s, even with the right password.
        assert_eq!(
            v.unlock("correct horse battery", 1_500).unwrap_err(),
            VaultError::TooManyAttempts { wait_ms: 500 }
        );
        v.unlock("correct horse battery", 2_000).unwrap();
        assert_eq!(
            v.db.with_conn(|c| store::load_meta(c).unwrap().unwrap().failed_tries),
            0
        );
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(2), 0);
        assert_eq!(backoff_ms(3), 1_000);
        assert_eq!(backoff_ms(5), 4_000);
        assert_eq!(backoff_ms(40), 60_000);
    }

    #[test]
    fn totp_display_is_not_activity() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let mut e = entry("a", "pw");
        e.totp = "JBSWY3DPEHPK3PXP".into();
        let id = v.save(None, e).unwrap();
        v.touch(1_000);
        assert_eq!(v.totp_code(&id).unwrap().code.len(), 6);
        assert!(v.lock_if_idle(1_000 + 61_000, 1));
    }

    #[test]
    fn idle_lock() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.touch(10_000);
        assert!(!v.lock_if_idle(10_000 + 59_000, 1));
        assert!(v.lock_if_idle(10_000 + 61_000, 1));
        assert_eq!(v.status(), VaultStatus::Locked);
    }

    #[test]
    fn change_password_keeps_entries() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        assert_eq!(
            v.change_password("wrong-old-pass", "new pass phrase")
                .unwrap_err(),
            VaultError::WrongPassword
        );
        v.change_password("correct horse battery", "new pass phrase")
            .unwrap();
        v.lock();
        assert_eq!(
            v.unlock("correct horse battery", 0).unwrap_err(),
            VaultError::WrongPassword
        );
        v.unlock("new pass phrase", 0).unwrap();
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn corrupt_entries_are_listed_not_fatal() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        v.db.with_conn(|c| {
            c.execute("UPDATE vault_entries SET ciphertext = zeroblob(40)", [])
                .unwrap()
        });
        let list = v.list().unwrap();
        assert!(list[0].corrupt);
        assert_eq!(v.get(&id).unwrap_err(), VaultError::Corrupt);
        v.delete(&id).unwrap();
    }

    #[test]
    fn invalid_totp_is_not_saved() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let mut bad = entry("x", "");
        bad.totp = "not base32!".into();
        assert!(matches!(
            v.save(None, bad).unwrap_err(),
            VaultError::InvalidTotp(_)
        ));
    }

    #[test]
    fn export_import_merge_and_replace() {
        let a = service();
        a.setup("correct horse battery").unwrap();
        let id = a.save(None, entry("GitHub", "pw-1")).unwrap();
        let file = a
            .export("correct horse battery", "backup pass phrase")
            .unwrap();
        assert!(!file.contains("pw-1") && !file.contains("GitHub"));

        let b = service();
        b.setup("another pass phrase").unwrap();
        b.save(None, entry("Local", "x")).unwrap();
        assert_eq!(
            b.import(&file, "wrong pass phrase", ImportMode::Merge)
                .unwrap_err(),
            VaultError::WrongPassword
        );
        assert_eq!(
            b.import(&file, "backup pass phrase", ImportMode::Merge)
                .unwrap(),
            1
        );
        assert_eq!(b.list().unwrap().len(), 2);
        assert_eq!(b.get(&id).unwrap().password, "pw-1");
        assert_eq!(
            b.import(&file, "backup pass phrase", ImportMode::Replace)
                .unwrap(),
            1
        );
        assert_eq!(b.list().unwrap().len(), 1);
    }

    #[test]
    fn merge_keeps_the_newer_copy() {
        let a = service();
        a.setup("correct horse battery").unwrap();
        let id = a.save(None, entry("x", "old")).unwrap();
        let file = a
            .export("correct horse battery", "backup pass phrase")
            .unwrap();
        let mut e = a.get(&id).unwrap();
        e.password = "new".into();
        a.save(Some(&id), e).unwrap();
        assert_eq!(
            a.import(&file, "backup pass phrase", ImportMode::Merge)
                .unwrap(),
            0
        );
        assert_eq!(a.get(&id).unwrap().password, "new");
    }

    #[test]
    fn bad_backups_are_rejected() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        assert!(matches!(
            v.import("{}", "x", ImportMode::Merge).unwrap_err(),
            VaultError::InvalidBackup(_)
        ));
        let file = v
            .export("correct horse battery", "backup pass phrase")
            .unwrap();
        let tampered = file.replacen("\"ciphertext\": \"", "\"ciphertext\": \"AAAA", 1);
        assert_ne!(tampered, file);
        assert!(
            v.import(&tampered, "backup pass phrase", ImportMode::Merge)
                .is_err()
        );
    }

    #[test]
    fn touch_id_unlocks_and_is_dropped_on_reset() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        assert_eq!(
            v.enable_touch_id("wrong password!").unwrap_err(),
            VaultError::WrongPassword
        );
        v.enable_touch_id("correct horse battery").unwrap();
        v.lock();
        v.unlock_with_touch_id("解锁密码本").unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
        v.disable_touch_id();
        v.lock();
        assert!(v.unlock_with_touch_id("解锁密码本").is_err());
    }

    #[test]
    fn every_password_check_is_behind_the_backoff() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.lock();
        let now = now_ms();
        for _ in 0..3 {
            assert_eq!(
                v.change_password("wrong-old-pass", "new pass phrase")
                    .unwrap_err(),
                VaultError::WrongPassword
            );
        }
        // Waiting now, whichever way the password is tried.
        assert!(matches!(
            v.enable_touch_id("correct horse battery").unwrap_err(),
            VaultError::TooManyAttempts { .. }
        ));
        assert!(matches!(
            v.export("correct horse battery", "backup pass phrase")
                .unwrap_err(),
            VaultError::TooManyAttempts { .. }
        ));
        assert!(matches!(
            v.unlock("correct horse battery", now).unwrap_err(),
            VaultError::TooManyAttempts { .. }
        ));
    }

    #[test]
    fn export_needs_the_master_password() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        assert_eq!(
            v.export("wrong password!", "backup pass phrase")
                .unwrap_err(),
            VaultError::WrongPassword
        );
    }

    #[test]
    fn a_lock_during_an_unlock_wins() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.lock();
        let generation = v.generation();
        let (_, dek) = v.verify("correct horse battery", now_ms()).unwrap();
        v.lock();
        assert_eq!(
            v.open_session(dek, now_ms(), generation).unwrap_err(),
            VaultError::Interrupted
        );
        assert_eq!(v.status(), VaultStatus::Locked);
    }

    #[test]
    fn changing_the_password_rotates_the_data_key() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.enable_touch_id("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        let old_meta = v.db.with_conn(store::load_meta).unwrap().unwrap();
        v.change_password("correct horse battery", "new pass phrase")
            .unwrap();

        // The old password and an old copy of the metadata open nothing now.
        let old_kek = derive_key("correct horse battery", &old_meta.salt, old_meta.params).unwrap();
        let old_dek = unwrap_key(&old_kek, &old_meta.wrapped_dek).unwrap();
        let row =
            v.db.with_conn(|c| store::load_entry(c, &id))
                .unwrap()
                .unwrap();
        assert_eq!(
            VaultService::decrypt(&old_dek, &row).unwrap_err(),
            VaultError::Corrupt
        );

        // Entries and Touch ID follow the new key.
        assert_eq!(v.get(&id).unwrap().password, "pw");
        v.lock();
        v.unlock_with_touch_id("test").unwrap();
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn touch_id_refuses_another_vaults_key_even_when_empty() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.keys.save(&SecretKey::random().unwrap()).unwrap();
        v.lock();
        assert!(matches!(
            v.unlock_with_touch_id("test").unwrap_err(),
            VaultError::Keychain(_)
        ));
        assert_eq!(v.status(), VaultStatus::Locked);
    }

    #[test]
    fn replacing_with_an_empty_backup_changes_nothing() {
        let a = service();
        a.setup("correct horse battery").unwrap();
        let empty = a
            .export("correct horse battery", "backup pass phrase")
            .unwrap();
        a.save(None, entry("keep", "pw")).unwrap();
        assert!(matches!(
            a.import(&empty, "backup pass phrase", ImportMode::Replace)
                .unwrap_err(),
            VaultError::InvalidBackup(_)
        ));
        assert_eq!(a.list().unwrap().len(), 1);
    }

    #[test]
    fn reset_wipes_everything() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.save(None, entry("a", "pw")).unwrap();
        v.reset().unwrap();
        assert_eq!(v.status(), VaultStatus::NotSetUp);
    }
}
