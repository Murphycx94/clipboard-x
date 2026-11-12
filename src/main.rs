//! The desktop app: the UI of the `clipboardx` crate in a window of its
//! own. On macOS a floating panel that a global shortcut and a menu-bar
//! icon show and hide; elsewhere NSH's standard window.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "android"))]
use std::process::ExitCode;

#[cfg(not(target_os = "android"))]
use clipboardx::services::services;

/// The window's size in points.
const WINDOW_SIZE: (i32, i32) = (420, 600);

#[cfg(not(target_os = "android"))]
fn main() -> ExitCode {
    clipboardx::startup();
    services().start_capture();
    services().start_sync_worker();
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

/// Android runs no executable of ours: NSH's host mounts the `App` of the
/// library (`nsh.json` `"root"`).
#[cfg(target_os = "android")]
fn main() {}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
)))]
compile_error!("ClipboardX runs on macOS, Windows, Linux and Android.");
