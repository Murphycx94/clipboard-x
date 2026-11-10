//! The app's UI: one window with the history, the archive and the settings.

mod archive;
mod dialogs;
mod header;
mod icons;
mod item_row;
mod keys;
mod main_panel;
mod settings;
mod state;
mod vault;
mod widgets;

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::services::services;

pub use state::{AppState, Page};

/// The application's root component.
#[component]
pub fn App() -> impl IntoElement {
    let state = AppState::new();
    services().start_capture();
    services().start_sync_worker();
    state.reload();
    state.apply_theme();
    state.watch_services();
    keys::bind(state);

    let page_is = move |page: Page| move || state.page.get() == page;
    view()
        .flex_1()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        .child(header::header(state))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(when(page_is(Page::Main), move || {
                    main_panel::main_panel(state)
                }))
                .child(when(page_is(Page::Archive), move || {
                    archive::archive_page(state)
                }))
                .child(when(page_is(Page::Settings), move || {
                    settings::settings_page(state)
                }))
                .child(when(page_is(Page::Vault), move || vault::vault_page(state))),
        )
        .child(widgets::toast_layer(state))
        .child(dialogs::dialogs(state))
}
