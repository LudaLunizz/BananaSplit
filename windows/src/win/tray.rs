//! The notification-area (tray) icon of the window process.
//!
//! Plain Win32 (`Shell_NotifyIconW`) on a hidden window of its own, created
//! on the thread that runs the window's event loop: that loop dispatches the
//! icon's messages to `wndproc` below. Left click shows or hides the
//! window; the menu has Show, Start/Stop recording and Quit, like the Linux
//! tray. The icon is added again when Explorer restarts.
//!
//! At login, Windows starts the autostart entry while Explorer is still
//! starting, and the notification area may not take an icon yet. The icon's
//! window then stays, and the icon is added when Explorer says the taskbar
//! is there ("TaskbarCreated"), or by a retry every two seconds.

use std::cell::RefCell;
use std::ffi::c_void;

type Hwnd = *mut c_void;
type Handle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}

#[repr(C)]
struct NotifyIconData {
    cb_size: u32,
    hwnd: Hwnd,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: Handle,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: Guid,
    balloon_icon: Handle,
}

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
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> i32;
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
    fn DestroyWindow(hwnd: Hwnd) -> i32;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn CreatePopupMenu() -> Handle;
    fn AppendMenuW(menu: Handle, flags: u32, id: usize, text: *const u16) -> i32;
    fn SetMenuDefaultItem(menu: Handle, item: u32, by_position: u32) -> i32;
    fn TrackPopupMenu(menu: Handle, flags: u32, x: i32, y: i32, reserved: i32, hwnd: Hwnd, rect: *const c_void) -> i32;
    fn DestroyMenu(menu: Handle) -> i32;
    fn GetCursorPos(p: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: Hwnd) -> i32;
    fn PostMessageW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> i32;
    fn CreateIcon(instance: Handle, w: i32, h: i32, planes: u8, bits: u8, and: *const u8, xor: *const u8) -> Handle;
    fn DestroyIcon(icon: Handle) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetTimer(hwnd: Hwnd, id: usize, ms: u32, func: *const c_void) -> usize;
    fn KillTimer(hwnd: Hwnd, id: usize) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Handle;
}

const WM_APP: u32 = 0x8000;
const WM_TRAY: u32 = WM_APP + 1;
const WM_NULL: u32 = 0;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_CONTEXTMENU: u32 = 0x007B;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
/// The timer that retries adding the icon, and how often.
const RETRY_TIMER: usize = 1;
const RETRY_MS: u32 = 2000;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 1;
const NIF_ICON: u32 = 2;
const NIF_TIP: u32 = 4;
const MF_STRING: u32 = 0;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTBUTTON: u32 = 0x2;
const TPM_RETURNCMD: u32 = 0x100;
const SM_CXSMICON: i32 = 49;

const MENU_SHOW: usize = 1;
const MENU_RECORD: usize = 2;
const MENU_QUIT: usize = 3;

/// What the user did with the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Left click: show the window, or hide it if it is in front.
    Toggle,
    Show,
    Record,
    Quit,
}

