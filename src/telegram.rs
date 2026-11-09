//! Sending the favorites to a Telegram chat through a bot: the texts as
//! one summary message, the images as photos (albums of ten at most).

use std::time::Duration;

use reqwest::blocking::{Client, multipart};

use crate::db::ItemContent;
use crate::model::{ClipboardItem, minute_label};

const API: &str = "https://api.telegram.org";
const MEDIA_GROUP_MAX: usize = 10;
/// Telegram's limit is 4096 characters per message; leave room for markup.
const MESSAGE_MAX_CHARS: usize = 3800;
/// The longest text of one favorite in the summary.
const ITEM_MAX_CHARS: usize = 1000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// The bot and the chat it posts to.
#[derive(Clone)]
pub struct TelegramTarget {
    pub token: String,
    pub chat_id: String,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_owned(),
    }
}

/// The summary of the text favorites, split into messages that fit.
/// `now` heads the first one; `total` counts every favorite.
pub fn text_messages(texts: &[(&ClipboardItem, &str)], total: usize, now: &str) -> Vec<String> {
    if texts.is_empty() {
        return Vec::new();
    }
    let mut messages = Vec::new();
    let mut current = format!("<b>ClipboardX 收藏列表</b>\n{now} · 共 {total} 条\n\n");
    for (index, (item, text)) in texts.iter().enumerate() {
        let mut entry = format!(
            "{}. <code>{}</code>\n",
            index + 1,
            html_escape(&truncate(text.trim(), ITEM_MAX_CHARS))
        );
        if let Some(note) = item.note_text() {
            entry.push_str(&format!("备注：{}\n", html_escape(note)));
        }
        entry.push_str(&format!("{}\n\n", minute_label(&item.created_at)));
        if current.chars().count() + entry.chars().count() > MESSAGE_MAX_CHARS {
            messages.push(std::mem::take(&mut current));
        }
        current.push_str(&entry);
    }
    messages.push(current);
    messages
}

/// The caption of a photo: its time, and its note.
pub fn photo_caption(item: &ClipboardItem) -> String {
    let time = minute_label(&item.created_at);
    match item.note_text() {
        Some(note) => format!("{time}\n备注：{note}"),
        None => time,
    }
}

fn check(response: reqwest::blocking::Response, method: &str) -> Result<(), String> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    Err(format!("{method} failed ({status}): {body}"))
}

fn photo_part(png: &[u8]) -> Result<multipart::Part, String> {
    multipart::Part::bytes(png.to_vec())
        .file_name("image.png")
        .mime_str("image/png")
        .map_err(|e| e.to_string())
}

/// Sends every favorite to the chat. Blocking: run it off the UI.
pub fn sync_favorites(
    target: &TelegramTarget,
    favorites: &[(ClipboardItem, ItemContent)],
) -> Result<(), String> {
    let client = Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let url = |method: &str| format!("{API}/bot{}/{method}", target.token);

    let texts: Vec<(&ClipboardItem, &str)> = favorites
        .iter()
        .filter_map(|(item, content)| match content {
            ItemContent::Text(text) => Some((item, text.as_str())),
            ItemContent::Image(_) => None,
        })
        .collect();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    for text in text_messages(&texts, favorites.len(), &now) {
        let payload = serde_json::json!({
            "chat_id": target.chat_id,
            "text": text,
            "parse_mode": "HTML",
        });
        let response = client
            .post(url("sendMessage"))
            .json(&payload)
            .send()
            .map_err(|e| e.to_string())?;
        check(response, "sendMessage")?;
    }

    let photos: Vec<(&[u8], String)> = favorites
        .iter()
        .filter_map(|(item, content)| match content {
            ItemContent::Image(png) if !png.is_empty() => {
                Some((png.as_slice(), photo_caption(item)))
            }
            _ => None,
        })
        .collect();
    for chunk in photos.chunks(MEDIA_GROUP_MAX) {
        if let [(png, caption)] = chunk {
            let form = multipart::Form::new()
                .text("chat_id", target.chat_id.clone())
                .text("caption", caption.clone())
                .part("photo", photo_part(png)?);
            let response = client
                .post(url("sendPhoto"))
                .multipart(form)
                .send()
                .map_err(|e| e.to_string())?;
            check(response, "sendPhoto")?;
            continue;
        }
        let media: Vec<serde_json::Value> = chunk
            .iter()
            .enumerate()
            .map(|(i, (_, caption))| {
                serde_json::json!({
                    "type": "photo",
                    "media": format!("attach://photo{i}"),
                    "caption": caption,
                })
            })
            .collect();
        let mut form = multipart::Form::new()
            .text("chat_id", target.chat_id.clone())
            .text("media", serde_json::Value::Array(media).to_string());
        for (i, (png, _)) in chunk.iter().enumerate() {
            form = form.part(format!("photo{i}"), photo_part(png)?);
        }
        let response = client
            .post(url("sendMediaGroup"))
            .multipart(form)
            .send()
            .map_err(|e| e.to_string())?;
        check(response, "sendMediaGroup")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ContentType;

    fn item(note: Option<&str>) -> ClipboardItem {
        ClipboardItem {
            id: 1,
            content_type: ContentType::Text,
            text_preview: None,
            thumbnail: None,
            created_at: "2026-03-17T14:32:05".into(),
            is_favorite: true,
            note: note.map(str::to_owned),
        }
    }

    #[test]
    fn no_texts_no_messages() {
        assert!(text_messages(&[], 3, "now").is_empty());
    }

    #[test]
    fn summary_escapes_html_and_lists_notes() {
        let a = item(Some("<prod>"));
        let b = item(None);
        let messages = text_messages(&[(&a, "a < b & c"), (&b, "plain")], 2, "2026-03-17 15:00");
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert!(msg.starts_with("<b>ClipboardX 收藏列表</b>\n2026-03-17 15:00 · 共 2 条"));
        assert!(
            msg.contains("1. <code>a &lt; b &amp; c</code>\n备注：&lt;prod&gt;\n2026-03-17 14:32")
        );
        assert!(msg.contains("2. <code>plain</code>\n2026-03-17 14:32"));
    }

    #[test]
    fn long_summaries_are_split_and_entries_truncated() {
        let it = item(None);
        let long = "y".repeat(5000);
        let texts: Vec<_> = (0..10).map(|_| (&it, long.as_str())).collect();
        let messages = text_messages(&texts, 10, "now");
        assert!(messages.len() > 1);
        assert!(
            messages
                .iter()
                .all(|m| m.chars().count() <= MESSAGE_MAX_CHARS + ITEM_MAX_CHARS)
        );
        assert!(messages[0].contains('…'));
    }

    #[test]
    fn captions_have_time_and_note() {
        assert_eq!(photo_caption(&item(None)), "2026-03-17 14:32");
        assert_eq!(
            photo_caption(&item(Some("设计稿"))),
            "2026-03-17 14:32\n备注：设计稿"
        );
    }

    #[test]
    fn truncates_by_characters() {
        assert_eq!(truncate("你好世界", 2), "你好…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
