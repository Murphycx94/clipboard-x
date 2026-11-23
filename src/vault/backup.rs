//! `.cxvault` files: every entry, encrypted with a key derived from a backup
//! password and a fresh salt.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::crypto::{KdfParams, SALT_LEN, Sealed, derive_key, open, random_bytes, seal};
use super::entry::VaultEntry;
use super::error::VaultError;

const FORMAT: &str = "clipboardx-vault";
const VERSION: u32 = 1;
const AAD: &[u8] = b"clipboardx/vault/backup/v1";

#[derive(Serialize, Deserialize)]
struct Kdf {
    alg: String,
    m: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    kdf: Kdf,
    /// Entries the vault could not read when this was made, so not in it.
    #[serde(default, skip_serializing_if = "is_zero")]
    unreadable: usize,
    nonce: String,
    ciphertext: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What the ciphertext is bound to: the format and, for a backup that
/// lacks entries, how many (so the file cannot be edited to hide it).
fn aad(unreadable: usize) -> Vec<u8> {
    match unreadable {
        0 => AAD.to_vec(),
        n => [AAD, format!("/unreadable:{n}").as_bytes()].concat(),
    }
}

/// What a backup holds.
pub struct Decoded {
    pub entries: Vec<BackupEntry>,
    /// Entries the vault it was made from could not read: not in it.
    pub unreadable: usize,
}

/// One entry in a backup.
#[derive(Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: String,
    pub updated_at: String,
    pub entry: VaultEntry,
}

/// A backup of `entries`, which lacks `unreadable` more.
pub fn encode(
    entries: &[BackupEntry],
    unreadable: usize,
    password: &str,
    params: KdfParams,
) -> Result<String, VaultError> {
    let salt = random_bytes::<SALT_LEN>()?;
    let key = derive_key(password, &salt, params)?;
    let json = Zeroizing::new(serde_json::to_vec(entries).map_err(|_| VaultError::Crypto)?);
    let sealed = seal(&key, &json, &aad(unreadable))?;
    let file = File {
        format: FORMAT.into(),
        version: VERSION,
        kdf: Kdf {
            alg: "argon2id".into(),
            m: params.m,
            t: params.t,
            p: params.p,
            salt: B64.encode(salt),
        },
        unreadable,
        nonce: B64.encode(sealed.nonce),
        ciphertext: B64.encode(&sealed.ciphertext),
    };
    serde_json::to_string_pretty(&file).map_err(|_| VaultError::Crypto)
}

pub fn decode(text: &str, password: &str) -> Result<Decoded, VaultError> {
    let invalid = |why: &str| VaultError::InvalidBackup(why.to_owned());
    let file: File =
        serde_json::from_str(text).map_err(|_| invalid("不是有效的 ClipboardX 备份文件"))?;
    if file.format != FORMAT || file.kdf.alg != "argon2id" {
        return Err(invalid("不是有效的 ClipboardX 备份文件"));
    }
    if file.version != VERSION {
        return Err(invalid("不支持的备份版本"));
    }
    let b64 = |s: &str| B64.decode(s).map_err(|_| invalid("备份文件已损坏"));
    let params = KdfParams {
        m: file.kdf.m,
        t: file.kdf.t,
        p: file.kdf.p,
    };
    // A hostile file could ask for gigabytes of memory or minutes of work.
    if !params.is_affordable() {
        return Err(invalid("备份文件参数异常"));
    }
    let key = derive_key(password, &b64(&file.kdf.salt)?, params)
        .map_err(|_| invalid("备份文件已损坏"))?;
    let sealed = Sealed::from_parts(&b64(&file.nonce)?, &b64(&file.ciphertext)?)
        .map_err(|_| invalid("备份文件已损坏"))?;
    let plain =
        open(&key, &sealed, &aad(file.unreadable)).map_err(|_| VaultError::WrongPassword)?;
    Ok(Decoded {
        entries: serde_json::from_slice(&plain).map_err(|_| invalid("备份文件已损坏"))?,
        unreadable: file.unreadable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_says_how_many_entries_it_lacks_and_cannot_be_made_to_hide_it() {
        let whole = encode(&[], 0, "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        assert_eq!(decode(&whole, "backup pass phrase").unwrap().unreadable, 0);
        assert!(!whole.contains("unreadable"));

        let partial = encode(&[], 2, "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        assert_eq!(
            decode(&partial, "backup pass phrase").unwrap().unreadable,
            2
        );
        // The count is part of what the key seals: changed or removed, the
        // file no longer opens.
        let hidden = partial.replace("\"unreadable\": 2", "\"unreadable\": 0");
        assert_ne!(hidden, partial);
        assert_eq!(
            decode(&hidden, "backup pass phrase").err(),
            Some(VaultError::WrongPassword)
        );
    }

    #[test]
    fn rejects_expensive_parameters() {
        let file = encode(&[], 0, "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        let hostile = file.replace("\"m\": 8", "\"m\": 99999999");
        assert_ne!(file, hostile);
        assert_eq!(
            decode(&hostile, "backup pass phrase").err(),
            Some(VaultError::InvalidBackup("备份文件参数异常".into()))
        );
    }

    #[test]
    fn rejects_other_versions_and_formats() {
        let file = encode(&[], 0, "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        let v2 = file.replace("\"version\": 1", "\"version\": 2");
        assert_eq!(
            decode(&v2, "backup pass phrase").err(),
            Some(VaultError::InvalidBackup("不支持的备份版本".into()))
        );
        let other = file.replace("clipboardx-vault", "something-else");
        assert!(decode(&other, "backup pass phrase").is_err());
    }
}
