//! Windows desktop integration.

use crate::win::ffi::{to_wide, wide};
use std::ffi::OsStr;
// use std::os::windows::ffi::OsStrExt;
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

fn reg_set_run(enable: bool) -> Result<(), String> {
    let name = wide("BananaSplit");
    let exe = exe_path();
    let cmd = format!("\"{}\" --gui --no-open", exe.display());
    let cmd_w = to_wide(&cmd);
    let hkcu = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    if enable {
        let status = Command::new("reg").args(["add", hkcu, "/v", "BananaSplit", "/t", "REG_SZ", "/d", &cmd, "/f"]).status();
        match status { Ok(s) if s.success() => Ok(()), _ => Err("failed to add to Windows autostart".into()) }
    } else {
        let _ = Command::new("reg").args(["delete", hkcu, "/v", "BananaSplit", "/f"]).status();
        Ok(())
    }
}

pub fn set_autostart(enable: bool) -> Result<String, String> {
    reg_set_run(enable)?;
    if enable { Ok("BananaSplit will start with Windows (hidden in the background).".into()) } else { Ok("BananaSplit will not start with Windows.".into()) }
}

pub fn autostart_enabled() -> bool {
    let out = Command::new("reg").args(["query", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run", "/v", "BananaSplit"]).output();
    matches!(out, Ok(o) if o.status.success())
}

pub fn install() -> Result<String, String> {
    let exe = exe_path();
    let start_menu = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\BananaSplit.lnk"));
    if let Some(lnk) = start_menu {
        let ps = format!("$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.Arguments='--gui';$s.IconLocation='{}';$s.Save()",
            lnk.to_string_lossy().replace('\'', "''"),
            exe.to_string_lossy().replace('\'', "''"),
            exe.to_string_lossy().replace('\'', "''"));
        let status = Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps]).status();
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
