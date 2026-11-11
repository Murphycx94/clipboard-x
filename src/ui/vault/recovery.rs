//! The recovery phrase: shown once and confirmed when the vault is created
//! or a phrase is set from the settings; typed back to reset a forgotten
//! master password.

use nsh_base::theme;
use nsh_core::prelude::*;
use zeroize::Zeroizing;

use crate::services::services;
use crate::vault::recovery::{RecoveryKey, WORDS};
use crate::vault::{MIN_PASSWORD_CHARS, VaultError, crypto::random_bytes};

use super::super::state::AppState;
use super::super::widgets::{Tone, field, modal, muted, text_button};

/// Words the user types back to show they wrote the phrase down.
pub(super) const CHECKED_WORDS: usize = 3;
const WORDS_PER_ROW: usize = 6;

/// What to tell the user about keeping the phrase.
pub(super) const KEEP_IT: &str = "请用纸笔按顺序抄下这 24 个字，放在安全的地方。忘记主密码时，凭它可以重置主密码并取回全部账号。不要截图、拍照，也不要存进网盘或备忘录：任何人拿到它都能打开密码本。";

/// Three different positions, in order, to check.
pub(super) fn checked_positions() -> [usize; CHECKED_WORDS] {
    let mut picked: Vec<usize> = Vec::with_capacity(CHECKED_WORDS);
    let candidates = random_bytes::<32>()
        .map(|bytes| bytes.map(|b| usize::from(b) % WORDS).to_vec())
        .unwrap_or_default()
        .into_iter()
        // Without randomness (or with all of it alike), fixed spread ones.
        .chain([2, 11, 19, 0, 1, 3]);
    for position in candidates {
        if !picked.contains(&position) {
            picked.push(position);
        }
        if picked.len() == CHECKED_WORDS {
            break;
        }
    }
    picked.sort_unstable();
    [picked[0], picked[1], picked[2]]
}

