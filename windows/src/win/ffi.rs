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
pub const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

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

// ---------------------------------------------------------------- window, tray and network structs

/// `NOTIFYICONDATAW` (Shell_NotifyIconW).
#[repr(C)]
pub struct NotifyIconData {
    pub cb_size: u32,
    pub hwnd: HWND,
    pub id: u32,
    pub flags: u32,
    pub callback_message: u32,
    pub icon: HANDLE,
    pub tip: [u16; 128],
    pub state: u32,
    pub state_mask: u32,
    pub info: [u16; 256],
    pub version: u32,
    pub info_title: [u16; 64],
    pub info_flags: u32,
    pub guid: GUID,
    pub balloon_icon: HANDLE,
}

/// A window procedure.
pub type WndProc = unsafe extern "system" fn(HWND, u32, usize, isize) -> isize;

/// `WNDCLASSEXW`.
#[repr(C)]
pub struct WndClassExW {
    pub cb_size: u32,
    pub style: u32,
    pub wndproc: Option<WndProc>,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub instance: HANDLE,
    pub icon: HANDLE,
    pub cursor: HANDLE,
    pub background: HANDLE,
    pub menu_name: *const u16,
    pub class_name: *const u16,
    pub icon_sm: HANDLE,
}

/// `MSG`.
#[repr(C)]
pub struct Msg {
    pub hwnd: HWND,
    pub message: u32,
    pub w: usize,
    pub l: isize,
    pub time: u32,
    pub pt: [i32; 2],
    pub private: u32,
}

/// `POINT`.
#[repr(C)]
#[derive(Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// `SYSTEMTIME`.
#[repr(C)]
#[derive(Default)]
pub struct SystemTime {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub milliseconds: u16,
}

/// `SOCKADDR` (address family and its data).
#[repr(C)]
pub struct RawSockaddr {
    pub family: u16,
    pub data: [u8; 14],
}

/// `SOCKET_ADDRESS`.
#[repr(C)]
pub struct SocketAddress {
    pub sock: *const RawSockaddr,
    pub len: i32,
}

/// `IP_ADAPTER_UNICAST_ADDRESS_LH`.
#[repr(C)]
pub struct IpAdapterUnicastAddress {
    pub len: u32,
    pub flags: u32,
    pub next: *mut IpAdapterUnicastAddress,
    pub addr: SocketAddress,
    // IP_PREFIX_ORIGIN, IP_SUFFIX_ORIGIN and IP_DAD_STATE are C enums: 4 bytes each.
    pub prefix: u32,
    pub suffix: u32,
    pub dad: u32,
    pub valid_lifetime: u32,
    pub preferred_lifetime: u32,
    pub lease_lifetime: u32,
    pub on_link_prefix_length: u8,
}

/// `IP_ADAPTER_ADDRESSES_LH`, up to the fields BananaSplit reads.
#[repr(C)]
pub struct IpAdapterAddresses {
    pub len: u32,
    pub if_index: u32,
    pub next: *mut IpAdapterAddresses,
    pub adapter_name: *const i8,
    pub first_unicast: *mut IpAdapterUnicastAddress,
    pub first_anycast: *mut c_void,
    pub first_multicast: *mut c_void,
    pub first_dns: *mut c_void,
    pub dns_suffix: *const u16,
    pub description: *const u16,
    pub friendly_name: *const u16,
    pub phys_addr: [u8; 8],
    pub phys_len: u32,
    pub flags: u32,
    pub mtu: u32,
    pub if_type: u32,
    pub oper_status: u32,
    pub ipv6_if_index: u32,
    pub zone: [u32; 16],
    pub first_prefix: *mut c_void,
    pub transmit_link_speed: u64,
    pub receive_link_speed: u64,
    pub first_wins: *mut c_void,
}

/// `SOCKADDR_IN`.
#[repr(C)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
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
    pub fn GetModuleHandleW(name: *const u16) -> HANDLE;
    pub fn GetModuleFileNameW(h: HANDLE, buf: *mut u16, size: u32) -> u32;
    pub fn GetDiskFreeSpaceExW(
        dir: *const u16,
        free_bytes_avail: *mut u64,
        total_bytes: *mut u64,
        total_free: *mut u64,
    ) -> BOOL;
    pub fn GetCurrentProcessId() -> u32;
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn OpenProcess(access: u32, inherit: BOOL, pid: u32) -> HANDLE;
    pub fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> BOOL;
    pub fn GetLocalTime(out: *mut SystemTime);
}

/// `AllowSetForegroundWindow`: any process.
pub const ASFW_ANY: u32 = u32::MAX;

