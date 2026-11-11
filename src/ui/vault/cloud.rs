//! Cloud sync in the UI: the settings group, setting up (the first device,
//! or one joining), the secret key, pairing codes and devices.

use nsh_base::theme;
use nsh_core::prelude::*;
use zeroize::Zeroizing;

use crate::cloud::{self, Client, api_base};
use crate::services::services;
use crate::vault::account_key::AccountKey;
use crate::vault::{VaultError, VaultStatus};

use super::super::icons::Icon;
use super::super::settings::{row, section};
use super::super::state::{AppState, DialogKind, Page, SyncForm, SyncMode, VaultPurpose};
use super::super::widgets::{Tone, field, modal, muted, text_button};

const DEFAULT_DEVICE_NAME: &str = "我的 Mac";

fn no_client() -> VaultError {
    VaultError::Sync("未开启同步".into())
}

impl AppState {
    /// Reads sync's state again.
    pub fn refresh_sync(self) {
        let state = services().vault().sync_state().ok().flatten();
        self.sync_on.set(state.is_some());
        self.sync_server
            .set(state.map(|s| s.server_url).unwrap_or_default());
        self.sync_status.set(services().sync_status());
    }

    pub fn open_sync_setup(self, mode: SyncMode) {
        self.sync_mode.set(mode);
        self.dialog.set(Some(DialogKind::SyncSetup));
    }

    /// The setup form's first step.
    pub fn sync_setup_next(self, form: SyncForm) {
        if let Err(e) = api_base(&form.url) {
            return self.error(e.to_string());
        }
        if form.code.trim().is_empty() || form.device_name.trim().is_empty() {
            return self.error("请填写完整");
        }
        let status = services().vault().status();
        match self.sync_mode.get() {
            SyncMode::First if status == VaultStatus::NotSetUp => {
                self.error("请先创建密码本");
            }
            // The vault already has a secret key (sync was on before).
            SyncMode::First if services().vault().uses_account_key() => {
                self.sync_connect_first(form, None);
            }
            SyncMode::First => match AccountKey::generate() {
                Ok(key) => {
                    self.sync_form.set(form);
                    self.sync_secret.set(key.encode().to_string());
                    self.sync_secret_reveal.set(false);
                    self.dialog.set(Some(DialogKind::SyncSecretKey));
                }
                Err(e) => self.error(e.to_string()),
            },
            SyncMode::Join if status != VaultStatus::NotSetUp => {
                self.error("此设备已有密码本：加入前请先导出备份，并在设置中重置密码本")
            }
            SyncMode::Join => self.sync_join(form),
        }
    }

    /// The secret key step: the user confirmed it and gave the master
    /// password.
    pub fn sync_finish_first(self, confirmation: String, password: String) {
        let Some(key) = AccountKey::parse(&self.sync_secret.get()) else {
            return self.error("秘密密钥无效，请重新开始");
        };
        let typed: String = confirmation
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if typed != *key.confirmation_group() {
            return self.error("确认字符不对：请输入密钥中倒数第二组的 5 个字符");
        }
        let form = self.sync_form.get();
        self.sync_connect_first(form, Some((key, Zeroizing::new(password))));
    }

    /// Turns the secret key on (when given), registers this device as the
    /// first one and uploads the vault.
    fn sync_connect_first(self, form: SyncForm, key: Option<(AccountKey, Zeroizing<String>)>) {
        self.run_vault(
            move || {
                let vault = services().vault();
                // The password check first: a wrong one must not spend the
                // admin code.
                if let Some((key, password)) = &key {
                    vault.enable_account_key(password, key)?;
                }
                let url = form.url.trim();
                let token =
                    Client::new(url)?.bootstrap(form.code.trim(), form.device_name.trim())?;
                vault.start_sync(url, &token.device_id, &token.token)?;
                let client = cloud::client_for(vault)?.ok_or_else(no_client)?;
                cloud::sync_once(vault, &client)
            },
            move |_| {
                self.sync_secret.set(String::new());
                self.sync_form.set(SyncForm::default());
                self.dialog.set(None);
                self.refresh_sync();
                services().sync_now();
                self.success("云同步已开启");
            },
        );
    }

