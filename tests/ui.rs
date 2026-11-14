//! The real UI, headless: NSH's engine with a desktop environment, on a
//! database in a temporary directory. One scenario, as the services are a
//! process-wide singleton.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV, WindowHost, services};
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

const WAIT: Duration = Duration::from_secs(5);

/// A host's window: what it was told about screen captures.
#[derive(Default, Clone)]
struct RecordingHost {
    capture_hidden: Arc<Mutex<Vec<bool>>>,
}

impl WindowHost for RecordingHost {
    fn hide(&self) {}
    fn set_hotkey(&self, _hotkey: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_hidden_from_capture(&self, hidden: bool) {
        self.capture_hidden.lock().unwrap().push(hidden);
    }
}

fn seed() {
    let db = services().db();
    db.insert_text("alpha text", "h-alpha").unwrap();
    db.insert_text("beta text", "h-beta").unwrap();
    let beta = db
        .today_items()
        .unwrap()
        .into_iter()
        .find(|i| i.text_preview.as_deref() == Some("beta text"))
        .unwrap();
    db.toggle_favorite(beta.id, "my note").unwrap();
    db.insert_text_at("old entry", "h-old", "2020-01-01T08:00:00")
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

fn wait_gone(app: &mut Headless, text: &str) {
    app.wait_for_gone(text, WAIT)
        .unwrap_or_else(|e| panic!("{text:?} still shown: {e}\n{:?}", app.screen().texts()));
}

#[test]
fn browsing_searching_and_clearing_history() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_CAPTURE_ENV, "1");
    }
    seed();
    let host = RecordingHost::default();
    services().set_host(Box::new(host.clone()));
    let mut app = start();

    // Today's history, newest first, with notes.
    app.wait_for("alpha text").unwrap();
    assert!(app.shows("beta text"));
    assert!(app.shows("my note"));
    assert!(!app.shows("old entry"));

    // Search filters by text.
    app.type_text("id:search", "alpha").unwrap();
    wait_gone(&mut app, "beta text");
    assert!(app.shows("alpha text"));
    app.clear("id:search").unwrap();
    app.wait_for("beta text").unwrap();

    // The favorites tab, by press and by Tab.
    app.press("收藏夹").unwrap();
    wait_gone(&mut app, "alpha text");
    assert!(app.shows("beta text"));
    app.press_key("tab").unwrap();
    app.wait_for("alpha text").unwrap();

    // Data written elsewhere (the clipboard monitor) shows up.
    services()
        .db()
        .insert_text("gamma text", "h-gamma")
        .unwrap();
    services().notify_changed();
    app.wait_for("gamma text").unwrap();

    // The archive, collapsed by day.
    app.press("label:历史归档").unwrap();
    app.wait_for("2020-01-01").unwrap();
    assert!(app.shows("1 条"));
    assert!(!app.shows("old entry"));
    app.press("2020-01-01").unwrap();
    app.wait_for("old entry").unwrap();

    // Escape goes back to the main page.
    app.press_key("escape").unwrap();
    app.wait_for("今日历史").unwrap();

    // Settings: clearing keeps the favorites.
    app.press("label:设置").unwrap();
    app.wait_for("全局唤出快捷键").unwrap();
    app.press("立即清除").unwrap();
    app.wait_for("将删除所有未收藏的历史记录，此操作不可撤销。")
        .unwrap();
    app.press("确认清除").unwrap();
    app.wait_for("已清除 3 条历史记录").unwrap();
    assert_eq!(services().db().today_items().unwrap().len(), 1);
    assert!(services().db().archive_groups().unwrap().is_empty());

    // A bad shortcut is refused with a message, a good one saved.
    app.clear("id:hotkey").unwrap();
    app.type_text("id:hotkey", "V").unwrap();
    app.press("保存").unwrap();
    app.wait_for("快捷键至少需要一个修饰键（Cmd/Ctrl/Shift/Alt）")
        .unwrap();
    app.clear("id:hotkey").unwrap();
    app.type_text("id:hotkey", "Alt+Space").unwrap();
    app.press("保存").unwrap();
    app.wait_for("快捷键已更新").unwrap();
    assert_eq!(services().db().settings().unwrap().hotkey, "Alt+Space");

    // Hidden from screen captures until the user lets them show the window.
    assert!(app.shows("防截屏录屏"));
    assert!(services().db().settings().unwrap().hide_from_capture);
    app.press("label:防截屏录屏").unwrap();
    app.settle();
    assert!(!services().db().settings().unwrap().hide_from_capture);
    assert_eq!(*host.capture_hidden.lock().unwrap(), vec![false]);
}
