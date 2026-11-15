//! The Windows host: a frameless, topmost panel at the right edge of the
//! monitor under the mouse, shown and hidden by the global shortcut and the
//! tray icon, hidden when it loses the keyboard if the user wants (but not
//! to a window of ours, nor to a system prompt of ours as Windows Hello's).
//! No taskbar button.
//!
//! Windows has no panel that takes the keyboard without becoming the
//! foreground window, so the host remembers the window that had the
//! keyboard and gives it back when the panel hides.
//!
//! The window, the tray icon and the shortcut live on one thread, in
//! `HOST`; the UI (on the engine thread) and the event handlers reach it
//! through `with_host`, which queues a call and wakes that thread.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use nsh_core::desktop::{MainWindowOptions, WindowAttributes, WindowFrameStyle, WindowLevel};
use nsh_core::element::Root;
use nsh_platform_windows::{App, AppOptions, AppView, Limits};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Power::{
    HPOWERNOTIFY, RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, DEVICE_NOTIFY_WINDOW_HANDLE, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    HWND_BROADCAST, HWND_TOPMOST, IsWindow, IsWindowVisible, MSG, PBT_APMSUSPEND, PostMessageW,
    RegisterWindowMessageW, SC_KEYMENU, SW_HIDE, SWP_NOACTIVATE, SetForegroundWindow,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE,
    WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_ENDSESSION,
    WM_NCDESTROY, WM_POWERBROADCAST, WM_SYSCOMMAND, WM_WTSSESSION_CHANGE, WTS_CONSOLE_DISCONNECT,
    WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF,
};
use windows::core::{PCWSTR, w};

use clipboardx::services::{Blur, WindowHost, services};

/// Set to anything to show the panel at launch (development).
const SHOW_ON_START_ENV: &str = "CLIPBOARDX_SHOW";

/// The gap between the panel and the monitor's right edge, in points.
const EDGE_MARGIN: i32 = 16;
/// Pixels per inch at 100% scaling: a point is a pixel there.
const BASE_DPI: u32 = 96;
/// The UI thread's stack, as NSH's own runner gives it.
const UI_STACK_BYTES: usize = 8 << 20;
/// The tray icon's size in pixels: sharp up to 200% scaling.
const TRAY_ICON_SIZE: u32 = 32;
/// A tray click this soon after the panel hid by losing the keyboard (to
/// that very click) or after another click (a double click) changes
/// nothing: at least this long, and the system's double-click time.
const TRAY_SETTLE: Duration = Duration::from_millis(400);
/// How long the UI waits for the host's thread to register a shortcut.
const HOTKEY_ANSWER: Duration = Duration::from_secs(2);

/// Wakes the host's thread to run the queued calls. NSH keeps the first
/// messages after `WM_APP` for itself on this window.
const WM_HOST_WAKE: u32 = WM_APP + 0x400;
/// The host's subclass of the panel.
const SUBCLASS_ID: usize = 0xC11B;

/// Held by the running instance.
const INSTANCE_MUTEX: PCWSTR = w!("Local\\com.clipboardx.app.instance");
/// What a second launch broadcasts: the running instance shows its panel.
const SHOW_MESSAGE_NAME: PCWSTR = w!("com.clipboardx.app.show");
/// What a second launch also sets, for a running instance that is still
/// starting and has no window to hear the broadcast yet.
const SHOW_REQUEST_EVENT: PCWSTR = w!("Local\\com.clipboardx.app.show-requested");

const APP_ICON: &[u8] = include_bytes!("../assets/icon.png");

/// The panel's window, for the threads that post to it; 0 before it is made.
static PANEL: AtomicIsize = AtomicIsize::new(0);
/// The id of [`SHOW_MESSAGE_NAME`] once registered.
static SHOW_MESSAGE: AtomicU32 = AtomicU32::new(0);
/// This instance's handle of [`SHOW_REQUEST_EVENT`]; 0 for none.
static SHOW_REQUEST: AtomicIsize = AtomicIsize::new(0);
/// The registration for the system's suspend notices; 0 for none.
static SUSPEND_NOTICES: AtomicIsize = AtomicIsize::new(0);

