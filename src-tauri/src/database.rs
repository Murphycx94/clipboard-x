use rusqlite::{Connection, Result, params};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClipboardItem {
    pub id: i64,
    pub content_type: String,
    pub text_content: Option<String>,
    pub image_thumbnail: Option<String>, // base64
    pub created_at: String,
    pub is_favorite: bool,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DateGroup {
    pub date: String,
    pub items: Vec<ClipboardItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettings {
    pub hotkey: String,
    pub retention_days: i64, // 0 = 永久保留
}

pub struct Database(pub Mutex<Connection>);

impl Database {
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS clipboard_items (
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
            INSERT OR IGNORE INTO settings(key, value) VALUES ('hotkey', 'CmdOrCtrl+Shift+V');
            INSERT OR IGNORE INTO settings(key, value) VALUES ('retention_days', '0');",
        )?;
        Ok(Database(Mutex::new(conn)))
    }

    pub fn insert_text(&self, content: &str, hash: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let now = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let rows = conn.execute(
            "INSERT OR IGNORE INTO clipboard_items (content_type, text_content, created_at, hash)
             VALUES ('text', ?1, ?2, ?3)",
            params![content, now, hash],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_image(&self, image_data: &[u8], thumbnail: &[u8], hash: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let now = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let rows = conn.execute(
            "INSERT OR IGNORE INTO clipboard_items
             (content_type, image_data, image_thumbnail, created_at, hash)
             VALUES ('image', ?1, ?2, ?3, ?4)",
            params![image_data, thumbnail, now, hash],
        )?;
        Ok(rows > 0)
    }

    fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<ClipboardItem> {
        let thumb_bytes: Option<Vec<u8>> = row.get(3)?;
        Ok(ClipboardItem {
            id: row.get(0)?,
            content_type: row.get(1)?,
            text_content: row.get(2)?,
            image_thumbnail: thumb_bytes.map(|b| BASE64.encode(b)),
            created_at: row.get(4)?,
            is_favorite: row.get::<_, i64>(5)? != 0,
            note: row.get(6)?,
        })
    }

    pub fn get_today_items(&self) -> Result<Vec<ClipboardItem>> {
        let conn = self.0.lock().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let mut stmt = conn.prepare(
            "SELECT id, content_type, text_content, image_thumbnail, created_at, is_favorite, note
             FROM clipboard_items
             WHERE date(created_at) = ?1
             ORDER BY created_at DESC",
        )?;
        let result = stmt.query_map(params![today], Self::row_to_item)?
            .collect::<Result<Vec<_>>>();
        result
    }

    pub fn get_archive_items(&self) -> Result<Vec<DateGroup>> {
        let conn = self.0.lock().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let mut stmt = conn.prepare(
            "SELECT id, content_type, text_content, image_thumbnail, created_at, is_favorite, note
             FROM clipboard_items
             WHERE date(created_at) < ?1
             ORDER BY created_at DESC",
        )?;
        let items = stmt.query_map(params![today], Self::row_to_item)?
            .collect::<Result<Vec<_>>>()?;

        let mut groups: Vec<DateGroup> = Vec::new();
        for item in items {
            let date = item.created_at[..10].to_string();
            if let Some(g) = groups.iter_mut().find(|g| g.date == date) {
                g.items.push(item);
            } else {
                groups.push(DateGroup { date, items: vec![item] });
            }
        }
        Ok(groups)
    }

    pub fn get_favorite_items(&self) -> Result<Vec<ClipboardItem>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content_type, text_content, image_thumbnail, created_at, is_favorite, note
             FROM clipboard_items
             WHERE is_favorite = 1
             ORDER BY created_at DESC",
        )?;
        let result = stmt.query_map([], Self::row_to_item)?
            .collect::<Result<Vec<_>>>();
        result
    }

    pub fn toggle_favorite(&self, id: i64, note: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE clipboard_items
             SET is_favorite = CASE WHEN is_favorite=1 THEN 0 ELSE 1 END, note=?2
             WHERE id=?1",
            params![id, note],
        )?;
        Ok(())
    }

    pub fn get_image_data(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT image_data FROM clipboard_items WHERE id=?1")?;
        Ok(stmt.query_row(params![id], |row| row.get(0)).ok())
    }

    pub fn get_image_base64(&self, id: i64) -> Result<Option<String>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT image_data FROM clipboard_items WHERE id=?1")?;
        Ok(stmt.query_row(params![id], |row| {
            let bytes: Option<Vec<u8>> = row.get(0)?;
            Ok(bytes.map(|b| BASE64.encode(b)))
        }).ok().flatten())
    }

    pub fn get_text_content(&self, id: i64) -> Result<Option<String>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT text_content FROM clipboard_items WHERE id=?1")?;
        Ok(stmt.query_row(params![id], |row| row.get(0)).ok())
    }

    pub fn get_content_type(&self, id: i64) -> Result<String> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT content_type FROM clipboard_items WHERE id=?1")?;
        stmt.query_row(params![id], |row| row.get(0))
    }

    pub fn delete_item(&self, id: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM clipboard_items WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn get_settings(&self) -> Result<AppSettings> {
        let conn = self.0.lock().unwrap();
        let hotkey: String = conn.query_row(
            "SELECT value FROM settings WHERE key='hotkey'",
            [],
            |row| row.get(0),
        )?;
        let retention_days: i64 = conn
            .query_row(
                "SELECT value FROM settings WHERE key='retention_days'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v.parse().unwrap_or(0))
            .unwrap_or(0);
        Ok(AppSettings { hotkey, retention_days })
    }

    pub fn update_retention(&self, days: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES ('retention_days', ?1)",
            params![days.to_string()],
        )?;
        Ok(())
    }

    pub fn clear_history(&self) -> Result<usize> {
        let conn = self.0.lock().unwrap();
        let count = conn.execute(
            "DELETE FROM clipboard_items WHERE is_favorite = 0",
            [],
        )?;
        Ok(count)
    }

    pub fn cleanup_expired(&self, retention_days: i64) -> Result<()> {
        if retention_days == 0 { return Ok(()); }
        let conn = self.0.lock().unwrap();
        conn.execute(
            "DELETE FROM clipboard_items WHERE is_favorite = 0
             AND created_at < datetime('now', ?1)",
            params![format!("-{} days", retention_days)],
        )?;
        Ok(())
    }

    pub fn update_hotkey(&self, hotkey: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES ('hotkey', ?1)",
            params![hotkey],
        )?;
        Ok(())
    }

    pub fn update_note(&self, id: i64, note: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE clipboard_items SET note = ?1 WHERE id = ?2",
            params![note, id],
        )?;
        Ok(())
    }
}
