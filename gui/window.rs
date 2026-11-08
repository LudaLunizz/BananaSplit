//! The app window process (`bananasplit --window`), seen from the mixer: started
//! and watched here, in the apps that show the page in a process of their own
//! (Linux, Windows). macOS shows it in a window of the mixer's own process.
//!
//! What differs between the systems is in `gui_platform`: how the process is
//! started (`window_command`) and where its error output goes (`window_stderr`).
//! The protocol is the same everywhere and written down in `winmsg.rs`: the
//! window prints a [`Hello`] once, then a [`FromWindow`] line per event; the
//! mixer writes [`ToWindow`] lines, or closes the window's input to end it.

use super::{FromWindow, ToWindow};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const WINDOW_FLAG: &str = "--window";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinMode {
    /// Our own window (WebKitGTK, WebView2), with the tray icon.
    App,
    /// Only the tray icon; the page is shown in a browser.
    Tray,
}

/// The first line a window process prints: what it can do, or why it cannot.
#[derive(Debug, Clone, PartialEq)]
pub enum Hello {
    /// `ready app` (a window of our own), `ready app tray` (with a tray icon
    /// too) or `ready tray <why>` (the tray icon only: `note` says why there is
    /// no window, and the page is shown in a browser).
    Ready { app: bool, tray: bool, note: String },
    /// `fail <why>`: neither a window nor a tray icon.
    Fail(String),
}

impl Hello {
    /// A window of our own, with a tray icon or without one.
    pub fn app(tray: bool) -> Hello {
        Hello::Ready { app: true, tray, note: String::new() }
    }

    /// Only the tray icon; `why` there is no window of our own.
    #[allow(dead_code)] // the Linux window falls back to this; the Windows one fails instead
    pub fn tray_only(why: impl Into<String>) -> Hello {
        Hello::Ready { app: false, tray: true, note: why.into() }
    }

    /// The line for the window's stdout (without the newline).
    pub fn line(&self) -> String {
        match self {
            Hello::Ready { app: true, tray: false, .. } => "ready app".into(),
            Hello::Ready { app: true, tray: true, .. } => "ready app tray".into(),
            // (with the space after `tray`, empty reason or not, as the window always printed it)
            Hello::Ready { app: false, note, .. } => format!("ready tray {note}"),
            Hello::Fail(why) => format!("fail {why}"),
        }
    }

    /// The window's first line. Anything that is not `ready ...` is a failure
    /// (with the text after `fail ` if it has that, else the whole line).
    pub fn parse(line: &str) -> Hello {
        let Some(m) = line.strip_prefix("ready ") else {
            return Hello::Fail(line.strip_prefix("fail ").unwrap_or(line).to_string());
        };
        let app = m.starts_with("app");
        let note = if app { String::new() } else { m.strip_prefix("tray").unwrap_or("").trim().to_string() };
        Hello::Ready { app, tray: m.split_whitespace().any(|w| w == "tray"), note }
    }
}

/// What the reader thread passes on: a message, or that the pipe ended.
enum Event {
    Msg(FromWindow),
    Gone,
}

pub struct WinOpts<'a> {
    pub url: &'a str,
    pub hidden: bool,
    pub close_to_tray: bool,
    pub size: Option<(u32, u32)>,
    pub token: Option<&'a str>,
}

/// How long the window process may take to say it is ready.
const WINDOW_START: Duration = Duration::from_secs(60);

/// The window process, seen from the mixer.
pub struct WindowProc {
    child: Child,
    stdin: Option<ChildStdin>,
    #[allow(dead_code)] // read by the Linux app (main.rs)
    pub mode: WinMode,
    /// It has a tray icon (`ready app tray`): closing the window hides it there.
    #[allow(dead_code)] // read by the Windows app (main.rs)
    pub tray: bool,
    rx: mpsc::Receiver<Event>,
    gone: bool,
}

