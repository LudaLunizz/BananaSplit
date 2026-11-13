//! Core Audio, CoreFoundation and Objective-C runtime calls, declared by
//! hand as in the SDK headers (the types and constants are in
//! `catypes.rs`), and safe wrappers for the device properties BananaSplit
//! reads.

use super::catypes::*;
use std::ffi::{c_char, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr::null;

type CFStringRef = *const c_void;
type CFIndex = isize;
type CFDictionaryRef = *const c_void;
/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// `kCFNotFound`.
const NOT_FOUND: CFIndex = -1;

/// An `AudioDeviceIOProc`: called by the HAL on the device's real-time IO
/// thread with its input and output for one IO cycle.
pub type IoProc = unsafe extern "C" fn(
    AudioObjectID,
    *const AudioTimeStamp,
    *const AudioBufferList,
    *const AudioTimeStamp,
    *mut AudioBufferList,
    *const AudioTimeStamp,
    *mut c_void,
) -> OSStatus;

/// An `AudioDeviceIOProcID`: the HAL's handle of one registration of an
/// IOProc (the type of the IOProc itself in the headers).
pub type IoProcId = Option<IoProc>;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    pub fn AudioDeviceCreateIOProcID(
        device: AudioObjectID,
        proc_: IoProcId,
        client: *mut c_void,
        out_id: *mut IoProcId,
    ) -> OSStatus;
    pub fn AudioDeviceDestroyIOProcID(device: AudioObjectID, id: IoProcId) -> OSStatus;
    pub fn AudioDeviceStart(device: AudioObjectID, id: IoProcId) -> OSStatus;
    pub fn AudioDeviceStop(device: AudioObjectID, id: IoProcId) -> OSStatus;
    /// `inDescription` is a `CATapDescription *` (macOS 14.2).
    fn AudioHardwareCreateProcessTap(description: *mut c_void, out_tap: *mut AudioObjectID) -> OSStatus;
    fn AudioHardwareDestroyProcessTap(tap: AudioObjectID) -> OSStatus;
    fn AudioHardwareCreateAggregateDevice(description: CFDictionaryRef, out_device: *mut AudioObjectID) -> OSStatus;
    fn AudioHardwareDestroyAggregateDevice(device: AudioObjectID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(s: CFStringRef, buffer: *mut c_char, size: CFIndex, encoding: u32) -> u8;
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> CFStringRef;
    fn CFRelease(cf: *const c_void);
    fn CFArrayCreate(
        alloc: *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const c_void,
    ) -> *const c_void;
    fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFNumberCreate(alloc: *const c_void, kind: CFIndex, value: *const c_void) -> *const c_void;
    /// Only their addresses are used (`CFArrayCallBacks`,
    /// `CFDictionaryKeyCallBacks`, `CFDictionaryValueCallBacks` structs).
    static kCFTypeArrayCallBacks: u8;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: *const c_void;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    /// `AVMediaTypeAudio`, an `NSString *`.
    static AVMediaTypeAudio: *const c_void;
}

// libSystem (always linked): libproc.h.
unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
}

/// `PROC_PIDPATHINFO_MAXSIZE` (sys/proc_info.h: 4 * MAXPATHLEN).
const PID_PATH_MAX: usize = 4096;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    fn objc_retain(object: *mut c_void) -> *mut c_void;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

