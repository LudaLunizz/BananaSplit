//! Native Windows desktop window for the mixer page.
//!
//! The mixer starts this as a process of its own (`bananasplit --window URL
//! …`, see `gui::WindowProc`), so the audio process never loads WebView2: if
//! the window crashes, audio keeps running and the window can be opened again.
//!
//! It speaks the line protocol of the Linux window (`shell.rs`). Mixer →
//! here (stdin): `show TOKEN|-`, `hide`, `close-to-tray 0|1`, `rec 0|1`
//! (the tray icon shows recording) and `quit`. The end of the input means
//! the mixer is gone, however it went, and the window closes with it.
//! Here → mixer (stdout): `ready app [tray]`, `fail REASON`, `quit`,
//! `record` (tray menu) and `size W H` (logical pixels, remembered as
//! `[gui] window`).
//!
//! It also owns BananaSplit's tray icon (`tray.rs`): a left click shows or
//! hides the window; the menu has Show, Start/Stop recording and Quit.
//! Closing the window hides it in the tray (while the icon is not in the
//! notification area, as on a server without Explorer, it minimizes
//! instead). With "Closing the
//! window keeps it running" off in the menu, closing it quits BananaSplit.
//! Started hidden (`--hidden`, the autostart entry), only the icon shows.

use crate::win::ffi::{GetFileType, GetStdHandle, SetForegroundWindow, FILE_TYPE_PIPE, HWND, STD_INPUT_HANDLE};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use crate::tray::{Tray, TrayAction};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Icon, UserAttentionType, Window, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

/// The window's default size (logical pixels), within the screen.
const DEFAULT_SIZE: (u32, u32) = (1200, 800);
/// Report the size once it stops changing (not on every step of a drag).
const SIZE_SETTLE: Duration = Duration::from_millis(500);
/// Started hidden while the notification area is not there (at login,
/// before Explorer): how long to wait for it before the window shows
/// (minimized) instead, so BananaSplit can't end up with nothing to show.
const TRAY_WAIT: Duration = Duration::from_secs(60);
/// Clicking the tray icon takes the focus from the window first: a window
/// that had it this recently counts as the one in front.
const FOCUS_GRACE: Duration = Duration::from_millis(600);
/// The page's background, shown until it has loaded.
const BACKGROUND: (u8, u8, u8, u8) = (11, 16, 21, 255);
/// The app icon (the tray pixmaps of the Linux version: ARGB32, big-endian).
const ICON_32: &[u8] = include_bytes!("../../assets/tray/idle-32.argb");
const ICON_64: &[u8] = include_bytes!("../../assets/tray/idle-64.argb");

#[derive(Debug, PartialEq)]
pub struct Opts {
    pub url: String,
    pub hidden: bool,
    pub close_to_tray: bool,
    pub size: Option<(u32, u32)>,
}

/// Parse `URL [--hidden] [--close-to-tray 0|1] [--size WxH] [--token T]`
/// (what follows `--window`).
pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut it = args.iter();
    let url = it.next().ok_or("--window needs a URL")?.clone();
    if !(url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")) {
        return Err("the desktop window only opens BananaSplit's own page".into());
    }
    let mut o = Opts { url, hidden: false, close_to_tray: true, size: None };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hidden" => o.hidden = true,
            "--close-to-tray" => o.close_to_tray = it.next().map(String::as_str) != Some("0"),
            "--size" => {
                let v = it.next().ok_or("--size needs WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size needs WxH")?;
                let (w, h) = (w.parse::<u32>().map_err(|e| e.to_string())?, h.parse::<u32>().map_err(|e| e.to_string())?);
                o.size = Some((w.clamp(640, 8192), h.clamp(480, 8192)));
            }
            // A desktop activation token (Linux); Windows has none.
            "--token" => {
                it.next();
            }
            x => return Err(format!("unknown argument {x:?}")),
        }
    }
    Ok(o)
}

/// The size to open with: the remembered or default one, within the screen
/// (leaving room for the taskbar and the title bar).
pub fn fit_size(want: Option<(u32, u32)>, screen: Option<(u32, u32)>) -> (u32, u32) {
    let (w, h) = want.unwrap_or(DEFAULT_SIZE);
    match screen {
        Some((sw, sh)) if sw > 0 && sh > 0 => (w.min(sw.saturating_sub(48)).max(640), h.min(sh.saturating_sub(120)).max(480)),
        _ => (w, h),
    }
}

