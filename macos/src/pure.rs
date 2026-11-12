//! Decisions of the Core Audio backend that need no Core Audio: they take
//! plain numbers and strings, so they compile and are tested on every
//! platform (`cargo test --lib`), while `mac/` (macOS only) calls them with
//! what the HAL reports.
//!
//! Nothing here allocates except the functions that build text, so the
//! ones the IOProcs or the device setup call stay cheap.

use crate::ring::MAX_RATIO;
use std::fmt::Write as _;

/// The mixer's rate.
pub const RATE: u32 = 48_000;
/// Extra frames of safety margin in each ring on top of the two periods
/// (at 48 kHz).
pub const RING_MARGIN: usize = 96;

/// `frames` at `rate`, in frames of the mixer (rounded up). A rate that is
/// not known (0) leaves them as they are.
pub fn to_mixer(frames: u32, rate: f64) -> u32 {
    if rate > 0.0 { (f64::from(frames) * f64::from(RATE) / rate).ceil() as u32 } else { frames }
}

/// Can BananaSplit run a device at this rate? Why not, if it can't: an
/// unknown rate, or one too far from the mixer's for the resampler.
pub fn check_rate(rate: f64) -> Result<(), String> {
    if rate <= 0.0 {
        return Err("its sample rate is unknown".into());
    }
    let ratio = rate / f64::from(RATE);
    if !(1.0 / MAX_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!("it runs at {} kHz: set it to 48 kHz in Audio MIDI Setup", rate / 1000.0));
    }
    Ok(())
}

/// One direction of a device in its signature: '-' when it has no
/// channels that way, else 'f' when it hands over 32-bit floats and 'x'
/// when not. `float32` is only asked when there are channels.
pub fn format_mark(channels: u32, float32: impl FnOnce() -> bool) -> char {
    if channels == 0 {
        '-'
    } else if float32() {
        'f'
    } else {
        'x'
    }
}

/// What the signature records of one device.
#[derive(Debug, Clone, PartialEq)]
pub struct SigDevice<'a> {
    pub uid: &'a str,
    /// The HAL's object ID (a new one when the device comes back).
    pub id: u32,
    pub name: &'a str,
    pub inputs: u32,
    pub outputs: u32,
    pub rate: f64,
    /// `format_mark` of the inputs and of the outputs.
    pub in_mark: char,
    pub out_mark: char,
}

/// The devices and the defaults as one text: a line per device, then the
/// default input's and output's UIDs. Any change to what BananaSplit uses
/// of a device changes it.
pub fn signature_text(devs: &[SigDevice<'_>], def_in: Option<&str>, def_out: Option<&str>) -> String {
    let mut s = String::new();
    for d in devs {
        let _ = writeln!(
            s,
            "{}#{}={} {}/{} {} {}{}",
            d.uid, d.id, d.name, d.inputs, d.outputs, d.rate, d.in_mark, d.out_mark
        );
    }
    let _ = writeln!(s, "in {}", def_in.unwrap_or_default());
    let _ = writeln!(s, "out {}", def_out.unwrap_or_default());
    s
}

/// The output that clocks the mixer: the first one that is not `bench`
/// (an output that stalled while it clocked it).
pub fn pick_master<'a>(uids: impl IntoIterator<Item = &'a str>, bench: Option<&str>) -> Option<usize> {
    uids.into_iter().position(|u| bench != Some(u))
}

/// The fill an output's ring (mixer -> device) aims at, in mixer frames:
/// the mixer's period, the device's own, and a margin.
pub fn sink_target(period: usize, dev_frames: u32, rate: f64) -> usize {
    period + to_mixer(dev_frames, rate) as usize + RING_MARGIN
}

/// An input's ring (device -> mixer), in the device's frames: the fill it
/// aims at (the mixer's period and the margin scaled to the device's rate,
/// plus the device's own period), and the ring's length.
pub fn capture_ring(period: usize, dev_frames: u32, rate: f64) -> (usize, usize) {
    let scale = rate / f64::from(RATE);
    let target =
        (period as f64 * scale).ceil() as usize + dev_frames as usize + (RING_MARGIN as f64 * scale).ceil() as usize;
    let len = (rate.round() as u32 as usize).max(RATE as usize).max(4 * target);
    (target, len)
}

