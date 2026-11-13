//! A process tap: one app's sound, captured through Core Audio (macOS 14.4
//! and later), into a lock-free ring. Not connected to the mixer or the
//! app yet: only `bananasplit --tap-test <pid>` uses it.
//!
//! ```text
//!  app processes ─ CATapDescription (private, stereo mixdown, muted while tapped)
//!        └─ tap ─ private aggregate device (the default output = clock + sub-device,
//!                   the tap in its tap list, drift compensation on)
//!                    └─ IOProc (coreaudio::launch, guarded) ─ ring ─ reader
//! ```
//!
//! The tap's channels come after the aggregate's physical input channels
//! (`pure::tap_channel_offset`), so a microphone on the output device is
//! skipped.
//!
//! Every Apple name used here was confirmed in two places: (A) the
//! objc2-core-audio 0.3.2 / objc2-core-foundation 0.3.2 generated sources
//! in ~/.cargo/registry, (B) the macOS 14.5 SDK headers
//! (CoreAudio.framework: AudioHardware.h, AudioHardwareTapping.h,
//! CATapDescription.h; CoreFoundation.framework: CFArray.h,
//! CFDictionary.h, CFNumber.h).
//!
//! | Name | Value / signature | A | B |
//! |---|---|---|---|
//! | `AudioHardwareCreateProcessTap` | `(CATapDescription*, AudioObjectID*) -> OSStatus`, macOS 14.2 | AudioHardware.rs:1794 | AudioHardwareTapping.h:43 |
//! | `AudioHardwareDestroyProcessTap` | `(AudioObjectID) -> OSStatus` | AudioHardware.rs:1806 | AudioHardwareTapping.h:54 |
//! | `AudioHardwareCreateAggregateDevice` | `(CFDictionaryRef, AudioObjectID*) -> OSStatus` | AudioHardware.rs:1114 | AudioHardware.h:667 |
//! | `AudioHardwareDestroyAggregateDevice` | `(AudioObjectID) -> OSStatus` | AudioHardware.rs:1129 | AudioHardware.h:680 |
//! | class `CATapDescription` (NSObject) | | AudioHardware.rs:1852 | CATapDescription.h:45 |
//! | `-initStereoMixdownOfProcesses:` | `NSArray<NSNumber*>*` of process objects | AudioHardware.rs:1872 | CATapDescription.h:56 |
//! | `-setPrivate:` | `BOOL` (property `privateTap`) | AudioHardware.rs:2045 | CATapDescription.h:153 |
//! | `-setMuteBehavior:` | `CATapMuteBehavior` (NSInteger) | AudioHardware.rs:2065 | CATapDescription.h:159 |
//! | `CATapMutedWhenTapped` | 2 | AudioHardware.rs:1831 | CATapDescription.h:33 |
//! | `kAudioTapPropertyUID` | 'tuid', CFString | AudioHardware.rs:1774 | AudioHardware.h:2020 |
//! | `kAudioTapPropertyFormat` | 'tfmt', AudioStreamBasicDescription | AudioHardware.rs:1778 | AudioHardware.h:2022 |
//! | aggregate keys `uid` `name` `subdevices` `master` `private` `taps` | strings | AudioHardware.rs:46-70 | AudioHardware.h:1557-1623 |
//! | sub-device `uid`, sub-tap `uid`, `drift` | strings | AudioHardware.rs:73, 95, 103 | AudioHardware.h:1747, 1857, 1883 |
//! | `CFArrayCreate`, `kCFTypeArrayCallBacks` | `(alloc, const void**, CFIndex, const CFArrayCallBacks*)` | CFArray.rs:256, 84 | CFArray.h:174, 92 |
//! | `CFDictionaryCreate`, key/value callbacks | `(alloc, keys, values, CFIndex, key cb*, value cb*)` | CFDictionary.rs:381, 94, 166 | CFDictionary.h:284, 118, 169 |
//! | `CFNumberCreate`, `kCFNumberSInt64Type` | `(alloc, CFNumberType, const void*)`, 4 | CFNumber.rs:172, 76 | CFNumber.h:74, 36 |
//! | `kCFBooleanTrue` | `CFBooleanRef` | CFNumber.rs:32 | CFNumber.h:21 |
//!
//! `+alloc` and `-release` are NSObject's own messages. Nothing here has run
//! on a Mac: it is type-checked only.

use super::catypes::{self, AudioBufferList, AudioObjectID};
use super::coreaudio::{Cycle, Device, Guarded, Proc, Stats, launch};
use super::ffi;
use super::route::DevInfo;
use bananasplit::pure;
use bananasplit::ring::{Consumer, Producer, ring};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A tap this process created; destroyed when dropped.
struct Tap(AudioObjectID);

impl Drop for Tap {
    fn drop(&mut self) {
        ffi::destroy_process_tap(self.0);
    }
}

/// An aggregate device this process created; destroyed when dropped.
struct Aggregate(AudioObjectID);

