//! Syncing the clipboard history's text items between devices, through the
//! vault's sync server: each item's state sealed with a key derived from
//! the vault's data key, so only while the vault is unlocked.
//!
//! - [`item`]: an item's state and how two of them merge.
//! - [`seal`]: the keys, sealing and checking items.
//! - `ledger`: what this device noted of its items, and applying others'.

pub mod item;
pub(crate) mod ledger;
pub mod seal;

use clipboardx_sync_proto::history::{HistoryDown, HistoryPush, MAX_CIPHERTEXT, PAGE_BYTES};

use self::item::ItemState;
use self::seal::HistoryKeys;
use crate::cloud::Client;
use crate::db::Database;
use crate::vault::{VaultError, VaultService};

/// The text (and notes) one upload carries at most: sealed and in Base64,
/// with every character escaped in JSON, still within what the server
/// takes in one request.
const UPLOAD_BYTES: usize = PAGE_BYTES / 2;

/// Uploads in one round at most: a device copying all the time does not
/// keep the round going for ever.
const MAX_UPLOADS: usize = 64;

/// Pages fetched in one round at most: a server that never says "no
/// more" does not keep the round going for ever.
const MAX_PAGES: usize = 1000;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How far ahead of this device's clock another device's may be: a time
/// beyond it would win every merge for years.
const CLOCK_SLACK_MS: i64 = DAY_MS;

/// How long a deletion the server has stays in the ledger: long enough to
/// settle races with devices that were away, not for ever (it keeps the
/// deleted text's hash).
const KEEP_DELETIONS_MS: i64 = 30 * DAY_MS;

/// What one round did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Whether the history here changed.
    pub changed: bool,
    /// Items uploaded.
    pub pushed: usize,
    /// Items the server held that did not open, or had impossible times:
    /// left out, and this device's state of them sent again.
    pub skipped: usize,
}

fn sync_error(why: &str) -> VaultError {
    VaultError::Sync(why.to_owned())
}

/// Another device changed the vault's header: the history follows once
/// this device takes it.
fn header_moved() -> VaultError {
    sync_error("密码本在其他设备上已更新，复制历史将在下次同步时继续")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The oldest copy time the retention setting keeps; `None` keeps all.
fn retention_cutoff(db: &Database) -> Result<Option<i64>, VaultError> {
    let days = db.settings()?.retention_days;
    Ok((days > 0).then(|| now_ms() - days.saturating_mul(DAY_MS)))
}

/// One round of history sync, when it is on here: the server's changes
/// down (checked, merged, written), then this device's up.
pub fn sync_once(
    vault: &VaultService,
    db: &Database,
    client: &Client,
) -> Result<Report, VaultError> {
    if !db.history_sync_enabled() {
        return Ok(Report::default());
    }
    let header = vault.header_version()?;
    let keys = vault.history_keys()?;
    // A new master password between the two reads: the key is not that
    // header's.
    if vault.header_version()? != header {
        return Err(header_moved());
    }
    db.with_conn(|c| ledger::forget_deletions_before(c, now_ms() - KEEP_DELETIONS_MS))?;
    let (changed, skipped) = fetch(db, client, &keys, header)?;
    let pushed = upload(db, client, &keys, header)?;
    Ok(Report {
        changed,
        pushed,
        skipped,
    })
}

/// `record` opened, if its times are ones a device could have made.
fn open(keys: &HistoryKeys, record: &HistoryDown, latest: i64) -> Option<ItemState> {
    let item = keys.open(record).ok()?;
    let times = [item.copied_at, item.meta_at, item.deleted_at];
    times
        .iter()
        .all(|t| (0..=latest).contains(t))
        .then_some(item)
}

/// This device's states of the items `ids` name go up again, over what
/// the server holds of them.
fn send_again(db: &Database, keys: &HistoryKeys, ids: &[&str]) -> Result<(), VaultError> {
    for hash in db.with_conn(ledger::hashes)? {
        if ids.contains(&keys.id_of(&hash)?.as_str()) {
            db.with_conn(|c| ledger::send_again(c, &hash))?;
        }
    }
    Ok(())
}

/// Takes the server's changes: whether the history changed, and how many
/// items were left out.
fn fetch(
    db: &Database,
    client: &Client,
    keys: &HistoryKeys,
    header: u64,
) -> Result<(bool, usize), VaultError> {
    let cutoff = retention_cutoff(db)?;
    let mut since = db.with_conn(|c| ledger::begin_round(c, header))?;
    let (mut changed, mut skipped) = (false, 0);
    for _ in 0..MAX_PAGES {
        if !db.history_sync_enabled() {
            break;
        }
        let page = client.history(since, header)?.ok_or_else(header_moved)?;
        if page.seq < since {
            log::warn!(
                "the server's history went back to {}: syncing it again",
                page.seq
            );
            since = db.with_conn(ledger::restart)?;
            continue;
        }
        let latest = now_ms() + CLOCK_SLACK_MS;
        let mut items = Vec::with_capacity(page.records.len());
        let mut bad = Vec::new();
        for record in &page.records {
            match open(keys, record, latest) {
                Some(item) => items.push(item),
                None => bad.push(record.id.as_str()),
            }
        }
        changed |= db.with_conn(|c| ledger::apply_page(c, &items, cutoff))?;
        if !bad.is_empty() {
            log::warn!(
                "{} history items from the server do not open: left out",
                bad.len()
            );
            skipped += bad.len();
            send_again(db, keys, &bad)?;
        }
        let next = match page.records.last() {
            Some(last) if page.more => last.seq,
            _ => page.seq,
        };
        if page.more && next <= since {
            return Err(sync_error("同步服务器返回的复制历史不完整"));
        }
        since = next;
        db.with_conn(|c| ledger::fetched_to(c, since))?;
        if !page.more {
            break;
        }
    }
    Ok((changed, skipped))
}

/// Sends this device's changes; how many items went.
fn upload(
    db: &Database,
    client: &Client,
    keys: &HistoryKeys,
    header: u64,
) -> Result<usize, VaultError> {
    let mut pushed = 0;
    for _ in 0..MAX_UPLOADS {
        if !db.history_sync_enabled() {
            break;
        }
        let batch = db.with_conn(|c| ledger::pending(c, UPLOAD_BYTES))?;
        if batch.is_empty() {
            break;
        }
        let mut records = Vec::with_capacity(batch.len());
        for item in &batch {
            let record = keys.seal(&item.state)?;
            if record.ciphertext.0.len() > MAX_CIPHERTEXT {
                // Never taken: noted as sent, not tried again each round.
                log::warn!("a history item is too large to sync once sealed");
                continue;
            }
            records.push(record);
        }
        if !records.is_empty() {
            let request = HistoryPush {
                header_version: header,
                records,
            };
            client.push_history(&request)?.ok_or_else(header_moved)?;
            pushed += request.records.len();
        }
        db.with_conn(|c| ledger::mark_pushed(c, &batch))?;
    }
    Ok(pushed)
}
