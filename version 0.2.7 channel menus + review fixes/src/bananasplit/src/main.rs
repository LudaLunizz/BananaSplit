//! BananaSplit — 6-strip / 5-bus virtual audio mixer for Linux (JACK / PipeWire).

mod backend;
mod cables;
mod chanmenu;
mod desktop;
mod devices;
mod gui;
mod jack;
mod lowlat;
mod shell;
mod sys;
mod tui;

use backend::{Backend, Wiring};
use cables::{CableSet, Cables};
use devices::Side;
use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use bananasplit::config::Config;
use bananasplit::engine::{NUM_STRIPS, Shared};
use bananasplit::player::{PlayTap, Player};
use bananasplit::recorder::{self, RecTap, Recorder};
use bananasplit::scenes::SceneStore;
use bananasplit::vban::{InState, NUM_IN, NUM_OUT, OutState, Vban, VbanConfig, VbanStatus, VbanTap};

const HELP: &str = "\
BananaSplit — 6-strip, 5-bus low-latency virtual audio mixer (JACK / PipeWire),
with an EQ on every strip and bus and VBAN network audio (Voicemeeter)

USAGE:
    bananasplit [--config FILE] [--no-cables]          terminal UI
    bananasplit [--config FILE] [--no-cables] --gui [--hidden | --no-open] [--gui-port PORT]
    bananasplit [--config FILE] [--no-cables] --headless
    bananasplit [--config FILE] --list-ports
    bananasplit [--config FILE] --cleanup
    bananasplit --low-latency [off]
    bananasplit --install | --uninstall
    bananasplit --quit [--gui-port PORT]
    bananasplit --print-default-config

OPTIONS:
    -c, --config FILE        Config file (default: $XDG_CONFIG_HOME/bananasplit/bananasplit.ini)
        --gui                The mixer as a desktop app: its own window and a tray
                             icon (the page is served on http://127.0.0.1:8790,
                             this computer only). No terminal needed. If it is
                             already running, this brings its window up.
        --hidden             With --gui: start in the tray, without a window
                             (what the GUI's Start with the system uses)
        --gui-port PORT      Port for --gui (default 8790)
        --no-open            With --gui: no window and no tray icon (open the URL yourself)
        --headless           No UI; run until SIGINT/SIGTERM (for systemd)
        --no-cables          Do not create PipeWire virtual cables this time
                             (the config file is not changed)
        --list-ports         Show the JACK port names `connect =` can use, with a
                             ready-made line for each device, then exit
        --cleanup            Remove virtual cables left behind by a BananaSplit that
                             was killed (refuses while it is running), then exit
        --low-latency [off]  Install PipeWire settings for 128-frame periods
                             (< 10 ms round trip) and apply them now; `off` undoes it
        --install            Copy BananaSplit to ~/.local/bin and add it to the app
                             menu (with its icon); --uninstall removes that again
        --quit               Quit the BananaSplit GUI that is running
        --print-default-config
    -h, --help
    -V, --version
";

#[derive(Debug, PartialEq)]
enum Mode {
    Run,
    ListPorts,
    Cleanup,
    LowLatency(bool),
    Install,
    Uninstall,
    QuitRunning,
}

struct Args {
    config: PathBuf,
    mode: Mode,
    headless: bool,
    /// Runtime only: never written into the config file.
    no_cables: bool,
    gui: bool,
    gui_port: Option<u16>,
    no_open: bool,
    hidden: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut a = Args {
        config: Config::default_path(),
        mode: Mode::Run,
        headless: false,
        no_cables: false,
        gui: false,
        gui_port: None,
        no_open: false,
        hidden: false,
    };
    let mut it = std::env::args().skip(1).peekable();
    let set_mode = |a: &mut Args, m: Mode| {
        if a.mode != Mode::Run {
            return Err("--list-ports, --cleanup, --low-latency, --install, --uninstall and --quit are separate commands; give only one".to_string());
        }
        a.mode = m;
        Ok(())
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-c" | "--config" => a.config = it.next().ok_or("--config needs a file")?.into(),
            "--headless" => a.headless = true,
            "--no-cables" => a.no_cables = true,
            "--gui" => a.gui = true,
            "--no-open" => a.no_open = true,
            "--hidden" => a.hidden = true,
            "--list-ports" => set_mode(&mut a, Mode::ListPorts)?,
            "--cleanup" => set_mode(&mut a, Mode::Cleanup)?,
            "--install" => set_mode(&mut a, Mode::Install)?,
            "--uninstall" => set_mode(&mut a, Mode::Uninstall)?,
            "--quit" => set_mode(&mut a, Mode::QuitRunning)?,
            "--low-latency" => {
                let on = match it.peek().map(String::as_str) {
                    Some("off") => false,
                    Some("on") => true,
                    _ => {
                        set_mode(&mut a, Mode::LowLatency(true))?;
                        continue;
                    }
                };
                it.next();
                set_mode(&mut a, Mode::LowLatency(on))?;
            }
            "--gui-port" => {
                let v = it.next().ok_or("--gui-port needs a port number")?;
                a.gui_port = Some(v.parse().ok().filter(|&p| p > 0).ok_or(format!("bad port {v:?}"))?);
            }
            "--print-default-config" => {
                print!("{}", Config::default().to_ini());
                return Ok(None);
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("BananaSplit {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other:?} (see --help)")),
        }
    }
    if a.gui && a.headless {
        return Err("--gui and --headless cannot be combined (for a background service with a GUI use --gui --no-open)".into());
    }
    if a.mode == Mode::QuitRunning && !(a.gui || a.headless || a.no_cables || a.no_open || a.hidden) {
        return Ok(Some(a)); // --quit [--gui-port PORT]
    }
    if !a.gui && (a.no_open || a.gui_port.is_some() || a.hidden) {
        return Err("--no-open, --hidden and --gui-port only work together with --gui".into());
    }
    if a.no_open && a.hidden {
        return Err("--hidden starts in the tray, --no-open without a window or tray: give only one".into());
    }
    if a.mode != Mode::Run && (a.gui || a.headless || a.no_cables) {
        return Err("--list-ports, --cleanup, --low-latency, --install, --uninstall and --quit do not run the mixer; drop --gui/--headless/--no-cables".into());
    }
    Ok(Some(a))
}