type Call = Box<dyn FnOnce(&Host) + Send>;

/// What the other threads asked of the host, until its thread wakes.
static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

/// What the host's thread keeps while the app runs.
struct Host {
    hwnd: HWND,
    view: Rc<AppView>,
    size: (i32, i32),
    hotkeys: Option<GlobalHotKeyManager>,
    hotkey: Cell<Option<HotKey>>,
    tray: RefCell<Option<TrayIcon>>,
    show_item: MenuItem,
    quit_item: MenuItem,
    /// The window that had the keyboard before the panel took it.
    previous: Cell<HWND>,
    /// When the panel last hid by losing the keyboard, or the tray icon
    /// was last clicked.
    settled: Cell<Option<Instant>>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Runs `f` with the host on its thread, now.
///
/// Only shared borrows: Windows calls back into the host synchronously
/// (`ShowWindow` sends `WM_ACTIVATE`), so the host keeps what changes in
/// cells and no borrow is ever exclusive while it acts.
fn host_now(f: impl FnOnce(&Host)) {
    HOST.with(|host| match host.try_borrow() {
        Ok(host) => {
            if let Some(host) = host.as_ref() {
                f(host);
            }
        }
        Err(_) => log::warn!("the host is busy; dropped a window action"),
    });
}

/// Runs `f` with the host, queued for its thread.
fn with_host(f: impl FnOnce(&Host) + Send + 'static) {
    let panel = PANEL.load(Ordering::Acquire);
    if panel == 0 {
        return;
    }
    match CALLS.lock() {
        Ok(mut calls) => calls.push(Box::new(f)),
        Err(_) => return,
    }
    // SAFETY: posting takes no ownership; a window that is gone fails it.
    let posted = unsafe {
        PostMessageW(
            Some(HWND(panel as *mut c_void)),
            WM_HOST_WAKE,
            WPARAM(0),
            LPARAM(0),
        )
    };
    if let Err(e) = posted {
        log::warn!("cannot wake the host: {e}");
    }
}

/// Runs what [`with_host`] queued.
fn run_calls() {
    let calls = match CALLS.lock() {
        Ok(mut calls) => std::mem::take(&mut *calls),
        Err(_) => return,
    };
    for call in calls {
        host_now(|host| call(host));
    }
}

/// What the UI calls: queues for the host's thread.
struct WindowsHost;

impl WindowHost for WindowsHost {
    fn hide(&self) {
        with_host(Host::hide);
    }

    /// Waits for the system's answer: it refuses a combination another
    /// program has, and a refused shortcut must be neither saved nor
    /// reported as set. Only the host's thread can ask (the shortcut
    /// belongs to a window of that thread).
    fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        let parsed = clipboardx::hotkey::parse(hotkey)?;
        let (answer, answered) = mpsc::channel();
        with_host(move |host| {
            let _ = answer.send(host.register_hotkey(parsed));
        });
        // A call that was dropped (no panel, a host that is quitting)
        // hangs up instead of answering.
        answered
            .recv_timeout(HOTKEY_ANSWER)
            .unwrap_or_else(|_| Err("无法注册快捷键，请重试".to_owned()))
    }

    fn set_hidden_from_capture(&self, hidden: bool) {
        with_host(move |host| host.set_hidden_from_capture(hidden));
    }

    fn take_keyboard(&self) {
        with_host(Host::take_keyboard);
    }
}

/// Where the panel goes, in pixels: `(x, y, width, height)` at the right
/// edge of the work area `work` of a monitor of `dpi`, centered vertically.
fn placement(work: RECT, dpi: u32, size: (i32, i32)) -> (i32, i32, i32, i32) {
    let pixels = |points: i32| {
        let scaled = i64::from(points) * i64::from(dpi) / i64::from(BASE_DPI);
        i32::try_from(scaled).unwrap_or(points)
    };
    let (width, height) = (pixels(size.0), pixels(size.1));
    let x = work.right - width - pixels(EDGE_MARGIN);
    let y = work.top + ((work.bottom - work.top - height) / 2).max(0);
    (x, y, width, height)
}

