//! The app on a phone, headless as Android: the clipboard is recorded when
//! the app comes to the foreground, the desktop's shortcuts and window
//! controls are gone, and a long press on an item opens its actions.

use clipboardx::services::DATA_DIR_ENV;
use clipboardx::vault::keychain::NO_KEYCHAIN_ENV;
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Lifecycle, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

mod common;
use common::wait;

fn start_phone() -> Headless {
    let mut config = EngineConfig::new(clipboardx::root());
    config.environment.os = Os::Android;
    config.environment.idiom = Idiom::Phone;
    config.viewport = Size {
        width: Points(412.0),
        height: Points(900.0),
    };
    let mut app = Headless::start(config).expect("engine starts");
    app.first_render().expect("first render");
    app
}

/// The user switches to another app, copies `text` there and comes back.
fn copy_elsewhere(app: &mut Headless, text: &str) {
    app.set_environment("to the background", |e| {
        e.lifecycle = Lifecycle::Background;
    })
    .unwrap();
    app.set_clipboard(Some(text.to_owned()));
    app.set_environment("back to the foreground", |e| {
        e.lifecycle = Lifecycle::Active;
    })
    .unwrap();
}

#[test]
fn a_phone_records_on_return_and_acts_on_long_press() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them. The
    // clipboard is the headless one: capture stays on.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_KEYCHAIN_ENV, "1");
    }
    let mut app = start_phone();

    // Comes forward: what was copied elsewhere is recorded.
    copy_elsewhere(&mut app, "copied in another app");
    wait(&mut app, "copied in another app");
    // The same clipboard again is not a new item.
    copy_elsewhere(&mut app, "copied in another app");
    app.settle();
    let count = app
        .screen()
        .texts()
        .iter()
        .filter(|t| **t == "copied in another app")
        .count();
    assert_eq!(count, 1, "{:?}", app.screen().texts());

    // No desktop parts: shortcuts, the segmented tabs (the system tab bar
    // switches pages), the window's close button.
    let texts = app.screen().texts();
    assert!(!texts.iter().any(|t| t.contains("Ctrl+Shift")), "{texts:?}");
    assert!(!app.screen().shows("今日历史"), "{texts:?}");
    assert!(app.press("label:关闭 (Esc)").is_err());

    // A long press opens the item's actions: favorite it, then delete it.
    app.long_press("copied in another app").unwrap();
    wait(&mut app, "收藏");
    app.press("收藏").unwrap();
    app.long_press("copied in another app").unwrap();
    wait(&mut app, "取消收藏");
    app.press("删除").unwrap();
    app.wait_for_gone("copied in another app", common::SLOW)
        .unwrap();
}
