//! Core Audio graph.
//!
//! ```text
//!  input device ─IOProc─ring─┐                          ┌─ring─IOProc─ output device (other bus devices)
//!  input device ─IOProc─ring─┤→ MASTER IOProc: Engine ──┤
//!  input device ─IOProc─ring─┘   (on the A1 device's    └───────────── master device (written directly)
//!                                 IO thread)
//! ```
//!
//! Every device BananaSplit uses gets an IOProc of its own
//! (`AudioDeviceCreateIOProcID`), which the HAL calls on the device's
//! real-time IO thread with the device's audio in the device's own format:
//! 32-bit floats at the rate the device runs at. As on Windows, every device
//! has a clock of its own: the mixer runs at 48 kHz in the IOProc of the
//! first bus device (normally A1), like JACK's process callback on Linux,
//! and every other device is connected through a lock-free ring read by a
//! drift-correcting resampler (`bananasplit::ring::Reader`), which also
//! converts between the device's rate and the mixer's. A master device that
//! runs at another rate gets the mix through a converter on its own IO
//! thread. Without any output device, a 10 ms timer runs the mixer, so the
//! meters, the recorder, VBAN and the virtual cables still work.
//!
//! Nothing in an IOProc allocates, locks or blocks.

#![allow(clippy::needless_range_loop)] // index loops mirror the [strip]/[bus] layout

use super::catypes::{
    self, AudioBufferList, AudioObjectID, AudioTimeStamp, OSStatus, kAudioHardwareBadDeviceError,
    kAudioHardwareBadObjectError,
};
use super::ffi::{self, IoProc, IoProcId};
use super::route::{self, DevInfo};
use bananasplit::config::{Config, DEFAULT_DEVICE};
use bananasplit::engine::{AtomicF32, BUS_IDS, Engine, NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::player::PlayTap;
use bananasplit::recorder::RecTap;
use bananasplit::ring::{MAX_RATIO, Producer, RateReader, Reader, ring};
use bananasplit::vban::VbanTap;
use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed, Ordering::SeqCst};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The mixer's rate.
pub const RATE: u32 = 48_000;
/// Largest block the mixer processes at once.
const MAX_BLOCK: usize = 8192;
/// Extra frames of safety margin in each ring on top of the two periods
/// (at 48 kHz).
const RING_MARGIN: usize = 96;

// ------------------------------------------------------------------ devices

#[derive(Debug, Clone)]
pub struct Device {
    pub info: DevInfo,
    pub id: AudioObjectID,
    /// Input and output channels (0: it has none of that direction).
    pub inputs: u32,
    pub outputs: u32,
    /// The rate it runs at (0 if unknown).
    pub rate: f64,
}

/// Every device macOS shows, in its order.
pub fn devices() -> Result<Vec<Device>, String> {
    ffi::hal_on_its_own_thread();
    let ids = ffi::device_ids().ok_or("cannot list the audio devices (is the audio system, coreaudiod, running?)")?;
    Ok(ids
        .into_iter()
        .filter_map(|id| {
            let uid = ffi::uid(id)?;
            let name = ffi::name(id).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "(unnamed device)".into());
            Some(Device {
                info: DevInfo { name, id: uid, virt: ffi::is_virtual(id) },
                id,
                inputs: ffi::channel_count(id, true),
                outputs: ffi::channel_count(id, false),
                rate: ffi::nominal_rate(id),
            })
        })
        .collect())
}

/// The UID of the default input (or output) device.
pub fn default_uid(input: bool) -> Option<String> {
    ffi::default_device(input).and_then(ffi::uid)
}