/// One stall check over the running IOProcs. `last` keeps, per IOProc,
/// its cycle count at the previous check and for how many checks in a row
/// it stood still (it is reset when the number of IOProcs changed);
/// `calls` are the counts now. Returns the IOProc that stood still for two
/// checks in a row: the master (`master_at`) if it is one of them, else the
/// first.
pub fn stall_check(
    last: &mut Vec<(u64, u32)>,
    calls: impl ExactSizeIterator<Item = u64>,
    master_at: Option<usize>,
) -> Option<usize> {
    if last.len() != calls.len() {
        *last = vec![(u64::MAX, 0); calls.len()];
    }
    let (mut master, mut other) = (None, None);
    for (k, (calls, (seen, still))) in calls.zip(last.iter_mut()).enumerate() {
        if calls == *seen {
            *still += 1;
            if *still >= 2 {
                let found = if master_at == Some(k) { &mut master } else { &mut other };
                if found.is_none() {
                    *found = Some(k);
                }
            }
        } else {
            *seen = calls;
            *still = 0;
        }
    }
    master.or(other)
}

/// The IO cycle to ask a device for: `frames` within the range it allows
/// (`range` = its minimum and maximum). None: leave it as it is (0 asked
/// for, no range, or a range that makes no sense, where clamp would panic).
pub fn buffer_request(frames: u32, range: Option<(f64, f64)>) -> Option<u32> {
    if frames == 0 {
        return None;
    }
    let (min, max) = range.filter(|(min, max)| min.is_finite() && max.is_finite() && min <= max)?;
    Some(f64::from(frames).clamp(min.max(1.0), max.max(1.0)).round() as u32)
}

/// `AVAuthorizationStatus` for the microphone: Some(true) allowed,
/// Some(false) denied or restricted (inputs read as silence), None not
/// asked yet.
pub fn mic_status(status: isize) -> Option<bool> {
    match status {
        3 => Some(true),      // AVAuthorizationStatusAuthorized
        1 | 2 => Some(false), // Restricted, Denied
        _ => None,            // NotDetermined
    }
}

/// One device for `--status`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusDevice {
    pub name: String,
    pub uid: String,
    pub rate: f64,
    pub inputs: u32,
    pub outputs: u32,
}

