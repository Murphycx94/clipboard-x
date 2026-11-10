//! The vault's side of sync, without the network: what to upload, how to
//! apply what came down (verified against the manifest the clients seal),
//! and the vault's header.
//!
//! The server only stores what passes here. It cannot read it, and what it
//! could do (roll back, withhold, replay or swap a record, drop the
//! manifest) fails [`VaultService::apply`] with [`VaultError::Tampered`],
//! which applies nothing. A header from the server never replaces the local
//! one directly: it waits apart until it proves itself by unlocking, so a
//! forged one costs nothing.

use std::collections::BTreeMap;

use clipboardx_sync_proto::{Bytes, Changes, Manifest, RecordUp, VaultHeader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::crypto::{KdfParams, NONCE_LEN, Sealed, SecretKey, open, seal};
use super::keychain::KeyItem;
use super::recovery;
use super::store::{self, EntryRow, MetaRow, SyncRow};
use super::{VaultError, VaultService, WITH_ACCOUNT_KEY, timestamp};

const MANIFEST_AAD: &[u8] = b"clipboardx/vault/manifest/v1";
const HEADER_AAD: &[u8] = b"clipboardx/vault/header/v1";
const SEAL_AAD: &[u8] = b"clipboardx/vault/header-seal/v1";
/// A sealed digest: nonce, 32 bytes, tag.
const SEALED_DIGEST_LEN: usize = NONCE_LEN + 32 + 16;
/// Added to the title of the copy kept of a local change that lost.
pub const CONFLICT_SUFFIX: &str = "（冲突副本）";

/// Bounds of a header's key derivation and fields: a header outside them
/// is refused before any key is derived from it.
const MAX_KDF_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_KDF_PASSES: u32 = 10;
const MAX_KDF_LANES: u32 = 8;
const MIN_SALT_LEN: usize = 16;
/// A wrapped data key: nonce, 32 bytes, tag.
const WRAPPED_KEY_LEN: usize = NONCE_LEN + 32 + 16;

pub type SyncState = SyncRow;

/// One record in the manifest: its revision and the digest of its
/// ciphertext, so a different ciphertext of the same revision is caught.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestEntry {
    revision: u64,
    digest: String,
}

/// Every record the server should have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestBody {
    version: u64,
    entries: BTreeMap<String, ManifestEntry>,
}

/// What [`VaultService::apply`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Records taken from the server.
    pub changed: usize,
    /// Local changes that lost to the server's and were kept as copies.
    pub conflicts: usize,
}

fn record_digest(nonce: &[u8], ciphertext: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(ciphertext);
    hex::encode(hasher.finalize())
}

fn seal_manifest(key: &SecretKey, body: &ManifestBody) -> Result<Manifest, VaultError> {
    let json = Zeroizing::new(serde_json::to_vec(body).map_err(|_| VaultError::Crypto)?);
    let sealed = seal(key, &json, MANIFEST_AAD)?;
    Ok(Manifest {
        version: body.version,
        sealed: Bytes(sealed.to_bytes()),
    })
}

fn open_manifest(key: &SecretKey, manifest: &Manifest) -> Result<ManifestBody, VaultError> {
    let sealed = Sealed::from_bytes(&manifest.sealed.0).map_err(|_| VaultError::Tampered)?;
    let plain = open(key, &sealed, MANIFEST_AAD).map_err(|_| VaultError::Tampered)?;
    let body: ManifestBody = serde_json::from_slice(&plain).map_err(|_| VaultError::Tampered)?;
    if body.version != manifest.version {
        return Err(VaultError::Tampered);
    }
    Ok(body)
}

/// A digest of everything in a header but its authenticator and seal.
fn header_digest(header: &VaultHeader) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(header.version.to_be_bytes());
    hasher.update(header.format.to_be_bytes());
    for n in [header.kdf_m, header.kdf_t, header.kdf_p] {
        hasher.update(n.to_be_bytes());
    }
    fn optional(field: &Option<Bytes>) -> &[u8] {
        field.as_ref().map_or(&[], |b| &b.0)
    }
    for field in [
        &header.salt.0[..],
        &header.wrapped_key.0,
        &header.key_check.0,
        optional(&header.recovery_wrap),
        optional(&header.recovery_sealed),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}

/// The authenticator of `header`: its digest sealed with the data key of
/// the header it replaces.
pub(super) fn header_auth(
    previous_key: &SecretKey,
    header: &VaultHeader,
) -> Result<Vec<u8>, VaultError> {
    Ok(seal(previous_key, &header_digest(header), HEADER_AAD)?.to_bytes())
}

fn header_is_authentic(key: &SecretKey, header: &VaultHeader) -> bool {
    opens_to_digest(key, header.auth.as_ref(), HEADER_AAD, header)
}

/// The seal of `header`: its digest sealed with its own data key.
fn header_seal(key: &SecretKey, header: &VaultHeader) -> Result<Vec<u8>, VaultError> {
    Ok(seal(key, &header_digest(header), SEAL_AAD)?.to_bytes())
}

/// Whether `key` (what the header opened to) sealed `header` as it is.
pub(super) fn header_is_sealed_by(key: &SecretKey, header: &VaultHeader) -> bool {
    opens_to_digest(key, header.seal.as_ref(), SEAL_AAD, header)
}

fn opens_to_digest(
    key: &SecretKey,
    sealed: Option<&Bytes>,
    aad: &[u8],
    header: &VaultHeader,
) -> bool {
    sealed
        .and_then(|bytes| Sealed::from_bytes(&bytes.0).ok())
        .and_then(|sealed| open(key, &sealed, aad).ok())
        .is_some_and(|plain| plain.as_slice() == header_digest(header))
}

/// Refuses a header no client of ours makes: keys from it would be costly
/// to derive or could not work.
fn check_header(header: &VaultHeader) -> Result<(), VaultError> {
    let sane = header.format >= WITH_ACCOUNT_KEY
        && (8..=MAX_KDF_MEMORY_KIB).contains(&header.kdf_m)
        && (1..=MAX_KDF_PASSES).contains(&header.kdf_t)
        && (1..=MAX_KDF_LANES).contains(&header.kdf_p)
        && header.salt.0.len() >= MIN_SALT_LEN
        && header.wrapped_key.0.len() == WRAPPED_KEY_LEN
        && !header.key_check.0.is_empty()
        && header
            .recovery_wrap
            .as_ref()
            .is_none_or(|b| recovery::WRAP_LENS.contains(&b.0.len()))
        && header
            .recovery_sealed
            .as_ref()
            .is_none_or(|b| b.0.len() == recovery::SEALED_LEN)
        // Both or neither.
        && header.recovery_wrap.is_some() == header.recovery_sealed.is_some()
        && header
            .seal
            .as_ref()
            .is_some_and(|b| b.0.len() == SEALED_DIGEST_LEN);
    if sane {
        Ok(())
    } else {
        Err(VaultError::Tampered)
    }
}