/// How long after a click or a hide by blur a tray click changes nothing.
fn tray_settle() -> Duration {
    // SAFETY: no arguments.
    let double_click = unsafe { GetDoubleClickTime() };
    Duration::from_millis(u64::from(double_click)).max(TRAY_SETTLE)
}

/// Whether the launch that held [`SHOW_REQUEST_EVENT`] first was asked to
/// show its panel while it had no window yet.
fn show_was_requested() -> bool {
    let request = SHOW_REQUEST.load(Ordering::Acquire);
    // SAFETY: the event this process created and keeps; not waited for.
    request != 0
        && unsafe { WaitForSingleObject(HANDLE(request as *mut c_void), 0) } == WAIT_OBJECT_0
}

/// Whether `window` belongs to this process: a dialog of ours, the tray's
/// menu.
fn is_ours(window: HWND) -> bool {
    if window.0.is_null() {
        return false;
    }
    let mut process = 0_u32;
    // SAFETY: any window handle may be asked; the out-pointer is a local.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
    process == std::process::id()
}

impl Host {
    fn is_shown(&self) -> bool {
        // SAFETY: our own window.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    fn has_keyboard(&self) -> bool {
        // SAFETY: no arguments.
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    /// Hides the panel; the window it took the keyboard from gets it back
    /// if the panel still had it.
    fn hide(&self) {
        if !self.is_shown() {
            return;
        }
        let had_keyboard = self.has_keyboard();
        // SAFETY: our own window, on its thread.
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        services().window_hidden();
        let previous = self.previous.replace(HWND::default());
        // SAFETY: `IsWindow` takes any value; a window that went meanwhile
        // only makes `SetForegroundWindow` fail.
        if had_keyboard && unsafe { IsWindow(Some(previous)) }.as_bool() {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
    }

    /// Hides the panel because it lost the keyboard.
    fn hide_blurred(&self) {
        if self.is_shown() && !self.has_keyboard() {
            self.settled.set(Some(Instant::now()));
            self.hide();
        }
    }

    /// Remembers the window that has the keyboard, before the panel takes
    /// it: the one to give it back to.
    fn remember_foreground(&self) {
        // SAFETY: no arguments.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground != self.hwnd && !is_ours(foreground) {
            self.previous.set(foreground);
        }
    }

    fn show(&self) {
        self.remember_foreground();
        self.round_corners();
        self.move_to_mouse_monitor();
        // Paints before it shows, and takes the taskbar button away again.
        self.view.show();
        self.take_keyboard();
        services().window_shown();
    }

    /// Makes the panel the foreground window if it is shown: it was just
    /// shown, or a system prompt (Windows Hello) had the keyboard.
    fn take_keyboard(&self) {
        if self.is_shown() {
            // SAFETY: our own window; a refusal (the foreground lock) only
            // leaves the panel without the keyboard.
            let _ = unsafe { SetForegroundWindow(self.hwnd) };
        }
    }

    fn toggle(&self) {
        if !self.is_shown() {
            self.show();
        } else if self.has_keyboard() {
            self.hide();
        } else {
            // Shown without the keyboard (it stays up when it loses it):
            // the user is in another window now.
            self.remember_foreground();
            self.take_keyboard();
        }
    }

    /// The tray icon was clicked: shows the panel or hides it, unless the
    /// click itself just hid it (by taking the keyboard away) or is a
    /// double click's second. The click took the keyboard to the taskbar,
    /// so a panel that is shown hides whether it had the keyboard or not.
    fn tray_clicked(&self) {
        let now = Instant::now();
        let settling = self
            .settled
            .get()
            .is_some_and(|at| now.duration_since(at) < tray_settle());
        self.settled.set(Some(now));
        if settling {
            return;
        }
        if self.is_shown() {
            self.hide();
        } else {
            self.show();
            // The click made the taskbar the foreground window: not the
            // one to give the keyboard back to. The system picks.
            self.previous.set(HWND::default());
        }
    }

    /// Keeps the panel out of what other apps read of the screen
    /// (screenshots, recordings, screen sharing), or lets them read it.
    fn set_hidden_from_capture(&self, hidden: bool) {
        let affinity = if hidden {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        // SAFETY: our own window, on its thread.
        let set = unsafe { SetWindowDisplayAffinity(self.hwnd, affinity) }.or_else(|e| {
            if !hidden {
                return Err(e);
            }
            // Before Windows 10 2004: black in captures instead of absent.
            // SAFETY: as above.
            unsafe { SetWindowDisplayAffinity(self.hwnd, WDA_MONITOR) }
        });
        if let Err(e) = set {
            log::error!("cannot change the panel's screen capture protection: {e}");
        }
    }

    /// Asks Windows 11 for rounded corners; Windows 10 has none.
    fn round_corners(&self) {
        let preference = DWMWCP_ROUND;
        // SAFETY: the attribute's value is a local of the size given.
        let _ = unsafe {
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const preference).cast(),
                size_of_val(&preference) as u32,
            )
        };
    }

