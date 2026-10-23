//! JACK backend: ports, real-time callback, auto-connection.
//!
//! The whole mixer runs inside ONE JACK process callback. JACK (and
//! PipeWire's JACK layer) call it once per period for every port at once, so
//! all 12 input and 10 output channels are processed for the same frames in
//! the same cycle — inherently sample-synchronous, with no extra buffering.

#![allow(clippy::needless_range_loop)] // index loops mirror the [strip][channel] layout

use crate::cables::{CableSet, Cables};
use crate::devices::{self, Resolver, Side, client_of};
use crate::jack::{JackClient, LATENCY_CAPTURE, LATENCY_PLAYBACK, Port, regex_escape};
use std::cell::Cell;
use std::ffi::{c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};
use bananasplit::config::Config;
use bananasplit::engine::{BUS_IDS, Engine, MAX_FRAMES, NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::player::PlayTap;
use bananasplit::recorder::RecTap;
use bananasplit::vban::VbanTap;

struct RtCtx {
    lib: &'static crate::jack::Lib,
    engine: Engine,
    ins: [[Port; 2]; NUM_STRIPS],
    outs: [[Port; 2]; NUM_BUSES],
}

pub struct CtlCtx {
    pub shared: Arc<Shared>,
    pub ports_changed: AtomicBool,
    /// Some connection was made or removed (by anyone).
    pub conns_changed: AtomicBool,
    pub shutdown: AtomicBool,
}

unsafe extern "C" fn process_cb(nframes: u32, arg: *mut c_void) -> c_int {
    // SAFETY: `arg` is the RtCtx box owned by Backend, alive until the client
    // is closed; JACK never runs two process callbacks of one client at once.
    let ctx = unsafe { &mut *arg.cast::<RtCtx>() };
    let n = nframes as usize;
    let lib = ctx.lib;

    // 1) Copy inputs into the engine (before any output buffer is touched,
    //    so an input that aliases one of our outputs is still read correctly).
    for s in 0..NUM_STRIPS {
        for c in 0..2 {
            let p = unsafe { (lib.jack_port_get_buffer)(ctx.ins[s][c], nframes) }.cast::<f32>();
            if p.is_null() || n > MAX_FRAMES {
                ctx.engine.clear_input(s, c, n);
            } else {
                // SAFETY: JACK guarantees `nframes` valid samples.
                ctx.engine.load_input(s, c, unsafe { std::slice::from_raw_parts(p, n) });
            }
        }
    }

    // 2) Mix straight into the output port buffers.
    let mut ptrs = [[std::ptr::null_mut::<f32>(); 2]; NUM_BUSES];
    for b in 0..NUM_BUSES {
        for c in 0..2 {
            let p = unsafe { (lib.jack_port_get_buffer)(ctx.outs[b][c], nframes) }.cast::<f32>();
            if p.is_null() {
                return 0;
            }
            ptrs[b][c] = p;
        }
    }
    // SAFETY: ten distinct output ports => ten distinct, non-overlapping
    // buffers of `nframes` samples each.
    let mut outs: [[&mut [f32]; 2]; NUM_BUSES] =
        std::array::from_fn(|b| std::array::from_fn(|c| unsafe { std::slice::from_raw_parts_mut(ptrs[b][c], n) }));
    ctx.engine.process(&mut outs);
    0
}

// The control callbacks below get the CtlCtx box owned by Backend as `arg`;
// it outlives the client, and they only touch atomics.

unsafe extern "C" fn xrun_cb(arg: *mut c_void) -> c_int {
    let ctl = unsafe { &*arg.cast::<CtlCtx>() };
    ctl.shared.xruns.fetch_add(1, Relaxed);
    0
}

unsafe extern "C" fn port_reg_cb(_port: u32, _register: c_int, arg: *mut c_void) {
    let ctl = unsafe { &*arg.cast::<CtlCtx>() };
    ctl.ports_changed.store(true, Relaxed);
}

unsafe extern "C" fn srate_cb(nframes: u32, arg: *mut c_void) -> c_int {
    let ctl = unsafe { &*arg.cast::<CtlCtx>() };
    if nframes > 0 {
        ctl.shared.sample_rate.store(nframes, Relaxed);
    }
    0
}

unsafe extern "C" fn connect_cb(_a: u32, _b: u32, _connect: c_int, arg: *mut c_void) {
    let ctl = unsafe { &*arg.cast::<CtlCtx>() };
    ctl.conns_changed.store(true, Relaxed);
}

unsafe extern "C" fn shutdown_cb(arg: *mut c_void) {
    let ctl = unsafe { &*arg.cast::<CtlCtx>() };
    ctl.shutdown.store(true, Relaxed);
}

/// Result of one auto-connect pass.
#[derive(Debug, Default, PartialEq)]
pub struct Wiring {
    /// Per row: what is connected.
    pub desc: Vec<String>,
    /// One short line per problem (or fallback), e.g. for the status line.
    pub errs: Vec<String>,
    /// A strip / bus regex matched nothing: worth listing that side's ports.
    pub missed: [bool; 2],
}

pub struct Backend {
    client: Option<JackClient>,
    rt: *mut RtCtx,
    pub ctl: Arc<CtlCtx>,
    pub in_names: [[String; 2]; NUM_STRIPS],
    pub out_names: [[String; 2]; NUM_BUSES],
    in_ports: [[Port; 2]; NUM_STRIPS],
    out_ports: [[Port; 2]; NUM_BUSES],
    pub client_name: String,
    /// What `jack_get_sample_rate` said last time (see `poll_rate`).
    rate_seen: Cell<u32>,
    /// The rate `poll_rate` reported last.
    rate_told: Cell<u32>,
}

impl Backend {
    /// `rec`: recorder tap fed from the process callback (see `bananasplit::recorder`);
    /// `vban`: the network streams' rings (see `bananasplit::vban`);
    /// `play`: recordings played back into the buses (see `bananasplit::player`).
    pub fn start(
        client: JackClient,
        shared: Arc<Shared>,
        rec: Option<Arc<RecTap>>,
        vban: Option<Arc<VbanTap>>,
        play: Option<Arc<PlayTap>>,
    ) -> Result<Backend, String> {
        let mut in_ports = [[std::ptr::null_mut(); 2]; NUM_STRIPS];
        let mut out_ports = [[std::ptr::null_mut(); 2]; NUM_BUSES];
        for (s, pair) in in_ports.iter_mut().enumerate() {
            pair[0] = client.register_audio_port(&format!("in{}_L", s + 1), true)?;
            pair[1] = client.register_audio_port(&format!("in{}_R", s + 1), true)?;
        }
        for (b, pair) in out_ports.iter_mut().enumerate() {
            pair[0] = client.register_audio_port(&format!("{}_L", BUS_IDS[b]), false)?;
            pair[1] = client.register_audio_port(&format!("{}_R", BUS_IDS[b]), false)?;
        }
        let in_names = in_ports.map(|p| p.map(|x| client.port_name(x)));
        let out_names = out_ports.map(|p| p.map(|x| client.port_name(x)));

        let ctl = Arc::new(CtlCtx {
            shared: shared.clone(),
            ports_changed: AtomicBool::new(true),
            conns_changed: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
        });
        let mut engine = Engine::new(shared);
        engine.set_sample_rate(client.sample_rate());
        if let Some(tap) = rec {
            engine.attach_recorder(tap);
        }
        if let Some(tap) = vban {
            engine.attach_vban(tap);
        }
        if let Some(tap) = play {
            engine.attach_player(tap);
        }
        let rt = Box::into_raw(Box::new(RtCtx { lib: client.lib, engine, ins: in_ports, outs: out_ports }));
        let client_name = client.name();
        let mut me = Backend {
            client: Some(client),
            rt,
            ctl,
            in_names,
            out_names,
            in_ports,
            out_ports,
            client_name,
            rate_seen: Cell::new(0),
            rate_told: Cell::new(0),
        };

        let c = me.client();
        let lib = c.lib;
        let ctl_ptr = Arc::as_ptr(&me.ctl).cast_mut().cast::<c_void>();
        // SAFETY: callbacks + context pointers stay valid until the client is
        // closed in Drop, which happens before the contexts are freed.
        unsafe {
            if (lib.jack_set_process_callback)(c.raw, Some(process_cb), me.rt.cast()) != 0 {
                return Err("jack_set_process_callback failed".into());
            }
            (lib.jack_set_xrun_callback)(c.raw, Some(xrun_cb), ctl_ptr);
            (lib.jack_set_port_registration_callback)(c.raw, Some(port_reg_cb), ctl_ptr);
            (lib.jack_set_sample_rate_callback)(c.raw, Some(srate_cb), ctl_ptr);
            (lib.jack_set_port_connect_callback)(c.raw, Some(connect_cb), ctl_ptr);
            (lib.jack_on_shutdown)(c.raw, Some(shutdown_cb), ctl_ptr);
        }
        c.activate()?;
        me.client_name = me.client().name();
        // Until the server has run the mixer once, PipeWire's libjack can
        // report the rate the mixer asked for (PIPEWIRE_LATENCY's 1/48000)
        // instead of the rate the graph really runs at, which made
        // recordings play too fast. Ask again after the first cycle; from
        // then on the sample-rate callback follows every change.
        let shared = &me.ctl.shared;
        let t0 = Instant::now();
        while shared.cycles.load(Relaxed) == 0 && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let sr = me.client().sample_rate();
        if sr > 0 {
            shared.sample_rate.store(sr, Relaxed);
        }
        me.rate_seen.set(sr);
        me.rate_told.set(shared.sample_rate.load(Relaxed));
        Ok(me)
    }

    /// The server's sample rate as the mixer knows it.
    pub fn sample_rate(&self) -> u32 {
        match self.ctl.shared.sample_rate.load(Relaxed) {
            0 => self.client().sample_rate(),
            r => r,
        }
    }

    /// Follow the server's sample rate (call a few times a second). The
    /// sample-rate callback normally reports a change; this also catches a
    /// libjack that does not call it. Returns the new rate after a change.
    pub fn poll_rate(&self) -> Option<u32> {
        let shared = &self.ctl.shared;
        let reported = self.client().sample_rate();
        // Two reads in a row must agree before a change is believed.
        if reported > 0 && reported == self.rate_seen.get() && reported != shared.sample_rate.load(Relaxed) {
            shared.sample_rate.store(reported, Relaxed);
        }
        self.rate_seen.set(reported);
        let now = shared.sample_rate.load(Relaxed);
        if now == 0 || now == self.rate_told.get() {
            return None;
        }
        let changed = self.rate_told.get() != 0;
        self.rate_told.set(now);
        changed.then_some(now)
    }

    pub fn client(&self) -> &JackClient {
        self.client.as_ref().expect("client present until drop")
    }

    /// (capture latency into strips, playback latency out of A buses), frames.
    pub fn io_latency(&self) -> (u32, u32) {
        let c = self.client();
        let cap = self.in_ports.iter().flatten().map(|&p| c.latency(p, LATENCY_CAPTURE).max).max().unwrap_or(0);
        let play = self.out_ports
            .iter()
            .flatten()
            .map(|&p| c.latency(p, LATENCY_PLAYBACK).max)
            .max()
            .unwrap_or(0);
        (cap, play)
    }

    /// Make every connection the config asks for. Idempotent; never removes
    /// connections.
    pub fn autoconnect(&self, cfg: &Config, cables: &Cables) -> Wiring {
        let c = self.client();
        let rs = Resolver::new(c, &self.client_name, CableSet::for_config(cfg).with_live(cables));
        let mut w = Wiring { desc: Vec::with_capacity(NUM_STRIPS + NUM_BUSES), ..Default::default() };
        let client_of = |p: &str| crate::devices::client_of(p).to_string();

        for row in 0..NUM_STRIPS + NUM_BUSES {
            let (side, label, connect) = if row < NUM_STRIPS {
                (Side::Strip, format!("strip {}", row + 1), &cfg.strips[row].connect)
            } else {
                (Side::Bus, format!("bus {}", BUS_IDS[row - NUM_STRIPS]), &cfg.buses[row - NUM_STRIPS].connect)
            };
            // The row's own virtual cable first (if it has one).
            let cable = cables.live(row);
            let mut targets: Vec<(Vec<String>, Option<String>)> = Vec::new();
            if let Some(spec) = cable {
                let ports = rs.ports(Some(&spec.jack_regex()), side.flag());
                let note = ports.is_empty().then(|| format!("cable \"{}\" has no ports yet", spec.description));
                targets.push((ports, note));
            }
            for pat in connect {
                let r = rs.resolve(pat, side);
                w.missed[side as usize] |= r.missed;
                targets.push((r.ports, r.note));
            }

            let mut seen = Vec::new();
            for (ports, note) in targets {
                if let Some(n) = note {
                    w.errs.push(format!("{label}: {n}"));
                }
                let res = match (side, ports.as_slice()) {
                    (_, []) => continue,
                    (Side::Strip, [p]) => self.connect_pair(p, p, &self.in_names[row], true),
                    (Side::Strip, [p, q, ..]) => self.connect_pair(p, q, &self.in_names[row], true),
                    (Side::Bus, [p]) => self.connect_pair(p, p, &self.out_names[row - NUM_STRIPS], false),
                    (Side::Bus, [p, q, ..]) => self.connect_pair(p, q, &self.out_names[row - NUM_STRIPS], false),
                };
                match res {
                    Ok(()) => seen.push(client_of(&ports[0])),
                    Err(e) => w.errs.push(e),
                }
            }
            w.desc.push(if seen.is_empty() { "(nothing connected)".into() } else { seen.join(" + ") });
        }
        w
    }

    /// Connect external ports `a`/`b` to our L/R (strip) or our L/R to them (bus).
    fn connect_pair(&self, a: &str, b: &str, ours: &[String; 2], into_strip: bool) -> Result<(), String> {
        let c = self.client();
        let [l, r] = ours;
        if into_strip {
            c.connect(a, l).and_then(|_| c.connect(b, r))
        } else {
            c.connect(l, a).and_then(|_| c.connect(r, b))
        }
    }

    /// The ports one side can use, one line per JACK client (for messages).
    pub fn port_listing(&self, cfg: &Config, side: Side) -> Vec<String> {
        Resolver::new(self.client(), &self.client_name, CableSet::for_config(cfg)).listing(side)
    }

    /// Channel `ch` (strips 0-5, then buses): its two ports, their names
    /// and its side.
    fn channel(&self, ch: usize) -> Option<(&[Port; 2], &[String; 2], Side)> {
        if ch < NUM_STRIPS {
            Some((&self.in_ports[ch], &self.in_names[ch], Side::Strip))
        } else {
            let b = ch.checked_sub(NUM_STRIPS).filter(|&b| b < NUM_BUSES)?;
            Some((&self.out_ports[b], &self.out_names[b], Side::Bus))
        }
    }

    /// The JACK clients channel `ch` is connected to now (by anyone), in
    /// the order JACK lists them.
    pub fn connected_clients(&self, ch: usize) -> Vec<String> {
        let Some((ports, _, _)) = self.channel(ch) else { return Vec::new() };
        let mut out: Vec<String> = Vec::new();
        for &p in ports {
            for other in self.client().connections(p) {
                let c = client_of(&other).to_string();
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// Connect channel `ch` to JACK client `client`'s first two ports (one
    /// port: to both sides), like a `connect = ^client:` line does.
    pub fn connect_client(&self, ch: usize, client: &str) -> Result<(), String> {
        let (_, ours, side) = self.channel(ch).ok_or("bad channel")?;
        let mut ports = self.client().ports(Some(&format!("^{}:", regex_escape(client))), side.flag());
        if ports.is_empty() {
            return Err(format!("\"{client}\" is gone"));
        }
        // a device's capture ports before the monitor ports of its other side
        ports.sort_by_key(|p| devices::is_monitor_port(p));
        let (a, b) = (&ports[0], ports.get(1).unwrap_or(&ports[0]));
        self.connect_pair(a, b, ours, side == Side::Strip)
    }

    /// Remove every connection between channel `ch` and JACK client
    /// `client`. Returns how many were removed.
    pub fn disconnect_client(&self, ch: usize, client: &str) -> Result<usize, String> {
        let (ports, ours, side) = self.channel(ch).ok_or("bad channel")?;
        let c = self.client();
        let mut n = 0;
        for (k, &p) in ports.iter().enumerate() {
            for other in c.connections(p) {
                if client_of(&other) != client {
                    continue;
                }
                match side {
                    Side::Strip => c.disconnect(&other, &ours[k])?,
                    Side::Bus => c.disconnect(&ours[k], &other)?,
                }
                n += 1;
            }
        }
        Ok(n)
    }

    /// Everything channel side `side` can be connected to (see
    /// [`Resolver::candidates`]); `devs`: what pactl said (None: unknown).
    pub fn candidates(&self, cfg: &Config, cables: &Cables, devs: Option<devices::Devices>, side: Side) -> Vec<devices::Candidate> {
        Resolver::new(self.client(), &self.client_name, CableSet::for_config(cfg).with_live(cables)).with_devices(devs).candidates(side)
    }

    /// Does `pattern` (a `connect` value) take in JACK client `client` on
    /// channel `ch`'s side right now? `devs`: what pactl said.
    pub fn pattern_reaches(&self, cfg: &Config, cables: &Cables, devs: Option<devices::Devices>, ch: usize, pattern: &str, client: &str) -> bool {
        let Some((_, _, side)) = self.channel(ch) else { return false };
        let rs = Resolver::new(self.client(), &self.client_name, CableSet::for_config(cfg).with_live(cables)).with_devices(devs);
        rs.resolve(pattern, side).ports.iter().take(2).any(|p| client_of(p) == client)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Close the client first: after jack_client_close returns, no
        // callback can run, so freeing the contexts is safe.
        drop(self.client.take());
        // SAFETY: created by Box::into_raw in start(), freed exactly once.
        unsafe { drop(Box::from_raw(self.rt)) };
    }
}