/// One line to the mixer. Nobody reads it when started by hand; that's fine.
fn tell(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// Started by the mixer (input is a pipe from it), not by hand.
fn started_by_mixer() -> bool {
    // SAFETY: plain Win32 calls on this process's standard input handle.
    unsafe { GetFileType(GetStdHandle(STD_INPUT_HANDLE)) == FILE_TYPE_PIPE }
}

fn icon(argb: &[u8], side: u32) -> Option<Icon> {
    let rgba = argb.as_chunks::<4>().0.iter().flat_map(|p| [p[1], p[2], p[3], p[0]]).collect();
    Icon::from_rgba(rgba, side, side).ok()
}

/// WebView2 keeps its profile here instead of next to the program (where it
/// could not be written to, under Program Files say).
fn webview_data() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(base).join("bananasplit").join("WebView2"))
}

/// Bring the window to the front when Windows lets this process (the start
/// that asked for it passes its right on); otherwise flash its taskbar
/// button. Not `Window::set_focus`: when Windows says no, that fakes an Alt
/// key press, which lands in whatever the user is typing in (and can leave
/// a window in menu mode).
fn bring_to_front(window: &Window) {
    // SAFETY: our own, live window handle.
    if unsafe { SetForegroundWindow(window.hwnd() as HWND) } == 0 {
        window.request_user_attention(Some(UserAttentionType::Informational));
    }
}

/// Tell the mixer the window's size, if it changed and is a normal one.
fn report_size(window: &Window, told: &mut Option<(u32, u32)>) {
    if window.is_minimized() || window.is_maximized() || !window.is_visible() {
        return;
    }
    let s = window.inner_size().to_logical::<f64>(window.scale_factor());
    let size = (s.width.round() as u32, s.height.round() as u32);
    if size.0 > 0 && size.1 > 0 && *told != Some(size) {
        *told = Some(size);
        tell(&format!("size {} {}", size.0, size.1));
    }
}

enum FromMixer {
    Line(String),
    Gone,
    /// Not from the mixer: the tray icon (same event loop).
    Tray(TrayAction),
}

/// Show the window, bring it up, and bring it to the front.
fn show(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    bring_to_front(window);
}

/// `bananasplit --window …`: runs until the mixer goes away or says `quit`
/// (or, when started by hand, until the window is closed).
pub fn main(args: &[String]) -> ExitCode {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            tell(&format!("fail {e}"));
            return ExitCode::from(2);
        }
    };
    match run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tell(&format!("fail {e}"));
            ExitCode::FAILURE
        }
    }
}