#[cfg_attr(windows, link(name = "user32"))]
unsafe extern "system" {
    pub fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, kind: u32) -> i32;
    pub fn AllowSetForegroundWindow(pid: u32) -> BOOL;
    pub fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    pub fn RegisterClassExW(class: *const WndClassExW) -> u16;
    pub fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HANDLE,
        instance: HANDLE,
        param: *mut c_void,
    ) -> HWND;
    pub fn DestroyWindow(hwnd: HWND) -> BOOL;
    pub fn DefWindowProcW(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize;
    pub fn RegisterWindowMessageW(name: *const u16) -> u32;
    pub fn GetMessageW(msg: *mut Msg, hwnd: HWND, min: u32, max: u32) -> BOOL;
    pub fn TranslateMessage(msg: *const Msg) -> BOOL;
    pub fn DispatchMessageW(msg: *const Msg) -> isize;
    pub fn PostMessageW(hwnd: HWND, msg: u32, w: usize, l: isize) -> BOOL;
    pub fn CreatePopupMenu() -> HANDLE;
    pub fn AppendMenuW(menu: HANDLE, flags: u32, id: usize, text: *const u16) -> BOOL;
    pub fn SetMenuDefaultItem(menu: HANDLE, item: u32, by_position: u32) -> BOOL;
    pub fn TrackPopupMenu(menu: HANDLE, flags: u32, x: i32, y: i32, reserved: i32, hwnd: HWND, rect: *const c_void) -> BOOL;
    pub fn DestroyMenu(menu: HANDLE) -> BOOL;
    pub fn GetCursorPos(p: *mut Point) -> BOOL;
    pub fn CreateIcon(instance: HANDLE, w: i32, h: i32, planes: u8, bits: u8, and: *const u8, xor: *const u8) -> HANDLE;
    pub fn DestroyIcon(icon: HANDLE) -> BOOL;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SetTimer(hwnd: HWND, id: usize, ms: u32, func: *const c_void) -> usize;
    pub fn KillTimer(hwnd: HWND, id: usize) -> BOOL;
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
    pub fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> BOOL;
}

#[cfg_attr(windows, link(name = "iphlpapi"))]
unsafe extern "system" {
    pub fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut c_void,
        adapter_addresses: *mut IpAdapterAddresses,
        size: *mut u32,
    ) -> u32;
    pub fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: BOOL, af: u32, class: u32, reserved: u32) -> u32;
}

#[cfg_attr(windows, link(name = "advapi32"))]
unsafe extern "system" {
    pub fn RegGetValueW(key: HANDLE, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
    pub fn RegSetKeyValueW(key: HANDLE, sub: *const u16, value: *const u16, ty: u32, data: *const c_void, len: u32) -> i32;
    pub fn RegDeleteKeyValueW(key: HANDLE, sub: *const u16, value: *const u16) -> i32;
    pub fn OpenProcessToken(process: HANDLE, access: u32, token: *mut HANDLE) -> BOOL;
    pub fn GetTokenInformation(token: HANDLE, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> BOOL;
    pub fn EqualSid(a: *mut c_void, b: *mut c_void) -> BOOL;
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
    // SAFETY: `p` is non-NULL and `p[n]` is the NUL found above, so `p[..n]` are readable u16s.
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
// `ComInit::mta()` fails on a thread that is in another apartment, so
// callers stop there instead of creating objects (see `from_raw`).
unsafe impl<V> Send for Com<V> {}

impl<V> Com<V> {
    /// # Safety
    /// `ptr` must be a valid, owned (already AddRef'ed) interface pointer of type V, or NULL,
    /// created on a thread in the multithreaded apartment (`ComInit::mta()` succeeded),
    /// because `Com` may be sent to other threads.
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
        // SAFETY: `ptr` is a live COM object of interface V (`from_raw`'s contract), whose first
        // field is its vtable pointer; the vtable outlives the object, and V matches the SDK order.
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

/// Win32 event handle, closed on drop. Only `Event::new` makes one, so the
/// handle is ours to close.
pub struct Event(HANDLE);
// SAFETY: an event handle may be used and closed from any thread.
unsafe impl Send for Event {}
impl Event {
    pub fn new() -> Result<Event, String> {
        // auto-reset, initially non-signalled
        // SAFETY: plain call: no security attributes and no name.
        let h = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) };
        if h.is_null() {
            Err("CreateEventW failed".into())
        } else {
            Ok(Event(h))
        }
    }
    /// The handle, for waiting on it or handing it to WASAPI; still owned here.
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from CreateEventW in `Event::new`, is owned here and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Per-thread COM initialisation; `CoUninitialize` on drop, on the same
/// thread (the marker keeps it from being sent to another one).
pub struct ComInit(bool, std::marker::PhantomData<*const ()>);
impl ComInit {
    /// Multithreaded apartment, which the audio objects (`Com`) need. Fails
    /// on a thread already in a single-threaded apartment (RPC_E_CHANGED_MODE)
    /// or when COM can't start.
    pub fn mta() -> Result<ComInit, String> {
        // SAFETY: plain call; the matching CoUninitialize runs on drop, on this thread.
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED) };
        // S_FALSE (already initialised) also needs a matching CoUninitialize.
        if hr == RPC_E_CHANGED_MODE {
            Err("COM: this thread is in a single-threaded apartment".into())
        } else if hr < 0 {
            Err(format!("COM: CoInitializeEx failed: {}", hr_text(hr)))
        } else {
            Ok(ComInit(true, std::marker::PhantomData))
        }
    }
    /// Single-threaded apartment, which ShellExecuteW wants: shell
    /// extensions and protocol handlers (`ms-settings:`) may need it. On a
    /// thread already in the multithreaded apartment this stays a no-op
    /// (RPC_E_CHANGED_MODE), and COM is initialised either way.
    pub fn sta() -> ComInit {
        // SAFETY: plain call; the matching CoUninitialize runs on drop, on this thread.
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        ComInit(hr >= 0, std::marker::PhantomData)
    }
}
impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: CoInitializeEx succeeded on this thread (`self.0`), and ComInit is !Send, so this balances it here.
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
            // SAFETY: `w` is NUL-terminated (`wide` appends a 0) and outlives the call.
            unsafe { from_wide(w.as_ptr()) },
            "CABLE Output (VB-Audio Virtual Cable) ü"
        );
    }
}

