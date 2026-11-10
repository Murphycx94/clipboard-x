//! Creating the vault and unlocking it.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::services::services;
use crate::vault::generator::strength;
use crate::vault::recovery::RecoveryKey;
use crate::vault::{MIN_PASSWORD_CHARS, VaultError};

use super::super::icons::{Icon, icon};
use super::super::state::{AppState, DialogKind};
use super::super::widgets::{Tone, field, muted, text_button};
use super::recovery::{
    CHECK_FAILED, CHECKED_WORDS, KEEP_IT, check_fields, checked_positions, checks_out, phrase_grid,
};

/// A column centered in the page, as wide as a form.
fn card(glyph: Icon, title: &'static str, note: &'static str) -> View {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .px(px(40.0))
        .gap(px(12.0))
        .child(
            view()
                .size(px(52.0))
                .rounded_full()
                .items_center()
                .justify_center()
                .bg(move || theme().primary.opacity(0.12))
                .child(icon(glyph, 24.0).text_color(move || theme().primary)),
        )
        .child(
            text(title)
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().foreground),
        )
        .child(text(note).text_xs().text_center().text_color(muted))
}

/// Creating the vault: the master password, then the recovery phrase,
/// then three of its words typed back. Nothing is saved before that.
pub fn setup_view(state: AppState) -> impl IntoElement {
    let password = signal(String::new());
    let confirm = signal(String::new());
    let step = signal(0_u8);
    let phrase = signal(None::<RecoveryKey>);
    let positions = signal(checked_positions());
    let typed = [(); CHECKED_WORDS].map(|()| signal(String::new()));
    let next = move || {
        if password.with(|p| p.chars().count() < MIN_PASSWORD_CHARS) {
            return state.error(VaultError::WeakPassword.to_string());
        }
        if password.get() != confirm.get() {
            return state.error("两次输入的密码不一致");
        }
        match RecoveryKey::generate() {
            Ok(key) => {
                phrase.set(Some(key));
                positions.set(checked_positions());
                typed.iter().for_each(|t| t.set(String::new()));
                step.set(1);
            }
            Err(e) => state.error(e.to_string()),
        }
    };
    let create = move || {
        let Some(key) = phrase.get() else {
            return step.set(0);
        };
        if !checks_out(&key, positions.get(), typed) {
            return state.error(CHECK_FAILED);
        }
        state.vault_setup_with_recovery(password.get(), key);
        password.set(String::new());
        confirm.set(String::new());
        phrase.set(None);
    };
    let level = move || password.with(|p| strength(p));
    let password_step = move || {
        card(
            Icon::Shield,
            "创建密码本",
            "账号和密码会用主密码加密后保存在本机。下一步会生成 24 字助记词，忘记主密码时凭它重置。",
        )
        .child(
            view()
                .w_full()
                .gap(px(8.0))
                .pt(px(6.0))
                .child(
                    field()
                        .test_id("vault-new")
                        .masked(true)
                        .value(move || password.get())
                        .on_change(move |t: &str| password.set(t.to_owned()))
                        .placeholder(format!("主密码（至少 {MIN_PASSWORD_CHARS} 个字符）"))
                        .auto_focus(true),
                )
                .child(
                    field()
                        .test_id("vault-confirm")
                        .masked(true)
                        .value(move || confirm.get())
                        .on_change(move |t: &str| confirm.set(t.to_owned()))
                        .on_submit(move |_: &str| next())
                        .placeholder("再次输入主密码"),
                )
                .child(strength_meter(level))
                .child(text_button("下一步", Tone::Primary, next)),
        )
    };
    let phrase_step = move || {
        card(Icon::Key, "抄下助记词", KEEP_IT).child(
            view()
                .w_full()
                .gap(px(10.0))
                .pt(px(6.0))
                .child(dynamic(move || {
                    phrase.with(|p| match p {
                        Some(p) => phrase_grid(p).into_element(),
                        None => view().into_element(),
                    })
                }))
                .child(steps(
                    text_button("上一步", Tone::Plain, move || step.set(0)),
                    text_button("我已抄写，下一步", Tone::Primary, move || {
                        step.set(2)
                    }),
                )),
        )
    };
    let check_step = move || {
        card(
            Icon::Check,
            "确认助记词",
            "按抄写的内容填出下面几个字，确认无误后创建密码本。",
        )
        .child(
            view()
                .w_full()
                .gap(px(10.0))
                .pt(px(6.0))
                .child(dynamic(move || check_fields(positions.get(), typed)))
                .child(steps(
                    text_button("上一步", Tone::Plain, move || step.set(1)),
                    text_button(
                        move || {
                            if state.vault_busy.get() {
                                "创建中…"
                            } else {
                                "创建密码本"
                            }
                        },
                        Tone::Primary,
                        create,
                    )
                    .disabled(move || state.vault_busy.get()),
                )),
        )
    };
    view()
        .flex_1()
        .child(when(move || step.get() == 0, password_step))
        .child(when(move || step.get() == 1, phrase_step))
        .child(when(move || step.get() == 2, check_step))
}