const ERROR_DIALOG_FLAG: &str = "--error-dialog";

fn main() -> ExitCode {
    // Helper processes BananaSplit starts itself: the cable watchdog
    // (cables.rs) and the app window (shell.rs).
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some(cables::REAPER_FLAG) => return cables::reaper_main(argv.get(1).map_or("", String::as_str)),
        Some(gui::WINDOW_FLAG) => return shell::main(&argv[1..]),
        Some(ERROR_DIALOG_FLAG) => {
            shell::error_dialog(argv.get(1).map_or("", String::as_str));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bananasplit: {e}");
            return ExitCode::FAILURE;
        }
    };
    let gui = args.gui;
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bananasplit: {e}");
            // Started from the app menu: there is no terminal to read this.
            if gui && !sys::stdin_is_tty() {
                show_error(&e);
            }
            ExitCode::FAILURE
        }
    }
}

/// The error dialog runs in its own process: this one may hold a JACK
/// client's leftovers, and a toolkit has no business in the mixer process.
fn show_error(e: &str) {
    let text = format!("BananaSplit could not start.\n\n{e}");
    if let Ok(exe) = desktop::helper_exe() {
        use std::os::unix::process::CommandExt;
        if std::process::Command::new(exe).arg0("bananasplit").arg(ERROR_DIALOG_FLAG).arg(&text).status().is_ok() {
            return;
        }
    }
    shell::error_dialog(&text);
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `bananasplit --list-ports`.
fn list_ports(path: &Path) -> Result<(), String> {
    let cfg = Config::load_or_default(path)?;
    let client = jack::JackClient::open_aux("bananasplit-ports")?;
    let rs = devices::Resolver::new(&client, &cfg.client_name, CableSet::for_config(&cfg));
    // (a closed pipe, `bananasplit --list-ports | head`, is not an error)
    let _ = std::io::stdout().write_all(rs.report().as_bytes());
    Ok(())
}

/// `bananasplit --cleanup`: remove cables a killed BananaSplit left behind.
fn cleanup(path: &Path) -> Result<(), String> {
    let cfg = Config::load_or_default(path)?;
    let name = &cfg.client_name;
    // A watchdog that is still there after a few seconds belongs to a BananaSplit
    // that is running (or one that is stuck); never pull cables from under it.
    let running = || format!("BananaSplit \"{name}\" is running; its cables are removed when it quits");
    if !cables::wait_for_reapers(name, Duration::from_secs(3)) {
        return Err(running());
    }
    if let Ok(c) = jack::JackClient::open_aux("bananasplit-cleanup") {
        if !c.ports(Some(&format!("^{}:in1_L$", jack::regex_escape(name))), 0).is_empty() {
            return Err(running());
        }
    }
    let n = cables::sweep(&CableSet::for_config(&cfg).node_names())?;
    println!("bananasplit: removed {} of \"{name}\"", plural(n, "virtual cable", "virtual cables"));
    Ok(())
}

fn run(args: Args) -> Result<(), String> {
    match args.mode {
        Mode::ListPorts => return list_ports(&args.config),
        Mode::Cleanup => return cleanup(&args.config),
        Mode::LowLatency(on) => return lowlat::command(on),
        Mode::Install => {
            println!("bananasplit: {}", desktop::install()?);
            return Ok(());
        }
        Mode::Uninstall => {
            println!("bananasplit: {}", desktop::uninstall()?);
            return Ok(());
        }
        Mode::QuitRunning => {
            let client = Config::load_or_default(&args.config).unwrap_or_default().client_name;
            let Some(port) = gui::find_running(&client, args.gui_port) else {
                return Err("no BananaSplit GUI is running".into());
            };
            return match gui::request_quit(port) {
                Some((200, _)) => Ok(()),
                Some((code, body)) => Err(format!("the BananaSplit on port {port} answered {code}: {body}")),
                None => Err(format!("no BananaSplit GUI is running on port {port}")),
            };
        }
        Mode::Run => {}
    }
    // Upgrading from vmix (2.4 and older): bring its settings along once.
    let migrated = if args.config == Config::default_path() {
        Config::migrate_legacy(&args.config, &Config::legacy_path()).unwrap_or_else(|e| Some(format!("vmix settings not copied: {e}")))
    } else {
        None
    };
    let mut cfg = Config::load_or_create(&args.config)?;
    // `--no-cables` applies to this run only. It is deliberately not stored
    // in `cfg`, which is written back to the file on save and on quit.
    let create_cables = cfg.create_cables && !args.no_cables;
    if !args.headless && !args.gui && !sys::stdin_is_tty() {
        return Err("not running in a terminal; use --gui or --headless".into());
    }
    let gui_port = args.gui_port.unwrap_or(gui::DEFAULT_PORT);
    let running = if args.gui { gui::find_running(&cfg.client_name, args.gui_port) } else { None };
    if let Some(gui_port) = running {
        // Already running (launched again from the app menu, or the
        // autostart entry while it runs): bring its window up.
        let url = gui::url(gui_port);
        if args.hidden {
            return Ok(());
        }
        let shown = !args.no_open && (gui::request_show(gui_port, &launch_token()) || gui::open_window(&url));
        if !shown {
            eprintln!("bananasplit: already running; open {url}");
        }
        return Ok(());
    }
    sys::install_signal_handlers();

    // Ask PipeWire for a small period for our node (honoured as long as it is
    // within the server's min/max quantum). Harmless under JACK2.
    if cfg.buffer_size > 0 && std::env::var_os("PIPEWIRE_LATENCY").is_none() {
        // SAFETY: still single-threaded at this point.
        unsafe { std::env::set_var("PIPEWIRE_LATENCY", format!("{}/48000", cfg.buffer_size)) };
    }

    // Started with the session (--hidden): the audio server may still be
    // starting, so keep trying for half a minute.
    let client = {
        let t0 = Instant::now();
        loop {
            match jack::JackClient::open(&cfg.client_name) {
                Ok(c) => break c,
                Err(e) if args.hidden && t0.elapsed() < Duration::from_secs(30) && !e.contains("already exists") => {
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(e) => return Err(e),
            }
        }
    };
    let mut notes: Vec<String> = migrated.into_iter().collect();
    if cfg.force_buffer_size && cfg.buffer_size > 0 && client.buffer_size() != cfg.buffer_size {
        if let Err(e) = client.set_buffer_size(cfg.buffer_size) {
            notes.push(e);
        }
    }

    // We own the JACK name now, so cables with our names that are still
    // loaded were left by a BananaSplit that was killed: let its watchdog finish,
    // then remove whatever is left (also with --no-cables).
    let cable_set = CableSet::for_config(&cfg);
    if !cables::wait_for_reapers(&cfg.client_name, Duration::from_secs(3)) {
        notes.push("the cable watchdog of an earlier BananaSplit is still running".into());
    }
    if let Ok(n @ 1..) = cables::sweep(&cable_set.node_names()) {
        notes.push(format!("removed {} left behind by a BananaSplit that was killed", plural(n, "virtual cable", "virtual cables")));
    }
    // Cables before ports so they exist when we auto-connect.
    let (cables, warn) = if create_cables {
        Cables::create(&cfg.client_name, &cable_set)
    } else if args.no_cables {
        (Cables::none("virtual cables are off for this run (--no-cables)"), Vec::new())
    } else {
        (Cables::none("virtual cables are off (create_cables = false in the config)"), Vec::new())
    };
    notes.extend(warn);
    let cables_active = cables.count() > 0;

    let shared = Shared::new();
    cfg.apply(&shared);
    // The recorder exists only when there is a UI to drive it; --headless
    // runs the same audio path without it. VBAN runs in every mode (nothing
    // is sent or received while it is off, the default).
    let tap = if args.headless { None } else { Some(RecTap::new()) };
    let vban_tap = VbanTap::new();
    // The player has a control only in the GUI.
    let play_tap = args.gui.then(PlayTap::new);
    let backend = Backend::start(client, shared.clone(), tap.clone(), Some(vban_tap.clone()), play_tap.clone())?;
    // The rate the server really runs the mixer at (see Backend::start).
    let rate = backend.sample_rate();
    let recorder = tap.map(|t| Arc::new(Recorder::new(t, rate, cfg.recorder.settings())));
    let vban = Arc::new(Vban::start(vban_tap, rate, cfg.vban.clone()));
    let player = play_tap.map(|t| {
        let p = Arc::new(Player::new(t, rate));
        p.set_routes(cfg.recorder.play_to);
        p
    });

    // Give PipeWire a moment to publish the cable ports.
    if cables_active {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let all = (0..cables::NUM_CHANNELS)
                .filter_map(|ch| cables.live(ch))
                .all(|c| !backend.client().ports(Some(&c.jack_regex()), 0).is_empty());
            if all {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let cables = RefCell::new(cables);
    let ctx = Ctx { backend: &backend, path: &args.config, cables: &cables, shared: &shared, vban: &vban };
    let result = match (&recorder, &player) {
        (None, _) => run_headless(&ctx, &cfg, notes),
        (Some(rec), Some(player)) => {
            let opts = GuiOpts { port: gui_port, explicit: args.gui_port.is_some(), open: !args.no_open, hidden: args.hidden };
            run_gui(&ctx, &mut cfg, rec, player, notes, &opts)
        }
        (Some(rec), None) => run_tui(&ctx, &mut cfg, rec, notes),
    };
    if let Some(p) = &player {
        p.shutdown();
    }
    vban.shutdown(); // stop the network threads (the engine just stops using the rings)
    if let Some(rec) = &recorder {
        rec.shutdown(); // finish the files while the audio thread still runs
    }
    drop(backend); // close JACK first...
    drop(cables); // ...then remove the cables
    result
}

/// What the UI loops share.
struct Ctx<'a> {
    backend: &'a Backend,
    path: &'a Path,
    /// The virtual cables (switched on and off from the app's channel menus).
    cables: &'a RefCell<Cables>,
    shared: &'a Arc<Shared>,
    vban: &'a Arc<Vban>,
}

// ---------------------------------------------------------------------------
// VBAN status texts
// ---------------------------------------------------------------------------

/// This computer's main IPv4 address (what the other computer sends to).
fn main_ip(ips: &[sys::LocalIp]) -> Option<String> {
    ips.first().map(|a| a.ip.to_string())
}

/// The line under the terminal UI's header ("" while VBAN is off).
fn vban_summary(st: &VbanStatus, cfg: &VbanConfig, ips: &[sys::LocalIp]) -> String {
    if !cfg.enabled {
        return String::new();
    }
    if let Some(e) = &st.error {
        return format!("\x1b[33mVBAN: {e}\x1b[0m");
    }
    let ins: Vec<usize> = (0..NUM_IN).filter(|&k| cfg.ins[k].on).collect();
    let outs: Vec<usize> = (0..NUM_OUT).filter(|&k| cfg.outs[k].on).collect();
    let recv = ins.iter().filter(|&&k| st.ins[k].state == InState::Receiving).count();
    let sending = outs.iter().filter(|&&k| st.outs[k].state == OutState::Sending).count();
    let port = match (st.listening, main_ip(ips)) {
        (Some(p), Some(ip)) => format!("{ip}:{p}"),
        (Some(p), None) => format!("UDP {p}"),
        (None, _) => "not listening yet".to_string(),
    };
    let mut s = format!(
        "\x1b[1mVBAN\x1b[0m {port} · in: {recv} of {} receiving · out: {sending} of {} sending",
        ins.len(),
        outs.len()
    );
    let problem = ins
        .iter()
        .find_map(|&k| (st.ins[k].state != InState::Receiving).then(|| st.ins[k].error.clone()).flatten())
        .or_else(|| outs.iter().find_map(|&k| st.outs[k].error.clone()));
    if let Some(p) = problem {
        s.push_str(&format!(" · \x1b[33m{p}\x1b[0m"));
    }
    s
}

/// VBAN streams that come into or go out of channel `row`.
fn vban_links(st: &VbanStatus, cfg: &VbanConfig, sample_rate: u32, row: usize) -> Vec<String> {
    let mut v = Vec::new();
    if !cfg.enabled {
        return v;
    }
    if row < NUM_STRIPS {
        for (k, c) in cfg.ins.iter().enumerate() {
            if c.on && c.strip == row {
                v.push(format!("VBAN in{} \"{}\" ({})", k + 1, c.name, st.ins[k].describe(c)));
            }
        }
    }
    for (k, c) in cfg.outs.iter().enumerate() {
        if c.on && c.source == row {
            v.push(format!("VBAN out{} \"{}\" ({})", k + 1, c.name, st.outs[k].describe(c, sample_rate)));
        }
    }
    v
}

/// The JACK connections of every row, with its VBAN streams added.
fn with_vban(conn: &[String], st: &VbanStatus, cfg: &VbanConfig, sample_rate: u32) -> Vec<String> {
    (0..tui::ROWS)
        .map(|r| {
            let jack = conn.get(r).map_or("", String::as_str);
            let v = vban_links(st, cfg, sample_rate, r);
            match (jack, v.is_empty()) {
                (_, true) => jack.to_string(),
                ("" | "(nothing connected)", false) => v.join(" + "),
                _ => format!("{jack} + {}", v.join(" + ")),
            }
        })
        .collect()
}

/// VBAN state for a log (stderr): one line per stream that is on, without
/// the numbers that change all the time, so a line is printed only when
/// something actually happens.
fn vban_events(st: &VbanStatus, cfg: &VbanConfig, sample_rate: u32, ips: &[sys::LocalIp]) -> Vec<String> {
    let mut v = Vec::new();
    if !cfg.enabled {
        return v;
    }
    if let Some(e) = &st.error {
        v.push(format!("vban: {e}"));
    } else if let Some(p) = st.listening {
        let here: Vec<String> = ips.iter().map(|a| format!("{} ({})", a.ip, a.iface)).collect();
        v.push(if here.is_empty() {
            format!("vban: listening on UDP port {p}")
        } else {
            format!("vban: listening on UDP port {p}; this computer is {}", here.join(", "))
        });
    }
    for (k, c) in cfg.ins.iter().enumerate().filter(|(_, c)| c.on) {
        let s = &st.ins[k];
        let what = match s.state {
            InState::Receiving => format!(
                "receiving {} Hz, {} ch, {} from {}",
                s.sample_rate,
                s.channels,
                s.format.map_or("", |f| f.label()),
                s.from.map_or(String::new(), |a| a.ip().to_string())
            ),
            _ => s.describe(c),
        };
        v.push(format!("vban in{} \"{}\" -> strip {}: {what}", k + 1, c.name, c.strip + 1));
    }
    for (k, c) in cfg.outs.iter().enumerate().filter(|(_, c)| c.on) {
        let s = &st.outs[k];
        let what = match s.state {
            OutState::Sending => format!("sending {} {}", recorder::source_id(c.source), s.describe(c, sample_rate)),
            _ => s.describe(c, sample_rate),
        };
        v.push(format!("vban out{} \"{}\": {what}", k + 1, c.name));
    }
    v
}

/// Prints VBAN state changes on stderr (headless and GUI modes).
#[derive(Default)]
struct VbanLog {
    last: Vec<String>,
    at: Option<Instant>,
    ips: Vec<sys::LocalIp>,
    ips_at: Option<Instant>,
}

impl VbanLog {
    fn poll(&mut self, vban: &Vban) {
        if self.at.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) || !vban.settled() {
            return;
        }
        self.at = Some(Instant::now());
        if self.ips_at.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.ips = sys::local_ipv4();
            self.ips_at = Some(Instant::now());
        }
        let lines = vban_events(&vban.status(), &vban.config(), vban.sample_rate(), &self.ips);
        for l in lines.iter().filter(|l| !self.last.contains(l)) {
            eprintln!("bananasplit: {l}");
        }
        self.last = lines;
    }
}

fn run_headless(ctx: &Ctx, cfg: &Config, notes: Vec<String>) -> Result<(), String> {
    let backend = ctx.backend;
    for n in notes {
        eprintln!("bananasplit: warning: {n}");
    }
    let c = backend.client();
    let sr = backend.sample_rate();
    eprintln!(
        "bananasplit: running as JACK client \"{}\" at {} Hz, {} frames ({:.2} ms period). Ctrl-C to stop.",
        backend.client_name,
        sr,
        c.buffer_size(),
        c.buffer_size() as f64 * 1000.0 / sr.max(1) as f64
    );
    let mut last_errs: Vec<String> = Vec::new();
    let mut changed_at: Option<Instant> = Some(Instant::now());
    let mut lat = LatencyCheck::new();
    let mut vlog = VbanLog::default();
    while !sys::QUIT.load(SeqCst) {
        if backend.ctl.shutdown.load(Relaxed) {
            return Err("the JACK/PipeWire server shut down".into());
        }
        if backend.ctl.ports_changed.swap(false, Relaxed) {
            changed_at = Some(Instant::now());
        }
        if changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            changed_at = None;
            let w = backend.autoconnect(cfg, &ctx.cables.borrow());
            print_wiring(backend, cfg, &w, &mut last_errs);
        }
        if let Some(m) = lat.poll(backend, cfg) {
            eprintln!("bananasplit: warning: {m}");
        }
        if let Some(m) = rate_change(backend) {
            eprintln!("bananasplit: {m}");
        }
        vlog.poll(ctx.vban);
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Problems from an auto-connect pass on stderr (when they change). When a
/// pattern matched nothing, also list the ports that side could use.
fn print_wiring(backend: &Backend, cfg: &Config, w: &Wiring, last: &mut Vec<String>) {
    if w.errs == *last {
        return;
    }
    for e in &w.errs {
        eprintln!("bananasplit: {e}");
    }
    for (side, what) in [(Side::Strip, "a strip can take (JACK outputs)"), (Side::Bus, "a bus can feed (JACK inputs)")] {
        if w.missed[side as usize] {
            eprintln!("bananasplit:   ports {what}:");
            for l in backend.port_listing(cfg, side) {
                eprintln!("bananasplit:     {l}");
            }
        }
    }
    *last = w.errs.clone();
}

/// Looks at the latency once, shortly after start (JACK reports port
/// latencies only once the ports are connected).
struct LatencyCheck {
    at: Option<Instant>,
}

impl LatencyCheck {
    fn new() -> Self {
        LatencyCheck { at: Some(Instant::now() + Duration::from_millis(1500)) }
    }

    fn poll(&mut self, backend: &Backend, cfg: &Config) -> Option<String> {
        if self.at.is_none_or(|t| Instant::now() < t) {
            return None;
        }
        self.at = None;
        let c = backend.client();
        let (lat_in, lat_out) = backend.io_latency();
        lowlat::advice(&lowlat::Facts {
            sample_rate: backend.sample_rate(),
            period: c.buffer_size(),
            requested: cfg.buffer_size,
            lat_in,
            lat_out,
            pipewire: cables::pactl(&["info"]).ok().map(|i| i.contains("PipeWire")),
            state: lowlat::state(&lowlat::pw_dir()),
        })
    }
}

/// A line for the user when the audio server changed its sample rate.
fn rate_change(backend: &Backend) -> Option<String> {
    backend.poll_rate().map(|r| {
        format!("the audio server now runs at {r} Hz; recording, VBAN and playback follow it")
    })
}

fn append(status: &mut String, msg: &str) {
    if !status.is_empty() {
        status.push_str(" | ");
    }
    status.push_str(msg);
}

/// The 11 channel names, numbered like the recorder sources (strips, then buses).
fn channel_names(cfg: &Config) -> Vec<String> {
    cfg.strips.iter().map(|s| s.name.clone()).chain(cfg.buses.iter().map(|b| b.name.clone())).collect()
}

/// Mixer state, recorder and VBAN settings into the config (before saving).
fn capture_all(cfg: &mut Config, shared: &Shared, rec: &Recorder, vban: &Vban) {
    cfg.capture(shared);
    cfg.recorder.set_settings(&rec.settings());
    cfg.vban = vban.config();
}

/// Switch VBAN on or off; returns a message for the status line.
fn toggle_vban(vban: &Vban) -> String {
    let mut on = false;
    let _ = vban.update(|c| {
        c.enabled = !c.enabled;
        on = c.enabled;
        Ok(())
    });
    if on {
        let c = vban.config();
        let (i, o) = (c.ins.iter().filter(|s| s.on).count(), c.outs.iter().filter(|s| s.on).count());
        format!("VBAN on (UDP port {}): {i} incoming, {o} outgoing streams", c.port)
    } else {
        "VBAN off".into()
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |f| f.to_string_lossy().into_owned())
}

/// Start or stop recording; returns a message for the status line.
fn toggle_record(rec: &Recorder, shared: &Shared, names: &[String]) -> Result<String, String> {
    if rec.is_recording() {
        rec.stop();
        return Ok("recording stopped".into());
    }
    let files = rec.start(shared.rec_links.load(Relaxed), names, &sys::local_timestamp())?;
    // Short: the status line must fit 80 columns (the folder is on the REC line).
    Ok(match files.len() {
        1 => format!("recording {}", files.first().map_or(String::new(), |f| file_name(f))),
        n => format!("recording {n} tracks, one file per channel"),
    })
}

/// Recorder status for the terminal UI.
fn rec_line(rec: &Recorder, shared: &Shared) -> tui::RecLine {
    let st = rec.status();
    let set = rec.settings();
    if st.on {
        let t = st.seconds() as u64;
        let mut text = format!("{:02}:{:02}:{:02}  {}  ", t / 3600, t / 60 % 60, t % 60, st.mode.as_str());
        if let Some(f) = st.files.first() {
            text.push_str(&file_name(f));
        }
        if st.files.len() > 1 {
            text.push_str(&format!(" +{}", st.files.len() - 1));
        }
        if st.dropped > 0 {
            text.push_str(&format!("  ({} frames lost: disk too slow)", st.dropped));
        }
        if st.clipped > 0 {
            text.push_str("  CLIPPING: lower a gain or use wav32f");
        }
        tui::RecLine { on: true, text }
    } else if let Some(e) = &st.error {
        tui::RecLine { on: false, text: e.clone() }
    } else {
        let links = recorder::format_links(shared.rec_links.load(Relaxed));
        tui::RecLine {
            on: false,
            text: format!("{} · {} · links {links} → {}", set.mode.as_str(), set.format.as_str(), set.folder),
        }
    }
}

fn run_tui(ctx: &Ctx, cfg: &mut Config, rec: &Recorder, notes: Vec<String>) -> Result<(), String> {
    let (backend, path, shared, vban) = (ctx.backend, ctx.path, &**ctx.shared, &**ctx.vban);
    let _term = sys::RawTerminal::enter()?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 64];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut view = tui::View::default();
    if !notes.is_empty() {
        view.status = format!("\x1b[33m{}\x1b[0m", notes.join(" | "));
    }
    let mut pending = Vec::new();
    let mut conn: Vec<String> = Vec::new();
    let mut changed_at: Option<Instant> = Some(Instant::now());
    let mut force_connect = false;
    let mut lat = (0u32, 0u32);
    let mut lat_at = Instant::now() - Duration::from_secs(10);
    let mut lat_check = LatencyCheck::new();
    let mut ips = sys::local_ipv4();
    let mut ips_at = Instant::now();
    let mut out = std::io::stdout();
    let frame = Duration::from_millis(33);

    loop {
        let t0 = Instant::now();
        if sys::QUIT.load(SeqCst) {
            break;
        }
        if backend.ctl.shutdown.load(Relaxed) {
            return Err("the JACK/PipeWire server shut down".into());
        }

        while let Ok(bytes) = rx.try_recv() {
            pending.extend_from_slice(&bytes);
        }
        let mut quit = false;
        for k in tui::decode(&mut pending) {
            match view.handle(shared, k) {
                tui::Action::None => {}
                tui::Action::Quit => quit = true,
                tui::Action::Reconnect => force_connect = true,
                tui::Action::Save => {
                    capture_all(cfg, shared, rec, vban);
                    view.status = match cfg.save(path) {
                        Ok(()) => format!("saved {}", path.display()),
                        Err(e) => format!("\x1b[31msave failed: {e}\x1b[0m"),
                    };
                }
                tui::Action::Record => {
                    view.status = match toggle_record(rec, shared, &channel_names(cfg)) {
                        Ok(m) => m,
                        Err(e) => format!("\x1b[31mrecorder: {e}\x1b[0m"),
                    };
                }
                tui::Action::ToggleVban => view.status = toggle_vban(vban),
                tui::Action::Note(m) => view.status = m,
            }
        }
        if quit {
            break;
        }

        if backend.ctl.ports_changed.swap(false, Relaxed) {
            changed_at = Some(Instant::now());
        }
        if force_connect || changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            let w = backend.autoconnect(cfg, &ctx.cables.borrow());
            conn = w.desc;
            if force_connect || !w.errs.is_empty() {
                view.status = if w.errs.is_empty() {
                    "all configured connections made".into()
                } else {
                    format!("\x1b[33m{}\x1b[0m", w.errs.join(" | "))
                };
            }
            force_connect = false;
            changed_at = None;
            lat_at = Instant::now() - Duration::from_secs(10); // refresh latency now
        }
        if lat_at.elapsed() > Duration::from_millis(500) {
            lat = backend.io_latency();
            lat_at = Instant::now();
        }
        if let Some(m) = lat_check.poll(backend, cfg) {
            view.latency = Some(m);
        }
        if let Some(m) = rate_change(backend) {
            view.status = m;
        }

        view.cols = sys::term_cols().unwrap_or(tui::MIN_COLS);
        view.update_meters(shared);
        view.rec = rec_line(rec, shared);
        let c = backend.client();
        let (vst, vcfg) = (vban.status(), vban.config());
        if ips_at.elapsed() > Duration::from_secs(2) {
            ips = sys::local_ipv4();
            ips_at = Instant::now();
        }
        let vline = vban_summary(&vst, &vcfg, &ips);
        let info = tui::Info {
            client: &backend.client_name,
            sample_rate: backend.sample_rate(),
            buffer_size: c.buffer_size(),
            cpu: c.cpu_load(),
            xruns: shared.xruns.load(Relaxed),
            oversize: shared.oversize_cycles.load(Relaxed),
            lat_in: lat.0,
            lat_out: lat.1,
            cables: ctx.cables.borrow().count(),
            vban: &vline,
        };
        let screen = view.render(shared, cfg, &info, &with_vban(&conn, &vst, &vcfg, backend.sample_rate()));
        let _ = out.write_all(screen.as_bytes());
        let _ = out.flush();

        if let Some(rest) = frame.checked_sub(t0.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    // Remember the mixer state for next time.
    capture_all(cfg, shared, rec, vban);
    cfg.save(path)
}

struct GuiOpts {
    port: u16,
    explicit: bool,
    /// Open the window (and the tray icon).
    open: bool,
    /// Start in the tray.
    hidden: bool,
}

/// The startup / activation token the desktop gave this launch, so the
/// window may come to the front (X11: DESKTOP_STARTUP_ID, Wayland:
/// XDG_ACTIVATION_TOKEN).
fn launch_token() -> String {
    ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|t| !t.is_empty()))
        .unwrap_or_default()
}

