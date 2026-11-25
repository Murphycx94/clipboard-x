//! The SQLite store of the history and the settings. The schema is the
//! Tauri version's, so an existing `clipboardx.db` keeps working.
//!
//! While history sync is on, every change to an item is noted in its
//! ledger ([`ledger`]) in the same transaction.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Local;
use rusqlite::{Connection, OptionalExtension, Result, Row, params};

use crate::history_sync::ledger;

use crate::model::{
    ClipboardItem, ContentType, DEFAULT_HOTKEY, DateGroup, Settings, ThemePreference, group_by_date,
};

/// Characters of text that lists load.
const PREVIEW_CHARS: i64 = 200;
/// `PRAGMA auto_vacuum` when the pages deleted items leave free go back to
/// the disk on request ([`Database::reclaim_space`]), not on every delete.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;
/// The bytes the WAL file is cut back to after a checkpoint: without it, it
/// keeps the size of the largest write ever made (a vacuum's, an image's).
const WAL_SIZE_LIMIT: i64 = 4 * 1024 * 1024;
/// The items of today (`?1`, as `YYYY-MM-DD`) and those before, by the
/// order of the timestamps' text, which is that of time: comparing the
/// column itself (not `date()` of it) lets the index on it find them.
const TODAY: &str = "created_at >= ?1";
const BEFORE_TODAY: &str = "created_at < ?1";

const SCHEMA: &str = "
    PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS clipboard_items (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        content_type    TEXT NOT NULL,
        text_content    TEXT,
        image_data      BLOB,
        image_thumbnail BLOB,
        created_at      TEXT NOT NULL,
        is_favorite     INTEGER NOT NULL DEFAULT 0,
        note            TEXT,
        hash            TEXT NOT NULL UNIQUE
    );
    CREATE INDEX IF NOT EXISTS idx_created_at ON clipboard_items(created_at);
    CREATE INDEX IF NOT EXISTS idx_favorite ON clipboard_items(is_favorite);
    CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
";

/// Setting keys.
mod key {
    pub const HOTKEY: &str = "hotkey";
    pub const RETENTION_DAYS: &str = "retention_days";
    pub const TELEGRAM_TOKEN: &str = "telegram_token";
    pub const TELEGRAM_CHAT_ID: &str = "telegram_chat_id";
    pub const THEME: &str = "theme";
    pub const HIDE_ON_BLUR: &str = "hide_on_blur";
    pub const HIDE_FROM_CAPTURE: &str = "hide_from_capture";
    pub const THUMBNAIL_VERSION: &str = "thumbnail_version";
    pub const VAULT_LOCK_MINUTES: &str = "vault_lock_minutes";
    pub const VAULT_LOCK_ON_HIDE: &str = "vault_lock_on_hide";
    pub const VAULT_TOUCH_ID: &str = "vault_touch_id";
}

/// The full content of an item, to copy it back or send it.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemContent {
    Text(String),
    /// PNG bytes.
    Image(Vec<u8>),
}

pub struct Database {
    conn: Mutex<Connection>,
    /// Whether changes to items are noted for history sync.
    syncs_history: AtomicBool,
}

/// Now, as history sync counts time.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The hash of item `id`; `None` when it is gone.
fn hash_of(conn: &Connection, id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT hash FROM clipboard_items WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
}

