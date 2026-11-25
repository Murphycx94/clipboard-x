//! The server's SQLite file: devices (token hashes only), the latest
//! ciphertext of each record, the vault's header and sealed manifest, and
//! the clipboard history's items ([`history`]).

mod history;

use std::path::Path;

use clipboardx_sync_proto::{
    Bytes, Device, Manifest, RecordDown, RecordUp, VaultHeader, VaultState,
};
use rusqlite::{Connection, OptionalExtension, Result, params};

/// Seconds between two notes of when a device was last seen.
const SEEN_EVERY: i64 = 60;

const SCHEMA: &str = "
    PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS devices (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at INTEGER NOT NULL,
        last_seen  INTEGER NOT NULL DEFAULT 0,
        revoked    INTEGER NOT NULL DEFAULT 0,
        added_by   TEXT
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

/// How far past the server's a version (a header's, a manifest's, a
/// record's revision) may be. Clients count up by one; a device long away
/// from the server is some steps ahead, never this many. Without a limit,
/// one upload at the largest number would leave no number above it: nothing
/// could ever replace that header, manifest or record.
pub const MAX_VERSION_STEP: u64 = 1 << 20;

/// Why a write was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The client's base is not the server's current version.
    Stale,
    /// A version that does not move forward.
    NotNewer,
    /// A version more than [`MAX_VERSION_STEP`] ahead.
    TooFar,
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
        conn.execute_batch(history::SCHEMA)?;
        // A database from before devices had adders.
        let has_adder = conn
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'added_by'")?
            .exists([])?;
        if !has_adder {
            conn.execute_batch("ALTER TABLE devices ADD COLUMN added_by TEXT")?;
        }
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

    /// Registers a device, added by device `added_by` when one let it in;
    /// whether it did. Not when that device is no longer active: a pairing
    /// code its revocation missed (one asked for at that very moment) lets
    /// nobody in.
    pub fn add_device(
        &self,
        id: &str,
        name: &str,
        token_hash: &str,
        now: i64,
        added_by: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT INTO devices (id, name, token_hash, created_at, added_by)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE ?5 IS NULL
                OR EXISTS (SELECT 1 FROM devices WHERE id = ?5 AND revoked = 0)",
            params![id, name, token_hash, now, added_by],
        )? > 0)
    }

    /// The active device whose token hashes to `token_hash`, seen now
    /// (noted once in [`SEEN_EVERY`] seconds: a device asks every few
    /// seconds, and each note is a write to the disk).
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
                // Either way: a clock set back does not freeze it.
                "UPDATE devices SET last_seen = ?2
                 WHERE id = ?1 AND (last_seen <= ?2 - ?3 OR last_seen > ?2)",
                params![id, now, SEEN_EVERY],
            )?;
        }
        Ok(id)
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, last_seen, added_by FROM devices
             WHERE revoked = 0 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Device {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                last_seen: r.get(3)?,
                added_by: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Whether device `id` is registered and not revoked.
    pub fn is_active(&self, id: &str) -> Result<bool> {
        self.conn
            .prepare("SELECT 1 FROM devices WHERE id = ?1 AND revoked = 0")?
            .exists(params![id])
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
    /// version the client based it on and only forward, by no more than
    /// [`MAX_VERSION_STEP`]; both or neither.
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
            if header.version - header_version > MAX_VERSION_STEP {
                return Ok(Err(Refused::TooFar));
            }
            tx.execute(
                "UPDATE vault SET header = ?1, header_version = ?2 WHERE id = 1",
                params![
                    serde_json::to_string(header).map_err(json_error)?,
                    i64_of(header.version)
                ],
            )?;
            // Sealed under the old header's key: the devices send theirs
            // again under the new one.
            history::clear(&tx)?;
        }
        if let Some((manifest, base)) = manifest {
            if base != manifest_version {
                return Ok(Err(Refused::Stale));
            }
            if manifest.version <= manifest_version {
                return Ok(Err(Refused::NotNewer));
            }
            if manifest.version - manifest_version > MAX_VERSION_STEP {
                return Ok(Err(Refused::TooFar));
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

    /// Stores the records whose base revision is the server's and whose
    /// revision is ahead of it by no more than [`MAX_VERSION_STEP`]; the new
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
            let moves = current == record.base_revision
                && record.revision > current
                && record.revision - current <= MAX_VERSION_STEP;
            if !moves {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_noted_as_seen_now_and_then() {
        let store = Store::in_memory().unwrap();
        store.add_device("d", "Mac", "hash", 1_000, None).unwrap();
        let seen = |store: &Store| store.devices().unwrap()[0].last_seen;
        let found = |now| store.device_by_token("hash", now).unwrap();

        assert_eq!(found(1_000), Some("d".to_owned()));
        assert_eq!(seen(&store), 1_000);
        // Every request finds the device; few move the time.
        assert_eq!(found(1_000 + SEEN_EVERY - 1), Some("d".to_owned()));
        assert_eq!(seen(&store), 1_000);
        assert_eq!(found(1_000 + SEEN_EVERY), Some("d".to_owned()));
        assert_eq!(seen(&store), 1_000 + SEEN_EVERY);
        // A clock set back: noted again, not frozen at a time to come.
        assert_eq!(found(500), Some("d".to_owned()));
        assert_eq!(seen(&store), 500);
        assert_eq!(store.device_by_token("other", 9_000).unwrap(), None);
    }

    #[test]
    fn a_database_from_before_devices_had_adders_is_brought_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.db");
        // As the first release made it.
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE devices (
                    id         TEXT PRIMARY KEY,
                    name       TEXT NOT NULL,
                    token_hash TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL,
                    last_seen  INTEGER NOT NULL DEFAULT 0,
                    revoked    INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO devices (id, name, token_hash, created_at)
                VALUES ('old', 'Mac', 'h1', 1);",
            )
            .unwrap();
        {
            let store = Store::open(&path).unwrap();
            assert!(
                store
                    .add_device("new", "Phone", "h2", 2, Some("old"))
                    .unwrap()
            );
        }
        // Opened again: nothing to bring up, nothing lost.
        let store = Store::open(&path).unwrap();
        let devices = store.devices().unwrap();
        let adders: Vec<_> = devices
            .iter()
            .map(|d| (d.id.as_str(), d.added_by.as_deref()))
            .collect();
        assert_eq!(adders, vec![("old", None), ("new", Some("old"))]);
        assert_eq!(
            store.device_by_token("h1", 10).unwrap().as_deref(),
            Some("old")
        );
    }

    #[test]
    fn a_revoked_device_adds_no_device() {
        let store = Store::in_memory().unwrap();
        assert!(store.add_device("a", "Mac", "h1", 1, None).unwrap());
        assert!(store.add_device("b", "Phone", "h2", 2, Some("a")).unwrap());
        assert!(store.revoke("a").unwrap());
        // With a pairing code it made before, whenever that code is used.
        assert!(!store.add_device("c", "Tablet", "h3", 3, Some("a")).unwrap());
        assert!(
            !store
                .add_device("d", "Tablet", "h4", 4, Some("nobody"))
                .unwrap()
        );
        let ids: Vec<_> = store.devices().unwrap().into_iter().map(|d| d.id).collect();
        assert_eq!(ids, vec!["b".to_owned()]);
        // Those it added while it could stay.
        assert!(store.is_active("b").unwrap());
    }

    #[test]
    fn only_active_devices_are_active() {
        let store = Store::in_memory().unwrap();
        store.add_device("d", "Mac", "hash", 1, None).unwrap();
        assert!(store.is_active("d").unwrap());
        assert!(!store.is_active("stranger").unwrap());
        assert!(store.revoke("d").unwrap());
        assert!(!store.is_active("d").unwrap());
    }
}
