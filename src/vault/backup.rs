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
const MAX_KDF_MEMORY_KIB: u32 = 256 * 1024;
const MAX_KDF_PASSES: u32 = 6;
const MAX_KDF_LANES: u32 = 4;

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
    nonce: String,
    ciphertext: String,
}

/// One entry in a backup.
#[derive(Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: String,
    pub updated_at: String,
    pub entry: VaultEntry,
}

pub fn encode(
    entries: &[BackupEntry],
    password: &str,
    params: KdfParams,
) -> Result<String, VaultError> {
    let salt = random_bytes::<SALT_LEN>()?;
    let key = derive_key(password, &salt, params)?;
    let json = Zeroizing::new(serde_json::to_vec(entries).map_err(|_| VaultError::Crypto)?);
    let sealed = seal(&key, &json, AAD)?;
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
        nonce: B64.encode(sealed.nonce),
        ciphertext: B64.encode(&sealed.ciphertext),
    };
    serde_json::to_string_pretty(&file).map_err(|_| VaultError::Crypto)
}

pub fn decode(text: &str, password: &str) -> Result<Vec<BackupEntry>, VaultError> {
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
    let in_range = (8..=MAX_KDF_MEMORY_KIB).contains(&params.m)
        && (1..=MAX_KDF_PASSES).contains(&params.t)
        && (1..=MAX_KDF_LANES).contains(&params.p);
    if !in_range {
        return Err(invalid("备份文件参数异常"));
    }
    let key = derive_key(password, &b64(&file.kdf.salt)?, params)
        .map_err(|_| invalid("备份文件已损坏"))?;
    let sealed = Sealed::from_parts(&b64(&file.nonce)?, &b64(&file.ciphertext)?)
        .map_err(|_| invalid("备份文件已损坏"))?;
    let plain = open(&key, &sealed, AAD).map_err(|_| VaultError::WrongPassword)?;
    serde_json::from_slice(&plain).map_err(|_| invalid("备份文件已损坏"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_expensive_parameters() {
        let file = encode(&[], "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        let hostile = file.replace("\"m\": 8", "\"m\": 99999999");
        assert_ne!(file, hostile);
        assert_eq!(
            decode(&hostile, "backup pass phrase").err(),
            Some(VaultError::InvalidBackup("备份文件参数异常".into()))
        );
    }

    #[test]
    fn rejects_other_versions_and_formats() {
        let file = encode(&[], "backup pass phrase", KdfParams::INSECURE_TEST).unwrap();
        let v2 = file.replace("\"version\": 1", "\"version\": 2");
        assert_eq!(
            decode(&v2, "backup pass phrase").err(),
            Some(VaultError::InvalidBackup("不支持的备份版本".into()))
        );
        let other = file.replace("clipboardx-vault", "something-else");
        assert!(decode(&other, "backup pass phrase").is_err());
    }
}
