//! The archive's list, headless: a day of hundreds of items renders only
//! the rows around the viewport, and scrolling brings the later ones.
//! A binary of its own, as the services are a process-wide singleton.

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV, services};
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

const OLD_ITEMS: usize = 300;

/// `old 000` (newest) to `old 299`, all on 2020-01-01.
fn seed() {
    let db = services().db();
    for i in 0..OLD_ITEMS {
        let at = format!("2020-01-01T{:02}:{:02}:00", 23 - i / 60, 59 - i % 60);
        db.insert_text_at(&format!("old {i:03}"), &format!("h-old-{i}"), &at)
            .unwrap();
    }
    db.insert_text_at("older entry", "h-older", "2019-12-31T08:00:00")
        .unwrap();
    db.set_retention_days(0).unwrap();
}

fn start() -> Headless {
    let mut config = EngineConfig::new(clipboardx::root());
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    config.viewport = Size {
        width: Points(420.0),
        height: Points(600.0),
    };
    let mut app = Headless::start(config).expect("engine starts");
    app.first_render().expect("first render");
    app
}

/// How many of the old day's items have a view.
fn rendered_old_items(app: &Headless) -> usize {
    app.screen()
        .texts()
        .iter()
        .filter(|t| t.trim().starts_with("old "))
        .count()
}

#[test]
fn a_long_archived_day_renders_only_the_rows_near_the_viewport() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(clipboardx::updater::NO_UPDATE_CHECK_ENV, "1");
        std::env::set_var(NO_CAPTURE_ENV, "1");
    }
    seed();
    let mut app = start();

    app.press("label:历史归档").unwrap();
    app.wait_for("2020-01-01").unwrap();
    assert!(app.shows("300 条"));
    assert!(app.shows("2019-12-31"));
    assert_eq!(rendered_old_items(&app), 0);

    // Opened: the first rows, not all 300.
    app.press("2020-01-01").unwrap();
    app.wait_for("old 000").unwrap();
    let rendered = rendered_old_items(&app);
    assert!(
        (1..OLD_ITEMS / 4).contains(&rendered),
        "{rendered} of {OLD_ITEMS} rows rendered"
    );
    assert!(!app.shows("old 299"));
    assert!(!app.shows("2019-12-31"), "the next day is far below");

    // Scrolled far down: the later rows come, the first ones go.
    app.scroll("id:archive", Points(100.0 * 50.0)).unwrap();
    app.wait_for("old 100").unwrap();
    assert!(!app.shows("old 000"));
    assert!(rendered_old_items(&app) < OLD_ITEMS / 4);

    // Closed again: the next day comes back up.
    app.scroll("id:archive", Points(-100.0 * 50.0)).unwrap();
    app.wait_for("old 000").unwrap();
    app.press("2020-01-01").unwrap();
    app.wait_for("2019-12-31").unwrap();
    assert_eq!(rendered_old_items(&app), 0);
}
