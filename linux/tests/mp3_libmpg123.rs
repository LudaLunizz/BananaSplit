//! BananaSplit's MP3 files as another decoder hears them: libmpg123 (the
//! decoder of mpg123, VLC, GStreamer and many players), loaded at runtime.
//! It must play every file at its own length (the LAME tag's encoder delay
//! and padding) with the same samples BananaSplit's own decoder gives.
//!
//! Skipped (with a note) where libmpg123 is not installed.

use bananasplit::mp3::enc::Encoder;
use bananasplit::mp3::{Reader, probe};
use std::ffi::{CString, c_char, c_int, c_long, c_void};
use std::path::Path;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type New = unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut c_void;
type Param = unsafe extern "C" fn(*mut c_void, c_int, c_long, f64) -> c_int;
type FormatNone = unsafe extern "C" fn(*mut c_void) -> c_int;
type Format = unsafe extern "C" fn(*mut c_void, c_long, c_int, c_int) -> c_int;
type Open = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut usize) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type Delete = unsafe extern "C" fn(*mut c_void);

struct Mpg123 {
    new: New,
    param: Param,
    format_none: FormatNone,
    format: Format,
    open: Open,
    read: ReadFn,
    close: Close,
    delete: Delete,
}

impl Mpg123 {
    fn load() -> Option<Mpg123> {
        let lib = CString::new("libmpg123.so.0").unwrap();
        // RTLD_NOW
        let h = unsafe { dlopen(lib.as_ptr(), 2) };
        if h.is_null() {
            return None;
        }
        let sym = |name: &str| -> Option<*mut c_void> {
            let c = CString::new(name).unwrap();
            let p = unsafe { dlsym(h, c.as_ptr()) };
            (!p.is_null()).then_some(p)
        };
        // SAFETY: the symbols are libmpg123's functions with these signatures.
        unsafe {
            Some(Mpg123 {
                new: std::mem::transmute::<*mut c_void, New>(sym("mpg123_new")?),
                param: std::mem::transmute::<*mut c_void, Param>(sym("mpg123_param")?),
                format_none: std::mem::transmute::<*mut c_void, FormatNone>(sym("mpg123_format_none")?),
                format: std::mem::transmute::<*mut c_void, Format>(sym("mpg123_format")?),
                open: std::mem::transmute::<*mut c_void, Open>(sym("mpg123_open")?),
                read: std::mem::transmute::<*mut c_void, ReadFn>(sym("mpg123_read")?),
                close: std::mem::transmute::<*mut c_void, Close>(sym("mpg123_close")?),
                delete: std::mem::transmute::<*mut c_void, Delete>(sym("mpg123_delete")?),
            })
        }
    }

    /// Stereo float samples of `path`, gapless (libmpg123's default).
    fn decode(&self, path: &Path) -> Vec<f32> {
        const ADD_FLAGS: c_int = 2;
        const QUIET: c_long = 0x20;
        const GAPLESS: c_long = 0x40;
        const FORCE_FLOAT: c_long = 0x400;
        const FORCE_STEREO: c_long = 0x8;
        const ENC_FLOAT_32: c_int = 0x200;
        const DONE: c_int = -12;
        const NEW_FORMAT: c_int = -11;
        let mut err = 0;
        let p = CString::new(path.to_str().unwrap()).unwrap();
        let mut out = Vec::new();
        // SAFETY: plain calls into libmpg123 with a valid handle and buffers.
        unsafe {
            let mh = (self.new)(std::ptr::null(), &mut err);
            assert!(!mh.is_null(), "mpg123_new: {err}");
            (self.param)(mh, ADD_FLAGS, QUIET | GAPLESS | FORCE_FLOAT | FORCE_STEREO, 0.0);
            (self.format_none)(mh);
            for r in [32000, 44100, 48000] {
                (self.format)(mh, r, 2, ENC_FLOAT_32);
            }
            assert_eq!((self.open)(mh, p.as_ptr()), 0, "mpg123_open");
            let mut buf = vec![0f32; 16384];
            loop {
                let mut done = 0usize;
                let r = (self.read)(mh, buf.as_mut_ptr().cast(), buf.len() * 4, &mut done);
                out.extend_from_slice(&buf[..done / 4]);
                if r == DONE {
                    break;
                }
                assert!(r == 0 || r == NEW_FORMAT, "mpg123_read: {r}");
            }
            (self.close)(mh);
            (self.delete)(mh);
        }
        out
    }
}

fn ours(path: &Path) -> Vec<f32> {
    let info = probe(path).unwrap();
    let mut r = Reader::open(path, &info).unwrap();
    let mut out = Vec::new();
    while r.read(4096, &mut out).unwrap() > 0 {}
    out
}

#[test]
fn libmpg123_plays_our_files_at_their_length() {
    let Some(lib) = Mpg123::load() else {
        eprintln!("libmpg123 is not installed: skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("bs-mp3-mpg123-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 1u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5
    };
    for (rate, kbps, n) in [(48000, 320, 100_001), (44100, 192, 88_200), (32000, 128, 31_999), (48000, 256, 0), (44100, 320, 1)] {
        let lr: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let click = if i % 9000 < 300 { 0.6 * noise() } else { 0.0 };
                let l = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() + click;
                [l, 0.5 * l + 0.1 * noise()]
            })
            .collect();
        let mut e = Encoder::new(rate, kbps).unwrap();
        let mut body = Vec::new();
        for c in lr.chunks(2 * 4096) {
            e.encode(c, &mut body);
        }
        e.finish(&mut body);
        let path = dir.join(format!("{rate}-{kbps}-{n}.mp3"));
        std::fs::write(&path, [e.info_frame(), body].concat()).unwrap();
        let theirs = lib.decode(&path);
        let mine = ours(&path);
        assert_eq!(theirs.len(), 2 * n, "{rate} Hz {kbps} kbps {n}: libmpg123's length");
        assert_eq!(mine.len(), 2 * n);
        let worst = theirs.iter().zip(&mine).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-5, "{rate} Hz {kbps} kbps: decoders differ by {worst}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
