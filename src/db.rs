//! The SQLite store of the history and the settings. The schema is the
//! Tauri version's, so an existing `clipboardx.db` keeps working.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Local;
use rusqlite::{Connection, OptionalExtension, Result, Row, params};

use crate::model::{
    ClipboardItem, ContentType, DEFAULT_HOTKEY, DateGroup, Settings, ThemePreference, group_by_date,
};

/// Characters of text that lists load.
const PREVIEW_CHARS: i64 = 200;

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
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(crate::vault::store::SCHEMA)?;
        crate::vault::store::migrate(&conn)?;
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
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves the connection usable.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn now() -> String {
        Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
    }

    fn today() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    // --- Items ------------------------------------------------------------

    /// Records copied text; text already there moves to now.
    pub fn insert_text(&self, content: &str, hash: &str) -> Result<()> {
        self.insert_text_at(content, hash, &Self::now())
    }

    /// [`insert_text`](Self::insert_text) at `created_at`.
    pub fn insert_text_at(&self, content: &str, hash: &str, created_at: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO clipboard_items (content_type, text_content, created_at, hash)
             VALUES ('text', ?1, ?2, ?3)
             ON CONFLICT(hash) DO UPDATE SET created_at = excluded.created_at",
            params![content, created_at, hash],
        )?;
        Ok(())
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
        self.query_items("date(created_at) = ?1", params![Self::today()])
    }

    /// Items of earlier days, grouped by day, newest first.
    pub fn archive_groups(&self) -> Result<Vec<DateGroup>> {
        Ok(group_by_date(self.query_items(
            "date(created_at) < ?1",
            params![Self::today()],
        )?))
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
        let conn = self.conn();
        conn.execute(
            "UPDATE clipboard_items
             SET is_favorite = CASE WHEN is_favorite = 1 THEN 0 ELSE 1 END, note = ?2
             WHERE id = ?1",
            params![id, note],
        )?;
        let favorite: i64 = conn.query_row(
            "SELECT is_favorite FROM clipboard_items WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(favorite != 0)
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
        self.conn().execute(
            "UPDATE clipboard_items SET note = ?1 WHERE id = ?2",
            params![note, id],
        )?;
        Ok(())
    }

    pub fn delete_item(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM clipboard_items WHERE id = ?1", params![id])?;
        Ok(())
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
        self.conn()
            .execute("DELETE FROM clipboard_items WHERE is_favorite = 0", [])
    }

    /// Deletes non-favorites older than `retention_days` (0 keeps all).
    pub fn cleanup_expired(&self, retention_days: i64) -> Result<usize> {
        if retention_days <= 0 {
            return Ok(0);
        }
        let cutoff = (Local::now() - chrono::Duration::days(retention_days))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        self.conn().execute(
            "DELETE FROM clipboard_items WHERE is_favorite = 0 AND created_at < ?1",
            params![cutoff],
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::in_memory().expect("in-memory db")
    }

    #[test]
    fn new_database_has_default_settings() {
        let settings = db().settings().unwrap();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn opens_a_file_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboardx.db");
        Database::open(&path)
            .unwrap()
            .insert_text("kept", "h")
            .unwrap();
        let again = Database::open(&path).unwrap();
        assert_eq!(again.today_items().unwrap().len(), 1);
    }

    #[test]
    fn same_text_moves_to_the_top_instead_of_duplicating() {
        let db = db();
        db.insert_text_at("first", "h1", "2020-01-01T10:00:00.000")
            .unwrap();
        db.insert_text_at("second", "h2", "2020-01-01T10:00:01.000")
            .unwrap();
        db.insert_text_at("first", "h1", "2020-01-01T10:00:02.000")
            .unwrap();
        let items = db.archive_groups().unwrap().remove(0).items;
        let texts: Vec<_> = items
            .iter()
            .map(|i| i.text_preview.clone().unwrap())
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn today_and_archive_split_by_day() {
        let db = db();
        db.insert_text("today", "h1").unwrap();
        db.insert_text_at("old", "h2", "2020-01-02T10:00:00")
            .unwrap();
        db.insert_text_at("older", "h3", "2020-01-01T10:00:00")
            .unwrap();
        assert_eq!(db.today_items().unwrap().len(), 1);
        let groups = db.archive_groups().unwrap();
        assert_eq!(
            groups.iter().map(|g| g.date.as_str()).collect::<Vec<_>>(),
            vec!["2020-01-02", "2020-01-01"]
        );
    }

    #[test]
    fn previews_are_truncated_but_content_is_whole() {
        let db = db();
        let long = "x".repeat(1000);
        db.insert_text(&long, "h").unwrap();
        let item = &db.today_items().unwrap()[0];
        assert_eq!(
            item.text_preview.as_ref().unwrap().len(),
            PREVIEW_CHARS as usize
        );
        assert_eq!(
            db.item_content(item.id).unwrap(),
            Some(ItemContent::Text(long))
        );
    }

    #[test]
    fn images_carry_a_thumbnail_data_uri_and_full_bytes() {
        let db = db();
        db.insert_image(&[1, 2, 3], &[4, 5], "img").unwrap();
        let item = &db.today_items().unwrap()[0];
        assert_eq!(item.content_type, ContentType::Image);
        assert!(
            item.thumbnail
                .as_ref()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(
            db.item_content(item.id).unwrap(),
            Some(ItemContent::Image(vec![1, 2, 3]))
        );
    }

    #[test]
    fn favorites_toggle_with_notes() {
        let db = db();
        db.insert_text("a", "h").unwrap();
        let id = db.today_items().unwrap()[0].id;
        assert!(db.toggle_favorite(id, "note").unwrap());
        assert!(db.is_favorite(id).unwrap());
        let favorites = db.favorite_items().unwrap();
        assert_eq!(favorites[0].note.as_deref(), Some("note"));
        assert_eq!(db.favorites_with_content().unwrap().len(), 1);
        db.update_note(id, "changed").unwrap();
        assert_eq!(
            db.favorite_items().unwrap()[0].note.as_deref(),
            Some("changed")
        );
        assert!(!db.toggle_favorite(id, "").unwrap());
        assert!(db.favorite_items().unwrap().is_empty());
    }

    #[test]
    fn delete_and_missing_items() {
        let db = db();
        db.insert_text("a", "h").unwrap();
        let id = db.today_items().unwrap()[0].id;
        db.delete_item(id).unwrap();
        assert_eq!(db.item_content(id).unwrap(), None);
        assert!(!db.is_favorite(id).unwrap());
    }

    #[test]
    fn clearing_and_expiry_keep_favorites() {
        let db = db();
        db.insert_text_at("old", "h1", "2020-01-01T10:00:00")
            .unwrap();
        db.insert_text_at("old favorite", "h2", "2020-01-01T10:00:00")
            .unwrap();
        db.insert_text("new", "h3").unwrap();
        let favorite = db
            .archive_groups()
            .unwrap()
            .remove(0)
            .items
            .into_iter()
            .find(|i| i.text_preview.as_deref() == Some("old favorite"))
            .unwrap();
        db.toggle_favorite(favorite.id, "").unwrap();

        assert_eq!(db.cleanup_expired(0).unwrap(), 0);
        assert_eq!(db.cleanup_expired(7).unwrap(), 1);
        assert_eq!(db.today_items().unwrap().len(), 1);
        assert_eq!(db.clear_history().unwrap(), 1);
        assert_eq!(db.favorite_items().unwrap().len(), 1);
    }

    #[test]
    fn vault_settings_have_defaults_and_save() {
        let db = db();
        let s = db.settings().unwrap();
        assert_eq!(
            (s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id),
            (5, true, false)
        );
        db.set_vault_lock_minutes(15).unwrap();
        db.set_vault_lock_on_hide(false).unwrap();
        db.set_vault_touch_id(true).unwrap();
        let s = db.settings().unwrap();
        assert_eq!(
            (s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id),
            (15, false, true)
        );
    }

    #[test]
    fn hiding_from_capture_is_on_until_turned_off() {
        let db = db();
        assert!(db.settings().unwrap().hide_from_capture);
        db.set_hide_from_capture(false).unwrap();
        assert!(!db.settings().unwrap().hide_from_capture);
        db.set_hide_from_capture(true).unwrap();
        assert!(db.settings().unwrap().hide_from_capture);
    }

    #[test]
    fn settings_are_saved() {
        let db = db();
        db.set_hotkey("Alt+Space").unwrap();
        db.set_retention_days(30).unwrap();
        db.set_telegram_token("token").unwrap();
        db.set_telegram_chat_id("42").unwrap();
        db.set_theme(ThemePreference::Dark).unwrap();
        db.set_hide_on_blur(false).unwrap();
        let settings = db.settings().unwrap();
        assert_eq!(settings.hotkey, "Alt+Space");
        assert_eq!(settings.retention_days, 30);
        assert_eq!(settings.telegram_token, "token");
        assert_eq!(settings.telegram_chat_id, "42");
        assert_eq!(settings.theme, ThemePreference::Dark);
        assert!(!settings.hide_on_blur);
        assert!(settings.telegram_configured());
    }
}
