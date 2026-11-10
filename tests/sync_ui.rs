//! Turning cloud sync on from the settings page, headless, against a real
//! sync server on a local port.

use clipboardx::services::{DATA_DIR_ENV, NO_CAPTURE_ENV, services};
use clipboardx::vault::keychain::NO_KEYCHAIN_ENV;
use clipboardx_sync_server::{Config, new_state, serve, store::Store};
use nsh_core::geometry::{Points, Size};
use nsh_core::platform::{Idiom, Os};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

mod common;
use common::wait;

const MASTER: &str = "correct horse battery";

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

fn start_app() -> Headless {
    let mut config = EngineConfig::new(clipboardx::root());
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    config.viewport = Size {
        width: Points(420.0),
        height: Points(900.0),
    };
    let mut app = Headless::start(config).expect("engine starts");
    app.first_render().expect("first render");
    app
}

#[test]
fn sync_is_turned_on_from_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set before any thread of this test binary reads them.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_CAPTURE_ENV, "1");
        std::env::set_var(NO_KEYCHAIN_ENV, "1");
    }
    let (url, admin) = start_server();
    let mut app = start_app();

    app.press("label:密码本").unwrap();
    common::create_vault(&mut app, MASTER);

    app.press("label:设置").unwrap();
    wait(&mut app, "端到端加密同步到自建服务器，服务器拿不到任何明文");
    app.press("开启").unwrap();
    wait(&mut app, "开启云同步");
    app.type_text("id:sync-url", &url).unwrap();
    app.type_text("id:sync-code", &admin).unwrap();
    app.press("下一步").unwrap();
    wait(&mut app, "秘密密钥");

    let key = app
        .screen()
        .texts()
        .into_iter()
        .find(|t| t.starts_with("CX-"))
        .expect("the secret key is shown")
        .to_owned();
    let confirmation = key.split('-').nth(5).unwrap().to_owned();

    app.type_text("id:sync-confirm", "WRONG").unwrap();
    app.type_text("id:sync-password", MASTER).unwrap();
    app.press("开启同步").unwrap();
    wait(&mut app, "确认字符不对：请输入密钥中倒数第二组的 5 个字符");

    app.clear("id:sync-confirm").unwrap();
    app.type_text("id:sync-confirm", &confirmation.to_lowercase())
        .unwrap();
    app.type_text("id:sync-password", MASTER).unwrap();
    app.press("开启同步").unwrap();
    // The dialog closes once sync is on (its toast may be gone already).
    app.wait_for_gone("id:sync-password", common::SLOW).unwrap();
    wait(&mut app, &format!("text:{url}"));
    wait(&mut app, "text:/^上次同步 /");

    let state = services().vault().sync_state().unwrap().unwrap();
    assert_eq!(state.server_url, url);
    assert!(services().vault().uses_account_key());
    // The key shown is the one the vault keeps.
    let kept = services().vault().reveal_account_key(MASTER).unwrap();
    assert_eq!(*kept, key);
}
