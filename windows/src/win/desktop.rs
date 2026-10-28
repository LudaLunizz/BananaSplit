//! Windows desktop integration.

use crate::win::ffi::{to_wide, wide};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(hwnd: *mut std::ffi::c_void, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> *mut std::ffi::c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleFileNameW(h: *mut std::ffi::c_void, buf: *mut u16, size: u32) -> u32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, ty: u32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(key: *mut std::ffi::c_void, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut std::ffi::c_void, len: *mut u32) -> i32;
    fn RegSetKeyValueW(key: *mut std::ffi::c_void, sub: *const u16, value: *const u16, ty: u32, data: *const std::ffi::c_void, len: u32) -> i32;
    fn RegDeleteKeyValueW(key: *mut std::ffi::c_void, sub: *const u16, value: *const u16) -> i32;
}

/// `HKEY_CURRENT_USER`: `(HKEY)(ULONG_PTR)(LONG)0x80000001`, sign-extended.
fn hkcu() -> *mut std::ffi::c_void {
    0x8000_0001u32 as i32 as isize as *mut std::ffi::c_void
}
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "BananaSplit";
const REG_SZ: u32 = 1;
const RRF_RT_REG_SZ: u32 = 0x0000_0002;
const ERROR_FILE_NOT_FOUND: i32 = 2;
/// For the console programs started here (PowerShell): no console window,
/// also when BananaSplit runs in the background without one.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SW_SHOWNORMAL: i32 = 1;
const MB_OK: u32 = 0x00000000;
const MB_YESNO: u32 = 0x00000004;
const MB_ICONERROR: u32 = 0x00000010;
const MB_ICONWARNING: u32 = 0x00000030;
const MB_SETFOREGROUND: u32 = 0x0001_0000;
const IDYES: i32 = 6;

/// Microsoft's page for the WebView2 Runtime (the window needs it; it is
/// part of Windows 11 and of most up-to-date Windows 10 installations).
pub const WEBVIEW2_PAGE: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

fn exe_path() -> PathBuf {
    // After an update in this session: the program's path (the new version).
    if let Some(p) = crate::update::original_program() {
        return p;
    }
    let mut buf = vec![0u16; 1024];
    unsafe {
        let n = GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32);
        // n == the buffer's length: the path was cut short.
        if n == 0 || n as usize >= buf.len() {
            return std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bananasplit.exe"));
        }
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(&buf[..n as usize]))
    }
}

/// The program file: after an update, the new one (at the same place).
pub fn program_path() -> Option<PathBuf> {
    Some(exe_path())
}

/// The file to start this program's helper processes (the window) from:
/// this very program, also after an update moved it aside (the window must
/// speak the same protocol as the mixer that starts it).
pub fn helper_exe() -> Result<PathBuf, String> {
    Ok(crate::update::moved_program().filter(|p| p.is_file()).unwrap_or_else(exe_path))
}

pub fn open_folder(path: &Path) -> Result<(), String> {
    // the HTTP threads that call this have no COM of their own
    let _com = crate::win::ffi::ComInit::sta();
    let p = to_wide(path);
    let open = wide("open");
    unsafe {
        let r = ShellExecuteW(std::ptr::null_mut(), open.as_ptr(), p.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
        if (r as usize) <= 32 { Err(format!("ShellExecuteW failed for {}", path.display())) } else { Ok(()) }
    }
}

/// Start with Windows: the `Run` entry, with the registry functions (rather
/// than `reg.exe`: `autostart_enabled` is asked every two seconds).
fn reg_set_run(enable: bool) -> Result<(), String> {
    let (sub, name) = (wide(RUN_KEY), wide(RUN_VALUE));
    if enable {
        let cmd = wide(&format!("\"{}\" --gui --no-open", exe_path().display()));
        // SAFETY: NUL-terminated UTF-16 strings; `len` is the data's size in bytes, NUL included.
        let r = unsafe { RegSetKeyValueW(hkcu(), sub.as_ptr(), name.as_ptr(), REG_SZ, cmd.as_ptr().cast(), (cmd.len() * 2) as u32) };
        if r == 0 { Ok(()) } else { Err(format!("failed to add BananaSplit to the Windows autostart (error {r})")) }
    } else {
        // SAFETY: NUL-terminated UTF-16 strings.
        let r = unsafe { RegDeleteKeyValueW(hkcu(), sub.as_ptr(), name.as_ptr()) };
        if r == 0 || r == ERROR_FILE_NOT_FOUND { Ok(()) } else { Err(format!("failed to remove BananaSplit from the Windows autostart (error {r})")) }
    }
}

pub fn set_autostart(enable: bool) -> Result<String, String> {
    reg_set_run(enable)?;
    if enable { Ok("BananaSplit will start with Windows, in the tray.".into()) } else { Ok("BananaSplit will not start with Windows.".into()) }
}

/// The `Run` entry's command line, if there is one.
fn run_value() -> Option<String> {
    let (sub, name) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut len = 0u32;
    // SAFETY: NUL-terminated UTF-16 strings; first only the size is asked
    // for, then the data is read into a buffer of that size.
    unsafe {
        if RegGetValueW(hkcu(), sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) != 0 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2).max(1)];
        let mut size = (buf.len() * 2) as u32;
        if RegGetValueW(hkcu(), sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) != 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

/// Does this `Run` command line start `exe`? (The program it names, quoted
/// or not, compared the way Windows compares paths: without regard to case
/// or to `/` versus `\`.)
pub fn run_targets(command: &str, exe: &Path) -> bool {
    let c = command.trim_start();
    let program = match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        // Unquoted: up to ".exe" (a path with spaces is not quoted by everyone).
        None => match c.to_ascii_lowercase().find(".exe") {
            Some(i) => &c[..i + 4],
            None => c.split_whitespace().next().unwrap_or(""),
        },
    };
    let norm = |p: &str| p.trim().replace('/', "\\").to_lowercase();
    !program.is_empty() && norm(program) == norm(&exe.to_string_lossy())
}

