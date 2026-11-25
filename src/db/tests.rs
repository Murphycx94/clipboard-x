use super::*;

#[test]
fn an_item_hash_is_found_by_id() {
    let db = db();
    db.insert_text_at("t", "h-text", "2020-01-01T00:00:00.000")
        .unwrap();
    let id = db.archive_groups().unwrap()[0].items[0].id;
    assert_eq!(db.item_hash(id).unwrap().as_deref(), Some("h-text"));
    assert_eq!(db.item_hash(id + 1).unwrap(), None);
}

#[test]
fn an_item_copied_again_moves_to_now() {
    let db = db();
    assert!(!db.touch_item("h").unwrap());
    db.insert_text_at("old", "h", "2020-01-01T00:00:00.000")
        .unwrap();
    assert!(db.today_items().unwrap().is_empty());
    assert!(db.touch_item("h").unwrap());
    assert_eq!(db.today_items().unwrap().len(), 1);
}

fn db() -> Database {
    Database::in_memory().expect("in-memory db")
}

fn pragma(conn: &Connection, name: &str) -> i64 {
    conn.pragma_query_value(None, name, |r| r.get(0)).unwrap()
}

/// An image item `n` with a blob big enough to span many pages.
fn insert_big_image(db: &Database, n: u8) {
    db.insert_image(&vec![n; 512 * 1024], &[n], &format!("big-{n}"))
        .unwrap();
}

#[test]
fn a_new_database_gives_freed_space_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("clipboardx.db")).unwrap();
    assert_eq!(pragma(&db.conn(), "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
    insert_big_image(&db, 1);
    insert_big_image(&db, 2);
    let pages = pragma(&db.conn(), "page_count");
    assert_eq!(db.clear_history().unwrap(), 2);
    assert!(pragma(&db.conn(), "freelist_count") > 100);
    assert!(db.reclaim_space().unwrap() > 100);
    assert_eq!(pragma(&db.conn(), "freelist_count"), 0);
    assert!(pragma(&db.conn(), "page_count") < pages / 10);
    // Nothing left to give back.
    assert_eq!(db.reclaim_space().unwrap(), 0);
}

#[test]
fn an_older_database_is_compacted_and_made_incremental_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clipboardx.db");
    {
        // As versions before this one left it: no auto-vacuum, and
        // the space of deleted items still held.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        assert_eq!(pragma(&conn, "auto_vacuum"), 0);
        let db = Database {
            conn: Mutex::new(conn),
            syncs_history: AtomicBool::new(false),
        };
        insert_big_image(&db, 1);
        insert_big_image(&db, 2);
        db.insert_text_at("kept", "kept", "2020-01-01T00:00:00.000")
            .unwrap();
        db.delete_item(db.image_ids().unwrap()[0]).unwrap();
        assert!(pragma(&db.conn(), "freelist_count") > 100);
    }
    let db = Database::open(&path).unwrap();
    assert_eq!(pragma(&db.conn(), "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
    assert_eq!(pragma(&db.conn(), "freelist_count"), 0);
    // The image left (today's) and the text (an earlier day's).
    assert_eq!(db.today_items().unwrap().len(), 1);
    assert_eq!(db.archive_groups().unwrap()[0].items.len(), 1);
    drop(db);
    // The file itself shrank: the WAL was checkpointed into it.
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size < 800 * 1024, "{size} bytes");
}

#[test]
fn new_database_has_default_settings() {
    let settings = db().settings().unwrap();
    assert_eq!(settings, Settings::default());
}

#[test]
fn opens_a_file_and_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clipboardx.db");
    Database::open(&path)
        .unwrap()
        .insert_text("kept", "h")
        .unwrap();
    let again = Database::open(&path).unwrap();
    assert_eq!(again.today_items().unwrap().len(), 1);
}

