//! What this device keeps of each synced history item besides its row in
//! `clipboard_items`: when it was copied, marked and deleted (Unix ms), and
//! whether the server has its latest state. A deleted item keeps its row
//! here, without text, so its deletion reaches the other devices.
//!
//! `Database` calls the `note_*` functions within the change they note
//! (same lock, same transaction), and only while history sync is on.

use chrono::{Local, NaiveDateTime, TimeZone};
use rusqlite::{Connection, OptionalExtension, Result, params};

use super::item::ItemState;

/// The longest text synced, in bytes; longer ones stay on this device.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

pub(crate) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS history_sync (
        hash       TEXT PRIMARY KEY,
        copied_at  INTEGER NOT NULL,
        meta_at    INTEGER NOT NULL DEFAULT 0,
        deleted_at INTEGER NOT NULL DEFAULT 0,
        local_rev  INTEGER NOT NULL DEFAULT 1,
        pushed_rev INTEGER NOT NULL DEFAULT 0
    );
";

/// Setting keys.
mod key {
    pub const ON: &str = "history_sync";
    /// The history sequence number fetched up to.
    pub const SEQ: &str = "history_sync_seq";
    /// The header version the items went up under.
    pub const HEADER: &str = "history_sync_header";
}

/// How `clipboard_items.created_at` is written.
const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f";

/// A `created_at` (local time, with or without milliseconds) as Unix ms.
pub fn ms_of_local(created_at: &str) -> i64 {
    let Ok(naive) = NaiveDateTime::parse_from_str(created_at, "%Y-%m-%dT%H:%M:%S%.f") else {
        return 0;
    };
    Local
        .from_local_datetime(&naive)
        .earliest()
        // A time a clock change skipped: as if it were UTC, hours off at worst.
        .map_or_else(|| naive.and_utc().timestamp_millis(), |t| t.timestamp_millis())
}

/// Unix ms as a `created_at`, in this device's time zone.
pub fn local_of_ms(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .earliest()
        .map_or_else(|| "1970-01-01T00:00:00.000".to_owned(), |t| {
            t.format(LOCAL_FORMAT).to_string()
        })
}

fn setting(c: &Connection, key: &str) -> Result<Option<String>> {
    c.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
}

fn set_setting(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO settings(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map(|_| ())
}

fn number_setting(c: &Connection, key: &str) -> Result<u64> {
    Ok(setting(c, key)?.and_then(|v| v.parse().ok()).unwrap_or(0))
}

pub(crate) fn is_enabled(c: &Connection) -> Result<bool> {
    Ok(setting(c, key::ON)?.as_deref() == Some("true"))
}

/// Starts keeping the ledger: every text item now in the history, all to
/// upload, and the history to fetch from the start. An item marked or
/// noted counts as changed when copied, so between two devices holding the
/// same text, the mark and the note win over their absence.
pub(crate) fn enable(c: &Connection) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    tx.execute("DELETE FROM history_sync", [])?;
    let items: Vec<(String, String, bool, String)> = tx
        .prepare(
            "SELECT hash, created_at, is_favorite, coalesce(note, '') FROM clipboard_items
             WHERE content_type = 'text' AND length(CAST(text_content AS BLOB)) <= ?1",
        )?
        .query_map(params![MAX_TEXT_BYTES as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?))
        })?
        .collect::<Result<_>>()?;
    for (hash, created_at, favorite, note) in items {
        let copied_at = ms_of_local(&created_at);
        let meta_at = if favorite || !note.is_empty() { copied_at } else { 0 };
        tx.execute(
            "INSERT INTO history_sync (hash, copied_at, meta_at) VALUES (?1, ?2, ?3)",
            params![hash, copied_at, meta_at],
        )?;
    }
    set_setting(&tx, key::ON, "true")?;
    set_setting(&tx, key::SEQ, "0")?;
    set_setting(&tx, key::HEADER, "0")?;
    tx.commit()
}

