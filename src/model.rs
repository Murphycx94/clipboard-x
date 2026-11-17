//! The app's data: clipboard items, archive groups and settings, with the
//! pure helpers the UI and the services share (filtering, grouping,
//! masking, labels).

use std::fmt;

/// What a clipboard item holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    Text,
    Image,
}

impl ContentType {
    /// The value stored in the `content_type` column.
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Text => "text",
            ContentType::Image => "image",
        }
    }

    /// Parses a stored `content_type`; anything but `"image"` is text.
    pub fn parse(value: &str) -> Self {
        if value == "image" {
            ContentType::Image
        } else {
            ContentType::Text
        }
    }
}

/// One entry of the clipboard history, as lists show it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipboardItem {
    pub id: i64,
    pub content_type: ContentType,
    /// The first characters of the text (lists never load all of it).
    pub text_preview: Option<String>,
    /// A small PNG of the image, as a `data:` URI.
    pub thumbnail: Option<String>,
    /// Local time, `YYYY-MM-DDTHH:MM:SS`.
    pub created_at: String,
    pub is_favorite: bool,
    pub note: Option<String>,
}

impl ClipboardItem {
    /// The item's note when it has a non-empty one.
    pub fn note_text(&self) -> Option<&str> {
        self.note.as_deref().filter(|n| !n.trim().is_empty())
    }

    /// `YYYY-MM-DD` of `created_at`.
    pub fn date(&self) -> &str {
        self.created_at.get(..10).unwrap_or(&self.created_at)
    }
}

/// The items copied on one day, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct DateGroup {
    pub date: String,
    pub items: Vec<ClipboardItem>,
}

/// The appearance the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    Light,
    Dark,
    #[default]
    System,
}

impl ThemePreference {
    pub const ALL: [ThemePreference; 3] = [Self::Light, Self::Dark, Self::System];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "浅色",
            Self::Dark => "深色",
            Self::System => "跟随系统",
        }
    }
}

/// How long non-favorite items are kept: (days, label); 0 keeps them forever.
pub const RETENTION_OPTIONS: [(i64, &str); 4] =
    [(7, "一周"), (30, "一个月"), (365, "一年"), (0, "永久")];

/// Idle minutes before the vault locks: (minutes, label).
pub const VAULT_LOCK_OPTIONS: [(i64, &str); 4] = [
    (1, "1 分钟"),
    (5, "5 分钟"),
    (15, "15 分钟"),
    (30, "30 分钟"),
];

/// The default global shortcut. Not Ctrl+Shift+V on Windows: a global
/// shortcut takes its keys from every app, and there those paste as plain
/// text in browsers, editors and terminals.
pub const DEFAULT_HOTKEY: &str = if cfg!(target_os = "windows") {
    "Alt+Shift+V"
} else {
    "CmdOrCtrl+Shift+V"
};

/// The user's settings, as stored.
#[derive(Clone, PartialEq)]
pub struct Settings {
    pub hotkey: String,
    /// 0 keeps history forever.
    pub retention_days: i64,
    pub telegram_token: String,
    pub telegram_chat_id: String,
    pub theme: ThemePreference,
    pub hide_on_blur: bool,
    /// Keep the window out of screenshots, recordings and screen sharing.
    pub hide_from_capture: bool,
    /// Idle minutes before the vault locks.
    pub vault_lock_minutes: i64,
    /// Lock the vault when the panel hides.
    pub vault_lock_on_hide: bool,
    /// Unlock the vault with Touch ID.
    pub vault_touch_id: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: DEFAULT_HOTKEY.to_owned(),
            retention_days: 7,
            telegram_token: String::new(),
            telegram_chat_id: String::new(),
            theme: ThemePreference::System,
            hide_on_blur: true,
            hide_from_capture: true,
            vault_lock_minutes: 5,
            vault_lock_on_hide: true,
            vault_touch_id: false,
        }
    }
}

impl Settings {
    /// Whether both Telegram fields are filled in.
    pub fn telegram_configured(&self) -> bool {
        !self.telegram_token.is_empty() && !self.telegram_chat_id.is_empty()
    }
}

// The token is a secret: keep it out of debug output and logs.
impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("hotkey", &self.hotkey)
            .field("retention_days", &self.retention_days)
            .field("telegram_token", &mask_token(&self.telegram_token))
            .field("telegram_chat_id", &self.telegram_chat_id)
            .field("theme", &self.theme)
            .field("hide_on_blur", &self.hide_on_blur)
            .field("hide_from_capture", &self.hide_from_capture)
            .field("vault_lock_minutes", &self.vault_lock_minutes)
            .field("vault_lock_on_hide", &self.vault_lock_on_hide)
            .field("vault_touch_id", &self.vault_touch_id)
            .finish()
    }
}

