//! The macOS host: a floating, non-activating panel at the right edge of
//! the screen under the mouse (it takes the keyboard without taking the
//! focus from the app the user copies from), shown and hidden by the
//! global shortcut and the menu-bar icon, hidden when it loses the
//! keyboard if the user wants. No Dock icon.
//!
//! Everything AppKit lives on the main thread, in `HOST`; the UI (on the
//! engine thread) reaches it through `MacHost`, which posts to the main
//! queue.

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;

use dispatch2::DispatchQueue;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use nsh_platform_macos::{App, AppOptions, AppView};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent,
    NSFloatingWindowLevel, NSMenu, NSMenuItem, NSPanel, NSScreen, NSView, NSWindowButton,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use clipboardx::services::{WindowHost, services};

/// Set to anything to show the panel at launch (development).
const SHOW_ON_START_ENV: &str = "CLIPBOARDX_SHOW";

/// The gap between the panel and the screen's right edge.
const EDGE_MARGIN: f64 = 16.0;

/// What the main thread keeps while the app runs.
struct Host {
    panel: Retained<NSPanel>,
    view: Rc<AppView>,
    _delegate: Retained<PanelDelegate>,
    hotkeys: Option<GlobalHotKeyManager>,
    hotkey: Cell<Option<HotKey>>,
    tray: RefCell<Option<TrayIcon>>,
    show_item: MenuItem,
    quit_item: MenuItem,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Runs `f` with the host on the main thread, now.
///
/// Only shared borrows: AppKit calls back into the host synchronously
/// (`orderOut:` sends `windowDidResignKey:`), so the host keeps what
/// changes in cells and no borrow is ever exclusive while it acts.
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

/// Runs `f` with the host, posted to the main thread.
fn with_host(f: impl FnOnce(&Host) + Send + 'static) {
    DispatchQueue::main().exec_async(move || host_now(f));
}

/// What the UI calls: posts to the main thread.
struct MacHost;

impl WindowHost for MacHost {
    fn hide(&self) {
        with_host(|host| host.hide());
    }

    fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        let parsed = clipboardx::hotkey::parse(hotkey)?;
        with_host(move |host| host.register_hotkey(parsed));
        Ok(())
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ClipboardXPanelDelegate"]
    struct PanelDelegate;

    unsafe impl NSObjectProtocol for PanelDelegate {}

    unsafe impl NSWindowDelegate for PanelDelegate {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            let hide = services()
                .db()
                .settings()
                .map(|s| s.hide_on_blur)
                .unwrap_or(true);
            if hide {
                host_now(Host::hide);
            }
        }
    }
);

impl PanelDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's designated initializer, on a class that adds
        // no state of its own.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

impl Host {
    fn is_shown(&self) -> bool {
        self.panel.isVisible()
    }

    fn hide(&self) {
        self.panel.orderOut(None);
    }

    fn show(&self) {
        self.move_to_mouse_screen();
        self.panel.makeKeyAndOrderFront(None);
        // The NSH root view takes the keys (the AppView's first subview).
        if let Some(root) = self.view.view().subviews().firstObject() {
            self.panel.makeFirstResponder(Some(&root));
        }
        services().window_shown();
    }

    fn toggle(&self) {
        if self.is_shown() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// Places the panel at the right edge of the screen the mouse is on,
    /// centered vertically.
    fn move_to_mouse_screen(&self) {
        let mtm = MainThreadMarker::from(&*self.panel);
        let mouse = NSEvent::mouseLocation();
        let screens = NSScreen::screens(mtm);
        let screen = screens
            .iter()
            .find(|s| contains(s.frame(), mouse))
            .or_else(|| NSScreen::mainScreen(mtm));
        let Some(screen) = screen else {
            return;
        };
        let area = screen.visibleFrame();
        let size = self.panel.frame().size;
        let x = area.origin.x + area.size.width - size.width - EDGE_MARGIN;
        let y = area.origin.y + ((area.size.height - size.height) / 2.0).max(0.0);
        self.panel.setFrameOrigin(NSPoint::new(x, y));
    }

    /// Registers `hotkey` in place of the current one; keeps the current
    /// one when the system refuses it.
    fn register_hotkey(&self, hotkey: HotKey) {
        let Some(manager) = self.hotkeys.as_ref() else {
            return;
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
            }
            Err(e) => {
                log::error!("cannot register the shortcut {hotkey}: {e}");
                if let Some(old) = old
                    && manager.register(old).is_ok()
                {
                    self.hotkey.set(Some(old));
                }
            }
        }
    }
}

fn contains(frame: NSRect, point: NSPoint) -> bool {
    point.x >= frame.origin.x
        && point.x < frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y < frame.origin.y + frame.size.height
}

