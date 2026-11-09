//! The vault's tables. Only ciphertext and non-secret metadata pass here.

use rusqlite::{Connection, OptionalExtension, Result, params};

use super::crypto::KdfParams;

pub const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS vault_meta (
        id           INTEGER PRIMARY KEY CHECK (id = 1),
        version      INTEGER NOT NULL,
        kdf_params   TEXT NOT NULL,
        salt         BLOB NOT NULL,
        wrapped_dek  BLOB NOT NULL,
        key_check    BLOB NOT NULL DEFAULT x'',
        failed_tries INTEGER NOT NULL DEFAULT 0,
        locked_until INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS vault_entries (
        id         TEXT PRIMARY KEY,
        nonce      BLOB NOT NULL,
        ciphertext BLOB NOT NULL,
        updated_at TEXT NOT NULL
    );
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaRow {
    pub version: i64,
    pub params: KdfParams,
    pub salt: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    /// A known value sealed under the data key (empty in vaults made before
    /// it existed).
    pub key_check: Vec<u8>,
    pub failed_tries: u32,
    /// Unix ms before which no unlock is tried.
    pub locked_until_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRow {
    pub id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub updated_at: String,
}

fn bad_params(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
}

pub fn load_meta(conn: &Connection) -> Result<Option<MetaRow>> {
    conn.query_row(
        "SELECT version, kdf_params, salt, wrapped_dek, failed_tries, locked_until, key_check FROM vault_meta WHERE id = 1",
        [],
        |r| {
            let params: String = r.get(1)?;
            Ok(MetaRow {
                version: r.get(0)?,
                params: serde_json::from_str(&params).map_err(bad_params)?,
                salt: r.get(2)?,
                wrapped_dek: r.get(3)?,
                failed_tries: r.get(4)?,
                locked_until_ms: r.get::<_, i64>(5)?.max(0) as u64,
                key_check: r.get(6)?,
            })
        },
    )
    .optional()
}

pub fn save_meta(conn: &Connection, meta: &MetaRow) -> Result<()> {
    let params = serde_json::to_string(&meta.params).map_err(bad_params)?;
    conn.execute(
        "INSERT OR REPLACE INTO vault_meta (id, version, kdf_params, salt, wrapped_dek, failed_tries, locked_until, key_check)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            meta.version,
            params,
            meta.salt,
            meta.wrapped_dek,
            meta.failed_tries,
            meta.locked_until_ms as i64,
            meta.key_check
        ],
    )?;
    Ok(())
}

/// Adds what later versions added to an existing vault's tables.
pub fn migrate(conn: &Connection) -> Result<()> {
    if conn
        .prepare("SELECT key_check FROM vault_meta LIMIT 0")
        .is_err()
    {
        conn.execute_batch(
            "ALTER TABLE vault_meta ADD COLUMN key_check BLOB NOT NULL DEFAULT x''",
        )?;
    }
    Ok(())
}

/// Writes the metadata and entries together (a new key and the entries
/// encrypted with it), atomically.
pub fn save_meta_and_entries(
    conn: &Connection,
    meta: &MetaRow,
    entries: &[EntryRow],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    save_meta(&tx, meta)?;
    for entry in entries {
        upsert_entry(&tx, entry)?;
    }
    tx.commit()
}

pub fn set_failures(conn: &Connection, tries: u32, locked_until_ms: u64) -> Result<()> {
    conn.execute(
        "UPDATE vault_meta SET failed_tries = ?1, locked_until = ?2 WHERE id = 1",
        params![tries, locked_until_ms as i64],
    )?;
    Ok(())
}

fn row(r: &rusqlite::Row) -> Result<EntryRow> {
    Ok(EntryRow {
        id: r.get(0)?,
        nonce: r.get(1)?,
        ciphertext: r.get(2)?,
        updated_at: r.get(3)?,
    })
}

pub fn load_entries(conn: &Connection) -> Result<Vec<EntryRow>> {
    let mut stmt = conn.prepare("SELECT id, nonce, ciphertext, updated_at FROM vault_entries")?;
    let rows = stmt.query_map([], row)?;
    rows.collect()
}

pub fn load_entry(conn: &Connection, id: &str) -> Result<Option<EntryRow>> {
    conn.query_row(
        "SELECT id, nonce, ciphertext, updated_at FROM vault_entries WHERE id = ?1",
        params![id],
        row,
    )
    .optional()
}

pub fn upsert_entry(conn: &Connection, entry: &EntryRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO vault_entries (id, nonce, ciphertext, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![entry.id, entry.nonce, entry.ciphertext, entry.updated_at],
    )?;
    Ok(())
}

pub fn delete_entry(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM vault_entries WHERE id = ?1", params![id])?;
    Ok(())
}

/// Replaces every entry, atomically.
pub fn replace_entries(conn: &Connection, entries: &[EntryRow]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM vault_entries", [])?;
    for entry in entries {
        upsert_entry(&tx, entry)?;
    }
    tx.commit()
}

/// Deletes the vault: its key and every entry.
pub fn wipe(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM vault_entries; DELETE FROM vault_meta;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn meta() -> MetaRow {
        MetaRow {
            version: 1,
            params: crate::vault::crypto::KdfParams::INSECURE_TEST,
            salt: vec![1; 16],
            wrapped_dek: vec![2; 72],
            key_check: vec![3; 64],
            failed_tries: 0,
            locked_until_ms: 0,
        }
    }

    #[test]
    fn migrate_adds_key_check_to_an_old_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE vault_meta (id INTEGER PRIMARY KEY, version INTEGER NOT NULL, kdf_params TEXT NOT NULL,
             salt BLOB NOT NULL, wrapped_dek BLOB NOT NULL, failed_tries INTEGER NOT NULL DEFAULT 0,
             locked_until INTEGER NOT NULL DEFAULT 0);
             INSERT INTO vault_meta VALUES (1, 1, '{\"m\":8,\"t\":1,\"p\":1}', x'00', x'00', 0, 0);",
        )
        .unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert!(load_meta(&conn).unwrap().unwrap().key_check.is_empty());
    }

    #[test]
    fn meta_round_trips_and_counts_failures() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            assert_eq!(load_meta(c).unwrap(), None);
            save_meta(c, &meta()).unwrap();
            assert_eq!(load_meta(c).unwrap(), Some(meta()));
            set_failures(c, 4, 1234).unwrap();
            let m = load_meta(c).unwrap().unwrap();
            assert_eq!((m.failed_tries, m.locked_until_ms), (4, 1234));
        });
    }

    #[test]
    fn entries_upsert_delete_replace() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            let row = |id: &str, t: &str| EntryRow {
                id: id.into(),
                nonce: vec![0; 24],
                ciphertext: vec![9; 20],
                updated_at: t.into(),
            };
            upsert_entry(c, &row("a", "1")).unwrap();
            upsert_entry(c, &row("b", "1")).unwrap();
            upsert_entry(c, &row("a", "2")).unwrap();
            assert_eq!(load_entries(c).unwrap().len(), 2);
            assert_eq!(load_entry(c, "a").unwrap().unwrap().updated_at, "2");
            delete_entry(c, "a").unwrap();
            assert!(load_entry(c, "a").unwrap().is_none());
            replace_entries(c, &[row("c", "3")]).unwrap();
            assert_eq!(
                load_entries(c)
                    .unwrap()
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["c"]
            );
            wipe(c).unwrap();
            assert!(load_meta(c).unwrap().is_none() && load_entries(c).unwrap().is_empty());
        });
    }
}
