use crate::database::{AppSettings, ClipboardItem, Database, DateGroup};
use crate::{logger, telegram};
use arboard::Clipboard;
use std::sync::Arc;
use tauri::State;

type DbState<'a> = State<'a, Arc<Database>>;

#[tauri::command]
pub fn get_today_items(db: DbState) -> Result<Vec<ClipboardItem>, String> {
    db.get_today_items().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_archive_items(db: DbState) -> Result<Vec<DateGroup>, String> {
    db.get_archive_items().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_favorite_items(db: DbState) -> Result<Vec<ClipboardItem>, String> {
    db.get_favorite_items().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn toggle_favorite(id: i64, note: String, db: DbState<'_>) -> Result<(), String> {
    let db = Arc::clone(&*db);
    let is_now_favorite = db.toggle_favorite(id, &note).map_err(|e| e.to_string())?;

    let (token, chat_id) = db.get_telegram_config().unwrap_or_default();
    if !token.is_empty() && !chat_id.is_empty() {
        let items = db.get_favorite_items().unwrap_or_default();
        let db_clone = Arc::clone(&db);
        tokio::spawn(async move {
            match telegram::sync_favorites(&token, &chat_id, &items, db_clone).await {
                Ok(_) => log::info!("Telegram sync success ({} favorites)", items.len()),
                Err(e) => log::error!("Telegram sync failed: {}", e),
            }
        });
    }

    Ok(())
}

#[tauri::command]
pub fn copy_to_clipboard(id: i64, db: DbState) -> Result<(), String> {
    let content_type = db.get_content_type(id).map_err(|e| e.to_string())?;
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;

    if content_type == "text" {
        if let Ok(Some(text)) = db.get_text_content(id) {
            clipboard.set_text(text).map_err(|e| e.to_string())?;
        }
    } else if let Ok(Some(data)) = db.get_image_data(id) {
        let img = image::load_from_memory(&data).map_err(|e| e.to_string())?;
        let rgba = img.to_rgba8();
        let img_data = arboard::ImageData {
            width: rgba.width() as usize,
            height: rgba.height() as usize,
            bytes: rgba.into_raw().into(),
        };
        clipboard.set_image(img_data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_item(id: i64, db: DbState<'_>) -> Result<(), String> {
    let db = Arc::clone(&*db);
    let was_favorite = db.is_favorite(id).unwrap_or(false);
    db.delete_item(id).map_err(|e| e.to_string())?;

    if was_favorite {
        let (token, chat_id) = db.get_telegram_config().unwrap_or_default();
        if !token.is_empty() && !chat_id.is_empty() {
            let items = db.get_favorite_items().unwrap_or_default();
            let db_clone = Arc::clone(&db);
            tokio::spawn(async move {
                match telegram::sync_favorites(&token, &chat_id, &items, db_clone).await {
                    Ok(_) => log::info!("Telegram sync success ({} favorites)", items.len()),
                    Err(e) => log::error!("Telegram sync failed: {}", e),
                }
            });
        }
    }

    Ok(())
}

#[tauri::command]
pub fn get_settings(db: DbState) -> Result<AppSettings, String> {
    db.get_settings().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_hotkey(hotkey: String, db: DbState) -> Result<(), String> {
    db.update_hotkey(&hotkey).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_note(id: i64, note: String, db: DbState) -> Result<(), String> {
    db.update_note(id, &note).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_image_base64(id: i64, db: DbState) -> Result<Option<String>, String> {
    db.get_image_base64(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_retention(days: i64, db: DbState) -> Result<(), String> {
    db.update_retention(days).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_history(db: DbState) -> Result<usize, String> {
    db.clear_history().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_telegram_token(token: String, db: DbState) -> Result<(), String> {
    db.update_telegram_token(&token).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_telegram_chat_id(chat_id: String, db: DbState) -> Result<(), String> {
    db.update_telegram_chat_id(&chat_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_logs() -> String {
    logger::get_log_contents()
}