struct State {
    hwnd: Hwnd,
    taskbar_created: u32,
    /// The icon is in the notification area (else it is being retried).
    shown: bool,
    recording: bool,
    icons: [Handle; 2],
    on_action: Box<dyn Fn(TrayAction)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

const IDLE: [(u32, &[u8]); 4] = [
    (22, include_bytes!("../../assets/tray/idle-22.argb")),
    (32, include_bytes!("../../assets/tray/idle-32.argb")),
    (48, include_bytes!("../../assets/tray/idle-48.argb")),
    (64, include_bytes!("../../assets/tray/idle-64.argb")),
];
const REC: [(u32, &[u8]); 4] = [
    (22, include_bytes!("../../assets/tray/rec-22.argb")),
    (32, include_bytes!("../../assets/tray/rec-32.argb")),
    (48, include_bytes!("../../assets/tray/rec-48.argb")),
    (64, include_bytes!("../../assets/tray/rec-64.argb")),
];

/// Scale one of the ARGB images (bytes A, R, G, B per pixel, straight
/// alpha) to `size` x `size` and return it as BGRA for `CreateIcon`.
/// Picks the smallest image at least as large and averages 4x4 samples per
/// pixel with premultiplied colour, so edges stay clean.
pub fn icon_bgra(set: &[(u32, &[u8])], size: u32) -> Vec<u8> {
    let (src_size, src) = set.iter().copied().find(|(s, _)| *s >= size).unwrap_or(set[set.len() - 1]);
    let px = |x: u32, y: u32| -> [f32; 4] {
        let i = ((y.min(src_size - 1) * src_size + x.min(src_size - 1)) * 4) as usize;
        let a = f32::from(src[i]) / 255.0;
        [a, f32::from(src[i + 1]) * a, f32::from(src[i + 2]) * a, f32::from(src[i + 3]) * a]
    };
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let scale = src_size as f32 / size as f32;
    const N: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..N {
                for sx in 0..N {
                    let fx = (x as f32 + (sx as f32 + 0.5) / N as f32) * scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / N as f32) * scale;
                    let p = px(fx as u32, fy as u32);
                    for k in 0..4 {
                        acc[k] += p[k];
                    }
                }
            }
            let n = (N * N) as f32;
            let a = acc[0] / n;
            let un = |c: f32| if a > 0.0 { (c / n / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            // BGRA
            out.extend_from_slice(&[un(acc[3]), un(acc[2]), un(acc[1]), (a * 255.0).round() as u8]);
        }
    }
    out
}

fn make_icon(set: &[(u32, &[u8])]) -> Handle {
    let size = match unsafe { GetSystemMetrics(SM_CXSMICON) } {
        s if (8..=256).contains(&s) => s as u32,
        _ => 16,
    };
    let bgra = icon_bgra(set, size);
    let and_mask = vec![0u8; (size.div_ceil(32) * 4 * size) as usize];
    unsafe {
        CreateIcon(GetModuleHandleW(std::ptr::null()), size as i32, size as i32, 1, 32, and_mask.as_ptr(), bgra.as_ptr())
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// Add the icon; if the notification area does not take it (yet), keep
/// retrying on a timer.
fn try_add(st: &mut State) {
    st.shown = notify(st, NIM_ADD);
    // SAFETY: our own window; the timer is (re)set or killed on its thread.
    unsafe {
        if st.shown {
            KillTimer(st.hwnd, RETRY_TIMER);
        } else {
            SetTimer(st.hwnd, RETRY_TIMER, RETRY_MS, std::ptr::null());
        }
    }
}

fn notify(st: &State, message: u32) -> bool {
    let mut d: NotifyIconData = unsafe { std::mem::zeroed() };
    d.cb_size = std::mem::size_of::<NotifyIconData>() as u32;
    d.hwnd = st.hwnd;
    d.id = 1;
    d.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.callback_message = WM_TRAY;
    d.icon = st.icons[usize::from(st.recording)];
    copy_wide(&mut d.tip, if st.recording { "BananaSplit (recording)" } else { "BananaSplit" });
    unsafe { Shell_NotifyIconW(message, &mut d) != 0 }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn menu(hwnd: Hwnd, recording: bool) -> Option<TrayAction> {
    unsafe {
        let m = CreatePopupMenu();
        if m.is_null() {
            return None;
        }
        let show = wide("Show BananaSplit");
        let rec = wide(if recording { "Stop recording" } else { "Start recording" });
        let quit = wide("Quit BananaSplit");
        AppendMenuW(m, MF_STRING, MENU_SHOW, show.as_ptr());
        AppendMenuW(m, MF_STRING, MENU_RECORD, rec.as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(m, MF_STRING, MENU_QUIT, quit.as_ptr());
        SetMenuDefaultItem(m, MENU_SHOW as u32, 0);
        let mut p = Point::default();
        GetCursorPos(&mut p);
        // Without this the menu does not close when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_RETURNCMD, p.x, p.y, 0, hwnd, std::ptr::null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
        item(cmd as usize)
    }
}

/// A menu item's action (also for a `WM_COMMAND` sent to the icon's window,
/// which is how the launch test picks one).
fn item(id: usize) -> Option<TrayAction> {
    match id {
        MENU_SHOW => Some(TrayAction::Show),
        MENU_RECORD => Some(TrayAction::Record),
        MENU_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    if msg == WM_TRAY || msg == WM_COMMAND {
        let action = if msg == WM_COMMAND {
            item(w & 0xFFFF)
        } else {
            match (l as usize & 0xFFFF) as u32 {
                WM_LBUTTONUP => Some(TrayAction::Toggle),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let recording = STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.recording));
                    menu(hwnd, recording)
                }
                _ => None,
            }
        };
        if let Some(a) = action {
            // The handler may send events but never re-enters this module.
            STATE.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    (st.on_action)(a);
                }
            });
        }
        return 0;
    }
    let readd = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.taskbar_created != 0 && msg == st.taskbar_created));
    if readd || (msg == WM_TIMER && w == RETRY_TIMER) {
        // The taskbar is (back) there: Explorer started late, or restarted
        // and lost the icon. Or the retry timer: try again.
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                try_add(st);
            }
        });
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// The icon, while it lives. Dropping it removes it.
pub struct Tray(());

