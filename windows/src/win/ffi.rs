//! Hand-written Windows API declarations (no crates), following the
//! Windows SDK headers: mmdeviceapi.h, audioclient.h, propsys.h,
//! consoleapi.h, wincon.h, winuser.h, shellapi.h, avrt.h.
//!
//! COM interfaces are plain vtable structs. Every vtable starts with the
//! three IUnknown slots, and methods are listed in header order, because the
//! slot position is the ABI.

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    clippy::upper_case_acronyms,
    dead_code
)]

use std::ffi::c_void;

pub type HRESULT = i32;
pub type HANDLE = *mut c_void;
pub type HWND = *mut c_void;
pub type BOOL = i32;
pub type REFERENCE_TIME = i64; // 100 ns units

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> GUID {
    GUID {
        data1: d1,
        data2: d2,
        data3: d3,
        data4: d4,
    }
}

pub const CLSID_MMDeviceEnumerator: GUID = guid(
    0xBCDE0395,
    0xE52F,
    0x467C,
    [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E],
);
pub const IID_IMMDeviceEnumerator: GUID = guid(
    0xA95664D2,
    0x9614,
    0x4F35,
    [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6],
);
pub const IID_IAudioClient: GUID = guid(
    0x1CB9AD4C,
    0xDBFA,
    0x4C32,
    [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2],
);
pub const IID_IAudioRenderClient: GUID = guid(
    0xF294ACFC,
    0x3146,
    0x4483,
    [0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2],
);
pub const IID_IAudioCaptureClient: GUID = guid(
    0xC8ADBD64,
    0xE71E,
    0x48A0,
    [0xA4, 0xDE, 0x18, 0x5C, 0x39, 0x5C, 0xD3, 0x17],
);

#[repr(C)]
pub struct PROPERTYKEY {
    pub fmtid: GUID,
    pub pid: u32,
}
pub const PKEY_Device_FriendlyName: PROPERTYKEY = PROPERTYKEY {
    fmtid: guid(
        0xA45C254E,
        0xDF1C,
        0x4EFD,
        [0x80, 0x20, 0x67, 0xD1, 0x46, 0xA8, 0x50, 0xE0],
    ),
    pid: 14,
};

/// PROPVARIANT: 8-byte header + a union as large as two pointers (DECIMAL
/// overlays the whole struct), i.e. 24 bytes on 64-bit, 16 on 32-bit.
#[repr(C)]
pub struct PROPVARIANT {
    pub vt: u16,
    pub r1: u16,
    pub r2: u16,
    pub r3: u16,
    pub data: [usize; 2],
}
pub const VT_LPWSTR: u16 = 31;

/// WAVEFORMATEX is byte-packed in mmreg.h (18 bytes).
#[repr(C, packed(1))]
#[derive(Clone, Copy)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    pub cbSize: u16,
}
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

// EDataFlow / ERole / device state
pub const eRender: u32 = 0;
pub const eCapture: u32 = 1;
pub const eConsole: u32 = 0;
pub const DEVICE_STATE_ACTIVE: u32 = 1;
pub const STGM_READ: u32 = 0;
pub const CLSCTX_ALL: u32 = 0x1 | 0x2 | 0x4 | 0x10;
pub const COINIT_MULTITHREADED: u32 = 0;
pub const COINIT_APARTMENTTHREADED: u32 = 0x2;

// IAudioClient
pub const AUDCLNT_SHAREMODE_SHARED: u32 = 0;
pub const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
pub const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x8000_0000;
pub const AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x0800_0000;
pub const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;
pub const AUDCLNT_E_DEVICE_INVALIDATED: HRESULT = 0x8889_0004_u32 as i32;
pub const AUDCLNT_E_SERVICE_NOT_RUNNING: HRESULT = 0x8889_0010_u32 as i32;
pub const AUDCLNT_E_RESOURCES_INVALIDATED: HRESULT = 0x8889_0026_u32 as i32;
pub const RPC_E_CHANGED_MODE: HRESULT = 0x8001_0106_u32 as i32;

// Waits
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 0x102;

