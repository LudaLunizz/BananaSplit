//! WASAPI audio graph.
//!
//! ```text
//!  capture thread ─ring─┐                         ┌─ring─ sink thread  (other bus devices)
//!  capture thread ─ring─┤→ MASTER thread: Engine ─┤
//!  capture thread ─ring─┘   (clocked by the A1    └────── master device (written directly)
//!                            device's events)
//! ```
//!
//! Every stream is opened in **shared mode at 48 kHz, stereo, float32**,
//! with AUTOCONVERTPCM, so Windows converts whatever the device runs at.
//! Shared mode never blocks other apps, and virtual cables can be opened
//! as many times as needed.
//!
//! Each Windows device has its own clock, so unlike JACK there is no single
//! graph clock. The mixer is clocked by the first bus device (normally A1),
//! and every other device is connected through a lock-free ring read by a
//! drift-correcting resampler (`bananasplit::ring`). Audio threads use MMCSS
//! "Pro Audio" priority and never allocate, lock or block once running.

#![allow(clippy::needless_range_loop)] // index loops mirror the [strip]/[bus] layout

use super::ffi::*;
use super::route::{self, DevInfo};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use bananasplit::config::Config;
use bananasplit::engine::{AtomicF32, BUS_IDS, Engine, NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::recorder::RecTap;
use bananasplit::vban::VbanTap;
use bananasplit::player::PlayTap;
use bananasplit::ring::{DriftReader, Producer, ring};

pub const RATE: u32 = 48_000;
/// Largest block the master processes at once (WASAPI hands out ~10 ms).
const MAX_BLOCK: usize = 8192;
/// Ring capacity: one second.
const RING_FRAMES: usize = RATE as usize;
/// Extra frames of safety margin in each ring on top of the two periods.
const RING_MARGIN: usize = 96;

// ------------------------------------------------------------------ devices

pub struct Device {
    pub info: DevInfo,
    dev: Com<IMMDeviceVtbl>,
}

pub struct Enumerator(Com<IMMDeviceEnumeratorVtbl>);

impl Enumerator {
    pub fn new() -> Result<Enumerator, String> {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: COM is up on this thread (callers hold `ComInit::mta`); the CLSID/IID are statics and `p` is a local out-pointer.
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_MMDeviceEnumerator,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IID_IMMDeviceEnumerator,
                &mut p,
            )
        };
        if hr < 0 {
            return Err(format!(
                "cannot create the Windows audio device enumerator: {}",
                hr_text(hr)
            ));
        }
        // SAFETY: `p` is the owned IMMDeviceEnumerator CoCreateInstance returned for its IID, on an MTA thread.
        unsafe { Com::from_raw(p) }
            .map(Enumerator)
            .ok_or_else(|| "device enumerator is NULL".into())
    }

    /// All active endpoints of one direction.
    pub fn devices(&self, capture: bool) -> Result<Vec<Device>, String> {
        let e = &self.0;
        let flow = if capture { eCapture } else { eRender };
        let mut pc: *mut c_void = std::ptr::null_mut();
        let hr =
            // SAFETY: `e` owns a live IMMDeviceEnumerator (vtable order as in the SDK); `pc` is a local out-pointer.
            unsafe { (e.vt().EnumAudioEndpoints)(e.as_ptr(), flow, DEVICE_STATE_ACTIVE, &mut pc) };
        if hr < 0 {
            return Err(format!("EnumAudioEndpoints: {}", hr_text(hr)));
        }
        // SAFETY: `pc` is the owned IMMDeviceCollection EnumAudioEndpoints returned, on this MTA thread.
        let Some(coll) = (unsafe { Com::<IMMDeviceCollectionVtbl>::from_raw(pc) }) else {
            return Ok(Vec::new());
        };
        let mut n = 0u32;
        // SAFETY: `coll` owns a live IMMDeviceCollection; `n` is a local u32 out-pointer.
        unsafe { (coll.vt().GetCount)(coll.as_ptr(), &mut n) };
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let mut pd: *mut c_void = std::ptr::null_mut();
            // SAFETY: `coll` owns a live IMMDeviceCollection, `i` < GetCount; `pd` is a local out-pointer.
            if unsafe { (coll.vt().Item)(coll.as_ptr(), i, &mut pd) } < 0 {
                continue;
            }
            // SAFETY: `pd` is the owned IMMDevice that Item returned, on this MTA thread.
            if let Some(dev) = unsafe { Com::<IMMDeviceVtbl>::from_raw(pd) } {
                let info = DevInfo {
                    name: device_name(&dev),
                    id: device_id(&dev),
                };
                out.push(Device { info, dev });
            }
        }
        Ok(out)
    }

    pub fn default_id(&self, capture: bool) -> Option<String> {
        let e = &self.0;
        let flow = if capture { eCapture } else { eRender };
        let mut pd: *mut c_void = std::ptr::null_mut();
        // SAFETY: `e` owns a live IMMDeviceEnumerator (vtable order as in the SDK); `pd` is a local out-pointer.
        let hr = unsafe { (e.vt().GetDefaultAudioEndpoint)(e.as_ptr(), flow, eConsole, &mut pd) };
        if hr < 0 {
            return None; // no device of this kind
        }
        // SAFETY: `pd` is the owned IMMDevice GetDefaultAudioEndpoint returned, on this MTA thread.
        unsafe { Com::<IMMDeviceVtbl>::from_raw(pd) }.map(|d| device_id(&d))
    }

    /// Changes whenever a device appears, disappears, is renamed, or a
    /// default device changes. Cheap enough to poll every few seconds.
    pub fn signature(&self) -> String {
        let mut s = String::new();
        for cap in [false, true] {
            if let Ok(devs) = self.devices(cap) {
                for d in devs {
                    s.push_str(&d.info.id);
                    s.push('=');
                    s.push_str(&d.info.name);
                    s.push('\n');
                }
            }
            s.push_str(&self.default_id(cap).unwrap_or_default());
            s.push('\n');
        }
        s
    }
}

