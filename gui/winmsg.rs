//! What the mixer and its window say to each other, in one place.
//!
//! * Linux and Windows run the window as a process of its own
//!   (`bananasplit --window`) and talk to it over its stdin and stdout, one
//!   line per message: [`ToWindow::line`] and [`FromWindow::line`] write the
//!   lines, `parse` reads them, on both ends.
//! * macOS has the window in the mixer's own process and sends the same
//!   messages through its event loop (`mac/app.rs`), without the lines.
//!
//! A window process first prints a `Hello` (`window.rs`: `ready app`,
//! `ready app tray`, `ready tray <why>` or `fail <why>`), then [`FromWindow`]
//! lines as things happen; the mixer writes [`ToWindow`] lines. A line one side does not know
//! is ignored by the other, as it always was: an older window works with a
//! newer mixer and the other way round.

/// What the mixer tells the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ToWindow {
    /// Bring the window up. The launch's startup or activation token, if
    /// there is one (`show -` on the wire: none).
    Show(Option<String>),
    /// Out of sight at once (the mixer is quitting).
    Hide,
    /// "Closing the window keeps it running" on or off.
    CloseToTray(bool),
    /// The recorder runs: the tray icon and its menu show it.
    Rec(bool),
    /// End the window.
    #[cfg_attr(target_os = "macos", allow(dead_code))] // macOS ends its window with its own `Exit`
    Quit,
}

/// The lines are for the window process (Linux, Windows); macOS sends the messages as they are.
#[cfg(any(not(target_os = "macos"), test))]
impl ToWindow {
    /// The line for the window's stdin (without the newline).
    pub fn line(&self) -> String {
        match self {
            ToWindow::Show(token) => format!("show {}", token.as_deref().filter(|t| !t.is_empty()).unwrap_or("-")),
            ToWindow::Hide => "hide".into(),
            ToWindow::CloseToTray(on) => format!("close-to-tray {}", u8::from(*on)),
            ToWindow::Rec(on) => format!("rec {}", u8::from(*on)),
            ToWindow::Quit => "quit".into(),
        }
    }

    /// One line from the mixer; `None` for one this window does not know.
    /// `rec` is on only for `1` and `close-to-tray` off only for `0`, as the
    /// windows always read them.
    pub fn parse(line: &str) -> Option<ToWindow> {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        Some(match cmd {
            "show" => ToWindow::Show(Some(arg).filter(|t| !t.is_empty() && *t != "-").map(str::to_string)),
            "hide" => ToWindow::Hide,
            "close-to-tray" => ToWindow::CloseToTray(arg != "0"),
            "rec" => ToWindow::Rec(arg == "1"),
            "quit" => ToWindow::Quit,
            _ => return None,
        })
    }
}

/// What the window tells the mixer.
#[derive(Debug, Clone, PartialEq)]
pub enum FromWindow {
    /// Show the page in a browser (there is no window of our own, only the
    /// tray icon).
    #[cfg_attr(target_os = "macos", allow(dead_code))] // macOS always has its window
    Open,
    /// Start or stop recording (the tray icon's menu).
    Record,
    /// Quit BananaSplit (the window was closed with "Closing the window keeps
    /// it running" off, or Quit was chosen in the tray menu).
    Quit,
    /// The size the user left the window at (logical pixels).
    Size(u32, u32),
}

/// The lines are for the window process (Linux, Windows); macOS sends the messages as they are.
#[cfg(any(not(target_os = "macos"), test))]
impl FromWindow {
    /// The line for the window's stdout (without the newline).
    pub fn line(&self) -> String {
        match self {
            FromWindow::Open => "open".into(),
            FromWindow::Record => "record".into(),
            FromWindow::Quit => "quit".into(),
            FromWindow::Size(w, h) => format!("size {w} {h}"),
        }
    }