// Console
pub const STD_INPUT_HANDLE: u32 = -10i32 as u32;
pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;
pub const FILE_TYPE_PIPE: u32 = 3;
pub const FILE_TYPE_UNKNOWN: u32 = 0;
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
/// `AttachConsole`: the console of the process that started this one.
pub const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const FILE_SHARE_READ: u32 = 1;
pub const FILE_SHARE_WRITE: u32 = 2;
pub const OPEN_EXISTING: u32 = 3;
pub const ENABLE_PROCESSED_INPUT: u32 = 0x1;
pub const ENABLE_LINE_INPUT: u32 = 0x2;
pub const ENABLE_ECHO_INPUT: u32 = 0x4;
pub const ENABLE_MOUSE_INPUT: u32 = 0x10;
pub const ENABLE_QUICK_EDIT_MODE: u32 = 0x40;
pub const ENABLE_EXTENDED_FLAGS: u32 = 0x80;
pub const ENABLE_PROCESSED_OUTPUT: u32 = 0x1;
pub const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x4;
pub const CP_UTF8: u32 = 65001;
pub const KEY_EVENT: u16 = 0x1;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const CTRL_C_EVENT: u32 = 0;
pub const CTRL_BREAK_EVENT: u32 = 1;
pub const CTRL_CLOSE_EVENT: u32 = 2;
pub const CTRL_LOGOFF_EVENT: u32 = 5;
pub const CTRL_SHUTDOWN_EVENT: u32 = 6;

/// INPUT_RECORD with the KEY_EVENT_RECORD arm of the union spelled out
/// (EventType, 2 bytes padding, 16-byte union) = 20 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct INPUT_RECORD {
    pub EventType: u16,
    pub _pad: u16,
    pub bKeyDown: BOOL,
    pub wRepeatCount: u16,
    pub wVirtualKeyCode: u16,
    pub wVirtualScanCode: u16,
    pub UnicodeChar: u16,
    pub dwControlKeyState: u32,
}

// MessageBox
pub const MB_OK: u32 = 0x0;
pub const MB_YESNO: u32 = 0x4;
pub const MB_YESNOCANCEL: u32 = 0x3;
pub const MB_ICONWARNING: u32 = 0x30;
pub const MB_ICONQUESTION: u32 = 0x20;
pub const MB_ICONINFORMATION: u32 = 0x40;
pub const MB_SETFOREGROUND: u32 = 0x1_0000;
pub const MB_TOPMOST: u32 = 0x4_0000;
pub const IDYES: i32 = 6;
pub const IDNO: i32 = 7;
pub const IDCANCEL: i32 = 2;
pub const SW_SHOWNORMAL: i32 = 1;

// ---------------------------------------------------------------- vtables

#[repr(C)]
pub struct IUnknownVtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
pub struct IMMDeviceEnumeratorVtbl {
    pub base: IUnknownVtbl,
    pub EnumAudioEndpoints:
        unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    pub GetDefaultAudioEndpoint:
        unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    pub GetDevice: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HRESULT,
    pub RegisterEndpointNotificationCallback:
        unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    pub UnregisterEndpointNotificationCallback:
        unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IMMDeviceCollectionVtbl {
    pub base: IUnknownVtbl,
    pub GetCount: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub Item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IMMDeviceVtbl {
    pub base: IUnknownVtbl,
    pub Activate: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        u32,
        *mut PROPVARIANT,
        *mut *mut c_void,
    ) -> HRESULT,
    pub OpenPropertyStore: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub GetId: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
    pub GetState: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

#[repr(C)]
pub struct IPropertyStoreVtbl {
    pub base: IUnknownVtbl,
    pub GetCount: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub GetAt: unsafe extern "system" fn(*mut c_void, u32, *mut PROPERTYKEY) -> HRESULT,
    pub GetValue:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *mut PROPVARIANT) -> HRESULT,
    pub SetValue:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *const PROPVARIANT) -> HRESULT,
    pub Commit: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IAudioClientVtbl {
    pub base: IUnknownVtbl,
    pub Initialize: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u32,
        REFERENCE_TIME,
        REFERENCE_TIME,
        *const WAVEFORMATEX,
        *const GUID,
    ) -> HRESULT,
    pub GetBufferSize: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub GetStreamLatency: unsafe extern "system" fn(*mut c_void, *mut REFERENCE_TIME) -> HRESULT,
    pub GetCurrentPadding: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub IsFormatSupported: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *const WAVEFORMATEX,
        *mut *mut WAVEFORMATEX,
    ) -> HRESULT,
    pub GetMixFormat: unsafe extern "system" fn(*mut c_void, *mut *mut WAVEFORMATEX) -> HRESULT,
    pub GetDevicePeriod:
        unsafe extern "system" fn(*mut c_void, *mut REFERENCE_TIME, *mut REFERENCE_TIME) -> HRESULT,
    pub Start: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub Stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub Reset: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub SetEventHandle: unsafe extern "system" fn(*mut c_void, HANDLE) -> HRESULT,
    pub GetService:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