/// Stops keeping it, and forgets it; the history itself stays.
pub(crate) fn disable(c: &Connection) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    tx.execute("DELETE FROM history_sync", [])?;
    tx.execute(
        "DELETE FROM settings WHERE key IN (?1, ?2, ?3)",
        params![key::ON, key::SEQ, key::HEADER],
    )?;
    tx.commit()
}

// --- Noting this device's changes -----------------------------------------

/// Whether the item with `hash` is a text history sync covers.
fn is_synced_text(c: &Connection, hash: &str) -> Result<bool> {
    let bytes: Option<Option<i64>> = c
        .query_row(
            "SELECT length(CAST(text_content AS BLOB)) FROM clipboard_items
             WHERE hash = ?1 AND content_type = 'text'",
            params![hash],
            |r| r.get(0),
        )
        .optional()?;
    Ok(bytes
        .flatten()
        .is_some_and(|n| usize::try_from(n).is_ok_and(|n| n <= MAX_TEXT_BYTES)))
}

/// `hash` was copied (again) at `at`: alive, even after a deletion.
pub(crate) fn note_copied(c: &Connection, hash: &str, at: i64) -> Result<()> {
    if !is_synced_text(c, hash)? {
        return Ok(());
    }
    c.execute(
        "INSERT INTO history_sync (hash, copied_at) VALUES (?1, ?2)
         ON CONFLICT(hash) DO UPDATE SET
             copied_at = max(copied_at, excluded.copied_at, deleted_at + 1),
             local_rev = local_rev + 1",
        params![hash, at],
    )
    .map(|_| ())
}

/// The mark or the note of `hash` changed at `at`: later than any change
/// before, so it wins here whatever the clock says.
pub(crate) fn note_marked(c: &Connection, hash: &str, at: i64) -> Result<()> {
    if !is_synced_text(c, hash)? {
        return Ok(());
    }
    c.execute(
        "INSERT INTO history_sync (hash, copied_at, meta_at) VALUES (?1, ?2, ?2)
         ON CONFLICT(hash) DO UPDATE SET
             meta_at = max(meta_at + 1, excluded.meta_at, deleted_at + 1),
             local_rev = local_rev + 1",
        params![hash, at],
    )
    .map(|_| ())
}

/// `hash` is about to be deleted at `at` (called while its row is still
/// there): later than anything it saw, so the deletion wins here.
pub(crate) fn note_deleted(c: &Connection, hash: &str, at: i64) -> Result<()> {
    if !is_synced_text(c, hash)? {
        return Ok(());
    }
    c.execute(
        "INSERT INTO history_sync (hash, copied_at, meta_at, deleted_at) VALUES (?1, ?2, ?2, ?2)
         ON CONFLICT(hash) DO UPDATE SET
             deleted_at = max(excluded.deleted_at, copied_at, meta_at + 1, deleted_at),
             meta_at = max(excluded.deleted_at, copied_at, meta_at + 1, deleted_at),
             local_rev = local_rev + 1",
        params![hash, at],
    )
    .map(|_| ())
}

/// Every item but the favorites is about to be deleted at `at`.
pub(crate) fn note_cleared(c: &Connection, at: i64) -> Result<()> {
    let hashes: Vec<String> = c
        .prepare("SELECT hash FROM clipboard_items WHERE is_favorite = 0 AND content_type = 'text'")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    hashes.iter().try_for_each(|hash| note_deleted(c, hash, at))
}

/// The items the retention setting is about to remove (non-favorites
/// copied before `cutoff`, a `created_at`) leave the ledger: removed here
/// only, nothing to tell the others.
pub(crate) fn forget_expired(c: &Connection, cutoff: &str) -> Result<()> {
    c.execute(
        "DELETE FROM history_sync WHERE hash IN (
             SELECT hash FROM clipboard_items WHERE is_favorite = 0 AND created_at < ?1)",
        params![cutoff],
    )
    .map(|_| ())
}

// --- Syncing ----------------------------------------------------------------

/// This device's state of an item, and its place in the uploads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalItem {
    pub state: ItemState,
    pub local_rev: i64,
    pub pushed_rev: i64,
}

