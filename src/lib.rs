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