/// `bananasplit --status`: a line per device (name, UID, rate, input and
/// output channels), then the default output and input, or "no devices".
pub fn status_report(devs: &[StatusDevice], def_out: Option<&str>, def_in: Option<&str>) -> String {
    if devs.is_empty() {
        return "no devices\n".into();
    }
    let mut s = String::new();
    for d in devs {
        let rate = if d.rate > 0.0 { format!("{} Hz", d.rate) } else { "rate unknown".into() };
        let _ = writeln!(s, "{}  uid={}  {rate}  in {} ch  out {} ch", d.name, d.uid, d.inputs, d.outputs);
    }
    let named = |uid: Option<&str>| match uid {
        None => "(none)".to_string(),
        Some(u) => match devs.iter().find(|d| d.uid == u) {
            Some(d) => format!("{} (uid={u})", d.name),
            None => format!("uid={u}"),
        },
    };
    let _ = writeln!(s, "default output: {}", named(def_out));
    let _ = writeln!(s, "default input: {}", named(def_in));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- to_mixer
    #[test]
    fn to_mixer_same_rate_is_unchanged() {
        assert_eq!(to_mixer(512, 48_000.0), 512);
        assert_eq!(to_mixer(0, 48_000.0), 0);
    }

    #[test]
    fn to_mixer_rounds_up_across_rates() {
        // 441 frames at 44.1 kHz = 480 at 48 kHz exactly
        assert_eq!(to_mixer(441, 44_100.0), 480);
        // 512 at 44.1 kHz = 557.27.. -> 558
        assert_eq!(to_mixer(512, 44_100.0), 558);
        // 512 at 96 kHz = 256
        assert_eq!(to_mixer(512, 96_000.0), 256);
        assert_eq!(to_mixer(1, 96_000.0), 1);
    }

    #[test]
    fn to_mixer_unknown_rate_leaves_frames() {
        assert_eq!(to_mixer(512, 0.0), 512);
        assert_eq!(to_mixer(512, -1.0), 512);
        // NaN is not > 0 either
        assert_eq!(to_mixer(512, f64::NAN), 512);
    }

    // ---- check_rate
    #[test]
    fn check_rate_accepts_common_rates() {
        for r in [8_000.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0, 384_000.0] {
            assert!(check_rate(r).is_ok(), "{r}");
        }
    }

    #[test]
    fn check_rate_unknown() {
        assert_eq!(check_rate(0.0).unwrap_err(), "its sample rate is unknown");
        assert_eq!(check_rate(-48_000.0).unwrap_err(), "its sample rate is unknown");
    }

    #[test]
    fn check_rate_out_of_range_names_the_rate() {
        // 48 kHz / 8 = 6 kHz is the floor, 48 kHz * 8 = 384 kHz the ceiling
        assert!(check_rate(6_000.0).is_ok());
        assert_eq!(check_rate(5_999.0).unwrap_err(), "it runs at 5.999 kHz: set it to 48 kHz in Audio MIDI Setup");
        assert!(check_rate(384_001.0).unwrap_err().starts_with("it runs at 384.001 kHz"));
        // NaN passes the "unknown" test and fails the range test, as before
        assert!(check_rate(f64::NAN).unwrap_err().starts_with("it runs at NaN kHz"));
    }

    // ---- format_mark
    #[test]
    fn format_mark_no_channels_never_asks() {
        assert_eq!(format_mark(0, || panic!("asked about a direction it does not have")), '-');
    }

    #[test]
    fn format_mark_float_or_not() {
        assert_eq!(format_mark(2, || true), 'f');
        assert_eq!(format_mark(2, || false), 'x');
    }

    #[test]
    fn format_mark_asks_once() {
        let mut asked = 0;
        let m = format_mark(64, || {
            asked += 1;
            true
        });
        assert_eq!((m, asked), ('f', 1));
    }

    // ---- signature_text
    fn sig(uid: &'static str, id: u32, rate: f64) -> SigDevice<'static> {
        SigDevice { uid, id, name: "Speakers", inputs: 0, outputs: 2, rate, in_mark: '-', out_mark: 'f' }
    }

    #[test]
    fn signature_text_layout() {
        let s = signature_text(&[sig("BuiltIn", 42, 48_000.0)], Some("Mic"), Some("BuiltIn"));
        assert_eq!(s, "BuiltIn#42=Speakers 0/2 48000 -f\nin Mic\nout BuiltIn\n");
    }

    #[test]
    fn signature_text_no_devices_no_defaults() {
        assert_eq!(signature_text(&[], None, None), "in \nout \n");
    }

    #[test]
    fn signature_text_changes_with_what_matters() {
        let base = signature_text(&[sig("U", 1, 48_000.0)], None, Some("U"));
        // replugged: same UID, new object ID
        assert_ne!(base, signature_text(&[sig("U", 2, 48_000.0)], None, Some("U")));
        // rate changed in Audio MIDI Setup
        assert_ne!(base, signature_text(&[sig("U", 1, 44_100.0)], None, Some("U")));
        // switched away from float
        let mut x = sig("U", 1, 48_000.0);
        x.out_mark = 'x';
        assert_ne!(base, signature_text(&[x], None, Some("U")));
        // default output changed
        assert_ne!(base, signature_text(&[sig("U", 1, 48_000.0)], None, None));
        // nothing changed
        assert_eq!(base, signature_text(&[sig("U", 1, 48_000.0)], None, Some("U")));
    }

    // ---- pick_master
    #[test]
    fn pick_master_first_without_bench() {
        assert_eq!(pick_master(["a", "b"], None), Some(0));
    }

    #[test]
    fn pick_master_skips_the_benched_device() {
        assert_eq!(pick_master(["a", "b", "c"], Some("a")), Some(1));
        assert_eq!(pick_master(["a", "b", "c"], Some("b")), Some(0));
    }

    #[test]
    fn pick_master_none_left() {
        assert_eq!(pick_master([], None), None);
        // the only output is the benched one: the timer clocks the mixer
        assert_eq!(pick_master(["a"], Some("a")), None);
    }

    // ---- sink_target / capture_ring
    #[test]
    fn sink_target_adds_period_device_and_margin() {
        assert_eq!(sink_target(512, 512, 48_000.0), 512 + 512 + RING_MARGIN);
        assert_eq!(sink_target(480, 512, 96_000.0), 480 + 256 + RING_MARGIN);
        // unknown rate: the device's frames as they are
        assert_eq!(sink_target(480, 100, 0.0), 480 + 100 + RING_MARGIN);
    }

    #[test]
    fn capture_ring_at_mixer_rate() {
        let (target, len) = capture_ring(512, 512, 48_000.0);
        assert_eq!(target, 512 + 512 + RING_MARGIN);
        // a second of audio at least
        assert_eq!(len, 48_000);
    }

    #[test]
    fn capture_ring_scales_to_the_device_rate() {
        // 96 kHz: the mixer's period and the margin double
        let (target, len) = capture_ring(480, 1024, 96_000.0);
        assert_eq!(target, 960 + 1024 + 2 * RING_MARGIN);
        assert_eq!(len, 96_000);
        // 44.1 kHz: rounded up
        let (target, _) = capture_ring(480, 512, 44_100.0);
        assert_eq!(target, 441 + 512 + 89);
    }

    #[test]
    fn capture_ring_huge_buffer_sets_the_length() {
        // a device left at a 16k buffer: four targets beat a second
        let (target, len) = capture_ring(16_384, 16_384, 48_000.0);
        assert_eq!(len, 4 * target);
        assert!(len > 48_000);
    }

    // ---- stall_check
    #[test]
    fn stall_check_running_never_stalls() {
        let mut last = Vec::new();
        for t in 1..10u64 {
            assert_eq!(stall_check(&mut last, [t * 10, t * 20].into_iter(), Some(0)), None);
        }
    }

    #[test]
    fn stall_check_needs_two_still_checks() {
        let mut last = Vec::new();
        assert_eq!(stall_check(&mut last, [5, 7].into_iter(), None), None); // first look
        assert_eq!(stall_check(&mut last, [6, 7].into_iter(), None), None); // still once
        assert_eq!(stall_check(&mut last, [8, 7].into_iter(), None), Some(1)); // still twice
        // it runs again: forgotten
        assert_eq!(stall_check(&mut last, [9, 8].into_iter(), None), None);
    }

    #[test]
    fn stall_check_master_first() {
        let mut last = Vec::new();
        for _ in 0..3 {
            stall_check(&mut last, [1, 1, 1].into_iter(), Some(2));
        }
        assert_eq!(stall_check(&mut last, [1, 1, 1].into_iter(), Some(2)), Some(2));
        assert_eq!(stall_check(&mut last, [1, 1, 1].into_iter(), None), Some(0));
    }

    #[test]
    fn stall_check_resets_when_the_graph_changes() {
        let mut last = Vec::new();
        stall_check(&mut last, [1].into_iter(), None);
        stall_check(&mut last, [1].into_iter(), None);
        // rebuilt with two IOProcs mid-way: counting starts over
        assert_eq!(stall_check(&mut last, [1, 1].into_iter(), None), None);
        assert_eq!(last.len(), 2);
        // no IOProcs at all
        assert_eq!(stall_check(&mut last, std::iter::empty(), None), None);
        assert!(last.is_empty());
    }

    // ---- buffer_request
    #[test]
    fn buffer_request_within_range() {
        assert_eq!(buffer_request(256, Some((14.0, 4096.0))), Some(256));
        assert_eq!(buffer_request(8, Some((14.0, 4096.0))), Some(14));
        assert_eq!(buffer_request(8192, Some((14.0, 4096.0))), Some(4096));
    }

    #[test]
    fn buffer_request_zero_or_no_range() {
        assert_eq!(buffer_request(0, Some((14.0, 4096.0))), None);
        assert_eq!(buffer_request(256, None), None);
    }

    #[test]
    fn buffer_request_bad_ranges_are_left_alone() {
        assert_eq!(buffer_request(256, Some((4096.0, 14.0))), None);
        assert_eq!(buffer_request(256, Some((f64::NAN, 4096.0))), None);
        assert_eq!(buffer_request(256, Some((14.0, f64::INFINITY))), None);
        // a range below one frame: at least one
        assert_eq!(buffer_request(256, Some((0.0, 0.0))), Some(1));
        // fractional bounds are rounded
        assert_eq!(buffer_request(1, Some((14.6, 100.0))), Some(15));
    }

    // ---- mic_status
    #[test]
    fn mic_status_allowed() {
        assert_eq!(mic_status(3), Some(true));
    }

    #[test]
    fn mic_status_denied_or_restricted() {
        assert_eq!(mic_status(1), Some(false));
        assert_eq!(mic_status(2), Some(false));
    }

    #[test]
    fn mic_status_not_asked_or_unknown() {
        assert_eq!(mic_status(0), None);
        assert_eq!(mic_status(4), None);
        assert_eq!(mic_status(-1), None);
    }

    // ---- status_report
    fn dev(name: &str, uid: &str, rate: f64, inputs: u32, outputs: u32) -> StatusDevice {
        StatusDevice { name: name.into(), uid: uid.into(), rate, inputs, outputs }
    }

    #[test]
    fn status_report_lists_devices_and_defaults() {
        let d =
            [dev("MacBook Pro Speakers", "spk", 48_000.0, 0, 2), dev("MacBook Pro Microphone", "mic", 44_100.0, 1, 0)];
        assert_eq!(
            status_report(&d, Some("spk"), Some("mic")),
            "MacBook Pro Speakers  uid=spk  48000 Hz  in 0 ch  out 2 ch\n\
             MacBook Pro Microphone  uid=mic  44100 Hz  in 1 ch  out 0 ch\n\
             default output: MacBook Pro Speakers (uid=spk)\n\
             default input: MacBook Pro Microphone (uid=mic)\n"
        );
    }

    #[test]
    fn status_report_no_devices() {
        assert_eq!(status_report(&[], Some("x"), None), "no devices\n");
    }

    #[test]
    fn status_report_missing_defaults_and_unknown_rate() {
        let s = status_report(&[dev("Odd", "o", 0.0, 2, 2)], None, Some("gone"));
        assert!(s.contains("uid=o  rate unknown  in 2 ch  out 2 ch\n"));
        assert!(s.contains("default output: (none)\n"));
        assert!(s.contains("default input: uid=gone\n"));
    }
}
