//! The macOS keychain's items in one: every [`KeyItem`] in a single entry
//! named after the app's version (`keys-<version>`), so that a new version
//! asks the user once, not once per item, and never again after.
//!
//! An app signed ad hoc, as ClipboardX is, is a new program to the keychain
//! at every version: an entry another version wrote asks for the login
//! password each time it is read. An entry this version writes it reads
//! silently. So the first read of a version copies what the newest older
//! entry holds (or, from before this store, the three entries of one item
//! each) into an entry of its own, and reads only that one after.
//!
//! Older entries stay: the keychain refuses to let a version delete or
//! overwrite another's (overwriting asks the user). Ours is kept even when
//! empty, else the older entry would come back with what was removed since.
//!
//! The keychain itself is a [`Backend`], faked in the tests.

use std::sync::Mutex;

use zeroize::Zeroizing;

use super::keychain::KeyItem;

/// What the store needs from the keychain: entries by name, one service.
pub trait Backend: Send + Sync {
    /// The names of the entries (reading names asks nobody).
    fn names(&self) -> Result<Vec<String>, String>;
    /// An entry's bytes; `None` when it is not there. Another version's
    /// entry may ask the user, who may refuse (an error).
    fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    /// Creates or replaces this version's entry.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Removes an entry; another version's is refused, quietly.
    fn remove(&self, name: &str) -> Result<(), String>;
}

const PREFIX: &str = "keys-";
const FORMAT: u8 = 1;
/// The entries from before this store, one item each.
const SEPARATE: [KeyItem; 3] = [KeyItem::DataKey, KeyItem::SecretKey, KeyItem::DeviceToken];

fn tag(item: KeyItem) -> u8 {
    match item {
        KeyItem::DataKey => 1,
        KeyItem::SecretKey => 2,
        KeyItem::DeviceToken => 3,
    }
}

fn item_of(tag: u8) -> Option<KeyItem> {
    SEPARATE.into_iter().find(|&item| self::tag(item) == tag)
}

/// The items of one entry.
#[derive(Default)]
struct Bundle(Vec<(KeyItem, Zeroizing<Vec<u8>>)>);

impl Bundle {
    fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>> {
        self.0
            .iter()
            .find(|(i, _)| *i == item)
            .map(|(_, b)| b.clone())
    }

    fn set(&mut self, item: KeyItem, bytes: &[u8]) {
        self.remove(item);
        self.0.push((item, Zeroizing::new(bytes.to_vec())));
    }

    fn remove(&mut self, item: KeyItem) {
        self.0.retain(|(i, _)| *i != item);
    }

    /// `FORMAT`, then each item: its tag, its length (2 bytes, big-endian)
    /// and its bytes.
    fn encode(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let mut out = Zeroizing::new(vec![FORMAT]);
        for (item, bytes) in &self.0 {
            let len = u16::try_from(bytes.len()).map_err(|_| format!("{item:?} is too long"))?;
            out.push(tag(*item));
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(bytes);
        }
        Ok(out)
    }

    fn decode(bytes: &[u8]) -> Result<Bundle, String> {
        let bad = || "钥匙串中的密钥已损坏".to_owned();
        let (&format, mut rest) = bytes.split_first().ok_or_else(bad)?;
        if format != FORMAT {
            return Err(bad());
        }
        let mut bundle = Bundle::default();
        while let [tag, a, b, tail @ ..] = rest {
            let len = usize::from(u16::from_be_bytes([*a, *b]));
            if tail.len() < len {
                return Err(bad());
            }
            let (value, next) = tail.split_at(len);
            // A newer version's items are kept out, not refused.
            if let Some(item) = item_of(*tag) {
                bundle.set(item, value);
            }
            rest = next;
        }
        if rest.is_empty() {
            Ok(bundle)
        } else {
            Err(bad())
        }
    }
}

/// The version an entry's name holds.
fn version_of(name: &str) -> Option<semver::Version> {
    semver::Version::parse(name.strip_prefix(PREFIX)?).ok()
}

/// The items in one keychain entry of this version.
pub struct BundleStore<B> {
    backend: B,
    /// This version's entry.
    own: String,
    /// One read or change at a time: two first reads would ask twice.
    lock: Mutex<()>,
}

