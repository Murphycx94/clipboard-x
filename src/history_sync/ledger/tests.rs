use super::*;
use crate::capture::hash_bytes;
use crate::db::Database;

/// A database with history sync on.
fn synced() -> Database {
    let db = Database::in_memory().unwrap();
    db.set_history_sync(true).unwrap();
    db
}

fn copy(db: &Database, text: &str) -> String {
    let hash = hash_bytes(text.as_bytes());
    db.insert_text(text, &hash).unwrap();
    hash
}

fn id_of(db: &Database, hash: &str) -> i64 {
    db.with_conn(|c| {
        c.query_row(
            "SELECT id FROM clipboard_items WHERE hash = ?1",
            params![hash],
            |r| r.get(0),
        )
    })
    .unwrap()
}

fn local(db: &Database, hash: &str) -> Option<LocalItem> {
    db.with_conn(|c| local_item(c, hash)).unwrap()
}

fn ledger_rows(db: &Database) -> i64 {
    db.with_conn(|c| c.query_row("SELECT count(*) FROM history_sync", [], |r| r.get(0)))
        .unwrap()
}

fn pending_hashes(db: &Database) -> Vec<String> {
    db.with_conn(|c| pending(c, usize::MAX))
        .unwrap()
        .into_iter()
        .map(|p| p.state.hash)
        .collect()
}

fn push_all(db: &Database) {
    db.with_conn(|c| {
        let all = pending(c, usize::MAX)?;
        mark_pushed(c, &all)
    })
    .unwrap();
}