impl Drop for Aggregate {
    fn drop(&mut self) {
        ffi::destroy_aggregate(self.0);
    }
}

/// The IOProc's state: the tap's stereo pair into the ring.
struct TapCapture {
    prod: Producer,
    pair: (usize, usize),
}

impl Cycle for TapCapture {
    unsafe fn cycle(&mut self, input: *const AudioBufferList, _: *mut AudioBufferList) {
        // SAFETY: this cycle's input from the HAL, as the trait promises.
        unsafe { catypes::take_input(input, self.pair, &mut self.prod) };
    }
}

fn check_os() -> Result<(), String> {
    let (major, minor) = pure::TAP_OS;
    match ffi::os_version() {
        Some((have_major, have_minor)) if pure::os_at_least(major, minor, have_major, have_minor) => Ok(()),
        _ => Err(pure::TAP_TOO_OLD.into()),
    }
}

/// One app's sound, captured. Built in this order: the tap, the aggregate
/// device, the IOProc; an error on the way drops what was made so far.
/// The fields are dropped in their order, the reverse: the IOProc first
/// (stopped and removed, `Proc`'s teardown rules), then the aggregate
/// device, then the tap.
pub struct TapRoute {
    // held only to be torn down, in this order, when the route drops
    _proc: Proc<Guarded<TapCapture>>,
    _aggregate: Aggregate,
    _tap: Tap,
    cons: Consumer,
}

impl TapRoute {
    /// Tap `processes` (Core Audio process objects) and start reading.
    pub fn start(processes: &[AudioObjectID]) -> Result<TapRoute, String> {
        check_os()?;
        if processes.is_empty() {
            return Err("no process to tap".into());
        }
        let tap = Tap(ffi::create_process_tap(processes)?);
        let tap_uid = ffi::tap_uid(tap.0).ok_or("cannot read the tap's UID")?;
        let tap_channels = ffi::tap_channels(tap.0).unwrap_or(2);
        let out = ffi::default_device(false).ok_or("there is no default output device to clock the tap")?;
        let out_uid = ffi::uid(out).ok_or("cannot read the default output device's UID")?;
        let uid = format!("io.github.ludalunizz.bananasplit.tap.{}.{}", std::process::id(), tap.0);
        let name = "BananaSplit app tap";
        let aggregate = Aggregate(ffi::create_private_aggregate(&uid, name, &out_uid, &tap_uid)?);
        let inputs = ffi::channel_count(aggregate.0, true);
        let offset = pure::tap_channel_offset(inputs, tap_channels)
            .ok_or_else(|| format!("the aggregate device has {inputs} inputs, fewer than the tap's {tap_channels}"))?;
        let pair = pure::tap_pair(offset, tap_channels);
        let rate = ffi::nominal_rate(aggregate.0);
        pure::check_rate(rate)?;
        let dev = Device {
            info: DevInfo { name: name.into(), id: uid, virt: true },
            id: aggregate.0,
            inputs,
            outputs: ffi::channel_count(aggregate.0, false),
            rate,
        };
        // a second of audio: the reader takes it every few milliseconds
        let (prod, cons) = ring(rate.round() as usize);
        let stats = Arc::new(Stats::default());
        let proc = launch(&dev, TapCapture { prod, pair }, pair, true, &stats)
            .map_err(|(e, _)| format!("cannot start reading the tap: {e}"))?;
        Ok(TapRoute { _proc: proc, _aggregate: aggregate, _tap: tap, cons })
    }

    /// The next captured stereo frame, if there is one.
    pub fn pop(&mut self) -> Option<[f32; 2]> {
        self.cons.pop()
    }
}

/// `bananasplit --tap-test <pid>`: tap the app that process belongs to
/// (with its helper processes, grouped as in `--status`) for 3 seconds;
/// the result line, or why it failed. Never starts the mixer.
pub fn tap_test(pid: u32) -> Result<String, String> {
    check_os()?;
    let procs = ffi::audio_processes();
    let list: Vec<(u32, String)> = procs
        .iter()
        .map(|p| (p.pid as u32, p.path.clone().or_else(|| p.bundle_id.clone()).unwrap_or_default()))
        .collect();
    let group = pure::group_by_app(&list)
        .into_iter()
        .find(|g| g.pids.contains(&pid))
        .ok_or_else(|| format!("process {pid} is not in Core Audio's process list: start its sound first"))?;
    let ids: Vec<AudioObjectID> = procs.iter().filter(|p| group.pids.contains(&(p.pid as u32))).map(|p| p.id).collect();
    let mut route = TapRoute::start(&ids)?;
    let (mut frames, mut peak) = (0u64, 0.0f32);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
        while let Some([l, r]) = route.pop() {
            frames += 1;
            peak = peak.max(l.abs()).max(r.abs());
        }
    }
    drop(route);
    Ok(pure::tap_test_line(frames, peak))
}
