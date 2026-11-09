//! The vault's dialogs: the editor, deleting, passwords and resetting.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::vault::ImportMode;

use super::super::state::{AppState, DialogKind, VaultPurpose};
use super::super::widgets::{Tone, field, modal, muted, segmented, text_button};
use super::editor::editor_dialog;

/// The text a reset must be confirmed with.
const RESET_WORD: &str = "重置";

pub fn vault_dialogs(state: AppState) -> impl IntoElement {
    let is = move |kind: DialogKind| move || state.dialog.get() == Some(kind);
    view()
        .child(when(is(DialogKind::VaultEditor), move || {
            editor_dialog(state)
        }))
        .child(when(is(DialogKind::VaultConfirmDelete), move || {
            delete_dialog(state)
        }))
        .child(when(is(DialogKind::VaultPassword), move || {
            password_dialog(state)
        }))
        .child(when(is(DialogKind::VaultReset), move || {
            reset_dialog(state)
        }))
}

fn footer(confirm: impl IntoElement, state: AppState) -> impl IntoElement {
    view()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .pt(px(14.0))
        .child(text_button("取消", Tone::Plain, move || {
            state.dialog.set(None)
        }))
        .child(confirm)
}

fn delete_dialog(state: AppState) -> impl IntoElement {
    let content = view()
        .child(
            text("删除后无法恢复。")
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(footer(
            text_button("删除", Tone::Danger, move || state.vault_delete()),
            state,
        ));
    modal(state, "删除这个账号？", 300.0, content)
}

fn secret_field(value: Signal<String>, id: &str, placeholder: &'static str) -> Input {
    field()
        .test_id(id)
        .masked(true)
        .value(move || value.get())
        .on_change(move |t: &str| value.set(t.to_owned()))
        .placeholder(placeholder)
}

fn password_dialog(state: AppState) -> impl IntoElement {
    let purpose = untracked(move || state.vault_purpose.get());
    let first = signal(String::new());
    let second = signal(String::new());
    let third = signal(String::new());
    let mode = signal(ImportMode::Merge);
    let busy = move || state.vault_busy.get();
    let submit = move || {
        let (a, b, c) = (first.get(), second.get(), third.get());
        match purpose {
            VaultPurpose::EnableTouchId => state.vault_enable_touch_id(a),
            VaultPurpose::ChangePassword => state.vault_change_password(a, b, c),
            VaultPurpose::Export => state.vault_export(a, b, c),
            VaultPurpose::Import => state.vault_import(a, mode.get()),
        }
    };
    let (title, note, confirm) = match purpose {
        VaultPurpose::EnableTouchId => {
            ("开启 Touch ID", "输入主密码以开启 Touch ID 解锁。", "开启")
        }
        VaultPurpose::ChangePassword => {
            ("修改主密码", "新主密码同样无法找回，请务必牢记。", "修改")
        }
        VaultPurpose::Export => (
            "导出加密备份",
            "先输入主密码确认身份，再设置备份密码（可与主密码相同）。导入时需要备份密码。",
            "导出",
        ),
        VaultPurpose::Import => ("导入备份", "输入导出时设置的备份密码。", "导入"),
    };
    let fields = view()
        .gap(px(8.0))
        .child(text(note).text_xs().text_color(muted))
        .child(match purpose {
            VaultPurpose::EnableTouchId => secret_field(first, "vault-dialog-1", "主密码")
                .on_submit(move |_: &str| submit())
                .auto_focus(true)
                .into_element(),
            VaultPurpose::ChangePassword => view()
                .gap(px(8.0))
                .child(secret_field(first, "vault-dialog-1", "当前主密码").auto_focus(true))
                .child(secret_field(second, "vault-dialog-2", "新主密码"))
                .child(secret_field(third, "vault-dialog-3", "再次输入新主密码"))
                .into_element(),
            VaultPurpose::Export => view()
                .gap(px(8.0))
                .child(secret_field(first, "vault-dialog-1", "主密码").auto_focus(true))
                .child(secret_field(second, "vault-dialog-2", "备份密码"))
                .child(secret_field(third, "vault-dialog-3", "再次输入备份密码"))
                .into_element(),
            VaultPurpose::Import => view()
                .gap(px(8.0))
                .child(secret_field(first, "vault-dialog-1", "备份密码").auto_focus(true))
                .child(segmented(
                    vec![(ImportMode::Merge, "合并"), (ImportMode::Replace, "替换")],
                    move || mode.get(),
                    move |m| mode.set(m),
                ))
                .child(
                    text(move || match mode.get() {
                        ImportMode::Merge => "保留现有条目；同一条目以较新的为准。",
                        ImportMode::Replace => "将删除密码本中现有的全部条目，换成备份中的内容。",
                    })
                    .text_xs()
                    .text_color(move || match mode.get() {
                        ImportMode::Merge => muted(),
                        ImportMode::Replace => theme().destructive,
                    }),
                )
                .into_element(),
        });
    let tone = move || {
        if purpose == VaultPurpose::Import && mode.get() == ImportMode::Replace {
            Tone::Danger
        } else {
            Tone::Primary
        }
    };
    let content = view().child(fields).child(footer(
        dynamic(move || {
            text_button(
                move || {
                    if busy() {
                        "处理中…"
                    } else if purpose == VaultPurpose::Import && mode.get() == ImportMode::Replace {
                        "确认替换"
                    } else {
                        confirm
                    }
                },
                tone(),
                submit,
            )
            .disabled(busy)
        }),
        state,
    ));
    modal(state, title, 320.0, content)
}

fn reset_dialog(state: AppState) -> impl IntoElement {
    let typed = signal(String::new());
    let content = view()
        .gap(px(8.0))
        .child(
            text("将永久删除密码本中的所有账号和主密码，无法恢复。")
                .text_sm()
                .text_color(move || theme().destructive),
        )
        .child(
            text(format!("请输入“{RESET_WORD}”以确认："))
                .text_xs()
                .text_color(muted),
        )
        .child(
            field()
                .test_id("vault-reset")
                .value(move || typed.get())
                .on_change(move |t: &str| typed.set(t.to_owned()))
                .auto_focus(true),
        )
        .child(footer(
            text_button("重置密码本", Tone::Danger, move || state.vault_reset())
                .disabled(move || typed.with(|t| t.trim() != RESET_WORD)),
            state,
        ));
    modal(state, "重置密码本", 320.0, content)
}
