//! Creating the vault and unlocking it.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::services::services;
use crate::vault::MIN_PASSWORD_CHARS;
use crate::vault::generator::strength;

use super::super::icons::{Icon, icon};
use super::super::state::AppState;
use super::super::widgets::{Tone, field, muted, text_button};

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

pub fn setup_view(state: AppState) -> impl IntoElement {
    let password = signal(String::new());
    let confirm = signal(String::new());
    let create = move || {
        state.vault_setup(password.get(), confirm.get());
        password.set(String::new());
        confirm.set(String::new());
    };
    let level = move || password.with(|p| strength(p));
    card(
        Icon::Shield,
        "创建密码本",
        "账号和密码会用主密码加密后保存在本机。主密码无法找回，请务必牢记。",
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
                    .on_submit(move |_: &str| create())
                    .placeholder("再次输入主密码"),
            )
            .child(strength_meter(level))
            .child(
                text_button(
                    move || {
                        if state.vault_busy.get() {
                            "创建中…"
                        } else {
                            "创建"
                        }
                    },
                    Tone::Primary,
                    create,
                )
                .disabled(move || state.vault_busy.get()),
            ),
    )
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
    let unlock = move || {
        state.vault_unlock(password.get());
        password.set(String::new());
    };
    let touch_id = move || {
        state.settings.with(|s| s.vault_touch_id) && services().vault().touch_id_available()
    };
    card(Icon::Lock, "密码本已锁定", "输入主密码解锁").child(
        view()
            .w_full()
            .gap(px(8.0))
            .pt(px(6.0))
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
            })),
    )
}
