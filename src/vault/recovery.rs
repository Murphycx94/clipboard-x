//! The recovery phrase: 256 random bits written as 24 characters of BIP39's
//! simplified Chinese word list (the last carries a checksum). It opens the
//! vault without the master password or the secret key, so it is shown once
//! and kept nowhere in the clear: the vault stores the data key wrapped by
//! it, and the phrase sealed under the data key (to wrap the next data key
//! when the master password changes).

use bip39::{Language, Mnemonic};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::account_key::{ACCOUNT_KEY_LEN, AccountKey};
use super::crypto::{CryptoError, KEY_LEN, NONCE_LEN, Sealed, SecretKey, open, random_bytes, seal};

pub const RECOVERY_KEY_LEN: usize = 32;
/// Words in a phrase.
pub const WORDS: usize = 24;
const KEK_DOMAIN: &[u8] = b"clipboardx/vault/recovery-kek/v1";
const WRAP_AAD: &[u8] = b"clipboardx/vault/recovery-wrap/v1";
const SEAL_AAD: &[u8] = b"clipboardx/vault/recovery-key/v1";
const TAG_LEN: usize = 16;
/// What the phrase wraps: the data key, and the secret key when the vault
/// uses one.
pub const WRAP_LENS: [usize; 2] = [
    NONCE_LEN + KEY_LEN + TAG_LEN,
    NONCE_LEN + KEY_LEN + ACCOUNT_KEY_LEN + TAG_LEN,
];
/// The phrase sealed under the data key.
pub const SEALED_LEN: usize = NONCE_LEN + RECOVERY_KEY_LEN + TAG_LEN;

/// The recovery key, wiped when dropped.
pub struct RecoveryKey(Zeroizing<[u8; RECOVERY_KEY_LEN]>);

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey(..)")
    }
}

impl Clone for RecoveryKey {
    fn clone(&self) -> Self {
        RecoveryKey(Zeroizing::new(*self.0))
    }
}

impl PartialEq for RecoveryKey {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl RecoveryKey {
    pub fn generate() -> Result<RecoveryKey, CryptoError> {
        Ok(RecoveryKey(Zeroizing::new(
            random_bytes::<RECOVERY_KEY_LEN>()?,
        )))
    }

    /// The key of 32 raw bytes (tests look for it in stored blobs).
    #[cfg(test)]
    pub(crate) fn parse_bytes(bytes: &[u8]) -> Option<RecoveryKey> {
        RecoveryKey::from_slice(bytes)
    }

    fn from_slice(bytes: &[u8]) -> Option<RecoveryKey> {
        let array: [u8; RECOVERY_KEY_LEN] = bytes.try_into().ok()?;
        Some(RecoveryKey(Zeroizing::new(array)))
    }

    /// The 24 words, in order.
    pub fn words(&self) -> Vec<&'static str> {
        // 32 bytes is a valid BIP39 entropy length.
        Mnemonic::from_entropy_in(Language::SimplifiedChinese, &self.0[..])
            .map(|m| m.words().collect())
            .unwrap_or_default()
    }

    /// The phrase as typed: every character of the list counts as a word;
    /// spaces and punctuation between them do not matter. `None` unless it
    /// is 24 words of the list with a valid checksum.
    pub fn parse(text: &str) -> Option<RecoveryKey> {
        let chars: Zeroizing<Vec<String>> = Zeroizing::new(
            text.chars()
                .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation() && !is_cjk_mark(*c))
                .map(String::from)
                .collect(),
        );
        let words = Zeroizing::new(chars.join(" "));
        let mnemonic = Mnemonic::parse_in_normalized(Language::SimplifiedChinese, &words).ok()?;
        if mnemonic.word_count() != WORDS {
            return None;
        }
        let (entropy, len) = mnemonic.to_entropy_array();
        let key = RecoveryKey::from_slice(&entropy[..len]);
        let mut entropy = entropy;
        zeroize::Zeroize::zeroize(&mut entropy);
        key
    }

    /// The key that wraps the data key: the phrase's 256 bits need no slow
    /// derivation, only one for this purpose.
    fn kek(&self) -> SecretKey {
        let mut hasher = Sha256::new();
        hasher.update(KEK_DOMAIN);
        hasher.update(&self.0[..]);
        let digest = Zeroizing::new(<[u8; KEY_LEN]>::from(hasher.finalize()));
        // A digest is KEY_LEN bytes.
        SecretKey::from_slice(&digest[..]).unwrap_or_else(|_| unreachable!("a key is 32 bytes"))
    }
}

/// Full-width punctuation people put between words (，、。；：).
fn is_cjk_mark(c: char) -> bool {
    matches!(c, '，' | '、' | '。' | '；' | '：' | '\u{3000}')
}

/// The data key (and the secret key) wrapped by `phrase`.
pub fn wrap(
    phrase: &RecoveryKey,
    dek: &SecretKey,
    account_key: Option<&AccountKey>,
) -> Result<Vec<u8>, CryptoError> {
    let mut plain = Zeroizing::new(dek.as_bytes().to_vec());
    if let Some(key) = account_key {
        plain.extend_from_slice(key.as_bytes());
    }
    Ok(seal(&phrase.kek(), &plain, WRAP_AAD)?.to_bytes())
}