fn make_panel(mtm: MainThreadMarker, size: (i32, i32)) -> Retained<NSPanel> {
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(size.0), f64::from(size.1)),
    );
    // Titled for rounded corners and the key window, its bar transparent
    // and its buttons hidden: the app draws its own header.
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::FullSizeContentView
        | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the host keeps the panel until the app quits.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&NSString::from_str("ClipboardX"));
    panel.setTitlebarAppearsTransparent(true);
    panel.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = panel.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setHidesOnDeactivate(false);
    panel.setBecomesKeyOnlyIfNeeded(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel
}

/// The Edit menu, so text fields answer ⌘C, ⌘V, ⌘X, ⌘A and ⌘Z.
fn edit_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&edit));
    bar.addItem(&holder);
    for (title, action, key) in [
        ("Undo", sel!(undo:), "z"),
        ("Redo", sel!(redo:), "Z"),
        ("Cut", sel!(cut:), "x"),
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
    ] {
        // SAFETY: standard AppKit actions, sent down the responder chain.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        edit.addItem(&item);
    }
    bar
}

/// A clipboard drawn as a menu-bar template image (black on clear).
fn tray_image() -> tray_icon::Icon {
    const SIZE: u32 = 36;
    let mut rgba = vec![0_u8; (SIZE * SIZE * 4) as usize];
    let mut set = |x: u32, y: u32| {
        let at = ((y * SIZE + x) * 4) as usize;
        rgba[at..at + 4].copy_from_slice(&[0, 0, 0, 255]);
    };
    let (left, right, top, bottom, stroke) = (7, 28, 6, 32, 3);
    for y in top..=bottom {
        for x in left..=right {
            let edge =
                x < left + stroke || x > right - stroke || y < top + stroke || y > bottom - stroke;
            let corner = (x == left || x == right) && (y == top || y == bottom);
            if edge && !corner {
                set(x, y);
            }
        }
    }
    // The clip, and two lines of text.
    for y in 3..10 {
        for x in 12..24 {
            set(x, y);
        }
    }
    for (y0, x1) in [(16, 23), (23, 20)] {
        for y in y0..y0 + 3 {
            for x in 13..x1 {
                set(x, y);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).unwrap_or_else(|e| panic!("tray icon: {e}"))
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
    TrayIconBuilder::new()
        .with_icon_templated(tray_image())
        .with_tooltip("ClipboardX")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| log::error!("cannot create the menu-bar icon: {e}"))
        .ok()
}

fn listen_to_events() {
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            with_host(|host| host.toggle());
        }
    }));
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        with_host(move |host| {
            if event.id == *host.show_item.id() {
                host.show();
            } else if event.id == *host.quit_item.id() {
                // `terminate:` exits the process: stop the engine first.
                host.view.stop();
                let mtm = MainThreadMarker::from(&*host.panel);
                NSApplication::sharedApplication(mtm).terminate(None);
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

/// Runs the app until it quits.
pub fn run(size: (i32, i32)) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("ClipboardX must start on the main thread");
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.setMainMenu(Some(&edit_menu(mtm)));

    nsh_platform_macos::set_ui_sets(vec![nsh_base::registry(), clipboardx::registry()]);
    let view = AppView::new(
        mtm,
        AppOptions {
            app_id: clipboardx::services::APP_ID.to_owned(),
            title: "ClipboardX".to_owned(),
            size,
            limits: nsh_platform_macos::Limits::standalone(),
            ..AppOptions::default()
        },
    );
    let panel = make_panel(mtm, size);
    let delegate = PanelDelegate::new(mtm);
    panel.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let content: Retained<NSView> = view.view();
    panel.setContentView(Some(&content));
    view.load(App::Rust(clipboardx::root()));

    let hotkeys = GlobalHotKeyManager::new()
        .map_err(|e| log::error!("global shortcuts are unavailable: {e}"))
        .ok();
    let show_item = MenuItem::new("显示 ClipboardX", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    HOST.with(|host| {
        *host.borrow_mut() = Some(Host {
            panel,
            view,
            _delegate: delegate,
            hotkeys,
            hotkey: Cell::new(None),
            tray: RefCell::new(None),
            show_item,
            quit_item,
        });
    });
    services().set_host(Box::new(MacHost));
    listen_to_events();

    // The menu-bar icon and the shortcut once the event loop runs.
    let hotkey = services()
        .db()
        .settings()
        .map(|s| s.hotkey)
        .unwrap_or_else(|_| clipboardx::model::DEFAULT_HOTKEY.to_owned());
    DispatchQueue::main().exec_async(move || {
        host_now(|host| {
            host.tray
                .replace(make_tray(&host.show_item, &host.quit_item));
            match clipboardx::hotkey::parse(&hotkey) {
                Ok(parsed) => host.register_hotkey(parsed),
                Err(e) => log::error!("saved shortcut {hotkey:?} is invalid: {e}"),
            }
            if std::env::var_os(SHOW_ON_START_ENV).is_some() {
                host.show();
            }
        });
    });

    app.run();
    HOST.with(|host| {
        if let Some(host) = host.borrow_mut().take() {
            host.view.stop();
        }
    });
    ExitCode::SUCCESS
}