fn device_id(dev: &Com<IMMDeviceVtbl>) -> String {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: `dev` owns a live IMMDevice; `p` is a local out-pointer for the CoTaskMem id string.
    if unsafe { (dev.vt().GetId)(dev.as_ptr(), &mut p) } < 0 || p.is_null() {
        return String::new();
    }
    // SAFETY: GetId succeeded and `p` is non-NULL: a NUL-terminated UTF-16 string.
    let s = unsafe { from_wide(p) };
    // SAFETY: `p` was allocated by GetId (CoTaskMemAlloc) and is freed once, after its last use.
    unsafe { CoTaskMemFree(p.cast()) };
    s
}

fn device_name(dev: &Com<IMMDeviceVtbl>) -> String {
    let mut ps: *mut c_void = std::ptr::null_mut();
    // SAFETY: `dev` owns a live IMMDevice; `ps` is a local out-pointer.
    if unsafe { (dev.vt().OpenPropertyStore)(dev.as_ptr(), STGM_READ, &mut ps) } < 0 {
        return "(unnamed device)".into();
    }
    // SAFETY: `ps` is the owned IPropertyStore OpenPropertyStore returned, on this MTA thread.
    let Some(store) = (unsafe { Com::<IPropertyStoreVtbl>::from_raw(ps) }) else {
        return "(unnamed device)".into();
    };
    let mut pv = PROPVARIANT {
        vt: 0,
        r1: 0,
        r2: 0,
        r3: 0,
        data: [0; 2],
    };
    let mut name = String::from("(unnamed device)");
    // SAFETY: `store` owns a live IPropertyStore; the key is a static, `pv` a local PROPVARIANT.
    if unsafe { (store.vt().GetValue)(store.as_ptr(), &PKEY_Device_FriendlyName, &mut pv) } >= 0 {
        if pv.vt == VT_LPWSTR && pv.data[0] != 0 {
            // SAFETY: VT_LPWSTR with a non-NULL data[0]: a NUL-terminated UTF-16 string owned by `pv`.
            name = unsafe { from_wide(pv.data[0] as *const u16) };
        }
        // SAFETY: GetValue succeeded, so `pv` is ours to clear, once, after its last use.
        unsafe { PropVariantClear(&mut pv) };
    }
    name
}

// ------------------------------------------------------------------ streams