/// The 24 words, numbered, six to a row.
pub(super) fn phrase_grid(phrase: &RecoveryKey) -> impl IntoElement {
    let words = phrase.words();
    view().gap(px(6.0)).child(
        words
            .chunks(WORDS_PER_ROW)
            .enumerate()
            .map(|(row, chunk)| {
                view().flex_row().gap(px(6.0)).child(
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(i, word)| word_cell(row * WORDS_PER_ROW + i + 1, word))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn word_cell(number: usize, word: &'static str) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .py(px(4.0))
        .rounded(px(7.0))
        .bg(move || theme().muted)
        .child(text(number.to_string()).text_xs().text_color(muted))
        .child(
            text(word)
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().foreground),
        )
}

/// A field per checked position, each labelled with its number. Enter goes
/// to the next field, and runs `after` in the last.
pub(super) fn check_fields(
    positions: [usize; CHECKED_WORDS],
    typed: [Signal<String>; CHECKED_WORDS],
    after: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    let refs = [(); CHECKED_WORDS].map(|()| element_ref());
    view().gap(px(6.0)).child(
        positions
            .iter()
            .zip(typed)
            .enumerate()
            .map(|(i, (position, value))| {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        text(format!("第 {} 个字", position + 1))
                            .w(px(64.0))
                            .text_xs()
                            .text_color(muted),
                    )
                    .child(
                        field()
                            .flex_1()
                            .test_id(&format!("recovery-check-{i}"))
                            .element_ref(&refs[i])
                            .value(move || value.get())
                            .on_change(move |t: &str| value.set(t.to_owned()))
                            .on_submit(move |_: &str| match refs.get(i + 1) {
                                Some(next) => next.focus(),
                                None => after(),
                            })
                            .auto_focus(i == 0),
                    )
            })
            .collect::<Vec<_>>(),
    )
}

/// Whether the typed words are the phrase's at those positions.
pub(super) fn checks_out(
    phrase: &RecoveryKey,
    positions: [usize; CHECKED_WORDS],
    typed: [Signal<String>; CHECKED_WORDS],
) -> bool {
    let words = phrase.words();
    positions
        .iter()
        .zip(typed)
        .all(|(&position, value)| value.with(|t| words.get(position) == Some(&t.trim())))
}

pub(super) const CHECK_FAILED: &str = "填写的字与助记词不一致，请对照抄写的内容检查";

impl AppState {
    /// Creates the vault once the user confirmed the phrase.
    pub fn vault_setup_with_recovery(self, password: String, phrase: RecoveryKey) {
        let password = Zeroizing::new(password);
        self.run_vault(
            move || services().vault().setup_with_recovery(&password, &phrase),
            move |()| self.success("密码本已创建"),
        );
    }

    /// Sets (or replaces) the phrase from the settings.
    fn vault_set_recovery(self, password: String, phrase: RecoveryKey) {
        let password = Zeroizing::new(password);
        self.run_vault(
            move || services().vault().set_recovery_phrase(&password, &phrase),
            move |()| {
                self.dialog.set(None);
                self.reload();
                services().sync_now();
                self.success("助记词已设置，请妥善保管抄写的纸张");
            },
        );
    }

    /// Resets the master password with the phrase as typed.
    fn vault_recover(self, phrase: String, password: String, confirm: String) {
        let phrase = Zeroizing::new(phrase);
        let Some(key) = RecoveryKey::parse(&phrase) else {
            return self.error(VaultError::InvalidRecoveryPhrase.to_string());
        };
        if password != confirm {
            return self.error("两次输入的新密码不一致");
        }
        let password = Zeroizing::new(password);
        self.run_vault(
            move || services().vault().recover(&key, &password),
            move |()| {
                self.dialog.set(None);
                services().sync_now();
                self.success("主密码已重置");
            },
        );
    }
}

fn footer(state: AppState, back: Option<Signal<u8>>, confirm: impl IntoElement) -> View {
    view()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .pt(px(12.0))
        .child(match back {
            Some(step) => text_button("上一步", Tone::Plain, move || step.set(0)).into_element(),
            None => text_button("取消", Tone::Plain, move || state.dialog.set(None)).into_element(),
        })
        .child(confirm)
}

/// From the settings: a new phrase, shown, then confirmed with three of its
/// words and the master password. The phrase lives as long as the dialog.
pub fn phrase_dialog(state: AppState) -> impl IntoElement {
    let replacing = untracked(|| services().vault().has_recovery_phrase());
    let phrase = match RecoveryKey::generate() {
        Ok(phrase) => signal(phrase),
        Err(e) => {
            state.error(e.to_string());
            state.dialog.set(None);
            return view().into_element();
        }
    };
    let step = signal(0_u8);
    let positions = checked_positions();
    let typed = [(); CHECKED_WORDS].map(|()| signal(String::new()));
    let password = signal(String::new());
    let password_field = element_ref();
    let busy = move || state.vault_busy.get();
    let show = move || {
        view()
            .gap(px(10.0))
            .child(when(
                move || replacing,
                || {
                    text("确认后会同时更换数据密钥：旧助记词打不开之后的内容，但仍能打开此前的备份和服务器快照。如果旧助记词可能已泄露，请同时修改重要账号的密码。其他已同步的设备需要重新输入主密码。")
                        .text_xs()
                        .text_color(move || theme().destructive)
                },
            ))
            .child(text(KEEP_IT).text_xs().text_color(muted))
            .child(phrase.with(|p| phrase_grid(p).into_element()))
            .child(footer(
                state,
                None,
                text_button("我已抄写，下一步", Tone::Primary, move || {
                    step.set(1)
                }),
            ))
    };
    let finish = move || {
        if phrase.with(|p| !checks_out(p, positions, typed)) {
            return state.error(CHECK_FAILED);
        }
        state.vault_set_recovery(password.get(), phrase.get());
        password.set(String::new());
    };
    let check = move || {
        view()
            .gap(px(10.0))
            .child(
                text("按抄写的内容填出下面几个字，再输入主密码。")
                    .text_xs()
                    .text_color(muted),
            )
            .child(check_fields(positions, typed, move || {
                password_field.focus()
            }))
            .child(
                field()
                    .test_id("recovery-password")
                    .element_ref(&password_field)
                    .masked(true)
                    .value(move || password.get())
                    .on_change(move |t: &str| password.set(t.to_owned()))
                    .on_submit(move |_: &str| finish())
                    .placeholder("主密码"),
            )
            .child(footer(
                state,
                Some(step),
                text_button(
                    move || if busy() { "处理中…" } else { "确认" },
                    Tone::Primary,
                    finish,
                )
                .disabled(busy),
            ))
    };
    let content = view()
        .child(when(move || step.get() == 0, show))
        .child(when(move || step.get() == 1, check));
    let title = if replacing {
        "重新生成助记词"
    } else {
        "生成助记词"
    };
    modal(state, title, 340.0, content).into_element()
}

/// From the unlock page: the phrase and a new master password.
pub fn recover_dialog(state: AppState) -> impl IntoElement {
    let phrase = signal(String::new());
    let password = signal(String::new());
    let confirm = signal(String::new());
    let new_field = element_ref();
    let confirm_field = element_ref();
    let busy = move || state.vault_busy.get();
    // The fields stay for another try; they go with the dialog.
    let submit = move || state.vault_recover(phrase.get(), password.get(), confirm.get());
    let content = view()
        .gap(px(8.0))
        .child(
            text("输入抄写的 24 个字（空格可有可无），再设置新的主密码。已保存的账号都会保留；其他同步设备需用新主密码解锁。")
                .text_xs()
                .text_color(muted),
        )
        .child(
            field()
                .test_id("recovery-phrase")
                .value(move || phrase.get())
                .on_change(move |t: &str| phrase.set(t.to_owned()))
                .on_submit(move |_: &str| new_field.focus())
                .placeholder("助记词")
                .auto_focus(true),
        )
        .child(
            field()
                .test_id("recovery-new")
                .element_ref(&new_field)
                .masked(true)
                .value(move || password.get())
                .on_change(move |t: &str| password.set(t.to_owned()))
                .on_submit(move |_: &str| confirm_field.focus())
                .placeholder(format!("新主密码（至少 {MIN_PASSWORD_CHARS} 个字符）")),
        )
        .child(
            field()
                .test_id("recovery-confirm")
                .element_ref(&confirm_field)
                .masked(true)
                .value(move || confirm.get())
                .on_change(move |t: &str| confirm.set(t.to_owned()))
                .on_submit(move |_: &str| submit())
                .placeholder("再次输入新主密码"),
        )
        .child(footer(
            state,
            None,
            text_button(
                move || if busy() { "处理中…" } else { "重置主密码" },
                Tone::Primary,
                submit,
            )
            .disabled(busy),
        ));
    modal(state, "用助记词重置主密码", 340.0, content)
}
