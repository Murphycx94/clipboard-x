//! Creating and editing an entry, with the password generator.

use nsh_base::{button, theme};
use nsh_core::prelude::*;

use crate::services::services;
use crate::vault::VaultEntry;
use crate::vault::generator::{GeneratorOptions, generate};

use super::super::icons::Icon;
use super::super::state::{AppState, DialogKind};
use super::super::widgets::{
    Tone, field, icon_button, modal, muted, segmented, submit_with_mod_enter, text_button,
};

const LENGTHS: [(usize, &str); 4] = [(12, "12 位"), (16, "16 位"), (20, "20 位"), (32, "32 位")];

/// The entry's fields while it is edited.
#[derive(Clone, Copy)]
struct Draft {
    title: Signal<String>,
    username: Signal<String>,
    password: Signal<String>,
    url: Signal<String>,
    totp: Signal<String>,
    note: Signal<String>,
    created_at: Signal<String>,
}

impl Draft {
    fn of(entry: &VaultEntry) -> Draft {
        Draft {
            title: signal(entry.title.clone()),
            username: signal(entry.username.clone()),
            password: signal(entry.password.clone()),
            url: signal(entry.url.clone()),
            totp: signal(entry.totp.clone()),
            note: signal(entry.note.clone()),
            created_at: signal(entry.created_at.clone()),
        }
    }

    fn entry(self) -> VaultEntry {
        let mut entry = VaultEntry::default();
        entry.title = self.title.get().trim().to_owned();
        entry.username = self.username.get().trim().to_owned();
        entry.password = self.password.get();
        entry.url = self.url.get().trim().to_owned();
        entry.totp = self.totp.get().trim().to_owned();
        entry.note = self.note.get();
        entry.created_at = self.created_at.get();
        entry
    }
}

/// A web address the browser can open: `https://` added when it has no scheme.
fn openable(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Some(url.to_owned())
    } else if url.contains("://") {
        None
    } else {
        Some(format!("https://{url}"))
    }
}

pub fn editor_dialog(state: AppState) -> impl IntoElement {
    let id = untracked(move || state.vault_editing.get());
    let loaded = match &id {
        Some(id) => match services().vault().get(id) {
            Ok(entry) => entry,
            Err(e) => {
                state.error(e.to_string());
                VaultEntry::default()
            }
        },
        None => VaultEntry::default(),
    };
    let draft = Draft::of(&loaded);
    drop(loaded);
    let reveal = signal(id.is_none());
    let generating = signal(false);
    let options = signal(GeneratorOptions::default());
    let regenerate = move || match generate(&options.get()) {
        Ok(password) => {
            draft.password.set(password);
            reveal.set(true);
        }
        Err(_) => state.error("请至少选择一种字符"),
    };
    let existing = id.is_some();
    let save_id = signal(id);
    // Enter in a one-line field saves; ⌘↩ anywhere, the note too.
    let save = move || state.vault_save(save_id.get(), draft.entry());
    submit_with_mod_enter(save);
    let content = view()
        .gap(px(10.0))
        .child(labeled(
            "名称",
            text_field(draft.title, "vault-title", "例如 GitHub", save).auto_focus(!existing),
        ))
        .child(labeled(
            "账号",
            text_field(draft.username, "vault-username", "用户名或邮箱", save),
        ))
        .child(labeled(
            "密码",
            view()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .child(
                    text_field(draft.password, "vault-password-field", "密码", save)
                        .flex_1()
                        .masked(move || !reveal.get()),
                )
                .child(icon_button(
                    Icon::Eye,
                    "显示 / 隐藏",
                    muted,
                    move || reveal.update(|r| *r = !*r),
                ))
                .child(icon_button(
                    Icon::Dice,
                    "生成密码",
                    move || {
                        if generating.get() {
                            theme().primary
                        } else {
                            muted()
                        }
                    },
                    move || {
                        generating.update(|g| *g = !*g);
                        if generating.get() {
                            regenerate();
                        }
                    },
                )),
        ))
        .child(when(
            move || generating.get(),
            move || generator_panel(options, regenerate),
        ))
        .child(labeled(
            "网址",
            view()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .child(text_field(draft.url, "vault-url", "https://", save).flex_1())
                .child(when(
                    move || draft.url.with(|u| openable(u).is_some()),
                    move || {
                        icon_button(Icon::External, "在浏览器中打开", muted, move || {
                            if let Some(url) = draft.url.with(|u| openable(u)) {
                                drop(open_url(url));
                            }
                        })
                    },
                )),
        ))
        .child(labeled(
            "两步验证",
            text_field(
                draft.totp,
                "vault-totp",
                "otpauth:// 链接或 Base32 密钥",
                save,
            ),
        ))
        .child(labeled(
            "备注",
            field()
                .test_id("vault-note")
                .multiline(true)
                .h(px(64.0))
                .py(px(6.0))
                .value(move || draft.note.get())
                .on_change(move |t: &str| draft.note.set(t.to_owned()))
                .placeholder("可选"),
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .pt(px(6.0))
                .child(when(
                    move || existing,
                    move || {
                        text_button("删除", Tone::Danger, move || {
                            state.dialog.set(Some(DialogKind::VaultConfirmDelete))
                        })
                    },
                ))
                .child(view().flex_1())
                .child(text_button("取消", Tone::Plain, move || {
                    state.dialog.set(None)
                }))
                .child(
                    text_button("保存", Tone::Primary, save)
                        .disabled(move || state.vault_busy.get()),
                ),
        );
    modal(
        state,
        if existing {
            "编辑账号"
        } else {
            "新建账号"
        },
        360.0,
        content,
    )
}

fn labeled(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    view()
        .gap(px(4.0))
        .child(
            text(label)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted),
        )
        .child(control)
}