/// The window: our own (with a tray icon), a browser's, or none.
struct Window {
    proc: Option<gui::WindowProc>,
    url: String,
    browser: bool,
    /// For the tray of a window process started while recording.
    recording: bool,
}

impl Window {
    fn kind(&self) -> &'static str {
        match &self.proc {
            Some(p) if p.mode == gui::WinMode::App => "app",
            _ if self.browser => "browser",
            _ => "none",
        }
    }

    fn tray(&self) -> bool {
        self.proc.is_some()
    }

    fn open_browser(&mut self) {
        if gui::open_window(&self.url) {
            self.browser = true;
        } else {
            eprintln!("bananasplit: could not open a window; open {} in a browser", self.url);
        }
    }

    fn start(&mut self, cfg: &Config, hidden: bool, token: &str) {
        let opts = gui::WinOpts {
            url: &self.url,
            hidden,
            close_to_tray: cfg.gui.close_to_tray,
            size: cfg.gui.window,
            token: Some(token),
        };
        match gui::WindowProc::spawn(&opts) {
            Ok(p) => {
                if p.mode == gui::WinMode::Tray {
                    eprintln!("bananasplit: WebKitGTK not found, showing the mixer in a browser (tray icon still works)");
                    if !hidden {
                        self.open_browser();
                    }
                }
                self.proc = Some(p);
                if self.recording {
                    self.send("rec 1");
                }
            }
            Err(e) => {
                eprintln!("bananasplit: no app window ({e}); showing the mixer in a browser");
                if !hidden {
                    self.open_browser();
                }
            }
        }
    }

    /// Bring it up (launched again from the app menu, or the tray).
    fn show(&mut self, cfg: &Config, token: &str) {
        let alive = self.proc.as_mut().is_some_and(gui::WindowProc::alive);
        match &mut self.proc {
            Some(p) if alive && p.mode == gui::WinMode::App => p.send(&format!("show {}", if token.is_empty() { "-" } else { token })),
            Some(_) if alive => self.open_browser(),
            _ => {
                self.proc = None;
                self.start(cfg, false, token);
            }
        }
    }

    fn send(&mut self, line: &str) {
        if let Some(p) = &mut self.proc {
            p.send(line);
        }
    }

    fn close(&mut self) {
        if let Some(p) = self.proc.take() {
            p.close();
        }
    }
}

