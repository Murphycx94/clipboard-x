//! The vault page, headless: set up, add an entry, lock, a wrong password,
//! unlock, search. Real key derivation; no keychain.

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV, services};
use clipboardx::vault::keychain::NO_KEYCHAIN_ENV;
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

mod common;
use common::{SLOW, fill_checks, shown_phrase, wait};

const MASTER: &str = "correct horse battery";
const NEW_MASTER: &str = "a brand new master pass";

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
    app.press("下一步").unwrap();
    wait(&mut app, "两次输入的密码不一致");
    app.clear("id:vault-confirm").unwrap();
    app.type_text("id:vault-confirm", MASTER).unwrap();
    app.press("下一步").unwrap();
    wait(&mut app, "抄下助记词");
    let words = shown_phrase(&app, "抄下助记词");
    app.press("我已抄写，下一步").unwrap();
    wait(&mut app, "确认助记词");
    fill_checks(&mut app, &words, true);
    app.press("创建密码本").unwrap();
    wait(&mut app, "填写的字与助记词不一致，请对照抄写的内容检查");
    assert_eq!(
        services().vault().status(),
        clipboardx::vault::VaultStatus::NotSetUp
    );
    fill_checks(&mut app, &words, false);
    app.press("创建密码本").unwrap();
    wait(&mut app, "还没有保存任何账号");
    assert_eq!(
        services().vault().status(),
        clipboardx::vault::VaultStatus::Unlocked
    );

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

    // Forgotten: the phrase resets it, entries kept.
    app.press("忘记主密码？").unwrap();
    wait(&mut app, "用助记词重置主密码");
    app.type_text("id:recovery-phrase", &words[..23].concat())
        .unwrap();
    app.type_text("id:recovery-new", NEW_MASTER).unwrap();
    app.type_text("id:recovery-confirm", NEW_MASTER).unwrap();
    app.press("重置主密码").unwrap();
    wait(&mut app, "text:/^助记词无效/");
    app.clear("id:recovery-phrase").unwrap();
    app.type_text("id:recovery-phrase", &words.join(" "))
        .unwrap();
    app.press("重置主密码").unwrap();
    app.wait_for_gone("id:recovery-phrase", SLOW).unwrap();
    wait(&mut app, "GitHub");
    app.press("label:锁定").unwrap();
    wait(&mut app, "密码本已锁定");
    app.type_text("id:vault-password", NEW_MASTER).unwrap();
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

    // A new phrase from the settings, after three of its words and the
    // master password.
    app.press("label:设置").unwrap();
    wait(&mut app, "已设置：忘记主密码时可用它重置");
    app.press("重新生成").unwrap();
    wait(&mut app, "重新生成助记词");
    let new_words = shown_phrase(&app, "重新生成助记词");
    assert_ne!(new_words, words);
    app.press("我已抄写，下一步").unwrap();
    fill_checks(&mut app, &new_words, false);
    app.type_text("id:recovery-password", NEW_MASTER).unwrap();
    app.press("确认").unwrap();
    app.wait_for_gone("id:recovery-password", SLOW).unwrap();
    assert!(services().vault().has_recovery_phrase());
    let phrase = clipboardx::vault::recovery::RecoveryKey::parse(&new_words.concat()).unwrap();
    services().vault().lock();
    services().vault().recover(&phrase, MASTER).unwrap();

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