    /// Joins with a pairing code: the vault comes from the server, locked.
    fn sync_join(self, form: SyncForm) {
        self.run_vault(
            move || {
                let vault = services().vault();
                let url = form.url.trim();
                let token = Client::new(url)?.join(form.code.trim(), form.device_name.trim())?;
                vault.start_sync(url, &token.device_id, &token.token)?;
                let client = cloud::client_for(vault)?.ok_or_else(no_client)?;
                match client.vault()?.header {
                    Some(header) => vault.join_header(&header),
                    None => {
                        let _ = client.revoke(&token.device_id);
                        vault.stop_sync()?;
                        Err(VaultError::Sync(
                            "服务器上还没有密码本，请先在第一台设备上开启云同步".into(),
                        ))
                    }
                }
            },
            move |()| {
                self.dialog.set(None);
                self.page.set(Page::Vault);
                self.refresh_sync();
                self.success("已加入，请用主密码和秘密密钥解锁");
            },
        );
    }

    pub fn sync_now_pressed(self) {
        if self.vault_status.get() != VaultStatus::Unlocked {
            return self.error("请先解锁密码本，解锁后会自动同步");
        }
        services().sync_now();
    }

    pub fn sync_pair(self) {
        self.run_vault(
            || {
                let vault = services().vault();
                cloud::client_for(vault)?.ok_or_else(no_client)?.pair()
            },
            move |code| {
                self.sync_pair.set(Some(code));
                self.dialog.set(Some(DialogKind::SyncPair));
            },
        );
    }

    pub fn sync_open_devices(self) {
        self.run_vault(
            || {
                let vault = services().vault();
                cloud::client_for(vault)?.ok_or_else(no_client)?.devices()
            },
            move |devices| {
                self.sync_devices.set(devices);
                self.dialog.set(Some(DialogKind::SyncDevices));
            },
        );
    }

    pub fn sync_revoke(self, device_id: String) {
        let own = services()
            .vault()
            .sync_state()
            .ok()
            .flatten()
            .is_some_and(|s| s.device_id == device_id);
        self.run_vault(
            move || {
                let vault = services().vault();
                let client = cloud::client_for(vault)?.ok_or_else(no_client)?;
                client.revoke(&device_id)?;
                if own {
                    vault.stop_sync()?;
                    return Ok(Vec::new());
                }
                client.devices()
            },
            move |devices| {
                self.sync_devices.set(devices);
                if own {
                    self.dialog.set(None);
                }
                self.refresh_sync();
                self.success("设备已吊销");
            },
        );
    }

    /// Turns sync off here (the server forgets this device too).
    pub fn sync_stop(self) {
        self.run_vault(
            || {
                let vault = services().vault();
                if let (Ok(Some(client)), Ok(Some(state))) =
                    (cloud::client_for(vault), vault.sync_state())
                    && let Err(e) = client.revoke(&state.device_id)
                {
                    log::warn!("cannot revoke this device on the server: {e}");
                }
                vault.stop_sync()
            },
            move |()| {
                self.refresh_sync();
                self.success("已关闭云同步，本机密码本保持不变");
            },
        );
    }

    /// Takes the master password another device set.
    pub fn sync_adopt_password(self, password: String) {
        let password = Zeroizing::new(password);
        self.run_vault(
            move || services().vault().adopt_staged_header(&password),
            move |()| {
                self.dialog.set(None);
                services().sync_now();
                self.success("已更新为其他设备的设置");
            },
        );
    }

    pub fn sync_reveal_secret(self, password: String) {
        let password = Zeroizing::new(password);
        self.run_vault(
            move || services().vault().reveal_account_key(&password),
            move |key| {
                self.sync_secret.set(key.to_string());
                self.sync_secret_reveal.set(true);
                self.dialog.set(Some(DialogKind::SyncSecretKey));
            },
        );
    }
}

