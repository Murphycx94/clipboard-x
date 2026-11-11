//! The keyboard in forms, headless on macOS: Tab moves between fields,
//! Enter goes to the next one or submits, ⌘↩ submits from a multi-line
//! field; the lists keep Tab, Enter and the arrows for themselves.

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV};
use clipboardx::vault::keychain::NO_KEYCHAIN_ENV;
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

mod common;
use common::{shown_phrase, wait};

/// Typed key by key, so letters only.
const MASTER: &str = "keyboardmaster";

fn start() -> Headless {
    let mut config = EngineConfig::new(clipboardx::root());
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    config.viewport = Size {
        width: Points(420.0),
        height: Points(700.0),
    };
    let mut app = Headless::start(config).expect("engine starts");
    app.first_render().expect("first render");
    app
}

/// Types `text` key by key into whichever field has the focus.
fn type_keys(app: &mut Headless, text: &str) {
    for c in text.chars() {
        app.press_key(&c.to_string()).unwrap();
    }
}

fn input(app: &Headless, selector: &str) -> String {
    app.screen()
        .input_text(selector)
        .unwrap()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn tab_and_enter_work_in_forms_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_CAPTURE_ENV, "1");
        std::env::set_var(NO_KEYCHAIN_ENV, "1");
    }
    let mut app = start();

    // The history list keeps Tab: it switches to the favorites.
    wait(&mut app, "今日暂无复制记录");
    app.press_key("tab").unwrap();
    wait(&mut app, "暂无收藏，悬停条目点击 ☆ 添加");
    app.press_key("tab").unwrap();
    wait(&mut app, "今日暂无复制记录");

    // Creating the vault: Tab to the confirmation, Enter to the next step.
    app.press("label:密码本").unwrap();
    wait(&mut app, "创建密码本");
    app.type_text("id:vault-new", MASTER).unwrap();
    app.press_key("tab").unwrap();
    type_keys(&mut app, MASTER);
    assert_eq!(input(&app, "id:vault-confirm"), MASTER);
    app.press_key("enter").unwrap();
    wait(&mut app, "抄下助记词");
    let words = shown_phrase(&app, "抄下助记词");
    app.press("我已抄写，下一步").unwrap();
    wait(&mut app, "确认助记词");

    // Enter in a check field goes to the next one.
    let asked: Vec<usize> = app
        .screen()
        .texts()
        .iter()
        .filter_map(|t| t.strip_prefix("第 ")?.strip_suffix(" 个字")?.parse().ok())
        .collect();
    app.type_text("id:recovery-check-0", &words[asked[0] - 1])
        .unwrap();
    app.press_key("enter").unwrap();
    type_keys(&mut app, "x");
    assert_eq!(input(&app, "id:recovery-check-1"), "x");
    app.clear("id:recovery-check-1").unwrap();
    app.type_text("id:recovery-check-1", &words[asked[1] - 1])
        .unwrap();
    app.type_text("id:recovery-check-2", &words[asked[2] - 1])
        .unwrap();
    // Enter in the last creates the vault.
    app.press_key("enter").unwrap();
    wait(&mut app, "还没有保存任何账号");

    // The editor: Tab from the name to the account, Enter saves.
    app.press("新建").unwrap();
    wait(&mut app, "新建账号");
    app.type_text("id:vault-title", "Example").unwrap();
    app.press_key("tab").unwrap();
    type_keys(&mut app, "octo");
    assert_eq!(input(&app, "id:vault-username"), "octo");
    app.press_key("enter").unwrap();
    wait(&mut app, "Example");
    assert!(!app.shows("新建账号"));

    // A multi-line note: Enter is a new line, ⌘↩ saves.
    app.press("新建").unwrap();
    wait(&mut app, "新建账号");
    app.type_text("id:vault-title", "Notes").unwrap();
    app.type_text("id:vault-note", "first").unwrap();
    app.press_key("enter").unwrap();
    assert!(app.shows("新建账号"));
    app.press_key("mod+enter").unwrap();
    wait(&mut app, "Notes");
    assert!(!app.shows("新建账号"));
}
