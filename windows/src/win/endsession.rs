//! Logoff, shutdown and restart: the mixer saves its settings first.
//!
//! A Windows (GUI-subsystem) program gets no console events, so Windows
//! tells it about the end of the session through window messages instead:
//! `WM_QUERYENDSESSION`, then `WM_ENDSESSION`, sent to every top-level
//! window. The mixer has none of its own (its page is shown by a window
//! process), so it keeps a hidden one on a thread of its own:
//!
//! - `WM_QUERYENDSESSION`: BananaSplit never holds up a logoff.
//! - `WM_ENDSESSION` (the session ends): ask the main loop to quit, which
//!   saves the settings, and wait for that. Windows ends the process once
//!   this returns, and allows about five seconds.
//! - `WM_CLOSE` (`taskkill /im bananasplit.exe` without `/f`, an installer's
//!   Restart Manager): quit the same way.
//!
//! The window is never shown and is titled "BananaSplit mixer", so it is not
//! taken for the app window.

use super::console::{CLEAN_EXIT, QUIT};
use std::ffi::c_void;
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

type Hwnd = *mut c_void;
type Handle = *mut c_void;

/// The class of the hidden window (the launch test looks for it).
pub const CLASS: &str = "BananaSplitMixer";
/// How long `WM_ENDSESSION` waits for the settings to be saved.
const SAVE_WAIT: Duration = Duration::from_millis(4500);

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    wndproc: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu_name: *const u16,
    class_name: *const u16,
    icon_sm: Handle,
}

#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w: usize,
    l: isize,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: Handle,
        instance: Handle,
        param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Handle;
}

const WM_CLOSE: u32 = 0x0010;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Ask the main loop to quit and wait (a while) until it has saved and
/// released the audio devices.
fn quit_and_wait() {
    QUIT.store(true, SeqCst);
    let t0 = Instant::now();
    while !CLEAN_EXIT.load(SeqCst) && t0.elapsed() < SAVE_WAIT {
        std::thread::sleep(Duration::from_millis(20));
    }
}

unsafe extern "system" fn wndproc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    match msg {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if w != 0 {
                quit_and_wait();
            }
            0
        }
        WM_CLOSE => {
            // Not DefWindowProc (it would destroy the window): the main loop
            // quits, saves, and the process ends.
            QUIT.store(true, SeqCst);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

/// Start listening for the end of the session (once, early in the mixer).
pub fn watch() {
    let _ = std::thread::Builder::new().name("bs-endsession".into()).spawn(|| {
        // SAFETY: plain Win32 calls; the window and its class live on this
        // thread, which runs its message loop until the process ends.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide(CLASS);
            let wc = WndClassExW {
                cb_size: std::mem::size_of::<WndClassExW>() as u32,
                style: 0,
                wndproc: Some(wndproc),
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
                icon_sm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                return;
            }
            let title = wide("BananaSplit mixer");
            // A top-level window (never shown), not a message-only one: only
            // top-level windows are told that the session ends.
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                return;
            }
            let mut msg: Msg = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_match_the_sdk() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<WndClassExW>(), 80);
            assert_eq!(std::mem::size_of::<Msg>(), 48);
        }
    }
}
