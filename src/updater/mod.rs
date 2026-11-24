//! Updating the app from GitHub Releases: [`check`] finds a newer release
//! and, in it, the file this platform installs ([`Asset`]);
//! [`download`] fetches it and checks it against the SHA-256 GitHub
//! reports; [`install`] puts it in place.
//!
//! - macOS: the release's `.app` zip replaces the running app, which then
//!   starts again.
//! - Windows: the installer runs silently and starts the app again; a copy
//!   unpacked from the zip (no uninstaller beside it) goes to the release
//!   page instead.
//! - Android: the APK goes to the system's Downloads, which opens for the
//!   user to install it (NSH's manifest cannot ask for the permission to
//!   start the installer itself). Before Android 10 the browser downloads
//!   it.
//!
//! What cannot update itself (Linux, an app on a read-only volume, a
//! release without a file for this platform) opens the release page.

mod download;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

pub use download::{Progress, download};

const LATEST_RELEASE: &str = "https://api.github.com/repos/Murphycx94/clipboard-x/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(15);

/// This build's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The end of the name of the release file this platform installs.
const ASSET_SUFFIX: Option<&str> = if cfg!(target_os = "macos") {
    Some("-macos-universal.zip")
} else if cfg!(target_os = "windows") {
    Some("-windows-x86_64-setup.exe")
} else if cfg!(target_os = "android") {
    Some("-android.apk")
} else {
    None
};

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    /// The release's page.
    pub url: String,
    /// The file this platform installs, when the release has one with a
    /// checksum.
    pub asset: Option<Asset>,
}

