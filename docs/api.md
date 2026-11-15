# BananaSplit's local API

The mixer's window, the touch deck (`/deck`) and `bananasplit --quit` all talk to a small HTTP
server that every BananaSplit (Linux, Windows, macOS) runs on the same computer. Anything on that
computer, run by the same user, can use it the same way: set a fader, start a recording, read the
meters.

This describes BananaSplit 2.8.22 and the changes merged since: the `platform` field of the state
and errors as JSON on request (both marked *new*), and **API v1** (from 2.9.0): one versioned command
model with paths, relative changes, fades and batches, which the MIDI bindings use too. `/api/cmd` is
the API the page is written against; there is no promise yet that it never changes, and changes are
named in the release notes. API v1 is the one to build on: it only grows, and anything that would
change what it does gets a new version (`/api/v2/`).

* [Reaching it](#reaching-it)
* [What it checks](#what-it-checks)
* [Endpoints](#endpoints)
* [`POST /api/cmd`: the commands](#post-apicmd-the-commands)
* [The state](#the-state)
* [Other answers](#other-answers)
* [API v1](#api-v1)
* [Errors](#errors)
* [Examples](#examples)

## Reaching it

`http://127.0.0.1:8790/`. The port is 8790 unless `--gui-port` says another one, or 8790 was busy
when BananaSplit started (then it takes a free one). A running BananaSplit notes its port in a file,
`bananasplit/<JACK client name>.port` (a name that is not letters, digits, `-` or `_` has those
characters replaced by `_`), in `$XDG_RUNTIME_DIR` (Linux; `~/.cache` without it; in a Flatpak
`$XDG_RUNTIME_DIR/app/<app ID>`), `%LOCALAPPDATA%` (Windows) or `~/Library/Caches` (macOS). The
server listens on 127.0.0.1 only. `GET /api/ping` answers `bananasplit-gui` and tells which mixer
it is (below); `bananasplit --gui` and `--quit` find a running one that way.

## What it checks

Every request has to pass these, or it is refused with a 403 before anything is read or done:

* **The other end is you.** The connection must come from a process of the user BananaSplit runs as
  (another account on the same computer is refused: *"BananaSplit only answers the user it runs
  as"*). If the system's list of connections can't be read (a sandbox, a hardened `/proc`), whose
  program it is can't be told, so every connection is refused, yours too, and the answer says so
  and names the list (from 2.8.26).
* **`Host`** must name this server: `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>`
  (*"wrong Host"*). A web page cannot talk to the server through a DNS name it controls.
* **Under `/api/`, not from another web page.** A request with `Sec-Fetch-Site: cross-site` or
  `same-site`, or with an `Origin` that is not one of the three `http://` origins above, is refused
  (*"forbidden"*). Programs (curl, scripts) send neither header and pass.
* **`POST` also needs `X-BananaSplit: 1`** (*"forbidden"* without it). A custom header makes a
  browser ask first, which the server never allows, so other pages cannot press buttons.

Limits: a request header of at most 16 KB, a body of at most 64 KB, a `Content-Length` (chunked
bodies are refused with 411), 32 connections at a time (a 33rd gets 503 *"busy"*) and 8 event
streams (*"too many windows open"*). Every answer closes the connection (`Connection: close`),
except the event stream.

## Endpoints

| Request | Answers |
|---|---|
| `GET /`, `GET /index.html` | The page (HTML). |
| `GET /deck`, `GET /deck.html` | The touch deck: the everyday controls for a small touchscreen. |
| `GET /api/ping` | `bananasplit-gui` (text), with the header `X-BananaSplit-Client: <JACK client name, percent-encoded>`. |
| `GET /api/state` | The latest [state](#the-state), once (JSON). |
| `GET /api/events` | The state as it changes: a server-sent event stream. |
| `GET /api/takes` | The recordings in the recorder's folder (JSON). |
| `GET /api/wave?key=K&n=N` | One recording's waveform overview (JSON). |
| `GET /api/devices` | What each channel can be connected to: the lists behind the channel menus (JSON). |
| `GET /api/update` | The updater's release page and notes (JSON). |
| `GET /api/cables` | Windows and macOS only: the virtual cables found and the system's defaults (JSON). 404 on Linux. |
| `POST /api/cmd` | One change, form-encoded: [the commands](#post-apicmd-the-commands). |
| `GET /api/v1/state` | The state with the API's version and `rev`: [API v1](#api-v1). |
| `GET /api/v1/paths` | Every value API v1 can change, with its range and value now. |
| `POST /api/v1/command` | One command or a batch, as JSON. |
| `POST /api/v1/scenes/{name}/recall` | Load a saved scene. |
| `POST /api/v1/recorder/start`, `.../stop` | Start or stop recording. |

Anything else under a known method is a 404; other methods are 405 (`Allow: GET, POST`).

## `POST /api/cmd`: the commands

The body is a form (`application/x-www-form-urlencoded`: `op=gain&ch=2&v=-3.5`; `+` and `%xx` as
usual, UTF-8). `op` says what to do, the other fields say with what. Fields may come in any order.
It answers 200 with a short text (the commands below say what it is; the page shows it) or
[an error](#errors).

A request is read in the order the fields are listed here, and the first one that is wrong is the
one that is said: with `op=gain&v=x`, the message is that `ch` is missing, not that `v` is not a
number.

**Values**

| | |
|---|---|
| `ch` | A channel, `0` to `10`: `0` to `5` are the input strips 1 to 6, `6` to `10` the buses V1, V2, V3, B1, B2 (V1 to V3 were called A1 to A3 up to 2.8.24). |
| flag (`v`, `on`) | `1`, `true` or `on`; `0`, `false` or `off`. |
| `bus` | A bus, `0` to `4` (V1 to B2). |

**Levels and routing**

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `gain` | `ch`, `v` dB | The channel's fader, -60 to +12 dB (clamped), to 0.1. | `-3.5 dB` |
| `trim` | `ch`, `v` dB | Input gain, -24 to +24 dB (clamped), to 0.1. | `gain +6.0 dB` |
| `pan` | `ch`, `v` | Pan (balance on a bus), -1 to 1 (clamped), to 0.01. | `L25`, `C`, `R100` |
| `limit` | `ch` (a bus), `v` dB or `off` | The bus's brickwall limiter (from 2.8.27): nothing leaves the bus louder than this, -40 to +12 dBFS (clamped), to 0.1; +12 or `off` = off. Strips have none. Not part of a scene. | `limit -6.0 dB`, `limit off` |
| `mute`, `mono` | `ch`, `v` flag | | `ok` |
| `solo` | `ch` (a strip), `v` flag | Buses have no solo. | `ok` |
| `route` | `ch`, `bus`, `v` flag | A strip or bus feeds a bus. A bus cannot feed itself or make a loop of sends. | `ok` |

**What a channel is connected to** (the main loop does it, and its answer comes back as the text)

| `op` | Fields | Does |
|---|---|---|
| `cable` | `ch`, `v` flag | The channel's virtual cable on or off. |
| `dev` | `ch`, `client`, `v` flag | Connect or disconnect a device or program (`client` is a name from `/api/devices`; at most 320 characters, no control characters). |
| `devdefault` | `ch`, `v` flag | `connect = @default`: follow the system's default device. |
| `devpattern` | `ch`, `pattern` | Remove one `connect` line from the config (and what only it connected). |
| `devnone` | `ch` | Disconnect everything (the cable stays). |
| `stream` | `kind` (`play` or `rec`), `id`, `ch` (`none` or empty: back to the default device) | Move an app's stream (from `play`/`rec` in `/api/devices`) into a channel's cable. |

**Recording**

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `link` | `ch`, `v` flag | Whether the channel is recorded. | `ok` |
| `rec` | `v` flag | Start or stop. Starting needs at least one linked channel. | `recording to <file>`, `recording 3 tracks to <folder>`, `recording stopped` |
| `recset` | any of `mode` (`mix` or `multi`), `format` (`mp3`, `wav16`, `wav24`, `wav32f`), `kbps` (128, 160, 192, 224, 256, 320), `folder` | Recorder settings; what is not given stays. | `ok`, or `applies to the next recording` |
| `name` | `ch`, `v` | Rename a channel: 1 to 32 characters, no control characters. | `ok` |
| `openfolder` | | Open the recorder's folder in the file manager. | `opened <folder>` |

**EQ, gate and compressor**

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `eq` | `ch`, `v` flag | The channel's EQ on or off. | `EQ on` / `EQ off` |
| `band` | `ch`, `band` (`0` to `5`), any of `on` flag, `type` (`bell`, `lowshelf`, `highshelf`, `lowcut`, `highcut`, `notch`), `f` Hz (20 to 20000), `g` dB (-24 to 24), `q` (0.1 to 30) | Change what is given of one band. | `ok` |
| `eqreset` | `ch` | All six bands flat. | `EQ bands reset (flat)` |
| `gate`, `comp` | `ch` (a strip), `v` flag | The gate or the compressor on or off. | `Gate on`, `Compressor off` ... |
| `dynset` | `ch` (a strip), `key`, `v` | One setting. Gate: `gate_threshold` (dB, -80 to 0), `gate_range` (-80 to 0), `gate_attack` (ms, 0.1 to 50), `gate_hold` (0 to 2000), `gate_release` (5 to 4000). Compressor: `comp_threshold` (dB, -60 to 0), `comp_ratio` (1 to 20), `comp_knee` (0 to 24), `comp_attack` (ms, 0.1 to 200), `comp_release` (10 to 4000), `comp_makeup` (dB, 0 to 24). A value outside its range is refused. | `ok` |
| `dynreset` | `ch` (a strip) | Gate and compressor settings back to their defaults. | `Gate and compressor settings reset` |

**VBAN** (network audio, Voicemeeter-compatible)

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `vban` | `v` flag | VBAN on or off. | `VBAN on` / `VBAN off` |
| `vbanport` | `v` | The UDP port to listen on, 1 to 65535. | `VBAN listens on UDP port 6980` |
| `vbanin` | `slot` (`0` to `7`), any of `name` (1 to 16 printable ASCII characters, no space at either end), `from` (an IPv4 address, `any`, or empty), `strip` (`1` to `6`: the strip it plays into), `buffer` (ms, 0 to 200), `on` flag | One incoming stream. | `ok` |
| `vbanout` | `slot` (`0` to `7`), any of `name`, `to` (an IPv4 address, or empty), `port`, `src` (`1` to `6`, `V1` to `B2`; the old `A1` to `A3` are accepted too), `format` (`int16`, `int24`, `int32`, `float32`), `channels` (`1` or `2`), `on` flag | One outgoing stream. | `ok` |

**Player**

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `play` | any of `key` (a take from `/api/takes`; none: the loaded one, else the newest), `from` seconds | Play, or resume. | `playing` |
| `take` | `key` | Load a take without playing. | `<label> selected` |
| `pause`, `stop` | | | `paused`, `stopped` |
| `seek` | `v` seconds | | `ok` |
| `playto` | `bus`, `v` flag | Which buses the player feeds. | `ok` |

**Scenes, settings and the program**

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `scene` | `name` (1 to 40 letters, digits, spaces and ``- _ . , ( ) & + ' !``, not starting with a dot), `act` (`load`, `save`, `delete`) | | `scene "Live" saved` ... |
| `theme` | `v` (`dark`, `light`, `auto`) | | `ok` |
| `look` | `v` (`classic`, `console`) | *New:* the theme set: the page's whole look, each with its own dark and light (`theme`). | `ok` |
| `zoom` | `v` (0.5 to 2.0) | | `ok` |
| `listen` | `v` a device's client name (empty: none) | The output device you listen on (from 2.8.27): bus cards playing to it get a blue border. | `ok` |
| `closetray` | `v` flag | "Closing the window keeps it running". | `ok` |
| `autostart` | `v` flag | Start with the system. | what it did |
| `install` | | Add BananaSplit to the application menu. | what it did |
| `autoupdate` | `v` flag | Automatic updates on or off. | `ok` |
| `update` | `a` (`check`, `install`, `restart`, `page`) | The updater. | what it did |
| `show` | `token` (optional: the launch's activation token, printable ASCII, 256 characters at most) | Bring the window up (what a second `bananasplit --gui` does). | `ok` |
| `save` | | Save the settings now. | `saving` |
| `reconnect` | | Connect to the audio server again. | `reconnecting` |
| `quit` | | Save and quit. | `quitting` |

**MIDI controllers** (from 2.9.0; see [API v1](#api-v1) for the paths a binding drives)

| `op` | Fields | Does | Answers |
|---|---|---|---|
| `midiport` | any of `in`, `out` (a port name from `midi.ins` / `midi.outs`, empty: none) | The port the controller sends on and the one it listens on (LEDs, motor faders). Opened as soon as it is there, again after it was unplugged. | `ok` |
| `midi` | `a` (`learn`, `stop`) | Learning: the next control moved on the controller is caught (`midi.learn`); nothing is changed while learning. Stops by itself after 60 s. | `move a control on your MIDI controller`, `learning stopped` |
| `midibind` | `src`, `target`, any of `mode`, `takeover` flag, `feedback` flag | Bind a control to a value, or replace its binding. `src`: `cc:CH:N`, `cc14:CH:N` (N 0 to 31, its LSB on N+32), `note:CH:N`, `pb:CH` (pitch bend), `nrpn:CH:N`, `pc:CH:N` (program change); CH 1 to 16. `target`: an API v1 path, or `scene:<name>`. `mode`: `abs`, `rel2` (1 up, 127 down), `rel64` (65 up, 63 down), `relsign` (1 up, 65 down), `toggle`, `momentary`, `trigger`; left out: the first that fits. A button cannot set a number. `takeover` (soft-takeover, default on for absolute controls except pitch bend) and `feedback` (send the value back, default on except for scenes and program changes). At most 256. | `CC 7 · ch 1 → Strip 1: fader` |
| `midiunbind` | `src` | Remove a control's binding. | `CC 7 · ch 1 unbound`, `... was not bound` |

**Windows and macOS only:** `cables` with `a` = `page` (the cable download page), `sound` (the
system's sound settings), `seen` (the first-start welcome was shown), and `splitter` (Windows) or
`midi` (macOS, Audio MIDI Setup). Any other `op` is refused: *unknown op "zzz"*.

## The state

`GET /api/state` returns it once, `GET /api/events` sends it again whenever it changes (about 30
times a second while there is something to show). It is one JSON object:

| Field | |
|---|---|
| `client` | The JACK client name (Linux: the mixer's name in the audio graph). |
| `sr`, `bs` | Sample rate (Hz) and buffer size (frames). |
| `period`, `rtt` | One buffer's length and the round-trip latency, in ms. `measured`: `rtt` was measured, not estimated from the buffer. |
| `cpu` | Audio load, percent. `xruns`: dropouts since start. `oversize`: cycles of more than 16384 frames, which are silenced instead of processed. `panics` (*new*): audio cycles in which the mixer hit a bug (a Rust panic) and played silence instead of stopping. |
| `cables` | How many virtual cables are in use. `devRev` goes up when what a channel menu lists changes. |
| `status` | The status line (text). `anySolo`: a strip is soloed. |
| `ch` | The 11 channels, below. |
| `rec` | The recorder: `on`, `takeMode` (`mix` or `multi`: how the current take is recorded), the settings `mode`, `format`, `kbps`, `sr`, `folder` (as written) and `path` (expanded), the running take's `secs`, `quiet` (seconds in which nothing audible was recorded), `dropped` (frames lost to a full buffer, filled with silence), `clipped` (samples louder than full scale that were clipped), `tracks`, `files` (the files being written), `error` and `note` (text or `null`). |
| `vban` | `on`, `port`, `ips` (this computer's addresses: `if`, `ip`, `bc` broadcast), `listening` (the port actually bound, or `null`), `error`, `in` (8 streams: `on`, `name`, `from`, `strip` (0 to 5), `buffer`, `state` (`off`, `blocked`, `waiting`, `receiving`), `text`, `lost`, `dropouts`, `ppm`), `out` (8 streams: `on`, `name`, `to`, `port`, `src` (0 to 10), `format`, `channels`, `state` (`off`, `blocked`, `sending`), `text`, `packets`, `errors`), `seen` (streams heard on the port: `name`, `from`, `sr`, `ch`, `format`). |
| `player` | `state` (`stopped`, `playing`, `paused`), `take` (key or `null`), `pos`, `dur` (seconds), `underruns`, `error`. `playTo`: the buses it feeds, a bit mask (bit 0 = V1 ... bit 4 = B2). |
| `takes` | `rev` (goes up when the list changes), `count`, `newest` (key or `null`). The list itself is `GET /api/takes`. |
| `disk` | `total` and `free` bytes where recordings go, and `low` (less than 1 GB free: the recorder stops and does not start; `rec.error` says why), or `null`. |
| `scene`, `scenes` | The current scene's name (empty: none) and the saved scenes. |
| `app` | `theme`, `look` (*new*: `classic` or `console`), `closeToTray`, `zoom`, `listen` (from 2.8.27: the device you listen on, `""` = none, `null` = not asked yet), `autostart`, `installed` (in the application menu), `window` (`app`: its own window, `browser`, `none`), `tray`, `platform` (*new*: `linux`, `windows` or `macos`) and, on Windows and macOS only, `welcome` (the first-start cables window is still to be shown). |
| `midi` | From 2.9.0, once MIDI runs: `available` (false: no MIDI on this system, `error` says why), `in`, `out` (the chosen ports, `""` = none), `inOpen`, `outOpen`, `ins`, `outs` (the ports there are now), `error` (`null` or why a port did not open), `last` (the last message received, as text), `lost` (messages dropped because they came in faster than they were read), `learn` (`null`, or `src`, `name` and the suggested `mode` of the control caught, each `null` until one is moved), `bindings` (each `src`, `name`, `target`, `label`, `mode`, `takeover`, `feedback`). |
| `update` | `null` (no updater), or `auto`, `phase` (`idle`, `checking`, `current`, `available`, `downloading`, `prepared`, `installing`, `ready`, `failed`), `current`, `latest`, `installed` (versions, or `null`), `message`, `got`, `total` (bytes), `method` (`replace`, `package`, `manual`), `canInstall`, `checkedAgo` (seconds or `null`), `rev`. |

A channel in `ch`:

| Field | |
|---|---|
| `id` | `1` to `6`, `V1`, `V2`, `V3`, `B1`, `B2` (until 2.8.24: `A1`, `A2`, `A3`). `name` is its name; `strip` is `true` for an input strip. |
| `gain`, `trim`, `pan` | dB, dB, -1 to 1. `mute`, `mono`; `link`: it is recorded; `solo` (strips only). |
| `routes` | A bit mask over the buses (bit 0 = V1 ... bit 4 = B2): the buses a strip feeds, or a bus sends to. |
| `from`, `via` | Buses only (from 2.8.25): what reaches the bus, as a bit mask over the channels (bits 0 to 5 = strips 1 to 6, bit 6 = V1 ... bit 10 = B2). `from`: routed or sent straight into it; `via`: reaching it only through other buses (strip 2 → V1 → B1 sets bit 1 of B1's `via`). |
| `limit`, `limGr` | Buses only (from 2.8.27): the limiter's setting in dBFS (12 = off), and its gain reduction now in dB (0 = not limiting; it falls back about 20 dB a second). |
| `apps` | From 2.8.27: the apps taking sound from the channel right now (a bus's virtual mic: Discord, OBS, ...), as names. Linux only; always `[]` on Windows and macOS and for strips. |
| `m`, `mi` | Output and input level `[left, right]` in dB (about -60: silence). |
| `conn` | What it is connected to, as text. `dev` is what the menu button says. `cable`: it has a virtual cable. |
| `eq` | `on`, and `b`: six bands, each `[on (0 or 1), type, Hz, dB, Q]`. |
| `dyn` | Strips only: `gate` and `comp` on/off, `g` (5 gate settings) and `c` (6 compressor settings) in the order of the `dynset` table, `ggr` and `cgr`: the gain reduction in the last cycle, dB. |

## Other answers

**`GET /api/ping`.** `bananasplit-gui`, and the header `X-BananaSplit-Client` with the JACK client
name, so that a second `bananasplit --gui` can tell its own mixer from another one on the same port.

**`GET /api/events`.** `text/event-stream`: `retry: 1000`, then `data: <the state>` on every change
and `: ping` once a second while nothing changes. Open it with `EventSource` or `curl -N`.

**`GET /api/takes`.** `{"rev": N, "takes": [...]}`, oldest first. A take: `key` (what `play`, `take`
and `/api/wave` use), `n` (its number in the list), `when`, `label`, `tracks` (labels), `secs`, `sr`,
`fmt`, `bytes`.

**`GET /api/wave?key=K&n=N`.** `{"key", "n", "secs", "peaks": [...]}`: `n` peak values (default 480,
16 to 2048), each 0 to 100 percent of full scale (1 at least where there is sound). 404 for an unknown
take.

**`GET /api/devices`.** `cables` (`ok`, `why`: whether virtual cables work; Windows and macOS also have
`none`), `pulse` (the sound server's streams are listed), `strip` and `bus` (what each side can connect
to: `client`, `label`, `kind` = `device`, `app` or `monitor`, `default`), `play` and `rec` (streams that
play or record: `id`, `app`, `media`, `on`, `ch`: the channel whose cable it is in, or -1), and `ch`
(for each channel: `id`, `cable` {`on`, `name`}, `conn`, `connect`: its `connect` lines). 503 while the
main loop is busy.

**`GET /api/update`.** `{"page", "notes", "manual", "rev"}`: the release's page and notes, `manual`
(why it has to be installed by hand, or `null`). 404 without an updater.

**`GET /api/cables`** (Windows and macOS). Windows: `splitter` {`found`, `cables`, `missing`}, `pairs`
(VB-Audio cables), `voicemeeter`, `output` and `input` (the system's defaults: `name`, `cable`).
macOS: `cables`, `output`, `input`. 503 with the reason if the system's devices cannot be listed.

## API v1

A versioned layer over the same commands: every change it makes is one of the commands of
`POST /api/cmd`, checked and done by the same code, so the two cannot disagree. What it adds:
a **path** for every value, changes relative to the value now, fades, and several commands in one
request. Its answers, errors included, are always JSON. The checks of [What it checks](#what-it-checks)
apply unchanged: a `POST` needs `X-BananaSplit: 1`.

**Paths**

| Path | Kind |
|---|---|
| `/strips/{1-6}/gain_db` | Fader, -60 to +12 dB, to 0.1 |
| `/strips/{1-6}/trim_db` | Input gain (GAIN knob), -24 to +24 dB, to 0.1 |
| `/strips/{1-6}/pan` | -1 to 1, to 0.01 |
| `/strips/{1-6}/mute`, `mono`, `solo`, `record`, `eq`, `gate`, `comp` | on/off (`record`: linked to the recorder) |
| `/strips/{1-6}/sends/{bus}` | on/off: the strip feeds the bus |
| `/buses/{bus}/gain_db`, `trim_db`, `pan` | as for strips (`pan` is the balance) |
| `/buses/{bus}/limit_db` | The limiter, -40 to +12 dBFS, to 0.1 (+12 = off) |
| `/buses/{bus}/mute`, `mono`, `record`, `eq` | on/off |
| `/buses/{bus}/sends/{bus}` | on/off: the bus feeds another bus (not itself, no loops) |
| `/recorder/recording` | on/off: recording now |

`{bus}` is `V1`, `V2`, `V3`, `B1` or `B2` (the old `A1` to `A3` are read too, in any case). Values
given outside their range are clamped and rounded as `/api/cmd` does.

**`GET /api/v1/state`.** The [state](#the-state) with two fields in front: `api` (1) and `rev`, a
number that goes up with every new state, so a client can tell whether anything changed since it last
looked.

**`GET /api/v1/paths`.** `{"api": 1, "paths": [...]}`, each `{"path", "label", "kind", "value"}` and,
for a number, `min`, `max`, `step` and `unit`. `kind` is `number` or `flag`; `value` is the value now
(`true`/`false` for a flag). `label` names the channel as the page does (`Strip 1 (Mic): fader`,
`B1: to V1`).

**`POST /api/v1/command`.** The body is one command object, or a list of 1 to 64 of them:

| `cmd` | Fields | Does |
|---|---|---|
| `set` | `path`, `value` | Set it. A number for a number; `true`/`false` (or `1`/`0`) for a flag. |
| `toggle` | `path` | Flip a flag. |
| `step` | `path`, `by` | Add `by` (may be negative) to a number. |
| `fade` | `path`, `to`, `ms` (0 to 60000) | Move a number to `to` over `ms` milliseconds. A gain moves evenly along the fader, as a hand would. A `set` or `step` on the same path, another fade on it, or a scene stops it. |
| `recall` | `scene` | Load a saved scene (the name as in `op=scene`). |

One command answers 200 with `{"ok": true, "text": "<what the page shows>", "path": ..., "value": ...}`
(`path` and `value`: the value after the change; `recall` has neither), or its error, as below.

A list is checked whole first: if any command in it is wrong (an unknown path, a missing field) the
answer is 400, `{"error": "command 3: ..."}`, and nothing is done. Then the commands run in order, and
the answer is 200 with `{"ok": <all worked>, "results": [...]}`, one result per command, each like the
answer to a single command or `{"ok": false, "error": "..."}` for one the mixer refused (a bus send
that would loop, a scene that is gone).

**`POST /api/v1/scenes/{name}/recall`** is `recall` with that name (percent-encoded in the path; `+`
is a plus). **`POST /api/v1/recorder/start`** and **`/stop`** set `/recorder/recording`. Both need no body.

```sh
P=8790; H='X-BananaSplit: 1'; J='Content-Type: application/json'
curl -s localhost:$P/api/v1/paths
curl -s -H "$H" -H "$J" --data '{"cmd":"set","path":"/strips/1/gain_db","value":-6}' localhost:$P/api/v1/command
    # {"ok":true,"text":"-6.0 dB","path":"/strips/1/gain_db","value":-6.0}
curl -s -H "$H" -H "$J" --data '{"cmd":"toggle","path":"/buses/B1/mute"}' localhost:$P/api/v1/command
curl -s -H "$H" -H "$J" --data '{"cmd":"step","path":"/strips/2/gain_db","by":3}' localhost:$P/api/v1/command
curl -s -H "$H" -H "$J" --data '[{"cmd":"fade","path":"/buses/V1/gain_db","to":-20,"ms":500},
  {"cmd":"set","path":"/strips/3/sends/B1","value":true}]' localhost:$P/api/v1/command
curl -s -H "$H" -X POST localhost:$P/api/v1/scenes/Live/recall
curl -s -H "$H" -X POST localhost:$P/api/v1/recorder/start
```

## Errors

A request that cannot be done is answered with a 4xx or 5xx status and its message:

* **As plain text** (`Content-Type: text/plain; charset=utf-8`), the message alone, for a client that
  does not ask for anything. This is what curl and scripts get.
* ***New:* as JSON** (`application/json`), `{"error":"<message>"}`, for a client that names it in its
  `Accept` header (`Accept: application/json`; `*/*` does not count). The page and the touch deck do,
  and everything under `/api/v1/` always answers this way.
  A refusal that comes before the request has been read (the other end is another user, a request
  that cannot be read) is always plain text.

| Status | When |
|---|---|
| 400 | A command that is wrong or cannot be done (the message says why), a body that is not UTF-8, a request that cannot be read. |
| 403 | Another user, a wrong `Host`, a request from another site or without `X-BananaSplit: 1`. |
| 404 | An unknown path, a recording that is not there, no updater. |
| 405 | A method other than `GET` or `POST`. |
| 408, 411, 413, 431 | A request that did not arrive in time, has no `Content-Length`, is too large, or has too large a header. |
| 503 | Too many connections or event streams, the main loop is busy, or the system's devices cannot be listed. |

## Examples

```sh
P=8790; H='X-BananaSplit: 1'

curl -s localhost:$P/api/state | head -c 300                       # the state, once
curl -sN localhost:$P/api/events                                   # the state as it changes

curl -s -H "$H" --data 'op=gain&ch=0&v=-6' localhost:$P/api/cmd    # strip 1 to -6 dB   -> -6.0 dB
curl -s -H "$H" --data 'op=mute&ch=3&v=1'  localhost:$P/api/cmd    # strip 4 muted      -> ok
curl -s -H "$H" --data 'op=route&ch=6&bus=3&v=1' localhost:$P/api/cmd   # V1 sends to B1
curl -s -H "$H" --data 'op=link&ch=0&v=1' localhost:$P/api/cmd && \
curl -s -H "$H" --data 'op=rec&v=1' localhost:$P/api/cmd           # record strip 1
curl -s -H "$H" --data 'op=scene&name=Live&act=load' localhost:$P/api/cmd

curl -s -H "$H" --data 'op=gain&ch=0&v=loud' localhost:$P/api/cmd                       # gain must be a number
curl -s -H "$H" -H 'Accept: application/json' --data 'op=gain&ch=0&v=loud' localhost:$P/api/cmd
                                                                  # {"error":"gain must be a number"}
```

In JavaScript (the page does exactly this):

```js
const r = await fetch("/api/cmd", {
  method: "POST",
  headers: { "X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded", "Accept": "application/json" },
  body: new URLSearchParams({ op: "gain", ch: "0", v: "-6" }).toString(),
});
const text = r.ok ? await r.text() : (await r.json()).error;
```
