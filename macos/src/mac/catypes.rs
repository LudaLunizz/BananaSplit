//! Core Audio's data types and constants, and reading and writing the
//! buffers an IOProc gets. No macOS calls, so it is unit-tested on every
//! platform; the functions are in `ffi.rs`.
//!
//! The layouts and values follow the macOS SDK headers (`CoreAudioTypes.h`,
//! `AudioHardware.h`, `AudioHardwareBase.h`), as the objc2-core-audio
//! bindings, generated from them, also have them.

#![allow(non_upper_case_globals, non_snake_case)]

use bananasplit::ring::Producer;
use std::ffi::c_void;

pub type OSStatus = i32;
pub type AudioObjectID = u32;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

/// `mNumberBuffers` buffers follow (the struct declares the first).
#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioValueRange {
    pub mMinimum: f64,
    pub mMaximum: f64,
}

/// Only ever behind a pointer here (an IOProc's time stamps are not read).
#[repr(C)]
pub struct AudioTimeStamp {
    _private: [u8; 0],
}

// The system object and the parts of a property's address.
pub const kAudioObjectSystemObject: AudioObjectID = 1;
pub const kAudioObjectUnknown: AudioObjectID = 0;
pub const kAudioObjectPropertyScopeGlobal: u32 = 0x676c_6f62; // 'glob'
pub const kAudioObjectPropertyScopeInput: u32 = 0x696e_7074; // 'inpt'
pub const kAudioObjectPropertyScopeOutput: u32 = 0x6f75_7470; // 'outp'
pub const kAudioObjectPropertyElementMain: u32 = 0;

// Properties of the system object.
pub const kAudioHardwarePropertyDevices: u32 = 0x6465_7623; // 'dev#'
pub const kAudioHardwarePropertyDefaultInputDevice: u32 = 0x6449_6e20; // 'dIn '
pub const kAudioHardwarePropertyDefaultOutputDevice: u32 = 0x644f_7574; // 'dOut'
pub const kAudioHardwarePropertyRunLoop: u32 = 0x726e_6c70; // 'rnlp'

// Properties of devices and streams.
pub const kAudioObjectPropertyName: u32 = 0x6c6e_616d; // 'lnam'
pub const kAudioDevicePropertyDeviceUID: u32 = 0x7569_6420; // 'uid '
pub const kAudioDevicePropertyStreamConfiguration: u32 = 0x736c_6179; // 'slay'
pub const kAudioDevicePropertyNominalSampleRate: u32 = 0x6e73_7274; // 'nsrt'
pub const kAudioDevicePropertyBufferFrameSize: u32 = 0x6673_697a; // 'fsiz'
pub const kAudioDevicePropertyBufferFrameSizeRange: u32 = 0x6673_7a23; // 'fsz#'
pub const kAudioDevicePropertyLatency: u32 = 0x6c74_6e63; // 'ltnc'
pub const kAudioDevicePropertySafetyOffset: u32 = 0x7361_6674; // 'saft'
pub const kAudioDevicePropertyStreams: u32 = 0x7374_6d23; // 'stm#'
pub const kAudioDevicePropertyTransportType: u32 = 0x7472_616e; // 'tran'
pub const kAudioDevicePropertyIsHidden: u32 = 0x6869_646e; // 'hidn'
pub const kAudioDevicePropertyPreferredChannelsForStereo: u32 = 0x6463_6832; // 'dch2'
pub const kAudioDevicePropertyIOProcStreamUsage: u32 = 0x7375_7365; // 'suse'
pub const kAudioStreamPropertyVirtualFormat: u32 = 0x7366_6d74; // 'sfmt'
/// Same selector as the device's latency, asked of a stream.
pub const kAudioStreamPropertyLatency: u32 = kAudioDevicePropertyLatency;

// Transport types.
pub const kAudioDeviceTransportTypeVirtual: u32 = 0x7669_7274; // 'virt'

// Formats.
pub const kAudioFormatLinearPCM: u32 = 0x6c70_636d; // 'lpcm'
pub const kAudioFormatFlagIsFloat: u32 = 1 << 0;
pub const kAudioFormatFlagIsBigEndian: u32 = 1 << 1;

