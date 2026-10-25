//! BananaSplit for Windows (WASAPI).
//!
//! The mixer itself (`bananasplit` crate) is shared with the Linux version;
//! this binary supplies the Windows audio backend (`win/`) and the GUI.
//!
//! A Windows (GUI-subsystem) program: no console window opens, whether it
//! is started from Explorer, the Start Menu or the autostart entry. Run from
//! a terminal for a command-line option, it borrows that terminal's console
//! (`win::console::attach_parent`).

// Not for `cargo test`: the test harness prints to its console.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[path = "gui.rs"]
mod gui;
#[path = "tui.rs"]
mod tui;
#[path = "win/mod.rs"]
mod win;
#[cfg(windows)]
#[path = "win/window.rs"]
mod app_window;
#[cfg(windows)]
#[path = "win/tray.rs"]
mod tray;

#[path = "win/desktop.rs"]
pub mod desktop;
#[path = "win/sys.rs"]
pub mod sys;

use bananasplit::config::Config;
use bananasplit::engine::{NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::player::{PlayTap, Player};
use bananasplit::recorder::{self, RecTap, Recorder};
use bananasplit::scenes::SceneStore;
use bananasplit::vban::{InState, OutState, Vban, VbanTap};
use gui::Request;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::time::{Duration, Instant};
use win::{Session, SERVER_GONE};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = concat!(
    "BananaSplit ",
    env!("CARGO_PKG_VERSION"),
    " - 6-strip, 5-bus low-latency virtual audio mixer (Windows WASAPI)

USAGE:
    bananasplit [--config FILE] [--gui] [--headless] [--no-open]
    bananasplit --window URL
    bananasplit --list-devices
    bananasplit --print-default-config
    bananasplit --quit

OPTIONS:
    --gui                Run the desktop app window (default)
    --gui-port PORT      GUI port (default 8790)
    --no-open, --hidden  Start in the background: only the tray icon shows
    --headless           Run the audio engine without a UI
    --list-devices       List Windows audio endpoints
    --print-default-config
                         Print the default configuration and exit
    --quit               Ask a running BananaSplit to quit
    -h, --help           Show this help
    -V, --version        Show the version
"
);

struct Args {
    path: PathBuf,
    headless: bool,
    no_open: bool,
    port: u16,
    explicit_port: bool,
    list: bool,
    quit: bool,
    print_default: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut a = Args {
        path: Config::default_path(),
        headless: false,
        no_open: false,
        port: gui::DEFAULT_PORT,
        explicit_port: false,
        list: false,
        quit: false,
        print_default: false,
    };
    while let Some(s) = args.next() {
        match s.as_str() {
            "-h" | "--help" => return Err(HELP.to_string()),
            "-V" | "--version" => return Err(format!("BananaSplit {VERSION} (Windows, WASAPI)\n")),
            "--config" => {
                let p = args.next().ok_or("missing path after --config")?;
                a.path = PathBuf::from(p);
            }
            "--gui" => {}
            "--headless" | "--no-gui" => a.headless = true,
            "--no-open" | "--hidden" => a.no_open = true,
            "--gui-port" => {
                let p = args.next().ok_or("missing port after --gui-port")?;
                a.port = p.parse().map_err(|_| "bad port")?;
                a.explicit_port = true;
            }
            "--list-devices" => a.list = true,
            "--quit" => a.quit = true,
            "--print-default-config" => a.print_default = true,
            other => return Err(format!("unknown option {other:?}\n\n{HELP}")),
        }
    }
    Ok(a)
}

fn channel_names(cfg: &Config) -> Vec<String> {
    cfg.strips
        .iter()
        .map(|s| s.name.clone())
        .chain(cfg.buses.iter().map(|b| b.name.clone()))
        .collect()
}

fn rec_folder(rec: &Recorder) -> PathBuf {
    recorder::expand_home(&rec.settings().folder)
}

fn capture_all(cfg: &mut Config, shared: &Shared, rec: &Recorder, vban: &Vban) {
    cfg.capture(shared);
    cfg.recorder.set_settings(&rec.settings());
    cfg.vban = vban.config();
}

fn error_main() -> ExitCode {
    sys::pause_on_error();
    ExitCode::FAILURE
}

/// An error that stops BananaSplit: printed, and shown in a dialog when
/// nobody would read it (started from Explorer, a shortcut or autostart).
fn fail(e: &str, headless: bool) -> ExitCode {
    eprintln!("error: {e}");
    if !headless && !win::console::has_output() {
        desktop::error_dialog(&format!("BananaSplit stopped.\n\n{e}"));
    }
    error_main()
}

struct Ctx {
    path: PathBuf,
    shared: Arc<Shared>,
    rec: Arc<Recorder>,
    vban: Arc<Vban>,
    player: Arc<Player>,
    session: Session,
}

// ---------------------------------------------------------------------------
// Headless
// ---------------------------------------------------------------------------

fn run_headless(ctx: &mut Ctx, cfg: &Config, notes: Vec<String>) -> Result<(), String> {
    for n in notes {
        eprintln!("bananasplit: {n}");
    }
    eprintln!("BananaSplit {VERSION} running (headless). Ctrl-C to quit.");
    sys::install_signal_handlers();
    let mut last = Instant::now();
    while !sys::QUIT.load(SeqCst) {
        if ctx.session.take_changed() {
            let (_, errs) = ctx.session.connect(cfg, false);
            for e in errs {
                eprintln!("bananasplit: {e}");
            }
        }
        if last.elapsed() >= Duration::from_secs(1) {
            last = Instant::now();
            let st = ctx.session.status();
            let vst = ctx.vban.status();
            let ins = vst.ins.iter().filter(|x| x.state == InState::Receiving).count();
            let outs = vst.outs.iter().filter(|x| x.state == OutState::Sending).count();
            eprintln!(
                "cpu {:.0}%  xruns {}  rec {}  vban in/out {} / {}",
                st.cpu,
                ctx.shared.xruns.load(Relaxed),
                if ctx.rec.is_recording() { "on" } else { "off" },
                ins,
                outs
            );
        }
        if ctx.session.shutdown() {
            return Err(SERVER_GONE.to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GUI
// ---------------------------------------------------------------------------

struct GuiOpts {
    port: u16,
    explicit_port: bool,
    no_open: bool,
    /// The first start on this computer, without VB-Audio cables: offer them.
    cable_prompt: bool,
}

/// The mixer's desktop window: a `bananasplit --window` process of its own
/// (the protocol of the Linux window, see `gui::WindowProc`). It reads its
/// input from the mixer, so it closes when the mixer ends, however it ends.
struct Window {
    proc: Option<gui::WindowProc>,
    url: String,
    /// Recording (the tray icon shows it).
    recording: bool,
}

impl Window {
    /// Start the window process: its window, and its tray icon. `hidden`
    /// (started in the background): only the tray icon shows.
    fn start(&mut self, cfg: &Config, hidden: bool) {
        let opts = gui::WinOpts {
            url: &self.url,
            hidden,
            close_to_tray: cfg.gui.close_to_tray,
            size: cfg.gui.window,
            token: None,
        };
        match gui::WindowProc::spawn(&opts) {
            Ok(p) => {
                self.proc = Some(p);
                if self.recording {
                    self.send("rec 1");
                }
            }
            Err(e) => {
                eprintln!("bananasplit: no window: {e}");
                if hidden {
                    return; // nothing was asked to show; the next start tries again
                }
                // Not on this thread: the mixer keeps running meanwhile.
                let url = self.url.clone();
                std::thread::spawn(move || desktop::no_window(&e, &url));
            }
        }
    }

    /// Does the window process have a tray icon?
    fn tray(&self) -> bool {
        self.proc.as_ref().is_some_and(|p| p.tray)
    }

    /// Bring it up (BananaSplit was started again), or open it again.
    fn show(&mut self, cfg: &Config) {
        let alive = self.proc.as_mut().is_some_and(gui::WindowProc::alive);
        match &mut self.proc {
            Some(p) if alive => p.send("show -"),
            _ => {
                self.proc = None;
                self.start(cfg, false);
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

fn run_gui(ctx: &mut Ctx, cfg: &mut Config, notes: Vec<String>, opts: &GuiOpts) -> Result<(), String> {
    let rec = ctx.rec.clone();
    let folder = rec_folder(&rec);
    let takes = gui::Takes::start(folder.clone());
    let scenes = SceneStore::for_config(&ctx.path);
    if !cfg.gui.scene.is_empty() && !scenes.exists(&cfg.gui.scene) {
        cfg.gui.scene.clear();
    }
    let hub = gui::Hub::new(
        ctx.shared.clone(),
        rec.clone(),
        ctx.vban.clone(),
        ctx.player.clone(),
        takes.clone(),
        scenes,
    );
    hub.set_names(channel_names(cfg));
    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
    hub.autostart.store(desktop::autostart_enabled(), SeqCst);
    hub.installed.store(desktop::menu_installed(), SeqCst);
    hub.set_client(&cfg.client_name);

    let port = gui::start(hub.clone(), opts.port, opts.explicit_port)?;
    gui::note_port(&cfg.client_name, port);
    let url = gui::url(port);
    let mut notes = notes;
    if port != opts.port {
        notes.push(format!("port {} is busy, using {port} instead", opts.port));
    }
    eprintln!("bananasplit: GUI at {url}  (quit with the power button, or Ctrl-C)");
    for n in &notes {
        eprintln!("bananasplit: {n}");
    }
    if opts.cable_prompt {
        // Not on this thread: the mixer starts meanwhile.
        std::thread::spawn(desktop::cable_prompt);
    }
    let mut window = Window { proc: None, url: url.clone(), recording: false };
    // Started after the first frame is published (it takes a moment). In the
    // background (`--no-open`), the window stays hidden: only the tray icon.
    let mut open_window = true;
    let mut was_recording = false;

    // What happened at start shows in the page's status line: there is no
    // console to print it to.
    let mut status = notes.join(" | ");
    let mut view = tui::View::default();
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut last_slow = Instant::now() - Duration::from_secs(10);
    let mut last_save = Instant::now();
    let mut dev_rev = 0u64;
    let mut ips = sys::local_ipv4();
    let mut chans = win::chanmenu::views(cfg);
    let mut views_rev = dev_rev;
    let mut disk = sys::disk_space(&folder);
    let frame = Duration::from_millis(33);

    sys::install_signal_handlers();
    let result = loop {
        let t0 = Instant::now();
        if sys::QUIT.load(SeqCst) {
            break Ok(());
        }
        if ctx.session.shutdown() {
            break Err(SERVER_GONE.to_string());
        }

        let mut quit = false;
        for req in hub.take_requests() {
            match req {
                Request::Quit => quit = true,
                Request::Reconnect => {
                    let (_, errs) = ctx.session.connect(cfg, true);
                    status = if errs.is_empty() {
                        "all configured connections made".to_string()
                    } else {
                        errs.join(" | ")
                    };
                    dev_rev += 1;
                }
                Request::Save => {
                    capture_all(cfg, &ctx.shared, &rec, &ctx.vban);
                    cfg.recorder.play_to = ctx.player.routes();
                    status = match cfg.save(&ctx.path) {
                        Ok(()) => format!("saved {}", ctx.path.display()),
                        Err(e) => format!("save failed: {e}"),
                    };
                    last_save = Instant::now();
                }
                Request::Rename(ch, name) => {
                    if ch < NUM_STRIPS {
                        cfg.strips[ch].name = name;
                    } else if ch - NUM_STRIPS < NUM_BUSES {
                        cfg.buses[ch - NUM_STRIPS].name = name;
                    }
                    hub.set_names(channel_names(cfg));
                    status = "renamed".to_string();
                }
                Request::Show(_) => window.show(cfg),
                Request::Theme(t) => {
                    cfg.gui.theme = t;
                    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
                }
                Request::CloseToTray(b) => {
                    cfg.gui.close_to_tray = b;
                    window.send(&format!("close-to-tray {}", u8::from(b)));
                }
                Request::Zoom(z) => {
                    cfg.gui.zoom = z;
                    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
                }
                Request::Scene(name) => cfg.gui.scene = name,
                Request::Dev(op, reply) => {
                    let is_list = op == gui::DevOp::List;
                    let r = win::chanmenu::handle(op, cfg);
                    if !is_list {
                        match &r {
                            Ok(m) => status = m.clone(),
                            Err(e) => status = e.clone(),
                        }
                        let (_, errs) = ctx.session.connect(cfg, true);
                        if !errs.is_empty() {
                            status = errs.join(" | ");
                        }
                        dev_rev += 1;
                    }
                    reply.send(r);
                }
            }
        }
        if let Some(p) = &mut window.proc {
            for m in p.poll() {
                match m {
                    // Closed with "Closing the window keeps it running" off.
                    gui::WinMsg::Quit => quit = true,
                    gui::WinMsg::Size(w, h) => cfg.gui.window = Some((w.clamp(640, 16384), h.clamp(480, 16384))),
                    // The tray menu.
                    gui::WinMsg::Record => {
                        let on = if rec.is_recording() { "0" } else { "1" };
                        status = match gui::command(&hub, &format!("op=rec&v={on}")) {
                            Ok(m) => m,
                            Err(e) => format!("recorder: {e}"),
                        };
                    }
                    // A browser (Linux without WebKitGTK); the reader's end.
                    gui::WinMsg::Open | gui::WinMsg::Gone => {}
                }
            }
            if !p.alive() {
                eprintln!("bananasplit: the window closed; start BananaSplit again to open it");
                window.proc = None;
            }
        }
        if quit {
            break Ok(());
        }

        if ctx.session.take_changed() {
            let (_, errs) = ctx.session.connect(cfg, false);
            if !errs.is_empty() {
                status = errs.join(" | ");
            }
            dev_rev += 1;
        }
        if last_scan.elapsed() >= Duration::from_secs(2) || dev_rev != views_rev {
            last_scan = Instant::now();
            chans = win::chanmenu::views(cfg);
            views_rev = dev_rev;
        }
        if last_slow.elapsed() >= Duration::from_secs(2) {
            last_slow = Instant::now();
            ips = sys::local_ipv4();
            disk = sys::disk_space(&folder);
            hub.autostart.store(desktop::autostart_enabled(), SeqCst);
            hub.installed.store(desktop::menu_installed(), SeqCst);
        }

        view.update_meters(&ctx.shared);
        let st = ctx.session.status();
        let lat = ctx.session.io_latency();
        let (trev, tlist) = takes.snapshot();
        let scene_list = hub.scene_list();
        let rec_status = rec.status();
        let rec_settings = rec.settings();
        let vst = ctx.vban.status();
        let vcfg = ctx.vban.config();
        let play = ctx.player.status();
        let conn = ctx.session.wiring();
        let info = tui::Info {
            client: &cfg.client_name,
            sample_rate: st.sample_rate,
            buffer_size: st.buffer_size,
            cpu: st.cpu,
            xruns: ctx.shared.xruns.load(Relaxed),
            oversize: ctx.shared.oversize_cycles.load(Relaxed),
            lat_in: lat.0,
            lat_out: lat.1,
            cables: ctx.session.cable_count(),
            vban: "",
        };
        let extra = gui::Extra {
            player: &play,
            play_to: ctx.player.routes(),
            takes_rev: trev,
            takes_count: tlist.len(),
            newest: tlist.last().map(|t| t.key.as_str()),
            disk,
            scenes: &scene_list,
            autostart: hub.autostart.load(SeqCst),
            installed: hub.installed.load(SeqCst),
            window: if window.proc.is_some() { "app" } else { "none" },
            tray: window.tray(),
        };
        hub.publish(gui::state_json(
            &ctx.shared,
            &gui::Frame {
                info: &info,
                cfg,
                meters: view.meters(),
                in_meters: view.in_meters(),
                conn: &conn,
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

        if std::mem::take(&mut open_window) {
            window.start(cfg, opts.no_open);
        }
        // The tray icon shows when it records; a finished take is listed.
        let recording = rec.is_recording();
        if recording != was_recording {
            was_recording = recording;
            window.recording = recording;
            window.send(&format!("rec {}", u8::from(recording)));
            takes.kick();
        }
        if last_save.elapsed() >= Duration::from_secs(120) {
            capture_all(cfg, &ctx.shared, &rec, &ctx.vban);
            cfg.recorder.play_to = ctx.player.routes();
            let _ = cfg.save(&ctx.path);
            last_save = Instant::now();
        }
        if let Some(rest) = frame.checked_sub(t0.elapsed()) {
            std::thread::sleep(rest);
        }
    };

    // Out of sight at once, rather than showing a disconnected page while
    // everything stops and the settings are saved.
    window.send("hide");
    hub.close();
    takes.shutdown();
    gui::forget_port(&cfg.client_name, port);
    ctx.rec.shutdown();
    ctx.player.shutdown();
    ctx.vban.shutdown();
    let saved = result.and_then(|()| {
        capture_all(cfg, &ctx.shared, &rec, &ctx.vban);
        cfg.recorder.play_to = ctx.player.routes();
        cfg.save(&ctx.path)
    });
    // Then the window (it goes by itself once the mixer has exited, too):
    // closing the console leaves BananaSplit only a few seconds in all.
    window.close();
    saved
}

// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    // The mixer's window (a process of its own, started by the mixer).
    #[cfg(windows)]
    {
        let argv: Vec<String> = std::env::args().collect();
        if argv.get(1).map(String::as_str) == Some(gui::WINDOW_FLAG) {
            return app_window::main(&argv[2..]);
        }
    }
    let args = match parse_args() {
        Ok(a) => a,
        Err(s) => {
            win::console::attach_parent();
            if s == HELP || s.starts_with("BananaSplit ") {
                print!("{s}");
                return ExitCode::SUCCESS;
            }
            eprintln!("{s}");
            return ExitCode::FAILURE;
        }
    };
    // The command-line uses print to the terminal they were run from.
    if args.print_default || args.list || args.quit || args.headless {
        win::console::attach_parent();
    }
    if args.print_default {
        print!("{}", Config::default().to_ini());
        return ExitCode::SUCCESS;
    }
    if args.list {
        return match win::list_devices() {
            Ok(s) => {
                print!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                error_main()
            }
        };
    }
    if args.quit {
        let defq = Config::default();
        if let Some(p) = gui::find_running(&defq.client_name, args.explicit_port.then_some(args.port)) {
            if let Some((code, _)) = gui::request_quit(p) {
                if code == 200 {
                    return ExitCode::SUCCESS;
                }
            }
        }
        if let Ok(c) = Config::load_or_default(&args.path) {
            if let Some(p) = gui::find_running(&c.client_name, args.explicit_port.then_some(args.port)) {
                let _ = gui::request_quit(p);
                return ExitCode::SUCCESS;
            }
        }
        eprintln!("no running BananaSplit found");
        return ExitCode::SUCCESS;
    }

    let mut notes = Vec::new();
    if let Ok(Some(note)) = Config::migrate_legacy(&args.path, &Config::legacy_path()) {
        eprintln!("bananasplit: {note}");
        notes.push(note);
    }
    // Before load_or_create writes it: the very first start.
    let first_start = !args.path.exists();
    let mut cfg = match Config::load_or_create(&args.path) {
        Ok(c) => c,
        Err(e) => return fail(&e, args.headless),
    };

    // A second launch (the Start Menu shortcut while autostart already runs
    // BananaSplit) brings up the running mixer's page instead of starting a
    // second audio engine.
    if !args.headless {
        if let Some(port) = gui::find_running(&cfg.client_name, args.explicit_port.then_some(args.port)) {
            let url = gui::url(port);
            if args.no_open {
                eprintln!("bananasplit: already running at {url}");
                return ExitCode::SUCCESS;
            }
            // Its window comes to the front: this start may pass that on.
            sys::allow_foreground();
            if !gui::request_show(port, "") && !gui::open_window(&url) {
                return fail(&format!("BananaSplit is already running at {url}, but its window could not be opened"), false);
            }
            return ExitCode::SUCCESS;
        }
    }

    let shared = Shared::new();
    cfg.apply(&shared);
    let rate = 48_000u32;
    shared.sample_rate.store(rate, SeqCst);

    let rec_tap = RecTap::new();
    let vban_tap = VbanTap::new();
    let play_tap = PlayTap::new();
    let recorder = Arc::new(Recorder::new(rec_tap.clone(), rate, cfg.recorder.settings()));
    let vban = Arc::new(Vban::start(vban_tap.clone(), rate, cfg.vban.clone()));
    let player = Arc::new(Player::new(play_tap.clone(), rate));
    player.set_routes(cfg.recorder.play_to);

    let no_cables = win::first_run(&mut cfg, &args.path, args.headless, &mut notes);
    let session = match Session::open(&cfg, shared.clone(), rec_tap.clone(), vban_tap.clone(), play_tap.clone(), &mut notes) {
        Ok(s) => s,
        Err(e) => return fail(&e, args.headless),
    };

    let mut ctx = Ctx {
        path: args.path.clone(),
        shared: shared.clone(),
        rec: recorder,
        vban,
        player,
        session,
    };
    // The graph Session::open built (opening every device again would only
    // take time); its connection errors.
    let (_, errs) = ctx.session.connect(&cfg, false);
    notes.extend(errs);

    // Logoff, shutdown and `taskkill` (without /f) save and stop it.
    win::endsession::watch();
    let result = if args.headless {
        run_headless(&mut ctx, &cfg, notes)
    } else {
        run_gui(
            &mut ctx,
            &mut cfg,
            notes,
            &GuiOpts {
                port: args.port,
                explicit_port: args.explicit_port,
                no_open: args.no_open,
                cable_prompt: first_start && no_cables && !args.no_open,
            },
        )
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e, args.headless),
    }
}