type Row = (
    Option<(i64, i64, i64, i64, i64)>,
    Option<(String, bool, String, String)>,
);

/// What this device has of `hash`: its ledger row and, while it is in the
/// history, its text, mark and note. An item gone from the history without
/// a deletion noted (removed while sync was off) counts as deleted then.
pub(crate) fn local_item(c: &Connection, hash: &str) -> Result<Option<LocalItem>> {
    let ledger = c
        .query_row(
            "SELECT copied_at, meta_at, deleted_at, local_rev, pushed_rev
             FROM history_sync WHERE hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let item = c
        .query_row(
            "SELECT text_content, is_favorite, coalesce(note, ''), created_at FROM clipboard_items
             WHERE hash = ?1 AND content_type = 'text' AND text_content IS NOT NULL",
            params![hash],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(combine(hash, (ledger, item)))
}

fn combine(hash: &str, row: Row) -> Option<LocalItem> {
    let state = |copied_at, meta_at, deleted_at, item: Option<(String, bool, String)>| {
        let (text, favorite, note) = match item {
            Some((text, favorite, note)) => (Some(text), favorite, note),
            None => (None, false, String::new()),
        };
        ItemState {
            hash: hash.to_owned(),
            text,
            copied_at,
            meta_at,
            favorite,
            note,
            deleted_at,
        }
    };
    match row {
        (None, None) => None,
        (None, Some((text, favorite, note, created_at))) => {
            let copied_at = ms_of_local(&created_at);
            let meta_at = if favorite || !note.is_empty() { copied_at } else { 0 };
            Some(LocalItem {
                state: state(copied_at, meta_at, 0, Some((text, favorite, note))),
                local_rev: 0,
                pushed_rev: 0,
            })
        }
        (Some((copied_at, meta_at, deleted_at, local_rev, pushed_rev)), item) => {
            let item = item.map(|(text, favorite, note, _)| (text, favorite, note));
            let alive = copied_at.max(meta_at) > deleted_at;
            let state = match (item, alive) {
                (Some(item), true) => state(copied_at, meta_at, deleted_at, Some(item)),
                (_, false) => state(copied_at, meta_at, deleted_at, None),
                // Alive in the ledger, gone from the history.
                (None, true) => {
                    let gone = copied_at.max(meta_at);
                    state(copied_at, gone, gone, None)
                }
            };
            Some(LocalItem {
                state,
                local_rev,
                pushed_rev,
            })
        }
    }
}

/// Writes `state` to the history: its row with its text, time, mark and
/// note while alive; no row once deleted.
fn write_item(c: &Connection, state: &ItemState) -> Result<()> {
    match (&state.text, state.alive()) {
        (Some(text), true) => c.execute(
            "INSERT INTO clipboard_items (content_type, text_content, created_at, is_favorite, note, hash)
             VALUES ('text', ?1, ?2, ?3, nullif(?4, ''), ?5)
             ON CONFLICT(hash) DO UPDATE SET
                 created_at = excluded.created_at,
                 is_favorite = excluded.is_favorite,
                 note = excluded.note",
            params![
                text,
                local_of_ms(state.copied_at),
                state.favorite,
                state.note,
                state.hash
            ],
        ),
        // Never so: what is alive somewhere came with its text.
        (None, true) => return Ok(()),
        (_, false) => c.execute(
            "DELETE FROM clipboard_items WHERE hash = ?1",
            params![state.hash],
        ),
    }
    .map(|_| ())
}

/// Records `state` in the ledger: as what the server has (`synced`), or
/// still to upload.
fn save_row(c: &Connection, state: &ItemState, synced: bool) -> Result<()> {
    c.execute(
        "INSERT INTO history_sync (hash, copied_at, meta_at, deleted_at, local_rev, pushed_rev)
         VALUES (?1, ?2, ?3, ?4, 1, CASE WHEN ?5 THEN 1 ELSE 0 END)
         ON CONFLICT(hash) DO UPDATE SET
             copied_at = excluded.copied_at,
             meta_at = excluded.meta_at,
             deleted_at = excluded.deleted_at,
             local_rev = CASE
                 WHEN ?5 THEN local_rev
                 WHEN local_rev = pushed_rev THEN local_rev + 1
                 ELSE local_rev END,
             pushed_rev = CASE WHEN ?5 THEN local_rev ELSE pushed_rev END",
        params![
            state.hash,
            state.copied_at,
            state.meta_at,
            state.deleted_at,
            synced
        ],
    )
    .map(|_| ())
}

/// Takes the server's state of an item: merges it with this device's and
/// writes the merge to the history (to upload when it differs from the
/// server's). Not an item this device never had that is deleted, or
/// older than `cutoff_ms` (the retention setting) and not a favorite.
/// Whether the history changed.
pub(crate) fn apply_remote(c: &Connection, remote: &ItemState, cutoff_ms: Option<i64>) -> Result<bool> {
    let Some(local) = local_item(c, &remote.hash)? else {
        let expired = !remote.favorite && cutoff_ms.is_some_and(|cut| remote.copied_at < cut);
        if !remote.alive() || expired {
            return Ok(false);
        }
        write_item(c, remote)?;
        save_row(c, remote, true)?;
        return Ok(true);
    };
    let merged = local.state.merge(remote);
    let changed = merged != local.state;
    if changed {
        write_item(c, &merged)?;
    }
    save_row(c, &merged, merged == *remote)?;
    Ok(changed)
}

/// An item to upload, at the ledger revision read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub state: ItemState,
    pub rev: i64,
}

/// Items to upload, about `budget` bytes of text and notes (at least one).
pub(crate) fn pending(c: &Connection, budget: usize) -> Result<Vec<Pending>> {
    let hashes: Vec<String> = c
        .prepare("SELECT hash FROM history_sync WHERE local_rev != pushed_rev ORDER BY rowid")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let mut batch = Vec::new();
    let mut bytes = 0;
    for hash in hashes {
        let Some(item) = local_item(c, &hash)? else {
            continue;
        };
        let size = item.state.text.as_ref().map_or(0, String::len) + item.state.note.len();
        if !batch.is_empty() && bytes + size > budget {
            break;
        }
        bytes += size;
        batch.push(Pending {
            state: item.state,
            rev: item.local_rev,
        });
    }
    Ok(batch)
}

/// The server has `pushed` at the revisions read; changed since, they
/// stay to upload.
pub(crate) fn mark_pushed(c: &Connection, pushed: &[Pending]) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    for item in pushed {
        tx.execute(
            "UPDATE history_sync SET pushed_rev = ?2 WHERE hash = ?1",
            params![item.state.hash, item.rev],
        )?;
    }
    tx.commit()
}