/// Changes whenever a device appears, disappears or comes back as a new
/// device object (replugged, its driver restarted: macOS gives it a new
/// ID, and IOProcs on the old one are never called again), is renamed,
/// changes its rate, its channels or its sample format (an IOProc reads
/// 32-bit floats: a device switched to another format in Audio MIDI Setup
/// is opened again, and refused if it can't be used), or a default device
/// changes. Cheap enough to poll every few seconds.
pub fn signature() -> String {
    let mut s = String::new();
    for d in devices().unwrap_or_default() {
        let float = |input: bool, n: u32| {
            if n == 0 {
                '-'
            } else if ffi::float32(d.id, input) {
                'f'
            } else {
                'x'
            }
        };
        let _ = writeln!(
            s,
            "{}#{}={} {}/{} {} {}{}",
            d.info.id,
            d.id,
            d.info.name,
            d.inputs,
            d.outputs,
            d.rate,
            float(true, d.inputs),
            float(false, d.outputs)
        );
    }
    let _ = writeln!(s, "in {}", default_uid(true).unwrap_or_default());
    let _ = writeln!(s, "out {}", default_uid(false).unwrap_or_default());
    s
}

/// `frames` at `rate`, in frames of the mixer.
fn to_mixer(frames: u32, rate: f64) -> u32 {
    if rate > 0.0 { (f64::from(frames) * f64::from(RATE) / rate).ceil() as u32 } else { frames }
}

fn zeros(n: usize) -> Box<[f32]> {
    vec![0.0; n].into_boxed_slice()
}

// ------------------------------------------------------------------ IOProcs

/// Live numbers for the UI, written by the audio threads.
#[derive(Default)]
pub struct Stats {
    pub block: AtomicU32,
    pub cpu: AtomicF32,
    pub lat_in: AtomicU32,
    pub lat_out: AtomicU32,
}

/// An IOProc's pulse: kept apart from its state, so the main thread can
/// look at it while the IOProc runs.
#[derive(Default)]
struct Beat {
    /// IO cycles so far (the stall check).
    calls: AtomicU64,
    /// In the IOProc right now.
    inside: AtomicBool,
}

impl Beat {
    #[inline]
    fn enter(&self) {
        self.inside.store(true, SeqCst);
        self.calls.fetch_add(1, Relaxed);
    }

    #[inline]
    fn leave(&self) {
        self.inside.store(false, SeqCst);
    }
}

/// One IOProc registered and started on one device, with its state.
/// Dropping it stops and removes the IOProc, then frees the state.
struct Proc<S> {
    device: AudioObjectID,
    id: IoProcId,
    state: *mut S,
    beat: Arc<Beat>,
    name: String,
    uid: String,
}

// SAFETY: while the IOProc is registered only it touches the state; after
// that only Drop does.
unsafe impl<S: Send> Send for Proc<S> {}

impl<S> Proc<S> {
    /// Register `f` with `state` on the device and start it. `input`: it
    /// records the device; otherwise it plays on it. On failure the state
    /// comes back (it was never used).
    fn start(
        dev: &Device,
        f: IoProc,
        state: Box<S>,
        beat: Arc<Beat>,
        input: bool,
    ) -> Result<Proc<S>, (String, Box<S>)> {
        let raw = Box::into_raw(state);
        let mut id: IoProcId = None;
        // SAFETY: `raw` stays valid until the IOProc is removed (Drop), or
        // is taken back below if it never ran.
        let st = unsafe { ffi::AudioDeviceCreateIOProcID(dev.id, Some(f), raw.cast(), &mut id) };
        if st != 0 || id.is_none() {
            // SAFETY: never registered, so still only ours.
            return Err((format!("cannot use it ({})", catypes::status_text(st)), unsafe { Box::from_raw(raw) }));
        }
        // Only the direction it uses runs for it. (A device that doesn't
        // allow this runs both, as it would by default.)
        let _ = ffi::streams_off(dev.id, id, !input);
        // SAFETY: the registration just made.
        let st = unsafe { ffi::AudioDeviceStart(dev.id, id) };
        if st != 0 {
            // SAFETY: never started, so the IOProc never ran: removing the
            // registration hands the state back.
            unsafe {
                ffi::AudioDeviceDestroyIOProcID(dev.id, id);
                return Err((format!("cannot start it ({})", catypes::status_text(st)), Box::from_raw(raw)));
            }
        }
        Ok(Proc { device: dev.id, id, state: raw, beat, name: dev.info.name.clone(), uid: dev.info.id.clone() })
    }
}

