#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod database;
mod clipboard;
mod commands;

use database::Database;
use clipboard::start_clipboard_monitor;
use commands::*;
use std::sync::Arc;
use tauri::{
    Manager,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    PhysicalPosition, PhysicalSize,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const OFFSCREEN: PhysicalPosition<i32> = PhysicalPosition::new(-10000, -10000);

fn is_offscreen(window: &tauri::WebviewWindow) -> bool {
    window
        .outer_position()
        .map(|p| p.x < -9000)
        .unwrap_or(true)
}

fn toggle_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };

    // 用"是否在屏幕外"代替 is_visible 判断开/关状态
    if !is_offscreen(&window) {
        let _ = window.set_position(OFFSCREEN);
        return;
    }

    // 找到鼠标所在显示器，直接定位到目标位置再聚焦
    if let Ok(monitors) = window.available_monitors() {
        if let Ok(cursor_pos) = window.cursor_position() {
            let cursor_x = cursor_pos.x as i64;
            let cursor_y = cursor_pos.y as i64;

            let target_monitor = monitors.iter().find(|m| {
                let pos = m.position();
                let size = m.size();
                cursor_x >= pos.x as i64
                    && cursor_x < (pos.x as i64 + size.width as i64)
                    && cursor_y >= pos.y as i64
                    && cursor_y < (pos.y as i64 + size.height as i64)
            });

            if let Some(monitor) = target_monitor {
                let mon_pos = monitor.position();
                let mon_size = monitor.size();
                let win_size = window.outer_size().unwrap_or(PhysicalSize::new(420, 600));
                let margin = 16i32;

                let x = mon_pos.x + mon_size.width as i32 - win_size.width as i32 - margin;
                let y = mon_pos.y + (mon_size.height as i32 - win_size.height as i32) / 2;
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }
        }
    }

    let _ = window.set_focus();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // Init database
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let db_path = app_data_dir.join("clipboardx.db");
            let db = Arc::new(Database::new(db_path.to_str().unwrap())?);

            // Cleanup expired items on startup
            if let Ok(s) = db.get_settings() {
                let _ = db.cleanup_expired(s.retention_days);
            }

            // Start clipboard monitor
            start_clipboard_monitor(db.clone());

            // Register global hotkey (read from db settings)
            let hotkey_str = db.get_settings()
                .map(|s| s.hotkey)
                .unwrap_or_else(|_| "CmdOrCtrl+Shift+V".to_string());

            register_hotkey(app.handle(), &hotkey_str);

            app.manage(db);

            // System tray
            let show_item = MenuItemBuilder::with_id("show", "显示 ClipboardX").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let menu = MenuBuilder::new(app)
                .item(&show_item)
                .item(&separator)
                .item(&quit_item)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => toggle_window(app),
                    "quit" => std::process::exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // Hide from dock on macOS
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_today_items,
            get_archive_items,
            get_favorite_items,
            toggle_favorite,
            copy_to_clipboard,
            delete_item,
            get_settings,
            update_hotkey,
            update_note,
            get_image_base64,
            update_retention,
            clear_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn register_hotkey(app: &tauri::AppHandle, hotkey_str: &str) {
    // Parse "CmdOrCtrl+Shift+V" style strings
    let parts: Vec<&str> = hotkey_str.split('+').collect();
    let mut modifiers = Modifiers::empty();
    let mut key_code = Code::KeyV;

    for part in &parts {
        match part.to_lowercase().as_str() {
            "cmdorctrl" | "command" | "ctrl" | "control" => {
                #[cfg(target_os = "macos")]
                { modifiers |= Modifiers::SUPER; }
                #[cfg(not(target_os = "macos"))]
                { modifiers |= Modifiers::CONTROL; }
            }
            "shift" => { modifiers |= Modifiers::SHIFT; }
            "alt" | "option" => { modifiers |= Modifiers::ALT; }
            key => {
                key_code = str_to_code(key);
            }
        }
    }

    let shortcut = Shortcut::new(Some(modifiers), key_code);
    let app_handle = app.clone();

    if let Err(e) = app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
        if event.state == ShortcutState::Pressed {
            toggle_window(&app_handle);
        }
    }) {
        eprintln!("Failed to register hotkey '{}': {}", hotkey_str, e);
    } else {
        println!("Hotkey registered: {}", hotkey_str);
    }
}

fn str_to_code(s: &str) -> Code {
    match s.to_lowercase().as_str() {
        "a" => Code::KeyA, "b" => Code::KeyB, "c" => Code::KeyC,
        "d" => Code::KeyD, "e" => Code::KeyE, "f" => Code::KeyF,
        "g" => Code::KeyG, "h" => Code::KeyH, "i" => Code::KeyI,
        "j" => Code::KeyJ, "k" => Code::KeyK, "l" => Code::KeyL,
        "m" => Code::KeyM, "n" => Code::KeyN, "o" => Code::KeyO,
        "p" => Code::KeyP, "q" => Code::KeyQ, "r" => Code::KeyR,
        "s" => Code::KeyS, "t" => Code::KeyT, "u" => Code::KeyU,
        "v" => Code::KeyV, "w" => Code::KeyW, "x" => Code::KeyX,
        "y" => Code::KeyY, "z" => Code::KeyZ,
        "space" => Code::Space,
        "f1" => Code::F1, "f2" => Code::F2, "f3" => Code::F3,
        "f4" => Code::F4, "f5" => Code::F5, "f6" => Code::F6,
        "f7" => Code::F7, "f8" => Code::F8, "f9" => Code::F9,
        "f10" => Code::F10, "f11" => Code::F11, "f12" => Code::F12,
        _ => Code::KeyV,
    }
}
