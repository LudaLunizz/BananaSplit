//! Windows console: raw keyboard input, ANSI/VT output, Ctrl-C and
//! window-close handling. The terminal UI itself (`tui.rs`) is shared with
//! Linux; this module only makes the Windows console behave like a raw
//! VT terminal.

use super::ffi::*;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

/// Set by Ctrl-C / Ctrl-Break / closing the window (and by `q` in the UI).
pub static QUIT: AtomicBool = AtomicBool::new(false);
/// Set once `main` has saved and released the audio devices.
pub static CLEAN_EXIT: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
    QUIT.store(true, SeqCst);
    if matches!(
        kind,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        // Windows kills the process as soon as this handler returns, so
        // give the main loop (which polls every ~33 ms) time to save the
        // config and stop the devices. Windows allows up to 5 s.
        for _ in 0..90 {
            if CLEAN_EXIT.load(SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    1 // handled
}

pub fn install_signal_handlers() {
    // SAFETY: `on_ctrl` is a `extern "system"` HandlerRoutine that lives for the whole program.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}

fn handle(which: u32) -> HANDLE {
    // SAFETY: GetStdHandle takes a plain constant; the handle is only passed to console calls.
    unsafe { GetStdHandle(which) }
}

pub fn stdin_is_tty() -> bool {
    let mut m = 0u32;
    // SAFETY: `m` is a local u32 out-pointer; a non-console handle just makes GetConsoleMode fail.
    unsafe {
        GetConsoleMode(handle(STD_INPUT_HANDLE), &mut m) != 0
            && GetConsoleMode(handle(STD_OUTPUT_HANDLE), &mut m) != 0
    }
}

struct Saved {
    input: u32,
    output: u32,
    cp: u32,
}
static ORIGINAL: OnceLock<Saved> = OnceLock::new();

fn restore() {
    if let Some(s) = ORIGINAL.get() {
        // SAFETY: plain console calls with the modes `enter` saved from these same handles.
        unsafe {
            SetConsoleMode(handle(STD_INPUT_HANDLE), s.input);
            SetConsoleOutputCP(s.cp);
        }
        print!("\x1b[0m\x1b[?25h\x1b[?1049l");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        // SAFETY: plain console call with the mode `enter` saved from this handle.
        unsafe { SetConsoleMode(handle(STD_OUTPUT_HANDLE), s.output) };
    }
}

/// Raw keyboard input + VT output + alternate screen; restored on drop and on panic.
pub struct RawTerminal;

impl RawTerminal {
    pub fn enter() -> Result<RawTerminal, String> {
        let (hin, hout) = (handle(STD_INPUT_HANDLE), handle(STD_OUTPUT_HANDLE));
        let (mut min, mut mout) = (0u32, 0u32);
        // SAFETY: `min` is a local u32 out-pointer; a non-console handle just fails.
        if unsafe { GetConsoleMode(hin, &mut min) } == 0
            // SAFETY: `mout` is a local u32 out-pointer; a non-console handle just fails.
            || unsafe { GetConsoleMode(hout, &mut mout) } == 0
        {
            return Err("not running in a console window (use --headless)".into());
        }
        let _ = ORIGINAL.set(Saved {
            input: min,
            output: mout,
            // SAFETY: plain call, no arguments.
            cp: unsafe { GetConsoleOutputCP() },
        });

        // Input: no line editing, no echo, Ctrl-C arrives as a key (byte 3),
        // no mouse, and no QuickEdit (a stray click would freeze the UI).
        let raw_in = (min
            & !(ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_MOUSE_INPUT
                | ENABLE_QUICK_EDIT_MODE))
            | ENABLE_EXTENDED_FLAGS;
        // Output: interpret ANSI escape codes (Windows 10 1511+).
        let vt_out = mout | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        // SAFETY: plain console calls on our std handles; `title` is a NUL-terminated UTF-16 local.
        unsafe {
            SetConsoleMode(hin, raw_in);
            if SetConsoleMode(hout, vt_out) == 0 {
                SetConsoleMode(hin, min);
                return Err(
                    "this console cannot show colours/escape codes (needs Windows 10 or newer)"
                        .into(),
                );
            }
            SetConsoleOutputCP(CP_UTF8);
            let title = wide("BananaSplit mixer");
            SetConsoleTitleW(title.as_ptr());
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));
        print!("\x1b[?1049h\x1b[?25l\x1b[2J");
        Ok(RawTerminal)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        restore();
    }
}

/// Translate one console key event into the bytes a VT terminal would send,
/// so the shared key decoder (`tui::decode`) works unchanged.
pub fn key_bytes(rec: &INPUT_RECORD, out: &mut Vec<u8>) {
    if rec.EventType != KEY_EVENT || rec.bKeyDown == 0 {
        return;
    }
    let seq: &[u8] = match rec.wVirtualKeyCode {
        VK_UP => b"\x1b[A",
        VK_DOWN => b"\x1b[B",
        VK_RIGHT => b"\x1b[C",
        VK_LEFT => b"\x1b[D",
        _ => b"",
    };
    let repeat = rec.wRepeatCount.max(1);
    for _ in 0..repeat {
        if !seq.is_empty() {
            out.extend_from_slice(seq);
        } else if rec.UnicodeChar != 0
            && let Some(c) = char::from_u32(rec.UnicodeChar as u32) {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
    }
}

/// Forward keyboard input to the UI thread as VT bytes.
pub fn spawn_input_thread(tx: std::sync::mpsc::Sender<Vec<u8>>) {
    let h = handle(STD_INPUT_HANDLE) as usize;
    std::thread::spawn(move || {
        let h = h as HANDLE;
        let mut recs = [INPUT_RECORD::default(); 32];
        loop {
            let mut n = 0u32;
            // SAFETY: `recs` and its length are passed together; `n` is a local out-pointer (at most the length).
            if unsafe { ReadConsoleInputW(h, recs.as_mut_ptr(), recs.len() as u32, &mut n) } == 0 {
                break;
            }
            let mut bytes = Vec::new();
            for r in &recs[..n as usize] {
                key_bytes(r, &mut bytes);
            }
            if !bytes.is_empty() && tx.send(bytes).is_err() {
                break;
            }
        }
    });
}

/// Is this the only process in its console? Then Windows opened that
/// console window for it (a double-click, a shortcut, the autostart entry);
/// it is not running in somebody's terminal (cmd, PowerShell).
fn owns_console() -> bool {
    let mut pids = [0u32; 4];
    // SAFETY: the buffer holds `pids.len()` process ids.
    unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 }
}

/// When BananaSplit was started by double-clicking it, its console window
/// closes the moment it exits, so an error message would flash by unread.
/// Keep the window open until a key is pressed in that case.
pub fn pause_on_error() {
    if owns_console() {
        eprintln!("\nPress Enter to close this window.");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
}

/// Set once BananaSplit borrowed the console of the terminal it was run from.
static ATTACHED: AtomicBool = AtomicBool::new(false);

/// A standard handle that goes somewhere: a console, a file or a pipe.
fn usable(which: u32) -> bool {
    // SAFETY: plain Win32 calls on this process's standard handles.
    unsafe {
        let h = GetStdHandle(which);
        !h.is_null() && h != INVALID_HANDLE_VALUE && GetFileType(h) != FILE_TYPE_UNKNOWN
    }
}

/// BananaSplit is a Windows (GUI) program, so Windows gives it no console
/// window. Run from a terminal (cmd, PowerShell) for a command-line option
/// (`--help`, `--list-devices`, `--headless` …), it borrows that terminal's
/// console for what it prints and for Ctrl-C. Output that is already
/// redirected (to a file or a pipe) stays where it goes. True if it is
/// now attached to a console.
pub fn attach_parent() -> bool {
    if usable(STD_OUTPUT_HANDLE) && usable(STD_ERROR_HANDLE) {
        return false;
    }
    // SAFETY: plain Win32 calls; the handles opened here live as long as
    // the process (they are its standard handles from now on).
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return false;
        }
        for (which, name) in [(STD_INPUT_HANDLE, "CONIN$"), (STD_OUTPUT_HANDLE, "CONOUT$"), (STD_ERROR_HANDLE, "CONOUT$")] {
            if usable(which) {
                continue;
            }
            let n = wide(name);
            let h = CreateFileW(
                n.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(which, h);
            }
        }
    }
    ATTACHED.store(true, SeqCst);
    // The terminal printed its prompt already (it does not wait for a
    // Windows program): start on a line of our own.
    eprintln!();
    true
}

/// Is there someone to read what BananaSplit prints? (It borrowed a
/// terminal's console, or its output is redirected.) If not, an error that
/// stops it is shown in a dialog.
pub fn has_output() -> bool {
    ATTACHED.load(SeqCst) || usable(STD_ERROR_HANDLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{Key, decode};

    fn key(vk: u16, ch: u16) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT,
            bKeyDown: 1,
            wRepeatCount: 1,
            wVirtualKeyCode: vk,
            UnicodeChar: ch,
            ..Default::default()
        }
    }

    #[test]
    fn console_keys_decode_like_a_vt_terminal() {
        let mut b = Vec::new();
        for r in [
            key(VK_UP, 0),
            key(VK_LEFT, 0),
            key(0x51, b'q' as u16),
            key(0x43, 3),
            key(0xBE, b'.' as u16),
        ] {
            key_bytes(&r, &mut b);
        }
        // key-up events and non-key events are ignored
        key_bytes(
            &INPUT_RECORD {
                bKeyDown: 0,
                ..key(VK_DOWN, 0)
            },
            &mut b,
        );
        key_bytes(
            &INPUT_RECORD {
                EventType: 2,
                ..key(VK_DOWN, 0)
            },
            &mut b,
        );
        // shift/ctrl alone produce no character
        key_bytes(&key(0x10, 0), &mut b);
        assert_eq!(
            decode(&mut b),
            vec![
                Key::Up,
                Key::Left,
                Key::Char(b'q'),
                Key::Char(3),
                Key::Char(b'.')
            ]
        );
        let mut rep = Vec::new();
        key_bytes(
            &INPUT_RECORD {
                wRepeatCount: 3,
                ..key(VK_RIGHT, 0)
            },
            &mut rep,
        );
        assert_eq!(decode(&mut rep), vec![Key::Right; 3]);
    }
}

