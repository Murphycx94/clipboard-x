//! What the headless UI tests share: waiting, and the recovery phrase steps.

#![allow(dead_code)]

use std::time::Duration;

use nsh_testing::Headless;

/// Argon2id with 64 MiB takes a moment.
pub const SLOW: Duration = Duration::from_secs(30);

pub fn wait(app: &mut Headless, text: &str) {
    app.wait_for_within(text, SLOW)
        .unwrap_or_else(|e| panic!("{text:?} not shown: {e}\n{:?}", app.screen().texts()));
}

/// The 24 words on screen after `anchor`: each number from 1 to 24
/// followed by its word.
pub fn shown_phrase(app: &Headless, anchor: &str) -> Vec<String> {
    let texts = app.screen().texts();
    let start = texts
        .iter()
        .position(|t| *t == anchor)
        .unwrap_or_else(|| panic!("{anchor:?} not on screen: {texts:?}"));
    let mut words = Vec::new();
    let mut i = start;
    for n in 1..=24 {
        i += texts[i..].iter().position(|t| *t == n.to_string()).unwrap();
        words.push(texts[i + 1].to_string());
        i += 1;
    }
    assert!(words.iter().all(|w| w.chars().count() == 1), "{words:?}");
    words
}

/// Types the asked words ("第 N 个字") into the check fields; `wrong` types
/// another word in the first.
pub fn fill_checks(app: &mut Headless, words: &[String], wrong: bool) {
    let asked: Vec<usize> = app
        .screen()
        .texts()
        .iter()
        .filter_map(|t| t.strip_prefix("第 ")?.strip_suffix(" 个字")?.parse().ok())
        .collect();
    assert_eq!(asked.len(), 3, "{:?}", app.screen().texts());
    for (k, n) in asked.into_iter().enumerate() {
        let word = if wrong && k == 0 {
            words.iter().find(|w| **w != words[n - 1]).unwrap()
        } else {
            &words[n - 1]
        };
        let field = format!("id:recovery-check-{k}");
        app.clear(&field).unwrap();
        app.type_text(&field, word).unwrap();
    }
}

/// Creates the vault from its page with `master`; the phrase it showed.
pub fn create_vault(app: &mut Headless, master: &str) -> Vec<String> {
    wait(app, "创建密码本");
    app.type_text("id:vault-new", master).unwrap();
    app.type_text("id:vault-confirm", master).unwrap();
    app.press("下一步").unwrap();
    wait(app, "抄下助记词");
    let words = shown_phrase(app, "抄下助记词");
    app.press("我已抄写，下一步").unwrap();
    wait(app, "确认助记词");
    fill_checks(app, &words, false);
    app.press("创建密码本").unwrap();
    // The empty list, not the toast: the toast may be gone by the time a
    // slow machine looks.
    wait(app, "还没有保存任何账号");
    words
}
