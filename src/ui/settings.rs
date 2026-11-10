//! The settings page.

use nsh_base::{switch, theme};
use nsh_core::prelude::*;

use crate::model::{RETENTION_OPTIONS, ThemePreference, VAULT_LOCK_OPTIONS, hotkey_label};
use crate::services::services;
use crate::vault::VaultStatus;

use super::icons::{Icon, icon};
use super::state::{AppState, DialogKind, Page, VaultPurpose};
use super::widgets::{Tone, field, muted, segmented, text_button};

pub fn settings_page(state: AppState) -> impl IntoElement {
    scroll().flex_1().child(
        view()
            .px(px(14.0))
            .py(px(14.0))
            .gap(px(14.0))
            .child(section(
                "常规",
                vec![
                    hotkey_row(state).into_element(),
                    row(
                        Icon::Eye,
                        "失焦自动隐藏",
                        "切换到其他应用时自动收起窗口",
                        switch_control(
                            move || state.settings.get().hide_on_blur,
                            move |on| state.set_hide_on_blur(on),
                        ),
                    )
                    .into_element(),
                    row(
                        Icon::Contrast,
                        "外观主题",
                        "选择界面显示模式",
                        segmented(
                            ThemePreference::ALL
                                .iter()
                                .map(|t| (*t, t.label()))
                                .collect(),
                            move || state.settings.get().theme,
                            move |theme| state.set_theme(theme),
                        ),
                    )
                    .into_element(),
                ],
            ))
            .child(section(
                "数据",
                vec![
                    row(
                        Icon::Clock,
                        "历史保留时间",
                        "非收藏内容的自动清理周期",
                        segmented(
                            RETENTION_OPTIONS.to_vec(),
                            move || state.settings.get().retention_days,
                            move |days| state.set_retention(days),
                        ),
                    )
                    .into_element(),
                    row(
                        Icon::Send,
                        "Telegram Bot",
                        "收藏变化时同步到 Telegram",
                        text_button(
                            move || {
                                if state.settings.with(|s| s.telegram_configured()) {
                                    "已配置"
                                } else {
                                    "设置"
                                }
                            },
                            Tone::Plain,
                            move || state.dialog.set(Some(DialogKind::Telegram)),
                        ),
                    )
                    .into_element(),
                    row(
                        Icon::Trash,
                        "清除历史缓存",
                        "删除所有未收藏的历史记录",
                        text_button("立即清除", Tone::Danger, move || {
                            state.dialog.set(Some(DialogKind::ConfirmClear))
                        }),
                    )
                    .into_element(),
                ],
            ))
            .child(vault_section(state))
            .child(super::vault::cloud_section(state))
            .child(section(
                "关于",
                vec![
                    row(
                        Icon::Info,
                        "当前版本",
                        concat!("v", env!("CARGO_PKG_VERSION")),
                        text_button(
                            move || {
                                if state.checking_update.get() {
                                    "检查中…"
                                } else {
                                    "检查更新"
                                }
                            },
                            Tone::Plain,
                            move || state.check_update(),
                        )
                        .disabled(move || state.checking_update.get()),
                    )
                    .into_element(),
                ],
            )),
    )
}

/// The vault's settings; a link to the vault page until it is set up.
fn vault_section(state: AppState) -> impl IntoElement {
    let set_up = move || state.vault_status.get() != VaultStatus::NotSetUp;
    when(set_up, move || {
        let mut rows = vec![
            row(
                Icon::Clock,
                "自动锁定",
                "无操作多久后锁定密码本",
                segmented(
                    VAULT_LOCK_OPTIONS.to_vec(),
                    move || state.settings.get().vault_lock_minutes,
                    move |minutes| state.set_vault_lock_minutes(minutes),
                ),
            )
            .into_element(),
            row(
                Icon::EyeOff,
                "收起面板时锁定",
                "系统睡眠或锁屏时总会锁定",
                switch_control(
                    move || state.settings.get().vault_lock_on_hide,
                    move |on| state.set_vault_lock_on_hide(on),
                ),
            )
            .into_element(),
        ];
        if services().vault().touch_id_available() {
            rows.push(
                row(
                    Icon::Shield,
                    "Touch ID 解锁",
                    "用 Touch ID 或系统密码代替主密码",
                    switch_control(
                        move || state.settings.get().vault_touch_id,
                        move |on| {
                            if on {
                                state.open_vault_password(VaultPurpose::EnableTouchId);
                            } else {
                                state.vault_disable_touch_id();
                            }
                        },
                    ),
                )
                .into_element(),
            );
        }
        rows.extend([
            row(
                Icon::Key,
                "修改主密码",
                "需要输入当前主密码",
                text_button("修改", Tone::Plain, move || {
                    state.open_vault_password(VaultPurpose::ChangePassword)
                }),
            )
            .into_element(),
            dynamic(move || {
                // Read again when a dialog closes (one may have set it).
                let _ = state.dialog.get();
                let has = services().vault().has_recovery_phrase();
                row(
                    Icon::Shield,
                    "恢复助记词",
                    if has {
                        "已设置：忘记主密码时可用它重置"
                    } else {
                        "未设置：忘记主密码将无法找回密码本"
                    },
                    text_button(
                        if has { "重新生成" } else { "生成" },
                        Tone::Plain,
                        move || state.dialog.set(Some(DialogKind::VaultRecoveryPhrase)),
                    ),
                )
                .into_element()
            })
            .into_element(),
            row(
                Icon::Archive,
                "加密备份",
                "导出为用备份密码加密的 .cxvault 文件，或从中导入（需先解锁）",
                view()
                    .flex_row()
                    .gap(px(6.0))
                    .child(text_button("导出", Tone::Plain, move || {
                        if state.vault_status.get() == VaultStatus::Unlocked {
                            state.open_vault_password(VaultPurpose::Export);
                        } else {
                            state.error("请先解锁密码本");
                        }
                    }))
                    .child(text_button("导入", Tone::Plain, move || {
                        state.vault_pick_import()
                    })),
            )
            .into_element(),
            row(
                Icon::Trash,
                "重置密码本",
                "删除全部账号和主密码",
                text_button("重置", Tone::Danger, move || {
                    state.dialog.set(Some(DialogKind::VaultReset))
                }),
            )
            .into_element(),
        ]);
        section("密码本", rows)
    })
    .otherwise(move || {
        section(
            "密码本",
            vec![
                row(
                    Icon::Key,
                    "密码本",
                    "尚未设置：用主密码加密保存账号和密码",
                    text_button("去设置", Tone::Plain, move || {
                        state.page.set(Page::Vault)
                    }),
                )
                .into_element(),
            ],
        )
    })
}

