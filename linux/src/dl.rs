//! The dynamic loader. libjack, GLib/GTK and WebKitGTK are opened at run
//! time, so BananaSplit starts (and explains what is missing) without them.
//! The loader's FFI and the macro that builds a library's function table
//! live here, once.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}
const RTLD_NOW: c_int = 2;

/// A library opened by `dlopen`. It is never closed: the function tables
/// made from it live for the whole process.
#[derive(Clone, Copy)]
pub struct Handle(NonNull<c_void>);

/// The loader's last error ("unknown error" if it has none).
pub fn error() -> String {
    // SAFETY: dlerror takes no arguments and returns NULL or a C string.
    let e = unsafe { dlerror() };
    if e.is_null() {
        return "unknown error".into();
    }
    // SAFETY: non-null here, a NUL-terminated string from dlerror.
    unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
}

/// Open the first of `names` (file names or paths) that loads; else every
/// loader error, joined.
pub fn open(names: &[&CStr]) -> Result<Handle, String> {
    let mut errs = Vec::new();
    for n in names {
        // SAFETY: a NUL-terminated library name or path.
        if let Some(h) = NonNull::new(unsafe { dlopen(n.as_ptr(), RTLD_NOW) }) {
            return Ok(Handle(h));
        }
        errs.push(error());
    }
    Err(errs.join("; "))
}

/// The address of `name` in `h`, or NULL.
pub fn sym(h: Handle, name: &CStr) -> *mut c_void {
    // SAFETY: a handle from dlopen (never closed) and a NUL-terminated name.
    unsafe { dlsym(h.0.as_ptr(), name.as_ptr()) }
}

/// A library's function table: a struct with one field per C function and
/// `unsafe fn load(Handle)`, which looks each one up by its field name.
macro_rules! dl_api {
    ($vis:vis struct $name:ident { $( $f:ident : $ty:ty ),* $(,)? }) => {
        $vis struct $name { $( $vis $f: $ty, )* }
        impl $name {
            /// # Safety
            /// `h` must be the library that declares these C functions, with
            /// exactly these signatures (from its headers).
            unsafe fn load(h: $crate::dl::Handle) -> Result<Self, String> {
                Ok(Self { $( $f: {
                    let name = std::ffi::CStr::from_bytes_with_nul(concat!(stringify!($f), "\0").as_bytes())
                        .expect("a C identifier");
                    let p = $crate::dl::sym(h, name);
                    if p.is_null() {
                        return Err(format!("no symbol `{}`", stringify!($f)));
                    }
                    // SAFETY: the symbol has the C signature declared here
                    // (the caller's contract).
                    unsafe { std::mem::transmute::<*mut std::ffi::c_void, $ty>(p) }
                }, )* })
            }
        }
    };
}
pub(crate) use dl_api;