impl WindowProc {
    /// Start it and wait until it says what it can do. `Err` explains why
    /// there is neither a window nor a tray icon (then use a browser).
    pub fn spawn(o: &WinOpts) -> Result<WindowProc, String> {
        let mut cmd = crate::gui_platform::window_command()?;
        cmd.arg(WINDOW_FLAG).arg(o.url).arg("--close-to-tray").arg(if o.close_to_tray { "1" } else { "0" });
        if o.hidden {
            cmd.arg("--hidden");
        }
        if let Some((w, h)) = o.size {
            cmd.arg("--size").arg(format!("{w}x{h}"));
        }
        if let Some(t) = o.token.filter(|t| !t.is_empty()) {
            cmd.arg("--token").arg(t);
        }
        // The launch token was for this window, not for anything it starts.
        cmd.env_remove("DESKTOP_STARTUP_ID").env_remove("XDG_ACTIVATION_TOKEN");
        // Where its messages go besides `fail …` lines: the terminal, if any.
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(crate::gui_platform::window_stderr());
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the window: {e}"))?;
        let stdout = child.stdout.take().ok_or("no pipe from the window")?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(WinMode, bool), String>>();
        let reader = std::thread::Builder::new().name("bs-window".into()).spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(r) = ready.take() {
                    let first = match Hello::parse(&line) {
                        Hello::Ready { app, tray, .. } => Ok((if app { WinMode::App } else { WinMode::Tray }, tray)),
                        Hello::Fail(why) => Err(why),
                    };
                    let failed = first.is_err();
                    let _ = r.send(first);
                    if failed {
                        break;
                    }
                    continue;
                }
                if let Some(m) = FromWindow::parse(&line)
                    && tx.send(Event::Msg(m)).is_err()
                {
                    break;
                }
            }
            let _ = tx.send(Event::Gone);
        });
        if reader.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cannot start a thread".into());
        }
        let stdin = child.stdin.take();
        // A window that ends at start ends this wait at once (Disconnected);
        // one that is still starting gets WINDOW_START: a cold WebView2 or
        // WebKit start on a slow computer can take well over 15 s.
        match ready_rx.recv_timeout(WINDOW_START) {
            Ok(Ok((mode, tray))) => Ok(WindowProc { child, stdin, mode, tray, rx, gone: false }),
            Ok(Err(why)) => {
                let _ = child.wait();
                Err(why)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.wait();
                Err("the window ended before it opened".into())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(format!("the window did not open within {} s", WINDOW_START.as_secs()))
            }
        }
    }

    pub fn send(&mut self, m: ToWindow) {
        if let Some(s) = &mut self.stdin
            && s.write_all(m.line().as_bytes()).and_then(|_| s.write_all(b"\n")).and_then(|_| s.flush()).is_err()
        {
            self.stdin = None;
        }
    }

    /// What the window has said since the last call. (That it has ended is not
    /// a message: `alive` says it.)
    pub fn poll(&mut self) -> Vec<FromWindow> {
        let mut said = Vec::new();
        for e in self.rx.try_iter() {
            match e {
                Event::Msg(m) => said.push(m),
                Event::Gone => self.gone = true,
            }
        }
        said
    }

    pub fn alive(&mut self) -> bool {
        !self.gone && matches!(self.child.try_wait(), Ok(None))
    }

    /// Close it (it saves nothing; the mixer has the state). Dropping it
    /// waits for the process to end.
    pub fn close(mut self) {
        self.send(ToWindow::Quit);
    }
}