/// The metadata a header describes.
pub(super) fn meta_of(header: &VaultHeader) -> MetaRow {
    MetaRow {
        version: header.format,
        params: KdfParams {
            m: header.kdf_m,
            t: header.kdf_t,
            p: header.kdf_p,
        },
        salt: header.salt.0.clone(),
        wrapped_dek: header.wrapped_key.0.clone(),
        key_check: header.key_check.0.clone(),
        recovery_wrap: header
            .recovery_wrap
            .as_ref()
            .map(|b| b.0.clone())
            .unwrap_or_default(),
        recovery_sealed: header
            .recovery_sealed
            .as_ref()
            .map(|b| b.0.clone())
            .unwrap_or_default(),
        failed_tries: 0,
        locked_until_ms: 0,
    }
}

/// The header of `meta` at `version`.
pub(super) fn header_of(meta: &MetaRow, version: u64, auth: Option<Vec<u8>>) -> VaultHeader {
    VaultHeader {
        version,
        format: meta.version,
        kdf_m: meta.params.m,
        kdf_t: meta.params.t,
        kdf_p: meta.params.p,
        salt: Bytes(meta.salt.clone()),
        wrapped_key: Bytes(meta.wrapped_dek.clone()),
        key_check: Bytes(meta.key_check.clone()),
        auth: auth.map(Bytes),
        recovery_wrap: (!meta.recovery_wrap.is_empty()).then(|| Bytes(meta.recovery_wrap.clone())),
        recovery_sealed: (!meta.recovery_sealed.is_empty())
            .then(|| Bytes(meta.recovery_sealed.clone())),
        seal: None,
    }
}

/// Whether `header` keys the vault as `meta` does (the same wrapped data
/// key): what else it changes needs no new password.
fn same_keys(meta: &MetaRow, header: &VaultHeader) -> bool {
    let theirs = meta_of(header);
    (
        meta.version,
        meta.params,
        &meta.salt,
        &meta.wrapped_dek,
        &meta.key_check,
    ) == (
        theirs.version,
        theirs.params,
        &theirs.salt,
        &theirs.wrapped_dek,
        &theirs.key_check,
    )
}

fn not_syncing() -> VaultError {
    VaultError::Sync("未开启同步".into())
}

impl VaultService {
    pub fn sync_state(&self) -> Result<Option<SyncState>, VaultError> {
        Ok(self.db.with_conn(store::load_sync)?)
    }

    /// Turns sync on for this device, as the first one (the vault must use
    /// the secret key) or one joining (no vault here; the header comes
    /// next, [`VaultService::join_header`]).
    pub fn start_sync(
        &self,
        server_url: &str,
        device_id: &str,
        token: &str,
    ) -> Result<(), VaultError> {
        let meta = self.meta()?;
        if meta.as_ref().is_some_and(|m| m.version < WITH_ACCOUNT_KEY) {
            return Err(VaultError::SecretKeyNeeded);
        }
        self.keys
            .put(KeyItem::DeviceToken, token.as_bytes())
            .map_err(VaultError::Keychain)?;
        let state = SyncRow {
            server_url: server_url.to_owned(),
            device_id: device_id.to_owned(),
            last_seq: 0,
            manifest_version: 0,
            header_version: 1,
            header_auth: None,
            staged_header: None,
            joined_seal: None,
        };
        Ok(self.db.with_conn(|c| store::save_sync(c, &state))?)
    }

    /// Turns sync off here; the vault stays as it is.
    pub fn stop_sync(&self) -> Result<(), VaultError> {
        self.keys.delete(KeyItem::DeviceToken);
        Ok(self.db.with_conn(store::delete_sync)?)
    }

    /// This device's token for the sync server.
    pub fn device_token(&self) -> Option<Zeroizing<String>> {
        let bytes = self.keys.get(KeyItem::DeviceToken)?;
        String::from_utf8(bytes.to_vec()).ok().map(Zeroizing::new)
    }

    /// What another device needs, with the master password and the secret
    /// key, to unlock; sealed, so needs the vault unlocked.
    pub fn header(&self) -> Result<VaultHeader, VaultError> {
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let state = self.sync_state()?.ok_or_else(not_syncing)?;
        let mut header = header_of(&meta, state.header_version, state.header_auth);
        header.seal = Some(Bytes(
            self.with_key_passive(|key| header_seal(key, &header))?,
        ));
        Ok(header)
    }

    /// Whether `header` is this device's own (the same digest).
    pub fn is_own_header(&self, header: &VaultHeader) -> Result<bool, VaultError> {
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let state = self.sync_state()?.ok_or_else(not_syncing)?;
        Ok(header_digest(&header_of(&meta, state.header_version, None)) == header_digest(header))
    }

    /// The server has another header at this device's version: two devices
    /// changed it at once. When theirs only sets another recovery phrase
    /// (authenticated by the data key in use) it is taken, and this
    /// device's phrase is gone; otherwise nothing changes. Either way the
    /// user is told.
    pub fn resolve_header_conflict(&self, header: &VaultHeader) -> Result<(), VaultError> {
        check_header(header)?;
        let authentic = self.with_key_passive(|key| Ok(header_is_authentic(key, header)))?;
        if authentic && self.take_header(header)? {
            return Err(VaultError::Sync(
                "另一台设备同时设置了助记词，本机刚设置的助记词已失效，请在设置中重新生成".into(),
            ));
        }
        Err(VaultError::Sync(
            "两台设备同时修改了主密码或助记词，请在其中一台设备上重新修改".into(),
        ))
    }

