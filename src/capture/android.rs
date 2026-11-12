//! Android's clipboard, through NSH (reading it: [`super::read_foreground`])
//! and, for secrets, through the bridge (`crate::android`), which alone
//! reaches the clip's sensitive flag. NSH writes no images there: the
//! history keeps text only.
//!
//! These run on the UI thread (NSH's clipboard is the engine's).

use std::thread;

use nsh_core::prelude::set_clipboard;

use super::{SECRET_LIFETIME, begin_secret_copy, is_latest_secret_copy};
use crate::android;
use crate::db::{Database, ItemContent};

/// Copies `text` as a secret: marked sensitive (left out of the system's
/// previews and keyboards' suggestions) and out of our history, and cleared
/// after [`SECRET_LIFETIME`] unless something else was copied.
pub fn copy_secret(text: &str) -> Result<(), String> {
    let copy = begin_secret_copy(text);
    android::copy_sensitive(text, copy)?;
    thread::Builder::new()
        .name("secret-clear".into())
        .spawn(move || {
            thread::sleep(SECRET_LIFETIME);
            // A later secret copy has a timer of its own.
            if !is_latest_secret_copy(copy) {
                return;
            }
            if let Err(e) = android::clear_sensitive(copy) {
                log::error!("cannot clear a copied secret: {e}");
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Puts item `id` back on the system clipboard.
pub fn copy_item(db: &Database, id: i64) -> Result<(), String> {
    let content = db
        .item_content(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "该条目已不存在".to_owned())?;
    match content {
        ItemContent::Text(text) => {
            set_clipboard(text);
            Ok(())
        }
        ItemContent::Image(_) => Err("安卓暂不支持复制图片".to_owned()),
    }
}
