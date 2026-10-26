//! BananaSplit for macOS (Core Audio).
//!
//! The mixer itself (`bananasplit` crate) is shared with the Linux and
//! Windows versions; this binary supplies the macOS audio backend (`mac/`)
//! and the app. AppKit wants the main thread, so the app (its window and
//! menu bar icon, `mac/app.rs`) runs there, and the mixer's main loop runs
//! on a thread of its own and drives the app as the other versions drive
//! their window process.

#[path = "gui.rs"]
mod gui;
// The terminal UI of the Linux version: the app uses its meters and the
// numbers it shows.
#[path = "tui.rs"]
#[allow(dead_code)]
mod tui;
#[path = "update.rs"]
mod update;
#[path = "mac/mod.rs"]
mod mac;
#[path = "mac/app.rs"]
mod app;

#[path = "mac/desktop.rs"]
pub mod desktop;
#[path = "mac/sys.rs"]
pub mod sys;

use app::{FromUi, StartOpts, ToUi, Ui};
use bananasplit::config::Config;
use bananasplit::engine::{NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::player::{PlayTap, Player};
use bananasplit::recorder::{self, RecTap, Recorder};
use bananasplit::scenes::SceneStore;
use bananasplit::vban::{InState, OutState, Vban, VbanTap};
use gui::Request;
use mac::{SERVER_GONE, Session};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = concat!(
    "BananaSplit ",
    env!("CARGO_PKG_VERSION"),
    " - 6-strip, 5-bus low-latency virtual audio mixer (macOS Core Audio)

USAGE:
    bananasplit [--config FILE] [--gui] [--headless] [--no-open]
    bananasplit --list-devices
    bananasplit --print-default-config
    bananasplit --update
    bananasplit --quit

OPTIONS:
    --gui                Run the app with its window (default)
    --gui-port PORT      GUI port (default 8790)
    --no-open, --hidden  Start in the background: only the menu bar icon shows
    --headless           Run the audio engine without a UI
    --list-devices       List the audio devices
    --print-default-config
                         Print the default configuration and exit
    --update             Look for a new version on GitHub (the app has this in
                         Menu > Updates)
    --quit               Ask a running BananaSplit to quit
    -h, --help           Show this help
    -V, --version        Show the version
"
);

#[derive(Debug)]
struct Args {
    path: PathBuf,
    headless: bool,
    no_open: bool,
    port: u16,
    explicit_port: bool,
    list: bool,
    quit: bool,
    print_default: bool,
    update: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = args;
    let mut a = Args {
        path: Config::default_path(),
        headless: false,
        no_open: false,
        port: gui::DEFAULT_PORT,
        explicit_port: false,
        list: false,
        quit: false,
        print_default: false,
        update: false,
    };
    while let Some(s) = args.next() {
        match s.as_str() {
            "-h" | "--help" => return Err(HELP.to_string()),
            "-V" | "--version" => return Err(format!("BananaSplit {VERSION} (macOS, Core Audio)\n")),
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
            "--update" => a.update = true,
            // The process serial number older macOS versions pass to an
            // app opened in Finder.
            psn if psn.starts_with("-psn_") => {}
            other => return Err(format!("unknown option {other:?}\n\n{HELP}")),
        }
    }
    Ok(a)
}

fn channel_names(cfg: &Config) -> Vec<String> {
    cfg.strips.iter().map(|s| s.name.clone()).chain(cfg.buses.iter().map(|b| b.name.clone())).collect()
}

fn rec_folder(rec: &Recorder) -> PathBuf {
    recorder::expand_home(&rec.settings().folder)
}

fn capture_all(cfg: &mut Config, shared: &Shared, rec: &Recorder, vban: &Vban) {
    cfg.capture(shared);
    cfg.recorder.set_settings(&rec.settings());
    cfg.vban = vban.config();
}

/// An error that stops BananaSplit: printed, and shown in a dialog when
/// nobody would read it (opened from Finder, the Dock or at login).
fn fail(e: &str, headless: bool) -> ExitCode {
    eprintln!("error: {e}");
    if !headless && !sys::stdin_is_tty() {
        desktop::error_dialog(&format!("BananaSplit stopped.\n\n{e}"));
    }
    ExitCode::FAILURE
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
    /// The first start on this computer, without virtual cables: the page
    /// opens its "Virtual cables" window.
    cable_welcome: bool,
}

/// The app's window, as the mixer sees it (`mac/app.rs` runs it on the
/// main thread).
struct Window<'a> {
    ui: &'a Ui,
    url: String,
    started: bool,
    /// It could not be made: the page opens in the browser instead.
    failed: bool,
    /// The menu bar icon is there.
    tray: bool,
    /// Recording (the menu bar icon shows it).
    recording: bool,
}