/// A titled group of rows on a card.
pub(super) fn section(title: &'static str, rows: Vec<AnyElement>) -> impl IntoElement {
    let count = rows.len();
    view()
        .gap(px(6.0))
        .child(
            text(title)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .uppercase()
                .text_color(muted)
                .px(px(4.0)),
        )
        .child(
            view()
                .rounded(px(12.0))
                .border_1()
                .border_color(move || theme().border)
                .bg(move || theme().card)
                .overflow_hidden()
                .child(
                    rows.into_iter()
                        .enumerate()
                        .map(|(i, row)| {
                            view()
                                .when(i + 1 < count, |v| {
                                    v.border_b_1().border_color(move || theme().border)
                                })
                                .child(row)
                        })
                        .collect::<Vec<_>>(),
                ),
        )
}

/// An icon, a title over a description, and a control on the right.
pub(super) fn row(
    glyph: Icon,
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(10.0))
        .child(
            view()
                .size(px(28.0))
                .rounded(px(7.0))
                .items_center()
                .justify_center()
                .bg(move || theme().muted)
                .child(icon(glyph, 15.0).text_color(move || theme().foreground)),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(1.0))
                .child(
                    text(title)
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme().card_foreground),
                )
                .child(
                    text(description)
                        .text_xs()
                        .number_of_lines(2)
                        .text_color(muted),
                ),
        )
        .child(control)
}

fn hotkey_row(state: AppState) -> impl IntoElement {
    let saved = move || state.settings.with(|s| s.hotkey.clone());
    let draft = signal(untracked(saved));
    // Follow the saved value when it changes elsewhere.
    effect(move || draft.set(saved()));
    let dirty = move || draft.with(|d| d.trim() != saved());
    view()
        .px(px(12.0))
        .py(px(10.0))
        .gap(px(8.0))
        .child(row_header(state))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .pl(px(40.0))
                .child(
                    field()
                        .test_id("hotkey")
                        .flex_1()
                        .value(move || draft.get())
                        .on_change(move |text: &str| draft.set(text.to_owned()))
                        .on_submit(move |_: &str| {
                            state.save_hotkey(&draft.get());
                        })
                        .placeholder("CmdOrCtrl+Shift+V"),
                )
                .child(
                    text_button("保存", Tone::Primary, move || {
                        state.save_hotkey(&draft.get());
                    })
                    .disabled(move || !dirty()),
                ),
        )
}

fn row_header(state: AppState) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .child(
            view()
                .size(px(28.0))
                .rounded(px(7.0))
                .items_center()
                .justify_center()
                .bg(move || theme().muted)
                .child(icon(Icon::Keyboard, 15.0).text_color(move || theme().foreground)),
        )
        .child(
            view()
                .flex_1()
                .gap(px(1.0))
                .child(
                    text("全局唤出快捷键")
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme().card_foreground),
                )
                .child(
                    text(move || {
                        format!(
                            "当前：{}　修饰键 CmdOrCtrl / Shift / Alt + 字母、数字、Space 或 F1–F12",
                            state.settings.with(|s| hotkey_label(&s.hotkey, apple_keys()))
                        )
                    })
                    .text_xs()
                    .number_of_lines(2)
                    .text_color(muted),
                ),
        )
}

fn switch_control(
    checked: impl Fn() -> bool + Copy + 'static,
    on_change: impl Fn(bool) + 'static,
) -> impl IntoElement {
    switch()
        .checked(checked)
        .on_change(move |next: &bool| on_change(*next))
        .track_style(|s| {
            s.w(px(38.0))
                .p(px(2.0))
                .rounded_full()
                .bg(move || theme().input)
                .transition_colors()
                .checked(|s| s.bg(move || theme().primary))
        })
        .thumb_style(|s| {
            s.size(px(18.0))
                .rounded_full()
                .bg(white())
                .shadow_sm()
                .transition_transform()
                .checked(|s| s.translate_x(px(16.0)))
        })
}