/// A property that is plain data of a fixed size (a `UInt32`, a
/// `Float64`, a struct of those), or None.
fn get<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
    let a = address(selector, scope);
    let mut v = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `v` has room for `size` bytes; the HAL writes at most that.
    let st = unsafe { AudioObjectGetPropertyData(object, &a, 0, null(), &mut size, v.as_mut_ptr().cast()) };
    // SAFETY: the HAL wrote a whole T (plain data, any bit pattern valid).
    (st == 0 && size as usize == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

/// A property that is an array of plain data (the device list, a device's
/// streams), or None.
fn get_vec<T: Copy + Default>(object: AudioObjectID, selector: u32, scope: u32) -> Option<Vec<T>> {
    let a = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: only the size is written.
    if unsafe { AudioObjectGetPropertyDataSize(object, &a, 0, null(), &mut size) } != 0 {
        return None;
    }
    let mut v = vec![T::default(); size as usize / size_of::<T>()];
    if v.is_empty() {
        return Some(v);
    }
    let mut got = (v.len() * size_of::<T>()) as u32;
    // SAFETY: `v` has room for `got` bytes.
    if unsafe { AudioObjectGetPropertyData(object, &a, 0, null(), &mut got, v.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    // (a device may have gone in between: fewer entries)
    v.truncate(got as usize / size_of::<T>());
    Some(v)
}

fn set<T: Copy>(object: AudioObjectID, selector: u32, scope: u32, value: T) -> OSStatus {
    let a = address(selector, scope);
    // SAFETY: `value` is size_of::<T>() bytes of plain data.
    unsafe { AudioObjectSetPropertyData(object, &a, 0, null(), size_of::<T>() as u32, (&value as *const T).cast()) }
}

/// A property that is a `CFStringRef` the caller releases (names, UIDs).
fn get_string(object: AudioObjectID, selector: u32, scope: u32) -> Option<String> {
    let s: CFStringRef = get(object, selector, scope)?;
    if s.is_null() {
        return None;
    }
    // SAFETY: a CFString the HAL handed over (retained for us), released
    // here once read.
    unsafe {
        let text = string_of(s);
        CFRelease(s);
        text
    }
}

/// # Safety
/// `s` is a live CFString.
unsafe fn string_of(s: CFStringRef) -> Option<String> {
    // SAFETY: the caller's promise.
    unsafe {
        let max = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), UTF8);
        if max == NOT_FOUND || max < 0 {
            return None;
        }
        let mut buf = vec![0u8; max as usize + 1];
        if CFStringGetCString(s, buf.as_mut_ptr().cast(), buf.len() as CFIndex, UTF8) == 0 {
            return None;
        }
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }
}

/// Let the HAL send its notifications on a thread of its own rather than
/// the main thread's run loop: the mixer runs on a thread without one (and
/// headless, nothing runs one at all), and the HAL keeps its list of
/// devices up to date only while its notifications are handled.
pub fn hal_on_its_own_thread() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let run_loop: *const c_void = null();
        let _ = set(kAudioObjectSystemObject, kAudioHardwarePropertyRunLoop, kAudioObjectPropertyScopeGlobal, run_loop);
    });
}

/// Every device (hidden ones left out), or None if the HAL can't be asked.
pub fn device_ids() -> Option<Vec<AudioObjectID>> {
    get_vec::<AudioObjectID>(kAudioObjectSystemObject, kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal)
        .map(|v| {
            v.into_iter()
                .filter(|&d| get::<u32>(d, kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal) != Some(1))
                .collect()
        })
}

/// The default input (or output) device, if there is one.
pub fn default_device(input: bool) -> Option<AudioObjectID> {
    let sel = if input { kAudioHardwarePropertyDefaultInputDevice } else { kAudioHardwarePropertyDefaultOutputDevice };
    get::<AudioObjectID>(kAudioObjectSystemObject, sel, kAudioObjectPropertyScopeGlobal)
        .filter(|&d| d != kAudioObjectUnknown)
}

pub fn name(device: AudioObjectID) -> Option<String> {
    get_string(device, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal)
}

pub fn uid(device: AudioObjectID) -> Option<String> {
    get_string(device, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal)
}

fn scope(input: bool) -> u32 {
    if input { kAudioObjectPropertyScopeInput } else { kAudioObjectPropertyScopeOutput }
}

/// The device's input (or output) channels, all streams together.
pub fn channel_count(device: AudioObjectID, input: bool) -> u32 {
    stream_layout(device, input).map_or(0, |v| v.iter().sum())
}