/// The items matching `query` (in the text or the note, ignoring case),
/// only images when `images_only`.
pub fn filter_items(items: &[ClipboardItem], query: &str, images_only: bool) -> Vec<ClipboardItem> {
    let query = query.trim().to_lowercase();
    items
        .iter()
        .filter(|item| !images_only || item.content_type == ContentType::Image)
        .filter(|item| {
            if query.is_empty() {
                return true;
            }
            let contains = |s: &Option<String>| {
                s.as_deref()
                    .is_some_and(|s| s.to_lowercase().contains(&query))
            };
            contains(&item.text_preview) || contains(&item.note)
        })
        .cloned()
        .collect()
}

/// Groups items (newest first) by their day, keeping their order.
pub fn group_by_date(items: Vec<ClipboardItem>) -> Vec<DateGroup> {
    let mut groups: Vec<DateGroup> = Vec::new();
    for item in items {
        let date = item.date().to_owned();
        match groups.last_mut() {
            Some(group) if group.date == date => group.items.push(item),
            _ => groups.push(DateGroup {
                date,
                items: vec![item],
            }),
        }
    }
    groups
}

/// A row of the archive's one list, pointing into its groups (no copy of
/// the items): a day's header, or an item of a day that is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRow {
    /// The header of `groups[group]`.
    Day { group: usize },
    /// `groups[group].items[index]`.
    Item { group: usize, index: usize },
}

/// What a row of the archive shows, kept as it moves: a new day archived
/// at the top shifts the positions, not the keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArchiveKey {
    Day(String),
    Item(i64),
}

/// The archive as one list: each day's header, its items after it when
/// the day is in `open`.
pub fn archive_rows(groups: &[DateGroup], open: &[String]) -> Vec<ArchiveRow> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(group, g)| {
            let items = if open.contains(&g.date) {
                g.items.len()
            } else {
                0
            };
            std::iter::once(ArchiveRow::Day { group })
                .chain((0..items).map(move |index| ArchiveRow::Item { group, index }))
        })
        .collect()
}

/// The key of `row` in `groups` (`None` for a row they no longer have).
pub fn archive_row_key(groups: &[DateGroup], row: ArchiveRow) -> Option<ArchiveKey> {
    match row {
        ArchiveRow::Day { group } => groups.get(group).map(|g| ArchiveKey::Day(g.date.clone())),
        ArchiveRow::Item { group, index } => groups
            .get(group)
            .and_then(|g| g.items.get(index))
            .map(|i| ArchiveKey::Item(i.id)),
    }
}

/// `abcd****wxyz` for a token, `****` for a short one, `""` for none.
pub fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    match chars.len() {
        0 => String::new(),
        1..=8 => "****".to_owned(),
        n => {
            let prefix: String = chars[..4].iter().collect();
            let suffix: String = chars[n - 4..].iter().collect();
            format!("{prefix}****{suffix}")
        }
    }
}

/// `HH:MM` of a stored timestamp.
pub fn time_label(created_at: &str) -> &str {
    created_at.get(11..16).unwrap_or("")
}

/// `YYYY-MM-DD HH:MM` of a stored timestamp.
pub fn minute_label(created_at: &str) -> String {
    created_at.get(..16).unwrap_or(created_at).replace('T', " ")
}

/// The label of the shortcut that copies the `index`-th item (1 to 9).
pub fn shortcut_label(index: usize, apple: bool) -> String {
    if apple {
        format!("⌘⇧{index}")
    } else {
        format!("Ctrl+Shift+{index}")
    }
}

