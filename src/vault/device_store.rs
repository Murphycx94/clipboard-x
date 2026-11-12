//! A key store of files, each sealed by a key the system keeps (Android's
//! Keystore): the secret key and the device token under a key read
//! silently, the data key under one that only the user's biometric
//! unlocks. The system's part is a [`Sealer`], so the files and the errors
//! are tested here with a fake one.

use std::fs;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::keychain::{KeyItem, KeyStore};

/// Which of the system's keys seals an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemKey {
    /// Used without asking the user.
    Silent,
    /// Used once per confirmation of the user's biometric.
    Biometric,
}

impl SystemKey {
    fn of(item: KeyItem) -> SystemKey {
        match item {
            KeyItem::DataKey => SystemKey::Biometric,
            KeyItem::SecretKey | KeyItem::DeviceToken => SystemKey::Silent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// The user closed the prompt.
    Canceled,
    /// The key is gone: the enrolled fingerprints or faces changed.
    Invalidated,
    Failed(String),
}

/// The system's keys: they seal and open bytes and never leave it.
pub trait Sealer: Send + Sync {
    /// Whether a strong biometric is enrolled.
    fn biometric_available(&self) -> bool;
    /// `plain` sealed by `key`; for [`SystemKey::Biometric`] after the user
    /// confirmed (`prompt` shown). Blocking.
    fn seal(&self, key: SystemKey, plain: &[u8], prompt: &str) -> Result<Vec<u8>, SealError>;
    /// What [`Sealer::seal`] sealed. Blocking.
    fn open(
        &self,
        key: SystemKey,
        sealed: &[u8],
        prompt: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SealError>;
    fn delete_key(&self, key: SystemKey);
}

/// Shown when turning biometric unlocking on: sealing the data key needs
/// the user's biometric too.
const ENABLE_PROMPT: &str = "开启指纹/面容解锁";
const CANCELED: &str = "已取消验证";
/// Toasts: short enough for a phone's width.
const INVALIDATED: &str = "指纹或面容有变化，已关闭指纹/面容解锁";
const NOT_ENABLED: &str = "指纹/面容解锁已失效，已关闭";

/// Items in `dir`, one file each.
pub struct DeviceStore<S: Sealer> {
    dir: PathBuf,
    sealer: S,
}

impl<S: Sealer> DeviceStore<S> {
    pub fn new(dir: impl Into<PathBuf>, sealer: S) -> DeviceStore<S> {
        DeviceStore {
            dir: dir.into(),
            sealer,
        }
    }

    fn path(&self, item: KeyItem) -> PathBuf {
        self.dir.join(format!("{}.sealed", item.account()))
    }

    fn remove(&self, item: KeyItem) {
        let path = self.path(item);
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot delete {}: {e}", path.display());
        }
    }

    /// Opens `item`'s file; a key that is gone takes the file with it.
    fn open(&self, item: KeyItem, prompt: &str) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let sealed = match fs::read(self.path(item)) {
            Ok(sealed) => sealed,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SealError::Failed(NOT_ENABLED.into()));
            }
            Err(e) => return Err(SealError::Failed(format!("无法读取密钥：{e}"))),
        };
        let opened = self.sealer.open(SystemKey::of(item), &sealed, prompt);
        if opened == Err(SealError::Invalidated) {
            self.remove(item);
        }
        opened
    }
}

fn message(error: SealError) -> String {
    match error {
        SealError::Canceled => CANCELED.into(),
        SealError::Invalidated => INVALIDATED.into(),
        SealError::Failed(message) => message,
    }
}

/// Writes `bytes` next to `path` and moves them over it: a crash leaves the
/// old file or the new one.
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

impl<S: Sealer> KeyStore for DeviceStore<S> {
    fn available(&self) -> bool {
        self.sealer.biometric_available()
    }

    fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String> {
        let sealed = self
            .sealer
            .seal(SystemKey::of(item), bytes, ENABLE_PROMPT)
            .map_err(message)?;
        fs::create_dir_all(&self.dir)
            .and_then(|()| write_replacing(&self.path(item), &sealed))
            .map_err(|e| format!("无法保存密钥：{e}"))
    }

    /// The silent items; the data key needs [`KeyStore::get_with_auth`].
    fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>> {
        if SystemKey::of(item) == SystemKey::Biometric || !self.path(item).exists() {
            return None;
        }
        self.open(item, "")
            .inspect_err(|e| log::error!("cannot open {item:?}: {e:?}"))
            .ok()
    }

    fn get_with_auth(&self, item: KeyItem, reason: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        self.open(item, reason).map_err(message)
    }

    /// Whether its file is there: the data key cannot be read to check.
    fn contains(&self, item: KeyItem) -> bool {
        self.path(item).exists()
    }

