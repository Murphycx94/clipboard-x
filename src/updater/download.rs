//! Fetching a release file, checked against the size and SHA-256 GitHub
//! reports before it is installed.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::Asset;

/// How long the connection may stay silent, connecting or downloading.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK: usize = 64 * 1024;

/// How far a download is, for the UI to read from another thread.
#[derive(Debug, Default)]
pub struct Progress {
    received: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    /// Percent done, 0 to 100; 0 while the size is unknown.
    pub fn percent(&self) -> u8 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0;
        }
        let received = self.received.load(Ordering::Relaxed).min(total);
        (received * 100 / total) as u8
    }
}

/// Downloads `asset` into `dir` and checks it. Blocking. Returns the
/// file's path.
pub fn download(asset: &Asset, dir: &Path, progress: &Progress) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(STALL_TIMEOUT)
        // Per read: a slow download goes on, a stalled one fails.
        .timeout(STALL_TIMEOUT)
        .user_agent(concat!("ClipboardX/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(&asset.url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("download answered {}", response.status()));
    }
    let dest = dir.join(file_name(&asset.name)?);
    save(response, &dest, asset, progress)?;
    Ok(dest)
}

/// The asset's name as a file name: nothing that leaves the directory.
fn file_name(name: &str) -> Result<&str, String> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    plain
        .then_some(name)
        .ok_or_else(|| format!("unexpected file name {name:?}"))
}

/// Writes `body` to `dest` through a `.part` file, renamed only once its
/// size and SHA-256 are the asset's.
fn save(
    mut body: impl Read,
    dest: &Path,
    asset: &Asset,
    progress: &Progress,
) -> Result<(), String> {
    let part = dest.with_extension("part");
    let written = write_checked(&mut body, &part, asset, progress);
    if let Err(e) = written {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dest).map_err(|e| format!("cannot keep the download: {e}"))
}

fn write_checked(
    body: &mut impl Read,
    part: &Path,
    asset: &Asset,
    progress: &Progress,
) -> Result<(), String> {
    progress.total.store(asset.size, Ordering::Relaxed);
    progress.received.store(0, Ordering::Relaxed);
    let file = File::create(part).map_err(|e| format!("cannot write the download: {e}"))?;
    let mut out = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let n = body
            .read(&mut buffer)
            .map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > asset.size {
            return Err("the download is larger than announced".to_owned());
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n])
            .map_err(|e| format!("cannot write the download: {e}"))?;
        progress.received.store(received, Ordering::Relaxed);
    }
    if received != asset.size {
        return Err(format!(
            "the download stopped at {received} of {} bytes",
            asset.size
        ));
    }
    let digest = hex::encode(hasher.finalize());
    if digest != asset.sha256 {
        return Err("the download's checksum does not match".to_owned());
    }
    let file = out
        .into_inner()
        .map_err(|e| format!("cannot write the download: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot write the download: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = b"the new app";

    fn asset(sha256: String, size: u64) -> Asset {
        Asset {
            name: "ClipboardX-9.9.9-macos-universal.zip".to_owned(),
            url: String::new(),
            sha256,
            size,
        }
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn keeps_a_download_that_matches() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app.zip");
        let progress = Progress::default();
        save(BODY, &dest, &asset(sha(BODY), BODY.len() as u64), &progress).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), BODY);
        assert_eq!(progress.percent(), 100);
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn drops_a_download_that_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app.zip");
        let size = BODY.len() as u64;
        let progress = Progress::default();
        let cases = [
            asset(sha(b"something else"), size),
            asset(sha(BODY), size + 1),
            asset(sha(BODY), size - 1),
        ];
        for case in cases {
            assert!(save(BODY, &dest, &case, &progress).is_err());
            assert!(!dest.exists());
            assert!(!dest.with_extension("part").exists());
        }
    }

    #[test]
    fn file_names_stay_in_the_directory() {
        assert!(file_name("ClipboardX-0.4.3-android.apk").is_ok());
        for bad in ["", "../x.apk", "a/b.zip", "a\\b.exe", ".hidden", "x y.zip"] {
            assert!(file_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn progress_without_a_size_is_zero() {
        assert_eq!(Progress::default().percent(), 0);
    }
}
