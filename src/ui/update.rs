//! Updating the app: a quiet check soon after the start and every few
//! hours, the header's 更新 button once a newer version is out, and what
//! that button does (see `crate::updater`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nsh_core::prelude::*;

use super::state::AppState;
use crate::updater::{self, Asset, Installed, Progress, Release};

/// The first check, after the start has settled.
const FIRST_CHECK: Duration = Duration::from_secs(10);
/// Then one every so often, for an app that stays up for days.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How often the button's percentage follows the download.
const PROGRESS_POLL: Duration = Duration::from_millis(200);
/// How long an installed update waits for the app to quit before telling
/// the user to restart it.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// Where an update the user started is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    /// Downloading, percent done.
    Downloading(u8),
    /// Putting the new version in place; the app restarts next.
    Installing,
    /// Android: the APK is in the system's Downloads, for the user to
    /// install from there.
    InDownloads,
}

impl AppState {
    /// Looks for a newer version soon after the start, then every few
    /// hours, quietly: a newer one brings up the header's button.
    pub fn watch_updates(self) {
        // Before any check, so never under an update the user started.
        updater::remove_leftovers();
        if !updater::checks_automatically() {
            return;
        }
        set_timeout(FIRST_CHECK, move || self.look_for_update(false));
        set_interval(CHECK_EVERY, move || self.look_for_update(false));
    }

    /// Settings' 检查更新: says what it found.
    pub fn check_update(self) {
        self.look_for_update(true);
    }

    fn look_for_update(self, asked: bool) {
        if self.checking_update.get() || updating(self) {
            return;
        }
        self.checking_update.set(true);
        spawn(async move {
            let result = blocking(updater::check)
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            self.checking_update.set(false);
            match result {
                Ok(Some(release)) => {
                    let version = release.version.clone();
                    let known = self
                        .release
                        .with(|r| r.as_ref().is_some_and(|r| r.version == version));
                    if !known {
                        // An APK in Downloads was for an older one.
                        self.update_phase.set(UpdatePhase::Idle);
                        self.release.set(Some(release));
                    }
                    if asked {
                        self.success(format!("发现新版本 v{version}，点右上角「更新」"));
                    }
                }
                Ok(None) => {
                    if asked {
                        self.success("当前已是最新版本");
                    }
                }
                Err(e) => {
                    log::warn!("update check failed: {e}");
                    if asked {
                        self.error("检查更新失败，请稍后重试");
                    }
                }
            }
        });
    }

    /// The 更新 button: downloads the new version, installs it and
    /// restarts. Where the app cannot update itself, the release's page
    /// opens instead.
    pub fn start_update(self) {
        let Some(release) = self.release.get() else {
            return;
        };
        match self.update_phase.get() {
            UpdatePhase::Idle => {}
            UpdatePhase::InDownloads => return reopen_downloads(self),
            UpdatePhase::Downloading(_) | UpdatePhase::Installing => return,
        }
        let asset = match &release.asset {
            Some(asset) if updater::can_install(&release) => asset.clone(),
            _ => {
                drop(open_url(release.url));
                return;
            }
        };
        self.update_phase.set(UpdatePhase::Downloading(0));
        spawn(async move {
            match self.download(asset.clone()).await {
                Ok(file) => self.install(release, asset, file).await,
                Err(e) => {
                    log::error!("cannot download v{}: {e}", release.version);
                    self.update_phase.set(UpdatePhase::Idle);
                    self.error("下载更新失败，请稍后重试");
                }
            }
        });
    }

    /// Downloads `asset`, the button showing how far.
    async fn download(self, asset: Asset) -> Result<PathBuf, String> {
        let progress = Arc::new(Progress::default());
        let job = {
            let progress = Arc::clone(&progress);
            blocking(move || updater::download(&asset, &updater::downloads_dir(), &progress))
        };
        while !job.is_finished() {
            self.update_phase
                .set(UpdatePhase::Downloading(progress.percent()));
            timer(PROGRESS_POLL).await;
        }
        job.await.map_err(|e| e.to_string())?
    }

