//! Two devices syncing their clipboard histories through a real sync server
//! on a local port: copies, marks, notes, deletions, clearing, a device with
//! it off, and a new master password.

use std::sync::Arc;

use clipboardx::capture::hash_bytes;
use clipboardx::cloud::{self, Client};
use clipboardx::db::Database;
use clipboardx::history_sync;
use clipboardx::vault::account_key::AccountKey;
use clipboardx::vault::keychain::MemoryKeyStore;
use clipboardx::vault::{KdfParams, VaultError, VaultService, now_ms};
use clipboardx_sync_server::{Config, new_state, serve, store::Store};

const PW: &str = "correct horse battery";
const NEW_PW: &str = "brand new pass phrase";

/// A sync server on a port of its own; its URL and admin code.
fn start_server() -> (String, String) {
    let (state, admin) = new_state(Store::in_memory().unwrap(), Config::default()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || runtime.block_on(async { serve(listener, state).await.unwrap() }));
    (url, admin.unwrap())
}

struct Device {
    vault: VaultService,
    db: Arc<Database>,
}

impl Device {
    fn new() -> Device {
        let db = Arc::new(Database::in_memory().unwrap());
        let vault = VaultService::with_params(
            db.clone(),
            Box::new(MemoryKeyStore::default()),
            KdfParams::INSECURE_TEST,
        );
        Device { vault, db }
    }

    fn client(&self) -> Client {
        cloud::client_for(&self.vault).unwrap().unwrap()
    }

    /// A round of the vault, then one of the history, as the worker does.
    fn sync(&self) -> Result<history_sync::Report, VaultError> {
        let client = self.client();
        cloud::sync_once(&self.vault, &client)?;
        history_sync::sync_once(&self.vault, &self.db, &client)
    }

    fn copy(&self, text: &str) -> String {
        let hash = hash_bytes(text.as_bytes());
        self.db.insert_text(text, &hash).unwrap();
        hash
    }

    fn id(&self, text: &str) -> i64 {
        self.db
            .today_items()
            .unwrap()
            .into_iter()
            .find(|i| i.text_preview.as_deref() == Some(text))
            .unwrap_or_else(|| panic!("{text} is not in the history"))
            .id
    }

    /// The texts of today's items, newest first.
    fn texts(&self) -> Vec<String> {
        self.db
            .today_items()
            .unwrap()
            .into_iter()
            .filter_map(|i| i.text_preview)
            .collect()
    }

    /// The favorites' texts and notes.
    fn favorites(&self) -> Vec<(String, String)> {
        self.db
            .favorite_items()
            .unwrap()
            .into_iter()
            .map(|i| (i.text_preview.unwrap_or_default(), i.note.unwrap_or_default()))
            .collect()
    }
}

/// The first device (with the admin code) and a second (with a pairing
/// code), both unlocked, synced, and with history sync as `on` says.
fn two_devices(on: [bool; 2]) -> (Device, Device) {
    let (url, admin) = start_server();
    let a = Device::new();
    a.vault.setup(PW).unwrap();
    let key = AccountKey::generate().unwrap();
    a.vault.enable_account_key(PW, &key).unwrap();
    let token = Client::new(&url).unwrap().bootstrap(&admin, "Mac").unwrap();
    a.vault
        .start_sync(&url, &token.device_id, &token.token)
        .unwrap();
    a.db.set_history_sync(on[0]).unwrap();
    a.sync().unwrap();

    let code = a.client().pair().unwrap().code;
    let b = Device::new();
    let joined = Client::new(&url).unwrap().join(&code, "Phone").unwrap();
    b.vault
        .start_sync(&url, &joined.device_id, &joined.token)
        .unwrap();
    let header = b.client().vault().unwrap().header.unwrap();
    b.vault.join_header(&header).unwrap();
    b.vault
        .unlock_with_account_key(PW, &key.encode(), now_ms())
        .unwrap();
    b.db.set_history_sync(on[1]).unwrap();
    b.sync().unwrap();
    (a, b)
}

