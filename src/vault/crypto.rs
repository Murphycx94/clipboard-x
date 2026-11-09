//! The vault's cryptography, pure: Argon2id derives a key-encryption key
//! from the master password; a random data key encrypts the entries with
//! XChaCha20-Poly1305 and is itself stored wrapped by the derived key.

use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 24;
/// Poly1305's tag.
const TAG_LEN: usize = 16;
/// Binds a wrapped data key to its purpose.
const DEK_AAD: &[u8] = b"clipboardx/vault/dek/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The OS gave no randomness.
    Random,
    /// Bad KDF parameters.
    Kdf,
    Encrypt,
    /// Wrong key, wrong associated data, or tampered data.
    Decrypt,
    Malformed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CryptoError::Random => "no system randomness",
            CryptoError::Kdf => "invalid key derivation parameters",
            CryptoError::Encrypt => "encryption failed",
            CryptoError::Decrypt => "decryption failed",
            CryptoError::Malformed => "malformed encrypted data",
        })
    }
}

impl std::error::Error for CryptoError {}

/// Argon2id costs: memory in KiB, iterations, lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes, 1 lane.
    pub const DEFAULT: KdfParams = KdfParams {
        m: 64 * 1024,
        t: 3,
        p: 1,
    };
    /// Argon2's minimum: for tests only.
    pub const INSECURE_TEST: KdfParams = KdfParams { m: 8, t: 1, p: 1 };
}

/// 256 secret bits, wiped when dropped.
pub struct SecretKey(Zeroizing<[u8; KEY_LEN]>);

impl SecretKey {
    pub fn random() -> Result<SecretKey, CryptoError> {
        Ok(SecretKey(Zeroizing::new(random_bytes::<KEY_LEN>()?)))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<SecretKey, CryptoError> {
        let array: [u8; KEY_LEN] = bytes.try_into().map_err(|_| CryptoError::Malformed)?;
        Ok(SecretKey(Zeroizing::new(array)))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        // The length is KEY_LEN by construction.
        XChaCha20Poly1305::new_from_slice(&self.0[..])
            .unwrap_or_else(|_| unreachable!("a key is {KEY_LEN} bytes"))
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

/// `N` bytes from the OS's CSPRNG.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|_| CryptoError::Random)?;
    Ok(bytes)
}

/// The key-encryption key of `password`.
pub fn derive_key(
    password: &str,
    salt: &[u8],
    params: KdfParams,
) -> Result<SecretKey, CryptoError> {
    let costs =
        Params::new(params.m, params.t, params.p, Some(KEY_LEN)).map_err(|_| CryptoError::Kdf)?;
    let mut out = Zeroizing::new([0_u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, costs)
        .hash_password_into(password.as_bytes(), salt, out.as_mut())
        .map_err(|_| CryptoError::Kdf)?;
    Ok(SecretKey(out))
}

/// A nonce and the ciphertext (with its tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl Sealed {
    /// `nonce || ciphertext`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(NONCE_LEN + self.ciphertext.len());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.ciphertext);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Sealed, CryptoError> {
        Sealed::from_parts(
            bytes.get(..NONCE_LEN).unwrap_or_default(),
            bytes.get(NONCE_LEN..).unwrap_or_default(),
        )
    }

    pub fn from_parts(nonce: &[u8], ciphertext: &[u8]) -> Result<Sealed, CryptoError> {
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Malformed)?;
        if ciphertext.len() < TAG_LEN {
            return Err(CryptoError::Malformed);
        }
        Ok(Sealed {
            nonce,
            ciphertext: ciphertext.to_vec(),
        })
    }
}

/// Encrypts `plaintext` bound to `aad`, under a fresh random nonce.
pub fn seal(key: &SecretKey, plaintext: &[u8], aad: &[u8]) -> Result<Sealed, CryptoError> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let ciphertext = key
        .cipher()
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(Sealed { nonce, ciphertext })
}

