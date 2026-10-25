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
const RRF_RT_ANY: u32 = 0xffff;
const ERROR_FILE_NOT_FOUND: i32 = 2;
/// For the console programs started here (PowerShell): no console window,
/// also when BananaSplit runs in the background without one.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SW_SHOWNORMAL: i32 = 1;
const MB_OK: u32 = 0x00000000;
const MB_ICONERROR: u32 = 0x00000010;

fn exe_path() -> PathBuf {
    let mut buf = vec![0u16; 1024];
    unsafe {
        let n = GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32);
        if n == 0 {
            return std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bananasplit.exe"));
        }
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(&buf[..n as usize]))
    }
}

pub fn helper_exe() -> Result<PathBuf, String> {
    Ok(exe_path())
}

pub fn open_folder(path: &Path) -> Result<(), String> {
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
    if enable { Ok("BananaSplit will start with Windows (hidden in the background).".into()) } else { Ok("BananaSplit will not start with Windows.".into()) }
}

pub fn autostart_enabled() -> bool {
    let (sub, name) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut len = 0u32;
    // SAFETY: NUL-terminated UTF-16 strings; no data buffer, only its size is asked for.
    unsafe { RegGetValueW(hkcu(), sub.as_ptr(), name.as_ptr(), RRF_RT_ANY, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) == 0 }
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

pub fn error_dialog(text: &str) {
    let cap = wide("BananaSplit");
    let t = to_wide(text);
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), cap.as_ptr(), MB_OK | MB_ICONERROR); }
}
