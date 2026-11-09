//! The keyboard: Escape closes, Tab and ←/→ switch tabs, ↑/↓ move, Enter
//! and ⌘⇧1-9 (Ctrl+Shift elsewhere) copy and close. On the vault page ↑/↓
//! move, Enter copies the password, ⌘N adds an entry and ⌘L locks.

use nsh_core::keymap::When;
use nsh_core::prelude::*;

use crate::vault::{Field, VaultStatus};

use super::state::{AppState, Page, Tab};

pub fn bind(state: AppState) {
    // The search field keeps the focus, so these fire while it has it.
    let always = KeyOptions {
        when: When::Always,
        ..KeyOptions::default()
    };
    let repeating = KeyOptions {
        when: When::Always,
        repeat: true,
    };
    on_key_with("escape", always, move |_| state.escape());
    on_key_with("tab", always, move |_| {
        if state.list_has_keys() {
            state.toggle_tab();
        }
    });
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
    on_key_with("up", repeating, move |_| {
        if state.vault_has_keys() {
            state.vault_move_focus(-1);
        } else {
            state.move_focus(-1);
        }
    });
    on_key_with("down", repeating, move |_| {
        if state.vault_has_keys() {
            state.vault_move_focus(1);
        } else {
            state.move_focus(1);
        }
    });
    on_key_with("enter", always, move |_| {
        if state.vault_has_keys() {
            if let Some(id) = state.vault_focused_id() {
                state.vault_copy(&id, Field::Password);
            }
        } else if state.list_has_keys() {
            state.copy_row(state.focused.get());
        }
    });
    // The vault: ⌘N a new entry, ⌘L lock.
    on_key_with("mod+n", always, move |_| {
        if state.vault_has_keys() {
            state.open_vault_editor(None);
        }
    });
    on_key_with("mod+l", always, move |_| {
        if state.page.get() == Page::Vault && state.vault_status.get() == VaultStatus::Unlocked {
            state.vault_lock();
        }
    });
    for digit in 1..=9 {
        on_key_with(&format!("mod+shift+{digit}"), always, move |_| {
            state.copy_row(digit - 1);
        });
    }
}