/// The hotkey as the user sees it on this platform.
pub fn hotkey_label(hotkey: &str, apple: bool) -> String {
    let platform = if apple { "Cmd" } else { "Ctrl" };
    hotkey
        .split('+')
        .map(|part| {
            if part.eq_ignore_ascii_case("cmdorctrl") {
                platform
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        id: i64,
        kind: ContentType,
        text: Option<&str>,
        note: Option<&str>,
        at: &str,
    ) -> ClipboardItem {
        ClipboardItem {
            id,
            content_type: kind,
            text_preview: text.map(str::to_owned),
            thumbnail: None,
            created_at: at.to_owned(),
            is_favorite: false,
            note: note.map(str::to_owned),
        }
    }

    #[test]
    fn content_type_round_trips_and_defaults_to_text() {
        assert_eq!(
            ContentType::parse(ContentType::Image.as_str()),
            ContentType::Image
        );
        assert_eq!(
            ContentType::parse(ContentType::Text.as_str()),
            ContentType::Text
        );
        assert_eq!(ContentType::parse("other"), ContentType::Text);
    }

    #[test]
    fn theme_preference_round_trips_and_defaults_to_system() {
        for theme in ThemePreference::ALL {
            assert_eq!(ThemePreference::parse(theme.as_str()), theme);
        }
        assert_eq!(ThemePreference::parse(""), ThemePreference::System);
    }

    #[test]
    fn filter_matches_text_or_note_ignoring_case() {
        let items = vec![
            item(
                1,
                ContentType::Text,
                Some("Hello World"),
                None,
                "2026-01-01T10:00:00",
            ),
            item(
                2,
                ContentType::Text,
                Some("other"),
                Some("WORLD note"),
                "2026-01-01T10:00:00",
            ),
            item(3, ContentType::Image, None, None, "2026-01-01T10:00:00"),
        ];
        let ids = |v: Vec<ClipboardItem>| v.iter().map(|i| i.id).collect::<Vec<_>>();
        assert_eq!(ids(filter_items(&items, "world", false)), vec![1, 2]);
        assert_eq!(ids(filter_items(&items, "  ", false)), vec![1, 2, 3]);
        assert_eq!(ids(filter_items(&items, "", true)), vec![3]);
        assert_eq!(ids(filter_items(&items, "world", true)), Vec::<i64>::new());
    }

    #[test]
    fn groups_keep_order_and_split_by_day() {
        let items = vec![
            item(1, ContentType::Text, Some("a"), None, "2026-03-17T12:00:00"),
            item(2, ContentType::Text, Some("b"), None, "2026-03-17T09:00:00"),
            item(3, ContentType::Text, Some("c"), None, "2026-03-15T09:00:00"),
        ];
        let groups = group_by_date(items);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].date, "2026-03-17");
        assert_eq!(
            groups[0].items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(groups[1].date, "2026-03-15");
        assert!(group_by_date(Vec::new()).is_empty());
    }

    #[test]
    fn archive_rows_list_each_day_and_the_items_of_open_days() {
        let groups = group_by_date(vec![
            item(1, ContentType::Text, Some("a"), None, "2026-03-17T12:00:00"),
            item(2, ContentType::Text, Some("b"), None, "2026-03-17T09:00:00"),
            item(3, ContentType::Text, Some("c"), None, "2026-03-15T09:00:00"),
        ]);

        let closed = archive_rows(&groups, &[]);
        assert_eq!(
            closed,
            vec![ArchiveRow::Day { group: 0 }, ArchiveRow::Day { group: 1 }]
        );

        let open = archive_rows(&groups, &["2026-03-17".to_owned()]);
        assert_eq!(
            open,
            vec![
                ArchiveRow::Day { group: 0 },
                ArchiveRow::Item { group: 0, index: 0 },
                ArchiveRow::Item { group: 0, index: 1 },
                ArchiveRow::Day { group: 1 },
            ]
        );
        assert!(archive_rows(&[], &["2026-03-17".to_owned()]).is_empty());
    }

    #[test]
    fn archive_row_keys_follow_the_day_and_the_item_not_the_position() {
        let groups = group_by_date(vec![
            item(7, ContentType::Text, Some("a"), None, "2026-03-17T12:00:00"),
            item(9, ContentType::Text, Some("c"), None, "2026-03-15T09:00:00"),
        ]);
        let key = |row| archive_row_key(&groups, row);
        assert_eq!(
            key(ArchiveRow::Day { group: 1 }),
            Some(ArchiveKey::Day("2026-03-15".to_owned()))
        );
        assert_eq!(
            key(ArchiveRow::Item { group: 0, index: 0 }),
            Some(ArchiveKey::Item(7))
        );
        assert_eq!(key(ArchiveRow::Item { group: 0, index: 5 }), None);
        assert_eq!(key(ArchiveRow::Day { group: 2 }), None);
    }

    #[test]
    fn masks_tokens_by_length() {
        assert_eq!(mask_token(""), "");
        assert_eq!(mask_token("12345678"), "****");
        assert_eq!(mask_token("123456789:ABCDEFG"), "1234****DEFG");
    }

    #[test]
    fn settings_debug_hides_the_token() {
        let settings = Settings {
            telegram_token: "123456789:SECRETVALUE".into(),
            ..Settings::default()
        };
        let shown = format!("{settings:?}");
        assert!(!shown.contains("SECRETVALUE"));
        assert!(!settings.telegram_configured());
    }

    #[test]
    fn labels() {
        assert_eq!(time_label("2026-03-17T14:32:05"), "14:32");
        assert_eq!(time_label("bad"), "");
        assert_eq!(minute_label("2026-03-17T14:32:05"), "2026-03-17 14:32");
        assert_eq!(shortcut_label(3, true), "⌘⇧3");
        assert_eq!(shortcut_label(3, false), "Ctrl+Shift+3");
        assert_eq!(hotkey_label("CmdOrCtrl+Shift+V", true), "Cmd+Shift+V");
        assert_eq!(hotkey_label("CmdOrCtrl+Shift+V", false), "Ctrl+Shift+V");
    }

    #[test]
    fn note_text_ignores_blank_notes() {
        let mut it = item(
            1,
            ContentType::Text,
            Some("a"),
            Some("  "),
            "2026-03-17T14:32:05",
        );
        assert_eq!(it.note_text(), None);
        it.note = Some("note".into());
        assert_eq!(it.note_text(), Some("note"));
        assert_eq!(it.date(), "2026-03-17");
    }
}
