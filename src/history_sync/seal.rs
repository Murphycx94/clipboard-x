//! The history's keys, derived from the vault's data key, and sealing an
//! item for the server. The server sees an id it cannot link to any text
//! (a keyed hash of the text's hash) and ciphertext bound to that id.

use clipboardx_sync_proto::Bytes;
use clipboardx_sync_proto::history::{HistoryDown, HistoryUp};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::item::ItemState;
use crate::capture::hash_bytes;
use crate::vault::VaultError;
use crate::vault::crypto::{self, KEY_LEN, SecretKey, Sealed};

const SEAL_LABEL: &[u8] = b"clipboardx/history/seal/v1";
const ID_LABEL: &[u8] = b"clipboardx/history/id/v1";

/// HMAC-SHA256 of `data` under `key`.
fn prf(key: &[u8], data: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| VaultError::Crypto)?;
    mac.update(data);
    Ok(Zeroizing::new(mac.finalize().into_bytes().into()))
}

/// What seals the clipboard history: never stored, made again from the
/// data key whenever the vault is unlocked.
pub struct HistoryKeys {
    seal: SecretKey,
    id: Zeroizing<[u8; KEY_LEN]>,
}

/// What binds a sealed item to its id: the server cannot pass one item off
/// as another.
fn aad(id: &str) -> Vec<u8> {
    format!("clipboardx/history/v1:{id}").into_bytes()
}

impl HistoryKeys {
    pub fn derive(data_key: &SecretKey) -> Result<HistoryKeys, VaultError> {
        let seal = prf(data_key.as_bytes(), SEAL_LABEL)?;
        Ok(HistoryKeys {
            seal: SecretKey::from_slice(seal.as_slice())?,
            id: prf(data_key.as_bytes(), ID_LABEL)?,
        })
    }

    /// The server's name of the item whose text hashes to `hash`.
    pub fn id_of(&self, hash: &str) -> Result<String, VaultError> {
        Ok(hex::encode(prf(self.id.as_slice(), hash.as_bytes())?.as_slice()))
    }

    /// `item` sealed, to upload.
    pub fn seal(&self, item: &ItemState) -> Result<HistoryUp, VaultError> {
        let id = self.id_of(&item.hash)?;
        let plain = Zeroizing::new(serde_json::to_vec(item).map_err(|_| VaultError::Crypto)?);
        let Sealed { nonce, ciphertext } = crypto::seal(&self.seal, &plain, &aad(&id))?;
        Ok(HistoryUp {
            id,
            nonce: Bytes(nonce.to_vec()),
            ciphertext: Bytes(ciphertext),
        })
    }

