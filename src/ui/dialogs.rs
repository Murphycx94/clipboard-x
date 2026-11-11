//! The dialogs over the window: one at a time, as `state.dialog` says.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::model::mask_token;

use super::icons::Icon;
use super::state::{AppState, DialogKind};
use super::widgets::{
    Tone, field, icon_button, modal, muted, submit_with_mod_enter, success_color, text_button,
};

pub fn dialogs(state: AppState) -> impl IntoElement {
    let is = move |kind: DialogKind| move || state.dialog.get() == Some(kind);
    view()
        .child(when(is(DialogKind::Note), move || note_dialog(state)))
        .child(when(is(DialogKind::Image), move || image_dialog(state)))
        .child(when(is(DialogKind::Telegram), move || {
            telegram_dialog(state)
        }))
        .child(when(is(DialogKind::Logs), move || logs_dialog(state)))
        .child(when(is(DialogKind::Update), move || update_dialog(state)))
        .child(when(is(DialogKind::ConfirmClear), move || {
            confirm_clear_dialog(state)
        }))
        .child(super::vault::vault_dialogs(state))
}

fn buttons(children: Vec<AnyElement>) -> impl IntoElement {
    view()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .pt(px(14.0))
        .child(children)
}

fn note_dialog(state: AppState) -> impl IntoElement {
    // Enter is a new line here: ⌘↩ saves.
    submit_with_mod_enter(move || state.save_note());
    let content = view()
        .child(
            field()
                .multiline(true)
                .h(px(96.0))
                .py(px(8.0))
                .value(move || state.note_draft.get())
                .on_change(move |text: &str| state.note_draft.set(text.to_owned()))
                .placeholder("输入备注内容…")
                .auto_focus(true),
        )
        .child(buttons(vec![
            text_button("取消", Tone::Plain, move || state.dialog.set(None)).into_element(),
            text_button("保存", Tone::Primary, move || state.save_note()).into_element(),
        ]));
    modal(state, "备注", 320.0, content)
}

fn image_dialog(state: AppState) -> impl IntoElement {
    let content = view()
        .h(px(380.0))
        .rounded(px(8.0))
        .bg(move || theme().muted)
        .overflow_hidden()
        .child(
            image(move || state.preview_src.get())
                .flex_1()
                .object_fit(ResizeMode::Contain),
        );
    modal(state, "图片预览", 392.0, content)
}

fn telegram_dialog(state: AppState) -> impl IntoElement {
    let token_draft = signal(String::new());
    let chat_draft = signal(untracked(move || state.settings.get().telegram_chat_id));
    let configured = move || state.settings.with(|s| s.telegram_configured());
    let save_token = move || {
        let token = token_draft.get();
        if token.trim().chars().count() > 8 {
            state.save_telegram_token(&token);
            token_draft.set(String::new());
        }
    };
    let save_chat = move || {
        let chat_id = chat_draft.get();
        if !chat_id.trim().is_empty() {
            state.save_telegram_chat_id(&chat_id);
        }
    };
    let label = |s: &'static str| {
        text(s)
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
    };
    let content = view()
        .gap(px(14.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(view().size(px(7.0)).rounded_full().bg(move || {
                    if configured() {
                        success_color()
                    } else {
                        theme().border
                    }
                }))
                .child(
                    text(move || {
                        if configured() {
                            "已配置，收藏变化时自动同步"
                        } else {
                            "未配置，填写下方信息后生效"
                        }
                    })
                    .text_xs()
                    .text_color(muted),
                ),
        )
        .child(
            view().gap(px(6.0)).child(label("Bot Token")).child(
                field()
                    .masked(true)
                    .value(move || token_draft.get())
                    .on_change(move |text: &str| token_draft.set(text.to_owned()))
                    .on_blur(save_token)
                    .on_submit(move |_: &str| save_token())
                    .placeholder(move || {
                        let current = state.settings.with(|s| mask_token(&s.telegram_token));
                        if current.is_empty() {
                            "输入 Bot Token，回车或失焦后保存".to_owned()
                        } else {
                            format!("{current}（输入新值以替换）")
                        }
                    }),
            ),
        )
        .child(
            view()
                .gap(px(6.0))
                .child(label("Chat ID（可通过 @userinfobot 获取）"))
                .child(
                    view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(
                            field()
                                .flex_1()
                                .value(move || chat_draft.get())
                                .on_change(move |text: &str| chat_draft.set(text.to_owned()))
                                .on_submit(move |_: &str| save_chat())
                                .placeholder("输入 Chat ID"),
                        )
                        .child(text_button("保存", Tone::Primary, save_chat)),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .pt(px(10.0))
                .border_t_1()
                .border_color(move || theme().border)
                .child(text_button("查看同步日志", Tone::Plain, move || {
                    state.show_logs()
                })),
        );
    modal(state, "配置 Telegram Bot", 340.0, content)
}

fn logs_dialog(state: AppState) -> impl IntoElement {
    let content = view()
        .h(px(380.0))
        .rounded(px(8.0))
        .bg(move || theme().muted)
        .child(
            scroll().flex_1().child(
                text(move || {
                    let logs = state.logs.get();
                    if logs.trim().is_empty() {
                        "暂无日志".to_owned()
                    } else {
                        logs
                    }
                })
                .p(px(10.0))
                .text_xs()
                .font_family("ui-monospace")
                .select_text()
                .text_color(move || theme().foreground),
            ),
        )
        .child(
            view()
                .absolute()
                .top(px(4.0))
                .right(px(4.0))
                .child(icon_button(Icon::Refresh, "刷新", muted, move || {
                    state.logs.set(crate::logger::contents())
                })),
        );
    modal(state, "同步日志", 380.0, content)
}

fn update_dialog(state: AppState) -> impl IntoElement {
    let release = move || state.release.get();
    let title = untracked(move || {
        release().map_or_else(
            || "发现新版本".to_owned(),
            |r| format!("发现新版本 v{}", r.version),
        )
    });
    let content = view()
        .child(
            scroll().max_h(px(260.0)).child(
                text(move || {
                    release()
                        .map(|r| r.notes)
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| "有新版本可用，是否前往下载？".to_owned())
                })
                .text_xs()
                .select_text()
                .text_color(move || theme().foreground),
            ),
        )
        .child(buttons(vec![
            text_button("稍后再说", Tone::Plain, move || state.dialog.set(None)).into_element(),
            text_button("前往下载", Tone::Primary, move || {
                if let Some(release) = release() {
                    drop(open_url(release.url));
                }
                state.dialog.set(None);
            })
            .into_element(),
        ]));
    modal(state, title, 340.0, content)
}

fn confirm_clear_dialog(state: AppState) -> impl IntoElement {
    let content = view()
        .child(
            text("将删除所有未收藏的历史记录，此操作不可撤销。")
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(buttons(vec![
            text_button("取消", Tone::Plain, move || state.dialog.set(None)).into_element(),
            text_button("确认清除", Tone::Danger, move || state.clear_history()).into_element(),
        ]));
    modal(state, "清除历史记录", 320.0, content)
}