    /// Places the panel at the right edge of the monitor the mouse is on,
    /// centered vertically, at that monitor's scale.
    fn move_to_mouse_monitor(&self) {
        let mut mouse = POINT::default();
        // SAFETY: the out-pointer is a local.
        if unsafe { GetCursorPos(&mut mouse) }.is_err() {
            return;
        }
        // SAFETY: a point; the nearest monitor always exists.
        let monitor = unsafe { MonitorFromPoint(mouse, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..MONITORINFO::default()
        };
        // SAFETY: `info` is a local with its size set.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return;
        }
        let (mut dpi, mut dpi_y) = (BASE_DPI, BASE_DPI);
        // SAFETY: the out-pointers are locals.
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) }.is_err() {
            dpi = BASE_DPI;
        }
        let (x, y, width, height) = placement(info.rcWork, dpi, self.size);
        // Twice: arriving on a monitor of another scale, the system has the
        // window resized by the ratio of the scales, from the size it was
        // just given. After the first call the window is wholly on the
        // monitor and at its scale; the second only sets the size right.
        for _ in 0..2 {
            // SAFETY: our own window, on its thread.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE,
                )
            };
        }
    }

    /// Registers `hotkey` in place of the current one; keeps the current
    /// one when the system refuses it, and says why.
    fn register_hotkey(&self, hotkey: HotKey) -> Result<(), String> {
        let Some(manager) = self.hotkeys.as_ref() else {
            return Err("全局快捷键不可用".to_owned());
        };
        let old = self.hotkey.take();
        if let Some(old) = old
            && let Err(e) = manager.unregister(old)
        {
            log::warn!("cannot unregister the old shortcut: {e}");
        }
        match manager.register(hotkey) {
            Ok(()) => {
                log::info!("shortcut registered: {hotkey}");
                self.hotkey.set(Some(hotkey));
                Ok(())
            }
            Err(e) => {
                log::error!("cannot register the shortcut {hotkey}: {e}");
                if let Some(old) = old
                    && manager.register(old).is_ok()
                {
                    self.hotkey.set(Some(old));
                }
                Err(match e {
                    global_hotkey::Error::AlreadyRegistered(_) => {
                        "该快捷键已被其他程序或系统占用".to_owned()
                    }
                    other => format!("无法注册快捷键：{other}"),
                })
            }
        }
    }

    /// Ends the app: the tray icon goes, then the window, which stops the
    /// engine and ends the message loop.
    fn quit(&self) {
        self.tray.replace(None);
        // SAFETY: our own window, on its thread.
        if let Err(e) = unsafe { DestroyWindow(self.hwnd) } {
            log::error!("cannot close the panel: {e}");
        }
    }
}