struct Stream {
    client: Com<IAudioClientVtbl>,
    render: Option<Com<IAudioRenderClientVtbl>>,
    capture: Option<Com<IAudioCaptureClientVtbl>>,
    event: Event,
    buffer_frames: u32,
    period_frames: u32,
    latency_frames: u32,
}

fn hns_to_frames(hns: i64) -> u32 {
    (hns.max(0) as u64 * RATE as u64 / 10_000_000) as u32
}

fn open_stream(dev: &Device, capture: bool) -> Result<Stream, String> {
    let name = route::short_name(&dev.info.name).to_string();
    let fail = |what: &str, hr: HRESULT| format!("{name}: {what} failed: {}", hr_text(hr));

    let mut pc: *mut c_void = std::ptr::null_mut();
    // SAFETY: `dev.dev` owns a live IMMDevice; the IID is a static and `pc` a local out-pointer.
    let hr = unsafe {
        (dev.dev.vt().Activate)(
            dev.dev.as_ptr(),
            &IID_IAudioClient,
            CLSCTX_ALL,
            std::ptr::null_mut(),
            &mut pc,
        )
    };
    if hr < 0 {
        return Err(fail("Activate", hr));
    }
    // SAFETY: `pc` is the owned IAudioClient Activate returned; callers hold `ComInit::mta`.
    let client = unsafe { Com::<IAudioClientVtbl>::from_raw(pc) }
        .ok_or_else(|| format!("{name}: no audio client"))?;
    let c = client.vt();

    let (mut def_period, mut min_period) = (0i64, 0i64);
    // SAFETY: `client` owns a live IAudioClient; both out-pointers are local i64s.
    unsafe { (c.GetDevicePeriod)(client.as_ptr(), &mut def_period, &mut min_period) };

    let fmt = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // Buffer duration 0 = the smallest buffer the engine allows (about one
    // device period in event-driven shared mode).
    // SAFETY: `client` owns a live IAudioClient; `fmt` is a local WAVEFORMATEX (cbSize 0) that outlives the call; no session GUID.
    let hr = unsafe {
        (c.Initialize)(
            client.as_ptr(),
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            0,
            0,
            &fmt,
            std::ptr::null(),
        )
    };
    if hr < 0 {
        return Err(fail("Initialize", hr));
    }
    let event = Event::new()?;
    // SAFETY: the handle is owned by `event`, which the Stream drops after `client` (field order).
    let hr = unsafe { (c.SetEventHandle)(client.as_ptr(), event.raw()) };
    if hr < 0 {
        return Err(fail("SetEventHandle", hr));
    }
    let mut buffer_frames = 0u32;
    // SAFETY: `client` owns an initialised IAudioClient; `buffer_frames` is a local out-pointer.
    unsafe { (c.GetBufferSize)(client.as_ptr(), &mut buffer_frames) };
    let mut lat = 0i64;
    // SAFETY: `client` owns an initialised IAudioClient; `lat` is a local out-pointer.
    unsafe { (c.GetStreamLatency)(client.as_ptr(), &mut lat) };

    let iid = if capture {
        &IID_IAudioCaptureClient
    } else {
        &IID_IAudioRenderClient
    };
    let mut ps: *mut c_void = std::ptr::null_mut();
    // SAFETY: `client` owns an initialised IAudioClient; `iid` is a static IID and `ps` a local out-pointer.
    let hr = unsafe { (c.GetService)(client.as_ptr(), iid, &mut ps) };
    if hr < 0 {
        return Err(fail("GetService", hr));
    }
    let (render, capture_svc) = if capture {
        // SAFETY: `ps` is the owned IAudioCaptureClient GetService returned for IID_IAudioCaptureClient.
        (None, unsafe {
            Com::<IAudioCaptureClientVtbl>::from_raw(ps)
        })
    } else {
        // SAFETY: `ps` is the owned IAudioRenderClient GetService returned for IID_IAudioRenderClient.
        (unsafe { Com::<IAudioRenderClientVtbl>::from_raw(ps) }, None)
    };
    Ok(Stream {
        client,
        render,
        capture: capture_svc,
        event,
        buffer_frames,
        period_frames: hns_to_frames(def_period).max(1),
        latency_frames: hns_to_frames(lat),
    })
}