fn in_history(db: &Database, hash: &str) -> bool {
    db.with_conn(|c| {
        c.query_row(
            "SELECT count(*) FROM clipboard_items WHERE hash = ?1",
            params![hash],
            |r| r.get::<_, i64>(0),
        )
    })
    .unwrap()
        > 0
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn remote(text: &str, copied_at: i64) -> ItemState {
    ItemState {
        hash: hash_bytes(text.as_bytes()),
        text: Some(text.into()),
        copied_at,
        meta_at: 0,
        favorite: false,
        note: String::new(),
        deleted_at: 0,
    }
}

fn apply(db: &Database, items: &[ItemState], cutoff_ms: Option<i64>) -> bool {
    db.with_conn(|c| apply_page(c, items, cutoff_ms)).unwrap()
}

#[test]
fn times_go_to_milliseconds_and_back() {
    for text in ["2026-11-25T10:20:30.123", "2020-01-01T00:00:00.000"] {
        assert_eq!(local_of_ms(ms_of_local(text)), text);
    }
    // As the Tauri version wrote them: no milliseconds.
    assert_eq!(
        ms_of_local("2026-11-25T10:20:30"),
        ms_of_local("2026-11-25T10:20:30.000")
    );
    assert_eq!(ms_of_local("not a time"), 0);
}

#[test]
fn a_copy_is_noted_to_upload() {
    let db = synced();
    let before = now();
    let hash = copy(&db, "hello");
    let item = local(&db, &hash).unwrap();
    assert!(item.state.alive());
    assert_eq!(item.state.text.as_deref(), Some("hello"));
    assert!(item.state.copied_at >= before - 1);
    assert_eq!(pending_hashes(&db), vec![hash.clone()]);

    // Copied again later: moves, and goes up again.
    push_all(&db);
    assert!(pending_hashes(&db).is_empty());
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(db.touch_item(&hash).unwrap());
    let again = local(&db, &hash).unwrap();
    assert!(again.state.copied_at > item.state.copied_at);
    assert_eq!(pending_hashes(&db), vec![hash]);
}

#[test]
fn images_long_texts_and_changes_while_off_are_not_noted() {
    let db = synced();
    db.insert_image(&[1, 2, 3], &[1], "image").unwrap();
    let long = "x".repeat(MAX_TEXT_BYTES + 1);
    copy(&db, &long);
    assert!(db.touch_item("image").unwrap());
    assert_eq!(ledger_rows(&db), 0);

    let off = Database::in_memory().unwrap();
    let hash = copy(&off, "hello");
    off.toggle_favorite(id_of(&off, &hash), "note").unwrap();
    off.delete_item(id_of(&off, &hash)).unwrap();
    assert_eq!(ledger_rows(&off), 0);
}

#[test]
fn marks_and_notes_are_noted() {
    let db = synced();
    let hash = copy(&db, "hello");
    push_all(&db);
    db.toggle_favorite(id_of(&db, &hash), "a note").unwrap();
    let item = local(&db, &hash).unwrap();
    assert!(item.state.favorite);
    assert_eq!(item.state.note, "a note");
    assert!(item.state.meta_at > 0);
    assert_eq!(pending_hashes(&db), vec![hash.clone()]);

    push_all(&db);
    db.update_note(id_of(&db, &hash), "another").unwrap();
    let edited = local(&db, &hash).unwrap();
    assert_eq!(edited.state.note, "another");
    assert!(edited.state.meta_at > item.state.meta_at);
    assert_eq!(pending_hashes(&db), vec![hash]);
}

#[test]
fn a_deletion_stays_in_the_ledger_without_the_text() {
    let db = synced();
    let hash = copy(&db, "secret");
    db.toggle_favorite(id_of(&db, &hash), "n").unwrap();
    push_all(&db);
    db.delete_item(id_of(&db, &hash)).unwrap();
    assert!(!in_history(&db, &hash));
    let item = local(&db, &hash).unwrap();
    assert!(!item.state.alive());
    assert_eq!(item.state.text, None);
    assert!(!item.state.favorite);
    assert_eq!(pending_hashes(&db), vec![hash.clone()]);

    // Copied again: back, unmarked.
    copy(&db, "secret");
    let back = local(&db, &hash).unwrap();
    assert!(back.state.alive());
    assert!(!back.state.favorite);
    assert_eq!(back.state.text.as_deref(), Some("secret"));
}

#[test]
fn clearing_the_history_deletes_all_but_the_favorites() {
    let db = synced();
    let plain = copy(&db, "plain");
    let kept = copy(&db, "kept");
    db.toggle_favorite(id_of(&db, &kept), "").unwrap();
    db.clear_history().unwrap();
    assert!(!local(&db, &plain).unwrap().state.alive());
    assert!(local(&db, &kept).unwrap().state.alive());
}

#[test]
fn expired_items_leave_no_trace_to_sync() {
    let db = synced();
    let old = hash_bytes(b"old");
    db.insert_text_at("old", &old, "2020-01-01T00:00:00").unwrap();
    let new = copy(&db, "new");
    db.cleanup_expired(30).unwrap();
    assert_eq!(local(&db, &old), None);
    assert!(local(&db, &new).is_some());
    assert_eq!(pending_hashes(&db), vec![new]);
}

#[test]
fn turning_it_on_takes_the_history_as_it_is_and_off_forgets() {
    let db = Database::in_memory().unwrap();
    let plain = copy(&db, "plain");
    let marked = copy(&db, "marked");
    db.toggle_favorite(id_of(&db, &marked), "").unwrap();
    db.insert_image(&[1], &[1], "image").unwrap();

    db.set_history_sync(true).unwrap();
    assert!(db.history_sync_enabled());
    let plain = local(&db, &plain).unwrap().state;
    let marked = local(&db, &marked).unwrap().state;
    assert_eq!(plain.meta_at, 0);
    assert_eq!(marked.meta_at, marked.copied_at);
    assert!(marked.copied_at > 0);
    assert_eq!(pending_hashes(&db).len(), 2);

    db.set_history_sync(false).unwrap();
    assert!(!db.history_sync_enabled());
    assert_eq!(ledger_rows(&db), 0);
    assert!(in_history(&db, &plain.hash));
    copy(&db, "while off");
    assert_eq!(ledger_rows(&db), 0);
}

#[test]
fn an_item_from_elsewhere_comes_in_as_the_server_has_it() {
    let db = synced();
    let at = now() - 60_000;
    let item = ItemState {
        favorite: true,
        note: "from the phone".into(),
        meta_at: at,
        ..remote("from the phone", at)
    };
    assert!(apply(&db, std::slice::from_ref(&item), None));
    let listed = db.favorite_items().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].created_at, local_of_ms(at));
    assert_eq!(listed[0].note.as_deref(), Some("from the phone"));
    assert_eq!(local(&db, &item.hash).unwrap().state, item);
    // What the server has: nothing to upload, and taking it again changes nothing.
    assert!(pending_hashes(&db).is_empty());
    assert!(!apply(&db, &[item], None));
}