#[test]
fn same_text_moves_to_the_top_instead_of_duplicating() {
    let db = db();
    db.insert_text_at("first", "h1", "2020-01-01T10:00:00.000")
        .unwrap();
    db.insert_text_at("second", "h2", "2020-01-01T10:00:01.000")
        .unwrap();
    db.insert_text_at("first", "h1", "2020-01-01T10:00:02.000")
        .unwrap();
    let items = db.archive_groups().unwrap().remove(0).items;
    let texts: Vec<_> = items
        .iter()
        .map(|i| i.text_preview.clone().unwrap())
        .collect();
    assert_eq!(texts, vec!["first", "second"]);
}

#[test]
fn the_day_splits_at_midnight_whatever_the_timestamps_look_like() {
    let db = db();
    let today = Database::today();
    let yesterday = (Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    // With milliseconds and, as older versions wrote them, without.
    db.insert_text_at("first", "h1", &format!("{today}T00:00:00.000"))
        .unwrap();
    db.insert_text_at("second", "h2", &format!("{today}T08:00:00"))
        .unwrap();
    db.insert_text_at("last", "h3", &format!("{yesterday}T23:59:59.999"))
        .unwrap();
    db.insert_text_at("late", "h4", &format!("{yesterday}T23:59:59"))
        .unwrap();
    let texts = |items: &[ClipboardItem]| -> Vec<String> {
        items
            .iter()
            .filter_map(|i| i.text_preview.clone())
            .collect()
    };
    assert_eq!(texts(&db.today_items().unwrap()), ["second", "first"]);
    let archive = db.archive_groups().unwrap();
    assert_eq!(archive.len(), 1);
    assert_eq!(archive[0].date, yesterday);
    assert_eq!(texts(&archive[0].items), ["last", "late"]);
}

#[test]
fn the_day_queries_use_the_index() {
    let db = db();
    let plan = |filter: &str| -> String {
        db.with_conn(|c| {
            let sql = format!(
                "EXPLAIN QUERY PLAN SELECT id FROM clipboard_items WHERE {filter}
                 ORDER BY created_at DESC, id DESC"
            );
            let mut stmt = c.prepare(&sql).unwrap();
            let rows = stmt
                .query_map(["2026-01-01"], |r| r.get::<_, String>(3))
                .unwrap();
            rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("; ")
        })
    };
    for filter in [TODAY, BEFORE_TODAY] {
        let plan = plan(filter);
        assert!(plan.contains("idx_created_at"), "{filter}: {plan}");
    }
}

#[test]
fn today_and_archive_split_by_day() {
    let db = db();
    db.insert_text("today", "h1").unwrap();
    db.insert_text_at("old", "h2", "2020-01-02T10:00:00")
        .unwrap();
    db.insert_text_at("older", "h3", "2020-01-01T10:00:00")
        .unwrap();
    assert_eq!(db.today_items().unwrap().len(), 1);
    let groups = db.archive_groups().unwrap();
    assert_eq!(
        groups.iter().map(|g| g.date.as_str()).collect::<Vec<_>>(),
        vec!["2020-01-02", "2020-01-01"]
    );
}

#[test]
fn previews_are_truncated_but_content_is_whole() {
    let db = db();
    let long = "x".repeat(1000);
    db.insert_text(&long, "h").unwrap();
    let item = &db.today_items().unwrap()[0];
    assert_eq!(
        item.text_preview.as_ref().unwrap().len(),
        PREVIEW_CHARS as usize
    );
    assert_eq!(
        db.item_content(item.id).unwrap(),
        Some(ItemContent::Text(long))
    );
}

#[test]
fn images_carry_a_thumbnail_data_uri_and_full_bytes() {
    let db = db();
    db.insert_image(&[1, 2, 3], &[4, 5], "img").unwrap();
    let item = &db.today_items().unwrap()[0];
    assert_eq!(item.content_type, ContentType::Image);
    assert!(
        item.thumbnail
            .as_ref()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(
        db.item_content(item.id).unwrap(),
        Some(ItemContent::Image(vec![1, 2, 3]))
    );
}

#[test]
fn favorites_toggle_with_notes() {
    let db = db();
    db.insert_text("a", "h").unwrap();
    let id = db.today_items().unwrap()[0].id;
    assert!(db.toggle_favorite(id, "note").unwrap());
    assert!(db.is_favorite(id).unwrap());
    let favorites = db.favorite_items().unwrap();
    assert_eq!(favorites[0].note.as_deref(), Some("note"));
    assert_eq!(db.favorites_with_content().unwrap().len(), 1);
    db.update_note(id, "changed").unwrap();
    assert_eq!(
        db.favorite_items().unwrap()[0].note.as_deref(),
        Some("changed")
    );
    assert!(!db.toggle_favorite(id, "").unwrap());
    assert!(db.favorite_items().unwrap().is_empty());
}

#[test]
fn delete_and_missing_items() {
    let db = db();
    db.insert_text("a", "h").unwrap();
    let id = db.today_items().unwrap()[0].id;
    db.delete_item(id).unwrap();
    assert_eq!(db.item_content(id).unwrap(), None);
    assert!(!db.is_favorite(id).unwrap());
}

#[test]
fn clearing_and_expiry_keep_favorites() {
    let db = db();
    db.insert_text_at("old", "h1", "2020-01-01T10:00:00")
        .unwrap();
    db.insert_text_at("old favorite", "h2", "2020-01-01T10:00:00")
        .unwrap();
    db.insert_text("new", "h3").unwrap();
    let favorite = db
        .archive_groups()
        .unwrap()
        .remove(0)
        .items
        .into_iter()
        .find(|i| i.text_preview.as_deref() == Some("old favorite"))
        .unwrap();
    db.toggle_favorite(favorite.id, "").unwrap();

    assert_eq!(db.cleanup_expired(0).unwrap(), 0);
    assert_eq!(db.cleanup_expired(7).unwrap(), 1);
    assert_eq!(db.today_items().unwrap().len(), 1);
    assert_eq!(db.clear_history().unwrap(), 1);
    assert_eq!(db.favorite_items().unwrap().len(), 1);
}

#[test]
fn vault_settings_have_defaults_and_save() {
    let db = db();
    let s = db.settings().unwrap();
    assert_eq!(
        (s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id),
        (5, true, false)
    );
    db.set_vault_lock_minutes(15).unwrap();
    db.set_vault_lock_on_hide(false).unwrap();
    db.set_vault_touch_id(true).unwrap();
    let s = db.settings().unwrap();
    assert_eq!(
        (s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id),
        (15, false, true)
    );
}

#[test]
fn hiding_from_capture_is_on_until_turned_off() {
    let db = db();
    assert!(db.settings().unwrap().hide_from_capture);
    db.set_hide_from_capture(false).unwrap();
    assert!(!db.settings().unwrap().hide_from_capture);
    db.set_hide_from_capture(true).unwrap();
    assert!(db.settings().unwrap().hide_from_capture);
}

#[test]
fn settings_are_saved() {
    let db = db();
    db.set_hotkey("Alt+Space").unwrap();
    db.set_retention_days(30).unwrap();
    db.set_telegram_token("token").unwrap();
    db.set_telegram_chat_id("42").unwrap();
    db.set_theme(ThemePreference::Dark).unwrap();
    db.set_hide_on_blur(false).unwrap();
    let settings = db.settings().unwrap();
    assert_eq!(settings.hotkey, "Alt+Space");
    assert_eq!(settings.retention_days, 30);
    assert_eq!(settings.telegram_token, "token");
    assert_eq!(settings.telegram_chat_id, "42");
    assert_eq!(settings.theme, ThemePreference::Dark);
    assert!(!settings.hide_on_blur);
    assert!(settings.telegram_configured());
}