/// What the panel's window hears before NSH does.
unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_HOST_WAKE => {
            run_calls();
            return LRESULT(0);
        }
        // Alt+F4 hides: closing the window would end the app.
        WM_CLOSE => {
            host_now(Host::hide);
            return LRESULT(0);
        }
        // Alt alone, or Alt+Space, would start the window's menu: the
        // panel shows none, and the menu would take the keys typed next
        // (the default shortcut has Alt in it).
        WM_SYSCOMMAND => {
            if (wparam.0 & 0xFFF0) as u32 == SC_KEYMENU {
                return LRESULT(0);
            }
        }
        WM_ACTIVATE => {
            let state = (wparam.0 & 0xFFFF) as u32;
            let other = HWND(lparam.0 as *mut c_void);
            if state == WA_INACTIVE && !is_ours(other) {
                // Queued: the system is still moving the keyboard.
                match services().window_blurred() {
                    Blur::Hide => with_host(Host::hide_blurred),
                    Blur::TakeBack => with_host(Host::take_keyboard),
                    Blur::Stay => {}
                }
            }
        }
        WM_WTSSESSION_CHANGE => {
            let event = wparam.0 as u32;
            if [
                WTS_SESSION_LOCK,
                WTS_SESSION_LOGOFF,
                WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_DISCONNECT,
            ]
            .contains(&event)
            {
                services().lock_vault();
            }
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMSUSPEND {
                services().lock_vault();
            }
        }
        // Signing out, shutting down, or an installer replacing the app.
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                host_now(Host::quit);
            }
        }
        WM_NCDESTROY => {
            PANEL.store(0, Ordering::Release);
            let suspend_notices = SUSPEND_NOTICES.swap(0, Ordering::AcqRel);
            // SAFETY: the window, the registrations made for it and the
            // subclass this function was set as.
            unsafe {
                if suspend_notices != 0 {
                    let _ = UnregisterSuspendResumeNotification(HPOWERNOTIFY(suspend_notices));
                }
                let _ = WTSUnRegisterSessionNotification(hwnd);
                let _ = RemoveWindowSubclass(hwnd, Some(panel_proc), SUBCLASS_ID);
            }
        }
        other => {
            let show = SHOW_MESSAGE.load(Ordering::Relaxed);
            if show != 0 && other == show {
                host_now(Host::show);
                return LRESULT(0);
            }
        }
    }
    // SAFETY: the arguments Windows gave this subclass procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// The app's root as the panel: no frame, above other windows, not
/// resizable, no taskbar button. NSH applies it with the first frame.
#[allow(
    clippy::field_reassign_with_default,
    reason = "the options are non-exhaustive: they have no struct literal"
)]
fn panel_root() -> Root {
    let mut attributes = WindowAttributes::default();
    attributes.frame = Some(WindowFrameStyle::None);
    attributes.resizable = Some(false);
    attributes.minimizable = Some(false);
    attributes.maximizable = Some(false);
    attributes.level = Some(WindowLevel::Floating);
    attributes.taskbar = Some(false);
    let mut options = MainWindowOptions::default();
    options.attributes = attributes;
    clipboardx::root().main_window(options)
}

/// The app's icon, for the tray: in color, as the taskbar may be dark or
/// light.
fn tray_image() -> Option<tray_icon::Icon> {
    let icon = image::load_from_memory(APP_ICON)
        .map_err(|e| log::error!("cannot read the app icon: {e}"))
        .ok()?
        .to_rgba8();
    let small = image::imageops::resize(
        &icon,
        TRAY_ICON_SIZE,
        TRAY_ICON_SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    tray_icon::Icon::from_rgba(small.into_raw(), TRAY_ICON_SIZE, TRAY_ICON_SIZE)
        .map_err(|e| log::error!("cannot make the tray icon: {e}"))
        .ok()
}

fn make_tray(show: &MenuItem, quit: &MenuItem) -> Option<TrayIcon> {
    let menu = Menu::new();
    let appended = menu
        .append(show)
        .and_then(|()| menu.append(&PredefinedMenuItem::separator()))
        .and_then(|()| menu.append(quit));
    if let Err(e) = appended {
        log::error!("cannot build the tray menu: {e}");
    }
    let mut builder = TrayIconBuilder::new()
        .with_tooltip("ClipboardX")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false);
    if let Some(icon) = tray_image() {
        builder = builder.with_icon(icon);
    }
    builder
        .build()
        .map_err(|e| log::error!("cannot create the tray icon: {e}"))
        .ok()
}

