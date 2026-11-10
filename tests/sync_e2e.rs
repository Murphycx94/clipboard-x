//! Two devices syncing their vaults through a real sync server on a local
//! port: joining, edits, deletions, conflicts, a new master password, and
//! changes arriving in real time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use clipboardx::cloud::{self, Client, Worker};
use clipboardx::db::Database;
use clipboardx::vault::account_key::AccountKey;
use clipboardx::vault::keychain::MemoryKeyStore;
use clipboardx::vault::sync::CONFLICT_SUFFIX;
use clipboardx::vault::{KdfParams, VaultEntry, VaultError, VaultService, VaultStatus, now_ms};
use clipboardx_sync_server::{Config, new_state, serve, store::Store};

const PW: &str = "correct horse battery";

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

fn vault() -> VaultService {
    VaultService::with_params(
        Arc::new(Database::in_memory().unwrap()),
        Box::new(MemoryKeyStore::default()),
        KdfParams::INSECURE_TEST,
    )
}

fn entry(title: &str, password: &str) -> VaultEntry {
    let mut e = VaultEntry::default();
    e.title = title.into();
    e.password = password.into();
    e
}

fn client(v: &VaultService) -> Client {
    cloud::client_for(v).unwrap().unwrap()
}

fn sync(v: &VaultService) -> Result<cloud::Report, VaultError> {
    cloud::sync_once(v, &client(v))
}

/// The first device (with the admin code) and a second (with a pairing
/// code), both unlocked and synced.
fn two_devices() -> (VaultService, VaultService, String) {
    let (url, admin) = start_server();
    let a = vault();
    a.setup(PW).unwrap();
    a.save(None, entry("from before sync", "old")).unwrap();
    let key = AccountKey::generate().unwrap();
    a.enable_account_key(PW, &key).unwrap();
    let token = Client::new(&url).unwrap().bootstrap(&admin, "Mac").unwrap();
    a.start_sync(&url, &token.device_id, &token.token).unwrap();
    sync(&a).unwrap();

    let code = client(&a).pair().unwrap().code;
    let b = vault();
    let joined = Client::new(&url).unwrap().join(&code, "Laptop").unwrap();
    b.start_sync(&url, &joined.device_id, &joined.token)
        .unwrap();
    let header = client(&b).vault().unwrap().header.unwrap();
    b.join_header(&header).unwrap();
    assert!(b.needs_account_key());
    b.unlock_with_account_key(PW, &key.encode(), now_ms())
        .unwrap();
    sync(&b).unwrap();
    (a, b, url)
}

fn titles(v: &VaultService) -> Vec<String> {
    let mut titles: Vec<String> = v.list().unwrap().into_iter().map(|s| s.title).collect();
    titles.sort();
    titles
}

#[test]
fn a_joined_device_gets_everything_and_both_stay_in_step() {
    let (a, b, _) = two_devices();
    assert_eq!(titles(&b), vec!["from before sync"]);

    let id = b.save(None, entry("GitHub", "pw-1")).unwrap();
    sync(&b).unwrap();
    sync(&a).unwrap();
    assert_eq!(a.get(&id).unwrap().password, "pw-1");

    let mut edited = a.get(&id).unwrap();
    edited.password = "pw-2".into();
    a.save(Some(&id), edited).unwrap();
    a.delete(
        &a.list()
            .unwrap()
            .into_iter()
            .find(|s| s.title == "from before sync")
            .unwrap()
            .id,
    )
    .unwrap();
    sync(&a).unwrap();
    sync(&b).unwrap();
    assert_eq!(b.get(&id).unwrap().password, "pw-2");
    assert_eq!(titles(&b), vec!["GitHub"]);
}

#[test]
fn concurrent_edits_leave_a_conflict_copy_on_both() {
    let (a, b, _) = two_devices();
    let id = a.save(None, entry("Mail", "base")).unwrap();
    sync(&a).unwrap();
    sync(&b).unwrap();
    for (v, password) in [(&a, "from-a"), (&b, "from-b")] {
        let mut e = v.get(&id).unwrap();
        e.password = password.into();
        v.save(Some(&id), e).unwrap();
    }
    sync(&a).unwrap();
    let report = sync(&b).unwrap();
    assert_eq!(report.applied.conflicts, 1);
    sync(&a).unwrap();
    let expected = vec![
        "Mail".to_owned(),
        format!("Mail{CONFLICT_SUFFIX}"),
        "from before sync".to_owned(),
    ];
    assert_eq!(titles(&a), expected);
    assert_eq!(titles(&b), expected);
    assert_eq!(b.get(&id).unwrap().password, "from-a");
}

#[test]
fn a_new_master_password_reaches_the_other_device() {
    let (a, b, _) = two_devices();
    a.change_password(PW, "brand new pass phrase").unwrap();
    sync(&a).unwrap();
    // b keeps its vault and waits for the new password.
    assert_eq!(sync(&b).unwrap_err(), VaultError::HeaderChanged);
    assert!(b.has_staged_header());
    assert_eq!(b.status(), VaultStatus::Unlocked);
    assert_eq!(
        b.adopt_staged_header(PW).unwrap_err(),
        VaultError::WrongPassword
    );
    b.adopt_staged_header("brand new pass phrase").unwrap();
    sync(&b).unwrap();
    assert_eq!(titles(&b), vec!["from before sync"]);
    // And b's edits still reach a.
    b.save(None, entry("after", "x")).unwrap();
    sync(&b).unwrap();
    sync(&a).unwrap();
    assert_eq!(titles(&a), vec!["after", "from before sync"]);
}

#[test]
fn changes_arrive_in_real_time() {
    let (a, b, _) = two_devices();
    let b: &'static VaultService = Box::leak(Box::new(b));
    let worker = Worker::default();
    worker.start(move || b, || {});
    // Let the worker connect to /events.
    std::thread::sleep(Duration::from_millis(500));

    let id = a.save(None, entry("live", "now")).unwrap();
    sync(&a).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while b.get(&id).is_err() {
        assert!(
            Instant::now() < deadline,
            "the change did not arrive: {:?}",
            worker.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(worker.status().last_error.is_none());
}

#[test]
fn a_revoked_device_cannot_sync() {
    let (a, b, _) = two_devices();
    let b_id = b.sync_state().unwrap().unwrap().device_id;
    client(&a).revoke(&b_id).unwrap();
    assert!(matches!(sync(&b).unwrap_err(), VaultError::Sync(_)));
    assert_eq!(client(&a).devices().unwrap().len(), 1);
}
