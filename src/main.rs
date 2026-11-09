//! The desktop app: the UI of the `clipboardx` crate in a window of its
//! own. On macOS a floating panel that a global shortcut and a menu-bar
//! icon show and hide; elsewhere NSH's standard window.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;

use std::process::ExitCode;

use clipboardx::services::{data_dir, services};

/// The window's size in points.
const WINDOW_SIZE: (i32, i32) = (420, 600);

fn main() -> ExitCode {
    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
    }
    clipboardx::logger::init(&dir);
    log::info!("ClipboardX {} starting", env!("CARGO_PKG_VERSION"));
    services().start_capture();
    run()
}

#[cfg(target_os = "macos")]
fn run() -> ExitCode {
    macos::run(WINDOW_SIZE)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn run() -> ExitCode {
    #[cfg(target_os = "linux")]
    use nsh_platform_linux as platform;
    #[cfg(target_os = "windows")]
    use nsh_platform_windows as platform;

    platform::run_app(
        platform::AppSource::Rust(clipboardx::root()),
        platform::AppOptions {
            title: "ClipboardX".to_owned(),
            app_id: clipboardx::services::APP_ID.to_owned(),
            size: WINDOW_SIZE,
            ..Default::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("ClipboardX runs on macOS, Windows and Linux.");
