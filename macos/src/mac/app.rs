//! The app: its window (the mixer's page in a WKWebView) and its menu bar
//! icon, on the main thread, as AppKit wants. The mixer runs on a thread of
//! its own (`main.rs`) and drives this through a [`Ui`] handle: what the
//! window processes of the Linux and Windows versions get on their input
//! (show, hide, close to the tray, recording, quit). Here → mixer:
//! [`FromUi`].
//!
//! With its window shown BananaSplit is an ordinary app, with a Dock icon
//! and a menu bar. Closed (with "Closing the window keeps it running" on,
//! the default) it keeps running as a menu bar icon only, until Quit: in
//! its menu, Cmd+Q, the page's power button, or the Dock's Quit. Started
//! in the background (`--no-open`, at login) only the icon shows.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
use tray_icon::menu::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::{WebView, WebViewBuilder};

/// The window's default size (logical pixels), within the screen.
const DEFAULT_SIZE: (u32, u32) = (1200, 800);
/// Report the size once it stops changing (not on every step of a drag).
const SIZE_SETTLE: Duration = Duration::from_millis(500);
/// How long a quit from macOS (log out, the Dock's Quit) waits for the
/// mixer to save and stop.
const QUIT_WAIT: Duration = Duration::from_secs(5);
/// The page's background, shown until it has loaded.
const BACKGROUND: (u8, u8, u8, u8) = (11, 16, 21, 255);
/// The menu bar icon (the Linux tray pixmaps: ARGB32, big-endian).
const ICON_IDLE: &[u8] = include_bytes!("../../assets/tray/idle-64.argb");
const ICON_REC: &[u8] = include_bytes!("../../assets/tray/rec-64.argb");

const MENU_SHOW: &str = "bs-show";
const MENU_RECORD: &str = "bs-record";
const MENU_QUIT: &str = "bs-quit";

/// What the window should look like when it is made.
pub struct StartOpts {
    pub url: String,
    /// Started in the background: only the menu bar icon shows.
    pub hidden: bool,
    pub close_to_tray: bool,
    pub size: Option<(u32, u32)>,
}

/// Mixer (or a menu) → the app.
pub enum ToUi {
    /// Make the window (and show it, unless hidden).
    Start(StartOpts),
    /// Bring the window up (BananaSplit was started again).
    Show,
    /// Out of sight at once (the mixer is quitting).
    Hide,
    CloseToTray(bool),
    /// The recorder runs: the icon and its menu show it.
    Rec(bool),
    /// The mixer has finished: end the app with this code.
    Exit(i32),
    /// A menu item was chosen (its id).
    Menu(String),
}

/// The app → the mixer.
#[derive(Debug, PartialEq)]
pub enum FromUi {
    /// Quit BananaSplit (the window was closed with "Closing the window
    /// keeps it running" off, or Quit was chosen).
    Quit,
    /// Start or stop recording (the menu bar icon's menu).
    Record,
    /// The window's size the user left it at (logical pixels).
    Size(u32, u32),
    /// The menu bar icon could not be made (closing the window quits).
    NoTray,
    /// The window could not be made: why.
    NoWindow(String),
}

/// The mixer's end of the app.
pub struct Ui {
    proxy: EventLoopProxy<ToUi>,
    rx: mpsc::Receiver<FromUi>,
}

impl Ui {
    pub fn send(&self, m: ToUi) {
        let _ = self.proxy.send_event(m);
    }

    pub fn poll(&self) -> Vec<FromUi> {
        self.rx.try_iter().collect()
    }
}

/// The app's event loop (made on the main thread before anything else
/// touches AppKit), the mixer's handle, and the app's end of the channel
/// back. `hidden`: started in the background, so no Dock icon at first.
pub fn event_loop(hidden: bool) -> (EventLoop<ToUi>, Ui, mpsc::Sender<FromUi>) {
    let mut el = EventLoopBuilder::<ToUi>::with_user_event().build();
    el.set_activation_policy(if hidden { ActivationPolicy::Accessory } else { ActivationPolicy::Regular });
    // Started in the background (at login): the app in front keeps the
    // keyboard.
    el.set_activate_ignoring_other_apps(!hidden);
    let (tx, rx) = mpsc::channel();
    let proxy = el.create_proxy();
    (el, Ui { proxy, rx }, tx)
}