/// Set before the tray icon and the shortcut exist: the first handler set
/// is the one that stays, and an event without one leaves none for good.
/// The handlers only queue, as some events come on threads of their own.
fn listen_to_events() {
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            with_host(Host::tray_clicked);
        }
    }));
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        with_host(move |host| {
            if event.id == *host.show_item.id() {
                host.show();
            } else if event.id == *host.quit_item.id() {
                host.quit();
            }
        });
    }));
    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed {
            with_host(move |host| {
                if host.hotkey.get().is_some_and(|h| h.id() == event.id) {
                    host.toggle();
                }
            });
        }
    }));
}

/// Whether the app already runs: if so its panel is asked to show, and
/// this launch has nothing left to do.
pub fn hand_over_to_running_instance() -> bool {
    // SAFETY: a named mutex; its handle is kept until the process ends, so
    // the name stays taken.
    let created = unsafe { CreateMutexW(None, false, INSTANCE_MUTEX) };
    // SAFETY: the error of the call just made, on this thread: it says
    // whether the mutex was there already.
    let last_error = unsafe { GetLastError() };
    let already = created.is_ok() && last_error == ERROR_ALREADY_EXISTS;
    // SAFETY: a named event (reset by hand, not set), the same one for
    // every launch; the first launch keeps its handle, so it lasts.
    let request = unsafe { CreateEventW(None, true, false, SHOW_REQUEST_EVENT) };
    if already {
        // SAFETY: the event just opened, and a registered message with no
        // data, to every top-level window; only the running instance
        // knows it.
        unsafe {
            // For a running instance without a window yet: it looks at the
            // event once its panel is ready.
            if let Ok(request) = request {
                let _ = SetEvent(request);
            }
            let show = RegisterWindowMessageW(SHOW_MESSAGE_NAME);
            // The running instance may come forward on this launch's behalf.
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = PostMessageW(Some(HWND_BROADCAST), show, WPARAM(0), LPARAM(0));
        }
    } else if let Ok(request) = request {
        SHOW_REQUEST.store(request.0 as isize, Ordering::Release);
    }
    already
}

