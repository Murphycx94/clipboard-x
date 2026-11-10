//! The server's SQLite file: devices (token hashes only), the latest
//! ciphertext of each record, the vault's header and sealed manifest.

use std::path::Path;

use clipboardx_sync_proto::{
    Bytes, Device, Manifest, RecordDown, RecordUp, VaultHeader, VaultState,
};
use rusqlite::{Connection, OptionalExtension, Result, params};

const SCHEMA: &str = "
    PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS devices (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        last_seen  INTEGER NOT NULL DEFAULT 0,
        revoked    INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS records (
        id         TEXT PRIMARY KEY,
        revision   INTEGER NOT NULL,
        deleted    INTEGER NOT NULL,
        nonce      BLOB NOT NULL,
        ciphertext BLOB NOT NULL,
        seq        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS records_seq ON records(seq);
    CREATE TABLE IF NOT EXISTS vault (
        id               INTEGER PRIMARY KEY CHECK (id = 1),
        header           TEXT,
        header_version   INTEGER NOT NULL DEFAULT 0,
        manifest         TEXT,
        manifest_version INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS counters (
        name  TEXT PRIMARY KEY,
        value INTEGER NOT NULL
    );
    INSERT OR IGNORE INTO vault (id) VALUES (1);
    INSERT OR IGNORE INTO counters (name, value) VALUES ('seq', 0);
";

/// Why a write was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The client's base is not the server's current version.
    Stale,
    /// A version that does not move forward.
    NotNewer,
}

pub struct Store {
    conn: Connection,
}

fn u64_of(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn i64_of(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn json_error(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        Store::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn seq(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT value FROM counters WHERE name = 'seq'", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(u64_of)
    }

    // --- Devices ---------------------------------------------------------

    pub fn active_devices(&self) -> Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM devices WHERE revoked = 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| usize::try_from(n).unwrap_or(0))
    }

    pub fn add_device(&self, id: &str, name: &str, token_hash: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices (id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, token_hash, now],
        )?;
        Ok(())
    }

    /// The active device whose token hashes to `token_hash`, seen now.
    pub fn device_by_token(&self, token_hash: &str, now: i64) -> Result<Option<String>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM devices WHERE token_hash = ?1 AND revoked = 0",
                params![token_hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = &id {
            self.conn.execute(
                "UPDATE devices SET last_seen = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        Ok(id)
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, last_seen FROM devices WHERE revoked = 0 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Device {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                last_seen: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Revokes device `id`; whether it was active.
    pub fn revoke(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?1 AND revoked = 0",
            params![id],
        )? > 0)
    }

    // --- The vault -------------------------------------------------------

    pub fn vault(&self) -> Result<VaultState> {
        self.conn
            .query_row("SELECT header, manifest FROM vault WHERE id = 1", [], |r| {
                let header: Option<String> = r.get(0)?;
                let manifest: Option<String> = r.get(1)?;
                Ok(VaultState {
                    header: header.and_then(|h| serde_json::from_str(&h).ok()),
                    manifest: manifest.and_then(|m| serde_json::from_str(&m).ok()),
                })
            })
    }

    fn versions(&self) -> Result<(u64, u64)> {
        self.conn.query_row(
            "SELECT header_version, manifest_version FROM vault WHERE id = 1",
            [],
            |r| Ok((u64_of(r.get(0)?), u64_of(r.get(1)?))),
        )
    }

    pub fn header_version(&self) -> Result<u64> {
        Ok(self.versions()?.0)
    }

    /// Replaces the header and the manifest given, each only over the
    /// version the client based it on and only forward; both or neither.
    pub fn update_vault(
        &mut self,
        header: Option<(&VaultHeader, u64)>,
        manifest: Option<(&Manifest, u64)>,
    ) -> Result<std::result::Result<(), Refused>> {
        let tx = self.conn.transaction()?;
        let (header_version, manifest_version) = tx.query_row(
            "SELECT header_version, manifest_version FROM vault WHERE id = 1",
            [],
            |r| Ok((u64_of(r.get(0)?), u64_of(r.get(1)?))),
        )?;
        if let Some((header, base)) = header {
            if base != header_version {
                return Ok(Err(Refused::Stale));
            }
            if header.version <= header_version {
                return Ok(Err(Refused::NotNewer));
            }
            tx.execute(
                "UPDATE vault SET header = ?1, header_version = ?2 WHERE id = 1",
                params![
                    serde_json::to_string(header).map_err(json_error)?,
                    i64_of(header.version)
                ],
            )?;
        }
        if let Some((manifest, base)) = manifest {
            if base != manifest_version {
                return Ok(Err(Refused::Stale));
            }
            if manifest.version <= manifest_version {
                return Ok(Err(Refused::NotNewer));
            }
            tx.execute(
                "UPDATE vault SET manifest = ?1, manifest_version = ?2 WHERE id = 1",
                params![
                    serde_json::to_string(manifest).map_err(json_error)?,
                    i64_of(manifest.version)
                ],
            )?;
        }
        tx.commit()?;
        Ok(Ok(()))
    }

    // --- Records ---------------------------------------------------------

    /// Stores the records whose base revision is the server's; the new
    /// sequence number and the ids refused.
    pub fn push(&mut self, records: &[RecordUp]) -> Result<(u64, Vec<String>)> {
        let tx = self.conn.transaction()?;
        let mut seq = u64_of(tx.query_row(
            "SELECT value FROM counters WHERE name = 'seq'",
            [],
            |r| r.get(0),
        )?);
        let mut conflicts = Vec::new();
        for record in records {
            let current: u64 = tx
                .query_row(
                    "SELECT revision FROM records WHERE id = ?1",
                    params![record.id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .map_or(0, u64_of);
            if current != record.base_revision || record.revision <= current {
                conflicts.push(record.id.clone());
                continue;
            }
            seq += 1;
            tx.execute(
                "INSERT OR REPLACE INTO records (id, revision, deleted, nonce, ciphertext, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id,
                    i64_of(record.revision),
                    record.deleted,
                    record.nonce.0,
                    record.ciphertext.0,
                    i64_of(seq)
                ],
            )?;
        }
        tx.execute(
            "UPDATE counters SET value = ?1 WHERE name = 'seq'",
            params![i64_of(seq)],
        )?;
        tx.commit()?;
        Ok((seq, conflicts))
    }

    /// The records changed after `since`, oldest first.
    pub fn changes(&self, since: u64) -> Result<Vec<RecordDown>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, revision, deleted, nonce, ciphertext, seq FROM records WHERE seq > ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![i64_of(since)], |r| {
            Ok(RecordDown {
                id: r.get(0)?,
                revision: u64_of(r.get(1)?),
                deleted: r.get::<_, i64>(2)? != 0,
                nonce: Bytes(r.get(3)?),
                ciphertext: Bytes(r.get(4)?),
                seq: u64_of(r.get(5)?),
            })
        })?;
        rows.collect()
    }
}