impl Window<'_> {
    /// Make the window; `hidden` (started in the background): only the menu
    /// bar icon shows.
    fn start(&mut self, cfg: &Config, hidden: bool) {
        self.ui.send(ToUi::Start(StartOpts { url: self.url.clone(), hidden, close_to_tray: cfg.gui.close_to_tray, size: cfg.gui.window }));
        self.started = true;
        if self.recording {
            self.ui.send(ToUi::Rec(true));
        }
    }

    /// Bring it up (BananaSplit was started again, or the menu asked).
    fn show(&mut self, cfg: &Config) {
        if self.failed {
            let _ = desktop::open_page(&self.url);
        } else if self.started {
            self.ui.send(ToUi::Show);
        } else {
            self.start(cfg, false);
        }
    }
}

fn run_gui(ctx: &mut Ctx, cfg: &mut Config, notes: Vec<String>, opts: &GuiOpts, ui: &Ui) -> Result<(), String> {
    let rec = ctx.rec.clone();
    let mut folder = rec_folder(&rec);
    let takes = gui::Takes::start(folder.clone());
    let scenes = SceneStore::for_config(&ctx.path);
    if !cfg.gui.scene.is_empty() && !scenes.exists(&cfg.gui.scene) {
        cfg.gui.scene.clear();
    }
    let hub = gui::Hub::new(ctx.shared.clone(), rec.clone(), ctx.vban.clone(), ctx.player.clone(), takes.clone(), scenes);
    hub.set_names(channel_names(cfg));
    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
    hub.autostart.store(desktop::autostart_enabled(), SeqCst);
    hub.installed.store(desktop::menu_installed(), SeqCst);
    hub.cable_welcome.store(opts.cable_welcome, SeqCst);
    hub.set_client(&cfg.client_name);
    // Checks only when asked, unless automatic updates are on.
    let updater = update::Updater::start();
    hub.set_updater(updater.clone());

    let port = gui::start(hub.clone(), opts.port, opts.explicit_port)?;
    gui::note_port(&cfg.client_name, port);
    let url = gui::url(port);
    let mut notes = notes;
    if port != opts.port {
        notes.push(format!("port {} is busy, using {port} instead", opts.port));
    }
    eprintln!("bananasplit: GUI at {url}  (quit with the power button, Cmd+Q or Ctrl-C)");
    for n in &notes {
        eprintln!("bananasplit: {n}");
    }
    let mut window = Window { ui, url: url.clone(), started: false, failed: false, tray: true, recording: false };
    // Started after the first frame is published (it takes a moment). In the
    // background (`--no-open`), the window stays hidden: only the icon.
    let mut open_window = true;
    let mut was_recording = false;

    // What happened at start shows in the page's status line.
    let mut status = notes.join(" | ");
    let mut view = tui::View::default();
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut last_slow = Instant::now() - Duration::from_secs(10);
    let mut last_save = Instant::now();
    let mut dev_rev = 0u64;
    let mut ips = sys::local_ipv4();
    let mut chans = mac::chanmenu::views(cfg);
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
                    status = if errs.is_empty() { "all configured connections made".to_string() } else { errs.join(" | ") };
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
                    ui.send(ToUi::CloseToTray(b));
                }
                Request::Zoom(z) => {
                    cfg.gui.zoom = z;
                    hub.set_look(cfg.gui.theme, cfg.gui.zoom);
                }
                Request::Scene(name) => cfg.gui.scene = name,
                Request::AutoUpdate(on) => {
                    cfg.gui.auto_update = on;
                    // kept at once (a setting about what happens while nobody looks)
                    capture_all(cfg, &ctx.shared, &rec, &ctx.vban);
                    cfg.recorder.play_to = ctx.player.routes();
                    status = match cfg.save(&ctx.path) {
                        Ok(()) if on => "BananaSplit looks for new versions once a day and tells you when there is one".to_string(),
                        Ok(()) => "automatic update checks are off: BananaSplit checks only when you ask".to_string(),
                        Err(e) => format!("save failed: {e}"),
                    };
                    last_save = Instant::now();
                }
                Request::Restart => {
                    if updater.ready() {
                        update::RESTART.store(true, SeqCst);
                        quit = true;
                    }
                }
                Request::Dev(op, reply) => {
                    let is_list = op == gui::DevOp::List;
                    let r = mac::chanmenu::handle(op, cfg);
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
        for m in ui.poll() {
            match m {
                // Closed with "Closing the window keeps it running" off, or Quit.
                FromUi::Quit => quit = true,
                FromUi::Size(w, h) => cfg.gui.window = Some((w.clamp(640, 16384), h.clamp(480, 16384))),
                // The menu bar icon's menu.
                FromUi::Record => {
                    let on = if rec.is_recording() { "0" } else { "1" };
                    status = match gui::command(&hub, &format!("op=rec&v={on}")) {
                        Ok(m) => m,
                        Err(e) => format!("recorder: {e}"),
                    };
                }
                FromUi::NoTray => window.tray = false,
                FromUi::NoWindow(why) => {
                    eprintln!("bananasplit: no window: {why}");
                    window.failed = true;
                    if !opts.no_open {
                        // Not on this thread: the mixer keeps running meanwhile.
                        let url = window.url.clone();
                        std::thread::spawn(move || desktop::no_window(&why, &url));
                    }
                }
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
            chans = mac::chanmenu::views(cfg);
            views_rev = dev_rev;
        }
        if last_slow.elapsed() >= Duration::from_secs(2) {
            last_slow = Instant::now();
            ips = sys::local_ipv4();
            // the recordings folder chosen in the page: the takes and the
            // free space follow it
            let f = rec_folder(&rec);
            if f != folder {
                takes.set_folder(f.clone());
                folder = f;
            }
            disk = sys::disk_space(&folder);
            hub.autostart.store(desktop::autostart_enabled(), SeqCst);
            hub.installed.store(desktop::menu_installed(), SeqCst);
            updater.tick(cfg.gui.auto_update);
        }
        if let Some(m) = updater.take_event() {
            eprintln!("bananasplit: {m}");
            status = m;
        }

        view.update_meters(&ctx.shared);
        let st = ctx.session.status();
        let lat = ctx.session.io_latency();
        let (trev, tlist) = takes.snapshot();
        let scene_list = hub.scene_list();
        let mut update_json = String::new();
        updater.json(&mut update_json, cfg.gui.auto_update);
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
            window: if window.started && !window.failed { "app" } else { "none" },
            tray: window.tray,
            update: &update_json,
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
        // The menu bar icon shows when it records; a finished take is listed.
        let recording = rec.is_recording();
        if recording != was_recording {
            was_recording = recording;
            window.recording = recording;
            ui.send(ToUi::Rec(recording));
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
    ui.send(ToUi::Hide);
    hub.close();
    gui::forget_port(&cfg.client_name, port);
    // What must not be lost comes first (quit from the Dock, or at log out,
    // macOS waits for it only a few seconds): the settings as they are, the
    // take being recorded, the settings file. Then the rest.
    if result.is_ok() {
        capture_all(cfg, &ctx.shared, &rec, &ctx.vban);
        cfg.recorder.play_to = ctx.player.routes();
    }
    ctx.rec.shutdown();
    let saved = result.and_then(|()| cfg.save(&ctx.path));
    takes.shutdown();
    ctx.player.shutdown();
    ctx.vban.shutdown();
    updater.shutdown();
    saved
}

/// A second start: bring up the running BananaSplit's window (started in
/// the background, only say that it runs).
fn show_running(port: u16, no_open: bool) -> ExitCode {
    let url = gui::url(port);
    if no_open {
        eprintln!("bananasplit: already running at {url}");
        return ExitCode::SUCCESS;
    }
    if !gui::request_show(port, "") && desktop::open_page(&url).is_err() {
        return fail(&format!("BananaSplit is already running at {url}, but its window could not be opened"), false);
    }
    ExitCode::SUCCESS
}

/// The mixer's thread in app mode: the GUI loop, then the restart an update
/// asked for. Returns the exit code.
fn mixer_thread(mut ctx: Ctx, mut cfg: Config, notes: Vec<String>, opts: GuiOpts, ui: &Ui) -> i32 {
    let result = run_gui(&mut ctx, &mut cfg, notes, &opts, ui);
    // The audio devices let go first.
    drop(ctx);
    match result {
        Ok(()) if update::RESTART.load(SeqCst) => match update::spawn_restarted(&[]) {
            Ok(()) => 0,
            Err(e) => {
                let _ = fail(&format!("BananaSplit was updated, but could not start again: {e}"), false);
                1
            }
        },
        Ok(()) => 0,
        Err(e) => {
            let _ = fail(&e, false);
            1
        }
    }
}

// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    // Restarted after an update: the BananaSplit it replaces lets go first.
    update::wait_for_previous();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(s) => {
            if s == HELP || s.starts_with("BananaSplit ") {
                print!("{s}");
                return ExitCode::SUCCESS;
            }
            eprintln!("{s}");
            return ExitCode::FAILURE;
        }
    };
    if args.update {
        // A running app would go on with the old version.
        let client = Config::load_or_default(&args.path).map(|c| c.client_name).unwrap_or_else(|_| Config::default().client_name);
        let running = gui::find_running(&client, args.explicit_port.then_some(args.port)).is_some();
        return match update::cli(running) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("bananasplit: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if args.print_default {
        print!("{}", Config::default().to_ini());
        return ExitCode::SUCCESS;
    }
    if args.list {
        return match mac::list_devices() {
            Ok(s) => {
                print!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
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
    // Before load_or_create writes it: the very first start.
    let first_start = !args.path.exists();
    let cfg = match Config::load_or_create(&args.path) {
        Ok(c) => c,
        Err(e) => return fail(&e, args.headless),
    };

    // A second start (from the Terminal, or the login item while BananaSplit
    // already runs) brings up the running mixer's window instead of starting
    // a second audio engine. (Finder and the Dock don't start a second one:
    // they bring the running app to the front.)
    let running = || gui::find_running(&cfg.client_name, args.explicit_port.then_some(args.port));
    if !args.headless {
        if let Some(port) = running() {
            return show_running(port, args.no_open);
        }
    }
    // One mixer per config file, also when two starts overlap (the login
    // item, and macOS reopening the apps that ran at log out, can start it
    // at the same moment): the second waits for the first to answer.
    let lock = args.path.with_extension("lock");
    let _instance = match sys::instance_lock(&lock) {
        Ok(Some(f)) => Some(f),
        Ok(None) => {
            if !args.headless {
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(20) {
                    if let Some(port) = running() {
                        return show_running(port, args.no_open);
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            return fail(&format!("another BananaSplit already runs with {}", args.path.display()), args.headless);
        }
        Err(e) => {
            notes.push(format!("cannot tell whether another BananaSplit runs ({}: {e})", lock.display()));
            None
        }
    };
    mac::ffi::no_app_nap();

    // What an update left next to the program (the old one, now unused).
    update::clean_leftovers();
    // Each channel keeps its own virtual cable (next to the config).
    mac::chanmenu::keep_cables_in(&args.path.with_file_name("cables.txt"));
    let shared = Shared::new();
    cfg.apply(&shared);
    let rate = mac::coreaudio::RATE;
    shared.sample_rate.store(rate, SeqCst);

    let rec_tap = RecTap::new();
    let vban_tap = VbanTap::new();
    let play_tap = PlayTap::new();
    let recorder = Arc::new(Recorder::new(rec_tap.clone(), rate, cfg.recorder.settings()));
    let vban = Arc::new(Vban::start(vban_tap.clone(), rate, cfg.vban.clone()));
    let player = Arc::new(Player::new(play_tap.clone(), rate));
    player.set_routes(cfg.recorder.play_to);

    let no_cables = mac::first_run(args.headless, &mut notes);
    let session = match Session::open(&cfg, shared.clone(), rec_tap.clone(), vban_tap.clone(), play_tap.clone(), &mut notes) {
        Ok(s) => s,
        Err(e) => return fail(&e, args.headless),
    };

    let mut ctx = Ctx { path: args.path.clone(), shared: shared.clone(), rec: recorder, vban, player, session };
    // The graph Session::open built (opening every device again would only
    // take time); its connection errors.
    let (_, errs) = ctx.session.connect(&cfg, false);
    notes.extend(errs);

    if args.headless {
        // (nothing changes the settings headless: the file is left as it is)
        let result = run_headless(&mut ctx, &cfg, notes);
        drop(ctx);
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e, true),
        };
    }

    // The app on the main thread, the mixer on its own.
    let (event_loop, ui, tx) = app::event_loop(args.no_open);
    let done = Arc::new(AtomicBool::new(false));
    let opts = GuiOpts {
        port: args.port,
        explicit_port: args.explicit_port,
        no_open: args.no_open,
        cable_welcome: first_start && no_cables && !args.no_open,
    };
    let mixer_done = done.clone();
    let spawned = std::thread::Builder::new().name("bs-main".into()).spawn(move || {
        // A panic ends the app too, rather than leaving a window with no
        // mixer behind it.
        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mixer_thread(ctx, cfg, notes, opts, &ui))).unwrap_or(101);
        mixer_done.store(true, SeqCst);
        ui.send(ToUi::Exit(code));
    });
    if let Err(e) = spawned {
        return fail(&format!("cannot start the mixer: {e}"), false);
    }
    app::run(event_loop, tx, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn command_line() {
        let a = args(&["--no-open", "--gui-port", "8800", "--config", "/tmp/x.ini"]).unwrap();
        assert!(a.no_open && a.explicit_port && a.port == 8800 && a.path == std::path::Path::new("/tmp/x.ini"));
        // what Finder passed to apps on older macOS versions
        assert!(args(&["-psn_0_123456"]).is_ok_and(|a| !a.headless && !a.no_open));
        assert!(args(&["--bogus"]).is_err());
        assert!(args(&["-V"]).unwrap_err().starts_with("BananaSplit 2."));
        assert_eq!(args(&["--help"]).unwrap_err(), HELP);
    }
}