/// A file of a release.
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`; releases from before GitHub kept digests have none.
    #[serde(default)]
    digest: Option<String>,
    size: u64,
}

fn parse_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// The hex of a `sha256:<hex>` digest, lowercase.
fn sha256_of(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_lowercase())
}

/// The asset whose name ends with `suffix`, if it has a checksum and an
/// HTTPS address.
fn pick_asset(assets: Vec<GithubAsset>, suffix: Option<&str>) -> Option<Asset> {
    let suffix = suffix?;
    let asset = assets
        .into_iter()
        .find(|a| a.name.ends_with(suffix))
        .filter(|a| a.browser_download_url.starts_with("https://"))?;
    Some(Asset {
        sha256: sha256_of(asset.digest.as_deref()?)?,
        name: asset.name,
        url: asset.browser_download_url,
        size: asset.size,
    })
}

/// The release described by GitHub's JSON when it is newer than `current`,
/// with the asset whose name ends with `suffix`.
fn newer_release(
    json: &str,
    current: &str,
    suffix: Option<&str>,
) -> Result<Option<Release>, String> {
    let release: GithubRelease = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| format!("unexpected tag {:?}", release.tag_name))?;
    let current = parse_version(current).ok_or_else(|| format!("bad version {current:?}"))?;
    Ok((latest > current).then(|| Release {
        version: latest.to_string(),
        notes: release.body.unwrap_or_default().trim().to_owned(),
        url: release.html_url,
        asset: pick_asset(release.assets, suffix),
    }))
}

/// Set to anything to skip the automatic checks (tests).
pub const NO_UPDATE_CHECK_ENV: &str = "CLIPBOARDX_NO_UPDATE_CHECK";
/// A file holding the latest release as GitHub's API describes it, read
/// instead of asking GitHub (tests; debug builds only).
pub const RELEASE_JSON_ENV: &str = "CLIPBOARDX_RELEASE_JSON";

/// Whether the app looks for updates by itself.
pub fn checks_automatically() -> bool {
    std::env::var_os(NO_UPDATE_CHECK_ENV).is_none()
}

/// Asks GitHub for the latest release. Blocking: run it off the UI.
pub fn check() -> Result<Option<Release>, String> {
    // Debug builds (the tests) only: a release build asks GitHub.
    let fixture = std::env::var_os(RELEASE_JSON_ENV).filter(|_| cfg!(debug_assertions));
    let json = match fixture {
        Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        None => latest_release_json()?,
    };
    newer_release(&json, CURRENT_VERSION, ASSET_SUFFIX)
}

fn latest_release_json() -> Result<String, String> {
    let client = crate::net::http_client()
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
    response.text().map_err(|e| e.to_string())
}

/// Where downloads wait to be installed.
pub fn downloads_dir() -> PathBuf {
    crate::services::data_dir().join("updates")
}

/// Removes what earlier updates left: their downloads (a file still in
/// use, the Windows installer running, stays until the next time) and, on
/// macOS, what one interrupted left beside the app.
pub fn remove_leftovers() {
    if let Ok(entries) = std::fs::read_dir(downloads_dir()) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    #[cfg(target_os = "macos")]
    macos::remove_leftovers();
}

/// Whether this copy of the app can install `release` itself; else the
/// user gets its page.
pub fn can_install(release: &Release) -> bool {
    release.asset.is_some() && can_install_here()
}

#[cfg(target_os = "macos")]
fn can_install_here() -> bool {
    macos::can_install()
}

#[cfg(target_os = "windows")]
fn can_install_here() -> bool {
    windows::installed_by_setup()
}

#[cfg(target_os = "android")]
fn can_install_here() -> bool {
    crate::android::can_save_to_downloads()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "android")))]
fn can_install_here() -> bool {
    false
}

/// What [`install`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// The new version starts once this process has exited: quit now.
    Quit,
    /// The new version is in place but will not start by itself.
    RestartByHand,
    /// The user installs it from the system's Downloads, `opened` for
    /// them (else they find it in a file manager).
    InDownloads { opened: bool },
}

/// Installs the downloaded `file` of `asset`. Blocking.
pub fn install(file: &Path, asset: &Asset) -> Result<Installed, String> {
    install_here(file, asset)
}

#[cfg(target_os = "macos")]
fn install_here(file: &Path, _asset: &Asset) -> Result<Installed, String> {
    macos::install(file).map(|relaunches| {
        if relaunches {
            Installed::Quit
        } else {
            Installed::RestartByHand
        }
    })
}

#[cfg(target_os = "windows")]
fn install_here(file: &Path, _asset: &Asset) -> Result<Installed, String> {
    windows::run_setup(file).map(|()| Installed::Quit)
}

#[cfg(target_os = "android")]
fn install_here(file: &Path, asset: &Asset) -> Result<Installed, String> {
    crate::android::save_to_downloads(file, &asset.name)?;
    // Downloads has its own copy now.
    if let Err(e) = std::fs::remove_file(file) {
        log::warn!("cannot remove {}: {e}", file.display());
    }
    let opened = crate::android::open_downloads()
        .inspect_err(|e| log::error!("cannot open Downloads: {e}"))
        .is_ok();
    Ok(Installed::InDownloads { opened })
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "android")))]
fn install_here(_file: &Path, _asset: &Asset) -> Result<Installed, String> {
    Err("this platform does not update itself".to_owned())
}

/// Opens the system's Downloads again, for an APK [`install`] put there.
#[cfg(target_os = "android")]
pub fn open_downloads() -> Result<(), String> {
    crate::android::open_downloads()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:95EF5809728094a7d1a6a534d7a03a9c9773f834bf980a16afbe490ce7be1d13";

    fn json(digest: Option<&str>) -> String {
        let digest = digest.map_or("null".to_owned(), |d| format!("{d:?}"));
        format!(
            r#"{{"tag_name":"v0.3.0","body":" notes ","html_url":"https://example.com/r",
            "assets":[
              {{"name":"ClipboardX-0.3.0.dmg","browser_download_url":"https://example.com/dmg","digest":{digest},"size":1}},
              {{"name":"ClipboardX-0.3.0-macos-universal.zip","browser_download_url":"https://example.com/zip","digest":{digest},"size":42}}
            ]}}"#
        )
    }

    #[test]
    fn reports_a_newer_release_with_its_asset() {
        let release = newer_release(&json(Some(DIGEST)), "0.2.0", Some("-macos-universal.zip"))
            .unwrap()
            .unwrap();
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.notes, "notes");
        assert_eq!(release.url, "https://example.com/r");
        assert_eq!(
            release.asset,
            Some(Asset {
                name: "ClipboardX-0.3.0-macos-universal.zip".to_owned(),
                url: "https://example.com/zip".to_owned(),
                sha256: DIGEST[7..].to_lowercase(),
                size: 42,
            })
        );
    }

    #[test]
    fn an_asset_without_a_checksum_or_for_no_platform_is_none() {
        let suffix = Some("-macos-universal.zip");
        let no_digest = newer_release(&json(None), "0.2.0", suffix)
            .unwrap()
            .unwrap();
        assert_eq!(no_digest.asset, None);
        let bad_digest = newer_release(&json(Some("md5:00")), "0.2.0", suffix)
            .unwrap()
            .unwrap();
        assert_eq!(bad_digest.asset, None);
        let other = newer_release(&json(Some(DIGEST)), "0.2.0", Some("-android.apk"));
        assert_eq!(other.unwrap().unwrap().asset, None);
        let linux = newer_release(&json(Some(DIGEST)), "0.2.0", None);
        assert_eq!(linux.unwrap().unwrap().asset, None);
        let plain_http =
            json(Some(DIGEST)).replace("https://example.com/zip", "http://example.com/zip");
        let plain_http = newer_release(&plain_http, "0.2.0", suffix)
            .unwrap()
            .unwrap();
        assert_eq!(plain_http.asset, None);
    }

    #[test]
    fn a_release_without_assets_parses() {
        let bare = r#"{"tag_name":"v0.3.0","html_url":"u"}"#;
        let release = newer_release(bare, "0.2.0", ASSET_SUFFIX).unwrap().unwrap();
        assert_eq!(release.asset, None);
        assert!(!can_install(&release));
    }

    #[test]
    fn same_or_older_is_up_to_date() {
        let json = json(Some(DIGEST));
        assert_eq!(newer_release(&json, "0.3.0", ASSET_SUFFIX).unwrap(), None);
        assert_eq!(newer_release(&json, "1.0.0", ASSET_SUFFIX).unwrap(), None);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(newer_release("not json", "0.1.0", None).is_err());
        let bad_tag = r#"{"tag_name":"latest","html_url":"u"}"#;
        assert!(newer_release(bad_tag, "0.1.0", None).is_err());
        assert!(newer_release(&json(Some(DIGEST)), "x", None).is_err());
    }

    #[test]
    fn current_version_parses() {
        assert!(parse_version(CURRENT_VERSION).is_some());
    }
}
