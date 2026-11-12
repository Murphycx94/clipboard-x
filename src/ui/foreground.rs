//! Phones: apps other than the input method read the clipboard only in
//! the foreground (Android 10+), so the history records it each time the
//! app comes forward. Once per return, not polled: Android 12+ tells the
//! user each time an app reads what another app copied.
//!
//! Going to the background is a phone's hiding of the panel: the vault
//! locks then when the user wants it (`vault_lock_on_hide`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;

use crate::capture;
use crate::services::services;

/// How often the app's lifecycle is checked.
const LIFECYCLE_POLL: Duration = Duration::from_millis(250);

/// Records the clipboard now and each time the app comes to the
/// foreground; tells the services when it goes to the background.
pub fn follow_lifecycle() {
    let captures = services().captures();
    // As if it was in the background: the launch reads the clipboard too.
    let was = Rc::new(Cell::new(Lifecycle::Background));
    set_interval(LIFECYCLE_POLL, move || {
        let now = lifecycle();
        let before = was.replace(now);
        if now == before {
            return;
        }
        match now {
            Lifecycle::Active if captures => {
                spawn(record_once());
            }
            Lifecycle::Background => services().window_hidden(),
            Lifecycle::Active | Lifecycle::Inactive => {}
        }
    });
}

async fn record_once() {
    let Some(captured) = capture::read_foreground().await else {
        return;
    };
    if let Err(e) = services().record_clipboard(&captured) {
        log::error!("cannot store a clipboard item: {e}");
    }
}