// Errors.
pub const kAudioHardwareNotRunningError: OSStatus = 0x7374_6f70; // 'stop'
pub const kAudioHardwareIllegalOperationError: OSStatus = 0x6e6f_7065; // 'nope'
pub const kAudioHardwareBadObjectError: OSStatus = 0x216f_626a; // '!obj'
pub const kAudioHardwareBadDeviceError: OSStatus = 0x2164_6576; // '!dev'
pub const kAudioHardwareUnsupportedOperationError: OSStatus = 0x756e_6f70; // 'unop'
pub const kAudioHardwareNotReadyError: OSStatus = 0x6e72_6479; // 'nrdy'
pub const kAudioDeviceUnsupportedFormatError: OSStatus = 0x2164_6174; // '!dat'
pub const kAudioDevicePermissionsError: OSStatus = 0x2168_6f67; // '!hog'

/// An `OSStatus` as people can look it up: its four characters when it is
/// a four-character code ('!hog'), else the number; with what it means for
/// the ones a device can answer.
pub fn status_text(st: OSStatus) -> String {
    let b = (st as u32).to_be_bytes();
    let code = if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("'{}'", String::from_utf8_lossy(&b))
    } else {
        st.to_string()
    };
    let why = match st {
        kAudioHardwareNotRunningError => ": the audio system is not running",
        kAudioHardwareBadDeviceError | kAudioHardwareBadObjectError => ": the device is gone",
        kAudioDevicePermissionsError => ": another app uses the device exclusively",
        kAudioDeviceUnsupportedFormatError => ": the device does not take this format",
        kAudioHardwareNotReadyError => ": the device is not ready",
        kAudioHardwareIllegalOperationError => ": not allowed",
        kAudioHardwareUnsupportedOperationError => ": not supported by the device",
        _ => "",
    };
    format!("{code}{why}")
}

/// 32-bit float samples in this computer's byte order: what an IOProc
/// reads and writes (the virtual format of nearly every device).
pub fn is_float32(f: &AudioStreamBasicDescription) -> bool {
    f.mFormatID == kAudioFormatLinearPCM
        && f.mFormatFlags & kAudioFormatFlagIsFloat != 0
        && (f.mFormatFlags & kAudioFormatFlagIsBigEndian != 0) == cfg!(target_endian = "big")
        && f.mBitsPerChannel == 32
}

/// The two channels (zero-based) a device's stereo goes to or comes from:
/// the ones chosen in Audio MIDI Setup (Configure Speakers), which macOS
/// gives as the device's preferred channels for stereo (one-based), when
/// they are two different channels it has; otherwise its first two.
pub fn stereo_pair(preferred: Option<[u32; 2]>, channels: u32) -> (usize, usize) {
    match preferred {
        Some([l, r]) if l != r && (1..=channels).contains(&l) && (1..=channels).contains(&r) => (l as usize - 1, r as usize - 1),
        _ => (0, 1),
    }
}

/// An `AudioHardwareIOProcStreamUsage` that switches all `streams` streams
/// of one direction off for the IOProc `proc_id`: the IOProc, the number
/// of streams, then a `UInt32` per stream (0: off). Returns its words
/// (8-byte aligned, as the struct is) and its size in bytes.
pub fn streams_off_usage(proc_id: *const c_void, streams: usize) -> (Vec<u64>, usize) {
    let head = std::mem::size_of::<*const c_void>() + std::mem::size_of::<u32>();
    let size = head + streams * std::mem::size_of::<u32>();
    let mut words = vec![0u64; size.div_ceil(8)];
    let p = words.as_mut_ptr().cast::<u8>();
    // SAFETY: `words` has room for `size` bytes and is 8-byte aligned: the
    // pointer goes first, the count right after it (every stream is 0).
    unsafe {
        p.cast::<*const c_void>().write(proc_id);
        p.add(std::mem::size_of::<*const c_void>()).cast::<u32>().write(streams as u32);
    }
    (words, size)
}