/// The channels in each of the device's input (or output) buffers, as its
/// IOProc gets them (its stream configuration). None if it can't be read.
pub fn stream_layout(device: AudioObjectID, input: bool) -> Option<Vec<u32>> {
    let a = address(kAudioDevicePropertyStreamConfiguration, scope(input));
    let mut size = 0u32;
    // SAFETY: only the size is written.
    if unsafe { AudioObjectGetPropertyDataSize(device, &a, 0, null(), &mut size) } != 0 || size == 0 {
        return None;
    }
    // u64s: the list holds pointers, so it needs their alignment
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let mut got = size;
    // SAFETY: `buf` has room for `size` bytes.
    if unsafe { AudioObjectGetPropertyData(device, &a, 0, null(), &mut got, buf.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    let list = buf.as_ptr().cast::<AudioBufferList>();
    // SAFETY: the count is read from what was written, and the buffers it
    // counts are checked to lie within it before they are read.
    unsafe {
        let n = (*list).mNumberBuffers as usize;
        if got as usize >= size_of::<u32>()
            && std::mem::offset_of!(AudioBufferList, mBuffers) + n * size_of::<AudioBuffer>() <= got as usize
        {
            Some(buffers(list).iter().map(|b| b.mNumberChannels).collect())
        } else {
            None
        }
    }
}

/// The rate the device runs at (0 if unknown).
pub fn nominal_rate(device: AudioObjectID) -> f64 {
    get::<f64>(device, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal)
        .filter(|r| r.is_finite() && *r > 0.0)
        .unwrap_or(0.0)
}

/// A software device (transport type "virtual").
pub fn is_virtual(device: AudioObjectID) -> bool {
    get::<u32>(device, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal)
        == Some(kAudioDeviceTransportTypeVirtual)
}

/// Every input (or output) stream of the device hands its samples to an
/// IOProc as 32-bit floats (it has at least one).
pub fn float32(device: AudioObjectID, input: bool) -> bool {
    let streams = get_vec::<AudioObjectID>(device, kAudioDevicePropertyStreams, scope(input)).unwrap_or_default();
    !streams.is_empty()
        && streams.iter().all(|&s| {
            get::<AudioStreamBasicDescription>(s, kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal)
                .is_some_and(|f| is_float32(&f))
        })
}

/// Ask the device for IO cycles of `frames` frames, within the range it
/// allows (0: leave it as it is); returns the size it uses now.
pub fn buffer_frames(device: AudioObjectID, frames: u32) -> u32 {
    if frames > 0 {
        // (a range that makes no sense is not used: clamp would panic)
        let range =
            get::<AudioValueRange>(device, kAudioDevicePropertyBufferFrameSizeRange, kAudioObjectPropertyScopeGlobal)
                .map(|r| (r.mMinimum, r.mMaximum));
        if let Some(want) = bananasplit::pure::buffer_request(frames, range) {
            let _ = set(device, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, want);
        }
    }
    get::<u32>(device, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal)
        .filter(|&f| f > 0)
        .unwrap_or(512)
}

/// The channels of the device's stereo, zero-based (see
/// `catypes::stereo_pair`).
pub fn stereo_pair(device: AudioObjectID, input: bool) -> (usize, usize) {
    let preferred = get::<[u32; 2]>(device, kAudioDevicePropertyPreferredChannelsForStereo, scope(input));
    super::catypes::stereo_pair(preferred, channel_count(device, input))
}

/// Switch off every input (`input`) or output stream of the device for one
/// IOProc, which doesn't use that direction. The HAL runs a direction only
/// for the IOProcs that use it: a headset BananaSplit only plays on keeps
/// its microphone off (no microphone light, no question about it).
pub fn streams_off(device: AudioObjectID, id: IoProcId, input: bool) -> OSStatus {
    let streams = get_vec::<AudioObjectID>(device, kAudioDevicePropertyStreams, scope(input)).map_or(0, |v| v.len());
    if streams == 0 {
        return 0;
    }
    let io_proc: *const c_void = id.map_or(null(), |f| f as *const c_void);
    let (words, size) = streams_off_usage(io_proc, streams);
    let a = address(kAudioDevicePropertyIOProcStreamUsage, scope(input));
    // SAFETY: `words` holds the struct's `size` bytes.
    unsafe { AudioObjectSetPropertyData(device, &a, 0, null(), size as u32, words.as_ptr().cast()) }
}

/// What the device adds on its way in (or out), in its frames: its own
/// latency, its first stream's and the HAL's safety offset.
pub fn latency_frames(device: AudioObjectID, input: bool) -> u32 {
    let sc = scope(input);
    let dev = get::<u32>(device, kAudioDevicePropertyLatency, sc).unwrap_or(0);
    let safety = get::<u32>(device, kAudioDevicePropertySafetyOffset, sc).unwrap_or(0);
    let stream = get_vec::<AudioObjectID>(device, kAudioDevicePropertyStreams, sc)
        .and_then(|v| v.first().copied())
        .and_then(|s| get::<u32>(s, kAudioStreamPropertyLatency, kAudioObjectPropertyScopeGlobal))
        .unwrap_or(0);
    dev.saturating_add(safety).saturating_add(stream)
}

/// May BananaSplit record from microphones? Denied or Restricted: the user
/// said no (or a policy does) in System Settings › Privacy & Security ›
/// Microphone, and every input reads as silence. NotDetermined: not asked
/// yet (the first input BananaSplit starts asks). Never asks by itself.
pub fn mic_access() -> bananasplit::pure::Access {
    // SAFETY: Objective-C runtime calls with NUL-terminated names; the
    // message `+[AVCaptureDevice authorizationStatusForMediaType:]` takes an
    // NSString and returns an NSInteger, and objc_msgSend is called through
    // a pointer of exactly that type, as Apple's ABI requires. The mixer's
    // thread has no autorelease pool of its own: this one takes what the
    // call leaves.
    unsafe {
        let class = objc_getClass(c"AVCaptureDevice".as_ptr());
        let sel = sel_registerName(c"authorizationStatusForMediaType:".as_ptr());
        if class.is_null() || sel.is_null() || AVMediaTypeAudio.is_null() {
            return bananasplit::pure::Access::NotDetermined;
        }
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> isize =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let pool = objc_autoreleasePoolPush();
        let status = send(class, sel, AVMediaTypeAudio);
        objc_autoreleasePoolPop(pool);
        bananasplit::pure::access_from_status(status)
    }
}

/// Keep App Nap away while BananaSplit runs. With only its menu bar icon
/// showing, macOS could otherwise count it as idle and slow down its
/// timers: the mixer's own clock when no output device plays, VBAN, the
/// recorder.
pub fn no_app_nap() {
    // NSActivityUserInitiatedAllowingIdleSystemSleep: work the user asked
    // for (no App Nap), while the Mac may still sleep when it is idle.
    const USER_INITIATED_ALLOWING_IDLE_SLEEP: u64 = 0x00FF_FFFF & !(1 << 20);
    // SAFETY: as in `mic_access`: `+[NSProcessInfo processInfo]` returns
    // the process's NSProcessInfo, and `-beginActivityWithOptions:reason:`
    // takes an NSActivityOptions (uint64_t) and an NSString (a CFString is
    // one) and returns the activity, retained here so that it lasts until
    // the process ends.
    unsafe {
        let class = objc_getClass(c"NSProcessInfo".as_ptr());
        let info = sel_registerName(c"processInfo".as_ptr());
        let begin = sel_registerName(c"beginActivityWithOptions:reason:".as_ptr());
        if class.is_null() || info.is_null() || begin.is_null() {
            return;
        }
        let reason = CFStringCreateWithCString(null(), c"BananaSplit mixes audio in the background".as_ptr(), UTF8);
        if reason.is_null() {
            return;
        }
        let process_info: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let begin_activity: unsafe extern "C" fn(*mut c_void, *mut c_void, u64, CFStringRef) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let pool = objc_autoreleasePoolPush();
        let pi = process_info(class, info);
        if !pi.is_null() {
            let activity = begin_activity(pi, begin, USER_INITIATED_ALLOWING_IDLE_SLEEP, reason);
            if !activity.is_null() {
                objc_retain(activity);
            }
        }
        objc_autoreleasePoolPop(pool);
        CFRelease(reason);
    }
}

/// The running macOS version (major, minor); None if it can't be read.
pub fn os_version() -> Option<(u32, u32)> {
    bananasplit::pure::parse_os_version(&crate::sys::os_product_version()?)
}

/// The executable of a process, or None (it ended, or isn't ours to see).
pub fn pid_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; PID_PATH_MAX];
    // SAFETY: `buf` has room for PID_PATH_MAX bytes; it returns the length
    // written (not counting the NUL), or 0 or less on failure.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), PID_PATH_MAX as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate((n as usize).min(PID_PATH_MAX));
    String::from_utf8(buf).ok()
}

