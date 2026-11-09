//! Where the Touch ID copy of the data key lives.

use std::sync::Mutex;

use super::crypto::SecretKey;

pub trait KeyStore: Send + Sync {
    /// Whether this machine can unlock with Touch ID or the system password.
    fn available(&self) -> bool;
    fn save(&self, key: &SecretKey) -> Result<(), String>;
    /// Asks the user (`reason` shown) and reads the key. Blocking.
    fn load(&self, reason: &str) -> Result<SecretKey, String>;
    /// Whether a key is stored, without asking the user.
    fn has_key(&self) -> bool;
    fn delete(&self);
}

/// No Touch ID (other platforms).
pub struct NoKeyStore;

impl KeyStore for NoKeyStore {
    fn available(&self) -> bool {
        false
    }
    fn save(&self, _: &SecretKey) -> Result<(), String> {
        Err("此平台不支持 Touch ID".into())
    }
    fn load(&self, _: &str) -> Result<SecretKey, String> {
        Err("此平台不支持 Touch ID".into())
    }
    fn has_key(&self) -> bool {
        false
    }
    fn delete(&self) {}
}

/// In memory, always approving (tests).
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<Option<[u8; super::crypto::KEY_LEN]>>);

impl KeyStore for MemoryKeyStore {
    fn available(&self) -> bool {
        true
    }
    fn save(&self, key: &SecretKey) -> Result<(), String> {
        *self.0.lock().map_err(|e| e.to_string())? = Some(*key.as_bytes());
        Ok(())
    }
    fn load(&self, _: &str) -> Result<SecretKey, String> {
        let stored = *self.0.lock().map_err(|e| e.to_string())?;
        stored
            .ok_or_else(|| "没有保存的密钥".to_owned())
            .and_then(|b| SecretKey::from_slice(&b).map_err(|e| e.to_string()))
    }
    fn has_key(&self) -> bool {
        self.0.lock().is_ok_and(|stored| stored.is_some())
    }
    fn delete(&self) {
        if let Ok(mut stored) = self.0.lock() {
            *stored = None;
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

    use super::super::crypto::SecretKey;
    use super::KeyStore;

    const SERVICE: &str = "com.clipboardx.app.vault";
    const ACCOUNT: &str = "data-key";
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

    pub struct MacKeyStore;

    fn context() -> Retained<LAContext> {
        // SAFETY: plain allocation and init of LAContext.
        unsafe { LAContext::new() }
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

        fn save(&self, key: &SecretKey) -> Result<(), String> {
            security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key.as_bytes())
                .map_err(|e| format!("无法写入钥匙串：{e}"))
        }

        fn load(&self, reason: &str) -> Result<SecretKey, String> {
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
                Ok(true) => {}
                Ok(false) | Err(_) => return Err("已取消验证".into()),
            }
            let bytes = zeroize::Zeroizing::new(
                security_framework::passwords::get_generic_password(SERVICE, ACCOUNT)
                    .map_err(|_| "钥匙串中没有密钥".to_owned())?,
            );
            SecretKey::from_slice(&bytes).map_err(|e| e.to_string())
        }

        fn has_key(&self) -> bool {
            security_framework::passwords::get_generic_password(SERVICE, ACCOUNT)
                .map(zeroize::Zeroizing::new)
                .is_ok()
        }

        fn delete(&self) {
            // Not found is the usual case; anything else is worth a line.
            const NOT_FOUND: i32 = -25300; // errSecItemNotFound
            if let Err(e) = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT)
                && e.code() != NOT_FOUND
            {
                log::warn!("cannot delete the Touch ID key: {e}");
            }
        }
    }
}

/// Set to anything to keep away from the system keychain (tests): no
/// Touch ID then.
pub const NO_KEYCHAIN_ENV: &str = "CLIPBOARDX_NO_KEYCHAIN";

/// The platform's store.
pub fn platform() -> Box<dyn KeyStore> {
    if std::env::var_os(NO_KEYCHAIN_ENV).is_some() {
        return Box::new(NoKeyStore);
    }
    #[cfg(target_os = "macos")]
    return Box::new(mac::MacKeyStore);
    #[cfg(not(target_os = "macos"))]
    Box::new(NoKeyStore)
}