/// Before a round under header version `header`: what went up under
/// another (or nothing yet) goes up again, and the history comes down from
/// the start. The sequence number to fetch after.
pub(crate) fn begin_round(c: &Connection, header: u64) -> Result<u64> {
    if number_setting(c, key::HEADER)? == header {
        return number_setting(c, key::SEQ);
    }
    let tx = c.unchecked_transaction()?;
    tx.execute("UPDATE history_sync SET pushed_rev = -1", [])?;
    set_setting(&tx, key::SEQ, "0")?;
    set_setting(&tx, key::HEADER, &header.to_string())?;
    tx.commit()?;
    Ok(0)
}

/// The history is fetched up to `seq`.
pub(crate) fn fetched_to(c: &Connection, seq: u64) -> Result<()> {
    set_setting(c, key::SEQ, &seq.to_string())
}

/// Applies the server's states in one transaction; whether the history
/// changed.
pub(crate) fn apply_page(c: &Connection, items: &[ItemState], cutoff_ms: Option<i64>) -> Result<bool> {
    let tx = c.unchecked_transaction()?;
    let mut changed = false;
    for item in items {
        changed |= apply_remote(&tx, item, cutoff_ms)?;
    }
    tx.commit()?;
    Ok(changed)
}

#[cfg(test)]
mod tests;