/// One process that Core Audio knows (it opened audio).
pub struct AudioProcess {
    /// Its process object (what a tap is made of).
    pub id: AudioObjectID,
    pub pid: i32,
    pub bundle_id: Option<String>,
    /// It runs IO with an output stream now.
    pub playing: bool,
    pub path: Option<String>,
}

/// Every process Core Audio lists, read only (no tap, nothing started).
/// Empty below macOS 14.2, where the list doesn't exist, or when it can't
/// be read.
pub fn audio_processes() -> Vec<AudioProcess> {
    let (need_major, need_minor) = bananasplit::pure::PROCESS_LIST_OS;
    match os_version() {
        Some((major, minor)) if bananasplit::pure::os_at_least(need_major, need_minor, major, minor) => {}
        _ => return Vec::new(),
    }
    hal_on_its_own_thread();
    let g = kAudioObjectPropertyScopeGlobal;
    let ids = get_vec::<AudioObjectID>(kAudioObjectSystemObject, kAudioHardwarePropertyProcessObjectList, g)
        .unwrap_or_default();
    ids.into_iter()
        .filter_map(|p| {
            let pid = get::<i32>(p, kAudioProcessPropertyPID, g)?;
            Some(AudioProcess {
                id: p,
                pid,
                bundle_id: get_string(p, kAudioProcessPropertyBundleID, g).filter(|b| !b.is_empty()),
                playing: get::<u32>(p, kAudioProcessPropertyIsRunningOutput, g).is_some_and(|v| v != 0),
                path: pid_path(pid),
            })
        })
        .collect()
}