impl<B: Backend> BundleStore<B> {
    /// The store of version `version` (the app's) over `backend`.
    pub fn new(backend: B, version: &str) -> BundleStore<B> {
        BundleStore {
            backend,
            own: format!("{PREFIX}{version}"),
            lock: Mutex::new(()),
        }
    }

    pub fn get(&self, item: KeyItem) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        Ok(self.load()?.and_then(|bundle| bundle.get(item)))
    }

    pub fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        let mut bundle = self.load()?.unwrap_or_default();
        bundle.set(item, bytes);
        self.backend.write(&self.own, &bundle.encode()?)
    }

    pub fn delete(&self, item: KeyItem) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        let Some(mut bundle) = self.load()? else {
            return Ok(());
        };
        bundle.remove(item);
        // Kept, empty or not: see the module's notes.
        self.backend.write(&self.own, &bundle.encode()?)
    }

    /// Whether `item` is stored, without asking the user: before this
    /// version has an entry of its own, an older entry that may hold it
    /// counts (only reading it, which asks, would tell).
    pub fn contains(&self, item: KeyItem) -> Result<bool, String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        if let Some(bytes) = self.backend.read(&self.own)?
            && let Ok(bundle) = Bundle::decode(&bytes)
        {
            return Ok(bundle.get(item).is_some());
        }
        let names = self.backend.names()?;
        Ok(names
            .iter()
            .any(|name| *name != self.own && version_of(name).is_some() || name == item.account()))
    }

    /// This version's items, made from the older entries the first time.
    /// `None`: no entry at all. An error (the user refused an older entry)
    /// changes nothing, so that the next try can ask again.
    fn load(&self) -> Result<Option<Bundle>, String> {
        if let Some(bytes) = self.backend.read(&self.own)? {
            match Bundle::decode(&bytes) {
                Ok(bundle) => return Ok(Some(bundle)),
                // Made again from the older entries, else from nothing.
                Err(e) => log::error!("keychain: {} is unreadable: {e}", self.own),
            }
        }
        let names = self.backend.names()?;
        let (bundle, read) = match self.load_newest(&names)? {
            Some(found) => found,
            None => self.load_separate(&names)?,
        };
        if read.is_empty() {
            return Ok(None);
        }
        self.backend.write(&self.own, &bundle.encode()?)?;
        log::info!("keychain: moved {} entries into {}", read.len(), self.own);
        for name in read {
            if let Err(e) = self.backend.remove(&name) {
                log::info!("keychain: {name} stays ({e})");
            }
        }
        Ok(Some(bundle))
    }

    /// The newest older entry that decodes (a damaged one, or one of a
    /// format to come, is passed over), and its name.
    fn load_newest(&self, names: &[String]) -> Result<Option<(Bundle, Vec<String>)>, String> {
        let mut older: Vec<(semver::Version, &String)> = names
            .iter()
            .filter(|name| **name != self.own)
            .filter_map(|name| Some((version_of(name)?, name)))
            .collect();
        older.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, name) in older {
            let Some(bytes) = self.backend.read(name)? else {
                continue;
            };
            match Bundle::decode(&bytes) {
                Ok(bundle) => return Ok(Some((bundle, vec![name.clone()]))),
                Err(e) => log::warn!("keychain: {name} passed over: {e}"),
            }
        }
        Ok(None)
    }

    /// The entries from before this store, and which were read.
    fn load_separate(&self, names: &[String]) -> Result<(Bundle, Vec<String>), String> {
        let mut bundle = Bundle::default();
        let mut read = Vec::new();
        for item in SEPARATE {
            let name = item.account();
            if !names.iter().any(|n| n == name) {
                continue;
            }
            if let Some(bytes) = self.backend.read(name)? {
                bundle.set(item, &bytes);
                read.push(name.to_owned());
            }
        }
        Ok((bundle, read))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A keychain where only `owner`'s entries are silent: reading another
    /// version's counts as asking the user, and removing it is refused.
    #[derive(Default)]
    struct FakeKeychain {
        entries: Mutex<HashMap<String, (String, Vec<u8>)>>,
        owner: Mutex<String>,
        asked: AtomicUsize,
        /// The user refuses every question.
        refuse: Mutex<bool>,
    }

    impl FakeKeychain {
        fn as_version(&self, version: &str) {
            *self.owner.lock().unwrap() = format!("{PREFIX}{version}");
        }

        fn insert(&self, name: &str, owner: &str, bytes: &[u8]) {
            self.entries
                .lock()
                .unwrap()
                .insert(name.to_owned(), (owner.to_owned(), bytes.to_vec()));
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = self.entries.lock().unwrap().keys().cloned().collect();
            names.sort();
            names
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }
    }

    impl Backend for &FakeKeychain {
        fn names(&self) -> Result<Vec<String>, String> {
            Ok(FakeKeychain::names(self))
        }

        fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
            let owner = self.owner.lock().unwrap().clone();
            let entries = self.entries.lock().unwrap();
            let Some((entry_owner, bytes)) = entries.get(name) else {
                return Ok(None);
            };
            if *entry_owner != owner {
                self.asked.fetch_add(1, Ordering::Relaxed);
                if *self.refuse.lock().unwrap() {
                    return Err("refused".to_owned());
                }
            }
            Ok(Some(Zeroizing::new(bytes.clone())))
        }

        fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            let owner = self.owner.lock().unwrap().clone();
            self.insert(name, &owner, bytes);
            Ok(())
        }

        fn remove(&self, name: &str) -> Result<(), String> {
            let owner = self.owner.lock().unwrap().clone();
            let mut entries = self.entries.lock().unwrap();
            match entries.get(name) {
                Some((entry_owner, _)) if *entry_owner != owner => Err("not ours".to_owned()),
                _ => {
                    entries.remove(name);
                    Ok(())
                }
            }
        }
    }

    fn value(store: &BundleStore<&FakeKeychain>, item: KeyItem) -> Option<Vec<u8>> {
        store.get(item).unwrap().map(|b| b.to_vec())
    }

    #[test]
    fn a_new_store_keeps_items_in_one_entry() {
        let keychain = FakeKeychain::default();
        keychain.as_version("1.0.0");
        let store = BundleStore::new(&keychain, "1.0.0");
        assert_eq!(value(&store, KeyItem::DataKey), None);
        store.put(KeyItem::DataKey, b"dk").unwrap();
        store.put(KeyItem::SecretKey, b"sk").unwrap();
        store.put(KeyItem::SecretKey, b"sk2").unwrap();
        assert_eq!(value(&store, KeyItem::DataKey).unwrap(), b"dk");
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"sk2");
        assert_eq!(value(&store, KeyItem::DeviceToken), None);
        assert_eq!(keychain.names(), vec!["keys-1.0.0"]);
        assert_eq!(keychain.asked(), 0);
    }

    #[test]
    fn the_separate_entries_move_in_once() {
        let keychain = FakeKeychain::default();
        keychain.insert("data-key", "old build", b"dk");
        keychain.insert("secret-key", "old build", b"sk");
        keychain.insert("Unrelated", "someone", b"x");
        keychain.as_version("0.4.4");
        let store = BundleStore::new(&keychain, "0.4.4");

        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"sk");
        // One question per entry there was, the first time only.
        assert_eq!(keychain.asked(), 2);
        assert_eq!(value(&store, KeyItem::DataKey).unwrap(), b"dk");
        assert_eq!(value(&store, KeyItem::DeviceToken), None);
        store.put(KeyItem::DeviceToken, b"t").unwrap();
        assert_eq!(value(&store, KeyItem::DeviceToken).unwrap(), b"t");
        assert_eq!(keychain.asked(), 2);
        // The old ones are another program's: they stay.
        assert!(keychain.names().contains(&"keys-0.4.4".to_owned()));
    }

    #[test]
    fn a_new_version_asks_once_for_the_newest_entry() {
        let keychain = FakeKeychain::default();
        keychain.as_version("0.4.4");
        BundleStore::new(&keychain, "0.4.4")
            .put(KeyItem::SecretKey, b"old")
            .unwrap();
        keychain.as_version("0.4.10");
        BundleStore::new(&keychain, "0.4.10")
            .put(KeyItem::SecretKey, b"newest")
            .unwrap();

        // 0.4.10 asked once, for 0.4.4's entry.
        assert_eq!(keychain.asked(), 1);

        keychain.as_version("0.5.0");
        let store = BundleStore::new(&keychain, "0.5.0");
        // 0.4.10 is newer than 0.4.4, though it sorts before it.
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"newest");
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"newest");
        assert_eq!(keychain.asked(), 2);
    }

    #[test]
    fn what_was_removed_stays_removed() {
        let keychain = FakeKeychain::default();
        keychain.insert("data-key", "old build", b"dk");
        keychain.as_version("0.4.4");
        let store = BundleStore::new(&keychain, "0.4.4");
        store.delete(KeyItem::DataKey).unwrap();
        // Our entry is empty, and still there: the old one is not read again.
        assert_eq!(value(&store, KeyItem::DataKey), None);
        assert_eq!(value(&store, KeyItem::DataKey), None);
        assert_eq!(keychain.asked(), 1);
    }

    #[test]
    fn a_refusal_changes_nothing_and_asks_again() {
        let keychain = FakeKeychain::default();
        keychain.insert("secret-key", "old build", b"sk");
        keychain.as_version("0.4.4");
        *keychain.refuse.lock().unwrap() = true;
        let store = BundleStore::new(&keychain, "0.4.4");

        assert!(store.get(KeyItem::SecretKey).is_err());
        // Writing would hide the old entry behind one without it.
        assert!(store.put(KeyItem::DeviceToken, b"t").is_err());
        assert_eq!(keychain.names(), vec!["secret-key"]);

        *keychain.refuse.lock().unwrap() = false;
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"sk");
        assert_eq!(keychain.asked(), 3);
    }

    #[test]
    fn contains_never_asks() {
        let keychain = FakeKeychain::default();
        keychain.insert("data-key", "old build", b"dk");
        keychain.as_version("0.4.4");
        let store = BundleStore::new(&keychain, "0.4.4");
        // Before the move: what may be there counts.
        assert!(store.contains(KeyItem::DataKey).unwrap());
        assert!(!store.contains(KeyItem::SecretKey).unwrap());
        assert_eq!(keychain.asked(), 0);

        store.put(KeyItem::SecretKey, b"sk").unwrap();
        store.delete(KeyItem::DataKey).unwrap();
        assert!(store.contains(KeyItem::SecretKey).unwrap());
        assert!(!store.contains(KeyItem::DataKey).unwrap());
        assert_eq!(keychain.asked(), 1);

        keychain.as_version("0.4.5");
        let next = BundleStore::new(&keychain, "0.4.5");
        assert!(next.contains(KeyItem::DataKey).unwrap());
        assert_eq!(keychain.asked(), 1);
    }

    #[test]
    fn a_damaged_entry_is_passed_over() {
        let keychain = FakeKeychain::default();
        keychain.as_version("0.4.4");
        BundleStore::new(&keychain, "0.4.4")
            .put(KeyItem::SecretKey, b"sk")
            .unwrap();
        keychain.insert("keys-0.4.5", "0.4.5", &[2, 0]);
        keychain.as_version("0.4.6");
        let store = BundleStore::new(&keychain, "0.4.6");
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"sk");

        // Our own, damaged: made again rather than failing for good.
        keychain.insert("keys-0.4.6", "keys-0.4.6", &[FORMAT, 1]);
        assert_eq!(value(&store, KeyItem::SecretKey).unwrap(), b"sk");
        store.put(KeyItem::DeviceToken, b"t").unwrap();
        assert_eq!(value(&store, KeyItem::DeviceToken).unwrap(), b"t");
    }

    #[test]
    fn bundles_round_trip_and_damage_is_refused() {
        let mut bundle = Bundle::default();
        bundle.set(KeyItem::DataKey, &[7; 32]);
        bundle.set(KeyItem::DeviceToken, b"");
        let bytes = bundle.encode().unwrap();
        let back = Bundle::decode(&bytes).unwrap();
        assert_eq!(back.get(KeyItem::DataKey).unwrap().as_slice(), &[7; 32]);
        assert_eq!(back.get(KeyItem::DeviceToken).unwrap().as_slice(), b"");
        assert!(back.get(KeyItem::SecretKey).is_none());

        assert!(Bundle::decode(&[]).is_err());
        assert!(Bundle::decode(&[9]).is_err());
        assert!(Bundle::decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(Bundle::decode(&[FORMAT, 1, 0]).is_err());
        // An item a newer version added is skipped.
        let newer = [FORMAT, 99, 0, 1, b'x', 2, 0, 2, b's', b'k'];
        let decoded = Bundle::decode(&newer).unwrap();
        assert_eq!(decoded.get(KeyItem::SecretKey).unwrap().as_slice(), b"sk");
    }
}