/// The size to open with: the remembered or default one, within the screen
/// (leaving room for the menu bar, the Dock and the title bar).
pub fn fit_size(want: Option<(u32, u32)>, screen: Option<(u32, u32)>) -> (u32, u32) {
    let (w, h) = want.unwrap_or(DEFAULT_SIZE);
    match screen {
        Some((sw, sh)) if sw > 0 && sh > 0 => {
            (w.min(sw.saturating_sub(48)).max(640), h.min(sh.saturating_sub(120)).max(480))
        }
        _ => (w, h),
    }
}

/// ARGB32 (big-endian) pixels as RGBA.
pub fn rgba(argb: &[u8]) -> Vec<u8> {
    argb.as_chunks::<4>().0.iter().flat_map(|p| [p[1], p[2], p[3], p[0]]).collect()
}

fn icon(argb: &[u8]) -> Option<Icon> {
    Icon::from_rgba(rgba(argb), 64, 64).ok()
}

/// The menu bar: the app menu, Edit (copy and paste in the page's fields
/// need it) and Window (returned too: it becomes the app's Window menu
/// once the menu bar is the app's).
fn app_menu() -> Result<(Menu, Submenu), String> {
    let e = |e: tray_icon::menu::Error| format!("menu: {e}");
    let about = AboutMetadata {
        name: Some("BananaSplit".into()),
        version: Some(crate::update::VERSION.into()),
        ..Default::default()
    };
    let quit = MenuItem::with_id(
        MENU_QUIT,
        "Quit BananaSplit",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyQ)),
    );
    let app = Submenu::with_items(
        "BananaSplit",
        true,
        &[
            &PredefinedMenuItem::about(None, Some(about)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &quit,
        ],
    )
    .map_err(e)?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .map_err(e)?;
    let window = Submenu::with_items(
        "Window",
        true,
        &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::close_window(None)],
    )
    .map_err(e)?;
    let menu = Menu::with_items(&[&app, &edit, &window]).map_err(e)?;
    Ok((menu, window))
}

/// The menu bar icon and the item that says what Record does.
fn tray() -> Result<(TrayIcon, MenuItem), String> {
    let e = |e: tray_icon::menu::Error| format!("menu: {e}");
    let record = MenuItem::with_id(MENU_RECORD, "Start recording", true, None);
    let menu = Menu::with_items(&[
        &MenuItem::with_id(MENU_SHOW, "Show BananaSplit", true, None),
        &record,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MENU_QUIT, "Quit BananaSplit", true, None),
    ])
    .map_err(e)?;
    let mut b = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("BananaSplit");
    if let Some(i) = icon(ICON_IDLE) {
        b = b.with_icon(i);
    }
    let t = b.build().map_err(|e| format!("menu bar icon: {e}"))?;
    Ok((t, record))
}

/// The window and its page.
struct Shown {
    window: Window,
    _page: WebView,
}

fn make_window(target: &EventLoopWindowTarget<ToUi>, o: &StartOpts) -> Result<Shown, String> {
    let screen = target.primary_monitor().map(|m| {
        let s = m.size().to_logical::<f64>(m.scale_factor());
        (s.width as u32, s.height as u32)
    });
    let (w, h) = fit_size(o.size, screen);
    let window = WindowBuilder::new()
        .with_title("BananaSplit")
        .with_inner_size(LogicalSize::new(f64::from(w), f64::from(h)))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .with_background_color(BACKGROUND)
        .with_visible(false)
        .build(target)
        .map_err(|e| format!("could not create the window: {e}"))?;
    let page = WebViewBuilder::new()
        .with_url(&o.url)
        .with_background_color(BACKGROUND)
        // a click into the window when another app is in front works at
        // once (a fader moves), as in other Mac apps
        .with_accept_first_mouse(true)
        .build(&window)
        .map_err(|e| format!("could not start the web view: {e}"))?;
    Ok(Shown { window, _page: page })
}