/// Back and forward, side by side.
fn steps(back: impl IntoElement, forward: impl IntoElement) -> impl IntoElement {
    view()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .child(back)
        .child(forward)
}

/// Four bars and a word.
fn strength_meter(level: impl Fn() -> u8 + Copy + 'static) -> impl IntoElement {
    let color = move || match level() {
        0 => theme().destructive,
        1 => theme().color("star").unwrap_or(theme().primary),
        _ => theme().color("success").unwrap_or(theme().primary),
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(
            (1..=3_u8)
                .map(|bar| {
                    view().flex_1().h(px(3.0)).rounded_full().bg(move || {
                        if level() >= bar {
                            color()
                        } else {
                            theme().muted
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
        .child(
            text(move || match level() {
                0 => "太短",
                1 => "弱",
                2 => "中",
                _ => "强",
            })
            .w(px(28.0))
            .text_xs()
            .text_right()
            .text_color(muted),
        )
}

pub fn unlock_view(state: AppState) -> impl IntoElement {
    let password = signal(String::new());
    // A device that joined sync needs the secret key once.
    let needs_key = untracked(|| services().vault().needs_account_key());
    let account_key = signal(String::new());
    let unlock = move || {
        state.vault_unlock(password.get(), needs_key.then(|| account_key.get()));
        password.set(String::new());
    };
    let touch_id = move || {
        state.settings.with(|s| s.vault_touch_id) && services().vault().touch_id_available()
    };
    let staged = untracked(|| services().vault().has_staged_header());
    let has_phrase = untracked(|| services().vault().has_recovery_phrase());
    let note = if needs_key {
        "此设备首次解锁：输入主密码和秘密密钥"
    } else if staged {
        "其他设备修改了主密码或助记词，请输入当前的主密码"
    } else {
        "输入主密码解锁"
    };
    card(Icon::Lock, "密码本已锁定", note).child(
        view()
            .w_full()
            .gap(px(8.0))
            .pt(px(6.0))
            .child(when(
                move || needs_key,
                move || {
                    field()
                        .test_id("vault-secret-key")
                        .value(move || account_key.get())
                        .on_change(move |t: &str| account_key.set(t.to_owned()))
                        .placeholder("秘密密钥 CX-XXXXX-…")
                },
            ))
            .child(
                field()
                    .test_id("vault-password")
                    .masked(true)
                    .value(move || password.get())
                    .on_change(move |t: &str| password.set(t.to_owned()))
                    .on_submit(move |_: &str| unlock())
                    .placeholder("主密码")
                    .auto_focus(true),
            )
            .child(
                text_button(
                    move || {
                        if state.vault_busy.get() {
                            "验证中…"
                        } else {
                            "解锁"
                        }
                    },
                    Tone::Primary,
                    unlock,
                )
                .disabled(move || state.vault_busy.get()),
            )
            .child(when(touch_id, move || {
                text_button("使用 Touch ID", Tone::Plain, move || {
                    state.vault_unlock_touch_id()
                })
                .disabled(move || state.vault_busy.get())
            }))
            .child(when(
                move || has_phrase,
                move || {
                    text_button("忘记主密码？", Tone::Plain, move || {
                        state.dialog.set(Some(DialogKind::VaultRecover))
                    })
                },
            )),
    )
}
