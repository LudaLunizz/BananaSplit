//! Decisions of the Core Audio backend that need no Core Audio: they take
//! plain numbers and strings, so they compile and are tested on every
//! platform (`cargo test --lib`), while `mac/` (macOS only) calls them with
//! what the HAL reports.
//!
//! Nothing here allocates except the functions that build text, so the
//! ones the IOProcs or the device setup call stay cheap.

use crate::ring::MAX_RATIO;
use std::fmt::Write as _;
use std::time::Duration;

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

/// May BananaSplit record from microphones (`AVAuthorizationStatus` for
/// audio)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Authorized,
    /// The user said no in System Settings: every input reads as silence.
    Denied,
    /// A policy (Screen Time, device management) says no.
    Restricted,
    /// Not asked yet: the first input BananaSplit starts asks.
    NotDetermined,
}

/// The `AVAuthorizationStatus` integer as an [`Access`] (values from the
/// SDK's AVCaptureDevice.h: 0 not determined, 1 restricted, 2 denied,
/// 3 authorized). An unknown value counts as not determined.
pub fn access_from_status(status: isize) -> Access {
    match status {
        3 => Access::Authorized,
        2 => Access::Denied,
        1 => Access::Restricted,
        _ => Access::NotDetermined,
    }
}

/// The status line while a device start waits for the user to answer
/// macOS's microphone question.
pub const WAITING_FOR_MIC: &str = "Waiting for microphone permission: answer macOS's question to start the audio";

/// The status line once that start is done (it replaces the waiting text).
pub const STARTED_AFTER_WAIT: &str = "The audio devices are started";

/// What to tell the user about microphone access, when it keeps the
/// inputs silent (Denied or Restricted); None otherwise.
pub fn access_note(access: Access) -> Option<&'static str> {
    match access {
        Access::Denied => Some(
            "macOS doesn't let BananaSplit record (every input is silent): allow it in System Settings › Privacy & Security › Microphone › BananaSplit, then restart BananaSplit. If it is allowed there but every input stays silent after an update, run tccutil reset Microphone io.github.ludalunizz.bananasplit in the Terminal and start BananaSplit again",
        ),
        Access::Restricted => Some(
            "a policy on this Mac (Screen Time or device management) doesn't let BananaSplit record (every input is silent): see System Settings › Privacy & Security › Microphone › BananaSplit, or ask whoever manages this Mac. If it is allowed there but every input stays silent after an update, run tccutil reset Microphone io.github.ludalunizz.bananasplit in the Terminal and start BananaSplit again",
        ),
        Access::Authorized | Access::NotDetermined => None,
    }
}

/// When to try a failed device start again: 1 s, 2 s, 4 s, ... up to 30 s
/// between attempts, from the first again after `reset` (the devices
/// changed, or a start worked).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    /// The longest wait between two attempts.
    pub const MAX: Duration = Duration::from_secs(30);

    /// One more failed attempt: how long to wait before the next.
    pub fn next_delay(&mut self) -> Duration {
        let secs = 1u64.checked_shl(self.failures).unwrap_or(u64::MAX);
        self.failures = self.failures.saturating_add(1);
        Duration::from_secs(secs).min(Self::MAX)
    }

    pub fn reset(&mut self) {
        self.failures = 0;
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

/// The shape of a device's buffer list in the direction an IOProc uses,
/// recorded when the IOProc starts: the channels in each buffer, and which
/// buffers hold the stereo pair it reads or writes (those must have data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    chans: Box<[u32]>,
    used: Box<[bool]>,
}

impl Layout {
    /// From the channels per buffer (the device's stream configuration)
    /// and the stereo pair (zero-based channels across all buffers; a
    /// channel the device doesn't have marks nothing, as a mono device's
    /// second channel).
    pub fn new(chans: &[u32], pair: (usize, usize)) -> Layout {
        let mut used = vec![false; chans.len()];
        let mut first = 0usize;
        for (k, &n) in chans.iter().enumerate() {
            let range = first..first + n as usize;
            used[k] = range.contains(&pair.0) || range.contains(&pair.1);
            first = range.end;
        }
        Layout { chans: chans.into(), used: used.into_boxed_slice() }
    }
}