    async fn install(self, release: Release, asset: Asset, file: PathBuf) {
        self.update_phase.set(UpdatePhase::Installing);
        let installed = blocking(move || updater::install(&file, &asset))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match installed {
            Ok(Installed::Quit) => {
                log::info!("v{} installed: restarting", release.version);
                if crate::services::services().quit_app() {
                    // Still here after a while: the quit did not happen.
                    set_timeout(QUIT_WAIT, move || self.installed_restart_by_hand());
                } else {
                    self.installed_restart_by_hand();
                }
            }
            Ok(Installed::RestartByHand) => self.installed_restart_by_hand(),
            Ok(Installed::InDownloads { opened }) => {
                self.update_phase.set(UpdatePhase::InDownloads);
                if opened {
                    self.success("安装包已存到「下载」，点它安装");
                } else {
                    self.success("安装包已存到「下载」，请在文件管理中打开它安装");
                }
            }
            Err(e) => {
                log::error!("cannot install v{}: {e}", release.version);
                self.update_phase.set(UpdatePhase::Idle);
                self.error("自动更新失败，已打开下载页");
                drop(open_url(release.url));
            }
        }
    }

    /// The new version is in place but this one runs on.
    fn installed_restart_by_hand(self) {
        log::warn!("the update is installed; the app did not restart");
        self.update_phase.set(UpdatePhase::Idle);
        self.release.set(None);
        self.success("新版本已安装，重启 ClipboardX 后生效");
    }
}

/// What the update buttons say; `long` names the version (settings).
pub fn button_label(state: AppState, long: bool) -> String {
    match state.update_phase.get() {
        UpdatePhase::Idle if long => state
            .release
            .with(|r| r.as_ref().map(|r| format!("更新到 v{}", r.version)))
            .unwrap_or_else(|| "更新".to_owned()),
        UpdatePhase::Idle => "更新".to_owned(),
        UpdatePhase::Downloading(percent) => format!("下载中 {percent}%"),
        UpdatePhase::Installing => "安装中…".to_owned(),
        UpdatePhase::InDownloads => "安装".to_owned(),
    }
}

/// The update button's hint: the version and the start of its notes.
pub fn release_hint(release: &Release) -> String {
    const LINES: usize = 4;
    const LINE_CHARS: usize = 60;
    let mut hint = format!("新版本 v{}", release.version);
    let lines = release
        .notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(LINES);
    for line in lines {
        hint.push('\n');
        if line.chars().count() > LINE_CHARS {
            hint.extend(line.chars().take(LINE_CHARS));
            hint.push('…');
        } else {
            hint.push_str(line);
        }
    }
    hint
}

/// Whether an update is under way (the buttons wait).
pub fn updating(state: AppState) -> bool {
    matches!(
        state.update_phase.get(),
        UpdatePhase::Downloading(_) | UpdatePhase::Installing
    )
}

#[cfg(target_os = "android")]
fn reopen_downloads(state: AppState) {
    if let Err(e) = updater::open_downloads() {
        log::error!("cannot open Downloads: {e}");
        state.error("无法打开「下载」，请在文件管理里找到安装包");
    }
}

#[cfg(not(target_os = "android"))]
fn reopen_downloads(_state: AppState) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(notes: &str) -> Release {
        Release {
            version: "0.4.3".to_owned(),
            notes: notes.to_owned(),
            url: String::new(),
            asset: None,
        }
    }

    #[test]
    fn the_hint_names_the_version_and_the_notes_start() {
        assert_eq!(release_hint(&release("")), "新版本 v0.4.3");
        let notes = "## 改动\n\n- 一\n- 二\n- 三\n- 四";
        assert_eq!(
            release_hint(&release(notes)),
            "新版本 v0.4.3\n## 改动\n- 一\n- 二\n- 三"
        );
        let long = "长".repeat(80);
        let hint = release_hint(&release(&long));
        assert!(hint.ends_with('…'));
        assert_eq!(hint.lines().nth(1).unwrap().chars().count(), 61);
    }
}