fn text_field(
    value: Signal<String>,
    id: &str,
    placeholder: &'static str,
    save: impl Fn() + 'static,
) -> Input {
    field()
        .test_id(id)
        .value(move || value.get())
        .on_change(move |t: &str| value.set(t.to_owned()))
        .on_submit(move |_: &str| save())
        .placeholder(placeholder)
}

fn generator_panel(
    options: Signal<GeneratorOptions>,
    regenerate: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    let set = move |f: fn(&mut GeneratorOptions)| {
        move || {
            options.update(f);
            regenerate();
        }
    };
    view()
        .gap(px(8.0))
        .p(px(10.0))
        .rounded(px(10.0))
        .bg(move || theme().muted)
        .child(segmented(
            LENGTHS.to_vec(),
            move || options.with(|o| o.length),
            move |length| {
                options.update(|o| o.length = length);
                regenerate();
            },
        ))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(chip(
                    "大写",
                    move || options.with(|o| o.upper),
                    set(|o| o.upper = !o.upper),
                ))
                .child(chip(
                    "小写",
                    move || options.with(|o| o.lower),
                    set(|o| o.lower = !o.lower),
                ))
                .child(chip(
                    "数字",
                    move || options.with(|o| o.digits),
                    set(|o| o.digits = !o.digits),
                ))
                .child(chip(
                    "符号",
                    move || options.with(|o| o.symbols),
                    set(|o| o.symbols = !o.symbols),
                ))
                .child(chip(
                    "排除易混淆",
                    move || options.with(|o| o.exclude_ambiguous),
                    set(|o| o.exclude_ambiguous = !o.exclude_ambiguous),
                )),
        )
        .child(text_button("重新生成", Tone::Plain, regenerate))
}

/// An on / off chip.
fn chip(
    label: &'static str,
    on: impl Fn() -> bool + Copy + 'static,
    toggle: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .on_press(toggle)
        .accessibility_label(label)
        .px(px(10.0))
        .h(px(24.0))
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .border_1()
        .border_color(move || {
            if on() {
                theme().primary
            } else {
                theme().border
            }
        })
        .bg(move || {
            if on() {
                theme().primary.opacity(0.12)
            } else {
                theme().background
            }
        })
        .child(
            text(label)
                .text_xs()
                .text_color(move || if on() { theme().primary } else { muted() }),
        )
}

#[cfg(test)]
mod tests {
    use super::openable;

    #[test]
    fn only_web_addresses_open() {
        assert_eq!(
            openable("github.com").as_deref(),
            Some("https://github.com")
        );
        assert_eq!(openable("http://x.test").as_deref(), Some("http://x.test"));
        assert_eq!(openable("javascript://alert(1)"), None);
        assert_eq!(openable("file:///etc/passwd"), None);
        assert_eq!(openable("  "), None);
    }
}
