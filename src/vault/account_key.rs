//! The secret key: 128 random bits that, with the master password, derive
//! the key-encryption key. It lives in this device's keychain and in the
//! user's own records, never on the sync server.
//!
//! Written `CX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XX`: 26 characters of a 32-letter
//! alphabet without look-alikes (no 0, O, 1, I) and a check character.

use std::fmt;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::crypto::{CryptoError, random_bytes};

pub const ACCOUNT_KEY_LEN: usize = 16;
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const DATA_CHARS: usize = 26;
const PREFIX: &str = "CX";

/// The secret key, wiped when dropped.
pub struct AccountKey(Zeroizing<[u8; ACCOUNT_KEY_LEN]>);

impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountKey(..)")
    }
}

fn check_char(bytes: &[u8]) -> u8 {
    ALPHABET[usize::from(Sha256::digest(bytes)[0] % 32)]
}

impl AccountKey {
    pub fn generate() -> Result<AccountKey, CryptoError> {
        Ok(AccountKey(Zeroizing::new(
            random_bytes::<ACCOUNT_KEY_LEN>()?
        )))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<AccountKey> {
        let array: [u8; ACCOUNT_KEY_LEN] = bytes.try_into().ok()?;
        Some(AccountKey(Zeroizing::new(array)))
    }

    pub fn as_bytes(&self) -> &[u8; ACCOUNT_KEY_LEN] {
        &self.0
    }

    /// `CX-` and groups of five.
    pub fn encode(&self) -> Zeroizing<String> {
        // 128 bits as 26 five-bit digits, the top digit holding 3 bits.
        let mut value = u128::from_be_bytes(*self.0);
        let mut chars = [0_u8; DATA_CHARS + 1];
        for slot in chars[..DATA_CHARS].iter_mut().rev() {
            *slot = ALPHABET[(value & 31) as usize];
            value >>= 5;
        }
        chars[DATA_CHARS] = check_char(&self.0[..]);
        let mut out = Zeroizing::new(String::from(PREFIX));
        for group in chars.chunks(5) {
            out.push('-');
            out.extend(group.iter().map(|&c| c as char));
        }
        out
    }

    /// The last full group of [`AccountKey::encode`]: what the user types to
    /// show they saved it.
    pub fn confirmation_group(&self) -> Zeroizing<String> {
        let encoded = self.encode();
        let group = encoded.split('-').nth(5).unwrap_or_default().to_owned();
        Zeroizing::new(group)
    }

    /// Reads a key as written: case, spaces and dashes do not matter, the
    /// `CX` prefix is optional; the check character must match.
    pub fn parse(text: &str) -> Option<AccountKey> {
        let cleaned: Zeroizing<String> = Zeroizing::new(
            text.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .collect(),
        );
        // The prefix is optional; the length tells whether it is there (the
        // data itself may begin with the same letters).
        let body = match cleaned.len() {
            n if n == PREFIX.len() + DATA_CHARS + 1 => cleaned.strip_prefix(PREFIX)?,
            n if n == DATA_CHARS + 1 => cleaned.as_str(),
            _ => return None,
        };
        let mut value: u128 = 0;
        for &c in &body.as_bytes()[..DATA_CHARS] {
            let digit = ALPHABET.iter().position(|&a| a == c)? as u128;
            value = value.checked_mul(32)?.checked_add(digit)?;
        }
        let key = AccountKey(Zeroizing::new(value.to_be_bytes()));
        (body.as_bytes()[DATA_CHARS] == check_char(&key.0[..])).then_some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_parses_back() {
        for _ in 0..50 {
            let key = AccountKey::generate().unwrap();
            let text = key.encode();
            assert_eq!(text.len(), 2 + 6 * 6 - 3);
            assert!(text.starts_with("CX-"));
            let back = AccountKey::parse(&text).unwrap();
            assert_eq!(back.as_bytes(), key.as_bytes());
            // Lowercase, spaces, no prefix.
            let loose = text[3..].replace('-', " ").to_lowercase();
            assert_eq!(
                AccountKey::parse(&loose).unwrap().as_bytes(),
                key.as_bytes()
            );
        }
    }

    #[test]
    fn data_starting_like_the_prefix_parses_without_it() {
        // Find a key whose data begins with "CX".
        let key = (0..u32::MAX)
            .map(|_| AccountKey::generate().unwrap())
            .find(|k| k.encode()[3..].starts_with("CX"))
            .unwrap();
        let text = key.encode();
        let without_prefix = text[3..].to_string();
        assert_eq!(
            AccountKey::parse(&without_prefix).unwrap().as_bytes(),
            key.as_bytes()
        );
        assert_eq!(AccountKey::parse(&text).unwrap().as_bytes(), key.as_bytes());
    }

    #[test]
    fn extremes_round_trip() {
        for bytes in [[0_u8; 16], [0xff; 16]] {
            let key = AccountKey::from_slice(&bytes).unwrap();
            assert_eq!(AccountKey::parse(&key.encode()).unwrap().as_bytes(), &bytes);
        }
    }

    #[test]
    fn typos_are_caught() {
        let key = AccountKey::from_slice(&[7; 16]).unwrap();
        let text = key.encode().to_string();
        let mut chars: Vec<char> = text.chars().collect();
        let at = 4;
        chars[at] = if chars[at] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert!(AccountKey::parse(&typo).is_none());
        assert!(AccountKey::parse("CX-SHORT").is_none());
        assert!(AccountKey::parse(&text.replace('A', "0")).is_none() || !text.contains('A'));
    }

    #[test]
    fn confirmation_is_the_last_full_group() {
        let key = AccountKey::from_slice(&[1; 16]).unwrap();
        let text = key.encode();
        assert_eq!(*key.confirmation_group(), text.split('-').nth(5).unwrap());
        assert_eq!(key.confirmation_group().len(), 5);
    }

    #[test]
    fn debug_hides_the_key() {
        let key = AccountKey::from_slice(&[9; 16]).unwrap();
        assert_eq!(format!("{key:?}"), "AccountKey(..)");
    }
}