    /// Checks the header this device joined with against the data key it
    /// opened to, once: the server cannot have swapped parts of it (such as
    /// the recovery phrase's fields).
    pub(super) fn check_joined(&self, meta: &MetaRow, key: &SecretKey) -> Result<(), VaultError> {
        let Some(state) = self.sync_state()? else {
            return Ok(());
        };
        let Some(seal) = state.joined_seal.clone() else {
            return Ok(());
        };
        let mut header = header_of(meta, state.header_version, None);
        header.seal = Some(Bytes(seal));
        if !header_is_sealed_by(key, &header) {
            return Err(VaultError::Tampered);
        }
        self.db.with_conn(|c| {
            let mut state = store::load_sync(c)?.ok_or_else(not_syncing)?;
            state.joined_seal = None;
            store::save_sync(c, &state)?;
            Ok::<_, VaultError>(())
        })
    }

    /// The header this device has.
    pub fn header_version(&self) -> Result<u64, VaultError> {
        Ok(self.sync_state()?.ok_or_else(not_syncing)?.header_version)
    }

    /// Takes the server's header on a device joining (it has no vault, so
    /// nothing is replaced); unlocking proves it.
    pub fn join_header(&self, header: &VaultHeader) -> Result<(), VaultError> {
        if self.meta()?.is_some() {
            return Err(VaultError::AlreadySetUp);
        }
        check_header(header)?;
        self.db.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            let mut state = store::load_sync(&tx)?.ok_or_else(not_syncing)?;
            store::save_meta(&tx, &meta_of(header))?;
            state.header_version = header.version;
            state.last_seq = 0;
            state.joined_seal = header.seal.as_ref().map(|b| b.0.clone());
            store::save_sync(&tx, &state)?;
            tx.commit()?;
            Ok::<_, VaultError>(())
        })
    }

    /// A newer header from the server; whether it waits for a new master
    /// password. The next header must be authenticated by the data key in
    /// use. When it keeps the data key as it is (another device set up a
    /// recovery phrase) it is taken at once. Otherwise (the master password
    /// changed) it is kept apart, not used: the vault keeps its own until
    /// the user unlocks with the new password
    /// ([`VaultService::adopt_staged_header`], or unlocking). A header
    /// further ahead cannot be checked, and only unlocking will tell.
    pub fn receive_header(&self, header: &VaultHeader) -> Result<bool, VaultError> {
        check_header(header)?;
        let state = self.sync_state()?.ok_or_else(not_syncing)?;
        if header.version <= state.header_version {
            return Err(VaultError::Tampered);
        }
        if header.version == state.header_version + 1 {
            let authentic = self.with_key_passive(|key| Ok(header_is_authentic(key, header)))?;
            if !authentic {
                return Err(VaultError::Tampered);
            }
            if self.take_header(header)? {
                return Ok(false);
            }
        }
        let json = serde_json::to_string(header).map_err(|_| VaultError::Crypto)?;
        self.db.with_conn(|c| {
            let mut state = store::load_sync(c)?.ok_or_else(not_syncing)?;
            state.staged_header = Some(json);
            store::save_sync(c, &state)?;
            Ok::<_, VaultError>(())
        })?;
        Ok(true)
    }

    /// Takes an authentic header if it keeps the data key: its recovery
    /// phrase fields replace ours, the rest of the metadata stays. Whether
    /// it did.
    fn take_header(&self, header: &VaultHeader) -> Result<bool, VaultError> {
        let theirs = meta_of(header);
        self.db.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            let meta = store::load_meta(&tx)?.ok_or(VaultError::NotSetUp)?;
            if !same_keys(&meta, header) {
                return Ok(false);
            }
            let mut state = store::load_sync(&tx)?.ok_or_else(not_syncing)?;
            store::set_recovery(&tx, &theirs.recovery_wrap, &theirs.recovery_sealed)?;
            state.header_version = header.version;
            state.header_auth = header.auth.as_ref().map(|b| b.0.clone());
            store::save_sync(&tx, &state)?;
            tx.commit()?;
            Ok(true)
        })
    }

    /// Whether a newer header waits for the new master password.
    pub fn has_staged_header(&self) -> bool {
        self.staged_header().is_some()
    }

    pub(super) fn staged_header(&self) -> Option<VaultHeader> {
        let json = self.sync_state().ok()??.staged_header?;
        serde_json::from_str(&json).ok()
    }

    /// The data key the staged header gives with `password` (and the secret
    /// key), if it is the key it claims to be.
    pub(super) fn open_staged(&self, header: &VaultHeader, password: &str) -> Option<SecretKey> {
        let meta = meta_of(header);
        let secret = self.stored_account_key()?;
        let kek = super::crypto::derive_key_keyed(
            password,
            &meta.salt,
            meta.params,
            Some(&secret.as_bytes()[..]),
        )
        .ok()?;
        let dek = super::crypto::unwrap_key(&kek, &meta.wrapped_dek).ok()?;
        // An older header renumbered (one from before a key change, which
        // this password may still open) fails its seal.
        (super::matches_check(&dek, &meta.key_check) && header_is_sealed_by(&dek, header))
            .then_some(dek)
    }

    /// Makes the staged header the vault's: its metadata replaces the old,
    /// and every record is fetched again. `reseal` re-encrypts local changes
    /// not yet uploaded from the old key to the new one (when the old one is
    /// at hand).
    pub(super) fn promote_staged(
        &self,
        header: &VaultHeader,
        reseal: Option<(&SecretKey, &SecretKey)>,
    ) -> Result<(), VaultError> {
        let rows = match reseal {
            Some((old, new)) => self
                .db
                .with_conn(store::load_entries)?
                .into_iter()
                .filter(|r| r.dirty)
                .filter_map(|r| self.reseal(old, new, &r, r.revision).ok())
                .collect(),
            None => Vec::new(),
        };
        // The Touch ID copy of the old key opens nothing now.
        self.keys.delete(KeyItem::DataKey);
        self.db.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            let mut state = store::load_sync(&tx)?.ok_or_else(not_syncing)?;
            store::save_meta(&tx, &meta_of(header))?;
            for row in &rows {
                store::upsert_entry(&tx, row)?;
            }
            state.header_version = header.version;
            state.header_auth = None;
            state.staged_header = None;
            state.last_seq = 0;
            store::save_sync(&tx, &state)?;
            tx.commit()?;
            Ok::<_, VaultError>(())
        })
    }

    /// While unlocked with the old key: takes the staged header with the new
    /// master password, re-encrypting changes not yet uploaded, and stays
    /// unlocked with the new key.
    pub fn adopt_staged_header(&self, password: &str) -> Result<(), VaultError> {
        let header = self
            .staged_header()
            .ok_or_else(|| VaultError::Sync("没有待更新的主密码".into()))?;
        let generation = self.generation();
        let new = self
            .open_staged(&header, password)
            .ok_or(VaultError::WrongPassword)?;
        self.with_key_passive(|old| self.promote_staged(&header, Some((old, &new))))?;
        self.open_session(new, super::now_ms(), generation)
    }

    /// The local changes the server does not have; a change this device
    /// cannot decrypt (sealed under a key it no longer has) stays here.
    pub fn pending(&self) -> Result<Vec<RecordUp>, VaultError> {
        self.with_key_passive(|key| {
            let rows = self.db.with_conn(store::load_entries)?;
            Ok(rows
                .into_iter()
                .filter(|r| r.dirty)
                .filter(|r| {
                    let readable = Self::decrypt_row(key, r).is_ok();
                    if !readable {
                        log::warn!("a local change cannot be decrypted: not uploaded");
                    }
                    readable
                })
                .map(|r| RecordUp {
                    id: r.id,
                    base_revision: r.synced_revision,
                    revision: r.revision,
                    deleted: r.deleted,
                    nonce: Bytes(r.nonce),
                    ciphertext: Bytes(r.ciphertext),
                })
                .collect())
        })
    }

    /// The server took `records` but those in `conflicts`.
    pub fn mark_pushed(
        &self,
        records: &[RecordUp],
        conflicts: &[String],
    ) -> Result<(), VaultError> {
        self.db.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            for record in records.iter().filter(|r| !conflicts.contains(&r.id)) {
                store::mark_synced(&tx, &record.id, record.revision)?;
            }
            tx.commit()
        })?;
        Ok(())
    }

    /// Fetches every record again next time (after a verification failure
    /// that a full pull may clear).
    pub fn reset_sync_cursor(&self) -> Result<(), VaultError> {
        self.db.with_conn(|c| {
            let mut state = store::load_sync(c)?.ok_or_else(not_syncing)?;
            state.last_seq = 0;
            store::save_sync(c, &state)?;
            Ok::<_, VaultError>(())
        })
    }

    /// Applies what came down, all or nothing. Every record must be
    /// authentic for its id and revision; the manifest must be authentic and
    /// no older than the last one seen; no record may be older than the
    /// manifest says, nor differ from it at the same revision; and nothing
    /// the manifest lists may be missing after. A local change the server's
    /// copy overtook is kept as a copy.
    pub fn apply(
        &self,
        changes: &Changes,
        manifest: Option<&Manifest>,
    ) -> Result<Applied, VaultError> {
        self.with_key_passive(|key| {
            self.db.with_conn(|c| {
                let tx = c.unchecked_transaction()?;
                let mut state = store::load_sync(&tx)?.ok_or_else(not_syncing)?;
                let body = match manifest {
                    Some(manifest) => {
                        let body = open_manifest(key, manifest)?;
                        if body.version < state.manifest_version {
                            return Err(VaultError::Tampered);
                        }
                        Some(body)
                    }
                    // The server dropped a manifest it had.
                    None if state.manifest_version > 0 => return Err(VaultError::Tampered),
                    None => None,
                };
                let mut applied = Applied::default();
                let mut last_seq = state.last_seq;
                for record in &changes.records {
                    // Revision 0 is authenticated by the id alone: never from
                    // the server.
                    if record.revision == 0 {
                        return Err(VaultError::Tampered);
                    }
                    if let Some(listed) = body.as_ref().and_then(|b| b.entries.get(&record.id)) {
                        let digest = record_digest(&record.nonce.0, &record.ciphertext.0);
                        if record.revision < listed.revision
                            || (record.revision == listed.revision && digest != listed.digest)
                        {
                            return Err(VaultError::Tampered);
                        }
                    }
                    let mut row = EntryRow {
                        id: record.id.clone(),
                        nonce: record.nonce.0.clone(),
                        ciphertext: record.ciphertext.0.clone(),
                        updated_at: String::new(),
                        revision: record.revision,
                        deleted: record.deleted,
                        dirty: false,
                        synced_revision: record.revision,
                    };
                    let entry = Self::decrypt_row(key, &row).map_err(|_| VaultError::Tampered)?;
                    row.updated_at = entry
                        .as_ref()
                        .map_or_else(timestamp, |e| e.updated_at.clone());
                    last_seq = last_seq.max(record.seq);
                    match store::load_entry(&tx, &record.id)? {
                        Some(local) if !local.dirty && local.revision >= record.revision => {}
                        Some(local) if local.dirty && record.revision <= local.synced_revision => {}
                        Some(local) if local.dirty => {
                            // Both changed it: the server's copy wins, ours
                            // stays as a copy.
                            match Self::decrypt_row(key, &local) {
                                Ok(Some(mut mine)) => {
                                    mine.title.push_str(CONFLICT_SUFFIX);
                                    let id = uuid::Uuid::new_v4().to_string();
                                    store::upsert_entry(
                                        &tx,
                                        &Self::encrypt(key, &id, 1, Some(&mine))?,
                                    )?;
                                    applied.conflicts += 1;
                                }
                                // A local deletion lost: the entry comes back.
                                Ok(None) => {}
                                Err(_) => log::warn!(
                                    "a local change that cannot be decrypted lost to the server's"
                                ),
                            }
                            store::upsert_entry(&tx, &row)?;
                            applied.changed += 1;
                        }
                        _ => {
                            store::upsert_entry(&tx, &row)?;
                            applied.changed += 1;
                        }
                    }
                }
                if let Some(body) = &body {
                    for (id, listed) in &body.entries {
                        let present = store::load_entry(&tx, id)?
                            .is_some_and(|local| local.dirty || local.revision >= listed.revision);
                        if !present {
                            return Err(VaultError::Tampered);
                        }
                    }
                    state.manifest_version = state.manifest_version.max(body.version);
                }
                state.last_seq = last_seq;
                store::save_sync(&tx, &state)?;
                tx.commit()?;
                Ok(applied)
            })
        })
    }

    /// The next manifest: every record the server has as this device last
    /// agreed with it, sealed, numbered after both the last one seen and
    /// `after` (the server's, which only orders it).
    pub fn next_manifest(&self, after: u64) -> Result<Manifest, VaultError> {
        self.with_key_passive(|key| {
            let state = self.sync_state()?.ok_or_else(not_syncing)?;
            let version = state
                .manifest_version
                .max(after)
                .checked_add(1)
                .ok_or(VaultError::Tampered)?;
            let rows = self.db.with_conn(store::load_entries)?;
            let entries = rows
                .iter()
                // A row changed here is listed once the server has it.
                .filter(|r| !r.dirty && r.revision > 0)
                .map(|r| {
                    (
                        r.id.clone(),
                        ManifestEntry {
                            revision: r.revision,
                            digest: record_digest(&r.nonce, &r.ciphertext),
                        },
                    )
                })
                .collect();
            seal_manifest(key, &ManifestBody { version, entries })
        })
    }

    /// The server stored this device's manifest `version`.
    pub fn manifest_stored(&self, version: u64) -> Result<(), VaultError> {
        self.db.with_conn(|c| {
            let mut state = store::load_sync(c)?.ok_or_else(not_syncing)?;
            state.manifest_version = state.manifest_version.max(version);
            store::save_sync(c, &state)?;
            Ok::<_, VaultError>(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use clipboardx_sync_proto::RecordDown;

    use super::*;
    use crate::db::Database;
    use crate::vault::account_key::AccountKey;
    use crate::vault::keychain::MemoryKeyStore;
    use crate::vault::recovery::RecoveryKey;
    use crate::vault::tests::entry;
    use crate::vault::{VaultStatus, now_ms};

    const PW: &str = "correct horse battery";
    const NEW_PW: &str = "brand new pass phrase";

    /// The server, as the clients see it: what /changes and /vault keep.
    #[derive(Default)]
    struct Server {
        records: BTreeMap<String, RecordDown>,
        seq: u64,
        header: Option<VaultHeader>,
        manifest: Option<Manifest>,
    }

    impl Server {
        fn push(&mut self, records: &[RecordUp]) -> Vec<String> {
            let mut conflicts = Vec::new();
            for r in records {
                let current = self.records.get(&r.id).map_or(0, |d| d.revision);
                if current != r.base_revision {
                    conflicts.push(r.id.clone());
                    continue;
                }
                self.seq += 1;
                self.records.insert(
                    r.id.clone(),
                    RecordDown {
                        id: r.id.clone(),
                        revision: r.revision,
                        deleted: r.deleted,
                        nonce: r.nonce.clone(),
                        ciphertext: r.ciphertext.clone(),
                        seq: self.seq,
                    },
                );
            }
            conflicts
        }

        fn changes(&self, since: u64) -> Changes {
            Changes {
                records: self
                    .records
                    .values()
                    .filter(|r| r.seq > since)
                    .cloned()
                    .collect(),
                seq: self.seq,
            }
        }

        fn manifest_version(&self) -> u64 {
            self.manifest.as_ref().map_or(0, |m| m.version)
        }
    }

    fn service() -> VaultService {
        VaultService::with_params(
            Arc::new(Database::in_memory().unwrap()),
            Box::new(MemoryKeyStore::default()),
            KdfParams::INSECURE_TEST,
        )
    }

    /// One sync of `v` with `server`, in the order of `cloud::sync_once`.
    fn sync(v: &VaultService, server: &mut Server) -> Result<Applied, VaultError> {
        if v.has_staged_header() {
            return Err(VaultError::HeaderChanged);
        }
        let local = v.header_version()?;
        match server.header.clone() {
            Some(remote) if remote.version > local => {
                if v.receive_header(&remote)? {
                    return Err(VaultError::HeaderChanged);
                }
            }
            Some(remote) if remote.version == local && !v.is_own_header(&remote)? => {
                v.resolve_header_conflict(&remote)?;
            }
            header if header.as_ref().is_none_or(|h| h.version < local) => {
                server.header = Some(v.header()?);
                let manifest = v.next_manifest(server.manifest_version())?;
                v.manifest_stored(manifest.version)?;
                server.manifest = Some(manifest);
            }
            _ => {}
        }
        let pending = v.pending()?;
        let conflicts = server.push(&pending);
        v.mark_pushed(&pending, &conflicts)?;
        let since = v.sync_state()?.unwrap().last_seq;
        let applied = match v.apply(&server.changes(since), server.manifest.as_ref()) {
            Err(VaultError::Tampered) if since > 0 => {
                v.reset_sync_cursor()?;
                v.apply(&server.changes(0), server.manifest.as_ref())?
            }
            result => result?,
        };
        let manifest = v.next_manifest(server.manifest_version())?;
        v.manifest_stored(manifest.version)?;
        server.manifest = Some(manifest);
        // Copies made by conflicts go up at once.
        let copies = v.pending()?;
        if !copies.is_empty() {
            let refused = server.push(&copies);
            v.mark_pushed(&copies, &refused)?;
        }
        Ok(applied)
    }

    /// A first device with sync on, and a second that joined.
    fn pair() -> (VaultService, VaultService, Server, AccountKey) {
        let mut server = Server::default();
        let a = service();
        a.setup(PW).unwrap();
        let key = AccountKey::generate().unwrap();
        a.enable_account_key(PW, &key).unwrap();
        a.start_sync("http://localhost:1", "a", "token-a").unwrap();
        sync(&a, &mut server).unwrap();

        let b = service();
        b.start_sync("http://localhost:1", "b", "token-b").unwrap();
        b.join_header(server.header.as_ref().unwrap()).unwrap();
        assert!(b.needs_account_key());
        assert_eq!(
            b.unlock(PW, now_ms()).unwrap_err(),
            VaultError::SecretKeyNeeded
        );
        b.unlock_with_account_key(PW, &key.encode(), now_ms())
            .unwrap();
        sync(&b, &mut server).unwrap();
        (a, b, server, key)
    }

    fn titles(v: &VaultService) -> Vec<String> {
        let mut titles: Vec<String> = v.list().unwrap().into_iter().map(|s| s.title).collect();
        titles.sort();
        titles
    }

    #[test]
    fn entries_edits_and_deletions_travel() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("GitHub", "pw-1")).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        assert_eq!(b.get(&id).unwrap().password, "pw-1");

        let mut edited = b.get(&id).unwrap();
        edited.password = "pw-2".into();
        b.save(Some(&id), edited).unwrap();
        sync(&b, &mut server).unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(a.get(&id).unwrap().password, "pw-2");

        a.delete(&id).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        assert!(titles(&b).is_empty());
    }

    #[test]
    fn entries_from_before_sync_go_up() {
        let mut server = Server::default();
        let a = service();
        a.setup(PW).unwrap();
        a.save(None, entry("old", "pw")).unwrap();
        a.enable_account_key(PW, &AccountKey::generate().unwrap())
            .unwrap();
        a.start_sync("http://localhost:1", "a", "t").unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(server.records.len(), 1);
    }

    #[test]
    fn concurrent_edits_keep_both() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("Mail", "base")).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();

        let mut on_a = a.get(&id).unwrap();
        on_a.password = "from-a".into();
        a.save(Some(&id), on_a).unwrap();
        let mut on_b = b.get(&id).unwrap();
        on_b.password = "from-b".into();
        b.save(Some(&id), on_b).unwrap();

        sync(&a, &mut server).unwrap();
        let applied = sync(&b, &mut server).unwrap();
        assert_eq!(applied.conflicts, 1);
        assert_eq!(b.get(&id).unwrap().password, "from-a");
        assert_eq!(
            titles(&b),
            vec!["Mail".to_owned(), format!("Mail{CONFLICT_SUFFIX}")]
        );

        sync(&a, &mut server).unwrap();
        assert_eq!(titles(&a).len(), 2);
    }

    #[test]
    fn rollback_withholding_replays_and_swaps_are_refused() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("x", "v1")).unwrap();
        sync(&a, &mut server).unwrap();
        // A manifest b will have moved past.
        let old_manifest = server.manifest.clone();
        sync(&b, &mut server).unwrap();
        let old_record = server.records[&id].clone();

        let mut edited = a.get(&id).unwrap();
        edited.password = "v2".into();
        a.save(Some(&id), edited).unwrap();
        sync(&a, &mut server).unwrap();
        let since = b.sync_state().unwrap().unwrap().last_seq;
        let refused = |changes: &Changes, manifest: Option<&Manifest>| {
            assert_eq!(
                b.apply(changes, manifest).unwrap_err(),
                VaultError::Tampered
            );
        };

        // An older manifest; none at all.
        refused(&server.changes(since), old_manifest.as_ref());
        refused(&server.changes(since), None);
        // The record withheld.
        refused(
            &Changes {
                records: vec![],
                seq: server.seq,
            },
            server.manifest.as_ref(),
        );
        // The old revision replayed as new.
        let mut replay = old_record.clone();
        replay.seq = server.seq + 1;
        refused(
            &Changes {
                records: vec![replay],
                seq: server.seq + 1,
            },
            server.manifest.as_ref(),
        );
        // The old ciphertext with the new revision number.
        let mut relabeled = old_record.clone();
        relabeled.revision = server.records[&id].revision;
        refused(
            &Changes {
                records: vec![relabeled],
                seq: server.seq,
            },
            server.manifest.as_ref(),
        );
        // Revision 0: authenticated by the id alone.
        let mut zero = server.records[&id].clone();
        zero.revision = 0;
        refused(
            &Changes {
                records: vec![zero],
                seq: server.seq,
            },
            server.manifest.as_ref(),
        );

        // Nothing changed on b; the honest sync still works.
        assert_eq!(b.get(&id).unwrap().password, "v1");
        sync(&b, &mut server).unwrap();
        assert_eq!(b.get(&id).unwrap().password, "v2");
    }

    #[test]
    fn another_ciphertext_of_the_listed_revision_is_refused() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("x", "real")).unwrap();
        sync(&a, &mut server).unwrap();
        // An authentic but different ciphertext of the same revision: what a
        // server could keep from a write it refused.
        let current =
            a.db.with_conn(|c| store::load_entry(c, &id))
                .unwrap()
                .unwrap();
        let mut other = entry("x", "other");
        other.updated_at = "t".into();
        let twin = a
            .with_key_passive(|k| VaultService::encrypt(k, &id, current.revision, Some(&other)))
            .unwrap();
        let mut swapped = server.records[&id].clone();
        swapped.nonce = Bytes(twin.nonce);
        swapped.ciphertext = Bytes(twin.ciphertext);
        let changes = Changes {
            records: vec![swapped],
            seq: server.seq,
        };
        assert_eq!(
            b.apply(&changes, server.manifest.as_ref()).unwrap_err(),
            VaultError::Tampered
        );
    }

    #[test]
    fn a_forged_deletion_is_refused() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("x", "v1")).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        let mut forged = server.records[&id].clone();
        forged.deleted = true;
        forged.revision += 1;
        forged.seq = server.seq + 1;
        let changes = Changes {
            records: vec![forged],
            seq: server.seq + 1,
        };
        assert_eq!(
            b.apply(&changes, server.manifest.as_ref()).unwrap_err(),
            VaultError::Tampered
        );
    }

    #[test]
    fn a_runaway_cursor_recovers_with_a_full_pull() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("x", "v1")).unwrap();
        sync(&a, &mut server).unwrap();
        // The server numbers a record absurdly far ahead.
        server.records.get_mut(&id).unwrap().seq = u64::MAX - 1;
        sync(&b, &mut server).unwrap();
        assert!(b.sync_state().unwrap().unwrap().last_seq > 1 << 60);
        // Later changes have ordinary numbers: b still gets them.
        let mut edited = a.get(&id).unwrap();
        edited.password = "v2".into();
        a.save(Some(&id), edited).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        assert_eq!(b.get(&id).unwrap().password, "v2");
    }

    #[test]
    fn manifest_numbers_cannot_overflow() {
        let (_, b, mut server, _) = pair();
        assert_eq!(b.next_manifest(u64::MAX).unwrap_err(), VaultError::Tampered);
        // A server claiming such a manifest gets nothing stored.
        let before = b.sync_state().unwrap().unwrap().manifest_version;
        server.manifest = Some(Manifest {
            version: u64::MAX,
            sealed: Bytes(vec![0; 64]),
        });
        assert_eq!(sync(&b, &mut server).unwrap_err(), VaultError::Tampered);
        assert_eq!(b.sync_state().unwrap().unwrap().manifest_version, before);
    }

    #[test]
    fn a_new_master_password_is_taken_while_unlocked_keeping_local_edits() {
        let (a, b, mut server, _) = pair();
        let id = a.save(None, entry("shared", "keep")).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        // b changes something it has not uploaded when a changes the password.
        let local = b.save(None, entry("only on b", "draft")).unwrap();
        a.change_password(PW, NEW_PW).unwrap();
        sync(&a, &mut server).unwrap();

        assert_eq!(
            sync(&b, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        // The header waits apart; b keeps working with its own.
        assert!(b.has_staged_header());
        assert_eq!(b.status(), VaultStatus::Unlocked);
        assert_eq!(
            b.adopt_staged_header(PW).unwrap_err(),
            VaultError::WrongPassword
        );
        b.adopt_staged_header(NEW_PW).unwrap();
        assert!(!b.has_staged_header());
        sync(&b, &mut server).unwrap();
        assert_eq!(b.get(&id).unwrap().password, "keep");
        // b's draft was sealed again under the new key and reached a.
        sync(&a, &mut server).unwrap();
        assert_eq!(a.get(&local).unwrap().password, "draft");
        b.lock();
        b.unlock(NEW_PW, now_ms()).unwrap();
    }

    #[test]
    fn a_new_master_password_is_taken_at_the_next_unlock() {
        let (a, b, mut server, _) = pair();
        a.change_password(PW, NEW_PW).unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(
            sync(&b, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        b.lock();
        // The old password still opens what b has: nothing was replaced.
        b.unlock(PW, now_ms()).unwrap();
        assert!(b.has_staged_header());
        b.lock();
        b.unlock(NEW_PW, now_ms()).unwrap();
        assert!(!b.has_staged_header());
        sync(&b, &mut server).unwrap();
        b.save(None, entry("after", "x")).unwrap();
        sync(&b, &mut server).unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(titles(&a), vec!["after".to_owned()]);
    }

    #[test]
    fn a_forged_header_is_refused_and_replaces_nothing() {
        let (_, b, mut server, _) = pair();
        let meta_before = b.meta().unwrap();
        let mut forged = server.header.clone().unwrap();
        forged.version += 1;
        forged.salt = Bytes(vec![7; 16]);
        // No authenticator, then one under another key.
        server.header = Some(forged.clone());
        assert_eq!(sync(&b, &mut server).unwrap_err(), VaultError::Tampered);
        forged.auth = Some(Bytes(
            header_auth(&SecretKey::random().unwrap(), &forged).unwrap(),
        ));
        server.header = Some(forged);
        assert_eq!(sync(&b, &mut server).unwrap_err(), VaultError::Tampered);
        assert!(!b.has_staged_header());
        assert_eq!(b.meta().unwrap(), meta_before);
        assert_eq!(b.status(), VaultStatus::Unlocked);
    }

    #[test]
    fn a_phrase_set_on_one_device_reaches_the_others_without_a_new_password() {
        let (a, b, mut server, _) = pair();
        a.save(None, entry("a", "1")).unwrap();
        let phrase = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &phrase).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&b, &mut server).unwrap();
        assert!(!b.has_staged_header());
        assert!(b.has_recovery_phrase());
        assert_eq!(b.header_version().unwrap(), a.header_version().unwrap());
        assert_eq!(titles(&b), vec!["a"]);

        // B changes the password: A's phrase still opens the vault.
        b.change_password(PW, NEW_PW).unwrap();
        sync(&b, &mut server).unwrap();
        assert_eq!(
            sync(&a, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        a.lock();
        a.recover(&phrase, "third pass phrase").unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(titles(&a), vec!["a"]);
        assert_eq!(
            sync(&b, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        b.lock();
        b.unlock("third pass phrase", now_ms()).unwrap();
        sync(&b, &mut server).unwrap();
        assert_eq!(titles(&b), vec!["a"]);
    }

    #[test]
    fn a_new_device_recovers_with_the_phrase_alone() {
        let (a, _b, mut server, _) = pair();
        a.save(None, entry("kept", "1")).unwrap();
        let phrase = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &phrase).unwrap();
        sync(&a, &mut server).unwrap();

        // No master password, no secret key: only the phrase.
        let c = service();
        c.start_sync("http://localhost:1", "c", "token-c").unwrap();
        c.join_header(server.header.as_ref().unwrap()).unwrap();
        assert!(c.needs_account_key());
        c.recover(&phrase, NEW_PW).unwrap();
        assert!(!c.needs_account_key());
        sync(&c, &mut server).unwrap();
        assert_eq!(titles(&c), vec!["kept"]);

        // The others take the new password.
        assert_eq!(
            sync(&a, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        a.lock();
        a.unlock(NEW_PW, now_ms()).unwrap();
        sync(&a, &mut server).unwrap();
        a.save(None, entry("from a", "2")).unwrap();
        sync(&a, &mut server).unwrap();
        sync(&c, &mut server).unwrap();
        assert_eq!(titles(&c), vec!["from a", "kept"]);
    }

    #[test]
    fn a_server_cannot_strip_or_split_the_phrase() {
        let (a, b, mut server, _) = pair();
        let phrase = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &phrase).unwrap();
        sync(&a, &mut server).unwrap();
        let good = server.header.clone().unwrap();
        // Stripped (so B's next password change would drop the phrase).
        let stripped = VaultHeader {
            recovery_wrap: None,
            recovery_sealed: None,
            ..good.clone()
        };
        assert_eq!(
            b.receive_header(&stripped).unwrap_err(),
            VaultError::Tampered
        );
        // Half a phrase is no header of ours.
        let half = VaultHeader {
            recovery_sealed: None,
            version: good.version + 3,
            ..good.clone()
        };
        assert_eq!(b.receive_header(&half).unwrap_err(), VaultError::Tampered);
        assert!(!b.has_recovery_phrase() && !b.has_staged_header());
        assert!(!b.receive_header(&good).unwrap());
        assert!(b.has_recovery_phrase());
    }

    #[test]
    fn an_older_header_renumbered_opens_nothing() {
        let (a, b, mut server, _) = pair();
        b.save(None, entry("kept", "1")).unwrap();
        sync(&b, &mut server).unwrap();
        let phrase = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &phrase).unwrap();
        sync(&a, &mut server).unwrap();
        let old = server.header.clone().unwrap();
        // A new phrase: a new data key under the same password; then a new
        // password, the same phrase.
        let newer = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &newer).unwrap();
        sync(&a, &mut server).unwrap();
        assert_eq!(
            sync(&b, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        b.adopt_staged_header(PW).unwrap();
        sync(&b, &mut server).unwrap();
        let before = b.meta().unwrap();

        // The server passes the old header off as the next one.
        let renumbered = VaultHeader {
            version: b.header_version().unwrap() + 5,
            ..old
        };
        assert!(b.receive_header(&renumbered).unwrap());
        b.lock();
        // The password opens it, the old phrase too: neither takes it.
        b.unlock(PW, now_ms()).unwrap();
        assert_eq!(b.meta().unwrap(), before);
        assert_eq!(
            b.adopt_staged_header(PW).unwrap_err(),
            VaultError::WrongPassword
        );
        b.lock();
        assert_eq!(
            b.recover(&phrase, NEW_PW).unwrap_err(),
            VaultError::WrongRecoveryPhrase
        );
        assert_eq!(
            b.recover(&newer, NEW_PW).unwrap_err(),
            VaultError::StaleRecoveryPhrase
        );
        b.unlock(PW, now_ms()).unwrap();
        assert_eq!(titles(&b), vec!["kept"]);
    }

    #[test]
    fn a_joining_device_refuses_a_header_with_parts_swapped() {
        let (a, _b, mut server, key) = pair();
        let phrase = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &phrase).unwrap();
        sync(&a, &mut server).unwrap();
        let stripped = VaultHeader {
            recovery_wrap: None,
            recovery_sealed: None,
            ..server.header.clone().unwrap()
        };
        let c = service();
        c.start_sync("http://localhost:1", "c", "token-c").unwrap();
        c.join_header(&stripped).unwrap();
        assert_eq!(
            c.unlock_with_account_key(PW, &key.encode(), now_ms())
                .unwrap_err(),
            VaultError::Tampered
        );
        assert_eq!(c.status(), VaultStatus::Locked);

        // The whole header: checked once, then unlocking as usual.
        let d = service();
        d.start_sync("http://localhost:1", "d", "token-d").unwrap();
        d.join_header(server.header.as_ref().unwrap()).unwrap();
        d.unlock_with_account_key(PW, &key.encode(), now_ms())
            .unwrap();
        assert!(d.sync_state().unwrap().unwrap().joined_seal.is_none());
        d.lock();
        d.unlock(PW, now_ms()).unwrap();
    }

    #[test]
    fn phrases_set_at_once_on_two_devices_leave_one_and_say_so() {
        let (a, b, mut server, _) = pair();
        let first = RecoveryKey::generate().unwrap();
        let second = RecoveryKey::generate().unwrap();
        a.set_recovery_phrase(PW, &first).unwrap();
        b.set_recovery_phrase(PW, &second).unwrap();
        sync(&a, &mut server).unwrap();
        match sync(&b, &mut server).unwrap_err() {
            VaultError::Sync(why) => assert!(why.contains("助记词已失效"), "{why}"),
            other => panic!("{other:?}"),
        }
        // B took A's: one phrase for the vault, and it works on B.
        sync(&b, &mut server).unwrap();
        assert!(b.is_own_header(server.header.as_ref().unwrap()).unwrap());
        b.lock();
        assert_eq!(
            b.recover(&second, NEW_PW).unwrap_err(),
            VaultError::WrongRecoveryPhrase
        );
        b.recover(&first, NEW_PW).unwrap();
    }

    #[test]
    fn absurd_headers_are_refused_before_any_key_is_derived() {
        let (_, b, server, _) = pair();
        let good = server.header.clone().unwrap();
        let bad: Vec<VaultHeader> = vec![
            VaultHeader {
                kdf_m: u32::MAX,
                ..good.clone()
            },
            VaultHeader {
                kdf_t: 0,
                ..good.clone()
            },
            VaultHeader {
                format: 1,
                ..good.clone()
            },
            VaultHeader {
                salt: Bytes(vec![1; 4]),
                ..good.clone()
            },
            VaultHeader {
                wrapped_key: Bytes(vec![]),
                ..good.clone()
            },
        ];
        for mut header in bad {
            header.version += 5;
            assert_eq!(b.receive_header(&header).unwrap_err(), VaultError::Tampered);
        }
        assert!(!b.has_staged_header());
    }

    #[test]
    fn a_header_further_ahead_waits_and_only_its_password_opens_it() {
        let (_, b, mut server, _) = pair();
        let mut ahead = server.header.clone().unwrap();
        ahead.version += 3;
        ahead.salt = Bytes(vec![9; 16]);
        server.header = Some(ahead);
        assert_eq!(
            sync(&b, &mut server).unwrap_err(),
            VaultError::HeaderChanged
        );
        // Unverifiable: staged, but it opens with no password, so the old
        // one keeps the vault.
        b.lock();
        b.unlock(PW, now_ms()).unwrap();
        assert!(b.has_staged_header());
        assert_eq!(b.list().unwrap().len(), 0);
    }

    #[test]
    fn sync_needs_the_secret_key() {
        let v = service();
        v.setup(PW).unwrap();
        assert_eq!(
            v.start_sync("http://localhost:1", "a", "t").unwrap_err(),
            VaultError::SecretKeyNeeded
        );
    }

    #[test]
    fn joining_replaces_no_vault() {
        let (_, b, server, _) = pair();
        assert_eq!(
            b.join_header(server.header.as_ref().unwrap()).unwrap_err(),
            VaultError::AlreadySetUp
        );
    }

    #[test]
    fn a_wrong_secret_key_does_not_unlock() {
        let (_, b, _, _) = pair();
        b.lock();
        let other = AccountKey::generate().unwrap();
        assert_eq!(
            b.unlock_with_account_key(PW, &other.encode(), now_ms())
                .unwrap_err(),
            VaultError::WrongPassword
        );
        assert_eq!(
            b.unlock_with_account_key(PW, "CX-NOT-A-KEY", now_ms())
                .unwrap_err(),
            VaultError::InvalidSecretKey
        );
    }

    #[test]
    fn the_server_never_sees_plaintext_or_the_secret_key() {
        let (a, _, mut server, _) = pair();
        a.save(None, entry("PLAINTITLE", "PLAINPASS")).unwrap();
        sync(&a, &mut server).unwrap();
        let mut seen = Vec::new();
        for r in server.records.values() {
            seen.extend_from_slice(&r.ciphertext.0);
        }
        seen.extend_from_slice(&server.manifest.as_ref().unwrap().sealed.0);
        let header = server.header.as_ref().unwrap();
        seen.extend_from_slice(&header.wrapped_key.0);
        let text = String::from_utf8_lossy(&seen);
        assert!(!text.contains("PLAINTITLE") && !text.contains("PLAINPASS"));
        let key = a.keys.get(KeyItem::SecretKey).unwrap();
        assert!(!seen.windows(key.len()).any(|w| w == key.as_slice()));
    }
}
