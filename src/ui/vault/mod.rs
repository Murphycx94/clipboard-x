//! The password vault page: set up, locked or unlocked.

mod actions;
mod cloud;
mod dialogs;
mod editor;
mod list;
mod lock;
mod recovery;

use nsh_core::prelude::*;

use crate::vault::VaultStatus;

use super::state::AppState;

pub use cloud::cloud_section;
pub use dialogs::vault_dialogs;

pub fn vault_page(state: AppState) -> impl IntoElement {
    let is = move |status: VaultStatus| move || state.vault_status.get() == status;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(when(is(VaultStatus::NotSetUp), move || {
            lock::setup_view(state)
        }))
        .child(when(is(VaultStatus::Locked), move || {
            lock::unlock_view(state)
        }))
        .child(when(is(VaultStatus::Unlocked), move || {
            list::list_view(state)
        }))
}
