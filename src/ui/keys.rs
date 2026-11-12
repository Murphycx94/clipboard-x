//! The keyboard: Escape closes, Tab and ←/→ switch tabs, ↑/↓ move, Enter
//! and ⌘⇧1-9 (Ctrl+Shift elsewhere) copy and close. On the vault page ↑/↓
//! move, Enter copies the password, ⌘N adds an entry and ⌘L locks.
//!
//! Tab, Enter and ↑/↓ are bound only while a list takes them
//! ([`list_keys`]): elsewhere (forms, dialogs) Tab moves the focus between
//! fields and Enter submits, as they should.

use nsh_core::keymap::When;
use nsh_core::prelude::*;

use crate::vault::{Field, VaultStatus};

use super::state::{AppState, Page, Tab};

/// Fires in text inputs too: the search field keeps the focus.
const ALWAYS: KeyOptions = KeyOptions {
    when: When::Always,
    repeat: false,
    pass_through: false,
};
const REPEATING: KeyOptions = KeyOptions {
    when: When::Always,
    repeat: true,
    pass_through: false,
};

pub fn bind(state: AppState) {
    on_key_with("escape", ALWAYS, move |_| state.escape());
    // ←/→ move the caret while the search has text.
    on_key("left", move |_| {
        if state.list_has_keys() {
            state.set_tab(Tab::History);
        }
    });
    on_key("right", move |_| {
        if state.list_has_keys() {
            state.set_tab(Tab::Favorites);
        }
    });
    // The vault: ⌘N a new entry, ⌘L lock.
    on_key_with("mod+n", ALWAYS, move |_| {
        if state.vault_has_keys() {
            state.open_vault_editor(None);
        }
    });
    on_key_with("mod+l", ALWAYS, move |_| {
        if state.page.get() == Page::Vault && state.vault_status.get() == VaultStatus::Unlocked {
            state.vault_lock();
        }
    });
    for digit in 1..=9 {
        on_key_with(&format!("mod+shift+{digit}"), ALWAYS, move |_| {
            state.copy_row(digit - 1);
        });
    }
}

/// Tab, ↑/↓ and Enter for the history and vault lists, bound while one of
/// them takes the keyboard (the bindings go with the elements).
pub fn list_keys(state: AppState) -> impl IntoElement {
    let lists = move || state.list_has_keys() || state.vault_has_keys();
    view()
        .absolute()
        .child(when(
            move || state.list_has_keys(),
            move || {
                on_key_with("tab", ALWAYS, move |_| state.toggle_tab());
                view()
            },
        ))
        .child(when(lists, move || {
            on_key_with("up", REPEATING, move |_| {
                if state.vault_has_keys() {
                    state.vault_move_focus(-1);
                } else {
                    state.move_focus(-1);
                }
            });
            on_key_with("down", REPEATING, move |_| {
                if state.vault_has_keys() {
                    state.vault_move_focus(1);
                } else {
                    state.move_focus(1);
                }
            });
            on_key_with("enter", ALWAYS, move |_| {
                if state.vault_has_keys() {
                    if let Some(id) = state.vault_focused_id() {
                        state.vault_copy(&id, Field::Password);
                    }
                } else if state.list_has_keys() {
                    state.copy_row(state.focused.get());
                }
            });
            view()
        }))
}
