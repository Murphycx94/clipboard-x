//! ClipboardX: a clipboard history manager, written in Rust on NSH.
//!
//! - [`model`], [`db`]: the history and the settings, in SQLite.
//! - [`capture`]: watching the system clipboard, copying items back.
//! - [`services`]: what the UI and the desktop host share.
//! - [`telegram`], [`updater`], [`hotkey`], [`logger`]: the rest of the
//!   app's services.
//! - `ui`: the NSH components; [`App`] is the root (`nsh.json` `"root"`).

pub mod capture;
pub mod cloud;
pub mod db;
#[cfg(not(target_os = "android"))]
pub mod hotkey;
pub mod logger;
pub mod model;
pub mod services;
pub mod telegram;
mod ui;
pub mod updater;
pub mod vault;

use nsh_core::element::Root;
use nsh_core::prelude::*;

pub use ui::App;

/// The app's look (`nsh.json` `"theme"`).
pub const THEME_JSON: &str = include_str!("../theme.json");

/// The app's UI set (`nsh.json` `"rust"`).
#[native_module("clipboardx/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// What comes before the UI on every platform, once: the data directory
/// and the log. The desktops' `main` calls it; on Android, where NSH's host
/// mounts [`App`] with no `main` of ours, `App` does.
pub fn startup() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let dir = services::data_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("cannot create {}: {e}", dir.display());
        }
        logger::init(&dir);
        log::info!("ClipboardX {} starting", env!("CARGO_PKG_VERSION"));
    });
}

/// The root a host mounts: [`App`] with the app's id and theme.
pub fn root() -> Root {
    let root = Root::of::<App>().app_id(services::APP_ID);
    match nsh_base::theme_overrides(THEME_JSON) {
        Ok(tokens) => root.tokens(tokens),
        Err(e) => {
            log::error!("theme.json: {e}");
            root
        }
    }
}