impl Stream {
    fn start(&self) -> Result<(), HRESULT> {
        // Pre-fill a render buffer with silence so the first period plays cleanly.
        if let Some(r) = &self.render {
            let mut p: *mut u8 = std::ptr::null_mut();
            // SAFETY: `r` owns a live IAudioRenderClient; before Start the whole buffer is free; `p` is a local out-pointer.
            if unsafe { (r.vt().GetBuffer)(r.as_ptr(), self.buffer_frames, &mut p) } >= 0 {
                // SAFETY: pairs the successful GetBuffer above, with the same frame count; no data is read.
                unsafe {
                    (r.vt().ReleaseBuffer)(
                        r.as_ptr(),
                        self.buffer_frames,
                        AUDCLNT_BUFFERFLAGS_SILENT,
                    )
                };
            }
        }
        // SAFETY: `self.client` owns an initialised IAudioClient.
        let hr = unsafe { (self.client.vt().Start)(self.client.as_ptr()) };
        if hr < 0 { Err(hr) } else { Ok(()) }
    }

    fn stop(&self) {
        // SAFETY: `self.client` owns an initialised IAudioClient.
        unsafe { (self.client.vt().Stop)(self.client.as_ptr()) };
    }

    /// Wait for the device to want/produce data. false on timeout.
    fn wait(&self) -> bool {
        self.wait_ms(100)
    }

    /// Like [`Stream::wait`], with an explicit timeout in milliseconds.
    fn wait_ms(&self, ms: u32) -> bool {
        // SAFETY: the handle is owned by `self.event`, alive for the call.
        unsafe { WaitForSingleObject(self.event.raw(), ms) == WAIT_OBJECT_0 }
    }

    /// Frames the render buffer can take right now.
    fn writable(&self) -> Result<u32, HRESULT> {
        let mut pad = 0u32;
        // SAFETY: `self.client` owns an initialised IAudioClient; `pad` is a local out-pointer.
        let hr = unsafe { (self.client.vt().GetCurrentPadding)(self.client.as_ptr(), &mut pad) };
        if hr < 0 {
            Err(hr)
        } else {
            Ok(self.buffer_frames.saturating_sub(pad))
        }
    }

    /// Hand `n` frames of interleaved stereo to the device; `fill` writes them.
    fn write(&self, n: u32, fill: impl FnOnce(&mut [f32])) -> Result<(), HRESULT> {
        let r = self.render.as_ref().expect("render stream");
        let mut p: *mut u8 = std::ptr::null_mut();
        // SAFETY: `r` owns a live IAudioRenderClient; `p` is a local out-pointer (GetBuffer fails if `n` frames do not fit).
        let hr = unsafe { (r.vt().GetBuffer)(r.as_ptr(), n, &mut p) };
        if hr < 0 {
            return Err(hr);
        }
        if !p.is_null() {
            // SAFETY: GetBuffer succeeded: `p` holds `n` frames of the stream's format,
            // stereo f32 (Initialize with AUTOCONVERTPCM), until ReleaseBuffer below.
            fill(unsafe { std::slice::from_raw_parts_mut(p.cast::<f32>(), n as usize * 2) });
        }
        // SAFETY: pairs the successful GetBuffer above, with the same `n` frames.
        let hr = unsafe { (r.vt().ReleaseBuffer)(r.as_ptr(), n, 0) };
        if hr < 0 { Err(hr) } else { Ok(()) }
    }
}

/// MMCSS "Pro Audio" scheduling for the current thread while alive.
struct Mmcss(HANDLE);
impl Mmcss {
    fn pro_audio() -> Mmcss {
        let task = wide("Pro Audio");
        let mut idx = 0u32;
        // SAFETY: `task` is a NUL-terminated UTF-16 string held in a local; `idx` is a local out-pointer.
        Mmcss(unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut idx) })
    }
}
impl Drop for Mmcss {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is the non-NULL handle AvSetMmThreadCharacteristicsW gave this thread (HANDLE keeps Mmcss !Send).
            unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }
}

// ------------------------------------------------------------------ graph

/// Live numbers for the UI, written by the audio threads.
#[derive(Default)]
pub struct Stats {
    /// A device went away or changed format: the graph must be rebuilt.
    pub invalidated: AtomicBool,
    pub block: AtomicU32,
    pub cpu: AtomicF32,
    pub lat_in: AtomicU32,
    pub lat_out: AtomicU32,
}