/// Both devices' rounds, then the first's again: what one changed reaches
/// the other.
fn settle(a: &Device, b: &Device) {
    a.sync().unwrap();
    b.sync().unwrap();
    a.sync().unwrap();
}

#[test]
fn a_copy_reaches_the_other_device() {
    let (a, b) = two_devices([true, true]);
    a.copy("from the Mac");
    let report = a.sync().unwrap();
    assert_eq!(report.pushed, 1);
    assert!(b.sync().unwrap().changed);
    assert_eq!(b.texts(), vec!["from the Mac"]);
    // Nothing new: a round changes nothing.
    assert_eq!(b.sync().unwrap(), history_sync::Report::default());
}

#[test]
fn marks_notes_deletions_and_clearing_follow() {
    let (a, b) = two_devices([true, true]);
    a.copy("kept");
    a.copy("dropped");
    a.copy("cleared");
    settle(&a, &b);

    b.db.toggle_favorite(b.id("kept"), "a note").unwrap();
    b.db.delete_item(b.id("dropped")).unwrap();
    settle(&a, &b);
    assert_eq!(a.favorites(), vec![("kept".into(), "a note".into())]);
    assert_eq!(a.texts(), vec!["cleared", "kept"]);

    a.db.update_note(a.id("kept"), "edited").unwrap();
    a.db.clear_history().unwrap();
    settle(&a, &b);
    assert_eq!(b.texts(), vec!["kept"]);
    assert_eq!(b.favorites(), vec![("kept".into(), "edited".into())]);
}

#[test]
fn a_deleted_text_copied_again_comes_back_everywhere() {
    let (a, b) = two_devices([true, true]);
    a.copy("again");
    settle(&a, &b);
    b.db.delete_item(b.id("again")).unwrap();
    settle(&a, &b);
    assert!(a.texts().is_empty());

    std::thread::sleep(std::time::Duration::from_millis(5));
    a.copy("again");
    settle(&a, &b);
    assert_eq!(b.texts(), vec!["again"]);
}

#[test]
fn a_device_with_it_off_neither_sends_nor_receives() {
    let (a, b) = two_devices([true, false]);
    a.copy("from the Mac");
    b.copy("from the phone");
    settle(&a, &b);
    assert_eq!(a.texts(), vec!["from the Mac"]);
    assert_eq!(b.texts(), vec!["from the phone"]);

    // Turned on later: what it has goes up, what the other has comes down.
    b.db.set_history_sync(true).unwrap();
    settle(&a, &b);
    let mut both = vec!["from the Mac".to_owned(), "from the phone".to_owned()];
    both.sort();
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(sorted(a.texts()), both);
    assert_eq!(sorted(b.texts()), both);
}

#[test]
fn turning_cloud_sync_off_turns_history_sync_off() {
    let (a, _b) = two_devices([true, true]);
    a.vault.stop_sync().unwrap();
    assert!(!a.db.history_sync_enabled());
}

#[test]
fn after_a_new_master_password_the_histories_go_up_again_under_the_new_key() {
    let (a, b) = two_devices([true, true]);
    a.copy("before");
    settle(&a, &b);

    a.vault.change_password(PW, NEW_PW).unwrap();
    // The new header clears the server's items; a sends its own again.
    a.sync().unwrap();
    assert_eq!(b.sync().unwrap_err(), VaultError::HeaderChanged);
    // Until b takes the new password, its history waits.
    b.copy("while waiting");
    b.vault.adopt_staged_header(NEW_PW).unwrap();
    settle(&a, &b);
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(sorted(a.texts()), vec!["before", "while waiting"]);
    assert_eq!(sorted(b.texts()), vec!["before", "while waiting"]);
}

#[test]
fn a_locked_vault_syncs_no_history() {
    let (a, _b) = two_devices([true, true]);
    a.copy("locked away");
    a.vault.lock();
    assert_eq!(
        history_sync::sync_once(&a.vault, &a.db, &a.client()).unwrap_err(),
        VaultError::Locked
    );
}