    fn delete(&self, item: KeyItem) {
        self.remove(item);
        if SystemKey::of(item) == SystemKey::Biometric {
            self.sealer.delete_key(SystemKey::Biometric);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// "Seals" by prefixing the key's name; a key can be canceled or gone.
    #[derive(Default)]
    struct FakeSealer {
        canceled: bool,
        invalidated: Mutex<bool>,
        prompts: Mutex<Vec<String>>,
        deleted: Mutex<Vec<SystemKey>>,
    }

    fn tag(key: SystemKey) -> &'static [u8] {
        match key {
            SystemKey::Silent => b"silent:",
            SystemKey::Biometric => b"bio:",
        }
    }

    impl Sealer for FakeSealer {
        fn biometric_available(&self) -> bool {
            true
        }

        fn seal(&self, key: SystemKey, plain: &[u8], prompt: &str) -> Result<Vec<u8>, SealError> {
            if key == SystemKey::Biometric {
                self.prompts.lock().unwrap().push(prompt.into());
            }
            Ok([tag(key), plain].concat())
        }

        fn open(
            &self,
            key: SystemKey,
            sealed: &[u8],
            prompt: &str,
        ) -> Result<Zeroizing<Vec<u8>>, SealError> {
            if key == SystemKey::Biometric {
                self.prompts.lock().unwrap().push(prompt.into());
                if self.canceled {
                    return Err(SealError::Canceled);
                }
                if *self.invalidated.lock().unwrap() {
                    return Err(SealError::Invalidated);
                }
            }
            sealed
                .strip_prefix(tag(key))
                .map(|plain| Zeroizing::new(plain.to_vec()))
                .ok_or_else(|| SealError::Failed("wrong key".into()))
        }

        fn delete_key(&self, key: SystemKey) {
            self.deleted.lock().unwrap().push(key);
        }
    }

    fn store(sealer: FakeSealer) -> (tempfile::TempDir, DeviceStore<FakeSealer>) {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::new(dir.path().join("keys"), sealer);
        (dir, store)
    }

    #[test]
    fn silent_items_are_sealed_on_disk_and_read_without_asking() {
        let (_dir, store) = store(FakeSealer::default());
        store.put(KeyItem::SecretKey, b"sk").unwrap();
        store.put(KeyItem::DeviceToken, b"token").unwrap();

        let on_disk = fs::read(store.path(KeyItem::SecretKey)).unwrap();
        assert_eq!(on_disk, b"silent:sk");
        assert_eq!(store.get(KeyItem::SecretKey).unwrap().as_slice(), b"sk");
        assert_eq!(
            store.get(KeyItem::DeviceToken).unwrap().as_slice(),
            b"token"
        );
        assert!(store.sealer.prompts.lock().unwrap().is_empty());
    }

    #[test]
    fn the_data_key_needs_the_biometric_each_time() {
        let (_dir, store) = store(FakeSealer::default());
        store.put(KeyItem::DataKey, b"dk").unwrap();

        assert!(store.get(KeyItem::DataKey).is_none());
        assert!(store.contains(KeyItem::DataKey));
        let key = store.get_with_auth(KeyItem::DataKey, "解锁").unwrap();
        assert_eq!(key.as_slice(), b"dk");
        assert_eq!(
            *store.sealer.prompts.lock().unwrap(),
            [ENABLE_PROMPT.to_owned(), "解锁".to_owned()]
        );
    }

    #[test]
    fn a_missing_item_is_none_or_says_to_use_the_password() {
        let (_dir, store) = store(FakeSealer::default());
        assert!(store.get(KeyItem::SecretKey).is_none());
        assert_eq!(
            store.get_with_auth(KeyItem::DataKey, "r").unwrap_err(),
            NOT_ENABLED
        );
    }

    #[test]
    fn canceling_the_prompt_keeps_the_key() {
        let (_dir, store) = store(FakeSealer {
            canceled: true,
            ..FakeSealer::default()
        });
        store.put(KeyItem::DataKey, b"dk").unwrap();

        assert_eq!(
            store.get_with_auth(KeyItem::DataKey, "r").unwrap_err(),
            CANCELED
        );
        assert!(store.path(KeyItem::DataKey).exists());
    }

    #[test]
    fn an_invalidated_key_takes_its_file_and_says_to_turn_it_on_again() {
        let (_dir, store) = store(FakeSealer::default());
        store.put(KeyItem::DataKey, b"dk").unwrap();
        *store.sealer.invalidated.lock().unwrap() = true;

        assert_eq!(
            store.get_with_auth(KeyItem::DataKey, "r").unwrap_err(),
            INVALIDATED
        );
        assert!(!store.contains(KeyItem::DataKey));
        assert_eq!(
            store.get_with_auth(KeyItem::DataKey, "r").unwrap_err(),
            NOT_ENABLED
        );
    }

    #[test]
    fn deleting_the_data_key_deletes_the_biometric_key_too() {
        let (_dir, store) = store(FakeSealer::default());
        store.put(KeyItem::DataKey, b"dk").unwrap();
        store.put(KeyItem::SecretKey, b"sk").unwrap();

        store.delete(KeyItem::SecretKey);
        assert!(store.sealer.deleted.lock().unwrap().is_empty());
        store.delete(KeyItem::DataKey);
        assert!(!store.path(KeyItem::DataKey).exists());
        assert_eq!(
            *store.sealer.deleted.lock().unwrap(),
            [SystemKey::Biometric]
        );
        // Deleting what is not there is quiet.
        store.delete(KeyItem::DataKey);
    }

    #[test]
    fn a_new_copy_replaces_the_old() {
        let (_dir, store) = store(FakeSealer::default());
        store.put(KeyItem::SecretKey, b"old").unwrap();
        store.put(KeyItem::SecretKey, b"new").unwrap();
        assert_eq!(store.get(KeyItem::SecretKey).unwrap().as_slice(), b"new");
        assert!(
            !store
                .path(KeyItem::SecretKey)
                .with_extension("partial")
                .exists()
        );
    }
}
