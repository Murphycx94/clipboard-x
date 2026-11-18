//! Windows: the release's installer runs silently over the installed app
//! (`scripts/windows-installer.iss`): it closes this app if it still runs,
//! keeps the user's choices (the start with Windows), and with `/relaunch=1`
//! starts the new version. Downloaded by the app, not a browser, it has no
//! mark of the web, so SmartScreen does not stop it.

use std::path::Path;
use std::process::Command;

/// The uninstaller Inno Setup puts beside the app it installs.
const UNINSTALLER: &str = "unins000.exe";

/// Whether the installer put this app where it runs; a copy unpacked from
/// the zip has no uninstaller beside it and is updated by hand.
pub fn installed_by_setup() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(UNINSTALLER).is_file()))
        .unwrap_or(false)
}

/// Starts the installer at `setup`; the caller quits next.
pub fn run_setup(setup: &Path) -> Result<(), String> {
    Command::new(setup)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/relaunch=1",
        ])
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start the installer: {e}"))
}