impl<S> Drop for Proc<S> {
    fn drop(&mut self) {
        // SAFETY: our own registration. Stopped from outside the IOProc,
        // the HAL returns once the IOProc is no longer called; it is then
        // removed, so nothing calls it again, and once it has left (it
        // marks that) the state is ours alone.
        let removed = unsafe {
            ffi::AudioDeviceStop(self.device, self.id);
            ffi::AudioDeviceDestroyIOProcID(self.device, self.id)
        };
        // Not removed, and not because the device is gone: the HAL may
        // still call it, so its state is left to it.
        if removed != 0 && removed != kAudioHardwareBadDeviceError && removed != kAudioHardwareBadObjectError {
            return;
        }
        let t0 = Instant::now();
        while self.beat.inside.load(SeqCst) {
            if t0.elapsed() > Duration::from_secs(1) {
                // Still inside after a second: leave its state to it rather
                // than free memory that may be in use.
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: as above; `state` came from Box::into_raw.
        drop(unsafe { Box::from_raw(self.state) });
    }
}

/// A started IOProc, whatever its state.
trait Running: Send {
    fn calls(&self) -> u64;
    fn name(&self) -> &str;
    fn uid(&self) -> &str;
}

impl<S: Send> Running for Proc<S> {
    fn calls(&self) -> u64 {
        self.beat.calls.load(Relaxed)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn uid(&self) -> &str {
        &self.uid
    }
}

/// An input device feeding a strip.
struct Capture {
    beat: Arc<Beat>,
    prod: Producer,
    /// Its stereo pair (`catypes::stereo_pair`).
    pair: (usize, usize),
}

unsafe extern "C" fn capture_proc(
    _: AudioObjectID,
    _: *const AudioTimeStamp,
    input: *const AudioBufferList,
    _: *const AudioTimeStamp,
    _: *mut AudioBufferList,
    _: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: `client` is the Capture this IOProc was registered with; only
    // this IOProc uses it while it is registered.
    let st = unsafe { &mut *client.cast::<Capture>() };
    st.beat.enter();
    // SAFETY: this cycle's input from the HAL, 32-bit floats (checked when
    // the device was opened).
    unsafe { catypes::take_input(input, st.pair, &mut st.prod) };
    st.beat.leave();
    0
}

/// An output device other than the master, fed through a ring.
struct Sink {
    beat: Arc<Beat>,
    reader: Reader,
    pair: (usize, usize),
    l: Box<[f32]>,
    r: Box<[f32]>,
    seen: u32,
    shared: Arc<Shared>,
}

unsafe extern "C" fn sink_proc(
    _: AudioObjectID,
    _: *const AudioTimeStamp,
    _: *const AudioBufferList,
    _: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: as in `capture_proc`.
    let st = unsafe { &mut *client.cast::<Sink>() };
    st.beat.enter();
    // SAFETY: this cycle's output from the HAL (zeroed, 32-bit floats).
    let n = unsafe { catypes::output_frames(output) }.min(MAX_BLOCK);
    if n > 0 {
        st.reader.read(&mut st.l[..n], &mut st.r[..n], false);
        // SAFETY: as above; n frames are there.
        unsafe { catypes::put_output(output, st.pair, 0, &st.l[..n], &st.r[..n]) };
        let g = st.reader.glitches();
        if g != st.seen {
            st.shared.xruns.fetch_add(g.wrapping_sub(st.seen), Relaxed);
            st.seen = g;
        }
    }
    st.beat.leave();
    0
}

/// The mixer: pulls the strips' rings, mixes, and fills the other output
/// devices' rings. All buffers are allocated before it runs.
struct Mixer {
    eng: Engine,
    strips: Vec<Vec<Reader>>,
    seen: Vec<Vec<u32>>,
    sinks: Vec<(usize, Producer)>,
    bus: [[Box<[f32]>; 2]; NUM_BUSES],
    in_l: Box<[f32]>,
    in_r: Box<[f32]>,
    shared: Arc<Shared>,
}

impl Mixer {
    fn new(eng: Engine, strips: Vec<Vec<Reader>>, sinks: Vec<(usize, Producer)>, shared: Arc<Shared>) -> Mixer {
        let seen = strips.iter().map(|v| vec![0; v.len()]).collect();
        Mixer {
            eng,
            strips,
            seen,
            sinks,
            bus: std::array::from_fn(|_| [zeros(MAX_BLOCK), zeros(MAX_BLOCK)]),
            in_l: zeros(MAX_BLOCK),
            in_r: zeros(MAX_BLOCK),
            shared,
        }
    }

    /// Mix `n` (at most MAX_BLOCK) frames; the buses are in `self.bus`.
    fn run(&mut self, n: usize) {
        let n = n.min(MAX_BLOCK);
        // 1) strips: pull every device feeding a strip, summed
        for (s, readers) in self.strips.iter_mut().enumerate() {
            if readers.is_empty() {
                self.eng.clear_input(s, 0, n);
                self.eng.clear_input(s, 1, n);
                continue;
            }
            self.in_l[..n].fill(0.0);
            self.in_r[..n].fill(0.0);
            for (k, rd) in readers.iter_mut().enumerate() {
                rd.read(&mut self.in_l[..n], &mut self.in_r[..n], true);
                let g = rd.glitches();
                if g != self.seen[s][k] {
                    self.shared.xruns.fetch_add(g.wrapping_sub(self.seen[s][k]), Relaxed);
                    self.seen[s][k] = g;
                }
            }
            self.eng.load_input(s, 0, &self.in_l[..n]);
            self.eng.load_input(s, 1, &self.in_r[..n]);
        }
        // 2) mix (the same engine as on Linux and Windows)
        {
            let mut outs = self.bus.each_mut().map(|pair| {
                let [l, r] = pair;
                [&mut l[..n], &mut r[..n]]
            });
            self.eng.process(&mut outs);
        }
        // 3) every other output device, through its ring
        for (b, prod) in self.sinks.iter_mut() {
            let [l, r] = &self.bus[*b];
            prod.push_planar(&l[..n], &r[..n]);
        }
    }
}

/// The master device runs at another rate: the mix goes through this ring
/// and converter (both on the device's IO thread).
struct Convert {
    prod: Producer,
    rr: RateReader,
    l: Box<[f32]>,
    r: Box<[f32]>,
}

/// The device that clocks the mixer.
struct Master {
    beat: Arc<Beat>,
    mix: Mixer,
    /// The bus it plays.
    bus: usize,
    pair: (usize, usize),
    convert: Option<Convert>,
    rate: f32,
    cpu: f32,
    stats: Arc<Stats>,
}

unsafe extern "C" fn master_proc(
    _: AudioObjectID,
    _: *const AudioTimeStamp,
    _: *const AudioBufferList,
    _: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: as in `capture_proc`.
    let st = unsafe { &mut *client.cast::<Master>() };
    st.beat.enter();
    // SAFETY: this cycle's output from the HAL (zeroed, 32-bit floats).
    let n = unsafe { catypes::output_frames(output) };
    if n > 0 {
        let t0 = Instant::now();
        let mixed = match &mut st.convert {
            None => {
                let mut at = 0;
                while at < n {
                    let k = (n - at).min(MAX_BLOCK);
                    st.mix.run(k);
                    let [l, r] = &st.mix.bus[st.bus];
                    // SAFETY: frames at..at + k are there.
                    unsafe { catypes::put_output(output, st.pair, at, &l[..k], &r[..k]) };
                    at += k;
                }
                n
            }
            Some(c) => {
                // Mix exactly what the converter needs for this cycle, so
                // the mix never waits in the ring.
                let n = n.min(MAX_BLOCK);
                let need = c.rr.needed(n);
                let (mut have, mut mixed) = (c.prod.len(), 0);
                while have < need {
                    let k = (need - have).min(MAX_BLOCK);
                    st.mix.run(k);
                    let [l, r] = &st.mix.bus[st.bus];
                    c.prod.push_planar(&l[..k], &r[..k]);
                    have += k;
                    mixed += k;
                }
                c.rr.read(&mut c.l[..n], &mut c.r[..n], false);
                // SAFETY: n frames are there.
                unsafe { catypes::put_output(output, st.pair, 0, &c.l[..n], &c.r[..n]) };
                mixed
            }
        };
        // DSP load = processing time / the cycle's duration
        let used = t0.elapsed().as_secs_f32() / (n as f32 / st.rate) * 100.0;
        st.cpu += 0.1 * (used - st.cpu);
        st.stats.cpu.store(st.cpu);
        if mixed > 0 {
            st.stats.block.store(mixed as u32, Relaxed);
        }
    }
    st.beat.leave();
    0
}

/// Without an output device: run the mixer every 10 ms, so the meters,
/// the recorder, VBAN and the cables still work.
fn timer_run(mut mix: Mixer, stop: &AtomicBool, stats: &Stats) {
    let tick = Duration::from_millis(10);
    let n = RATE as usize / 100;
    let mut next = Instant::now() + tick;
    let mut cpu = 0.0f32;
    while !stop.load(Relaxed) {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        next += tick;
        // Far behind (the computer slept): go on from now, not catch up.
        if Instant::now() > next + tick * 10 {
            next = Instant::now() + tick;
        }
        let t0 = Instant::now();
        mix.run(n);
        let used = t0.elapsed().as_secs_f32() / 0.01 * 100.0;
        cpu += 0.1 * (used - cpu);
        stats.cpu.store(cpu);
        stats.block.store(n as u32, Relaxed);
    }
}

// ------------------------------------------------------------------ graph

/// A device whose IOProc the HAL stopped calling.
pub struct Stall {
    pub name: String,
    pub uid: String,
    /// It clocks the mixer: everything waits for it.
    pub master: bool,
}

pub struct Graph {
    /// The other output devices, the master, then (from `first_input` on)
    /// the input devices: the order they start and stop in.
    procs: Vec<Box<dyn Running>>,
    first_input: usize,
    /// Where the master is in `procs` (None: the timer clocks the mixer).
    master_at: Option<usize>,
    timer: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    pub desc: Vec<String>,
    pub errs: Vec<String>,
    pub cables: usize,
    /// Input devices it records.
    pub inputs: usize,
    /// Each IOProc's cycle count at the last stall check, and for how many
    /// checks in a row it stood still.
    last: Vec<(u64, u32)>,
}

impl Drop for Graph {
    fn drop(&mut self) {
        // Every ring's reader stops before its writer, so none runs dry on
        // the way down (that would count as an xrun): the other outputs,
        // the mixer (its device or its timer), then the inputs.
        let inputs = self.procs.split_off(self.first_input.min(self.procs.len()));
        self.procs.clear();
        if let Some((stop, t)) = self.timer.take() {
            stop.store(true, Relaxed);
            let _ = t.join();
        }
        drop(inputs);
    }
}

/// Can BananaSplit use this device in this direction? Why not, if it can't.
fn usable(d: &Device, input: bool) -> Result<(), String> {
    if d.rate <= 0.0 {
        return Err("its sample rate is unknown".into());
    }
    let ratio = d.rate / f64::from(RATE);
    if !(1.0 / MAX_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!("it runs at {} kHz: set it to 48 kHz in Audio MIDI Setup", d.rate / 1000.0));
    }
    if !ffi::float32(d.id, input) {
        return Err("its audio format is not one BananaSplit can use (32-bit float)".into());
    }
    Ok(())
}

impl Graph {
    /// Open every configured device and start the audio. `bench`: the UID
    /// of an output device that must not clock the mixer (it stalled while
    /// it did); it only plays, through a ring like the other outputs.
    pub fn build(
        cfg: &Config,
        shared: &Arc<Shared>,
        stats: &Arc<Stats>,
        rec_tap: Arc<RecTap>,
        vban_tap: Arc<VbanTap>,
        play_tap: Option<Arc<PlayTap>>,
        bench: Option<&str>,
    ) -> Result<Graph, String> {
        let all = devices()?;
        let caps: Vec<&Device> = all.iter().filter(|d| d.inputs > 0).collect();
        let rens: Vec<&Device> = all.iter().filter(|d| d.outputs > 0).collect();
        let cap_info: Vec<DevInfo> = caps.iter().map(|d| d.info.clone()).collect();
        let ren_info: Vec<DevInfo> = rens.iter().map(|d| d.info.clone()).collect();
        let (def_cap, def_ren) = (default_uid(true), default_uid(false));
        // Virtual cables switched on in the channel menus become ordinary
        // connect entries: strips record their cable, buses play into
        // theirs. The same assignment is shown in the menus.
        let (strip_cable, bus_cable) = super::chanmenu::cable_extra(cfg, &cap_info, &ren_info);

        let mut desc = Vec::with_capacity(NUM_STRIPS + NUM_BUSES);
        let mut errs = Vec::new();
        let mut cables = BTreeSet::new();

        // Resolve `connect` entries to device indices (deduplicated per row).
        let mut pick = |row: &str, patterns: &[String], infos: &[DevInfo], def: Option<&str>| -> (Vec<usize>, String) {
            let mut idx: Vec<usize> = Vec::new();
            let mut shown: Vec<String> = Vec::new();
            for pat in patterns {
                if pat == DEFAULT_DEVICE
                    && let Some(c) = route::default_cable(infos, def)
                {
                    errs.push(format!(
                            "{row}: the macOS default is {}, a virtual cable BananaSplit uses, so it would feed back; using the real device instead",
                            c.name
                        ));
                }
                match route::resolve(pat, infos, def) {
                    Some(d) => {
                        let i = infos.iter().position(|x| x.id == d.id).expect("from the list");
                        if !idx.contains(&i) {
                            idx.push(i);
                            shown.push(d.name.clone());
                            if route::is_cable(&d.name) {
                                cables.insert(d.name.clone());
                            }
                        }
                    }
                    None if route::names_cable(pat) => shown.push(format!("({pat} not installed)")),
                    None => errs.push(format!("{row}: no device matches {pat:?}")),
                }
            }
            let text = if shown.is_empty() { "(nothing connected)".to_string() } else { shown.join(" + ") };
            (idx, text)
        };

        let mut strip_devs = Vec::with_capacity(NUM_STRIPS);
        for s in 0..NUM_STRIPS {
            let mut patterns = cfg.strips[s].connect.clone();
            if let Some(c) = &strip_cable[s]
                && !patterns.iter().any(|p| p.eq_ignore_ascii_case(c))
            {
                patterns.push(c.clone());
            }
            let (idx, text) = pick(&format!("strip {}", s + 1), &patterns, &cap_info, def_cap.as_deref());
            strip_devs.push(idx);
            desc.push(text);
        }
        let mut bus_devs = Vec::with_capacity(NUM_BUSES);
        for b in 0..NUM_BUSES {
            let mut patterns = cfg.buses[b].connect.clone();
            if let Some(c) = &bus_cable[b]
                && !patterns.iter().any(|p| p.eq_ignore_ascii_case(c))
            {
                patterns.push(c.clone());
            }
            let (idx, text) = pick(&format!("bus {}", BUS_IDS[b]), &patterns, &ren_info, def_ren.as_deref());
            bus_devs.push(idx);
            desc.push(text);
        }
        errs.extend(route::cable_feedback(&cap_info, &ren_info, &strip_devs, &bus_devs));

        // Each device's IO cycle: asked for once (BananaSplit's own setting,
        // other apps keep theirs).
        let mut sizes: HashMap<AudioObjectID, u32> = HashMap::new();
        let mut frames = |d: &Device| *sizes.entry(d.id).or_insert_with(|| ffi::buffer_frames(d.id, cfg.buffer_size));

        // ---- output devices: the first that can be used clocks the mixer ---
        let mut outs: Vec<(usize, &Device)> = Vec::new();
        for (b, idx) in bus_devs.iter().enumerate() {
            for &i in idx {
                match usable(rens[i], false) {
                    Ok(()) => outs.push((b, rens[i])),
                    Err(e) => errs.push(format!("bus {}: {}: {e}", BUS_IDS[b], rens[i].info.name)),
                }
            }
        }
        let master_out = outs.iter().position(|(_, d)| bench != Some(d.info.id.as_str()));
        let master = master_out.map(|k| outs[k]);
        // The mixer's period, in its own frames.
        let period = master.map_or(RATE as usize / 100, |(_, d)| to_mixer(frames(d), d.rate) as usize);
        let mut max_out_lat = master.map_or(0, |(_, d)| to_mixer(frames(d) + ffi::latency_frames(d.id, false), d.rate));

        let mut procs: Vec<Box<dyn Running>> = Vec::new();

        // Every ring's reader starts before its writer (and stops before it,
        // see Drop), so no ring fills up while its reader isn't there yet,
        // which would count as an xrun: the other outputs, the mixer (the
        // master device or the timer), then the inputs.

        // ---- other outputs: mixer -> ring -> device ------------------------
        let mut sinks: Vec<(usize, Producer)> = Vec::new();
        for (k, &(b, d)) in outs.iter().enumerate() {
            if Some(k) == master_out {
                continue;
            }
            let dev_frames = frames(d);
            let target = period + to_mixer(dev_frames, d.rate) as usize + RING_MARGIN;
            let (prod, cons) = ring(RATE as usize);
            let beat = Arc::new(Beat::default());
            let state = Box::new(Sink {
                beat: beat.clone(),
                reader: Reader::new(cons, target, RATE, d.rate.round() as u32),
                pair: ffi::stereo_pair(d.id, false),
                l: zeros(MAX_BLOCK),
                r: zeros(MAX_BLOCK),
                seen: 0,
                shared: shared.clone(),
            });
            match Proc::start(d, sink_proc, state, beat, false) {
                Ok(p) => {
                    procs.push(Box::new(p));
                    sinks.push((b, prod));
                    max_out_lat = max_out_lat
                        .max(target as u32 + to_mixer(dev_frames + ffi::latency_frames(d.id, false), d.rate));
                }
                Err((e, _)) => errs.push(format!("bus {}: {}: {e}", BUS_IDS[b], d.info.name)),
            }
        }

        // ---- inputs: device -> ring -> mixer (started after the mixer) -----
        let mut readers: Vec<Vec<Reader>> = (0..NUM_STRIPS).map(|_| Vec::new()).collect();
        let mut captures = Vec::new();
        for (s, idx) in strip_devs.iter().enumerate() {
            for &i in idx {
                let d = caps[i];
                if let Err(e) = usable(d, true) {
                    errs.push(format!("strip {}: {}: {e}", s + 1, d.info.name));
                    continue;
                }
                // in the device's frames
                let scale = d.rate / f64::from(RATE);
                let target = (period as f64 * scale).ceil() as usize
                    + frames(d) as usize
                    + (RING_MARGIN as f64 * scale).ceil() as usize;
                let rate = d.rate.round() as u32;
                let (prod, cons) = ring((rate as usize).max(RATE as usize).max(4 * target));
                let beat = Arc::new(Beat::default());
                readers[s].push(Reader::new(cons, target, rate, RATE));
                let state = Box::new(Capture { beat: beat.clone(), prod, pair: ffi::stereo_pair(d.id, true) });
                captures.push((s, d, target, state, beat));
            }
        }
        stats.lat_out.store(max_out_lat, Relaxed);
        stats.block.store(period as u32, Relaxed);

        // ---- the mixer -------------------------------------------------------
        let mut eng = Engine::new(shared.clone());
        eng.set_sample_rate(RATE);
        eng.attach_recorder(rec_tap);
        eng.attach_vban(vban_tap);
        if let Some(t) = play_tap {
            eng.attach_player(t);
        }
        let mix = Mixer::new(eng, readers, sinks, shared.clone());
        let mut master_at = None;
        let unclocked = match master {
            Some((b, d)) => {
                let rate = d.rate.round() as u32;
                let convert = (rate != RATE).then(|| {
                    let (prod, cons) = ring(MAX_BLOCK * (MAX_RATIO as usize + 1));
                    Convert { prod, rr: RateReader::fixed(cons, RATE, rate), l: zeros(MAX_BLOCK), r: zeros(MAX_BLOCK) }
                });
                let beat = Arc::new(Beat::default());
                let pair = ffi::stereo_pair(d.id, false);
                let state = Box::new(Master {
                    beat: beat.clone(),
                    mix,
                    bus: b,
                    pair,
                    convert,
                    rate: d.rate as f32,
                    cpu: 0.0,
                    stats: stats.clone(),
                });
                match Proc::start(d, master_proc, state, beat, false) {
                    Ok(p) => {
                        master_at = Some(procs.len());
                        procs.push(Box::new(p));
                        None
                    }
                    Err((e, state)) => {
                        // The other devices go on without it.
                        errs.push(format!("bus {}: {}: {e}", BUS_IDS[b], d.info.name));
                        let Master { mix, .. } = *state;
                        Some(mix)
                    }
                }
            }
            None => Some(mix),
        };
        let mut timer = None;
        if let Some(mix) = unclocked {
            let stop = Arc::new(AtomicBool::new(false));
            let (s, st) = (stop.clone(), stats.clone());
            let t = std::thread::Builder::new()
                .name("bs-mixer".into())
                .spawn(move || timer_run(mix, &s, &st))
                .map_err(|e| format!("cannot start the mixer: {e}"))?;
            timer = Some((stop, t));
        }

        // ---- the inputs, now that the mixer reads their rings ---------------
        let first_input = procs.len();
        let mut max_in_lat = 0u32;
        let mut inputs = 0;
        for (s, d, target, state, beat) in captures {
            match Proc::start(d, capture_proc, state, beat, true) {
                Ok(p) => {
                    procs.push(Box::new(p));
                    inputs += 1;
                    max_in_lat = max_in_lat.max(to_mixer(target as u32 + ffi::latency_frames(d.id, true), d.rate));
                }
                // (its reader in the mixer stays silent: nothing fills its ring)
                Err((e, _)) => errs.push(format!("strip {}: {}: {e}", s + 1, d.info.name)),
            }
        }
        stats.lat_in.store(max_in_lat, Relaxed);

        Ok(Graph { procs, first_input, master_at, timer, desc, errs, cables: cables.len(), inputs, last: Vec::new() })
    }

    /// A device whose IOProc the HAL stopped calling (it stopped, or went
    /// away without leaving the device list), once it stood still for two
    /// checks in a row; the master first, if it is one of them. Called
    /// every few seconds.
    pub fn stalled(&mut self) -> Option<Stall> {
        if self.last.len() != self.procs.len() {
            self.last = vec![(u64::MAX, 0); self.procs.len()];
        }
        let (mut master, mut other) = (None, None);
        for (k, (p, (seen, still))) in self.procs.iter().zip(self.last.iter_mut()).enumerate() {
            let calls = p.calls();
            if calls == *seen {
                *still += 1;
                if *still >= 2 {
                    let is_master = self.master_at == Some(k);
                    let found = if is_master { &mut master } else { &mut other };
                    if found.is_none() {
                        *found =
                            Some(Stall { name: p.name().to_string(), uid: p.uid().to_string(), master: is_master });
                    }
                }
            } else {
                *seen = calls;
                *still = 0;
            }
        }
        master.or(other)
    }
}