impl Drop for WindowProc {
    /// Every way out (close, a crashed or closed window, a restart) ends
    /// here, so the process is always waited for (where an unwaited child
    /// stays a zombie, on Linux, it would never go away).
    fn drop(&mut self) {
        self.stdin = None; // end of input: it quits even if it missed "quit"
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A window process that is starting: `WindowProc::spawn` on a thread of
/// its own, so the mixer's loop keeps running (and publishing its state)
/// while a cold WebView2 or WebKit start takes its time.
pub struct WindowStart {
    rx: mpsc::Receiver<Result<WindowProc, String>>,
}

impl WindowStart {
    pub fn begin(o: &WinOpts) -> WindowStart {
        let (url, token) = (o.url.to_string(), o.token.map(str::to_string));
        let (hidden, close_to_tray, size) = (o.hidden, o.close_to_tray, o.size);
        let (tx, rx) = mpsc::channel();
        // Without the thread, `tx` is gone and `poll` reports it.
        let _ = std::thread::Builder::new().name("bs-window-start".into()).spawn(move || {
            let o = WinOpts { url: &url, hidden, close_to_tray, size, token: token.as_deref() };
            // Dropped when nobody waits any more (the mixer is closing):
            // its input ends with it, so the window quits by itself.
            let _ = tx.send(WindowProc::spawn(&o));
        });
        WindowStart { rx }
    }

    /// `None` while it is starting; then, once, how the start went.
    pub fn poll(&self) -> Option<Result<WindowProc, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("cannot start a thread for the window".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact first lines on the wire, as the windows always printed them.
    #[test]
    fn the_hello_lines_are_what_the_windows_always_said() {
        let ready = |app, tray, note: &str| Hello::Ready { app, tray, note: note.into() };
        assert_eq!(ready(true, false, "").line(), "ready app");
        assert_eq!(ready(true, true, "").line(), "ready app tray");
        assert_eq!(ready(false, true, "WebKitGTK is not installed").line(), "ready tray WebKitGTK is not installed");
        assert_eq!(ready(false, true, "").line(), "ready tray ");
        assert_eq!(Hello::app(true), ready(true, true, ""));
        assert_eq!(Hello::tray_only("no display"), ready(false, true, "no display"));
        assert_eq!(Hello::Fail("no D-Bus".into()).line(), "fail no D-Bus");
    }

    #[test]
    fn what_a_window_says_first_is_read_back() {
        for h in [
            Hello::Ready { app: true, tray: false, note: String::new() },
            Hello::Ready { app: true, tray: true, note: String::new() },
            Hello::Ready { app: false, tray: true, note: "no window".into() },
            Hello::Ready { app: false, tray: true, note: String::new() },
            Hello::Fail("GLib: no display".into()),
        ] {
            assert_eq!(Hello::parse(&h.line()), h);
        }
    }

    #[test]
    fn the_first_line_says_what_the_window_can_do() {
        let ready = |app, tray, note: &str| Hello::Ready { app, tray, note: note.into() };
        assert_eq!(Hello::parse("ready app"), ready(true, false, ""));
        assert_eq!(Hello::parse("ready app tray"), ready(true, true, ""));
        assert_eq!(Hello::parse("ready tray WebKitGTK not found"), ready(false, true, "WebKitGTK not found"));
        assert_eq!(Hello::parse("fail GLib: no display"), Hello::Fail("GLib: no display".into()));
        // not "ready ..." and not "fail ...": the whole line is the reason
        assert_eq!(Hello::parse("Segmentation fault"), Hello::Fail("Segmentation fault".into()));
        assert_eq!(Hello::parse("ready"), Hello::Fail("ready".into()));
    }

    /// A window process that ended by itself (a crash) is waited for when
    /// the mixer drops it, so it does not stay a zombie (review #9).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_window_that_died_is_reaped() {
        use std::process::Command;
        let child = Command::new("true").stdin(Stdio::piped()).spawn().unwrap();
        let pid = child.id();
        let (_tx, rx) = mpsc::channel();
        let mut w = WindowProc { child, stdin: None, mode: WinMode::App, tray: false, rx, gone: false };
        while matches!(std::fs::read_to_string(format!("/proc/{pid}/stat")), Ok(s) if !s.contains(") Z ")) {
            std::thread::sleep(Duration::from_millis(5));
        }
        w.gone = true; // the reader saw the pipe close
        assert!(!w.alive());
        drop(w);
        assert!(std::fs::metadata(format!("/proc/{pid}")).is_err(), "pid {pid} is still there");
    }
}