/// Does this cycle's buffer list look as it did at start? `got` gives each
/// buffer's channel count and whether it has data. The same number of
/// buffers, the same channels in each, and data in every buffer that holds
/// the stereo pair. Called in the IOProc: does not allocate.
pub fn layout_matches(want: &Layout, got: impl ExactSizeIterator<Item = (u32, bool)>) -> bool {
    got.len() == want.chans.len()
        && got.zip(want.chans.iter().zip(want.used.iter())).all(|((n, data), (&w, &used))| n == w && (data || !used))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- layout_matches
    fn check(want: &Layout, got: &[(u32, bool)]) -> bool {
        layout_matches(want, got.iter().copied())
    }

    #[test]
    fn layout_matching_list_passes() {
        let want = Layout::new(&[2, 2], (0, 1));
        assert!(check(&want, &[(2, true), (2, true)]));
    }

    #[test]
    fn layout_fewer_or_more_buffers_fail() {
        let want = Layout::new(&[2, 2], (0, 1));
        assert!(!check(&want, &[(2, true)]));
        assert!(!check(&want, &[]));
        assert!(!check(&want, &[(2, true), (2, true), (2, true)]));
    }

    #[test]
    fn layout_wrong_channel_count_fails() {
        let want = Layout::new(&[2, 2], (0, 1));
        assert!(!check(&want, &[(2, true), (1, true)]));
        assert!(!check(&want, &[(8, true), (2, true)]));
    }

    #[test]
    fn layout_null_data_fails_only_where_the_pair_is() {
        // the pair is in the first buffer: the second may be switched off
        let want = Layout::new(&[2, 2], (0, 1));
        assert!(!check(&want, &[(2, false), (2, true)]));
        assert!(check(&want, &[(2, true), (2, false)]));
        // a pair across two mono buffers needs both
        let split = Layout::new(&[1, 1, 1], (1, 2));
        assert!(!check(&split, &[(1, true), (1, true), (1, false)]));
        assert!(!check(&split, &[(1, true), (1, false), (1, true)]));
        assert!(check(&split, &[(1, false), (1, true), (1, true)]));
    }

    #[test]
    fn layout_mono_device_and_empty_list() {
        // a mono device: the pair's second channel doesn't exist
        let mono = Layout::new(&[1], (0, 1));
        assert!(check(&mono, &[(1, true)]));
        assert!(!check(&mono, &[(1, false)]));
        // nothing recorded at start: only an empty list matches
        let none = Layout::new(&[], (0, 1));
        assert!(check(&none, &[]));
        assert!(!check(&none, &[(2, true)]));
    }

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

    // ---- access_from_status, access_note
    #[test]
    fn access_each_known_status() {
        assert_eq!(access_from_status(0), Access::NotDetermined);
        assert_eq!(access_from_status(1), Access::Restricted);
        assert_eq!(access_from_status(2), Access::Denied);
        assert_eq!(access_from_status(3), Access::Authorized);
    }

    #[test]
    fn access_unknown_status_is_not_determined() {
        assert_eq!(access_from_status(4), Access::NotDetermined);
        assert_eq!(access_from_status(-1), Access::NotDetermined);
        assert_eq!(access_from_status(isize::MAX), Access::NotDetermined);
    }

    #[test]
    fn access_note_only_when_inputs_are_silenced() {
        assert_eq!(access_note(Access::Authorized), None);
        assert_eq!(access_note(Access::NotDetermined), None);
        for a in [Access::Denied, Access::Restricted] {
            let n = access_note(a).expect("a note");
            assert!(n.contains("System Settings › Privacy & Security › Microphone › BananaSplit"));
            assert!(n.contains("tccutil reset Microphone io.github.ludalunizz.bananasplit"));
        }
    }

    // ---- Backoff
    #[test]
    fn backoff_first_attempt_waits_one_second_then_doubles() {
        let mut b = Backoff::default();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        assert_eq!(b.next_delay(), Duration::from_secs(4));
        assert_eq!(b.next_delay(), Duration::from_secs(8));
        assert_eq!(b.next_delay(), Duration::from_secs(16));
    }

    #[test]
    fn backoff_caps_at_thirty_seconds_and_never_overflows() {
        let mut b = Backoff::default();
        for _ in 0..5 {
            b.next_delay();
        }
        assert_eq!(b.next_delay(), Backoff::MAX);
        for _ in 0..200 {
            assert_eq!(b.next_delay(), Duration::from_secs(30));
        }
    }

    #[test]
    fn backoff_reset_starts_over() {
        let mut b = Backoff::default();
        for _ in 0..10 {
            b.next_delay();
        }
        b.reset();
        assert_eq!(b, Backoff::default());
        assert_eq!(b.next_delay(), Duration::from_secs(1));
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
