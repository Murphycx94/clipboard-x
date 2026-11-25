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

use clipboardx_sync_proto::history::{HistoryPush, MAX_CIPHERTEXT, PAGE_BYTES};

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

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// What one round did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Whether the history here changed.
    pub changed: bool,
    /// Items uploaded.
    pub pushed: usize,
}

fn sync_error(why: &str) -> VaultError {
    VaultError::Sync(why.to_owned())
}

/// Another device changed the vault's header: the history follows once
/// this device takes it.
fn header_moved() -> VaultError {
    sync_error("密码本在其他设备上已更新，复制历史将在下次同步时继续")
}

/// The oldest copy time the retention setting keeps; `None` keeps all.
fn retention_cutoff(db: &Database) -> Result<Option<i64>, VaultError> {
    let days = db.settings()?.retention_days;
    let now = chrono::Utc::now().timestamp_millis();
    Ok((days > 0).then(|| now - days.saturating_mul(DAY_MS)))
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
    let changed = fetch(db, client, &keys, header)?;
    let pushed = upload(db, client, &keys, header)?;
    Ok(Report { changed, pushed })
}

/// Takes the server's changes; whether the history changed.
fn fetch(
    db: &Database,
    client: &Client,
    keys: &seal::HistoryKeys,
    header: u64,
) -> Result<bool, VaultError> {
    let cutoff = retention_cutoff(db)?;
    let mut since = db.with_conn(|c| ledger::begin_round(c, header))?;
    let mut changed = false;
    loop {
        let page = client.history(since, header)?.ok_or_else(header_moved)?;
        // A page is taken whole or not at all.
        let items = page
            .records
            .iter()
            .map(|r| keys.open(r))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| sync_error("复制历史的同步数据异常，本机未作改动"))?;
        changed |= db.with_conn(|c| ledger::apply_page(c, &items, cutoff))?;
        since = match page.records.last() {
            Some(last) if page.more => last.seq,
            _ => page.seq,
        };
        db.with_conn(|c| ledger::fetched_to(c, since))?;
        if !page.more {
            return Ok(changed);
        }
    }
}

/// Sends this device's changes; how many items went.
fn upload(
    db: &Database,
    client: &Client,
    keys: &seal::HistoryKeys,
    header: u64,
) -> Result<usize, VaultError> {
    let mut pushed = 0;
    for _ in 0..MAX_UPLOADS {
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
