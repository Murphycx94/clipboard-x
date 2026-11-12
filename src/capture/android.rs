//! Android's clipboard, through NSH (reading it: [`super::read_foreground`]).
//! NSH writes no images there: the history keeps text only.
//!
//! These run on the UI thread (NSH's clipboard is the engine's).

use nsh_core::prelude::set_clipboard;

use super::begin_secret_copy;
use crate::db::{Database, ItemContent};

/// Copies `text` as a secret: left out of our history. Marking it for the
/// system and clearing it after 30 seconds are the Android host's (it
/// alone reaches `ClipDescription`).
pub fn copy_secret(text: &str) -> Result<(), String> {
    begin_secret_copy(text);
    set_clipboard(text);
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
