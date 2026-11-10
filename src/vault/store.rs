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
        recovery_wrap   BLOB NOT NULL DEFAULT x'',
        recovery_sealed BLOB NOT NULL DEFAULT x'',
        failed_tries INTEGER NOT NULL DEFAULT 0,
        locked_until INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS vault_entries (
        id              TEXT PRIMARY KEY,
        nonce           BLOB NOT NULL,
        ciphertext      BLOB NOT NULL,
        updated_at      TEXT NOT NULL,
        revision        INTEGER NOT NULL DEFAULT 0,
        deleted         INTEGER NOT NULL DEFAULT 0,
        dirty           INTEGER NOT NULL DEFAULT 0,
        synced_revision INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS vault_sync (
        id               INTEGER PRIMARY KEY CHECK (id = 1),
        server_url       TEXT NOT NULL,
        device_id        TEXT NOT NULL,
        last_seq         INTEGER NOT NULL DEFAULT 0,
        manifest_version INTEGER NOT NULL DEFAULT 0,
        header_version   INTEGER NOT NULL DEFAULT 1,
        header_auth      BLOB,
        staged_header    TEXT,
        joined_seal      BLOB
    );
";

/// Columns later versions added: (table, column, definition).
const ADDED_COLUMNS: [(&str, &str, &str); 10] = [
    ("vault_meta", "key_check", "BLOB NOT NULL DEFAULT x''"),
    ("vault_meta", "recovery_wrap", "BLOB NOT NULL DEFAULT x''"),
    ("vault_meta", "recovery_sealed", "BLOB NOT NULL DEFAULT x''"),
    ("vault_entries", "revision", "INTEGER NOT NULL DEFAULT 0"),
    ("vault_entries", "deleted", "INTEGER NOT NULL DEFAULT 0"),
    ("vault_entries", "dirty", "INTEGER NOT NULL DEFAULT 0"),
    (
        "vault_entries",
        "synced_revision",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("vault_sync", "header_auth", "BLOB"),
    ("vault_sync", "staged_header", "TEXT"),
    ("vault_sync", "joined_seal", "BLOB"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaRow {
    /// 1: the master password alone; 2: with the secret key.
    pub version: i64,
    pub params: KdfParams,
    pub salt: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    /// A known value sealed under the data key (empty in vaults made before
    /// it existed).
    pub key_check: Vec<u8>,
    /// The data key (and secret key) wrapped by the recovery phrase; empty
    /// without one.
    pub recovery_wrap: Vec<u8>,
    /// The recovery phrase sealed under the data key; empty without one.
    pub recovery_sealed: Vec<u8>,
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
    /// 0 for entries from before sync (authenticated by their id alone).
    pub revision: u64,
    /// A deletion marker: what it seals is no entry.
    pub deleted: bool,
    /// Changed here and not yet on the sync server.
    pub dirty: bool,
    /// The revision the sync server had last time they agreed.
    pub synced_revision: u64,
}

/// Sync's state while it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRow {
    pub server_url: String,
    pub device_id: String,
    pub last_seq: u64,
    pub manifest_version: u64,
    pub header_version: u64,
    /// What authenticates this device's header to the others (after a
    /// password change here).
    pub header_auth: Option<Vec<u8>>,
    /// A newer header from the server, as JSON, kept apart until it proves
    /// itself by unlocking.
    pub staged_header: Option<String>,
    /// The seal of the header this device joined with, until unlocking
    /// checks it.
    pub joined_seal: Option<Vec<u8>>,
}

fn bad_params(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
}

fn u64_of(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn i64_of(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub fn load_meta(conn: &Connection) -> Result<Option<MetaRow>> {
    conn.query_row(
        "SELECT version, kdf_params, salt, wrapped_dek, failed_tries, locked_until, key_check,
                recovery_wrap, recovery_sealed FROM vault_meta WHERE id = 1",
        [],
        |r| {
            let params: String = r.get(1)?;
            Ok(MetaRow {
                version: r.get(0)?,
                params: serde_json::from_str(&params).map_err(bad_params)?,
                salt: r.get(2)?,
                wrapped_dek: r.get(3)?,
                failed_tries: r.get(4)?,
                locked_until_ms: u64_of(r.get(5)?),
                key_check: r.get(6)?,
                recovery_wrap: r.get(7)?,
                recovery_sealed: r.get(8)?,
            })
        },
    )
    .optional()
}

pub fn save_meta(conn: &Connection, meta: &MetaRow) -> Result<()> {
    let params = serde_json::to_string(&meta.params).map_err(bad_params)?;
    conn.execute(
        "INSERT OR REPLACE INTO vault_meta (id, version, kdf_params, salt, wrapped_dek, failed_tries, locked_until,
         key_check, recovery_wrap, recovery_sealed)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            meta.version,
            params,
            meta.salt,
            meta.wrapped_dek,
            meta.failed_tries,
            i64_of(meta.locked_until_ms),
            meta.key_check,
            meta.recovery_wrap,
            meta.recovery_sealed
        ],
    )?;
    Ok(())
}

/// Adds what later versions added to an existing vault's tables.
pub fn migrate(conn: &Connection) -> Result<()> {
    for (table, column, definition) in ADDED_COLUMNS {
        // A table the schema has not made yet gets every column with it.
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |r| r.get(0),
        )?;
        if exists
            && conn
                .prepare(&format!("SELECT {column} FROM {table} LIMIT 0"))
                .is_err()
        {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    Ok(())
}

pub fn set_failures(conn: &Connection, tries: u32, locked_until_ms: u64) -> Result<()> {
    conn.execute(
        "UPDATE vault_meta SET failed_tries = ?1, locked_until = ?2 WHERE id = 1",
        params![tries, i64_of(locked_until_ms)],
    )?;
    Ok(())
}

const ENTRY_COLUMNS: &str =
    "id, nonce, ciphertext, updated_at, revision, deleted, dirty, synced_revision";

fn row(r: &rusqlite::Row) -> Result<EntryRow> {
    Ok(EntryRow {
        id: r.get(0)?,
        nonce: r.get(1)?,
        ciphertext: r.get(2)?,
        updated_at: r.get(3)?,
        revision: u64_of(r.get(4)?),
        deleted: r.get::<_, i64>(5)? != 0,
        dirty: r.get::<_, i64>(6)? != 0,
        synced_revision: u64_of(r.get(7)?),
    })
}

/// Every row, deletion markers included.
pub fn load_entries(conn: &Connection) -> Result<Vec<EntryRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {ENTRY_COLUMNS} FROM vault_entries"))?;
    let rows = stmt.query_map([], row)?;
    rows.collect()
}

pub fn load_entry(conn: &Connection, id: &str) -> Result<Option<EntryRow>> {
    conn.query_row(
        &format!("SELECT {ENTRY_COLUMNS} FROM vault_entries WHERE id = ?1"),
        params![id],
        row,
    )
    .optional()
}

pub fn upsert_entry(conn: &Connection, entry: &EntryRow) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO vault_entries ({ENTRY_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        params![
            entry.id,
            entry.nonce,
            entry.ciphertext,
            entry.updated_at,
            i64_of(entry.revision),
            entry.deleted,
            entry.dirty,
            i64_of(entry.synced_revision)
        ],
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

/// Records that the server has `id` at `revision`, unless it changed here
/// since.
pub fn mark_synced(conn: &Connection, id: &str, revision: u64) -> Result<()> {
    conn.execute(
        "UPDATE vault_entries SET dirty = 0, synced_revision = ?2 WHERE id = ?1 AND revision = ?2",
        params![id, i64_of(revision)],
    )?;
    Ok(())
}

pub fn load_sync(conn: &Connection) -> Result<Option<SyncRow>> {
    conn.query_row(
        "SELECT server_url, device_id, last_seq, manifest_version, header_version, header_auth, staged_header,
         joined_seal FROM vault_sync WHERE id = 1",
        [],
        |r| {
            Ok(SyncRow {
                server_url: r.get(0)?,
                device_id: r.get(1)?,
                last_seq: u64_of(r.get(2)?),
                manifest_version: u64_of(r.get(3)?),
                header_version: u64_of(r.get(4)?),
                header_auth: r.get(5)?,
                staged_header: r.get(6)?,
                joined_seal: r.get(7)?,
            })
        },
    )
    .optional()
}

pub fn save_sync(conn: &Connection, sync: &SyncRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO vault_sync
         (id, server_url, device_id, last_seq, manifest_version, header_version, header_auth, staged_header,
          joined_seal)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            sync.server_url,
            sync.device_id,
            i64_of(sync.last_seq),
            i64_of(sync.manifest_version),
            i64_of(sync.header_version),
            sync.header_auth,
            sync.staged_header,
            sync.joined_seal
        ],
    )?;
    Ok(())
}

/// Replaces the recovery phrase's fields alone.
pub fn set_recovery(conn: &Connection, wrap: &[u8], sealed: &[u8]) -> Result<()> {
    conn.execute(
        "UPDATE vault_meta SET recovery_wrap = ?1, recovery_sealed = ?2 WHERE id = 1",
        params![wrap, sealed],
    )?;
    Ok(())
}

pub fn delete_sync(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM vault_sync", [])?;
    Ok(())
}

/// Deletes the vault: its key, every entry and sync's state.
pub fn wipe(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM vault_entries; DELETE FROM vault_meta; DELETE FROM vault_sync;")
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
            recovery_wrap: vec![4; 72],
            recovery_sealed: vec![5; 72],
            failed_tries: 0,
            locked_until_ms: 0,
        }
    }

    fn entry(id: &str, t: &str) -> EntryRow {
        EntryRow {
            id: id.into(),
            nonce: vec![0; 24],
            ciphertext: vec![9; 20],
            updated_at: t.into(),
            revision: 1,
            deleted: false,
            dirty: true,
            synced_revision: 0,
        }
    }

    #[test]
    fn migrate_upgrades_old_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE vault_meta (id INTEGER PRIMARY KEY, version INTEGER NOT NULL, kdf_params TEXT NOT NULL,
             salt BLOB NOT NULL, wrapped_dek BLOB NOT NULL, failed_tries INTEGER NOT NULL DEFAULT 0,
             locked_until INTEGER NOT NULL DEFAULT 0);
             INSERT INTO vault_meta VALUES (1, 1, '{\"m\":8,\"t\":1,\"p\":1}', x'00', x'00', 0, 0);
             CREATE TABLE vault_entries (id TEXT PRIMARY KEY, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL,
             updated_at TEXT NOT NULL);
             INSERT INTO vault_entries VALUES ('old', x'00', x'00', 't');",
        )
        .unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let meta = load_meta(&conn).unwrap().unwrap();
        assert!(meta.key_check.is_empty() && meta.recovery_wrap.is_empty());
        assert!(meta.recovery_sealed.is_empty());
        let old = load_entry(&conn, "old").unwrap().unwrap();
        assert_eq!((old.revision, old.deleted, old.dirty), (0, false, false));
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
            upsert_entry(c, &entry("a", "1")).unwrap();
            upsert_entry(c, &entry("b", "1")).unwrap();
            upsert_entry(c, &entry("a", "2")).unwrap();
            assert_eq!(load_entries(c).unwrap().len(), 2);
            assert_eq!(load_entry(c, "a").unwrap().unwrap().updated_at, "2");
            delete_entry(c, "a").unwrap();
            assert!(load_entry(c, "a").unwrap().is_none());
            replace_entries(c, &[entry("c", "3")]).unwrap();
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

    #[test]
    fn synced_marks_only_the_same_revision() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            let mut row = entry("a", "1");
            row.revision = 3;
            upsert_entry(c, &row).unwrap();
            mark_synced(c, "a", 2).unwrap();
            assert!(load_entry(c, "a").unwrap().unwrap().dirty);
            mark_synced(c, "a", 3).unwrap();
            let synced = load_entry(c, "a").unwrap().unwrap();
            assert!(!synced.dirty);
            assert_eq!(synced.synced_revision, 3);
        });
    }

    #[test]
    fn sync_state_round_trips() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            assert!(load_sync(c).unwrap().is_none());
            let state = SyncRow {
                server_url: "http://100.64.0.1:8787".into(),
                device_id: "d1".into(),
                last_seq: 5,
                manifest_version: 2,
                header_version: 1,
                header_auth: Some(vec![1, 2]),
                staged_header: Some("{}".into()),
                joined_seal: Some(vec![3]),
            };
            save_sync(c, &state).unwrap();
            assert_eq!(load_sync(c).unwrap(), Some(state));
            delete_sync(c).unwrap();
            assert!(load_sync(c).unwrap().is_none());
        });
    }
}