    /// One line from the window; `None` for one the mixer does not know.
    pub fn parse(line: &str) -> Option<FromWindow> {
        let mut w = line.split_whitespace();
        match w.next()? {
            "open" => Some(FromWindow::Open),
            "record" => Some(FromWindow::Record),
            "quit" => Some(FromWindow::Quit),
            "size" => {
                let (a, b) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
                Some(FromWindow::Size(a, b))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact lines on the wire. Windows and Linux windows of any version
    /// speak these, so they never change.
    #[test]
    fn the_lines_are_what_the_windows_always_said() {
        for (m, line) in [
            (ToWindow::Show(None), "show -"),
            (ToWindow::Show(Some(String::new())), "show -"),
            (ToWindow::Show(Some("tok_123".into())), "show tok_123"),
            (ToWindow::Hide, "hide"),
            (ToWindow::CloseToTray(true), "close-to-tray 1"),
            (ToWindow::CloseToTray(false), "close-to-tray 0"),
            (ToWindow::Rec(true), "rec 1"),
            (ToWindow::Rec(false), "rec 0"),
            (ToWindow::Quit, "quit"),
        ] {
            assert_eq!(m.line(), line);
        }
        for (m, line) in [
            (FromWindow::Open, "open"),
            (FromWindow::Record, "record"),
            (FromWindow::Quit, "quit"),
            (FromWindow::Size(1600, 900), "size 1600 900"),
        ] {
            assert_eq!(m.line(), line);
        }
    }

    #[test]
    fn what_is_written_is_read_back() {
        for m in [
            ToWindow::Show(None),
            ToWindow::Show(Some("tok".into())),
            ToWindow::Hide,
            ToWindow::CloseToTray(true),
            ToWindow::CloseToTray(false),
            ToWindow::Rec(true),
            ToWindow::Rec(false),
            ToWindow::Quit,
        ] {
            assert_eq!(ToWindow::parse(&m.line()), Some(m.clone()), "{m:?}");
        }
        for m in [FromWindow::Open, FromWindow::Record, FromWindow::Quit, FromWindow::Size(640, 480)] {
            assert_eq!(FromWindow::parse(&m.line()), Some(m.clone()), "{m:?}");
        }
    }

    /// What the mixer reads from a window, as it did before the types.
    #[test]
    fn window_lines_are_read_leniently() {
        assert_eq!(FromWindow::parse("quit"), Some(FromWindow::Quit));
        assert_eq!(FromWindow::parse("record"), Some(FromWindow::Record));
        assert_eq!(FromWindow::parse("open"), Some(FromWindow::Open));
        assert_eq!(FromWindow::parse("size 1600 900"), Some(FromWindow::Size(1600, 900)));
        assert_eq!(FromWindow::parse("  open  "), Some(FromWindow::Open), "blanks around are ignored");
        assert_eq!(FromWindow::parse("open now"), Some(FromWindow::Open), "so is what follows");
        assert_eq!(FromWindow::parse("size x"), None);
        assert_eq!(FromWindow::parse("size 1"), None);
        assert_eq!(FromWindow::parse("size -1 5"), None);
        assert_eq!(FromWindow::parse("bogus"), None);
        assert_eq!(FromWindow::parse(""), None);
    }

    /// What a window reads from the mixer, as it did before the types:
    /// `rec` is on only for exactly `1`, `close-to-tray` off only for `0`,
    /// `show` without a usable token is `show -`, anything else is ignored.
    #[test]
    fn mixer_lines_are_read_leniently() {
        assert_eq!(ToWindow::parse("rec 1"), Some(ToWindow::Rec(true)));
        assert_eq!(ToWindow::parse("rec 0"), Some(ToWindow::Rec(false)));
        assert_eq!(ToWindow::parse("rec"), Some(ToWindow::Rec(false)));
        assert_eq!(ToWindow::parse("rec yes"), Some(ToWindow::Rec(false)));
        assert_eq!(ToWindow::parse("close-to-tray 0"), Some(ToWindow::CloseToTray(false)));
        assert_eq!(ToWindow::parse("close-to-tray 1"), Some(ToWindow::CloseToTray(true)));
        assert_eq!(ToWindow::parse("close-to-tray"), Some(ToWindow::CloseToTray(true)));
        assert_eq!(ToWindow::parse("show -"), Some(ToWindow::Show(None)));
        assert_eq!(ToWindow::parse("show"), Some(ToWindow::Show(None)));
        assert_eq!(ToWindow::parse("show "), Some(ToWindow::Show(None)));
        assert_eq!(ToWindow::parse("show abc"), Some(ToWindow::Show(Some("abc".into()))));
        assert_eq!(ToWindow::parse("hide"), Some(ToWindow::Hide));
        assert_eq!(ToWindow::parse("quit"), Some(ToWindow::Quit));
        assert_eq!(ToWindow::parse("shutdown"), None);
        assert_eq!(ToWindow::parse(""), None);
    }
}