/// The sync status as a line.
fn status_line(state: AppState) -> String {
    let status = state.sync_status.get();
    if status.syncing {
        return "正在同步…".to_owned();
    }
    if let Some(error) = status.last_error {
        return error;
    }
    if status.last_ok_ms == 0 {
        return if state.vault_status.get() == VaultStatus::Unlocked {
            "等待同步".to_owned()
        } else {
            "密码本解锁后同步".to_owned()
        };
    }
    let at = chrono::DateTime::from_timestamp_millis(status.last_ok_ms as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    match status.conflicts {
        0 => format!("上次同步 {at}"),
        n => format!("上次同步 {at}，{n} 条冲突副本已保留"),
    }
}

/// The settings group of cloud sync.
pub fn cloud_section(state: AppState) -> impl IntoElement {
    when(
        move || state.sync_on.get(),
        move || {
            section(
                "云同步",
                vec![
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .px(px(12.0))
                        .py(px(10.0))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .gap(px(1.0))
                                .child(
                                    text(move || state.sync_server.get())
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .number_of_lines(1)
                                        .text_color(move || theme().card_foreground),
                                )
                                .child(
                                    text(move || status_line(state))
                                        .text_xs()
                                        .number_of_lines(2)
                                        .text_color(move || {
                                            if state.sync_status.with(|s| s.last_error.is_some()) {
                                                theme().destructive
                                            } else {
                                                muted()
                                            }
                                        }),
                                ),
                        )
                        .child(text_button("立即同步", Tone::Plain, move || {
                            state.sync_now_pressed()
                        }))
                        .into_element(),
                    row(
                        Icon::Key,
                        "添加设备",
                        "生成 10 分钟内有效的配对码",
                        text_button("配对码", Tone::Plain, move || state.sync_pair()),
                    )
                    .into_element(),
                    row(
                        Icon::User,
                        "设备",
                        "查看或吊销已同步的设备",
                        text_button("管理", Tone::Plain, move || state.sync_open_devices()),
                    )
                    .into_element(),
                    row(
                        Icon::Shield,
                        "秘密密钥",
                        "新设备首次解锁时需要（需主密码）",
                        text_button("查看", Tone::Plain, move || {
                            state.open_vault_password(VaultPurpose::RevealSecretKey)
                        }),
                    )
                    .into_element(),
                    row(
                        Icon::Close,
                        "关闭云同步",
                        "本机密码本保留，服务器上的本设备被吊销",
                        text_button("关闭", Tone::Danger, move || state.sync_stop()),
                    )
                    .into_element(),
                ],
            )
        },
    )
    .otherwise(move || {
        section(
            "云同步",
            vec![
                row(
                    Icon::Send,
                    "云同步",
                    "端到端加密同步到自建服务器，服务器拿不到任何明文",
                    view()
                        .flex_row()
                        .gap(px(6.0))
                        .child(text_button("开启", Tone::Plain, move || {
                            state.open_sync_setup(SyncMode::First)
                        }))
                        .child(text_button("加入", Tone::Plain, move || {
                            state.open_sync_setup(SyncMode::Join)
                        })),
                )
                .into_element(),
            ],
        )
    })
}

// --- Dialogs ---------------------------------------------------------------

pub fn sync_dialogs(state: AppState) -> impl IntoElement {
    let is = move |kind: DialogKind| move || state.dialog.get() == Some(kind);
    view()
        .child(when(is(DialogKind::SyncSetup), move || setup_dialog(state)))
        .child(when(is(DialogKind::SyncSecretKey), move || {
            secret_dialog(state)
        }))
        .child(when(is(DialogKind::SyncPair), move || pair_dialog(state)))
        .child(when(is(DialogKind::SyncDevices), move || {
            devices_dialog(state)
        }))
        .child(when(is(DialogKind::SyncNewPassword), move || {
            new_password_dialog(state)
        }))
}

fn label(text_: &'static str) -> impl IntoElement {
    text(text_)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(muted)
}

fn input(value: Signal<String>, id: &str, placeholder: &'static str) -> Input {
    field()
        .test_id(id)
        .value(move || value.get())
        .on_change(move |t: &str| value.set(t.to_owned()))
        .placeholder(placeholder)
}