pub struct Graph {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    pub desc: Vec<String>,
    pub errs: Vec<String>,
    pub cables: usize,
}

impl Drop for Graph {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Graph {
    /// Open every configured device and start the audio threads.
    pub fn build(
        en: &Enumerator,
        cfg: &Config,
        shared: &Arc<Shared>,
        stats: &Arc<Stats>,
        rec_tap: Arc<RecTap>,
        vban_tap: Arc<VbanTap>,
        play_tap: Option<Arc<PlayTap>>,
    ) -> Result<Graph, String> {
        let caps = en.devices(true)?;
        let rens = en.devices(false)?;
        let cap_info: Vec<DevInfo> = caps.iter().map(|d| d.info.clone()).collect();
        let ren_info: Vec<DevInfo> = rens.iter().map(|d| d.info.clone()).collect();
        let def_cap = en.default_id(true);
        let def_ren = en.default_id(false);
        // Virtual cables switched on in the channel menus become ordinary
        // connect entries: strips capture the cable's Output, buses play to
        // its Input. The same assignment is shown in the menus.
        let (strip_cable, bus_cable) = crate::win::chanmenu::cable_extra(cfg, &cap_info, &ren_info);

        let mut desc = Vec::with_capacity(NUM_STRIPS + NUM_BUSES);
        let mut errs = Vec::new();
        let mut cables = std::collections::BTreeSet::new();
        let stop = Arc::new(AtomicBool::new(false));
        stats.invalidated.store(false, Relaxed);

        // Resolve `connect` entries to device indices (deduplicated per row).
        let mut pick = |row: &str,
                        patterns: &[String],
                        infos: &[DevInfo],
                        def: Option<&str>|
         -> (Vec<usize>, String) {
            let mut idx: Vec<usize> = Vec::new();
            let mut shown: Vec<String> = Vec::new();
            for pat in patterns {
                if pat == bananasplit::config::DEFAULT_DEVICE
                    && let Some(c) = route::default_cable(infos, def) {
                        errs.push(format!(
                            "{row}: the Windows default is {}, a virtual cable BananaSplit uses, so it would feed back; using the real device instead",
                            route::short_name(&c.name)
                        ));
                    }
                match route::resolve(pat, infos, def) {
                    Some(d) => {
                        let i = infos.iter().position(|x| x.id == d.id).expect("from list");
                        if !idx.contains(&i) {
                            idx.push(i);
                            shown.push(route::short_name(&d.name).to_string());
                            if route::is_cable(&d.name) {
                                cables.insert(
                                    route::short_name(&d.name)
                                        .replace(" Input", "")
                                        .replace(" Output", ""),
                                );
                            }
                        }
                    }
                    None if route::names_cable(pat) => shown.push(format!("({pat} not installed)")),
                    None => errs.push(format!("{row}: no device matches {pat:?}")),
                }
            }
            let text = if idx.is_empty() && shown.is_empty() {
                "(nothing connected)".to_string()
            } else {
                shown.join(" + ")
            };
            (idx, text)
        };

        let mut strip_devs = Vec::with_capacity(NUM_STRIPS);
        for s in 0..NUM_STRIPS {
            let mut patterns = cfg.strips[s].connect.clone();
            if let Some(c) = &strip_cable[s]
                && !patterns.iter().any(|p| p.eq_ignore_ascii_case(c)) {
                    patterns.push(c.clone());
                }
            let (idx, text) = pick(
                &format!("strip {}", s + 1),
                &patterns,
                &cap_info,
                def_cap.as_deref(),
            );
            strip_devs.push(idx);
            desc.push(text);
        }
        let mut bus_devs = Vec::with_capacity(NUM_BUSES);
        for b in 0..NUM_BUSES {
            let mut patterns = cfg.buses[b].connect.clone();
            if let Some(c) = &bus_cable[b]
                && !patterns.iter().any(|p| p.eq_ignore_ascii_case(c)) {
                    patterns.push(c.clone());
                }
            let (idx, text) = pick(
                &format!("bus {}", BUS_IDS[b]),
                &patterns,
                &ren_info,
                def_ren.as_deref(),
            );
            bus_devs.push(idx);
            desc.push(text);
        }

        errs.extend(route::splitter_feedback(&cap_info, &ren_info, &strip_devs, &bus_devs));

        // ---- open render streams: first one becomes the master clock ------
        let mut master: Option<(usize, Stream)> = None;
        let mut sink_streams: Vec<(usize, Stream)> = Vec::new();
        for (b, idx) in bus_devs.iter().enumerate() {
            for &i in idx {
                match open_stream(&rens[i], false) {
                    Ok(s) if master.is_none() => master = Some((b, s)),
                    Ok(s) => sink_streams.push((b, s)),
                    Err(e) => errs.push(format!("bus {}: {e}", BUS_IDS[b])),
                }
            }
        }
        let master_period = master
            .as_ref()
            .map_or(480, |(_, s)| s.period_frames as usize);

        let mut threads = Vec::new();
        // The loops below shadow `stop` with clones for their closures.
        let halt = stop.clone();

        // ---- other sinks: master -> ring -> device ------------------------
        let mut sink_prods: Vec<(usize, Producer)> = Vec::new();
        let mut max_out_lat = master
            .as_ref()
            .map_or(0, |(_, s)| s.buffer_frames + s.latency_frames);
        for (b, s) in sink_streams {
            let (prod, cons) = ring(RING_FRAMES);
            let target = master_period + s.period_frames as usize + RING_MARGIN;
            max_out_lat = max_out_lat.max((target as u32) + s.buffer_frames + s.latency_frames);
            let rd = DriftReader::new(cons, target);
            sink_prods.push((b, prod));
            let (stop, stats, shared) = (stop.clone(), stats.clone(), shared.clone());
            start(&mut threads, &halt, "vmix-sink", move || {
                sink_thread(s, rd, &stop, &stats, &shared)
            })?;
        }

        // ---- capture: device -> ring -> master -----------------------------
        let mut readers: Vec<Vec<DriftReader>> = (0..NUM_STRIPS).map(|_| Vec::new()).collect();
        let mut max_in_lat = 0u32;
        for (st, idx) in strip_devs.iter().enumerate() {
            for &i in idx {
                match open_stream(&caps[i], true) {
                    Ok(s) => {
                        let (prod, cons) = ring(RING_FRAMES);
                        let target = master_period + s.period_frames as usize + RING_MARGIN;
                        max_in_lat = max_in_lat.max(target as u32 + s.latency_frames);
                        readers[st].push(DriftReader::new(cons, target));
                        let (stop, stats) = (stop.clone(), stats.clone());
                        start(&mut threads, &halt, "vmix-capture", move || {
                            capture_thread(s, prod, &stop, &stats)
                        })?;
                    }
                    Err(e) => errs.push(format!("strip {}: {e}", st + 1)),
                }
            }
        }
        stats.lat_in.store(max_in_lat, Relaxed);
        stats.lat_out.store(max_out_lat, Relaxed);
        stats.block.store(master_period as u32, Relaxed);

        // ---- master ---------------------------------------------------------
        let mut engine = Engine::new(shared.clone());
        engine.set_sample_rate(RATE);
        engine.attach_recorder(rec_tap);
        engine.attach_vban(vban_tap);
        if let Some(t) = play_tap { engine.attach_player(t); }
        let (master_bus, master_stream) = match master {
            Some((b, s)) => (b, Some(s)),
            None => (0, None),
        };
        {
            let (stop, stats, shared) = (stop.clone(), stats.clone(), shared.clone());
            start(&mut threads, &halt, "vmix-mixer", move || {
                master_thread(
                    master_stream,
                    engine,
                    readers,
                    master_bus,
                    sink_prods,
                    &stop,
                    &stats,
                    &shared,
                )
            })?;
        }

        Ok(Graph {
            stop,
            threads,
            desc,
            errs,
            cables: cables.len(),
        })
    }
}

/// Start an audio thread. If it can't start, the threads already started
/// are stopped and joined, so their devices are closed.
fn start(threads: &mut Vec<JoinHandle<()>>, stop: &AtomicBool, name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    match std::thread::Builder::new().name(name.into()).spawn(f) {
        Ok(t) => {
            threads.push(t);
            Ok(())
        }
        Err(e) => {
            stop.store(true, Relaxed);
            for t in threads.drain(..) {
                let _ = t.join();
            }
            Err(format!("cannot start the {name} thread: {e}"))
        }
    }
}

// ------------------------------------------------------------------ threads

fn capture_thread(s: Stream, mut prod: Producer, stop: &AtomicBool, stats: &Stats) {
    let Ok(_com) = ComInit::mta() else {
        stats.invalidated.store(true, Relaxed);
        return;
    };
    // Rebound after COM is up, so the stream is released before COM ends
    // (parameters are dropped after the function's locals).
    let s = s;
    let _mm = Mmcss::pro_audio();
    let cap = s.capture.as_ref().expect("capture stream");
    if s.start().is_err() {
        stats.invalidated.store(true, Relaxed);
        return;
    }
    let quiet = s.period_frames.max(1) as usize;
    'run: while !stop.load(Relaxed) {
        let mut got_data = false;
        if !s.wait_ms(10) {
            // An idle endpoint (a virtual cable nothing plays into, an
            // unplugged microphone) produces no packets at all. Feed its ring
            // silence at real-time pace, or the strip's reader runs dry thirty
            // times a second and reports xruns for a channel with no audio.
            if prod.len() < quiet * 2 {
                prod.push_silence(quiet);
            }
            continue;
        }
        loop {
            let mut next = 0u32;
            // SAFETY: `cap` owns a live IAudioCaptureClient (this thread is MTA); `next` is a local out-pointer.
            let hr = unsafe { (cap.vt().GetNextPacketSize)(cap.as_ptr(), &mut next) };
            if hr < 0 {
                    stats.invalidated.store(true, Relaxed);
                break 'run;
            }
            if next == 0 {
                break;
            }
            let (mut p, mut frames, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
            // SAFETY: `cap` owns a live IAudioCaptureClient; the out-pointers are locals of the right types, the optional ones NULL.
            let hr = unsafe {
                (cap.vt().GetBuffer)(
                    cap.as_ptr(),
                    &mut p,
                    &mut frames,
                    &mut flags,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if hr < 0 {
                    stats.invalidated.store(true, Relaxed);
                break 'run;
            }
            if frames == 0 {
                break; // AUDCLNT_S_BUFFER_EMPTY
            }
            if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || p.is_null() {
                prod.push_silence(frames as usize);
            } else {
                // SAFETY: GetBuffer succeeded with a non-NULL `p`: `frames` frames of the stream's
                // format, stereo f32 (Initialize with AUTOCONVERTPCM), until ReleaseBuffer below.
                prod.push_interleaved(unsafe {
                    std::slice::from_raw_parts(p.cast::<f32>(), frames as usize * 2)
                });
            }
            // SAFETY: releases the packet the successful GetBuffer above handed out, with its frame count.
            unsafe { (cap.vt().ReleaseBuffer)(cap.as_ptr(), frames) };
            got_data = true;
        }
        if !got_data && prod.len() < quiet * 2 {
            prod.push_silence(quiet);
        }
    }
    s.stop();
}

fn sink_thread(s: Stream, mut rd: DriftReader, stop: &AtomicBool, stats: &Stats, shared: &Shared) {
    let Ok(_com) = ComInit::mta() else {
        stats.invalidated.store(true, Relaxed);
        return;
    };
    // Rebound after COM is up, so the stream is released before COM ends
    // (parameters are dropped after the function's locals).
    let s = s;
    let _mm = Mmcss::pro_audio();
    let n_max = s.buffer_frames.max(1) as usize;
    let (mut l, mut r) = (vec![0.0f32; n_max], vec![0.0f32; n_max]);
    if s.start().is_err() {
        stats.invalidated.store(true, Relaxed);
        return;
    }
    let mut glitches = 0;
    while !stop.load(Relaxed) {
        if !s.wait() {
            continue;
        }
        let res = s.writable().and_then(|n| {
            if n == 0 {
                return Ok(());
            }
            s.write(n, |buf| {
                rd.read_interleaved(buf, &mut l, &mut r);
            })
        });
        if res.is_err() {
                stats.invalidated.store(true, Relaxed);
            break;
        }
        if rd.glitches != glitches {
            shared.xruns.fetch_add(rd.glitches - glitches, Relaxed);
            glitches = rd.glitches;
        }
    }
    s.stop();
}

#[allow(clippy::too_many_arguments)]
fn master_thread(
    out: Option<Stream>,
    mut eng: Engine,
    mut strips: Vec<Vec<DriftReader>>,
    master_bus: usize,
    mut sinks: Vec<(usize, Producer)>,
    stop: &AtomicBool,
    stats: &Stats,
    shared: &Shared,
) {
    let Ok(_com) = ComInit::mta() else {
        stats.invalidated.store(true, Relaxed);
        return;
    };
    // Rebound after COM is up, so the stream is released before COM ends
    // (parameters are dropped after the function's locals).
    let out = out;
    let _mm = Mmcss::pro_audio();
    // All buffers are allocated here, before the loop.
    let mut in_l = vec![0.0f32; MAX_BLOCK];
    let mut in_r = vec![0.0f32; MAX_BLOCK];
    let mut bus: [[Vec<f32>; 2]; NUM_BUSES] =
        std::array::from_fn(|_| [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]]);
    let mut glitch_seen: Vec<Vec<u32>> = strips.iter().map(|v| vec![0; v.len()]).collect();

    if let Some(s) = &out
        && s.start().is_err() {
            stats.invalidated.store(true, Relaxed);
            return;
        }
    // Without any output device, run on a 10 ms timer so meters and the
    // virtual-cable buses still work.
    let tick = Duration::from_millis(10);
    let mut next_tick = Instant::now() + tick;
    let mut cpu_avg = 0.0f32;

    while !stop.load(Relaxed) {
        let n = match &out {
            Some(s) => {
                if !s.wait() {
                    continue;
                }
                match s.writable() {
                    Ok(n) => n as usize,
                    Err(_) => {
                            stats.invalidated.store(true, Relaxed);
                        break;
                    }
                }
            }
            None => {
                let now = Instant::now();
                if next_tick > now {
                    std::thread::sleep(next_tick - now);
                }
                next_tick += tick;
                RATE as usize / 100
            }
        };
        let n = n.min(MAX_BLOCK);
        if n == 0 {
            continue;
        }
        let t0 = Instant::now();

        // 1) strips: pull every device feeding a strip, summed
        for (s, readers) in strips.iter_mut().enumerate() {
            if readers.is_empty() {
                eng.clear_input(s, 0, n);
                eng.clear_input(s, 1, n);
                continue;
            }
            in_l[..n].fill(0.0);
            in_r[..n].fill(0.0);
            for (k, rd) in readers.iter_mut().enumerate() {
                rd.read(&mut in_l[..n], &mut in_r[..n], true);
                if rd.glitches != glitch_seen[s][k] {
                    shared
                        .xruns
                        .fetch_add(rd.glitches - glitch_seen[s][k], Relaxed);
                    glitch_seen[s][k] = rd.glitches;
                }
            }
            eng.load_input(s, 0, &in_l[..n]);
            eng.load_input(s, 1, &in_r[..n]);
        }

        // 2) mix (same engine as on Linux)
        {
            let mut outs = bus.each_mut().map(|pair| {
                let [l, r] = pair;
                [&mut l[..n], &mut r[..n]]
            });
            eng.process(&mut outs);
        }

        // 3) master device, written directly
        if let Some(s) = &out {
            let [l, r] = &bus[master_bus];
            let res = s.write(n as u32, |buf| {
                for i in 0..n {
                    buf[2 * i] = l[i];
                    buf[2 * i + 1] = r[i];
                }
            });
            if res.is_err() {
                    stats.invalidated.store(true, Relaxed);
                break;
            }
        }

        // 4) every other output device through its ring
        for (b, prod) in sinks.iter_mut() {
            let [l, r] = &bus[*b];
            prod.push_planar(&l[..n], &r[..n]);
        }

        // DSP load = processing time / block duration
        let used = t0.elapsed().as_secs_f32() / (n as f32 / RATE as f32) * 100.0;
        cpu_avg += 0.1 * (used - cpu_avg);
        stats.cpu.store(cpu_avg);
        stats.block.store(n as u32, Relaxed);
    }
    if let Some(s) = &out {
        s.stop();
    }
}