/// What [`wrap`] wrapped, if `phrase` is the one.
pub fn unwrap(
    phrase: &RecoveryKey,
    wrapped: &[u8],
) -> Result<(SecretKey, Option<AccountKey>), CryptoError> {
    let plain = open(&phrase.kek(), &Sealed::from_bytes(wrapped)?, WRAP_AAD)?;
    let dek = SecretKey::from_slice(plain.get(..KEY_LEN).ok_or(CryptoError::Malformed)?)?;
    let account_key = match plain.len() - KEY_LEN {
        0 => None,
        ACCOUNT_KEY_LEN => {
            Some(AccountKey::from_slice(&plain[KEY_LEN..]).ok_or(CryptoError::Malformed)?)
        }
        _ => return Err(CryptoError::Malformed),
    };
    Ok((dek, account_key))
}

/// `phrase` sealed under the data key.
pub fn seal_key(dek: &SecretKey, phrase: &RecoveryKey) -> Result<Vec<u8>, CryptoError> {
    Ok(seal(dek, &phrase.0[..], SEAL_AAD)?.to_bytes())
}

pub fn open_key(dek: &SecretKey, sealed: &[u8]) -> Result<RecoveryKey, CryptoError> {
    let plain = open(dek, &Sealed::from_bytes(sealed)?, SEAL_AAD)?;
    RecoveryKey::from_slice(&plain).ok_or(CryptoError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_is_24_chinese_characters_that_parse_back() {
        let key = RecoveryKey::generate().unwrap();
        let words = key.words();
        assert_eq!(words.len(), WORDS);
        assert!(words.iter().all(|w| w.chars().count() == 1));
        assert_eq!(RecoveryKey::parse(&words.join(" ")), Some(key.clone()));
        // Without spaces, or with Chinese punctuation, it reads the same.
        assert_eq!(RecoveryKey::parse(&words.concat()), Some(key.clone()));
        assert_eq!(RecoveryKey::parse(&words.join("，")), Some(key.clone()));
        assert_eq!(
            RecoveryKey::parse(&format!("  {}\n", words.join("  "))),
            Some(key)
        );
    }

    #[test]
    fn typos_short_phrases_and_bad_checksums_are_refused() {
        let key = RecoveryKey::generate().unwrap();
        let words = key.words();
        assert_eq!(RecoveryKey::parse(&words[..23].concat()), None);
        assert_eq!(RecoveryKey::parse(""), None);
        assert_eq!(RecoveryKey::parse("hello world"), None);
        // Swapping two different words almost always breaks the checksum;
        // a character not in the list always does.
        let mut wrong = words.clone();
        wrong[0] = "龘";
        assert_eq!(RecoveryKey::parse(&wrong.concat()), None);
    }

    #[test]
    fn a_known_entropy_gives_the_bip39_words() {
        // BIP39's all-zero 256-bit vector: 23 × first word and a checksum word.
        let key = RecoveryKey::from_slice(&[0; 32]).unwrap();
        let words = key.words();
        assert!(words[..23].iter().all(|w| *w == "的"));
        assert_eq!(RecoveryKey::parse(&words.concat()), Some(key));
    }

    #[test]
    fn wrap_unwrap_with_and_without_the_secret_key() {
        let phrase = RecoveryKey::generate().unwrap();
        let dek = SecretKey::random().unwrap();
        let wrapped = wrap(&phrase, &dek, None).unwrap();
        assert_eq!(wrapped.len(), WRAP_LENS[0]);
        let (back, account) = unwrap(&phrase, &wrapped).unwrap();
        assert_eq!(back.as_bytes(), dek.as_bytes());
        assert!(account.is_none());

        let account_key = AccountKey::generate().unwrap();
        let wrapped = wrap(&phrase, &dek, Some(&account_key)).unwrap();
        assert_eq!(wrapped.len(), WRAP_LENS[1]);
        let (_, account) = unwrap(&phrase, &wrapped).unwrap();
        assert_eq!(account.unwrap().as_bytes(), account_key.as_bytes());

        let other = RecoveryKey::generate().unwrap();
        assert!(unwrap(&other, &wrapped).is_err());
    }

    #[test]
    fn the_phrase_sealed_under_the_data_key_opens_only_with_it() {
        let phrase = RecoveryKey::generate().unwrap();
        let dek = SecretKey::random().unwrap();
        let sealed = seal_key(&dek, &phrase).unwrap();
        assert_eq!(sealed.len(), SEALED_LEN);
        assert_eq!(open_key(&dek, &sealed).unwrap(), phrase);
        assert!(open_key(&SecretKey::random().unwrap(), &sealed).is_err());
        // Neither seal opens as the other.
        let wrapped = wrap(&phrase, &dek, None).unwrap();
        assert!(open_key(&dek, &wrapped).is_err());
    }
}