fn setup_dialog(state: AppState) -> impl IntoElement {
    let mode = untracked(move || state.sync_mode.get());
    let url = signal(String::new());
    let code = signal(String::new());
    let name = signal(DEFAULT_DEVICE_NAME.to_owned());
    let code_field = element_ref();
    let name_field = element_ref();
    let next = move || {
        state.sync_setup_next(SyncForm {
            url: url.get(),
            code: code.get(),
            device_name: name.get(),
        })
    };
    let (title, code_label, note, action) = match mode {
        SyncMode::First => (
            "开启云同步",
            "管理码（服务器首次启动时打印在日志里）",
            "下一步会生成秘密密钥：新设备解锁时需要它和主密码。",
            "下一步",
        ),
        SyncMode::Join => (
            "加入云同步",
            "配对码（在已同步的设备上：设置 › 云同步 › 添加设备）",
            "加入后，用主密码和秘密密钥解锁。",
            "加入",
        ),
    };
    let content = view()
        .gap(px(10.0))
        .child(text(note).text_xs().text_color(muted))
        .child(
            view()
                .gap(px(4.0))
                .child(label("服务器地址（Tailscale）"))
                .child(
                    input(url, "sync-url", "http://100.x.x.x:8787")
                        .on_submit(move |_: &str| code_field.focus())
                        .auto_focus(true),
                ),
        )
        .child(
            view().gap(px(4.0)).child(label(code_label)).child(
                input(code, "sync-code", "XXXX-XXXX")
                    .element_ref(&code_field)
                    .on_submit(move |_: &str| name_field.focus()),
            ),
        )
        .child(
            view().gap(px(4.0)).child(label("设备名称")).child(
                input(name, "sync-device", DEFAULT_DEVICE_NAME)
                    .element_ref(&name_field)
                    .on_submit(move |_: &str| next()),
            ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(px(8.0))
                .pt(px(6.0))
                .child(text_button("取消", Tone::Plain, move || {
                    state.dialog.set(None)
                }))
                .child(
                    text_button(
                        move || {
                            if state.vault_busy.get() {
                                "处理中…"
                            } else {
                                action
                            }
                        },
                        Tone::Primary,
                        next,
                    )
                    .disabled(move || state.vault_busy.get()),
                ),
        );
    modal(state, title, 340.0, content)
}

fn secret_dialog(state: AppState) -> impl IntoElement {
    let reveal = untracked(move || state.sync_secret_reveal.get());
    let confirmation = signal(String::new());
    let password = signal(String::new());
    let password_field = element_ref();
    let close = move || {
        state.sync_secret.set(String::new());
        state.dialog.set(None);
    };
    let finish = move || {
        state.sync_finish_first(confirmation.get(), password.get());
        password.set(String::new());
    };
    let key_box = view()
        .p(px(12.0))
        .rounded(px(10.0))
        .bg(move || theme().muted)
        .child(
            text(move || state.sync_secret.get())
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .font_family("ui-monospace")
                .select_text()
                .text_center()
                .text_color(move || theme().foreground),
        );
    let content = if reveal {
        view()
            .gap(px(10.0))
            .child(
                text(
                    "新设备首次解锁时，需要这个秘密密钥和主密码。它只保存在你的设备上，不会上传。",
                )
                .text_xs()
                .text_color(muted),
            )
            .child(key_box)
            .child(
                view()
                    .flex_row()
                    .justify_end()
                    .pt(px(6.0))
                    .child(text_button("关闭", Tone::Primary, close)),
            )
    } else {
        view()
            .gap(px(10.0))
            .child(
                text("这是你的秘密密钥。新设备解锁时需要它和主密码；服务器上没有它，拿到数据也无法破解。")
                    .text_xs()
                    .text_color(muted),
            )
            .child(key_box)
            .child(
                text("请现在抄写或保存到安全的地方。主密码或秘密密钥丢失、且没有其他已解锁的设备时，数据无法恢复。")
                    .text_xs()
                    .text_color(move || theme().destructive),
            )
            .child(
                input(confirmation, "sync-confirm", "输入密钥倒数第二组的 5 个字符以确认")
                    .on_submit(move |_: &str| password_field.focus())
                    .auto_focus(true),
            )
            .child(
                field()
                    .test_id("sync-password")
                    .element_ref(&password_field)
                    .masked(true)
                    .value(move || password.get())
                    .on_change(move |t: &str| password.set(t.to_owned()))
                    .on_submit(move |_: &str| finish())
                    .placeholder("主密码"),
            )
            .child(
                view()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .pt(px(6.0))
                    .child(text_button("取消", Tone::Plain, close))
                    .child(
                        text_button(
                            move || if state.vault_busy.get() { "处理中…" } else { "开启同步" },
                            Tone::Primary,
                            finish,
                        )
                        .disabled(move || state.vault_busy.get()),
                    ),
            )
    };
    modal(state, "秘密密钥", 360.0, content)
}

fn pair_dialog(state: AppState) -> impl IntoElement {
    let code = move || {
        state
            .sync_pair
            .with(|p| p.as_ref().map(|p| p.code.clone()).unwrap_or_default())
    };
    let content = view()
        .gap(px(10.0))
        .child(
            text("在新设备上：设置 › 云同步 › 加入，输入服务器地址和下面的配对码。10 分钟内有效，只能使用一次。")
                .text_xs()
                .text_color(muted),
        )
        .child(
            view()
                .p(px(14.0))
                .rounded(px(10.0))
                .bg(move || theme().muted)
                .child(
                    text(code)
                        .text_2xl()
                        .font_weight(FontWeight::BOLD)
                        .font_family("ui-monospace")
                        .select_text()
                        .text_center()
                        .text_color(move || theme().foreground),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .pt(px(6.0))
                .child(text_button("完成", Tone::Primary, move || {
                    state.sync_pair.set(None);
                    state.dialog.set(None);
                })),
        );
    modal(state, "配对码", 320.0, content)
}

fn devices_dialog(state: AppState) -> impl IntoElement {
    let own = untracked(|| {
        services()
            .vault()
            .sync_state()
            .ok()
            .flatten()
            .map(|s| s.device_id)
            .unwrap_or_default()
    });
    let rows = each(
        move || state.sync_devices.get(),
        |d| d.id.clone(),
        move |device, _| {
            let own = own.clone();
            let is_own = device.with(|d| d.id == own);
            let seen = move || {
                device.with(|d| {
                    chrono::DateTime::from_timestamp(d.last_seen, 0)
                        .filter(|_| d.last_seen > 0)
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("最近活动 %Y-%m-%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_else(|| "尚未活动".to_owned())
                })
            };
            view()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(move || theme().border)
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        .child(
                            text(move || {
                                let name = device.with(|d| d.name.clone());
                                if is_own {
                                    format!("{name}（本机）")
                                } else {
                                    name
                                }
                            })
                            .text_sm()
                            .text_color(move || theme().foreground),
                        )
                        .child(text(seen).text_xs().text_color(muted)),
                )
                .child(text_button("吊销", Tone::Danger, move || {
                    state.sync_revoke(device.with(|d| d.id.clone()))
                }))
        },
    );
    let content = view()
        .gap(px(4.0))
        .child(
            text("吊销后，该设备无法再同步；本机被吊销时会关闭云同步。")
                .text_xs()
                .text_color(muted),
        )
        .child(scroll().max_h(px(300.0)).child(rows))
        .child(
            view()
                .flex_row()
                .justify_end()
                .pt(px(10.0))
                .child(text_button("完成", Tone::Primary, move || {
                    state.dialog.set(None)
                })),
        );
    modal(state, "设备", 340.0, content)
}

fn new_password_dialog(state: AppState) -> impl IntoElement {
    let password = signal(String::new());
    let submit = move || {
        state.sync_adopt_password(password.get());
        password.set(String::new());
    };
    let content = view()
        .gap(px(10.0))
        .child(
            text("其他设备修改了主密码，或重新生成了助记词。输入当前的主密码以继续同步，本机尚未同步的修改会一并保留。")
                .text_xs()
                .text_color(muted),
        )
        .child(
            field()
                .test_id("sync-new-password")
                .masked(true)
                .value(move || password.get())
                .on_change(move |t: &str| password.set(t.to_owned()))
                .on_submit(move |_: &str| submit())
                .placeholder("当前的主密码")
                .auto_focus(true),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(px(8.0))
                .pt(px(6.0))
                .child(text_button("稍后", Tone::Plain, move || {
                    state.dialog.set(None)
                }))
                .child(
                    text_button(
                        move || {
                            if state.vault_busy.get() {
                                "验证中…"
                            } else {
                                "确认"
                            }
                        },
                        Tone::Primary,
                        submit,
                    )
                    .disabled(move || state.vault_busy.get()),
                ),
        );
    modal(state, "主密码已更新", 320.0, content)
}
