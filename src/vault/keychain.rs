//! What the vault keeps on this device only: the Touch ID copy of the data
//! key (read after the system asks the user), the secret key and the sync
//! device token (read silently, as 1Password keeps its secret key).

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

/// An item of the key store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyItem {
    /// The data key, for Touch ID unlocking.
    DataKey,
    /// The secret key, the second factor of the key derivation.
    SecretKey,
    /// The sync server's device token.
    DeviceToken,
}

impl KeyItem {
    /// The item's name in the system's store.
    pub(crate) fn account(self) -> &'static str {
        match self {
            KeyItem::DataKey => "data-key",
            KeyItem::SecretKey => "secret-key",
            KeyItem::DeviceToken => "device-token",
        }
    }
}

pub trait KeyStore: Send + Sync {
    /// Whether the system can confirm the user (Touch ID or the login
    /// password) before [`KeyStore::get_with_auth`].
    fn available(&self) -> bool;
    fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String>;
    /// The item, without asking the user.
    fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>>;
    /// The item after the system confirmed the user (`reason` shown). Blocking.
    fn get_with_auth(&self, item: KeyItem, reason: &str) -> Result<Zeroizing<Vec<u8>>, String>;
    fn delete(&self, item: KeyItem);
    /// Whether the item is stored (without asking the user).
    fn contains(&self, item: KeyItem) -> bool {
        self.get(item).is_some()
    }
}

/// In memory, for the process's life: tests, and platforms without a
/// keychain of ours yet. `auth` says whether it plays Touch ID (always
/// approving).
pub struct MemoryKeyStore {
    items: Mutex<HashMap<KeyItem, Zeroizing<Vec<u8>>>>,
    auth: bool,
}

impl MemoryKeyStore {
    /// A store that approves every confirmation (tests of Touch ID).
    pub fn with_auth() -> MemoryKeyStore {
        MemoryKeyStore {
            items: Mutex::new(HashMap::new()),
            auth: true,
        }
    }

    /// A store without Touch ID.
    pub fn without_auth() -> MemoryKeyStore {
        MemoryKeyStore {
            auth: false,
            ..MemoryKeyStore::with_auth()
        }
    }
}

impl Default for MemoryKeyStore {
    fn default() -> Self {
        MemoryKeyStore::with_auth()
    }
}

impl KeyStore for MemoryKeyStore {
    fn available(&self) -> bool {
        self.auth
    }

    fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String> {
        self.items
            .lock()
            .map_err(|e| e.to_string())?
            .insert(item, Zeroizing::new(bytes.to_vec()));
        Ok(())
    }

    fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>> {
        self.items.lock().ok()?.get(&item).cloned()
    }

    fn get_with_auth(&self, item: KeyItem, _reason: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        if !self.auth {
            return Err("此设备不支持 Touch ID".into());
        }
        self.get(item).ok_or_else(|| "没有保存的密钥".to_owned())
    }

    fn delete(&self, item: KeyItem) {
        if let Ok(mut items) = self.items.lock() {
            items.remove(&item);
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};
    use zeroize::Zeroizing;

    use super::{KeyItem, KeyStore};

    const SERVICE: &str = "com.clipboardx.app.vault";
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
    /// errSecItemNotFound: deleting what is not there.
    const NOT_FOUND: i32 = -25300;

    /// The login keychain, items readable by this app only.
    pub struct MacKeyStore;

    fn context() -> Retained<LAContext> {
        // SAFETY: plain allocation and init of LAContext.
        unsafe { LAContext::new() }
    }

    /// Asks the system to confirm the user; blocking.
    fn confirm_user(reason: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<bool>();
        let reply = RcBlock::new(move |ok: Bool, _error: *mut NSError| {
            let _ = tx.send(ok.as_bool());
        });
        let ctx = context();
        // SAFETY: the block is 'static and only sends on a channel (sendable).
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(
                LAPolicy::DeviceOwnerAuthentication,
                &NSString::from_str(reason),
                &reply,
            );
        }
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err("已取消验证".into()),
        }
    }

    impl KeyStore for MacKeyStore {
        fn available(&self) -> bool {
            // SAFETY: a fresh context; the error out-parameter is optional.
            unsafe {
                context()
                    .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication)
                    .is_ok()
            }
        }

        fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String> {
            security_framework::passwords::set_generic_password(SERVICE, item.account(), bytes)
                .map_err(|e| format!("无法写入钥匙串：{e}"))
        }

        fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>> {
            security_framework::passwords::get_generic_password(SERVICE, item.account())
                .ok()
                .map(Zeroizing::new)
        }

        fn get_with_auth(&self, item: KeyItem, reason: &str) -> Result<Zeroizing<Vec<u8>>, String> {
            confirm_user(reason)?;
            self.get(item).ok_or_else(|| "钥匙串中没有密钥".to_owned())
        }

        fn delete(&self, item: KeyItem) {
            if let Err(e) =
                security_framework::passwords::delete_generic_password(SERVICE, item.account())
                && e.code() != NOT_FOUND
            {
                log::warn!("cannot delete keychain item {item:?}: {e}");
            }
        }
    }
}

/// Set to anything to keep away from the system keychain (tests): items
/// then live in memory, without Touch ID.
pub const NO_KEYCHAIN_ENV: &str = "CLIPBOARDX_NO_KEYCHAIN";

/// The platform's store; Android's keeps its sealed files in `data_dir`.
pub fn platform(data_dir: &std::path::Path) -> Box<dyn KeyStore> {
    if std::env::var_os(NO_KEYCHAIN_ENV).is_some() {
        return Box::new(MemoryKeyStore::without_auth());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = data_dir;
        Box::new(mac::MacKeyStore)
    }
    #[cfg(target_os = "android")]
    {
        Box::new(super::device_store::DeviceStore::new(
            data_dir.join("keys"),
            crate::android::KeystoreSealer,
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        let _ = data_dir;
        Box::new(MemoryKeyStore::without_auth())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_keeps_items_apart() {
        let store = MemoryKeyStore::with_auth();
        store.put(KeyItem::SecretKey, b"sk").unwrap();
        store.put(KeyItem::DataKey, b"dk").unwrap();
        assert_eq!(store.get(KeyItem::SecretKey).unwrap().as_slice(), b"sk");
        assert_eq!(
            store
                .get_with_auth(KeyItem::DataKey, "r")
                .unwrap()
                .as_slice(),
            b"dk"
        );
        store.delete(KeyItem::DataKey);
        assert!(store.get(KeyItem::DataKey).is_none());
        assert!(store.get(KeyItem::DeviceToken).is_none());
    }

    #[test]
    fn without_auth_refuses_confirmation() {
        let store = MemoryKeyStore::without_auth();
        store.put(KeyItem::DataKey, b"dk").unwrap();
        assert!(!store.available());
        assert!(store.get_with_auth(KeyItem::DataKey, "r").is_err());
    }
}