fn run(opts: Opts) -> Result<(), String> {
    let owned = started_by_mixer();
    let event_loop = EventLoopBuilder::<FromMixer>::with_user_event().build();
    let screen = event_loop.primary_monitor().map(|m| {
        let s = m.size().to_logical::<f64>(m.scale_factor());
        (s.width as u32, s.height as u32)
    });
    let (w, h) = fit_size(opts.size, screen);
    // The tray icon, on this thread (its messages come through this event
    // loop). Not when started by hand: there is no mixer to tell.
    let mut tray = if owned {
        let proxy = event_loop.create_proxy();
        match Tray::add(move |a| {
            let _ = proxy.send_event(FromMixer::Tray(a));
        }) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("bananasplit window: no tray icon: {e}");
                None
            }
        }
    } else {
        None
    };
    let has_tray = tray.is_some();
    let window = WindowBuilder::new()
        .with_title("BananaSplit")
        .with_inner_size(LogicalSize::new(f64::from(w), f64::from(h)))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .with_background_color(BACKGROUND)
        .with_window_icon(icon(ICON_32, 32))
        .with_taskbar_icon(icon(ICON_64, 64))
        // Started in the background with a tray icon: only the icon shows.
        .with_visible(!(opts.hidden && has_tray))
        .build(&event_loop)
        .map_err(|e| format!("could not create the window: {e}"))?;
    if opts.hidden && !has_tray {
        window.set_minimized(true);
    }
    let mut context = WebContext::new(webview_data());
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&opts.url)
        .with_background_color(BACKGROUND)
        .build(&window)
        .map_err(|e| format!("could not start WebView2: {e}"))?;
    tell(if has_tray { "ready app tray" } else { "ready app" });

    if owned {
        let proxy = event_loop.create_proxy();
        let reader = std::thread::Builder::new().name("bs-window-input".into()).spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if proxy.send_event(FromMixer::Line(line.trim().to_string())).is_err() {
                    return;
                }
            }
            // The mixer quit, crashed or was ended: the window goes too.
            let _ = proxy.send_event(FromMixer::Gone);
        });
        if reader.is_err() {
            return Err("cannot start a thread".into());
        }
    }

    let mut close_to_tray = opts.close_to_tray;
    let mut quitting = false;
    let mut resized_at: Option<Instant> = None;
    // The size it opened with (fitted to this screen) is not the user's
    // choice: only report sizes the user makes.
    let opened = window.inner_size().to_logical::<f64>(window.scale_factor());
    let mut told = Some((opened.width.round() as u32, opened.height.round() as u32));
    // When the window last lost the focus (None: it has it, or never had).
    let mut blurred: Option<Instant> = None;
    // Started hidden, but the icon is not in the notification area yet.
    let mut tray_wait = (opts.hidden && tray.as_ref().is_some_and(|t| !t.shown())).then(|| Instant::now() + TRAY_WAIT);
    event_loop.run(move |event, _, control_flow| {
        let _keep = (&webview, &context);
        match event {
            Event::UserEvent(FromMixer::Gone) => quitting = true,
            Event::UserEvent(FromMixer::Tray(action)) => match action {
                TrayAction::Toggle => {
                    let in_front = window.is_focused() || blurred.is_some_and(|t| t.elapsed() < FOCUS_GRACE);
                    if window.is_visible() && !window.is_minimized() && in_front {
                        report_size(&window, &mut told);
                        window.set_visible(false);
                    } else {
                        show(&window);
                    }
                }
                TrayAction::Show => show(&window),
                TrayAction::Record => tell("record"),
                TrayAction::Quit => {
                    // The mixer saves, then ends this window.
                    report_size(&window, &mut told);
                    window.set_visible(false);
                    tell("quit");
                }
            },
            Event::UserEvent(FromMixer::Line(line)) => {
                let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                match cmd {
                    "show" => show(&window),
                    "hide" => {
                        report_size(&window, &mut told);
                        window.set_visible(false);
                    }
                    "close-to-tray" => close_to_tray = arg != "0",
                    "rec" => {
                        if let Some(t) = &tray {
                            t.set_recording(arg == "1");
                        }
                    }
                    "quit" => quitting = true,
                    _ => {}
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if !owned {
                    quitting = true;
                } else if close_to_tray && tray.as_ref().is_some_and(Tray::shown) {
                    report_size(&window, &mut told);
                    window.set_visible(false);
                } else if close_to_tray {
                    window.set_minimized(true);
                } else {
                    // Quit BananaSplit: the mixer saves, then ends this window.
                    report_size(&window, &mut told);
                    window.set_visible(false);
                    tell("quit");
                }
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => resized_at = Some(Instant::now()),
            Event::WindowEvent { event: WindowEvent::Focused(f), .. } => blurred = if f { None } else { Some(Instant::now()) },
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if resized_at.is_some_and(|t| t.elapsed() >= SIZE_SETTLE) {
                    resized_at = None;
                    report_size(&window, &mut told);
                }
                if tray_wait.is_some_and(|t| Instant::now() >= t) {
                    tray_wait = None;
                    if !window.is_visible() && !tray.as_ref().is_some_and(Tray::shown) {
                        // No notification area after all: on the taskbar.
                        window.set_visible(true);
                        window.set_minimized(true);
                    }
                }
            }
            _ => {}
        }
        if tray_wait.is_some() && tray.as_ref().is_some_and(Tray::shown) {
            tray_wait = None;
        }
        *control_flow = if quitting {
            // tao ends the process without running destructors: take the
            // icon out of the notification area now, or a dead one stays.
            tray = None;
            ControlFlow::Exit
        } else if let Some(t) = [resized_at.map(|t| t + SIZE_SETTLE), tray_wait].into_iter().flatten().min() {
            ControlFlow::WaitUntil(t)
        } else {
            ControlFlow::Wait
        };
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_args() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let o = a(&["http://127.0.0.1:8790/", "--hidden", "--close-to-tray", "0", "--size", "1300x700", "--token", "x"]).unwrap();
        assert_eq!(o, Opts { url: "http://127.0.0.1:8790/".into(), hidden: true, close_to_tray: false, size: Some((1300, 700)) });
        let o = a(&["http://127.0.0.1:8790/"]).unwrap();
        assert!(!o.hidden && o.close_to_tray && o.size.is_none());
        assert!(a(&["https://example.com/"]).is_err());
        assert!(a(&[]).is_err());
        assert!(a(&["http://127.0.0.1:1/", "--size", "big"]).is_err());
        assert!(a(&["http://127.0.0.1:1/", "--colour", "red"]).is_err());
        assert_eq!(a(&["http://127.0.0.1:1/", "--size", "10x10"]).unwrap().size, Some((640, 480)));
    }

    #[test]
    fn window_size_fits_the_screen() {
        assert_eq!(fit_size(None, Some((2560, 1440))), (1200, 800));
        assert_eq!(fit_size(None, Some((1366, 768))), (1200, 648));
        assert_eq!(fit_size(Some((1600, 900)), Some((1280, 720))), (1232, 600));
        assert_eq!(fit_size(Some((1600, 900)), None), (1600, 900));
        assert_eq!(fit_size(None, Some((640, 480))), (640, 480));
    }

    #[test]
    fn icons_are_square_argb() {
        assert_eq!(ICON_32.len(), 32 * 32 * 4);
        assert_eq!(ICON_64.len(), 64 * 64 * 4);
        assert!(icon(ICON_32, 32).is_some() && icon(ICON_64, 64).is_some());
    }
}