impl Tray {
    /// Add the icon. Must be called on the thread that runs the event loop.
    /// If the notification area is not there yet, the icon comes as soon as
    /// it is (`shown` says whether it has).
    pub fn add(on_action: impl Fn(TrayAction) + 'static) -> Result<Tray, String> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("BananaSplitTray");
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
                return Err("cannot register the tray window class".into());
            }
            let title = wide("BananaSplit tray");
            // A real (never shown) top-level window, not a message-only one:
            // only top-level windows hear "TaskbarCreated".
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
                return Err("cannot create the tray window".into());
            }
            let mut st = State {
                hwnd,
                taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                shown: false,
                recording: false,
                icons: [make_icon(&IDLE), make_icon(&REC)],
                on_action: Box::new(on_action),
            };
            try_add(&mut st);
            STATE.with(|s| *s.borrow_mut() = Some(st));
            Ok(Tray(()))
        }
    }

    /// Show the recording icon, tooltip and menu entry.
    pub fn set_recording(&self, on: bool) {
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut()
                && st.recording != on {
                    st.recording = on;
                    // Not shown yet: it is added with the right icon later.
                    if st.shown {
                        notify(st, NIM_MODIFY);
                    }
                }
        });
    }

    /// Is the icon in the notification area now?
    pub fn shown(&self) -> bool {
        STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.shown))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(st) = STATE.with(|s| s.borrow_mut().take()) {
            if st.shown {
                notify(&st, NIM_DELETE);
            }
            unsafe {
                KillTimer(st.hwnd, RETRY_TIMER);
                for i in st.icons {
                    if !i.is_null() {
                        DestroyIcon(i);
                    }
                }
                DestroyWindow(st.hwnd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_icon_data_matches_the_sdk() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<NotifyIconData>(), 976);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<WndClassExW>(), 80);
    }

    #[test]
    fn icons_scale_to_any_size() {
        for size in [16, 20, 24, 32, 40, 64, 80] {
            let b = icon_bgra(&IDLE, size);
            assert_eq!(b.len(), (size * size * 4) as usize);
            // the corners of the badge are transparent, the middle is not
            assert_eq!(b[3], 0, "{size}: top-left transparent");
            let mid = ((size / 2 * size + size / 2) * 4 + 3) as usize;
            assert!(b[mid] > 200, "{size}: middle opaque ({})", b[mid]);
        }
        // the recording icon differs from the idle one
        assert_ne!(icon_bgra(&IDLE, 32), icon_bgra(&REC, 32));
    }
}