/// Same control loop as the terminal UI, driving the GUI instead.
fn run_gui(
    ctx: &Ctx,
    cfg: &mut Config,
    rec: &Arc<Recorder>,
    player: &Arc<Player>,
    notes: Vec<String>,
    opts: &GuiOpts,
) -> Result<(), String> {
    let (backend, path, shared, vban) = (ctx.backend, ctx.path, ctx.shared, ctx.vban);
    let rec_folder = |r: &Recorder| recorder::expand_home(&r.settings().folder);
    let takes = gui::Takes::start(rec_folder(rec));
    let scenes = SceneStore::for_config(path);
    if !cfg.gui.scene.is_empty() && !scenes.exists(&cfg.gui.scene) {
        cfg.gui.scene.clear();
    }
    let hub = gui::Hub::new(shared.clone(), rec.clone(), vban.clone(), player.clone(), takes.clone(), scenes);
    hub.set_names(channel_names(cfg));
    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
    hub.autostart.store(desktop::autostart_enabled(), SeqCst);
    hub.installed.store(desktop::menu_installed(), SeqCst);
    hub.set_client(&backend.client_name);
    let port = gui::start(hub.clone(), opts.port, opts.explicit)?;
    let url = gui::url(port);
    gui::note_port(&backend.client_name, port);
    for n in &notes {
        eprintln!("bananasplit: warning: {n}");
    }
    let c = backend.client();
    let sr = backend.sample_rate();
    eprintln!(
        "bananasplit: running as JACK client \"{}\" at {} Hz, {} frames ({:.2} ms period).",
        backend.client_name,
        sr,
        c.buffer_size(),
        c.buffer_size() as f64 * 1000.0 / sr.max(1) as f64
    );
    if port != opts.port {
        eprintln!("bananasplit: port {} is busy, using {port} instead", opts.port);
    }
    eprintln!("bananasplit: GUI at {url}  (quit with the power button in the GUI, or Ctrl-C)");
    let mut window = Window { proc: None, url: url.clone(), browser: false, recording: rec.is_recording() };
    if opts.open {
        window.start(cfg, opts.hidden, &launch_token());
    }

    let mut status = notes.join(" | ");
    let mut view = tui::View::default(); // same meter ballistics as the terminal UI
    // What the channels' menus show: the sound server's apps and devices
    // (its own thread), the JACK connections, this computer's addresses.
    let pulse = devices::PulseWatch::start();
    let mut conns: Vec<Vec<String>> = vec![Vec::new(); cables::NUM_CHANNELS];
    let mut conns_at = Instant::now() - Duration::from_secs(10);
    let mut pulse_rev = u64::MAX;
    let mut dev_rev = 0u64;
    let mut ips = sys::local_ipv4();
    let mut conn: Vec<String> = Vec::new();
    let mut last_errs: Vec<String> = Vec::new();
    let mut changed_at: Option<Instant> = Some(Instant::now());
    let mut force_connect = false;
    let mut lat = (0u32, 0u32);
    let mut lat_at = Instant::now() - Duration::from_secs(10);
    let mut lat_check = LatencyCheck::new();
    let mut vlog = VbanLog::default();
    let mut was_recording = false;
    let mut folder = rec_folder(rec);
    let mut disk = sys::disk_space(&folder);
    let mut slow_at = Instant::now();
    let mut scenes_at = Instant::now();
    let frame = Duration::from_millis(33);

    let result = loop {
        let t0 = Instant::now();
        if sys::QUIT.load(SeqCst) {
            break Ok(());
        }
        if backend.ctl.shutdown.load(Relaxed) {
            break Err("the JACK/PipeWire server shut down".to_string());
        }

        let mut quit = false;
        for r in hub.take_requests() {
            match r {
                gui::Request::Quit => quit = true,
                gui::Request::Reconnect => force_connect = true,
                gui::Request::Save => {
                    capture_all(cfg, shared, rec, vban);
                    cfg.recorder.play_to = player.routes();
                    status = match cfg.save(path) {
                        Ok(()) => format!("saved {}", path.display()),
                        Err(e) => format!("save failed: {e}"),
                    };
                }
                gui::Request::Rename(ch, name) => {
                    let (n, id) = if ch < cfg.strips.len() {
                        (&mut cfg.strips[ch].name, recorder::source_id(ch))
                    } else {
                        (&mut cfg.buses[ch - cfg.strips.len()].name, recorder::source_id(ch))
                    };
                    *n = name;
                    hub.set_names(channel_names(cfg));
                    status = format!("renamed {id}; the virtual cable keeps its old name until BananaSplit restarts");
                }
                gui::Request::Show(token) => window.show(cfg, &token),
                gui::Request::Theme(t) => {
                    cfg.gui.theme = t;
                    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
                }
                gui::Request::Zoom(z) => {
                    cfg.gui.zoom = z;
                    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
                }
                gui::Request::CloseToTray(on) => {
                    cfg.gui.close_to_tray = on;
                    window.send(&format!("close-to-tray {}", u8::from(on)));
                }
                gui::Request::Scene(name) => cfg.gui.scene = name,
                gui::Request::Dev(op, reply) => {
                    let list = op == gui::DevOp::List;
                    let info = pulse.snapshot().1;
                    let r = chanmenu::handle(op, backend, cfg, &mut ctx.cables.borrow_mut(), &info);
                    if !list {
                        pulse.kick();
                        conns_at = Instant::now() - Duration::from_secs(10);
                        dev_rev += 1;
                        match &r {
                            Ok(m) => status = m.clone(),
                            Err(e) => status = e.clone(),
                        }
                    }
                    reply.send(r);
                }
            }
        }
        if let Some(p) = &mut window.proc {
            for m in p.poll() {
                match m {
                    gui::WinMsg::Quit => quit = true,
                    gui::WinMsg::Open => window.browser |= gui::open_window(&url),
                    gui::WinMsg::Record => {
                        status = match toggle_record(rec, shared, &channel_names(cfg)) {
                            Ok(m) => m,
                            Err(e) => format!("recorder: {e}"),
                        }
                    }
                    gui::WinMsg::Size(w, h) => cfg.gui.window = Some((w.clamp(640, 16384), h.clamp(480, 16384))),
                    gui::WinMsg::Gone => {}
                }
            }
            if !p.alive() {
                eprintln!("bananasplit: the window closed; run `bananasplit --gui` (or use the app menu) to open it again");
                window.proc = None;
            }
        }
        if quit {
            break Ok(());
        }

        if backend.ctl.ports_changed.swap(false, Relaxed) {
            changed_at = Some(Instant::now());
        }
        if force_connect || changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            let w = backend.autoconnect(cfg, &ctx.cables.borrow());
            print_wiring(backend, cfg, &w, &mut last_errs);
            if force_connect || !w.errs.is_empty() {
                status = if w.errs.is_empty() { "all configured connections made".into() } else { w.errs.join(" | ") };
            }
            conn = w.desc;
            force_connect = false;
            changed_at = None;
            lat_at = Instant::now() - Duration::from_secs(10);
        }
        if lat_at.elapsed() > Duration::from_millis(500) {
            lat = backend.io_latency();
            lat_at = Instant::now();
        }
        if let Some(m) = lat_check.poll(backend, cfg) {
            eprintln!("bananasplit: warning: {m}");
            append(&mut status, &m);
        }
        if let Some(m) = rate_change(backend) {
            eprintln!("bananasplit: {m}");
            status = m;
        }
        vlog.poll(vban);

        // Recorder: tell the tray, and look for the new take when it ends.
        let recording = rec.is_recording();
        if recording != was_recording {
            window.recording = recording;
            window.send(&format!("rec {}", u8::from(recording)));
            takes.kick();
            was_recording = recording;
        }
        // Slow things: the recordings folder, disk space, the autostart file.
        if backend.ctl.conns_changed.swap(false, Relaxed) || conns_at.elapsed() > Duration::from_secs(2) {
            let c = chanmenu::connections(backend, &ctx.cables.borrow());
            if c != conns {
                conns = c;
                dev_rev += 1;
            }
            conns_at = Instant::now();
        }
        let (rev, info) = pulse.snapshot();
        if rev != pulse_rev {
            pulse_rev = rev;
            dev_rev += 1;
        }
        let chans = chanmenu::views(&ctx.cables.borrow(), &info, &conns);
        if slow_at.elapsed() > Duration::from_secs(2) {
            slow_at = Instant::now();
            ips = sys::local_ipv4();
            let f = rec_folder(rec);
            if f != folder {
                takes.set_folder(f.clone());
                folder = f;
            }
            disk = sys::disk_space(&folder);
            hub.autostart.store(desktop::autostart_enabled(), SeqCst);
            hub.installed.store(desktop::menu_installed(), SeqCst);
        }
        if scenes_at.elapsed() > Duration::from_secs(5) {
            scenes_at = Instant::now();
            hub.refresh_scenes();
        }

        view.update_meters(shared);
        let info = tui::Info {
            client: &backend.client_name,
            sample_rate: backend.sample_rate(),
            buffer_size: c.buffer_size(),
            cpu: c.cpu_load(),
            xruns: shared.xruns.load(Relaxed),
            oversize: shared.oversize_cycles.load(Relaxed),
            lat_in: lat.0,
            lat_out: lat.1,
            cables: ctx.cables.borrow().count(),
            vban: "",
        };
        let rec_status = rec.status();
        let rec_settings = rec.settings();
        let (vst, vcfg) = (vban.status(), vban.config());
        let conn_all = with_vban(&conn, &vst, &vcfg, backend.sample_rate());
        let play = player.status();
        let (takes_rev, list) = takes.snapshot();
        let scene_list = hub.scene_list();
        let extra = gui::Extra {
            player: &play,
            play_to: player.routes(),
            takes_rev,
            takes_count: list.len(),
            newest: list.last().map(|t| t.key.as_str()),
            disk,
            scenes: &scene_list,
            autostart: hub.autostart.load(SeqCst),
            installed: hub.installed.load(SeqCst),
            window: window.kind(),
            tray: window.tray(),
        };
        hub.publish(gui::state_json(
            shared,
            &gui::Frame {
                info: &info,
                cfg,
                meters: view.meters(),
                in_meters: view.in_meters(),
                conn: &conn_all,
                chans: &chans,
                dev_rev,
                ips: &ips,
                status: &status,
                rec: &rec_status,
                rec_settings: &rec_settings,
                vban: &vst,
                vban_cfg: &vcfg,
                extra: &extra,
            },
        ));

        if let Some(rest) = frame.checked_sub(t0.elapsed()) {
            std::thread::sleep(rest);
        }
    };
    hub.close();
    window.close();
    takes.shutdown();
    pulse.shutdown();
    gui::forget_port(&backend.client_name, port);
    result?;
    // Remember the mixer state for next time (like the terminal UI).
    capture_all(cfg, shared, rec, vban);
    cfg.recorder.play_to = player.routes();
    cfg.save(path)
}