#[test]
fn a_deletion_from_elsewhere_removes_the_item_here() {
    let db = synced();
    let hash = copy(&db, "hello");
    push_all(&db);
    let mine = local(&db, &hash).unwrap().state;
    let deleted = ItemState {
        text: None,
        meta_at: mine.copied_at + 10,
        deleted_at: mine.copied_at + 10,
        ..mine
    };
    assert!(apply(&db, std::slice::from_ref(&deleted), None));
    assert!(!in_history(&db, &hash));
    assert_eq!(local(&db, &hash).unwrap().state, deleted);
    assert!(pending_hashes(&db).is_empty());
}

#[test]
fn old_and_deleted_items_this_device_never_had_stay_out() {
    let db = synced();
    let cutoff = now() - 1_000;
    let old = remote("old", cutoff - 1);
    let deleted = ItemState {
        text: None,
        meta_at: 5,
        deleted_at: 5,
        ..remote("gone", 1)
    };
    assert!(!apply(&db, &[old.clone(), deleted], Some(cutoff)));
    assert_eq!(ledger_rows(&db), 0);
    // A favorite is kept whatever its age.
    let favorite = ItemState {
        favorite: true,
        meta_at: cutoff - 1,
        ..old
    };
    assert!(apply(&db, &[favorite], Some(cutoff)));
    assert_eq!(db.favorite_items().unwrap().len(), 1);
}

#[test]
fn a_merge_newer_than_the_servers_goes_up_again() {
    let db = synced();
    let hash = copy(&db, "hello");
    db.toggle_favorite(id_of(&db, &hash), "mine").unwrap();
    push_all(&db);
    let mine = local(&db, &hash).unwrap().state;
    // Another device's older state: the merge is this one's.
    let older = remote("hello", mine.copied_at - 1_000);
    assert!(!apply(&db, &[older], None));
    assert_eq!(pending_hashes(&db), vec![hash.clone()]);
    assert_eq!(local(&db, &hash).unwrap().state, mine);
}

#[test]
fn changes_made_during_an_upload_stay_to_upload() {
    let db = synced();
    let hash = copy(&db, "hello");
    let read = db.with_conn(|c| pending(c, usize::MAX)).unwrap();
    db.toggle_favorite(id_of(&db, &hash), "").unwrap();
    db.with_conn(|c| mark_pushed(c, &read)).unwrap();
    assert_eq!(pending_hashes(&db), vec![hash]);
}

#[test]
fn uploads_come_in_batches_of_about_the_budget() {
    let db = synced();
    for text in ["aaaa", "bbbb", "cccc"] {
        copy(&db, text);
    }
    let batch = db.with_conn(|c| pending(c, 9)).unwrap();
    assert_eq!(batch.len(), 2);
    let one = db.with_conn(|c| pending(c, 1)).unwrap();
    assert_eq!(one.len(), 1);
}

#[test]
fn a_new_header_sends_everything_again_from_the_start() {
    let db = synced();
    let hash = copy(&db, "hello");
    assert_eq!(db.with_conn(|c| begin_round(c, 1)).unwrap(), 0);
    push_all(&db);
    db.with_conn(|c| fetched_to(c, 7)).unwrap();
    assert_eq!(db.with_conn(|c| begin_round(c, 1)).unwrap(), 7);
    assert!(pending_hashes(&db).is_empty());

    assert_eq!(db.with_conn(|c| begin_round(c, 2)).unwrap(), 0);
    assert_eq!(pending_hashes(&db), vec![hash]);
}

#[test]
fn an_item_gone_from_the_history_without_a_deletion_counts_as_deleted() {
    let db = synced();
    let hash = copy(&db, "hello");
    db.with_conn(|c| c.execute("DELETE FROM clipboard_items", []))
        .unwrap();
    let item = local(&db, &hash).unwrap().state;
    assert!(!item.alive());
    assert_eq!(item.text, None);
}
