//! Doing what a [`Cmd`] asks, on the mixer: the work of `POST /api/cmd`, after
//! `Cmd::parse` has read and checked the form. What can only be known from the
//! mixer's state (a bus send that would loop, no recordings yet, no updater) is
//! decided here. Success answers with the short text the page shows.

use super::api::ApiError;
use super::cmd::{Cmd, DynSetting, SceneAct, UpdateAct};
use super::{DevOp, Hub, Request, changed_take, lock, pan_text, set_bit_u8};
use bananasplit::engine::{BUS_IDS, NUM_STRIPS};
use bananasplit::eq::DEFAULT_BANDS;
use bananasplit::player::PlayState;
use bananasplit::recorder::{self, ALL_LINKS};
use std::sync::atomic::Ordering::SeqCst;

/// Read one `POST /api/cmd` body and do what it asks.
pub fn command(hub: &Hub, body: &str) -> Result<String, ApiError> {
    apply(hub, Cmd::parse(body)?)
}

fn ok() -> Result<String, ApiError> {
    Ok("ok".into())
}

/// What a channel is connected to is the main loop's to change: it answers.
fn ask(hub: &Hub, op: DevOp) -> Result<String, ApiError> {
    hub.ask(op).map_err(ApiError::from)
}

pub fn apply(hub: &Hub, cmd: Cmd) -> Result<String, ApiError> {
    let sh = &*hub.shared;
    match cmd {
        Cmd::Gain { ch, db } => {
            if ch < NUM_STRIPS {
                sh.strips[ch].gain_db.store(db)
            } else {
                sh.buses[ch - NUM_STRIPS].gain_db.store(db)
            }
            Ok(format!("{db:+.1} dB"))
        }
        Cmd::Trim { ch, db } => {
            if ch < NUM_STRIPS {
                sh.strips[ch].trim_db.store(db)
            } else {
                sh.buses[ch - NUM_STRIPS].trim_db.store(db)
            }
            Ok(format!("gain {db:+.1} dB"))
        }
        Cmd::Pan { ch, pan } => {
            if ch < NUM_STRIPS {
                sh.strips[ch].pan.store(pan)
            } else {
                sh.buses[ch - NUM_STRIPS].pan.store(pan)
            }
            Ok(pan_text(pan))
        }
        Cmd::Mute { ch, on } => {
            let a = if ch < NUM_STRIPS { &sh.strips[ch].mute } else { &sh.buses[ch - NUM_STRIPS].mute };
            a.store(on, SeqCst);
            ok()
        }
        Cmd::Mono { ch, on } => {
            let a = if ch < NUM_STRIPS { &sh.strips[ch].mono } else { &sh.buses[ch - NUM_STRIPS].mono };
            a.store(on, SeqCst);
            ok()
        }
        Cmd::Solo { strip, on } => {
            sh.strips[strip].solo.store(on, SeqCst);
            ok()
        }
        Cmd::Route { ch, bus, on } => {
            if ch < NUM_STRIPS {
                set_bit_u8(&sh.strips[ch].routes, bus, on);
                return ok();
            }
            // a bus feeding another bus: never itself, never in a loop
            let from = ch - NUM_STRIPS;
            let _g = lock(&hub.routing);
            if on && from == bus {
                return Err(format!("{} cannot feed itself", BUS_IDS[from]).into());
            }
            if on && sh.send_loops(from, bus) {
                return Err(format!(
                    "{} already feeds {} (through its sends): that would be a loop",
                    BUS_IDS[bus], BUS_IDS[from]
                )
                .into());
            }
            set_bit_u8(&sh.buses[from].routes, bus, on);
            ok()
        }
        // ---- what a channel is connected to (the main loop does it) ----
        Cmd::Cable { ch, on } => ask(hub, DevOp::Cable { ch, on }),
        Cmd::Dev { ch, client, on } => ask(hub, DevOp::Client { ch, client, on }),
        Cmd::DevDefault { ch, on } => ask(hub, DevOp::Default { ch, on }),
        Cmd::DevPattern { ch, pattern } => ask(hub, DevOp::Unpattern { ch, pattern }),
        Cmd::DevNone { ch } => ask(hub, DevOp::Nothing { ch }),
        Cmd::Stream { rec, id, ch } => ask(hub, DevOp::Stream { rec, id, ch }),
        Cmd::Link { ch, on } => {
            if on {
                sh.rec_links.fetch_or(1 << ch, SeqCst)
            } else {
                sh.rec_links.fetch_and(!(1u16 << ch), SeqCst)
            };
            ok()
        }
        Cmd::Rec { on } => {
            let rec = &hub.recorder;
            if on {
                let names = lock(&hub.names).clone();
                let files =
                    rec.start(sh.rec_links.load(SeqCst) & ALL_LINKS, &names, &crate::gui_platform::local_timestamp())?;
                Ok(match files.len() {
                    1 => format!("recording to {}", files[0].display()),
                    n => format!(
                        "recording {n} tracks to {}",
                        files[0].parent().map_or(String::new(), |p| p.display().to_string())
                    ),
                })
            } else {
                rec.stop();
                Ok("recording stopped".into())
            }
        }
        Cmd::RecSet { mode, format, kbps, folder } => {
            let rec = &hub.recorder;
            let mut s = rec.settings();
            if let Some(m) = mode {
                s.mode = m;
            }
            if let Some(fm) = format {
                s.format = fm;
            }
            if let Some(k) = kbps {
                s.kbps = k;
            }
            if let Some(d) = folder {
                s.folder = d;
            }
            rec.set_settings(s)?;
            Ok(if rec.is_recording() { "applies to the next recording".into() } else { "ok".into() })
        }
        Cmd::Name { ch, name } => {
            hub.request(Request::Rename(ch, name));
            ok()
        }
        Cmd::Eq { ch, on } => {
            sh.eq(ch).on.store(on, SeqCst);
            Ok(if on { "EQ on" } else { "EQ off" }.into())
        }
        Cmd::Band { ch, band, on, kind, freq, gain_db, q } => {
            let p = &sh.eq(ch).bands[band];
            let mut b = p.load();
            if let Some(on) = on {
                b.on = on;
            }
            if let Some(k) = kind {
                b.kind = k;
            }
            if let Some(v) = freq {
                b.freq = v;
            }
            if let Some(v) = gain_db {
                b.gain_db = v;
            }
            if let Some(v) = q {
                b.q = v;
            }
            p.store(&b);
            ok()
        }
        Cmd::EqReset { ch } => {
            sh.eq(ch).store_bands(&DEFAULT_BANDS);
            Ok("EQ bands reset (flat)".into())
        }
        Cmd::Gate { strip, on } => {
            sh.strips[strip].dynamics.gate_on.store(on, SeqCst);
            Ok(format!("Gate {}", if on { "on" } else { "off" }))
        }
        Cmd::Comp { strip, on } => {
            sh.strips[strip].dynamics.comp_on.store(on, SeqCst);
            Ok(format!("Compressor {}", if on { "on" } else { "off" }))
        }
        Cmd::DynSet { strip, setting, value } => {
            let d = &sh.strips[strip].dynamics;
            match setting {
                DynSetting::Gate(i) => d.gate[i].store(value),
                DynSetting::Comp(i) => d.comp[i].store(value),
            }
            ok()
        }
        Cmd::DynReset { strip } => {
            sh.strips[strip].dynamics.reset_settings();
            Ok("Gate and compressor settings reset".into())
        }
        Cmd::Vban { on } => {
            hub.vban.update(|c| {
                c.enabled = on;
                Ok(())
            })?;
            Ok(if on { "VBAN on" } else { "VBAN off" }.into())
        }
        Cmd::VbanPort { port } => {
            hub.vban.update(|c| {
                c.port = port;
                Ok(())
            })?;
            Ok(format!("VBAN listens on UDP port {port}"))
        }
        Cmd::VbanIn { slot, name, from, strip, buffer_ms, on } => {
            hub.vban.update(|c| {
                let s = &mut c.ins[slot];
                if let Some(v) = name {
                    s.name = v;
                }
                if let Some(v) = from {
                    s.from = v;
                }
                if let Some(v) = strip {
                    s.strip = v;
                }
                if let Some(v) = buffer_ms {
                    s.buffer_ms = v;
                }
                if let Some(v) = on {
                    s.on = v;
                }
                Ok(())
            })?;
            ok()
        }
        Cmd::VbanOut { slot, name, to, port, source, format, channels, on } => {
            hub.vban.update(|c| {
                let s = &mut c.outs[slot];
                if let Some(v) = name {
                    s.name = v;
                }
                if let Some(v) = to {
                    s.to = v;
                }
                if let Some(v) = port {
                    s.port = v;
                }
                if let Some(v) = source {
                    s.source = v;
                }
                if let Some(v) = format {
                    s.format = v;
                }
                if let Some(v) = channels {
                    s.channels = v;
                }
                if let Some(v) = on {
                    s.on = v;
                }
                Ok(())
            })?;
            ok()
        }
        // ---- player ----
        Cmd::Play { from, key } => {
            let p = &hub.player;
            let st = p.status();
            let key = match key {
                Some(k) => k,
                None => match (&st.take, hub.takes.snapshot().1.last()) {
                    (Some(k), _) => k.clone(),
                    (None, Some(t)) => t.key.clone(),
                    (None, None) => return Err("there are no recordings to play yet".into()),
                },
            };
            if st.take.as_deref() == Some(key.as_str()) && from.is_none() {
                if st.state == PlayState::Playing {
                    return Ok("playing".into());
                }
                match changed_take(hub, &st) {
                    Some(t) => p.play(&t, st.pos, true)?,
                    None => p.resume()?,
                }
            } else {
                let take = hub.takes.find(&key).ok_or("that recording is no longer in the recorder folder")?;
                p.play(&take, from.unwrap_or(0.0), true)?;
            }
            Ok("playing".into())
        }
        Cmd::Take { key } => {
            let take = hub.takes.find(&key).ok_or("that recording is no longer in the recorder folder")?;
            hub.player.play(&take, 0.0, false)?;
            Ok(format!("{} selected", take.label()))
        }
        Cmd::Pause => {
            hub.player.pause()?;
            Ok("paused".into())
        }
        Cmd::Stop => {
            hub.player.stop()?;
            Ok("stopped".into())
        }
        Cmd::Seek { to } => {
            let st = hub.player.status();
            match changed_take(hub, &st) {
                Some(t) => hub.player.play(&t, to, st.state == PlayState::Playing)?,
                None => hub.player.seek(to)?,
            }
            ok()
        }
        Cmd::PlayTo { bus, on } => {
            let m = hub.player.routes();
            hub.player.set_routes(if on { m | 1 << bus } else { m & !(1u8 << bus) });
            ok()
        }
        Cmd::OpenFolder => {
            let dir = recorder::expand_home(&hub.recorder.settings().folder);
            crate::gui_platform::open_folder(&dir)?;
            Ok(format!("opened {}", dir.display()))
        }
        // ---- scenes ----
        Cmd::Scene { name, act } => Ok(match act {
            SceneAct::Load => {
                hub.scenes.load(&name, sh)?;
                hub.request(Request::Scene(name.clone()));
                format!("scene \"{name}\" loaded")
            }
            SceneAct::Save => {
                hub.scenes.save(&name, sh)?;
                hub.refresh_scenes();
                hub.request(Request::Scene(name.clone()));
                format!("scene \"{name}\" saved")
            }
            SceneAct::Delete => {
                hub.scenes.delete(&name)?;
                hub.refresh_scenes();
                hub.request(Request::Scene(String::new()));
                format!("scene \"{name}\" deleted")
            }
        }),
        // ---- app settings ----
        Cmd::Theme(t) => {
            hub.request(Request::Theme(t));
            ok()
        }
        Cmd::CloseToTray(on) => {
            hub.request(Request::CloseToTray(on));
            ok()
        }
        Cmd::Zoom(z) => {
            hub.request(Request::Zoom(z));
            ok()
        }
        Cmd::Autostart(on) => {
            let msg = crate::gui_platform::set_autostart(on)?;
            hub.autostart.store(crate::gui_platform::autostart_enabled(), SeqCst);
            Ok(msg)
        }
        Cmd::Install => {
            let msg = crate::gui_platform::install()?;
            hub.installed.store(crate::gui_platform::menu_installed(), SeqCst);
            Ok(msg)
        }
        Cmd::AutoUpdate(on) => {
            hub.request(Request::AutoUpdate(on));
            ok()
        }
        Cmd::Update(act) => {
            let u = hub.updater()?;
            match act {
                UpdateAct::Check => u.check().map_err(ApiError::from),
                UpdateAct::Install => u.install().map_err(ApiError::from),
                UpdateAct::Restart => {
                    if !u.ready() {
                        return Err("no update is installed yet".into());
                    }
                    hub.request(Request::Restart);
                    Ok("restarting BananaSplit".into())
                }
                UpdateAct::Page => {
                    crate::gui_platform::open_page(&u.page())?;
                    Ok("opening the release page in your browser".into())
                }
                UpdateAct::Unknown(a) => Err(format!("unknown update action {a:?}").into()),
                UpdateAct::Missing => Err(format!("missing {:?}", "a").into()),
            }
        }
        Cmd::Show { token } => {
            hub.request(Request::Show(token));
            ok()
        }
        Cmd::Save => {
            hub.request(Request::Save);
            Ok("saving".into())
        }
        Cmd::Reconnect => {
            hub.request(Request::Reconnect);
            Ok("reconnecting".into())
        }
        Cmd::Quit => {
            hub.request(Request::Quit);
            Ok("quitting".into())
        }
        Cmd::Platform { op, form } => crate::gui_platform::command(hub, &op, &form)
            .map_or_else(|| Err(format!("unknown op {op:?}").into()), |r| r.map_err(ApiError::from)),
    }
}
