//! The vault page, headless: set up, add an entry, lock, a wrong password,
//! unlock, search. Real key derivation; no keychain.

use std::time::Duration;

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV, services};
use clipboardx::vault::keychain::NO_KEYCHAIN_ENV;
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

/// Argon2id with 64 MiB takes a moment.
const SLOW: Duration = Duration::from_secs(30);
const MASTER: &str = "correct horse battery";

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

fn wait(app: &mut Headless, text: &str) {
    app.wait_for_within(text, SLOW)
        .unwrap_or_else(|e| panic!("{text:?} not shown: {e}\n{:?}", app.screen().texts()));
}

#[test]
fn vault_setup_entries_lock_and_unlock() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_CAPTURE_ENV, "1");
        std::env::set_var(NO_KEYCHAIN_ENV, "1");
    }
    let mut app = start();

    app.press("label:密码本").unwrap();
    wait(&mut app, "创建密码本");
    app.type_text("id:vault-new", MASTER).unwrap();
    app.type_text("id:vault-confirm", "something else").unwrap();
    app.press("创建").unwrap();
    wait(&mut app, "两次输入的密码不一致");
    app.type_text("id:vault-new", MASTER).unwrap();
    app.type_text("id:vault-confirm", MASTER).unwrap();
    app.press("创建").unwrap();
    wait(&mut app, "还没有保存任何账号");
    // The background setup is done once it says so.
    wait(&mut app, "密码本已创建");

    app.press("新建").unwrap();
    wait(&mut app, "新建账号");
    app.type_text("id:vault-title", "GitHub").unwrap();
    app.type_text("id:vault-username", "octo@example.com")
        .unwrap();
    app.type_text("id:vault-password-field", "hunter2-secret")
        .unwrap();
    app.type_text("id:vault-url", "https://www.github.com/login")
        .unwrap();
    app.press("保存").unwrap();
    wait(&mut app, "octo@example.com · github.com");
    assert!(app.shows("GitHub"));
    assert!(!app.shows("hunter2-secret"));

    app.type_text("id:vault-search", "nothing-matches").unwrap();
    wait(&mut app, "无匹配内容");
    app.clear("id:vault-search").unwrap();
    wait(&mut app, "GitHub");

    app.press("label:锁定").unwrap();
    wait(&mut app, "密码本已锁定");
    assert!(!app.shows("GitHub"));

    app.type_text("id:vault-password", "wrong password")
        .unwrap();
    app.press("解锁").unwrap();
    wait(&mut app, "密码错误");
    app.type_text("id:vault-password", MASTER).unwrap();
    app.press("解锁").unwrap();
    wait(&mut app, "GitHub");

    // Edit, then delete.
    app.press("GitHub").unwrap();
    wait(&mut app, "编辑账号");
    assert_eq!(
        app.screen().input_text("id:vault-password-field").unwrap(),
        Some("hunter2-secret")
    );
    app.press("删除").unwrap();
    wait(&mut app, "删除后无法恢复。");
    app.press("删除").unwrap();
    wait(&mut app, "还没有保存任何账号");

    // Nothing on disk in the clear.
    let db = std::fs::read(dir.path().join("clipboardx.db")).unwrap();
    let wal = std::fs::read(dir.path().join("clipboardx.db-wal")).unwrap_or_default();
    for bytes in [db, wal] {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("hunter2-secret") && !text.contains("octo@example.com"));
    }
    assert_eq!(
        services().vault().status(),
        clipboardx::vault::VaultStatus::Unlocked
    );
}