pub struct IAudioRenderClientVtbl {
    pub base: IUnknownVtbl,
    pub GetBuffer: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u8) -> HRESULT,
    pub ReleaseBuffer: unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT,
}

#[repr(C)]
pub struct IAudioCaptureClientVtbl {
    pub base: IUnknownVtbl,
    pub GetBuffer: unsafe extern "system" fn(
        *mut c_void,
        *mut *mut u8,
        *mut u32,
        *mut u32,
        *mut u64,
        *mut u64,
    ) -> HRESULT,
    pub ReleaseBuffer: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    pub GetNextPacketSize: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
}

// ---------------------------------------------------------------- functions

#[cfg_attr(windows, link(name = "ole32"))]
unsafe extern "system" {
    pub fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> HRESULT;
    pub fn CoUninitialize();
    pub fn CoCreateInstance(
        clsid: *const GUID,
        outer: *mut c_void,
        clsctx: u32,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> HRESULT;
    pub fn CoTaskMemFree(p: *mut c_void);
    pub fn PropVariantClear(p: *mut PROPVARIANT) -> HRESULT;
}

#[cfg_attr(windows, link(name = "kernel32"))]
unsafe extern "system" {
    pub fn CreateEventW(
        attrs: *mut c_void,
        manual_reset: BOOL,
        initial: BOOL,
        name: *const u16,
    ) -> HANDLE;
    pub fn WaitForSingleObject(h: HANDLE, ms: u32) -> u32;
    pub fn CloseHandle(h: HANDLE) -> BOOL;
    pub fn GetStdHandle(which: u32) -> HANDLE;
    pub fn GetConsoleMode(h: HANDLE, mode: *mut u32) -> BOOL;
    pub fn SetConsoleMode(h: HANDLE, mode: u32) -> BOOL;
    pub fn GetConsoleOutputCP() -> u32;
    pub fn SetConsoleOutputCP(cp: u32) -> BOOL;
    pub fn SetConsoleTitleW(title: *const u16) -> BOOL;
    pub fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> BOOL>,
        add: BOOL,
    ) -> BOOL;
    pub fn ReadConsoleInputW(h: HANDLE, buf: *mut INPUT_RECORD, len: u32, read: *mut u32) -> BOOL;
    pub fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    pub fn FreeConsole() -> BOOL;
    pub fn SetStdHandle(which: u32, h: HANDLE) -> BOOL;
    pub fn GetFileType(h: HANDLE) -> u32;
    pub fn AttachConsole(pid: u32) -> BOOL;
    pub fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
}

/// `AllowSetForegroundWindow`: any process.
pub const ASFW_ANY: u32 = u32::MAX;

#[cfg_attr(windows, link(name = "user32"))]
unsafe extern "system" {
    pub fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, kind: u32) -> i32;
    pub fn AllowSetForegroundWindow(pid: u32) -> BOOL;
    pub fn SetForegroundWindow(hwnd: HWND) -> BOOL;
}