/// Runs the app until it quits.
pub fn run(size: (i32, i32)) -> ExitCode {
    // A thread of its own, as NSH's runner has: its graphics devices end
    // with the thread, before the process does.
    let ui = std::thread::Builder::new()
        .name("ui".into())
        .stack_size(UI_STACK_BYTES)
        .spawn(move || run_ui(size));
    match ui.map(std::thread::JoinHandle::join) {
        Ok(Ok(code)) => code,
        Ok(Err(_)) => {
            log::error!("the UI thread panicked");
            ExitCode::FAILURE
        }
        Err(e) => {
            log::error!("cannot start the UI thread: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_ui(size: (i32, i32)) -> ExitCode {
    // SAFETY: once, before any window: per-monitor scaling (refused when
    // the manifest already says so), and OLE for NSH's clipboard and drops.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if let Err(e) = OleInitialize(None) {
            log::error!("cannot initialize OLE: {e}");
        }
    }
    nsh_platform_windows::set_ui_sets(vec![nsh_base::registry(), clipboardx::registry()]);
    let view = match AppView::new(
        None,
        AppOptions {
            app_id: clipboardx::services::APP_ID.to_owned(),
            title: "ClipboardX".to_owned(),
            size,
            skip_taskbar: true,
            limits: Limits::standalone(),
            ..AppOptions::default()
        },
    ) {
        Ok(view) => view,
        Err(e) => {
            log::error!("cannot create the panel: {e}");
            return ExitCode::FAILURE;
        }
    };
    let hwnd = view.hwnd();
    // SAFETY: our own window, on its thread; the procedure is a static
    // function. A second launch's message and session changes come to it.
    unsafe {
        SHOW_MESSAGE.store(RegisterWindowMessageW(SHOW_MESSAGE_NAME), Ordering::Relaxed);
        if !SetWindowSubclass(hwnd, Some(panel_proc), SUBCLASS_ID, 0).as_bool() {
            log::error!("cannot watch the panel's window");
        }
        if let Err(e) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            log::error!("cannot hear about the session locking: {e}");
        }
        // Asked for: a machine that sleeps by Modern Standby tells only
        // the windows that did.
        match RegisterSuspendResumeNotification(HANDLE(hwnd.0), DEVICE_NOTIFY_WINDOW_HANDLE) {
            Ok(notices) => SUSPEND_NOTICES.store(notices.0, Ordering::Release),
            Err(e) => log::error!("cannot hear about the system suspending: {e}"),
        }
    }
    PANEL.store(hwnd.0 as isize, Ordering::Release);
    clipboardx::vault::windows_store::set_prompt_owner(hwnd.0 as isize);

    listen_to_events();
    let hotkeys = GlobalHotKeyManager::new()
        .map_err(|e| log::error!("global shortcuts are unavailable: {e}"))
        .ok();
    let show_item = MenuItem::new("显示 ClipboardX", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    let tray = make_tray(&show_item, &quit_item);
    HOST.with(|host| {
        *host.borrow_mut() = Some(Host {
            hwnd,
            view: view.clone(),
            size,
            hotkeys,
            hotkey: Cell::new(None),
            tray: RefCell::new(tray),
            show_item,
            quit_item,
            previous: Cell::new(HWND::default()),
            settled: Cell::new(None),
        });
    });
    services().set_host(Box::new(WindowsHost));
    // Queued before the panel can show.
    if let Err(e) = services().apply_capture_protection() {
        log::error!("cannot hide the panel from screen captures: {e}");
    }
    view.load(App::Rust(panel_root()));

    let hotkey = services()
        .db()
        .settings()
        .map(|s| s.hotkey)
        .unwrap_or_else(|_| clipboardx::model::DEFAULT_HOTKEY.to_owned());
    host_now(|host| match clipboardx::hotkey::parse(&hotkey) {
        // Refused (another program took it since): logged; the tray icon
        // still shows the panel, and the settings take another shortcut.
        Ok(parsed) => drop(host.register_hotkey(parsed)),
        Err(e) => log::error!("saved shortcut {hotkey:?} is invalid: {e}"),
    });
    // A launch that came while this one was starting asked for the panel.
    if std::env::var_os(SHOW_ON_START_ENV).is_some() || show_was_requested() {
        with_host(Host::show);
    }

    let mut message = MSG::default();
    // SAFETY: the thread's own message loop: every window of the thread
    // (NSH's, the tray's, the shortcut's) and its timers.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    // The tray icon and the shortcut go on the thread that made them.
    HOST.with(|host| drop(host.borrow_mut().take()));
    view.stop();
    drop(view);
    // SAFETY: pairs with `OleInitialize` above, after the last window.
    unsafe { OleUninitialize() };
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (i32, i32) = (420, 600);

    fn work(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn the_panel_sits_at_the_right_edge_centered_vertically() {
        let (x, y, width, height) = placement(work(0, 0, 1920, 1040), 96, SIZE);

        assert_eq!((width, height), (420, 600));
        assert_eq!(x, 1920 - 420 - 16);
        assert_eq!(y, (1040 - 600) / 2);
    }

    #[test]
    fn the_panel_scales_with_the_monitor() {
        let (x, y, width, height) = placement(work(0, 0, 3840, 2100), 192, SIZE);

        assert_eq!((width, height), (840, 1200));
        assert_eq!(x, 3840 - 840 - 32);
        assert_eq!(y, (2100 - 1200) / 2);
    }

    #[test]
    fn the_panel_follows_a_monitor_to_the_left_of_the_main_one() {
        let (x, y, _, _) = placement(work(-1280, 200, 0, 1000), 96, SIZE);

        assert_eq!(x, -420 - 16);
        assert_eq!(y, 200 + (800 - 600) / 2);
    }

    #[test]
    fn a_panel_taller_than_the_work_area_starts_at_its_top() {
        let (_, y, _, _) = placement(work(0, 30, 800, 500), 96, SIZE);

        assert_eq!(y, 30);
    }

    #[test]
    fn a_window_of_no_one_is_not_ours() {
        assert!(!is_ours(HWND::default()));
    }
}
