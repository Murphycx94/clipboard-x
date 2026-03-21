use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crate::database::{ClipboardItem, Database};
use std::sync::Arc;

const MEDIA_GROUP_MAX: usize = 10;

pub async fn sync_favorites(
    token: &str,
    chat_id: &str,
    items: &[ClipboardItem],
    db: Arc<Database>,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    // 文本条目汇总为一条消息
    let text_items: Vec<_> = items.iter().filter(|i| i.content_type == "text").collect();
    if !text_items.is_empty() {
        let text = format_text_message(&text_items, items.len());
        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML"
        });
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("sendMessage error: {}", body));
        }
    }

    // 收集有效图片数据
    let image_items: Vec<_> = items.iter().filter(|i| i.content_type == "image").collect();
    let mut image_payloads: Vec<(Vec<u8>, String)> = Vec::new(); // (bytes, caption)
    for item in &image_items {
        let image_bytes = match db.get_image_data(item.id) {
            Ok(Some(data)) => data,
            _ => match &item.image_thumbnail {
                Some(b64) => BASE64.decode(b64).unwrap_or_default(),
                None => continue,
            },
        };
        if image_bytes.is_empty() {
            continue;
        }
        let time_str = item
            .created_at
            .get(..16)
            .unwrap_or(&item.created_at)
            .replace('T', " ");
        let caption = match &item.note {
            Some(n) if !n.is_empty() => format!("{}\n备注：{}", time_str, n),
            _ => time_str,
        };
        image_payloads.push((image_bytes, caption));
    }

    // 按最多 10 张分批，用 sendMediaGroup 批量发送
    for chunk in image_payloads.chunks(MEDIA_GROUP_MAX) {
        if chunk.len() == 1 {
            // 单张用 sendPhoto（更简单，caption 限制少）
            let (bytes, caption) = &chunk[0];
            let url = format!("https://api.telegram.org/bot{}/sendPhoto", token);
            let part = reqwest::multipart::Part::bytes(bytes.clone())
                .file_name("image.png")
                .mime_str("image/png")
                .map_err(|e| e.to_string())?;
            let form = reqwest::multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .text("caption", caption.clone())
                .part("photo", part);
            let resp = client
                .post(&url)
                .multipart(form)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("sendPhoto error: {}", body));
            }
        } else {
            // 多张用 sendMediaGroup
            let url = format!("https://api.telegram.org/bot{}/sendMediaGroup", token);
            let media_json: Vec<serde_json::Value> = chunk
                .iter()
                .enumerate()
                .map(|(i, (_, caption))| {
                    serde_json::json!({
                        "type": "photo",
                        "media": format!("attach://photo{}", i),
                        "caption": caption
                    })
                })
                .collect();

            let mut form = reqwest::multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .text("media", serde_json::to_string(&media_json).unwrap_or_default());

            for (i, (bytes, _)) in chunk.iter().enumerate() {
                let part = reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name("image.png")
                    .mime_str("image/png")
                    .map_err(|e| e.to_string())?;
                form = form.part(format!("photo{}", i), part);
            }

            let resp = client
                .post(&url)
                .multipart(form)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("sendMediaGroup error: {}", body));
            }
        }
    }

    Ok(())
}

fn format_text_message(text_items: &[&ClipboardItem], total: usize) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let mut msg = format!(
        "<b>ClipboardX 收藏列表</b>\n{} · 共 {} 条\n\n",
        now, total
    );

    for (i, item) in text_items.iter().enumerate() {
        let content = html_escape(item.text_content.as_deref().unwrap_or("").trim());
        msg.push_str(&format!("{}. <code>{}</code>\n", i + 1, content));
        if let Some(note) = &item.note {
            if !note.is_empty() {
                msg.push_str(&format!("备注：{}\n", html_escape(note)));
            }
        }
        let time_str = item
            .created_at
            .get(..16)
            .unwrap_or(&item.created_at)
            .replace('T', " ");
        msg.push_str(&format!("{}\n\n", time_str));
    }

    msg
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