/// One channel in a buffer list: its first sample, the distance between
/// its samples (the buffer's channel count) and its number of frames.
#[derive(Debug, Clone, Copy)]
pub struct Chan {
    pub data: *mut f32,
    pub stride: usize,
    pub frames: usize,
}

/// The buffers of a list (`mNumberBuffers` of them).
///
/// # Safety
/// `list` is null or points at a buffer list with `mNumberBuffers` buffers.
pub unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
    if list.is_null() {
        return &[];
    }
    // SAFETY: the caller's promise; the buffers follow the count.
    unsafe {
        let n = (*list).mNumberBuffers as usize;
        std::slice::from_raw_parts(std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(), n)
    }
}

/// Channel `ch` of a list, counting through all its buffers (a device has a
/// buffer per stream; a stream's channels are interleaved in it). None if
/// there is no such channel or its stream is switched off (no data).
///
/// # Safety
/// As for [`buffers`].
pub unsafe fn channel(list: *const AudioBufferList, ch: usize) -> Option<Chan> {
    let mut ch = ch;
    // SAFETY: the caller's promise.
    for b in unsafe { buffers(list) } {
        let n = b.mNumberChannels as usize;
        if ch < n {
            if b.mData.is_null() {
                return None;
            }
            // its first sample is the ch-th of the first frame (only read
            // below `frames`, so an empty buffer is never read)
            let data = b.mData.cast::<f32>().wrapping_add(ch);
            return Some(Chan { data, stride: n, frames: b.mDataByteSize as usize / (4 * n) });
        }
        ch -= n;
    }
    None
}

/// All channels of a list.
///
/// # Safety
/// As for [`buffers`].
pub unsafe fn channels(list: *const AudioBufferList) -> u32 {
    // SAFETY: the caller's promise.
    unsafe { buffers(list) }.iter().map(|b| b.mNumberChannels).sum()
}

/// Frames in a device's output buffers this cycle (0 if it has none).
///
/// # Safety
/// As for [`buffers`].
pub unsafe fn output_frames(list: *const AudioBufferList) -> usize {
    // SAFETY: the caller's promise.
    unsafe { channel(list, 0) }.map_or(0, |c| c.frames)
}

/// Push a device's input into a ring: its stereo pair (`pair`, see
/// [`stereo_pair`]) as L and R, or its only channel as both. Returns the
/// frames pushed.
///
/// # Safety
/// As for [`buffers`]; the buffers hold 32-bit floats.
pub unsafe fn take_input(list: *const AudioBufferList, pair: (usize, usize), prod: &mut Producer) -> usize {
    // SAFETY: the caller's promise.
    let Some(l) = (unsafe { channel(list, pair.0) }) else { return 0 };
    let r = unsafe { channel(list, pair.1) }.unwrap_or(l);
    let n = l.frames.min(r.frames);
    for i in 0..n {
        // SAFETY: i < frames of each channel, stepping by its stride.
        let f = unsafe { [*l.data.add(i * l.stride), *r.data.add(i * r.stride)] };
        if !prod.push(f) {
            break; // full: the reader is far behind and trims
        }
    }
    n
}

