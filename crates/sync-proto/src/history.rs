//! The clipboard history's part of the API: each item's latest state,
//! sealed by a client. The server keeps the newest ciphertext of every id
//! and hands them out by sequence number; it reads none of them.

use serde::{Deserialize, Serialize};

use crate::Bytes;

/// The largest sealed item the server stores: an item of up to 256 KiB of
/// text, escaped as JSON, with room to spare.
pub const MAX_CIPHERTEXT: usize = 2 * 1024 * 1024;

/// About how much ciphertext one page of `GET /history` holds (at least one
/// item), and one upload carries.
pub const PAGE_BYTES: usize = 4 * 1024 * 1024;

/// The length of a nonce (XChaCha20-Poly1305).
pub const NONCE_LEN: usize = 24;

/// Whether `id` is one a client makes: 64 lowercase hexadecimal digits.
pub fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An item going up: its whole latest state, sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryUp {
    pub id: String,
    pub nonce: Bytes,
    pub ciphertext: Bytes,
}

/// `POST /history`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPush {
    /// The header version whose data key sealed the items.
    pub header_version: u64,
    pub records: Vec<HistoryUp>,
}

/// `POST /history`'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPushed {
    /// The latest history sequence number.
    pub seq: u64,
}

/// An item coming down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDown {
    pub id: String,
    pub nonce: Bytes,
    pub ciphertext: Bytes,
    pub seq: u64,
}

/// `GET /history?since=n&header=v`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    /// Items changed after `since`, oldest first.
    pub records: Vec<HistoryDown>,
    /// The latest history sequence number.
    pub seq: u64,
    /// Whether more follow the last of `records`.
    pub more: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_64_lowercase_hex_digits() {
        assert!(valid_id(&"0123456789abcdef".repeat(4)));
        assert!(!valid_id(&"0123456789ABCDEF".repeat(4)));
        assert!(!valid_id(&"a".repeat(63)));
        assert!(!valid_id(&"a".repeat(65)));
        assert!(!valid_id(&format!("{}g", "a".repeat(63))));
        assert!(!valid_id(""));
    }
}
