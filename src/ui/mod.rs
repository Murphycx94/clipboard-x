//! The app's UI: one window with the history, the archive and the settings.

mod archive;
mod dialogs;
mod foreground;
mod header;
mod icons;
mod item_row;
mod keys;
mod main_panel;
mod mobile;
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
    crate::startup();
    let state = AppState::new();
    let touch = mobile::is_touch();
    if touch {
        foreground::follow_lifecycle();
        // No desktop host here to do it for its window.
        if let Err(e) = services().apply_capture_protection() {
            log::error!("cannot hide the window from screen captures: {e}");
        }
    } else {
        services().start_capture();
        keys::bind(state);
    }
    services().start_sync_worker();
    state.reload();
    state.apply_theme();
    state.watch_services();

    let page_is = move |page: Page| move || state.page.get() == page;
    view()
        .flex_1()
        .bg(move || theme().background)
        .text_color(move || theme().foreground)
        // Phones: below the status bar.
        .pt(move || {
            if touch {
                Length::from(safe_area().top)
            } else {
                Length::from(px(0.0))
            }
        })
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
        .child(when(move || !touch, move || keys::list_keys(state)))
        .child(when(move || touch, move || mobile::tab_bar(state)))
        .child(widgets::toast_layer(state))
        .child(dialogs::dialogs(state))
}
