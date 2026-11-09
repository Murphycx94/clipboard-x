//! Checking GitHub Releases for a newer version. Installing is left to the
//! user: the release page opens in the browser.

use std::time::Duration;

use serde::Deserialize;

const LATEST_RELEASE: &str = "https://api.github.com/repos/Murphycx94/clipboard-x/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(15);

/// This build's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
}

fn parse_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// The release described by GitHub's JSON when it is newer than `current`.
pub fn newer_release(json: &str, current: &str) -> Result<Option<Release>, String> {
    let release: GithubRelease = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| format!("unexpected tag {:?}", release.tag_name))?;
    let current = parse_version(current).ok_or_else(|| format!("bad version {current:?}"))?;
    Ok((latest > current).then(|| Release {
        version: latest.to_string(),
        notes: release.body.unwrap_or_default().trim().to_owned(),
        url: release.html_url,
    }))
}

/// Asks GitHub for the latest release. Blocking: run it off the UI.
pub fn check() -> Result<Option<Release>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("ClipboardX/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    newer_release(
        &response.text().map_err(|e| e.to_string())?,
        CURRENT_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str =
        r#"{"tag_name":"v0.3.0","body":" notes ","html_url":"https://example.com/r"}"#;

    #[test]
    fn reports_a_newer_release() {
        let release = newer_release(JSON, "0.2.0").unwrap().unwrap();
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.notes, "notes");
        assert_eq!(release.url, "https://example.com/r");
    }

    #[test]
    fn same_or_older_is_up_to_date() {
        assert_eq!(newer_release(JSON, "0.3.0").unwrap(), None);
        assert_eq!(newer_release(JSON, "1.0.0").unwrap(), None);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(newer_release("not json", "0.1.0").is_err());
        let bad_tag = r#"{"tag_name":"latest","html_url":"u"}"#;
        assert!(newer_release(bad_tag, "0.1.0").is_err());
        assert!(newer_release(JSON, "x").is_err());
    }

    #[test]
    fn current_version_parses() {
        assert!(parse_version(CURRENT_VERSION).is_some());
    }
}