/// A CoreFoundation object this code created: released when dropped.
struct Cf(*const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created here with a +1 retain count, released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(s: &str) -> Result<Cf, String> {
    let c = std::ffi::CString::new(s).map_err(|_| "a name with a NUL in it")?;
    // SAFETY: a NUL-terminated UTF-8 string; the default allocator.
    let r = unsafe { CFStringCreateWithCString(null(), c.as_ptr(), UTF8) };
    if r.is_null() { Err("cannot make a CFString".into()) } else { Ok(Cf(r)) }
}

fn cf_number(v: i64) -> Result<Cf, String> {
    // SAFETY: `v` is an SInt64, as the type says; the default allocator.
    let r = unsafe { CFNumberCreate(null(), kCFNumberSInt64Type, (&v as *const i64).cast()) };
    if r.is_null() { Err("cannot make a CFNumber".into()) } else { Ok(Cf(r)) }
}

/// A CFArray of CF objects (it retains them).
fn cf_array(items: &[&Cf]) -> Result<Cf, String> {
    let ptrs: Vec<*const c_void> = items.iter().map(|c| c.0).collect();
    // SAFETY: `ptrs` holds live CF objects; kCFTypeArrayCallBacks retains
    // them for the array.
    let r = unsafe {
        CFArrayCreate(null(), ptrs.as_ptr(), ptrs.len() as CFIndex, (&raw const kCFTypeArrayCallBacks).cast())
    };
    if r.is_null() { Err("cannot make a CFArray".into()) } else { Ok(Cf(r)) }
}

/// A CFDictionary from (key, value) CF objects (it retains them).
fn cf_dict(pairs: &[(&Cf, *const c_void)]) -> Result<Cf, String> {
    let keys: Vec<*const c_void> = pairs.iter().map(|(k, _)| k.0).collect();
    let values: Vec<*const c_void> = pairs.iter().map(|(_, v)| *v).collect();
    // SAFETY: live CF keys and values; the CFType callbacks retain them.
    let r = unsafe {
        CFDictionaryCreate(
            null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as CFIndex,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        )
    };
    if r.is_null() { Err("cannot make a CFDictionary".into()) } else { Ok(Cf(r)) }
}

fn cf_key(k: &std::ffi::CStr) -> Result<Cf, String> {
    cf_string(k.to_str().map_err(|_| "a key that is not UTF-8")?)
}

/// Create a private process tap: a stereo mixdown of `processes` (process
/// objects), each of them silent on its own device while the tap is read
/// (`CATapMutedWhenTapped`). Returns the tap's object ID.
pub fn create_process_tap(processes: &[AudioObjectID]) -> Result<AudioObjectID, String> {
    let numbers = processes.iter().map(|&p| cf_number(i64::from(p))).collect::<Result<Vec<_>, _>>()?;
    // a CFArray of CFNumbers is an NSArray of NSNumbers (toll-free bridged)
    let list = cf_array(&numbers.iter().collect::<Vec<_>>())?;
    // SAFETY: Objective-C runtime calls with NUL-terminated names. Each
    // message is sent through an objc_msgSend pointer of exactly its type
    // (Apple's ABI): +alloc and -release take no argument,
    // -initStereoMixdownOfProcesses: takes an NSArray and returns the
    // object (it consumes the alloc'd one), -setPrivate: takes a BOOL (one
    // byte on both architectures), -setMuteBehavior: an NSInteger. The
    // description is released after the HAL made the tap from it.
    unsafe {
        let class = objc_getClass(c"CATapDescription".as_ptr());
        let alloc = sel_registerName(c"alloc".as_ptr());
        let init = sel_registerName(c"initStereoMixdownOfProcesses:".as_ptr());
        let set_private = sel_registerName(c"setPrivate:".as_ptr());
        let set_mute = sel_registerName(c"setMuteBehavior:".as_ptr());
        let release = sel_registerName(c"release".as_ptr());
        if class.is_null() {
            return Err("this macOS has no process taps (CATapDescription is missing)".into());
        }
        let send0: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_obj: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_bool: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_int: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let pool = objc_autoreleasePoolPush();
        let desc = send_obj(send0(class, alloc), init, list.0);
        let result = if desc.is_null() {
            Err("cannot describe the tap (CATapDescription)".to_string())
        } else {
            send_bool(desc, set_private, true);
            send_int(desc, set_mute, CATapMutedWhenTapped);
            let mut tap: AudioObjectID = kAudioObjectUnknown;
            let st = AudioHardwareCreateProcessTap(desc, &mut tap);
            send0(desc, release);
            if st != 0 || tap == kAudioObjectUnknown {
                Err(format!("cannot create the tap ({})", status_text(st)))
            } else {
                Ok(tap)
            }
        };
        objc_autoreleasePoolPop(pool);
        result
    }
}

pub fn destroy_process_tap(tap: AudioObjectID) {
    // SAFETY: a tap this process created; destroyed once (its owner's Drop).
    unsafe { AudioHardwareDestroyProcessTap(tap) };
}

/// The tap's UID (for an aggregate device's tap list).
pub fn tap_uid(tap: AudioObjectID) -> Option<String> {
    get_string(tap, kAudioTapPropertyUID, kAudioObjectPropertyScopeGlobal)
}

/// The tap's channel count (a stereo mixdown has 2).
pub fn tap_channels(tap: AudioObjectID) -> Option<u32> {
    get::<AudioStreamBasicDescription>(tap, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal)
        .map(|f| f.mChannelsPerFrame)
}

/// Create a private aggregate device (only this process sees it) named
/// `name` with UID `uid`: the output device `main_uid` as its only
/// sub-device and its clock, and the tap `tap_uid` in its tap list with
/// drift compensation on. Returns its object ID.
pub fn create_private_aggregate(uid: &str, name: &str, main_uid: &str, tap_uid: &str) -> Result<AudioObjectID, String> {
    let (k_uid, k_name, k_subs, k_main, k_private, k_taps) = (
        cf_key(kAudioAggregateDeviceUIDKey)?,
        cf_key(kAudioAggregateDeviceNameKey)?,
        cf_key(kAudioAggregateDeviceSubDeviceListKey)?,
        cf_key(kAudioAggregateDeviceMainSubDeviceKey)?,
        cf_key(kAudioAggregateDeviceIsPrivateKey)?,
        cf_key(kAudioAggregateDeviceTapListKey)?,
    );
    let (k_sub_uid, k_tap_uid, k_drift) =
        (cf_key(kAudioSubDeviceUIDKey)?, cf_key(kAudioSubTapUIDKey)?, cf_key(kAudioSubTapDriftCompensationKey)?);
    let (v_uid, v_name, v_main, v_tap) = (cf_string(uid)?, cf_string(name)?, cf_string(main_uid)?, cf_string(tap_uid)?);
    // SAFETY: a CFBoolean constant of CoreFoundation, never released.
    let yes = unsafe { kCFBooleanTrue };
    let sub = cf_dict(&[(&k_sub_uid, v_main.0)])?;
    let subs = cf_array(&[&sub])?;
    let tap = cf_dict(&[(&k_tap_uid, v_tap.0), (&k_drift, yes)])?;
    let taps = cf_array(&[&tap])?;
    let desc = cf_dict(&[
        (&k_uid, v_uid.0),
        (&k_name, v_name.0),
        (&k_subs, subs.0),
        (&k_main, v_main.0),
        (&k_private, yes),
        (&k_taps, taps.0),
    ])?;
    let mut dev: AudioObjectID = kAudioObjectUnknown;
    // SAFETY: a CFDictionary made above, alive for the call.
    let st = unsafe { AudioHardwareCreateAggregateDevice(desc.0, &mut dev) };
    if st != 0 || dev == kAudioObjectUnknown {
        return Err(format!("cannot create the aggregate device ({})", status_text(st)));
    }
    Ok(dev)
}

pub fn destroy_aggregate(device: AudioObjectID) {
    // SAFETY: an aggregate this process created; destroyed once (its
    // owner's Drop).
    unsafe { AudioHardwareDestroyAggregateDevice(device) };
}
