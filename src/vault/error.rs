//! What vault operations fail with; messages are shown to the user and never
//! contain vault data.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    NotSetUp,
    AlreadySetUp,
    Locked,
    /// The vault locked while it was being unlocked.
    Interrupted,
    WeakPassword,
    WrongPassword,
    TooManyAttempts {
        wait_ms: u64,
    },
    Corrupt,
    NotFound,
    /// The vault needs the secret key, which this device does not have.
    SecretKeyNeeded,
    InvalidSecretKey,
    /// Not 24 words of the list, or a bad checksum.
    InvalidRecoveryPhrase,
    /// A valid phrase, but not this vault's.
    WrongRecoveryPhrase,
    /// It opens this device's copy, not the newer one another device made.
    StaleRecoveryPhrase,
    /// The sync server's data failed verification (rolled back, withheld or
    /// altered): nothing was applied.
    Tampered,
    /// The master password changed on another device.
    HeaderChanged,
    Sync(String),
    InvalidTotp(String),
    InvalidBackup(String),
    Keychain(String),
    Storage(String),
    Crypto,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotSetUp => f.write_str("密码本尚未设置"),
            VaultError::AlreadySetUp => f.write_str("密码本已经设置过"),
            VaultError::Locked => f.write_str("密码本已锁定"),
            VaultError::Interrupted => f.write_str("密码本在解锁过程中被锁定，请重新解锁"),
            VaultError::WeakPassword => {
                write!(f, "主密码至少需要 {} 个字符", super::MIN_PASSWORD_CHARS)
            }
            VaultError::WrongPassword => f.write_str("密码错误"),
            VaultError::TooManyAttempts { wait_ms } => {
                write!(f, "尝试次数过多，请 {} 秒后再试", wait_ms.div_ceil(1000))
            }
            VaultError::Corrupt => f.write_str("该条目已损坏，无法解密"),
            VaultError::SecretKeyNeeded => f.write_str("此设备需要输入秘密密钥"),
            VaultError::InvalidSecretKey => f.write_str("秘密密钥格式不正确，请检查后重新输入"),
            VaultError::InvalidRecoveryPhrase => {
                f.write_str("助记词无效：请检查是否为 24 个字、顺序是否正确、有无错别字")
            }
            VaultError::WrongRecoveryPhrase => f.write_str("助记词与此密码本不匹配"),
            VaultError::StaleRecoveryPhrase => {
                f.write_str("其他设备已更新了主密码或助记词，请输入最新的助记词")
            }
            VaultError::Tampered => f.write_str("同步数据校验失败，已拒绝本次同步，本机数据未改动"),
            VaultError::HeaderChanged => {
                f.write_str("其他设备修改了主密码或助记词，请用当前的主密码解锁")
            }
            VaultError::Sync(why) => write!(f, "同步失败：{why}"),
            VaultError::NotFound => f.write_str("条目不存在"),
            VaultError::InvalidTotp(why)
            | VaultError::InvalidBackup(why)
            | VaultError::Keychain(why) => f.write_str(why),
            VaultError::Storage(_) => f.write_str("读写密码本失败"),
            VaultError::Crypto => f.write_str("加密操作失败"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<rusqlite::Error> for VaultError {
    fn from(e: rusqlite::Error) -> Self {
        log::error!("vault storage: {e}");
        VaultError::Storage(e.to_string())
    }
}

impl From<super::crypto::CryptoError> for VaultError {
    fn from(e: super::crypto::CryptoError) -> Self {
        log::error!("vault crypto: {e}");
        VaultError::Crypto
    }
}
