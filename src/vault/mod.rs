//! The password vault: everything secret lives under this module.

pub mod account_key;
pub mod backup;
pub mod crypto;
pub mod entry;
pub mod error;
pub mod generator;
pub mod keychain;
pub mod recovery;
pub mod store;
pub mod sync;
pub mod totp;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::db::Database;
use account_key::AccountKey;
pub use crypto::KdfParams;
use crypto::{
    SALT_LEN, Sealed, SecretKey, derive_key_keyed, open, random_bytes, seal, unwrap_key, wrap_key,
};
pub use entry::{EntrySummary, VaultEntry};
pub use error::VaultError;
use keychain::{KeyItem, KeyStore};
use recovery::RecoveryKey;
use store::{EntryRow, MetaRow};

pub const MIN_PASSWORD_CHARS: usize = 10;
/// Key formats (`vault_meta.version`): the master password alone, or with
/// the secret key.
const PASSWORD_ONLY: i64 = 1;
const WITH_ACCOUNT_KEY: i64 = 2;
/// What a deletion marker seals.
const DELETED: &[u8] = b"null";
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

/// The associated data of an entry's ciphertext: its id, and its revision
/// once it has one, so an old revision cannot stand in for a newer one.
fn aad(id: &str, revision: u64) -> Vec<u8> {
    if revision == 0 {
        id.as_bytes().to_vec()
    } else {
        format!("{id}:{revision}").into_bytes()
    }
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
        self.create(password, None)
    }

    /// Creates the vault with `password` and the recovery phrase `phrase`
    /// (which the user has written down) and leaves it unlocked.
    pub fn setup_with_recovery(
        &self,
        password: &str,
        phrase: &RecoveryKey,
    ) -> Result<(), VaultError> {
        self.create(password, Some(phrase))
    }

    fn create(&self, password: &str, phrase: Option<&RecoveryKey>) -> Result<(), VaultError> {
        if self.meta()?.is_some() {
            return Err(VaultError::AlreadySetUp);
        }
        check_strength(password)?;
        let generation = self.generation();
        // Keys of an earlier vault must not outlive it (the key check would
        // refuse them anyway).
        self.forget_device_keys();
        let dek = SecretKey::random()?;
        let meta = self.new_meta(password, &dek, None, phrase)?;
        self.db.with_conn(|c| store::save_meta(c, &meta))?;
        self.open_session(dek, now_ms(), generation)
    }

    /// The vault's metadata for `password` (and the secret key, when sync
    /// is on, and the recovery phrase, when there is one) and data key
    /// `dek`.
    fn new_meta(
        &self,
        password: &str,
        dek: &SecretKey,
        account_key: Option<&AccountKey>,
        phrase: Option<&RecoveryKey>,
    ) -> Result<MetaRow, VaultError> {
        let salt = random_bytes::<SALT_LEN>()?;
        let secret = account_key.map(|k| &k.as_bytes()[..]);
        let kek = derive_key_keyed(password, &salt, self.params, secret)?;
        Ok(MetaRow {
            version: if account_key.is_some() {
                WITH_ACCOUNT_KEY
            } else {
                PASSWORD_ONLY
            },
            params: self.params,
            salt: salt.to_vec(),
            wrapped_dek: wrap_key(&kek, dek)?,
            key_check: key_check(dek)?,
            recovery_wrap: match phrase {
                Some(phrase) => recovery::wrap(phrase, dek, account_key)?,
                None => Vec::new(),
            },
            recovery_sealed: match phrase {
                Some(phrase) => recovery::seal_key(dek, phrase)?,
                None => Vec::new(),
            },
            failed_tries: 0,
            locked_until_ms: 0,
        })
    }

    /// The recovery phrase `meta` keeps under `dek`, to wrap a new data
    /// key for it. One that does not open is lost (the settings show the
    /// vault without a phrase).
    fn phrase_of(meta: &MetaRow, dek: &SecretKey) -> Option<RecoveryKey> {
        if meta.recovery_sealed.is_empty() {
            return None;
        }
        recovery::open_key(dek, &meta.recovery_sealed)
            .inspect_err(|_| log::warn!("the recovery phrase does not open: dropped"))
            .ok()
    }

    /// The data key (and secret key) `phrase` unwraps from `meta`, if it is
    /// that vault's key.
    fn open_with_phrase(
        meta: &MetaRow,
        phrase: &RecoveryKey,
    ) -> Option<(SecretKey, Option<AccountKey>)> {
        if meta.recovery_wrap.is_empty() {
            return None;
        }
        let (dek, account_key) = recovery::unwrap(phrase, &meta.recovery_wrap).ok()?;
        matches_check(&dek, &meta.key_check).then_some((dek, account_key))
    }

    /// The secret key `meta` needs, from this device.
    fn account_key_for(&self, meta: &MetaRow) -> Result<Option<AccountKey>, VaultError> {
        if meta.version < WITH_ACCOUNT_KEY {
            return Ok(None);
        }
        self.stored_account_key()
            .map(Some)
            .ok_or(VaultError::SecretKeyNeeded)
    }

    /// Writes `meta` and `rows` atomically. When syncing, `meta` becomes the
    /// next header, its digest sealed under `previous` (the data key the
    /// other devices hold) so they can tell this device made it.
    fn save_header(
        &self,
        meta: &MetaRow,
        rows: &[EntryRow],
        previous: &SecretKey,
    ) -> Result<(), VaultError> {
        self.db.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            store::save_meta(&tx, meta)?;
            for row in rows {
                store::upsert_entry(&tx, row)?;
            }
            if let Some(mut sync) = store::load_sync(&tx)? {
                sync.header_version += 1;
                let header = sync::header_of(meta, sync.header_version, None);
                sync.header_auth = Some(sync::header_auth(previous, &header)?);
                sync.staged_header = None;
                store::save_sync(&tx, &sync)?;
            }
            tx.commit()?;
            Ok::<_, VaultError>(())
        })
    }

    /// Moves the vault to a new data key under `password` (with the secret
    /// key and the recovery phrase given): every entry is encrypted again,
    /// so an old copy of the database opens none of them. The vault is left
    /// unlocked.
    fn rekey(
        &self,
        old: &SecretKey,
        password: &str,
        account_key: Option<&AccountKey>,
        phrase: Option<&RecoveryKey>,
        generation: u64,
    ) -> Result<(), VaultError> {
        let new_dek = SecretKey::random()?;
        let rows = self
            .db
            .with_conn(store::load_entries)?
            .into_iter()
            // A corrupt entry stays as it was.
            .filter_map(|row| self.reseal(old, &new_dek, &row, row.revision + 1).ok())
            .collect::<Vec<_>>();
        let meta = self.new_meta(password, &new_dek, account_key, phrase)?;
        self.save_header(&meta, &rows, old)?;
        // A Touch ID copy follows the new key, or goes.
        if self.keys.get(KeyItem::DataKey).is_some()
            && self.keys.put(KeyItem::DataKey, new_dek.as_bytes()).is_err()
        {
            self.keys.delete(KeyItem::DataKey);
        }
        // Locked meanwhile: the key changed all the same.
        let _ = self.open_session(new_dek, now_ms(), generation);
        Ok(())
    }

    /// Whether the vault has a recovery phrase.
    pub fn has_recovery_phrase(&self) -> bool {
        self.meta()
            .ok()
            .flatten()
            .is_some_and(|m| !m.recovery_sealed.is_empty())
    }

    /// Checks the master password (behind the backoff), leaving the vault as
    /// it is.
    pub fn check_password(&self, password: &str) -> Result<(), VaultError> {
        self.verify(password, now_ms()).map(|_| ())
    }

    /// Sets the recovery phrase to `phrase`, after the master password. A
    /// phrase replacing another comes with a new data key, so the old phrase
    /// opens no copy of the vault made from now on.
    pub fn set_recovery_phrase(
        &self,
        password: &str,
        phrase: &RecoveryKey,
    ) -> Result<(), VaultError> {
        let generation = self.generation();
        let (meta, dek) = self.verify(password, now_ms())?;
        // A newer header waits: what this device would write goes under it.
        if self.has_staged_header() {
            return Err(VaultError::HeaderChanged);
        }
        let account_key = self.account_key_for(&meta)?;
        if !meta.recovery_sealed.is_empty() {
            return self.rekey(
                &dek,
                password,
                account_key.as_ref(),
                Some(phrase),
                generation,
            );
        }
        let meta = MetaRow {
            recovery_wrap: recovery::wrap(phrase, &dek, account_key.as_ref())?,
            recovery_sealed: recovery::seal_key(&dek, phrase)?,
            ..meta
        };
        self.save_header(&meta, &[], &dek)?;
        let _ = self.open_session(dek, now_ms(), generation);
        Ok(())
    }

    /// Opens the vault with its recovery phrase (the master password, and
    /// the secret key, forgotten or not on this device) and sets
    /// `new_password`; the phrase stays. A newer header from another device,
    /// when one waits, is the one to open.
    pub fn recover(&self, phrase: &RecoveryKey, new_password: &str) -> Result<(), VaultError> {
        check_strength(new_password)?;
        let generation = self.generation();
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let mut local = Self::open_with_phrase(&meta, phrase);
        // A newer header waits: only it counts (this device's copy is
        // behind), and only when it is whole at its version, so the server
        // cannot pass an older one off as newer.
        let staged = self.staged_header();
        let (dek, account_key, format) = match &staged {
            Some(header) => {
                let (dek, account_key) = Self::open_with_phrase(&sync::meta_of(header), phrase)
                    .filter(|(dek, _)| sync::header_is_sealed_by(dek, header))
                    .ok_or(if local.is_some() {
                        VaultError::StaleRecoveryPhrase
                    } else {
                        VaultError::WrongRecoveryPhrase
                    })?;
                (dek, account_key, header.format)
            }
            None => {
                let (dek, account_key) = local.take().ok_or(VaultError::WrongRecoveryPhrase)?;
                self.check_joined(&meta, &dek)?;
                (dek, account_key, meta.version)
            }
        };
        let account_key = match (format >= WITH_ACCOUNT_KEY, account_key) {
            (false, _) => None,
            (true, Some(key)) => {
                self.keys
                    .put(KeyItem::SecretKey, key.as_bytes())
                    .map_err(VaultError::Keychain)?;
                Some(key)
            }
            (true, None) => Some(
                self.stored_account_key()
                    .ok_or(VaultError::SecretKeyNeeded)?,
            ),
        };
        if let Some(header) = &staged {
            // Changes made here under the old key follow when it is known.
            let old = local.as_ref().map(|(old, _)| old);
            self.promote_staged(header, old.map(|old| (old, &dek)))?;
        }
        if self.sync_state()?.is_none() {
            // This device holds every entry: a new data key too, so an old
            // copy of the database and the old password open nothing new.
            return self.rekey(
                &dek,
                new_password,
                account_key.as_ref(),
                Some(phrase),
                generation,
            );
        }
        // Syncing, the data key stays: a device that just joined holds only
        // the server's records, under that key, and cannot encrypt them
        // again. The secret key still guards an old copy.
        let meta = self.new_meta(new_password, &dek, account_key.as_ref(), Some(phrase))?;
        self.save_header(&meta, &[], &dek)?;
        // Locked meanwhile: the password changed all the same.
        let _ = self.open_session(dek, now_ms(), generation);
        Ok(())
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
        self.verify_with(password, now, None)
    }

    /// [`Self::verify`] with the secret key given (else this device's).
    fn verify_with(
        &self,
        password: &str,
        now: u64,
        account_key: Option<&AccountKey>,
    ) -> Result<(MetaRow, SecretKey), VaultError> {
        let mut meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        if now < meta.locked_until_ms {
            return Err(VaultError::TooManyAttempts {
                wait_ms: meta.locked_until_ms - now,
            });
        }
        let stored;
        let account_key = match (meta.version >= WITH_ACCOUNT_KEY, account_key) {
            (false, _) => None,
            (true, Some(key)) => Some(key),
            (true, None) => {
                stored = self
                    .stored_account_key()
                    .ok_or(VaultError::SecretKeyNeeded)?;
                Some(&stored)
            }
        };
        let tries = meta.failed_tries.saturating_add(1);
        self.db
            .with_conn(|c| store::set_failures(c, tries, now + backoff_ms(tries)))?;
        // A newer header from another device's password change: it becomes
        // the vault's once the password opens it (changes made here under
        // the old key and not uploaded cannot follow without that key).
        if let Some(staged) = self.staged_header()
            && let Some(dek) = self.open_staged(&staged, password)
        {
            self.promote_staged(&staged, None)?;
            let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
            return Ok((meta, dek));
        }
        let secret = account_key.map(|k| &k.as_bytes()[..]);
        let kek = derive_key_keyed(password, &meta.salt, meta.params, secret)?;
        let dek = unwrap_key(&kek, &meta.wrapped_dek).map_err(|_| VaultError::WrongPassword)?;
        self.check_joined(&meta, &dek)?;
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

    /// Unlocks with the master password and the secret key as the user typed
    /// it (a device that does not have it yet); keeps the key on this device.
    pub fn unlock_with_account_key(
        &self,
        password: &str,
        account_key: &str,
        now: u64,
    ) -> Result<(), VaultError> {
        let key = AccountKey::parse(account_key).ok_or(VaultError::InvalidSecretKey)?;
        let generation = self.generation();
        let (_, dek) = self.verify_with(password, now, Some(&key))?;
        self.keys
            .put(KeyItem::SecretKey, key.as_bytes())
            .map_err(VaultError::Keychain)?;
        self.open_session(dek, now, generation)
    }

    /// Whether the vault is keyed by the secret key too (sync was set up).
    pub fn uses_account_key(&self) -> bool {
        self.meta()
            .ok()
            .flatten()
            .is_some_and(|m| m.version >= WITH_ACCOUNT_KEY)
    }

    /// Whether unlocking needs a secret key this device does not have.
    pub fn needs_account_key(&self) -> bool {
        self.meta()
            .ok()
            .flatten()
            .is_some_and(|m| m.version >= WITH_ACCOUNT_KEY)
            && self.stored_account_key().is_none()
    }

    fn stored_account_key(&self) -> Option<AccountKey> {
        self.keys
            .get(KeyItem::SecretKey)
            .and_then(|bytes| AccountKey::from_slice(&bytes))
    }

    /// Turns on the secret key: the data key is wrapped again with the
    /// master password and `account_key`, and kept on this device. Entries
    /// from before get revisions, so they can sync.
    pub fn enable_account_key(
        &self,
        password: &str,
        account_key: &AccountKey,
    ) -> Result<(), VaultError> {
        let generation = self.generation();
        let (meta, old_dek) = self.verify(password, now_ms())?;
        if meta.version >= WITH_ACCOUNT_KEY {
            return Err(VaultError::AlreadySetUp);
        }
        self.keys
            .put(KeyItem::SecretKey, account_key.as_bytes())
            .map_err(VaultError::Keychain)?;
        // A new data key: copies of the vault from before (backups, freed
        // pages) hold the old one wrapped by the password alone, and must
        // open nothing that syncs. The recovery phrase now wraps the secret
        // key too.
        let phrase = Self::phrase_of(&meta, &old_dek);
        self.rekey(
            &old_dek,
            password,
            Some(account_key),
            phrase.as_ref(),
            generation,
        )
    }

    /// The secret key of this device, as written, after the master password.
    pub fn reveal_account_key(
        &self,
        password: &str,
    ) -> Result<zeroize::Zeroizing<String>, VaultError> {
        self.verify(password, now_ms())?;
        self.stored_account_key()
            .map(|k| k.encode())
            .ok_or(VaultError::SecretKeyNeeded)
    }

    /// Deletes the keys this device keeps for the vault.
    fn forget_device_keys(&self) {
        for item in [KeyItem::DataKey, KeyItem::SecretKey, KeyItem::DeviceToken] {
            self.keys.delete(item);
        }
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

    /// The entry a row seals; [`VaultError::NotFound`] for a deletion marker.
    fn decrypt(key: &SecretKey, row: &EntryRow) -> Result<VaultEntry, VaultError> {
        Self::decrypt_row(key, row)?.ok_or(VaultError::NotFound)
    }

    /// What a row seals: an entry, or `None` for a deletion marker. Fails
    /// unless it is authentic for its id and revision.
    pub(crate) fn decrypt_row(
        key: &SecretKey,
        row: &EntryRow,
    ) -> Result<Option<VaultEntry>, VaultError> {
        let sealed =
            Sealed::from_parts(&row.nonce, &row.ciphertext).map_err(|_| VaultError::Corrupt)?;
        let plain =
            open(key, &sealed, &aad(&row.id, row.revision)).map_err(|_| VaultError::Corrupt)?;
        if row.deleted {
            return if plain.as_slice() == DELETED {
                Ok(None)
            } else {
                Err(VaultError::Corrupt)
            };
        }
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| VaultError::Corrupt)
    }

    /// A row sealing `entry` (`None`: a deletion marker) as `id` at
    /// `revision`, to upload.
    pub(crate) fn encrypt(
        key: &SecretKey,
        id: &str,
        revision: u64,
        entry: Option<&VaultEntry>,
    ) -> Result<EntryRow, VaultError> {
        let plain = match entry {
            Some(entry) => {
                zeroize::Zeroizing::new(serde_json::to_vec(entry).map_err(|_| VaultError::Crypto)?)
            }
            None => zeroize::Zeroizing::new(DELETED.to_vec()),
        };
        let sealed = seal(key, &plain, &aad(id, revision))?;
        Ok(EntryRow {
            id: id.to_owned(),
            nonce: sealed.nonce.to_vec(),
            ciphertext: sealed.ciphertext,
            updated_at: entry
                .map(|e| e.updated_at.clone())
                .unwrap_or_else(timestamp),
            revision,
            deleted: entry.is_none(),
            dirty: true,
            synced_revision: 0,
        })
    }

    /// `row` decrypted with `old` and sealed again with `new` at `revision`,
    /// still to upload, keeping what the server last had.
    fn reseal(
        &self,
        old: &SecretKey,
        new: &SecretKey,
        row: &EntryRow,
        revision: u64,
    ) -> Result<EntryRow, VaultError> {
        let entry = Self::decrypt_row(old, row)?;
        let mut sealed = Self::encrypt(new, &row.id, revision, entry.as_ref())?;
        sealed.synced_revision = row.synced_revision;
        sealed.updated_at = row.updated_at.clone();
        Ok(sealed)
    }

    /// The row of `id` sealing `entry` (`None`: deleted) at its next
    /// revision.
    fn next_row(
        &self,
        key: &SecretKey,
        id: &str,
        entry: Option<&VaultEntry>,
    ) -> Result<EntryRow, VaultError> {
        let current = self.db.with_conn(|c| store::load_entry(c, id))?;
        let revision = current.as_ref().map_or(0, |r| r.revision) + 1;
        let mut row = Self::encrypt(key, id, revision, entry)?;
        row.synced_revision = current.map_or(0, |r| r.synced_revision);
        Ok(row)
    }

    /// Every entry's summary, sorted by title.
    pub fn list(&self) -> Result<Vec<EntrySummary>, VaultError> {
        self.with_key(|key| {
            let rows = self.db.with_conn(store::load_entries)?;
            let mut list: Vec<EntrySummary> = rows
                .iter()
                .filter(|row| !row.deleted)
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
            let row = self.next_row(key, &id, Some(&entry))?;
            self.db.with_conn(|c| store::upsert_entry(c, &row))?;
            Ok(id)
        })
    }

    /// Deletes entry `id`, leaving a deletion marker to sync.
    pub fn delete(&self, id: &str) -> Result<(), VaultError> {
        self.with_key(|key| {
            let row = self.next_row(key, id, None)?;
            Ok(self.db.with_conn(|c| store::upsert_entry(c, &row))?)
        })
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
        let (meta, old_dek) = self.verify(old, now_ms())?;
        let account_key = self.account_key_for(&meta)?;
        // The recovery phrase stays: it wraps the new data key.
        let phrase = Self::phrase_of(&meta, &old_dek);
        self.rekey(
            &old_dek,
            new,
            account_key.as_ref(),
            phrase.as_ref(),
            generation,
        )
    }

    /// Every entry, encrypted with `backup_password`; needs the master
    /// password again.
    pub fn export(&self, master: &str, backup_password: &str) -> Result<String, VaultError> {
        check_strength(backup_password)?;
        let (_, dek) = self.verify(master, now_ms())?;
        let rows = self.db.with_conn(store::load_entries)?;
        let entries = rows
            .iter()
            .filter(|row| !row.deleted)
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
                let current = |id: &str| existing.iter().find(|r| r.id == id);
                let rows: Vec<EntryRow> = entries
                    .iter()
                    .filter(|b| match mode {
                        ImportMode::Replace => true,
                        ImportMode::Merge => current(&b.id)
                            .is_none_or(|r| r.deleted || r.updated_at < b.entry.updated_at),
                    })
                    .map(|b| {
                        let base = current(&b.id);
                        let revision = base.map_or(0, |r| r.revision) + 1;
                        let mut row = Self::encrypt(key, &b.id, revision, Some(&b.entry))?;
                        row.synced_revision = base.map_or(0, |r| r.synced_revision);
                        Ok::<_, VaultError>(row)
                    })
                    .collect::<Result<_, _>>()?;
                if mode == ImportMode::Replace {
                    // Entries not in the backup leave deletion markers to sync.
                    for gone in existing
                        .iter()
                        .filter(|r| !r.deleted && !entries.iter().any(|b| b.id == r.id))
                    {
                        let mut marker = Self::encrypt(key, &gone.id, gone.revision + 1, None)?;
                        marker.synced_revision = gone.synced_revision;
                        store::upsert_entry(&tx, &marker)?;
                    }
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
        self.keys
            .put(KeyItem::DataKey, dek.as_bytes())
            .map_err(VaultError::Keychain)
    }

    pub fn disable_touch_id(&self) {
        self.keys.delete(KeyItem::DataKey);
    }

    /// Unlocks with the stored key after the system asks the user; only
    /// this vault's key (its key check) opens it. Blocking.
    pub fn unlock_with_touch_id(&self, reason: &str) -> Result<(), VaultError> {
        let generation = self.generation();
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        if meta.key_check.is_empty() {
            return Err(VaultError::Keychain("请先用主密码解锁一次".into()));
        }
        let bytes = self
            .keys
            .get_with_auth(KeyItem::DataKey, reason)
            .map_err(VaultError::Keychain)?;
        let dek = SecretKey::from_slice(&bytes)
            .map_err(|_| VaultError::Keychain("Touch ID 密钥已失效，请用主密码解锁".into()))?;
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
        self.forget_device_keys();
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

    const PW: &str = "correct horse battery";
    const NEW_PW: &str = "brand new pass phrase";

    #[test]
    fn a_forgotten_password_is_reset_with_the_recovery_phrase() {
        let v = service();
        let phrase = RecoveryKey::generate().unwrap();
        v.setup_with_recovery(PW, &phrase).unwrap();
        assert!(v.has_recovery_phrase());
        let id = v.save(None, entry("a", "pw")).unwrap();
        v.lock();
        let typed = phrase.words().concat();
        let parsed = RecoveryKey::parse(&typed).unwrap();
        assert_eq!(
            v.recover(&parsed, "short").unwrap_err(),
            VaultError::WeakPassword
        );
        v.recover(&parsed, NEW_PW).unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
        assert_eq!(v.get(&id).unwrap().password, "pw");
        v.lock();
        assert_eq!(v.unlock(PW, 0).unwrap_err(), VaultError::WrongPassword);
        v.unlock(NEW_PW, 0).unwrap();
        // The phrase stays the vault's.
        v.lock();
        v.recover(&phrase, PW).unwrap();
        v.lock();
        v.unlock(PW, 0).unwrap();
    }

    #[test]
    fn another_phrase_or_none_recovers_nothing() {
        let v = service();
        v.setup(PW).unwrap();
        assert!(!v.has_recovery_phrase());
        let phrase = RecoveryKey::generate().unwrap();
        assert_eq!(
            v.recover(&phrase, NEW_PW).unwrap_err(),
            VaultError::WrongRecoveryPhrase
        );
        v.set_recovery_phrase(PW, &phrase).unwrap();
        let other = RecoveryKey::generate().unwrap();
        assert_eq!(
            v.recover(&other, NEW_PW).unwrap_err(),
            VaultError::WrongRecoveryPhrase
        );
        v.lock();
        v.unlock(PW, 0).unwrap();
    }

    #[test]
    fn recovering_without_sync_changes_the_data_key() {
        let v = service();
        let phrase = RecoveryKey::generate().unwrap();
        v.setup_with_recovery(PW, &phrase).unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        let before = v.meta().unwrap().unwrap();
        v.lock();
        v.recover(&phrase, NEW_PW).unwrap();
        let after = v.meta().unwrap().unwrap();
        // Another key: the old password with an old copy opens nothing new.
        assert_ne!(before.key_check, after.key_check);
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn the_phrase_survives_a_password_change() {
        let v = service();
        let phrase = RecoveryKey::generate().unwrap();
        v.setup_with_recovery(PW, &phrase).unwrap();
        v.change_password(PW, NEW_PW).unwrap();
        assert!(v.has_recovery_phrase());
        v.lock();
        v.recover(&phrase, "third pass phrase").unwrap();
        v.lock();
        v.unlock("third pass phrase", 0).unwrap();
    }

    #[test]
    fn a_phrase_added_later_keeps_the_key_a_new_one_replaces_it() {
        let v = service();
        v.setup(PW).unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        let first = RecoveryKey::generate().unwrap();
        assert_eq!(
            v.set_recovery_phrase("wrong password!", &first)
                .unwrap_err(),
            VaultError::WrongPassword
        );
        let before = v.meta().unwrap().unwrap();
        v.set_recovery_phrase(PW, &first).unwrap();
        let after = v.meta().unwrap().unwrap();
        assert_eq!(before.wrapped_dek, after.wrapped_dek);

        let second = RecoveryKey::generate().unwrap();
        v.set_recovery_phrase(PW, &second).unwrap();
        let replaced = v.meta().unwrap().unwrap();
        assert_ne!(after.wrapped_dek, replaced.wrapped_dek);
        v.lock();
        assert_eq!(
            v.recover(&first, NEW_PW).unwrap_err(),
            VaultError::WrongRecoveryPhrase
        );
        v.recover(&second, NEW_PW).unwrap();
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn the_phrase_is_never_stored_in_the_clear() {
        let v = service();
        let phrase = RecoveryKey::generate().unwrap();
        v.setup_with_recovery(PW, &phrase).unwrap();
        let raw = format!("{:?}", v.meta().unwrap().unwrap());
        let words = phrase.words().concat();
        assert!(!raw.contains(&words));
        let meta = v.meta().unwrap().unwrap();
        let bytes = RecoveryKey::parse(&words).unwrap();
        for blob in [&meta.recovery_wrap, &meta.recovery_sealed] {
            assert!(
                !blob
                    .windows(32)
                    .any(|w| RecoveryKey::parse_bytes(w).as_ref() == Some(&bytes))
            );
        }
    }

    #[test]
    fn recovery_restores_the_secret_key_on_a_device_without_it() {
        let v = service();
        let phrase = RecoveryKey::generate().unwrap();
        v.setup_with_recovery(PW, &phrase).unwrap();
        let account_key = AccountKey::generate().unwrap();
        v.enable_account_key(PW, &account_key).unwrap();
        v.lock();
        // As on a device that never had the secret key.
        v.keys.delete(KeyItem::SecretKey);
        assert!(v.needs_account_key());
        v.recover(&phrase, NEW_PW).unwrap();
        assert!(!v.needs_account_key());
        assert_eq!(
            v.stored_account_key().unwrap().as_bytes(),
            account_key.as_bytes()
        );
        v.lock();
        v.unlock(NEW_PW, 0).unwrap();
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
        let old_kek =
            crypto::derive_key("correct horse battery", &old_meta.salt, old_meta.params).unwrap();
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
    fn enabling_the_secret_key_rotates_the_data_key() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        let old_meta = v.db.with_conn(store::load_meta).unwrap().unwrap();
        v.enable_account_key("correct horse battery", &AccountKey::generate().unwrap())
            .unwrap();
        // A copy of the vault from before (password only) opens nothing now.
        let old_kek =
            crypto::derive_key("correct horse battery", &old_meta.salt, old_meta.params).unwrap();
        let old_dek = unwrap_key(&old_kek, &old_meta.wrapped_dek).unwrap();
        let row =
            v.db.with_conn(|c| store::load_entry(c, &id))
                .unwrap()
                .unwrap();
        assert_eq!(
            VaultService::decrypt(&old_dek, &row).unwrap_err(),
            VaultError::Corrupt
        );
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn touch_id_refuses_another_vaults_key_even_when_empty() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.keys
            .put(KeyItem::DataKey, SecretKey::random().unwrap().as_bytes())
            .unwrap();
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
