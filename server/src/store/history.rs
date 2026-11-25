//! The clipboard history's table: the newest sealed state of each item,
//! numbered by a sequence of its own. The server compares nothing: clients
//! merge what they fetch and send the merge back.

use clipboardx_sync_proto::Bytes;
use clipboardx_sync_proto::history::{HistoryDown, HistoryPage, HistoryUp};
use rusqlite::{Connection, Result, params};

use super::{Store, i64_of, u64_of};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS history (
        id         TEXT PRIMARY KEY,
        nonce      BLOB NOT NULL,
        ciphertext BLOB NOT NULL,
        seq        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS history_by_seq ON history(seq);
    INSERT OR IGNORE INTO counters (name, value) VALUES ('history_seq', 0);
";

fn counter(conn: &Connection) -> Result<u64> {
    conn.query_row(
        "SELECT value FROM counters WHERE name = 'history_seq'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(u64_of)
}

/// Forgets every item: they were sealed under a data key that is no longer
/// the vault's. The sequence goes on from where it was.
pub(super) fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM history", []).map(|_| ())
}

impl Store {
    /// The latest history sequence number.
    pub fn history_seq(&self) -> Result<u64> {
        counter(&self.conn)
    }

    /// Stores `records`, each replacing what the server had of its id, under
    /// new sequence numbers; the latest.
    pub fn push_history(&mut self, records: &[HistoryUp]) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let mut seq = counter(&tx)?;
        for record in records {
            seq += 1;
            tx.execute(
                "INSERT OR REPLACE INTO history (id, nonce, ciphertext, seq)
                 VALUES (?1, ?2, ?3, ?4)",
                params![record.id, record.nonce.0, record.ciphertext.0, i64_of(seq)],
            )?;
        }
        tx.execute(
            "UPDATE counters SET value = ?1 WHERE name = 'history_seq'",
            params![i64_of(seq)],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// The items changed after `since`, oldest first, up to about `budget`
    /// bytes of ciphertext (at least one item).
    pub fn history_page(&self, since: u64, budget: usize) -> Result<HistoryPage> {
        let seq = counter(&self.conn)?;
        let mut stmt = self.conn.prepare(
            "SELECT id, nonce, ciphertext, seq FROM history WHERE seq > ?1 ORDER BY seq",
        )?;
        let mut rows = stmt.query(params![i64_of(since)])?;
        let mut records = Vec::new();
        let mut bytes = 0;
        while let Some(row) = rows.next()? {
            let ciphertext: Vec<u8> = row.get(2)?;
            if !records.is_empty() && bytes + ciphertext.len() > budget {
                return Ok(HistoryPage {
                    records,
                    seq,
                    more: true,
                });
            }
            bytes += ciphertext.len();
            records.push(HistoryDown {
                id: row.get(0)?,
                nonce: Bytes(row.get(1)?),
                ciphertext: Bytes(ciphertext),
                seq: u64_of(row.get(3)?),
            });
        }
        Ok(HistoryPage {
            records,
            seq,
            more: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use clipboardx_sync_proto::VaultHeader;

    use super::*;

    fn item(id: char, ciphertext: &[u8]) -> HistoryUp {
        HistoryUp {
            id: id.to_string().repeat(64),
            nonce: Bytes(vec![1; 24]),
            ciphertext: Bytes(ciphertext.to_vec()),
        }
    }

    fn ids(page: &HistoryPage) -> Vec<(char, u64)> {
        page.records
            .iter()
            .map(|r| (r.id.chars().next().unwrap(), r.seq))
            .collect()
    }

    #[test]
    fn items_get_rising_numbers_and_only_the_latest_of_each_stays() {
        let mut store = Store::in_memory().unwrap();
        assert_eq!(store.history_seq().unwrap(), 0);
        assert_eq!(store.push_history(&[item('a', b"1"), item('b', b"1")]).unwrap(), 2);
        assert_eq!(store.push_history(&[item('a', b"2")]).unwrap(), 3);

        let page = store.history_page(0, usize::MAX).unwrap();
        assert_eq!(ids(&page), vec![('b', 2), ('a', 3)]);
        assert_eq!(page.records[1].ciphertext, Bytes(b"2".to_vec()));
        assert_eq!((page.seq, page.more), (3, false));
        assert_eq!(ids(&store.history_page(2, usize::MAX).unwrap()), vec![('a', 3)]);
        assert!(store.history_page(3, usize::MAX).unwrap().records.is_empty());
    }

    #[test]
    fn pages_hold_about_the_budget_and_at_least_one_item() {
        let mut store = Store::in_memory().unwrap();
        store
            .push_history(&[item('a', &[0; 10]), item('b', &[0; 10]), item('c', &[0; 30])])
            .unwrap();

        let first = store.history_page(0, 25).unwrap();
        assert_eq!((ids(&first), first.more), (vec![('a', 1), ('b', 2)], true));
        // Larger than the budget alone: still handed out.
        let second = store.history_page(2, 25).unwrap();
        assert_eq!((ids(&second), second.more), (vec![('c', 3)], false));
        assert_eq!(second.seq, 3);
    }

    fn header(version: u64) -> VaultHeader {
        VaultHeader {
            version,
            format: 2,
            kdf_m: 1,
            kdf_t: 1,
            kdf_p: 1,
            salt: Bytes(vec![]),
            wrapped_key: Bytes(vec![]),
            key_check: Bytes(vec![]),
            auth: None,
            recovery_wrap: None,
            recovery_sealed: None,
            seal: None,
        }
    }

    #[test]
    fn a_new_header_forgets_the_items_sealed_under_the_old_one() {
        let mut store = Store::in_memory().unwrap();
        store.update_vault(Some((&header(1), 0)), None).unwrap().unwrap();
        store.push_history(&[item('a', b"1")]).unwrap();
        // A new manifest alone keeps them.
        let manifest = clipboardx_sync_proto::Manifest {
            version: 1,
            sealed: Bytes(vec![]),
        };
        store.update_vault(None, Some((&manifest, 0))).unwrap().unwrap();
        assert_eq!(store.history_page(0, usize::MAX).unwrap().records.len(), 1);

        store.update_vault(Some((&header(2), 1)), None).unwrap().unwrap();
        let page = store.history_page(0, usize::MAX).unwrap();
        assert!(page.records.is_empty());
        // Numbering goes on: a device's cursor never points past the end.
        assert_eq!(page.seq, 1);
        assert_eq!(store.push_history(&[item('b', b"1")]).unwrap(), 2);
    }

    #[test]
    fn a_refused_header_keeps_the_items() {
        let mut store = Store::in_memory().unwrap();
        store.update_vault(Some((&header(1), 0)), None).unwrap().unwrap();
        store.push_history(&[item('a', b"1")]).unwrap();
        assert!(store.update_vault(Some((&header(2), 0)), None).unwrap().is_err());
        assert_eq!(store.history_page(0, usize::MAX).unwrap().records.len(), 1);
    }
}