/// Decrypts what [`seal`] made with the same key and `aad`.
pub fn open(
    key: &SecretKey,
    sealed: &Sealed,
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    key.cipher()
        .decrypt(
            &XNonce::from(sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Decrypt)
}

/// The data key encrypted by the key-encryption key, as stored.
pub fn wrap_key(kek: &SecretKey, dek: &SecretKey) -> Result<Vec<u8>, CryptoError> {
    Ok(seal(kek, dek.as_bytes(), DEK_AAD)?.to_bytes())
}

pub fn unwrap_key(kek: &SecretKey, wrapped: &[u8]) -> Result<SecretKey, CryptoError> {
    let plain = open(kek, &Sealed::from_bytes(wrapped)?, DEK_AAD)?;
    SecretKey::from_slice(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: KdfParams = KdfParams::INSECURE_TEST;

    #[test]
    fn same_password_and_salt_derive_the_same_key() {
        let a = derive_key("correct horse", b"0123456789abcdef", P).unwrap();
        let b = derive_key("correct horse", b"0123456789abcdef", P).unwrap();
        let c = derive_key("correct horsf", b"0123456789abcdef", P).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
        assert_ne!(a.as_bytes(), c.as_bytes());
    }

    #[test]
    fn sealed_data_opens_with_the_key_and_aad() {
        let key = SecretKey::random().unwrap();
        let sealed = seal(&key, b"secret", b"id-1").unwrap();
        assert_eq!(&*open(&key, &sealed, b"id-1").unwrap(), b"secret");
    }

    #[test]
    fn tampering_wrong_key_or_wrong_aad_fail() {
        let key = SecretKey::random().unwrap();
        let sealed = seal(&key, b"secret", b"id-1").unwrap();
        assert_eq!(
            open(&key, &sealed, b"id-2").unwrap_err(),
            CryptoError::Decrypt
        );
        let other = SecretKey::random().unwrap();
        assert_eq!(
            open(&other, &sealed, b"id-1").unwrap_err(),
            CryptoError::Decrypt
        );
        let mut bytes = sealed.to_bytes();
        for at in [0, NONCE_LEN, bytes.len() - 1] {
            bytes[at] ^= 1;
            let broken = Sealed::from_bytes(&bytes).unwrap();
            assert_eq!(
                open(&key, &broken, b"id-1").unwrap_err(),
                CryptoError::Decrypt
            );
            bytes[at] ^= 1;
        }
    }

    #[test]
    fn nonces_are_fresh() {
        let key = SecretKey::random().unwrap();
        let a = seal(&key, b"x", b"").unwrap();
        let b = seal(&key, b"x", b"").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn keys_wrap_and_rewrap() {
        let dek = SecretKey::random().unwrap();
        let kek1 = derive_key("one", b"salt-salt-salt-1", P).unwrap();
        let kek2 = derive_key("two", b"salt-salt-salt-2", P).unwrap();
        let wrapped = wrap_key(&kek1, &dek).unwrap();
        let back = unwrap_key(&kek1, &wrapped).unwrap();
        assert_eq!(back.as_bytes(), dek.as_bytes());
        assert_eq!(
            unwrap_key(&kek2, &wrapped).unwrap_err(),
            CryptoError::Decrypt
        );
        let rewrapped = wrap_key(&kek2, &back).unwrap();
        assert_eq!(
            unwrap_key(&kek2, &rewrapped).unwrap().as_bytes(),
            dek.as_bytes()
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(
            Sealed::from_bytes(&[0; 10]).unwrap_err(),
            CryptoError::Malformed
        );
        assert_eq!(
            SecretKey::from_slice(&[0; 31]).unwrap_err(),
            CryptoError::Malformed
        );
    }

    #[test]
    fn debug_never_shows_key_bytes() {
        let key = SecretKey::from_slice(&[7; KEY_LEN]).unwrap();
        assert_eq!(format!("{key:?}"), "SecretKey(..)");
    }
}