/// "Start with Windows" is on: the `Run` entry starts this very program.
/// One left behind by a copy elsewhere (an older version, before it was
/// moved) does not count, so ticking the box points it here.
pub fn autostart_enabled() -> bool {
    run_value().is_some_and(|v| run_targets(&v, &exe_path()))
}

pub fn install() -> Result<String, String> {
    let exe = exe_path();
    let start_menu = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\BananaSplit.lnk"));
    if let Some(lnk) = start_menu {
        let ps = format!("$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.Arguments='--gui';$s.IconLocation='{}';$s.Save()",
            lnk.to_string_lossy().replace('\'', "''"),
            exe.to_string_lossy().replace('\'', "''"),
            exe.to_string_lossy().replace('\'', "''"));
        let status = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if matches!(status, Ok(s) if s.success()) && lnk.exists() {
            Ok("Created a Start Menu shortcut for BananaSplit.".into())
        } else {
            Err("Could not create the Start Menu shortcut.".into())
        }
    } else {
        Err("Could not find the Start Menu folder.".into())
    }
}

pub fn menu_installed() -> bool {
    std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\BananaSplit.lnk").exists())
        .unwrap_or(false)
}

/// Open a web page (or the mixer's own page) in the default browser.
pub fn open_url(url: &str) -> bool {
    // the HTTP threads that call this have no COM of their own
    let _com = crate::win::ffi::ComInit::sta();
    let (open, u) = (wide("open"), wide(url));
    // SAFETY: NUL-terminated UTF-16 strings.
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), open.as_ptr(), u.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    r as usize > 32
}

/// Open a web page (a release's page) in the default browser.
pub fn open_page(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1:") {
        return Err("not a web page".into());
    }
    if open_url(url) { Ok(()) } else { Err("could not open the web browser".into()) }
}

/// Windows' sound settings (Settings › System › Sound): to check, or set
/// back, the default devices.
pub fn open_sound_settings() -> Result<(), String> {
    if open_url("ms-settings:sound") { Ok(()) } else { Err("could not open the sound settings".into()) }
}

/// The desktop window could not start (usually: no WebView2 Runtime). The
/// mixer runs anyway, so its page opens in the browser, and the user can
/// get WebView2 from Microsoft.
pub fn no_window(why: &str, mixer_url: &str) {
    let text = format!(
        "BananaSplit's window could not open:\n{why}\n\nIt needs the Microsoft Edge WebView2 Runtime. \
         The mixer is running, and opens in your web browser for now.\n\n\
         Open Microsoft's download page for WebView2?"
    );
    let (cap, t) = (wide("BananaSplit"), wide(&text));
    // SAFETY: NUL-terminated UTF-16 strings.
    let answer = unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), cap.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND) };
    if answer == IDYES {
        open_url(WEBVIEW2_PAGE);
    }
    open_url(mixer_url);
}

pub fn error_dialog(text: &str) {
    let cap = wide("BananaSplit");
    let t = to_wide(text);
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), cap.as_ptr(), MB_OK | MB_ICONERROR | MB_SETFOREGROUND); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_entry_must_start_this_program() {
        let exe = Path::new(r"C:\Users\me\Apps\BananaSplit 2.7.1\bananasplit.exe");
        assert!(run_targets(r#""C:\Users\me\Apps\BananaSplit 2.7.1\bananasplit.exe" --gui --no-open"#, exe));
        assert!(run_targets(r#""c:\users\ME\apps\bananasplit 2.7.1\BANANASPLIT.EXE" --gui"#, exe), "case");
        assert!(run_targets(r"C:/Users/me/Apps/BananaSplit 2.7.1/bananasplit.exe --gui --no-open", exe), "unquoted, slashes");
        // an older copy, or the same one before it was moved
        assert!(!run_targets(r#""C:\Users\me\Downloads\bananasplit-2.7.0\bananasplit.exe" --gui --no-open"#, exe));
        assert!(!run_targets(r#""C:\Users\me\Apps\BananaSplit 2.7.1\bananasplit.exe.bak""#, exe));
        assert!(!run_targets("", exe));
    }
}