/// Write `l` and `r` into a device's output from frame `at` on: its stereo
/// pair (`pair`, see [`stereo_pair`]), or (a mono device) their average
/// into its only channel. The HAL hands the buffers over zeroed, so other
/// channels stay silent.
///
/// # Safety
/// As for [`buffers`]; the buffers hold 32-bit floats and at least
/// `at + l.len()` frames.
pub unsafe fn put_output(list: *mut AudioBufferList, pair: (usize, usize), at: usize, l: &[f32], r: &[f32]) {
    let n = l.len().min(r.len());
    // SAFETY: the caller's promise.
    let (Some(a), b) = (unsafe { channel(list, pair.0) }, unsafe { channel(list, pair.1) }) else { return };
    let n = n.min(a.frames.saturating_sub(at));
    match b {
        Some(b) => {
            let n = n.min(b.frames.saturating_sub(at));
            for i in 0..n {
                // SAFETY: at + i < frames of each channel.
                unsafe {
                    *a.data.add((at + i) * a.stride) = l[i];
                    *b.data.add((at + i) * b.stride) = r[i];
                }
            }
        }
        None => {
            for i in 0..n {
                // SAFETY: at + i < frames.
                unsafe { *a.data.add((at + i) * a.stride) = 0.5 * (l[i] + r[i]) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bananasplit::ring::ring;

    /// A list of two buffers, as a device with two streams hands it over.
    #[repr(C)]
    struct List2 {
        n: u32,
        b: [AudioBuffer; 2],
    }

    fn buf(channels: u32, data: &mut [f32]) -> AudioBuffer {
        AudioBuffer { mNumberChannels: channels, mDataByteSize: (data.len() * 4) as u32, mData: data.as_mut_ptr().cast() }
    }

    #[test]
    fn layout_matches_the_sdk() {
        // 64-bit macOS: the buffers start 8 bytes in, 16 bytes each
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<AudioBuffer>(), 16);
            assert_eq!(std::mem::offset_of!(AudioBufferList, mBuffers), 8);
            assert_eq!(std::mem::size_of::<AudioBufferList>(), 24);
        }
        assert_eq!(std::mem::size_of::<AudioStreamBasicDescription>(), 40);
        assert_eq!(std::mem::size_of::<AudioObjectPropertyAddress>(), 12);
        assert_eq!(std::mem::size_of::<AudioValueRange>(), 16);
        assert_eq!(std::mem::offset_of!(List2, b), std::mem::offset_of!(AudioBufferList, mBuffers));
    }

    #[test]
    fn four_character_codes() {
        let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
        assert_eq!(kAudioObjectPropertyScopeGlobal, code(b"glob"));
        assert_eq!(kAudioObjectPropertyScopeInput, code(b"inpt"));
        assert_eq!(kAudioObjectPropertyScopeOutput, code(b"outp"));
        assert_eq!(kAudioHardwarePropertyDevices, code(b"dev#"));
        assert_eq!(kAudioHardwarePropertyDefaultInputDevice, code(b"dIn "));
        assert_eq!(kAudioHardwarePropertyDefaultOutputDevice, code(b"dOut"));
        assert_eq!(kAudioHardwarePropertyRunLoop, code(b"rnlp"));
        assert_eq!(kAudioObjectPropertyName, code(b"lnam"));
        assert_eq!(kAudioDevicePropertyDeviceUID, code(b"uid "));
        assert_eq!(kAudioDevicePropertyStreamConfiguration, code(b"slay"));
        assert_eq!(kAudioDevicePropertyNominalSampleRate, code(b"nsrt"));
        assert_eq!(kAudioDevicePropertyBufferFrameSize, code(b"fsiz"));
        assert_eq!(kAudioDevicePropertyBufferFrameSizeRange, code(b"fsz#"));
        assert_eq!(kAudioDevicePropertyLatency, code(b"ltnc"));
        assert_eq!(kAudioDevicePropertySafetyOffset, code(b"saft"));
        assert_eq!(kAudioDevicePropertyStreams, code(b"stm#"));
        assert_eq!(kAudioDevicePropertyTransportType, code(b"tran"));
        assert_eq!(kAudioDevicePropertyIsHidden, code(b"hidn"));
        assert_eq!(kAudioDevicePropertyPreferredChannelsForStereo, code(b"dch2"));
        assert_eq!(kAudioDevicePropertyIOProcStreamUsage, code(b"suse"));
        assert_eq!(kAudioStreamPropertyVirtualFormat, code(b"sfmt"));
        assert_eq!(kAudioDeviceTransportTypeVirtual, code(b"virt"));
        assert_eq!(kAudioFormatLinearPCM, code(b"lpcm"));
        assert_eq!(kAudioDevicePermissionsError as u32, code(b"!hog"));
        assert_eq!(kAudioHardwareBadDeviceError as u32, code(b"!dev"));
    }

    #[test]
    fn status_texts() {
        assert_eq!(status_text(kAudioDevicePermissionsError), "'!hog': another app uses the device exclusively");
        assert_eq!(status_text(0x7768_6f3f), "'who?'"); // kAudioHardwareUnknownPropertyError
        assert_eq!(status_text(-50), "-50");
        assert_eq!(status_text(0), "0");
    }

    #[test]
    fn float32_formats() {
        let mut f = AudioStreamBasicDescription {
            mSampleRate: 48_000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | 8, // packed
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        assert!(is_float32(&f));
        f.mBitsPerChannel = 24;
        assert!(!is_float32(&f));
        f.mBitsPerChannel = 32;
        f.mFormatFlags = 4; // signed integer
        assert!(!is_float32(&f));
    }

    #[test]
    fn stereo_from_one_interleaved_buffer() {
        // three channels, interleaved: frame i is [10i, 10i+1, 10i+2]
        let mut d: Vec<f32> = (0..4).flat_map(|i| [10.0 * i as f32, 10.0 * i as f32 + 1.0, 10.0 * i as f32 + 2.0]).collect();
        let list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(3, &mut d)] };
        let (mut p, mut c) = ring(16);
        assert_eq!(unsafe { channels(&list) }, 3);
        assert_eq!(unsafe { take_input(&list, (0, 1), &mut p) }, 4);
        for i in 0..4 {
            assert_eq!(c.pop(), Some([10.0 * i as f32, 10.0 * i as f32 + 1.0]));
        }
        assert!(c.pop().is_none());
    }

    #[test]
    fn mono_input_goes_to_both_sides() {
        let mut d = [0.5f32, -0.5, 0.25];
        let list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(1, &mut d)] };
        let (mut p, mut c) = ring(8);
        assert_eq!(unsafe { take_input(&list, (0, 1), &mut p) }, 3);
        assert_eq!(c.pop(), Some([0.5, 0.5]));
        assert_eq!(c.pop(), Some([-0.5, -0.5]));
    }

    #[test]
    fn channels_across_streams() {
        // two mono streams (a device with one buffer per channel)
        let (mut a, mut b) = ([1.0f32, 2.0], [-1.0f32, -2.0]);
        let list = List2 { n: 2, b: [buf(1, &mut a), buf(1, &mut b)] };
        let ptr = (&list as *const List2).cast::<AudioBufferList>();
        let (mut p, mut c) = ring(8);
        assert_eq!(unsafe { take_input(ptr, (0, 1), &mut p) }, 2);
        assert_eq!(c.pop(), Some([1.0, -1.0]));
        assert_eq!(c.pop(), Some([2.0, -2.0]));
        // a stream that is switched off has no data
        let off = List2 { n: 2, b: [buf(1, &mut a), AudioBuffer { mNumberChannels: 1, mDataByteSize: 8, mData: std::ptr::null_mut() }] };
        assert!(unsafe { channel((&off as *const List2).cast(), 1) }.is_none());
        assert!(unsafe { channel((&off as *const List2).cast(), 2) }.is_none());
        // nothing at all
        assert_eq!(unsafe { take_input(std::ptr::null(), (0, 1), &mut p) }, 0);
        assert_eq!(unsafe { channels(std::ptr::null()) }, 0);
    }

    #[test]
    fn output_into_stereo_mono_and_wider_devices() {
        let (l, r) = ([0.5f32, 0.25], [-0.5f32, 1.0]);
        // stereo, interleaved, written from frame 1 on
        let mut d = [0.0f32; 6];
        let mut list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(2, &mut d)] };
        assert_eq!(unsafe { output_frames(&list) }, 3);
        unsafe { put_output(&mut list, (0, 1), 1, &l, &r) };
        assert_eq!(d, [0.0, 0.0, 0.5, -0.5, 0.25, 1.0]);
        // mono: the average
        let mut m = [0.0f32; 2];
        let mut list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(1, &mut m)] };
        unsafe { put_output(&mut list, (0, 1), 0, &l, &r) };
        assert_eq!(m, [0.0, 0.625]);
        // four channels: the others stay as the HAL left them (zeroed)
        let mut w = [0.0f32; 8];
        let mut list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(4, &mut w)] };
        unsafe { put_output(&mut list, (0, 1), 0, &l, &r) };
        assert_eq!(w, [0.5, -0.5, 0.0, 0.0, 0.25, 1.0, 0.0, 0.0]);
        // never past the end of the buffer
        let mut s = [0.0f32; 2];
        let mut list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(2, &mut s)] };
        unsafe { put_output(&mut list, (0, 1), 0, &l, &r) };
        assert_eq!(s, [0.5, -0.5]);
    }

    #[test]
    fn stereo_pairs() {
        assert_eq!(stereo_pair(Some([1, 2]), 2), (0, 1));
        assert_eq!(stereo_pair(Some([3, 4]), 8), (2, 3), "speakers on 3 and 4");
        assert_eq!(stereo_pair(Some([2, 1]), 2), (1, 0), "swapped, as chosen");
        assert_eq!(stereo_pair(None, 2), (0, 1));
        assert_eq!(stereo_pair(Some([1, 1]), 1), (0, 1), "mono: its only channel");
        assert_eq!(stereo_pair(Some([3, 4]), 2), (0, 1), "channels it doesn't have");
        assert_eq!(stereo_pair(Some([0, 1]), 2), (0, 1));
    }

    #[test]
    fn a_chosen_stereo_pair() {
        // four channels, interleaved: frame i is [10i, 10i+1, 10i+2, 10i+3]
        let mut d: Vec<f32> = (0..2).flat_map(|i| (0..4).map(move |c| 10.0 * i as f32 + c as f32)).collect();
        let list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(4, &mut d)] };
        let (mut p, mut c) = ring(8);
        assert_eq!(unsafe { take_input(&list, (2, 3), &mut p) }, 2);
        assert_eq!(c.pop(), Some([2.0, 3.0]));
        assert_eq!(c.pop(), Some([12.0, 13.0]));
        // out: only the pair is written
        let (l, r) = ([0.5f32, 0.25], [-0.5f32, 1.0]);
        let mut w = [0.0f32; 8];
        let mut list = AudioBufferList { mNumberBuffers: 1, mBuffers: [buf(4, &mut w)] };
        unsafe { put_output(&mut list, (2, 3), 0, &l, &r) };
        assert_eq!(w, [0.0, 0.0, 0.5, -0.5, 0.0, 0.0, 0.25, 1.0]);
        // a pair in the second of two stereo streams
        let (mut s1, mut s2) = ([0.0f32; 4], [0.0f32; 4]);
        let mut list = List2 { n: 2, b: [buf(2, &mut s1), buf(2, &mut s2)] };
        unsafe { put_output((&mut list as *mut List2).cast(), (3, 2), 0, &l, &r) };
        assert_eq!((s1, s2), ([0.0; 4], [-0.5, 0.5, 1.0, 0.25]), "R on channel 3, L on channel 4");
    }

    #[test]
    fn stream_usage_layout() {
        // as the SDK declares it (one stream)
        #[repr(C)]
        struct Usage {
            io_proc: *const c_void,
            n: u32,
            on: [u32; 1],
        }
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::offset_of!(Usage, on), 12);
        let fake = 0x1122_3344_5566_7788usize as *const c_void;
        let (w, size) = streams_off_usage(fake, 3);
        assert_eq!(size, std::mem::offset_of!(Usage, on) + 3 * 4);
        assert!(w.len() * 8 >= size);
        // SAFETY: `size` bytes of `w`.
        let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr().cast::<u8>(), size) };
        assert_eq!(bytes[..8], (fake as usize).to_ne_bytes());
        assert_eq!(bytes[8..12], 3u32.to_ne_bytes());
        assert!(bytes[12..].iter().all(|&b| b == 0), "every stream off");
        // SAFETY: the words hold at least one Usage.
        let u = unsafe { &*w.as_ptr().cast::<Usage>() };
        assert_eq!((u.io_proc, u.n, u.on[0]), (fake, 3, 0));
    }
}