#[cfg_attr(windows, link(name = "shell32"))]
unsafe extern "system" {
    pub fn ShellExecuteW(
        hwnd: HWND,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> *mut c_void;
}

#[cfg_attr(windows, link(name = "avrt"))]
unsafe extern "system" {
    pub fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> HANDLE;
    pub fn AvRevertMmThreadCharacteristics(h: HANDLE) -> BOOL;
}

// ---------------------------------------------------------------- helpers

/// NUL-terminated UTF-16 for Win32 "W" functions.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// NUL-terminated UTF-16 path for Win32 "W" functions.
pub fn to_wide<P: AsRef<std::ffi::OsStr>>(p: P) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Read a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: caller guarantees NUL termination.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

pub fn hr_text(hr: HRESULT) -> String {
    let known = match hr {
        AUDCLNT_E_DEVICE_INVALIDATED => " (device removed or changed)",
        AUDCLNT_E_SERVICE_NOT_RUNNING => " (Windows Audio service not running)",
        AUDCLNT_E_RESOURCES_INVALIDATED => " (audio resources invalidated)",
        x if x == 0x8889_000A_u32 as i32 => " (device in use in exclusive mode by another app)",
        x if x == 0x8889_0008_u32 as i32 => " (format not supported)",
        x if x == 0x8007_0005_u32 as i32 => " (access denied — check microphone privacy settings)",
        _ => "",
    };
    format!("0x{:08X}{known}", hr as u32)
}

/// Owned COM interface pointer; `Release`d on drop.
pub struct Com<V> {
    ptr: *mut c_void,
    _v: std::marker::PhantomData<*const V>,
}

// SAFETY: every object we use (MMDevice API, audio clients) is created and
// used from multithreaded-apartment threads, where COM objects are free-threaded.
unsafe impl<V> Send for Com<V> {}

impl<V> Com<V> {
    /// # Safety
    /// `ptr` must be a valid, owned (already AddRef'ed) interface pointer of type V, or NULL.
    pub unsafe fn from_raw(ptr: *mut c_void) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Com {
                ptr,
                _v: std::marker::PhantomData,
            })
        }
    }
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
    /// The interface's vtable.
    pub fn vt(&self) -> &V {
        // SAFETY: a COM object pointer points at its vtable pointer.
        unsafe { &**(self.ptr as *const *const V) }
    }
}

impl<V> Drop for Com<V> {
    fn drop(&mut self) {
        // SAFETY: every vtable starts with IUnknown; we own one reference.
        unsafe {
            let unk = &**(self.ptr as *const *const IUnknownVtbl);
            (unk.Release)(self.ptr);
        }
    }
}

/// Win32 event handle, closed on drop.
pub struct Event(pub HANDLE);
unsafe impl Send for Event {}
impl Event {
    pub fn new() -> Result<Event, String> {
        // auto-reset, initially non-signalled
        let h = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) };
        if h.is_null() {
            Err("CreateEventW failed".into())
        } else {
            Ok(Event(h))
        }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Per-thread COM initialisation (multithreaded apartment).
pub struct ComInit(bool);
impl ComInit {
    pub fn mta() -> ComInit {
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED) };
        // S_FALSE (already initialised) also needs a matching CoUninitialize;
        // RPC_E_CHANGED_MODE means someone else owns this thread's apartment.
        ComInit(hr >= 0)
    }
}
impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_the_sdk() {
        assert_eq!(std::mem::size_of::<WAVEFORMATEX>(), 18);
        assert_eq!(std::mem::size_of::<INPUT_RECORD>(), 20);
        assert_eq!(std::mem::size_of::<GUID>(), 16);
        assert_eq!(std::mem::size_of::<PROPERTYKEY>(), 20);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<PROPVARIANT>(), 24);
        assert_eq!(
            std::mem::size_of::<IAudioClientVtbl>(),
            15 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<IMMDeviceEnumeratorVtbl>(),
            8 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<IAudioCaptureClientVtbl>(),
            6 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn wide_round_trip() {
        let w = wide("CABLE Output (VB-Audio Virtual Cable) ü");
        assert_eq!(*w.last().unwrap(), 0);
        assert_eq!(
            unsafe { from_wide(w.as_ptr()) },
            "CABLE Output (VB-Audio Virtual Cable) ü"
        );
    }
}