impl Database {
    /// Opens (or creates) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// A database in memory, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // Before any table exists: a new database starts incremental.
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT)?;
        make_incremental(&conn);
        conn.execute_batch(crate::vault::store::SCHEMA)?;
        crate::vault::store::migrate(&conn)?;
        conn.execute_batch(ledger::SCHEMA)?;
        let syncs_history = ledger::is_enabled(&conn)?;
        let defaults = Settings::default();
        for (key, value) in [
            (key::HOTKEY, DEFAULT_HOTKEY.to_owned()),
            (key::RETENTION_DAYS, defaults.retention_days.to_string()),
            (key::TELEGRAM_TOKEN, String::new()),
            (key::TELEGRAM_CHAT_ID, String::new()),
            (key::THEME, defaults.theme.as_str().to_owned()),
            (key::HIDE_ON_BLUR, defaults.hide_on_blur.to_string()),
            (
                key::HIDE_FROM_CAPTURE,
                defaults.hide_from_capture.to_string(),
            ),
            (
                key::VAULT_LOCK_MINUTES,
                defaults.vault_lock_minutes.to_string(),
            ),
            (
                key::VAULT_LOCK_ON_HIDE,
                defaults.vault_lock_on_hide.to_string(),
            ),
            (key::VAULT_TOUCH_ID, defaults.vault_touch_id.to_string()),
        ] {
            conn.execute(
                "INSERT OR IGNORE INTO settings(key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        Ok(Database {
            conn: Mutex::new(conn),
            syncs_history: AtomicBool::new(syncs_history),
        })
    }

    /// Runs a change to items in one transaction, telling it whether
    /// history sync notes it.
    fn change<R>(&self, f: impl FnOnce(&Connection, bool) -> Result<R>) -> Result<R> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let result = f(&tx, self.syncs_history.load(Ordering::Acquire))?;
        tx.commit()?;
        Ok(result)
    }

    /// Whether this device syncs its history.
    pub fn history_sync_enabled(&self) -> bool {
        self.syncs_history.load(Ordering::Acquire)
    }

    /// Turns history sync on (noting the history as it is, all to upload)
    /// or off (forgetting what it noted); the history itself stays.
    pub fn set_history_sync(&self, on: bool) -> Result<()> {
        let conn = self.conn();
        if on {
            ledger::enable(&conn)?;
        } else {
            ledger::disable(&conn)?;
        }
        // Under the lock: a change after it is noted, or not, as it says.
        self.syncs_history.store(on, Ordering::Release);
        Ok(())
    }

    /// Gives the pages that deleted items left free back to the disk; how
    /// many it gave back.
    pub fn reclaim_space(&self) -> Result<i64> {
        let conn = self.conn();
        let free: i64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
        if free == 0 {
            return Ok(0);
        }
        // Stepped to its end: a step may free only part of the pages.
        let mut vacuum = conn.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = vacuum.query([])?;
        while rows.next()?.is_some() {}
        drop(rows);
        drop(vacuum);
        checkpoint(&conn)?;
        Ok(free)
    }

    /// The pages deleted items left free, not yet given back.
    #[cfg(test)]
    pub(crate) fn free_pages(&self) -> Result<i64> {
        self.conn()
            .pragma_query_value(None, "freelist_count", |r| r.get(0))
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves the connection usable.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn now() -> String {
        Self::now_timed().0
    }

    /// Now as a `created_at` and in Unix ms, read once: the text alone is
    /// ambiguous in the hour a clock change repeats.
    fn now_timed() -> (String, i64) {
        let now = Local::now();
        (
            now.format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
            now.timestamp_millis(),
        )
    }

    fn today() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    // --- Items ------------------------------------------------------------

    /// Records copied text; text already there moves to now.
    pub fn insert_text(&self, content: &str, hash: &str) -> Result<()> {
        let (created_at, ms) = Self::now_timed();
        self.insert_text_timed(content, hash, &created_at, ms)
    }

    /// [`insert_text`](Self::insert_text) at `created_at`.
    pub fn insert_text_at(&self, content: &str, hash: &str, created_at: &str) -> Result<()> {
        self.insert_text_timed(content, hash, created_at, ledger::ms_of_local(created_at))
    }

    /// Records copied text at `created_at`, which is `ms` in Unix ms.
    fn insert_text_timed(
        &self,
        content: &str,
        hash: &str,
        created_at: &str,
        ms: i64,
    ) -> Result<()> {
        self.change(|c, syncs| {
            c.execute(
                "INSERT INTO clipboard_items (content_type, text_content, created_at, hash)
                 VALUES ('text', ?1, ?2, ?3)
                 ON CONFLICT(hash) DO UPDATE SET created_at = excluded.created_at",
                params![content, created_at, hash],
            )?;
            if syncs {
                ledger::note_copied(c, hash, ms)?;
            }
            Ok(())
        })
    }

    /// Records a copied image (PNG) and its thumbnail (PNG).
    pub fn insert_image(&self, png: &[u8], thumbnail: &[u8], hash: &str) -> Result<()> {
        self.insert_image_at(png, thumbnail, hash, &Self::now())
    }

    /// [`insert_image`](Self::insert_image) at `created_at`.
    pub fn insert_image_at(
        &self,
        png: &[u8],
        thumbnail: &[u8],
        hash: &str,
        created_at: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO clipboard_items (content_type, image_data, image_thumbnail, created_at, hash)
             VALUES ('image', ?1, ?2, ?3, ?4)
             ON CONFLICT(hash) DO UPDATE SET created_at = excluded.created_at",
            params![png, thumbnail, created_at, hash],
        )?;
        Ok(())
    }

    /// Moves the item with `hash` to now, as copying it again does; whether
    /// there is one.
    pub fn touch_item(&self, hash: &str) -> Result<bool> {
        let (now, ms) = Self::now_timed();
        self.change(|c, syncs| {
            let moved = c.execute(
                "UPDATE clipboard_items SET created_at = ?1 WHERE hash = ?2",
                params![now, hash],
            )? > 0;
            if moved && syncs {
                ledger::note_copied(c, hash, ms)?;
            }
            Ok(moved)
        })
    }

    /// The id and the PNG of the image item with `hash`.
    pub fn image_by_hash(&self, hash: &str) -> Result<Option<(i64, Vec<u8>)>> {
        self.conn()
            .query_row(
                "SELECT id, image_data FROM clipboard_items
                 WHERE hash = ?1 AND content_type = 'image' AND image_data IS NOT NULL",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// Replaces the PNG of image item `id` (the same pixels, encoded
    /// smaller).
    pub fn set_image_data(&self, id: i64, png: &[u8]) -> Result<()> {
        self.conn().execute(
            "UPDATE clipboard_items SET image_data = ?1 WHERE id = ?2",
            params![png, id],
        )?;
        Ok(())
    }

    fn row_to_item(row: &Row) -> Result<ClipboardItem> {
        let thumbnail: Option<Vec<u8>> = row.get(3)?;
        Ok(ClipboardItem {
            id: row.get(0)?,
            content_type: ContentType::parse(&row.get::<_, String>(1)?),
            text_preview: row.get(2)?,
            thumbnail: thumbnail.map(|b| format!("data:image/png;base64,{}", BASE64.encode(b))),
            created_at: row.get(4)?,
            is_favorite: row.get::<_, i64>(5)? != 0,
            note: row.get(6)?,
        })
    }

    fn query_items(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<ClipboardItem>> {
        let conn = self.conn();
        // The columns `row_to_item` reads, in its order.
        let sql = format!(
            "SELECT id, content_type, SUBSTR(text_content, 1, {PREVIEW_CHARS}), image_thumbnail,
                    created_at, is_favorite, note
             FROM clipboard_items WHERE {filter} ORDER BY created_at DESC, id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(args, Self::row_to_item)?;
        rows.collect()
    }

    /// Today's items, newest first.
    pub fn today_items(&self) -> Result<Vec<ClipboardItem>> {
        self.query_items(TODAY, params![Self::today()])
    }

    /// Items of earlier days, grouped by day, newest first.
    pub fn archive_groups(&self) -> Result<Vec<DateGroup>> {
        Ok(group_by_date(
            self.query_items(BEFORE_TODAY, params![Self::today()])?,
        ))
    }

    /// The favorites, newest first.
    pub fn favorite_items(&self) -> Result<Vec<ClipboardItem>> {
        self.query_items("is_favorite = 1", [])
    }

    /// The favorites with their full content, for syncing.
    pub fn favorites_with_content(&self) -> Result<Vec<(ClipboardItem, ItemContent)>> {
        self.favorite_items()?
            .into_iter()
            .filter_map(|item| match self.item_content(item.id) {
                Ok(Some(content)) => Some(Ok((item, content))),
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            })
            .collect()
    }

    /// Favorites an item or takes it out of the favorites, with `note`;
    /// whether it is a favorite now.
    pub fn toggle_favorite(&self, id: i64, note: &str) -> Result<bool> {
        self.change(|c, syncs| {
            c.execute(
                "UPDATE clipboard_items
                 SET is_favorite = CASE WHEN is_favorite = 1 THEN 0 ELSE 1 END, note = ?2
                 WHERE id = ?1",
                params![id, note],
            )?;
            let (favorite, hash): (i64, String) = c.query_row(
                "SELECT is_favorite, hash FROM clipboard_items WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if syncs {
                ledger::note_marked(c, &hash, now_ms())?;
            }
            Ok(favorite != 0)
        })
    }

    pub fn is_favorite(&self, id: i64) -> Result<bool> {
        let favorite: Option<i64> = self
            .conn()
            .query_row(
                "SELECT is_favorite FROM clipboard_items WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(favorite.unwrap_or(0) != 0)
    }

    pub fn update_note(&self, id: i64, note: &str) -> Result<()> {
        self.change(|c, syncs| {
            c.execute(
                "UPDATE clipboard_items SET note = ?1 WHERE id = ?2",
                params![note, id],
            )?;
            if syncs && let Some(hash) = hash_of(c, id)? {
                ledger::note_marked(c, &hash, now_ms())?;
            }
            Ok(())
        })
    }

    pub fn delete_item(&self, id: i64) -> Result<()> {
        self.change(|c, syncs| {
            if syncs && let Some(hash) = hash_of(c, id)? {
                ledger::note_deleted(c, &hash, now_ms())?;
            }
            c.execute("DELETE FROM clipboard_items WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// The full text or image of an item; `None` when it is gone.
    pub fn item_content(&self, id: i64) -> Result<Option<ItemContent>> {
        let row: Option<(String, Option<String>, Option<Vec<u8>>)> = self
            .conn()
            .query_row(
                "SELECT content_type, text_content, image_data FROM clipboard_items WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(
            row.and_then(|(kind, text, image)| match ContentType::parse(&kind) {
                ContentType::Text => text.map(ItemContent::Text),
                ContentType::Image => image.map(ItemContent::Image),
            }),
        )
    }

    /// The hash of item `id`; `None` when it is gone.
    pub fn item_hash(&self, id: i64) -> Result<Option<String>> {
        hash_of(&self.conn(), id)
    }

    /// The ids of every image item.
    pub fn image_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id FROM clipboard_items WHERE content_type = 'image'")?;
        let ids = stmt.query_map([], |r| r.get(0))?;
        ids.collect()
    }

    /// The stored thumbnail (PNG) of item `id`.
    pub fn thumbnail(&self, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT image_thumbnail FROM clipboard_items WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_thumbnail(&self, id: i64, png: &[u8]) -> Result<()> {
        self.conn().execute(
            "UPDATE clipboard_items SET image_thumbnail = ?1 WHERE id = ?2",
            params![png, id],
        )?;
        Ok(())
    }

    /// The way the stored thumbnails were made (1 before it was recorded).
    pub fn thumbnail_version(&self) -> Result<i64> {
        Ok(self
            .setting(key::THUMBNAIL_VERSION)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1))
    }

    pub fn set_thumbnail_version(&self, version: i64) -> Result<()> {
        self.set_setting(key::THUMBNAIL_VERSION, &version.to_string())
    }

    /// Deletes every item that is not a favorite; how many went.
    pub fn clear_history(&self) -> Result<usize> {
        self.change(|c, syncs| {
            if syncs {
                ledger::note_cleared(c, now_ms())?;
            }
            c.execute("DELETE FROM clipboard_items WHERE is_favorite = 0", [])
        })
    }

    /// Deletes non-favorites older than `retention_days` (0 keeps all).
    pub fn cleanup_expired(&self, retention_days: i64) -> Result<usize> {
        if retention_days <= 0 {
            return Ok(0);
        }
        let cutoff = (Local::now() - chrono::Duration::days(retention_days))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        self.change(|c, _| {
            // Expiry is this device's: the others keep theirs.
            ledger::forget_expired(c, &cutoff)?;
            c.execute(
                "DELETE FROM clipboard_items WHERE is_favorite = 0 AND created_at < ?1",
                params![cutoff],
            )
        })
    }

    // --- Settings ---------------------------------------------------------

    fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
    }

    fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings> {
        let defaults = Settings::default();
        let text = |key: &str, default: String| -> Result<String> {
            Ok(self.setting(key)?.unwrap_or(default))
        };
        Ok(Settings {
            hotkey: text(key::HOTKEY, defaults.hotkey)?,
            retention_days: self
                .setting(key::RETENTION_DAYS)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.retention_days),
            telegram_token: text(key::TELEGRAM_TOKEN, String::new())?,
            telegram_chat_id: text(key::TELEGRAM_CHAT_ID, String::new())?,
            theme: self
                .setting(key::THEME)?
                .map(|v| ThemePreference::parse(&v))
                .unwrap_or_default(),
            hide_on_blur: self
                .setting(key::HIDE_ON_BLUR)?
                .map(|v| v != "false")
                .unwrap_or(defaults.hide_on_blur),
            hide_from_capture: self
                .setting(key::HIDE_FROM_CAPTURE)?
                .map(|v| v != "false")
                .unwrap_or(defaults.hide_from_capture),
            vault_lock_minutes: self
                .setting(key::VAULT_LOCK_MINUTES)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.vault_lock_minutes),
            vault_lock_on_hide: self
                .setting(key::VAULT_LOCK_ON_HIDE)?
                .map(|v| v != "false")
                .unwrap_or(defaults.vault_lock_on_hide),
            vault_touch_id: self
                .setting(key::VAULT_TOUCH_ID)?
                .map(|v| v == "true")
                .unwrap_or(defaults.vault_touch_id),
        })
    }

    pub fn set_hotkey(&self, hotkey: &str) -> Result<()> {
        self.set_setting(key::HOTKEY, hotkey)
    }

    pub fn set_retention_days(&self, days: i64) -> Result<()> {
        self.set_setting(key::RETENTION_DAYS, &days.to_string())
    }

    pub fn set_telegram_token(&self, token: &str) -> Result<()> {
        self.set_setting(key::TELEGRAM_TOKEN, token)
    }

    pub fn set_telegram_chat_id(&self, chat_id: &str) -> Result<()> {
        self.set_setting(key::TELEGRAM_CHAT_ID, chat_id)
    }

    pub fn set_theme(&self, theme: ThemePreference) -> Result<()> {
        self.set_setting(key::THEME, theme.as_str())
    }

    pub fn set_hide_on_blur(&self, hide: bool) -> Result<()> {
        self.set_setting(key::HIDE_ON_BLUR, &hide.to_string())
    }

    pub fn set_hide_from_capture(&self, hide: bool) -> Result<()> {
        self.set_setting(key::HIDE_FROM_CAPTURE, &hide.to_string())
    }

    /// Runs `f` with the connection (the vault's store).
    pub(crate) fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        f(&self.conn())
    }

    pub fn set_vault_lock_minutes(&self, minutes: i64) -> Result<()> {
        self.set_setting(key::VAULT_LOCK_MINUTES, &minutes.to_string())
    }

    pub fn set_vault_lock_on_hide(&self, on: bool) -> Result<()> {
        self.set_setting(key::VAULT_LOCK_ON_HIDE, &on.to_string())
    }

    pub fn set_vault_touch_id(&self, on: bool) -> Result<()> {
        self.set_setting(key::VAULT_TOUCH_ID, &on.to_string())
    }
}

/// Makes a database that earlier versions left without auto-vacuum
/// incremental, which takes a full `VACUUM` (a fraction of a second for a
/// history with a gigabyte of deleted images): it also hands back all the
/// space it held free, once. A failure (a full disk) leaves it as it was,
/// still working: it only keeps its size, and is tried again next start.
fn make_incremental(conn: &Connection) {
    let mode = conn.pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0));
    match mode {
        Ok(AUTO_VACUUM_INCREMENTAL) => return,
        Ok(_) => {}
        Err(e) => {
            log::warn!("cannot read the database's vacuum mode: {e}");
            return;
        }
    }
    let started = std::time::Instant::now();
    match conn.execute_batch("VACUUM").and_then(|()| checkpoint(conn)) {
        Ok(()) => log::info!("compacted the database in {:?}", started.elapsed()),
        Err(e) => log::warn!("cannot compact the database: {e}"),
    }
}

/// Moves what the WAL holds into the database file, then truncates the
/// WAL: the file shrinks now, not at some later checkpoint. Readers in the
/// middle of a read can keep it from finishing, which is no error.
fn checkpoint(conn: &Connection) -> Result<()> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
}

#[cfg(test)]
mod tests;