    /// The item `record` seals, if it is one a device sealed under these
    /// keys for that id: its id is the one its hash names, an alive item
    /// carries the text of that hash, a deleted one no text. Anything else
    /// is [`VaultError::Tampered`].
    pub fn open(&self, record: &HistoryDown) -> Result<ItemState, VaultError> {
        let sealed = Sealed::from_parts(&record.nonce.0, &record.ciphertext.0)
            .map_err(|_| VaultError::Tampered)?;
        let plain =
            crypto::open(&self.seal, &sealed, &aad(&record.id)).map_err(|_| VaultError::Tampered)?;
        let item: ItemState = serde_json::from_slice(&plain).map_err(|_| VaultError::Tampered)?;
        if self.id_of(&item.hash)? != record.id {
            return Err(VaultError::Tampered);
        }
        let whole = match (&item.text, item.alive()) {
            (Some(text), true) => hash_bytes(text.as_bytes()) == item.hash,
            (None, false) => true,
            _ => false,
        };
        if whole { Ok(item) } else { Err(VaultError::Tampered) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(byte: u8) -> HistoryKeys {
        HistoryKeys::derive(&SecretKey::from_slice(&[byte; KEY_LEN]).unwrap()).unwrap()
    }

    fn item(text: &str) -> ItemState {
        ItemState {
            hash: hash_bytes(text.as_bytes()),
            text: Some(text.into()),
            copied_at: 10,
            meta_at: 0,
            favorite: false,
            note: String::new(),
            deleted_at: 0,
        }
    }

    fn down(up: HistoryUp) -> HistoryDown {
        HistoryDown {
            id: up.id,
            nonce: up.nonce,
            ciphertext: up.ciphertext,
            seq: 1,
        }
    }

    /// What `keys` would seal for `item` without checking it: a device
    /// gone wrong, or a forger holding the key.
    fn forged(keys: &HistoryKeys, id: &str, item: &ItemState) -> HistoryDown {
        let plain = serde_json::to_vec(item).unwrap();
        let sealed = crypto::seal(&keys.seal, &plain, &aad(id)).unwrap();
        HistoryDown {
            id: id.into(),
            nonce: Bytes(sealed.nonce.to_vec()),
            ciphertext: Bytes(sealed.ciphertext),
            seq: 1,
        }
    }

    #[test]
    fn items_round_trip() {
        let keys = keys(1);
        let alive = item("hello");
        assert_eq!(keys.open(&down(keys.seal(&alive).unwrap())).unwrap(), alive);
        let deleted = ItemState {
            text: None,
            meta_at: 20,
            deleted_at: 20,
            ..item("hello")
        };
        assert_eq!(keys.open(&down(keys.seal(&deleted).unwrap())).unwrap(), deleted);
    }

    #[test]
    fn ids_name_the_text_only_under_the_vaults_key() {
        let hash = hash_bytes(b"hello");
        let id = keys(1).id_of(&hash).unwrap();
        assert_eq!(id, keys(1).id_of(&hash).unwrap());
        assert_eq!(id.len(), 64);
        assert!(clipboardx_sync_proto::history::valid_id(&id));
        assert_ne!(id, hash);
        assert_ne!(id, keys(2).id_of(&hash).unwrap());
        assert_ne!(id, keys(1).id_of(&hash_bytes(b"other")).unwrap());
    }

    #[test]
    fn another_vaults_items_do_not_open() {
        let sealed = down(keys(1).seal(&item("hello")).unwrap());
        assert_eq!(keys(2).open(&sealed), Err(VaultError::Tampered));
    }

    #[test]
    fn an_item_moved_to_another_id_does_not_open() {
        let keys = keys(1);
        let mut moved = down(keys.seal(&item("hello")).unwrap());
        moved.id = keys.id_of(&hash_bytes(b"other")).unwrap();
        assert_eq!(keys.open(&moved), Err(VaultError::Tampered));
        let mut cut = down(keys.seal(&item("hello")).unwrap());
        cut.ciphertext.0.pop();
        assert_eq!(keys.open(&cut), Err(VaultError::Tampered));
    }

    #[test]
    fn an_item_whose_parts_disagree_is_refused() {
        let keys = keys(1);
        let hello = item("hello");
        let id = keys.id_of(&hello.hash).unwrap();
        // Sealed under the right id, but another text.
        let swapped = ItemState {
            text: Some("goodbye".into()),
            ..hello.clone()
        };
        assert_eq!(keys.open(&forged(&keys, &id, &swapped)), Err(VaultError::Tampered));
        // Another hash than the id names.
        let renamed = item("goodbye");
        assert_eq!(keys.open(&forged(&keys, &id, &renamed)), Err(VaultError::Tampered));
        // Alive without its text, deleted with it.
        let textless = ItemState {
            text: None,
            ..hello.clone()
        };
        assert_eq!(keys.open(&forged(&keys, &id, &textless)), Err(VaultError::Tampered));
        let deleted_with_text = ItemState {
            deleted_at: 20,
            meta_at: 20,
            ..hello
        };
        assert_eq!(
            keys.open(&forged(&keys, &id, &deleted_with_text)),
            Err(VaultError::Tampered)
        );
    }
}
