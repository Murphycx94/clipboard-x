//! The unlocked vault: search, entries, copy buttons and TOTP codes.

use nsh_base::{button, theme};
use nsh_core::prelude::*;

use crate::services::services;
use crate::vault::{EntrySummary, Field};

use super::super::icons::Icon;
use super::super::state::AppState;
use super::super::widgets::{Tone, empty_state, field, hint, icon_button, muted, text_button};

pub fn list_view(state: AppState) -> impl IntoElement {
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(toolbar(state))
        .child(entries(state))
}

fn toolbar(state: AppState) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            field()
                .test_id("vault-search")
                .flex_1()
                .value(move || state.vault_query.get())
                .on_change(move |q: &str| {
                    state.vault_query.set(q.to_owned());
                    state.vault_focused.set(0);
                    state.vault_touch();
                })
                .placeholder("搜索名称、账号或网址…")
                .auto_focus(true),
        )
        .child(text_button("新建", Tone::Primary, move || {
            state.open_vault_editor(None)
        }))
        .child(icon_button(Icon::Lock, "锁定", muted, move || {
            state.vault_lock()
        }))
}

fn entries(state: AppState) -> impl IntoElement {
    let empty = move || {
        if state.vault_items.with(Vec::is_empty) {
            "还没有保存任何账号".to_owned()
        } else {
            "无匹配内容".to_owned()
        }
    };
    virtual_list(
        move || state.vault_visible.with(Vec::len),
        move |at| {
            let index = computed(move || at.index());
            let item = computed(move || {
                state
                    .vault_visible
                    .with(|items| items.get(index.get()).cloned())
            });
            row(state, item, index)
        },
    )
    .keys(move || {
        state
            .vault_visible
            .with(|items| items.iter().map(|i| i.id.clone()).collect::<Vec<_>>())
    })
    .empty(empty_state(Icon::Key, empty))
    .estimated_item_size(px(56.0))
    .element_ref(&state.vault_list)
    .flex_1()
}

fn row(state: AppState, item: Memo<Option<EntrySummary>>, index: Memo<usize>) -> impl IntoElement {
    let id = move || item.with(|i| i.as_ref().map(|i| i.id.clone()).unwrap_or_default());
    let get = move |f: fn(&EntrySummary) -> String| {
        move || item.with(|i| i.as_ref().map(f).unwrap_or_default())
    };
    let corrupt = move || item.with(|i| i.as_ref().is_some_and(|i| i.corrupt));
    let has_totp = move || item.with(|i| i.as_ref().is_some_and(|i| i.has_totp));
    let focused = move || state.vault_focused.get() == index.get();
    let subtitle = move || {
        item.with(|i| match i {
            Some(i) if i.corrupt => "无法解密，可以删除".to_owned(),
            Some(i) => [i.username.as_str(), i.host.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            None => String::new(),
        })
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .min_h(px(54.0))
        .px(px(16.0))
        .py(px(8.0))
        .cursor_pointer()
        .bg(move || {
            if focused() {
                theme().accent
            } else {
                theme().background
            }
        })
        .transition_colors()
        .on_mouse_enter(move |_: &MouseEvent| state.vault_focused.set(index.get()))
        .on_press(move || {
            state.vault_editing.set(Some(id()));
            if corrupt() {
                state
                    .dialog
                    .set(Some(super::super::state::DialogKind::VaultConfirmDelete));
            } else {
                state.open_vault_editor(Some(id()));
            }
        })
        .child(
            view()
                .size(px(32.0))
                .rounded(px(8.0))
                .items_center()
                .justify_center()
                .bg(move || theme().muted)
                .child(
                    text(move || {
                        get(|i| {
                            i.title
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_default()
                        })()
                    })
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || theme().foreground),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(get(|i| i.title.clone()))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(1)
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(subtitle)
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(move || {
                            if corrupt() {
                                theme().destructive
                            } else {
                                muted()
                            }
                        }),
                ),
        )
        .child(when(has_totp, move || totp_pill(state, id)))
        .child(when(
            move || !corrupt(),
            move || {
                view()
                    .flex_row()
                    .gap(px(2.0))
                    .child(icon_button(Icon::User, "复制账号", muted, move || {
                        state.vault_copy(&id(), Field::Username)
                    }))
                    .child(icon_button(Icon::Key, "复制密码", muted, move || {
                        state.vault_copy(&id(), Field::Password)
                    }))
            },
        ))
}

/// The current code and its seconds left; a press copies it.
fn totp_pill(state: AppState, id: impl Fn() -> String + Copy + 'static) -> impl IntoElement {
    let code = move || {
        state.vault_tick.get();
        services().vault().totp_code(&id()).ok()
    };
    let label = move || match code() {
        Some(c) if c.code.len() == 6 => format!("{} {}", &c.code[..3], &c.code[3..]),
        Some(c) => c.code,
        None => "------".to_owned(),
    };
    let seconds = move || code().map_or(0, |c| c.remaining);
    view().tooltip(hint("复制验证码")).child(
        button()
            .on_press(move || state.vault_copy(&id(), Field::Totp))
            .accessibility_label("复制验证码")
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .h(px(26.0))
            .rounded(px(7.0))
            .cursor_pointer()
            .bg(move || theme().muted)
            .hover(|s| s.bg(move || theme().accent))
            .child(
                text(label)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .font_family("ui-monospace")
                    .text_color(move || theme().foreground),
            )
            .child(
                text(move || format!("{}s", seconds()))
                    .text_size(px(10.0))
                    .text_color(move || {
                        if seconds() <= 5 {
                            theme().destructive
                        } else {
                            muted()
                        }
                    }),
            ),
    )
}