/// Tell the mixer the window's size, if it changed and is a normal one.
fn report_size(tx: &mpsc::Sender<FromUi>, window: &Window, told: &mut Option<(u32, u32)>) {
    if window.is_minimized() || window.is_maximized() || !window.is_visible() {
        return;
    }
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    let size = (s.width.round() as u32, s.height.round() as u32);
    if size.0 > 0 && size.1 > 0 && *told != Some(size) {
        *told = Some(size);
        let _ = tx.send(FromUi::Size(size.0, size.1));
    }
}

/// Run the app until the mixer says Exit, or macOS ends it (log out, the
/// Dock's Quit): then the mixer is asked to quit and given time to save.
/// `mixer_done` is set when the mixer has saved and stopped.
pub fn run(event_loop: EventLoop<ToUi>, tx: mpsc::Sender<FromUi>, mixer_done: Arc<AtomicBool>) -> ! {
    // Menus and the icon tell the event loop (it wakes up for them).
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = proxy.send_event(ToUi::Menu(e.id.0));
    }));
    // The icon's own clicks are not used (its menu opens on a click), so
    // they are dropped instead of piling up.
    TrayIconEvent::set_event_handler(Some(|_| {}));

    let mut shown: Option<Shown> = None;
    let mut icon_menu: Option<(TrayIcon, MenuItem)> = None;
    // kept alive: the menu bar's items live as long as these
    let mut _menus: Option<Menu> = None;
    let mut close_to_tray = true;
    let mut recording = false;
    let mut resized_at: Option<Instant> = None;
    let mut told: Option<(u32, u32)> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        let show = |s: &Shown, target: &EventLoopWindowTarget<ToUi>| {
            target.set_activation_policy_at_runtime(ActivationPolicy::Regular);
            s.window.set_visible(true);
            s.window.set_minimized(false);
            s.window.set_focus();
        };
        // Out of sight, and out of the Dock: a menu bar icon only.
        let hide = |s: &Shown, target: &EventLoopWindowTarget<ToUi>, told: &mut Option<(u32, u32)>| {
            report_size(&tx, &s.window, told);
            s.window.set_visible(false);
            target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
        };
        match event {
            Event::NewEvents(StartCause::Init) => {
                match app_menu() {
                    Ok((m, window)) => {
                        m.init_for_nsapp();
                        // (muda finds it in the app's menu bar, so only now)
                        window.set_as_windows_menu_for_nsapp();
                        _menus = Some(m);
                    }
                    Err(e) => eprintln!("bananasplit: {e}"),
                }
                match tray() {
                    Ok(t) => icon_menu = Some(t),
                    Err(e) => {
                        eprintln!("bananasplit: {e}");
                        let _ = tx.send(FromUi::NoTray);
                    }
                }
            }
            Event::UserEvent(ToUi::Start(o)) => {
                close_to_tray = o.close_to_tray;
                if shown.is_none() {
                    match make_window(target, &o) {
                        Ok(s) => {
                            // The size it opened with (fitted to this screen) is
                            // not the user's choice: only report sizes they make.
                            let opened = s.window.inner_size().to_logical::<f64>(s.window.scale_factor());
                            told = Some((opened.width.round() as u32, opened.height.round() as u32));
                            // Hidden only with an icon to come back from.
                            if !o.hidden || icon_menu.is_none() {
                                show(&s, target);
                            }
                            shown = Some(s);
                        }
                        Err(e) => {
                            let _ = tx.send(FromUi::NoWindow(e));
                        }
                    }
                } else if !o.hidden
                    && let Some(s) = &shown
                {
                    show(s, target);
                }
            }
            Event::UserEvent(ToUi::Show) => {
                if let Some(s) = &shown {
                    show(s, target);
                }
            }
            Event::UserEvent(ToUi::Hide) => {
                if let Some(s) = &shown {
                    hide(s, target, &mut told);
                }
            }
            Event::UserEvent(ToUi::CloseToTray(on)) => close_to_tray = on,
            Event::UserEvent(ToUi::Rec(on)) => {
                if on != recording {
                    recording = on;
                    if let Some((t, item)) = &icon_menu {
                        let _ = t.set_icon(icon(if on { ICON_REC } else { ICON_IDLE }));
                        let _ = t.set_tooltip(Some(if on { "BananaSplit (recording)" } else { "BananaSplit" }));
                        item.set_text(if on { "Stop recording" } else { "Start recording" });
                    }
                }
            }
            Event::UserEvent(ToUi::Exit(code)) => *control_flow = ControlFlow::ExitWithCode(code),
            Event::UserEvent(ToUi::Menu(id)) => match id.as_str() {
                MENU_SHOW => {
                    if let Some(s) = &shown {
                        show(s, target);
                    }
                }
                MENU_RECORD => {
                    let _ = tx.send(FromUi::Record);
                }
                MENU_QUIT => {
                    // The mixer saves, then ends the app.
                    if let Some(s) = &shown {
                        hide(s, target, &mut told);
                    }
                    let _ = tx.send(FromUi::Quit);
                }
                _ => {}
            },
            // A click on the Dock icon, or the app opened again in Finder.
            Event::Reopen { .. } => {
                if let Some(s) = &shown {
                    show(s, target);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if let Some(s) = &shown {
                    hide(s, target, &mut told);
                    if !close_to_tray || icon_menu.is_none() {
                        // Quit BananaSplit: the mixer saves, then ends the app.
                        let _ = tx.send(FromUi::Quit);
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => resized_at = Some(Instant::now()),
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if resized_at.is_some_and(|t| t.elapsed() >= SIZE_SETTLE) {
                    resized_at = None;
                    if let Some(s) = &shown {
                        report_size(&tx, &s.window, &mut told);
                    }
                }
            }
            Event::LoopDestroyed => {
                // macOS ends the app (or the mixer did): the mixer saves and
                // stops first. The window goes at once: the mixer's Hide
                // can't reach this thread while it waits here.
                if let Some(s) = &shown {
                    s.window.set_visible(false);
                }
                crate::sys::QUIT.store(true, SeqCst);
                let t0 = Instant::now();
                while !mixer_done.load(SeqCst) && t0.elapsed() < QUIT_WAIT {
                    std::thread::sleep(Duration::from_millis(20));
                }
                icon_menu = None;
                _menus = None;
                shown = None;
            }
            _ => {}
        }
        if let Some(t) = resized_at
            && !matches!(*control_flow, ControlFlow::ExitWithCode(_))
        {
            *control_flow = ControlFlow::WaitUntil(t + SIZE_SETTLE);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_fits_the_screen() {
        assert_eq!(fit_size(None, Some((2560, 1440))), (1200, 800));
        assert_eq!(fit_size(None, Some((1440, 900))), (1200, 780));
        assert_eq!(fit_size(Some((1600, 900)), Some((1280, 800))), (1232, 680));
        assert_eq!(fit_size(Some((1600, 900)), None), (1600, 900));
        assert_eq!(fit_size(None, Some((640, 480))), (640, 480));
    }

    #[test]
    fn icons_are_square_argb() {
        assert_eq!(ICON_IDLE.len(), 64 * 64 * 4);
        assert_eq!(ICON_REC.len(), 64 * 64 * 4);
        assert_eq!(rgba(&[1, 2, 3, 4, 5, 6, 7, 8]), [2, 3, 4, 1, 6, 7, 8, 5]);
        assert!(icon(ICON_IDLE).is_some() && icon(ICON_REC).is_some());
    }
}
