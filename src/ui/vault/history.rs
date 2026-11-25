//! The cloud section's switch for syncing the clipboard history.

use nsh_core::prelude::*;

use crate::services::services;

use super::super::icons::Icon;
use super::super::settings::{row, switch_control};
use super::super::state::AppState;

const TITLE: &str = "同步复制历史";
const DESCRIPTION: &str = "文本条目在开启了此项的设备间同步，密码本解锁时进行";

impl AppState {
    /// Turns clipboard history sync on or off here.
    pub fn set_history_sync(self, on: bool) {
        match services().set_history_sync(on) {
            Ok(()) if on => self.success("已开启，本机的文本历史将上传"),
            Ok(()) => {}
            Err(e) => self.error(format!("无法切换复制历史同步：{e}")),
        }
        self.history_sync_on.set(services().history_sync_enabled());
    }
}

/// What the row says under its title: what went wrong in the last round,
/// if anything did, else what the switch does.
fn description(state: AppState) -> String {
    let problem = state.sync_status.with(|s| s.history_error.clone());
    match problem {
        Some(problem) if state.history_sync_on.get() => problem,
        _ => DESCRIPTION.to_owned(),
    }
}

pub fn history_sync_row(state: AppState) -> impl IntoElement {
    row(
        Icon::Clock,
        TITLE,
        move || description(state),
        switch_control(
            move || state.history_sync_on.get(),
            move |on| state.set_history_sync(on),
        )
        .accessibility_label(TITLE),
    )
}
