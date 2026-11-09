//! A vault entry (secret, zeroized) and its summary (no secrets) for lists.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// One account, as encrypted in the database.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct VaultEntry {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub url: String,
    /// An `otpauth://` URI or a Base32 secret; empty without one.
    #[serde(default)]
    pub totp: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

impl fmt::Debug for VaultEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultEntry")
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

impl VaultEntry {
    pub fn summary(&self, id: &str) -> EntrySummary {
        EntrySummary {
            id: id.to_owned(),
            title: self.title.clone(),
            username: self.username.clone(),
            host: host_of(&self.url),
            has_totp: !self.totp.trim().is_empty(),
            corrupt: false,
        }
    }
}

/// What a list shows of an entry: never its password, TOTP secret or note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySummary {
    pub id: String,
    pub title: String,
    pub username: String,
    pub host: String,
    pub has_totp: bool,
    /// It failed to decrypt.
    pub corrupt: bool,
}

impl EntrySummary {
    pub fn corrupt(id: &str) -> EntrySummary {
        EntrySummary {
            id: id.to_owned(),
            title: "（已损坏）".to_owned(),
            username: String::new(),
            host: String::new(),
            has_totp: false,
            corrupt: true,
        }
    }

    /// Whether `query` is in the title, the username or the host.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [&self.title, &self.username, &self.host]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
    }
}

/// `example.com` of `https://user@www.example.com/path`.
pub fn host_of(url: &str) -> String {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host.strip_prefix("www.").unwrap_or(host).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> VaultEntry {
        let mut e = VaultEntry::default();
        e.title = "GitHub".into();
        e.username = "octo@example.com".into();
        e.password = "hunter2-secret".into();
        e.url = "https://www.github.com/login?x=1".into();
        e.totp = "JBSWY3DPEHPK3PXP".into();
        e.note = "work".into();
        e
    }

    #[test]
    fn summary_has_no_secrets() {
        let summary = entry().summary("id-1");
        assert_eq!(summary.title, "GitHub");
        assert_eq!(summary.host, "github.com");
        assert!(summary.has_totp);
        assert!(!format!("{summary:?}").contains("hunter2"));
    }

    #[test]
    fn debug_hides_secrets() {
        let shown = format!("{:?}", entry());
        assert!(!shown.contains("hunter2") && !shown.contains("JBSWY3DP"));
    }

    #[test]
    fn hosts() {
        assert_eq!(
            host_of("https://user:pw@www.example.com:8443/a"),
            "example.com:8443"
        );
        assert_eq!(host_of("example.com/path"), "example.com");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn search_matches_title_username_and_host() {
        let summary = entry().summary("id-1");
        assert!(summary.matches("git"));
        assert!(summary.matches("OCTO"));
        assert!(summary.matches("github.com"));
        assert!(summary.matches("  "));
        assert!(!summary.matches("hunter2"));
    }

    #[test]
    fn json_round_trip_and_unknown_fields_default() {
        let json = serde_json::to_vec(&entry()).unwrap();
        let back: VaultEntry = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, entry());
        let partial: VaultEntry = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
        assert_eq!(partial.title, "x");
        assert!(partial.password.is_empty());
    }
}
