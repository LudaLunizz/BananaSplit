# Changelog

All notable changes to BananaSplit, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions are the
app releases (`linux/`, `windows/` and `macos/` share one version number).

The text of each release is the "What's new" note of that release, kept in
[`linux/README.md`](linux/README.md); a release without a date is one whose
tag could not be found. Each release's files, checksums and signature are on
the [releases page](https://github.com/LudaLunizz/BananaSplit/releases).

## [2.9.8] - 2026-11-19

* **Black hole for the Dark switch** (#199): MENU ▸ Mode ▸ Dark (and Auto when the system is dark) now plays a black hole, the counterpart of the flashbang: it opens in the middle of the page, the page spins into it, the hole swells over the window while the page turns dark, then it shrinks away as the dark page comes back out. It is our own animation (nothing new in `NOTICE`), and with reduced motion on the page switches at once. A click on Mode while an animation is still switching the page now ends on your last choice (before, Dark clicked while the grenade rolled ended on Light), and clicking Dark while the white flash is still fading cuts the flash short and plays the hole. Nothing else changes in how the mixer sounds or behaves.

## [2.9.7] - 2026-11-18

* **Recording apps keep their bus** (#195, Linux): the counterpart of `app-strips` for apps that record. A new file `app-buses` next to the config holds one rule per line: `<bus> <app>`, or `<bus> <app> @ <what the stream asks for>` when a program has two streams, for example `B2 vesktop @ vencord-screen-share` (Vesktop's screen share goes to B2 and its microphone stays on B1). BananaSplit moves a matching stream to that bus's virtual microphone when it first appears, at the start and when the app opens a new one. Ticking a recording app in a bus's menu writes the rule, unticking removes it, and you can edit the file while BananaSplit runs. A stream you move yourself afterwards is left alone. Nothing else changes in how the mixer sounds or behaves.

## [2.9.6] - 2026-11-18

* **Full credit for the flashbang** (#191): the licence files (`NOTICE` and `LICENSE-AGPL-3.0`) now come in every package: the `.deb` (with a copyright entry), the Linux `.tar.gz`, the Windows `.zip`, the macOS app and the Flatpak. The macOS app also carries the GPL `LICENSE`. The port is dated (18 November 2026).
* **Source and credits** (#191): MENU ▸ **Source and credits…** opens the repository's `NOTICE` in your browser: where the source is, and whose work is in BananaSplit (Silvertree2010's flashbang, AGPL-3.0). New API op `source` (needs the token, like `deck`). The mixer sounds and behaves exactly as before.

## [2.9.5] - 2026-11-18

* **Flashbang for the Light switch** (#187): MENU ▸ Mode ▸ Light (and Auto when the system is light) now plays a flashbang: a pixel grenade rolls in, the page turns light when it goes off, then the white flash fades. Going dark has no animation, and with reduced motion on the page switches at once. The animation is [Silvertree2010](https://github.com/Silvertree2010)'s, from [ApolloOffice](https://github.com/Silvertree2010/ApolloOffice), used under its AGPL-3.0 licence: see `NOTICE`, the README and `LICENSE-AGPL-3.0`.
* **Project files**: `SECURITY.md` (how to report a vulnerability, privately), and a manual-only workflow, "Windows exe", that builds a test copy of the Windows program from any branch (#185).

## [2.9.4] - 2026-11-16

* **Beta, not pre-alpha** (#174): the README and the website now say beta and explain what that means.
* **Errors that say what went wrong** (#175): loading or saving the settings, saving, loading or deleting a scene, and checking a downloaded release now report the kind of failure (a file problem, a bad line, a bad scene name, a wrong signature or version) instead of only a sentence. The messages you see are the same as before.
* **One copy of two helpers** (#176): the ramp and the lock helper that were copied into several modules each exist once. The mixer sounds and behaves exactly as before.
* **Docs in the code** (#177): every public item of the shared core has a doc comment, and a lint keeps it that way.
* **Project files**: `CHANGELOG.md`, `ARCHITECTURE.md` (a map of the code) and `CONTRIBUTING.md` (setup and the exact test and lint commands).

## [2.9.3] - 2026-11-15

* **Recording stops before the disk is full** (#164): with less than 1 GB free on the recordings drive, a running take is finished cleanly (every file plays) and recording stops; below 1 GB a new take does not start. The recorder panel says why (`disk.low`, `rec.error`).
* **A bug in a mixing cycle no longer stops BananaSplit** (#163): that cycle (a few milliseconds) plays silence and mixing goes on, on Linux, Windows and macOS. Counted as `panics` in the state, red in the terminal view.
* **API v1 reaches more** (#165): paths for each EQ band (on, type, frequency, gain, Q), every gate and compressor setting, VBAN (on/off, port, each stream) and the player (play, pause, stop, seek, take, sends). MIDI controls can drive the numbers and switches among them. `/api/cmd` keeps working for the page and the deck but gets no new ops.
* **A token for what reaches beyond the mixer** (#166): quitting, updates, starting with the system, the app menu entry, opening folders, recording (and its folder) and VBAN out now need a token BananaSplit writes at each start to a file only you can read (`$XDG_RUNTIME_DIR/bananasplit/BananaSplit.token`; Windows `%LOCALAPPDATA%`, macOS `~/Library/Caches`). The window, the touch deck opened from the menu (new: MENU ▸ **Open the touch deck**) and `--quit` get it by themselves; mixer controls (faders, mutes, routing, EQ, scenes, player) work without it, so the deck and Companion keep working. Scripts that quit or record send `X-BananaSplit-Token` (see `docs/api.md`).
* **Docs** (#162): why BananaSplit next to other mixers, what runs where on each OS, what pre-alpha means, a MIDI guide (`docs/midi.md`), Windows latency, and stale statements fixed.

## [2.9.2] - 2026-11-15

* **No crash at start right after boot** (#158): started within a few seconds of booting (autostart on a fast machine), BananaSplit could stop with a panic; its timers no longer reach back before the system clock began.
* **Windows: the Disk line is right again** (#158): it showed the free space as the disk size, so it always read about 100% free; it now shows the disk size and your free share.
* **Windows: sturdier audio threads** (#158): an audio stream in an unexpected state is rebuilt like an unplugged device instead of stopping the mixer.
* Behind the scenes: CI now checks the oldest Rust the source promises (1.88), lint exceptions say why, more code documented.

## [2.9.1] - 2026-11-15

* **Themes** (#152): MENU ▸ **Theme** picks the whole look: **Classic** (the old look, still the default) or the new **Console** (neutral greys, a flat console, a dB scale on the faders). Dark / Light / Auto is now called **Mode** and works with both. API: `op=look`.
* **MIDI fixes** (#154): a motor fader, pitch bend, 14-bit CC or NRPN bound as a button or to a scene now works (past halfway = pressed); 14-bit faders no longer overshoot when their coarse step changes; LEDs and motor faders catch up after a send error (the port is opened again); on Linux a controller unplugged and plugged back in quickly is connected again; messages lost because the controller sent too fast are shown in the MIDI window (state `midi.lost`).

## [2.9.0] - 2026-11-14

* **MIDI controllers** (#144): MENU ▸ **MIDI controllers…** — choose your controller's ports, click **Learn a control**, move a fader, knob or button and pick what it drives (any fader, gain, pan, mute, solo, route, EQ, gate, limiter, record arm, recording, or loading a scene). Absolute faders and knobs, endless encoders (three relative kinds), 14-bit CCs, pitch-bend faders, NRPN, buttons that switch or work while held. **Pickup**: a fader that doesn't match the value does nothing until it reaches it, so nothing jumps. **LEDs and motor faders follow** every change, from the window, the deck, the API or a scene. Plug the controller in later and it is opened within seconds. Linux (ALSA sequencer, no new library), Windows (WinMM), macOS (CoreMIDI). Kept in `midi.json` next to the settings.
* **API v1** (#143): one documented command model for scripts, decks and the MIDI bindings. Every value has a path (`/strips/1/gain_db`, `/buses/B1/mute`, `/strips/2/sends/V1`, `/recorder/recording`); `POST /api/v1/command` sets, toggles, steps or fades it, one command or a batch of up to 64. `GET /api/v1/state` (with `rev`), `GET /api/v1/paths`, scene recall and recorder start/stop shortcuts. See `docs/api.md`. `/api/cmd` keeps working.

## [2.8.27] - 2026-11-14

* **A limiter on every bus** (#133): a brickwall limiter is the last stage of each bus, so nothing leaves it (device, cable, sends, recorder, VBAN) louder than its limit, even with the fader higher. Set it with the yellow-edged **LIM box on the bus meter** (like Voicemeeter's Lim), dragged up and down; a thin yellow line shows the same level on the fader, and both brighten while it limits. Top of the meter = off (bit for bit as before). Saved in the config; scenes leave it alone. API: `op=limit`, state `limit` and `limGr`.
* **Ctrl + drag moves a whole group** (#133, #135): hold Ctrl while dragging a LIM box, a fader or a GAIN knob and all buses (or all strips) jump to that level and follow it. Ctrl also works with the arrow keys and double-click.
* **Borders that say what a channel does** (#137, #140): **blue** on the buses that play to the output you listen on (BananaSplit asks once; MENU ▸ Listening on…), **red** on channels being recorded, **yellow** on buses an app (Discord, OBS) takes sound from (Linux), next to the green of the selected channel; several stack as rings. API: `op=listen`, state `app.listen`, `apps`.

## [2.8.26] - 2026-11-13

* **A refused page says why** (#128): when BananaSplit can't read the system's list of connections, the page and the API stay closed (as in 2.8.25), but the window now names the list, says why it is closed and that the mixer keeps running, instead of a bare refusal. All three systems.
* **Bus cards: keys for the other buses** (#129): with a bus card selected, keys 1–6 switch the inputs into it and **7 8 9 0** the other four buses (bus-to-bus sends), in the order of its FROM row. A send that would make a loop is refused, as with a click.
* **wry 0.57.2** on Windows and macOS (the window library, Dependabot #119, #120).

## [2.8.25] - 2026-11-13

* **Routing: V1–V3 in the middle, B1 = mic out, B2 = system out** (#123): buses A1–A3 are now called **V1–V3**, the mixes between the inputs and the outputs. B1 is the mic out (the microphone Discord and OBS pick), B2 the system out (what you hear): a new setup plays B2 on the default output. Something you want to hear but not stream goes to B2 and not to B1. Old settings, scenes, recordings and API calls with `A1`–`A3` still work, and the device names other apps remember (`bananasplit_out_a1`, the JACK ports, Splitter's endpoints) don't change. A 2.8.25 settings file can't be read by 2.8.24.
* **FROM keys on every bus card** (#125): the bottom of a bus card lists everything that can feed it, inputs 1–6 and the other buses. Lit: it goes straight in. Dashed: it only arrives through another bus. A white ring with a glow: its sound is coming out of this bus right now. Click a key to route that channel straight into the bus. `/api/state` has the same as `from` and `via` per bus (`docs/api.md`).
* **Updater: a backup key and the version inside the signature** (#121): a release signed with BananaSplit's offline backup key is accepted too, so the release key can be replaced if it is ever lost or leaked. A release is installed only when its signed `SHA256SUMS.txt` names that version.
* **The page refuses unknown users** (#117): when BananaSplit can't read the system's list of connections, the page and the API now refuse the connection instead of letting it in (all three systems).
* **Build pipeline** (#115): GitHub Actions are pinned to fixed versions, and every build checks the dependencies for known security problems (`cargo deny`); `time` was updated for RUSTSEC-2026-0009 (Windows and macOS builds).

## [2.8.24] - 2026-11-13

* **Updater fix** (#112): a signature file (`SHA256SUMS.txt.sig`) with a non-ASCII character in a certain place made the update check stop with an error instead of refusing the file. It is now refused like any other bad signature. No update was ever installed from such a file: the check stopped before installing anything. All three systems.
* **Windows: channel menus** (#104): unticking a Splitter cable on a bus (A1–A3, B1, B2) works again; ticking or unticking a device no longer changes `@default` or `@physical`; the menus read the devices once instead of many times; the two halves of a Splitter cable must be on the same Splitter device, also in the cables report and the feedback warnings.
* **macOS** (#108, #110): `bananasplit --status` shows every device and the default output and input without starting any audio or asking for a permission. If the audio code fails on a device, or a device's buffers change shape while it runs, that device plays silence and BananaSplit reconnects, instead of the app stopping or writing to the wrong channels.

## [2.8.23] - 2026-11-09

* **New licence: GPL-3.0-or-later** (#100): BananaSplit is now free software under the GNU General Public License, version 3 or later. You can still use, share and change it freely; anyone who shares a changed version must share its source code under the same licence. Releases up to 2.8.22 stay under MIT.
* **A documented local API** (#97): every command and every field of `/api/state` is described in [`docs/api.md`](../docs/api.md), and the tests check the real server against it. `/api/state` now also says which system it runs on (`app.platform`). An error is plain text; a client that sends `Accept: application/json` gets `{"error": "…"}` instead.
* **One window page for all three systems** (#87–#95): Linux, Windows and macOS now share one page, server and command code instead of three copies. Nothing changes in use.
* **Fixes from the 2.8.21 review** (#81, #83):
  * the gate no longer gets stuck on tiny leftover values in digital silence, which can cost CPU;
  * Windows and macOS: starting BananaSplit, `--quit` and `--update` now check that the BananaSplit already running belongs to you, as Linux has done since 2.7.3, so another account on the same computer can't answer in its place;
  * Windows: the updater checks the downloaded file again right before it runs it or puts it in place;
  * Windows: restoring an app to its strip no longer misses a device whose ID only differs in upper/lower case;
  * a window process that crashed is cleaned up instead of staying behind, and long recordings no longer copy their whole file list 30 times a second.

## [2.8.22] - 2026-11-08

* **Other web pages can no longer lock the window out** (#75): every `/api/` request from another site, or from a page on another localhost port, is now turned away, not only button presses. Before, any open web page could hold all 8 live-update streams and the mixer window showed "Lost connection". curl and `--quit` are not affected.
* **The take list stays fast with thousands of recordings** (#74): the player's take list rescanned the whole recordings folder every 3 s, and that grew quadratically (about 2 s of CPU per rescan at 12,000 files). It now skips the scan when nothing changed (under 1 ms) and a full scan takes about 33 ms.
* **No more short bursts of noise from a huge sample** (#67): a very loud but valid sample that a strip EQ pushed past the float limit could put about 0.7 ms of garbage on the buses and out of your speakers. The buses are now checked before they leave the mixer.
* **Lighter EQ and compressor** (#68, #66): the EQ needs less CPU and the compressor skips work it doesn't need, with exactly the same sound as before.
* **One shared mixer core** (#69): the mixer, EQ, dynamics, recorder, player, MP3, VBAN, scenes, config and terminal view now live once in `core/`, shared by the Linux, Windows and macOS apps. Nothing changes in use.

## [2.8.21] - 2026-11-07

* **Windows: apps keep their strip** (#63): an app you put on a strip's cable in Windows' Volume mixer is noted in `app-strips` next to the config (the same file as on Linux, e.g. `2 Discord`). Windows forgets that choice when an update installs the app into a new folder (Discord does) or the cable driver is reinstalled; BananaSplit then puts the app back on its strip's cable once and says so in the status line. Linux unchanged.
* **macOS:** the menu-bar library is updated (tray-icon 0.24, #54).

## [2.8.20] - 2026-11-05

* **A touch deck for a small screen** (#59): open `http://127.0.0.1:8790/deck` on a 7" touchscreen (1024×600 or 1280×600) or a phone held sideways. One column per strip with a big fader, level meter, MUTE, SOLO and the A1 A2 A3 / B1 B2 route buttons; a Buses tab (`/deck#buses`) with the bus faders, mutes and which strips feed each bus; a REC button. Faders move by dragging (a tap never jumps the volume) and a double tap puts one at 0 dB. It changes the same mixer as the window and answers only your own user, like the main page.

## [2.8.19] - 2026-11-02

* **macOS follow-ups** (#52), Linux and Windows unchanged:
  * a device left at a buffer size above 8192 frames now plays its whole cycle (only the first 8192 frames played before);
  * the app is signed with the audio-input entitlement, ready for a Developer ID signature later;
  * the macOS README says how to get the microphone back when an update leaves every input silent (`tccutil reset Microphone io.github.ludalunizz.bananasplit`);
  * Dependabot now also watches the macOS dependencies.
* **A slow first start no longer loses the window** (#55, Linux and Windows): BananaSplit now waits up to 60 s (was 15 s) for its window to start. On Windows a slow start is no longer blamed on a missing WebView2; that message only shows when WebView2 itself fails.
* **The mixer keeps running while its window starts** (#57, Linux and Windows): the levels, the state and the buttons in a browser no longer freeze during a slow window start.

## [2.8.18] - 2026-11-02

* **The logo shows the normal arrow cursor** (#50). Hovering "BananaSplit." in the header showed the text cursor; the logo is no longer selectable text.

## [2.8.17] - 2026-11-01

* **Fix: the gate and compressor can no longer turn a broken sample into invalid audio** (#46). One absurdly loud sample from a misbehaving app, with the strip's input gain above 0 dB, made a whole audio cycle invalid (NaN) on every bus the strip feeds. The level they read is now capped, and a test covers it.

## [2.8.16] - 2026-10-31

* **A gate and a compressor on every input strip** (#40). The gate mutes a strip while it is quieter than its threshold (the hiss and room noise of a microphone between words); the compressor brings loud and quiet parts closer together, with make-up gain. Both sit before the EQ, read the level after the GAIN knob, and are off by default: off, the strip is unchanged bit for bit. Open them with the new **DYN** button next to EQ on a strip (or **D**); the terminal UI switches them with `g` and `C`. They are saved in the config and in scenes. See [Gate and compressor](#gate-and-compressor).

## [2.8.15] - 2026-10-29

* **macOS: other users on the Mac can't control your mixer.** BananaSplit now checks who is on the other end of a connection by asking the macOS kernel's list of TCP sockets, as it already did on Linux and Windows (#36). On macOS, a device switched to another sample format while it plays is now opened again, instead of making noise until a restart. Nothing changes on Linux and Windows.

## [2.8.14] - 2026-10-28

* **BananaSplit for macOS.** A new app for Apple silicon and Intel Macs (macOS 11 or newer): the same mixer on Core Audio, with BlackHole as virtual cables (#34). It's signed ad hoc, not with an Apple Developer ID: open it the first time with Open Anyway (see [the macOS manual](../macos/README.md#installing)). Nothing changes on Linux and Windows.

## [2.8.13] - 2026-10-28

* **Cleaner code under the hood.** The last part of the Rust code review (#28): every unsafe block says why it is sound, CI now fails on any clippy warning, and the calls into C and Windows libraries are declared in one module per platform instead of five. Nothing changes in how BananaSplit works.

## [2.8.12] - 2026-10-28

* **Safer building blocks.** The second part of the Rust code review (#26): JACK ports can no longer be passed around as bare pointers, the compiler now checks what the audio threads share, tray texts are checked before GLib reads them (a bad one is logged instead of crashing the window), and on Windows COM reports when it can't start, its handles can't be closed twice, and a failed audio thread stops the others instead of leaving devices open.

## [2.8.11] - 2026-10-28

* **Code review fixes.** A review of the Rust code against the current Rust and Linux state found no memory bugs that can happen today, and fixed a handful of small ones that could later: the tray and dark-style checks free their errors and check D-Bus replies, recording passes less data between threads, Windows audio threads release their devices before ending COM, and the network list and autostart path are read safely. Needs Rust 1.88 or newer to build (#24).

## [2.8.10] - 2026-10-28

* **A quieter silence warning.** When a take records only silence, the recorder's label still turns orange and says **Recording silence**, but the long notice below the recorder is gone (#22).

## [2.8.9] - 2026-10-28

* **Every app keeps its strip, also apps that don't choose a device themselves.** Since 2.8.7 BananaSplit put an app back on its strip after a restart, but it only learned that strip from apps that choose their own output device (Discord does). An app like Strawberry plays wherever it is put and says nothing about it, so ticking it in a strip's menu was forgotten at the next restart. Now ticking an app in a strip's menu writes it into `app-strips` at once, and moving it back to a device of the computer in the menu takes it out.
* **`app-strips` is yours to edit.** It sits next to the config: one `<strip> <app>` per line, for example `5 strawberry` (the name is the program's, as the strip's menu shows it; case doesn't matter). A change applies while BananaSplit runs: the apps that play now go to their strip at once.

Nothing to change on your side. Apps you put on a strip before stay remembered.

## [2.8.8] - 2026-10-28

* **A folder per day for your recordings.** New takes go into a folder named after the day, inside the recordings folder: `BananaSplit/2026-10-28/2026-10-28_14-03-05_mix.mp3`. A take that runs past midnight continues with its next part (the next hourly file) in the next day's folder, so every day's folder holds the files that started on that day. The player lists the takes of every day folder, and a take whose parts are in two folders is still one take that plays straight through.

Nothing to change on your side. Recordings you already have stay where they are and are still listed.

## [2.8.7] - 2026-10-27

* **Apps go back to their strip when BananaSplit starts again.** Quitting removes the virtual cables, and an app that opens a new stream when its cable comes back (Discord does) used to land on the default output, often another strip's cable: after an update or a restart, Discord played into the Desktop strip, and a recording of the Discord strip's bus stayed silent. BananaSplit now remembers which strip you put an app on (in a channel's menu, in the sound settings or in the app's own device setting) in `app-strips` next to the config, and puts it back there once when it sees the app again, and says so in the status line. An app you put somewhere else on purpose (untick it in the menu) is forgotten, and an app you move afterwards is left where you put it.
* **The recorder warns when a take records silence.** After 10 seconds in which the armed tracks got no sound (nothing above −80 dBFS reached the file), the recorder panel says "Recording silence" (since 2.8.10 only in its orange label, without the notice below).
* **Clearer recorder labels:** PLAY TO is now **Play back on** and ARMED TRACKS is **Armed tracks · recorded**, with tooltips: Play back on only chooses where recordings are played, never what is recorded.
* **The open window needs much less CPU.** The time code and the levels change on every frame; each change made WebKit lay out the whole console again. They now sit in boxes of a fixed size, the level bars move without a layout, and names, tooltips and panels follow 10 times a second (meters, knobs and faders 30 times). Measured on a recording mixer: about a third of one core instead of nearly all of it. A window in the tray needs almost none, as before.

Nothing to change on your side. `app-strips` is a file of its own, so older versions still read the config.

## [2.8.5] - 2026-10-27

* **BananaSplit as a Flatpak.** Every release now also has `bananasplit-<version>-x86_64.flatpak`, for any distribution with Flatpak: `flatpak install --user bananasplit-<version>-x86_64.flatpak` (it fetches the GNOME runtime from Flathub). It is the same mixer, with its virtual cables, tray icon, VBAN and recorder; see [Install](#install) for what the Flatpak may reach and what works differently in it.

Nothing changes for the .deb and the portable package.

## [2.8.4] - 2026-10-27

* **Recordings are MP3 now.** Takes are recorded as MP3 at 320 kbit/s: about 144 MB per hour and track, a seventh of 24-bit WAV, and they play everywhere. **FORMAT** in the recorder panel has MP3 next to 16-bit, 24-bit and 32-bit float WAV, which are still there for lossless takes; `bitrate =` under `[recorder]` sets 128 to 320 kbit/s. BananaSplit encodes the MP3 itself, on Linux and on Windows: nothing else to install.
* **At their own speed, to the sample.** An MP3 has the sample rate the audio server runs at, so it plays at 1.0x. MP3 holds 32, 44.1 and 48 kHz: when the server runs at another rate (96 kHz, say), the take is converted to 48 kHz, and the recorder panel says so. Every file starts with an Info frame whose LAME tag gives its exact length, so players that read it (mpg123, GStreamer, ffmpeg and the programs built on them, and BananaSplit's own player) play exactly the samples that were recorded, without the short silence an MP3 otherwise starts with.
* **Hourly parts that join without a seam.** A long take still continues in `_part2.mp3`, `_part3.mp3` … every hour, at the same sample on every track. Each part also holds the end of the part before it and the start of the next one, hidden by its LAME tag, so the moment where two parts meet is encoded like any other: played one after the other, the parts give the whole take back, without a gap, a repeated sample or a click.
* **Every track is written by a thread of its own.** Encoding up to 11 tracks, finishing a part and waiting for the disk to store it no longer hold up the recorder. Before, a disk that took longer than the 4-second buffer to finish the files at an hourly split left a gap of silence at the start of the next part.
* **The player plays MP3:** BananaSplit's takes and any other MP3 in the recordings folder (MPEG-1, 2 and 2.5, constant or variable bitrate, with ID3 or APE tags), with their waveform, from any point. A WAV file and an MP3 file of the same name are two takes.

Configs saved by 2.8.3 and older say `format = wav24`, the default they were written with: those record MP3 now. A 16-bit or 32-bit float choice stays; 24-bit WAV is kept once you choose it again. Configs saved by 2.8.4 have a `bitrate =` line, and may say `format = mp3`, which 2.8.3 and older refuse; delete the line (and pick a WAV format) to go back.

## [2.8.3] - 2026-10-26

* **Windows: BananaSplit's own virtual cables.** BananaSplit for Windows now works with **Splitter**, its own cable driver (installed separately), which gives every channel a cable of its own, as Linux has always had: apps play into `Splitter 1` … `Splitter 6` to reach strips 1 to 6, and record `Splitter A1` … `Splitter B2` to hear buses A1 to B2 (once the bus's cable is on in its menu; B1 and B2 have theirs on to begin with). Switching one channel's cable off no longer moves another channel's. VB-Audio's cables still work, for any channel whose Splitter cable is missing; with Splitter installed, the other channels move to Splitter's cables, so apps you pointed at "CABLE Input" or "CABLE Output" need pointing at the Splitter cable of that channel. **Menu ▸ Virtual cables…** shows both. For now Splitter is a test build: Windows loads it only in test-signing mode. See the [Windows manual](../windows/README.md#virtual-cables).
* **Windows:** the Quit item and the "BananaSplit has stopped" message no longer say that quitting removes the virtual cables; on Windows they stay installed and fall silent.

Nothing changes on Linux. On Windows without Splitter, nothing changes either.

## [2.8.2] - 2026-10-26

* **Long recordings are split every hour.** A take now continues in `_part2.wav`, `_part3.wav` … every 60 minutes, at the same sample for every track, so a long session is a set of files you can open and send one at a time. Change it with `split =` under `[recorder]` (minutes, up to a day; `0` or `off` splits only before 4 GB, as before).
* **Resizing the window is smooth again.** On Linux, BananaSplit switched WebKitGTK's GPU renderer off everywhere, which made the window lag while it was resized; it now does that only with NVIDIA's own driver, where the window would otherwise stay empty. Channel names are no longer measured again on every resize step.
* **Windows: a setup window for virtual cables.** **Menu ▸ Virtual cables…** explains what the optional VB-Audio cables are for, how to install them, that they leave Voicemeeter alone, and that Windows may make a new cable its default device. It shows which cables are installed, whether Voicemeeter is, and the Windows defaults, and opens by itself on the first start without cables (instead of a message box that could hide behind the window). See the [Windows manual](../windows/README.md#virtual-cables).

Nothing to change on your side. Configs saved by 2.8.2 have a `split =` line, which 2.8.1 and older refuse; delete that line to go back.

## [2.8.1] - 2026-10-26

Fixes to the updater from a review of 2.8.0. Nothing to change on your side.

* **Automatic updates wait two days.** With **Install updates automatically** on, a new release is installed by itself once it has been out for two days, so a broken release can be withdrawn before it reaches you. **Check now** and **Install** still take it at once.
* **Quitting during an update on Windows** could leave BananaSplit without its `bananasplit.exe` when checking the new version took long (a virus scanner). Quitting now waits for the update to finish or stops it before it changes anything.
* **`bananasplit --update` and the app download to folders of their own**, so running `--update` while the app is downloading no longer deletes the app's download.
* **Downloads through wget refuse a redirect to plain HTTP**, as downloads through curl already did.
* **A release link with an unusual character** no longer stops the check with an internal error.
* **Releases are signed.** BananaSplit now installs a release only when its `SHA256SUMS.txt` carries the signature of BananaSplit's release key (`SHA256SUMS.txt.sig`). That key is never on GitHub, so someone who got into the GitHub account could still publish a release but not one BananaSplit installs. Matching checksums alone only showed that a download was not damaged.
* **The .deb is checked again as root.** After you give your password, the package is copied to a folder only root can write, its SHA-256 is checked once more, and only then is it installed; apt-get also never removes another package to install it.
* **An updated program keeps its permissions** (for example a copy only you can run stays that way).
* **The Updates window could stay on "installing"** when a check finished just before Install started. It now always ends with what happened.

## [2.8.0] - 2026-10-25

* **BananaSplit can update itself.** **Menu ▸ Check for updates…** opens the Updates window: **Check now** looks for a new version on BananaSplit's GitHub releases, shows what is new in it, and **Install** downloads it, checks it and puts it in the place of the running program. The mixer keeps running on the old version; **Restart now** (or your next start) runs the new one, with your settings. See [Updates](#updates).
* **Automatic updates are off by default.** Switched off, BananaSplit never contacts anything by itself: it looks for a new version only when you click Check now. Switch **Install updates automatically** on in the Updates window and it checks a minute after it starts and once a day, installs a new version by itself and tells you when a restart finishes the update. It never restarts on its own, so your audio is never cut.
* `bananasplit --update` does the same from a terminal.

Nothing to change on your side: configs, scenes and recordings load as they are, and 2.7.3 can still read a config saved by 2.8.0 while automatic updates are off.

## [2.7.3] - 2026-10-25

Fixes from two reviews of 2.7.1. Nothing to change on your side: configs, scenes and recordings load as they are. (There is no 2.7.2.)

* **The recordings list keeps working with any file name.** A folder or file in the recordings folder whose name ends in certain non-Latin or accented letters (`録音`, `Größe`) stopped the list for the rest of the session: new takes never showed up until BananaSplit was restarted.
* **A take played while it was being recorded plays to its end.** Play after the recording stopped played only what was there when you first selected it, and a click past that point on the waveform stopped playback.
* **Takes are listed in the order you recorded them**: also the tenth take in one second (it came before the second), and on the night summer time ends, when the clock shows the same hour twice.
* **A channel called "Guitar part2"** is recorded as `1-Guitar-part2`, so a take longer than 4 GB still lists it as one track (it became two, played on top of each other).
* **A mix take and a multi-track take started in the same second** are two takes (they were listed, and played, as one).
* **Switching off a VBAN input that is playing fades it out** instead of stopping it dead with a click; so does renaming it, or the sender restarting.
* **Two computers sending the same VBAN stream name** (Voicemeeter's default `Stream1`, say) to an input that takes any sender: the first one plays on, instead of both giving silence. The second one is listed under the streams BananaSplit sees, so you can give it an input of its own; it takes over when the first one stops.
* **VBAN senders that restart, or that do not number their packets**, are heard again at once (a restart with lower numbers was silent until the numbers caught up; unnumbered packets were never heard after the first).
* **"N dropouts" in a VBAN stream's status** counts dropouts since you switched the stream on, and no longer the sender's pauses.
* **The example config in this README loads.** Its comments after the values made BananaSplit refuse it (and became part of names and connect patterns). A comment after a value now gets an error that says where comments go: on a line of their own.
* **Saving keeps a config that is a link** (a dotfiles folder) a link, and keeps its permissions; scenes too.
* **Configs and scenes saved by an editor that adds a byte order mark** (Notepad) load, and such scenes are listed.
* **"Start with the system" starts the copy in your app menu** (`~/.local/bin`) when BananaSplit was added to it, not the copy you happened to run (from a download folder that may go away).
* **A window that found BananaSplit's page full** ("too many windows open") connects by itself once another window closes; it said "Trying again" and never did.
* **Another user of this computer can no longer pose as your BananaSplit.** `bananasplit --gui` and `--quit` talked to whatever answered on port 8790: another user's program there could make BananaSplit think it was already running, or get the quit command. They now only talk to a BananaSplit of your own user, as the page server already only answered its own user.

## [2.7.1] - 2026-10-25

Fourteen fixes from a review of 2.7. Nothing to change on your side: configs and scenes load as they are.

* **Your settings are saved when the audio server stops.** If PipeWire (or JACK) was restarted or crashed while BananaSplit ran, the app and the terminal UI closed without saving what you had changed since the last save. They now save first, and the message says "BananaSplit stopped" and that the settings are saved, instead of "BananaSplit could not start".
* **`@default` follows the desktop's default device at once.** When you pick another output or microphone in the sound settings, the channels set to follow it (A1 and strip 1 out of the box) move to it right away, and let go of the device they had, so you no longer hear (or record) both. `@physical` lines follow it the same way. A device that a channel also has its own line for stays. With the setup this README suggests (strip 3's cable as the default output), A1 now stays on the device it was playing on, instead of jumping to the first physical device; if that device is unplugged, A1 plays on another one meanwhile and goes back to it when it returns.
* **A crashed BananaSplit can be started again.** The helper that tells BananaSplit about changes in the sound settings (`pactl subscribe`) kept a copy of the connection to the JACK server; on JACK2 that could keep a crashed BananaSplit's name taken until you killed it by hand. It no longer does, and it is started again if pipewire-pulse restarts.
* **Apps are listed once in the channel menus**, under the apps that play or record. 2.7 listed them a second time under Programs (JACK), and ticking that one made the app play twice.
* **A headset's microphone reaches both sides of its strip.** For a headset whose mono mic and stereo output share a name (one JACK client), the strip's right side got what the headset plays (its monitor), which could feed back. It now gets the mic on both sides.
* **Virtual cables that someone else removes come back.** When pipewire-pulse restarts, or a cable's module is unloaded with `pactl unload-module`, BananaSplit makes the cable again at once and says so (if pipewire-pulse is not back yet, it keeps trying for half a minute); one that someone removes again within a minute stays off (switch it on in the channel's menu). In 2.7 the menu went on saying it was on, could not switch it off or on, and BananaSplit could later unload *another program's* module, because pipewire-pulse gives a removed module's number to the next one loaded. Cables are now always found by their names.
* **A long message no longer covers the strips.** When many connections failed at once (a laptop away from its dock), the status text grew the bottom bar over the strips' TO rows. It now keeps to three lines; point at it to read all of it.
* **Devices PipeWire numbers are found again.** PipeWire tells apart two JACK clients with the same name by adding a number (`…-63`), and that number changes when the device comes back or the computer restarts. A line the menu saved with the old number now still connects the device, as long as only one fits (one that another line names is left to that line); when several do (a microphone and what an output plays, say), the message names them, so you can tick the right one.
* **The menus mark the right device as default.** When PipeWire gave the number to the microphone instead of the output (`Yeti …-63` is the mic), the menu and `--list-ports` marked what the output plays as the default input. Strip 1 itself was always connected right.
* **Switching on Default input answers for that channel.** A problem with another channel's default device was shown instead of this channel's.
* **The channel button's tooltip shows what is connected now**, also right after a change in its menu (2.7 showed what was connected at the last automatic pass).
* **Renaming a bus** no longer says that its virtual microphone keeps the old name (a bus's cable is not named after it). Renaming a strip says how to give its cable the new name: switch it off and on in the menu.
* **A multi-track take that continues in new files** (because the server changed its sample rate, or a file reached 4 GB) shows its number of tracks and the files being written. 2.7 counted every file of the take as a track ("REC · 4 tracks" for two).
* **Two BananaSplits whose names differ only in capitals or dashes** (`BananaSplit` and `bananasplit`) would have cables with the same names. The second one took the first one's cables for leftovers of a crash and removed them. It now makes no cables and says why (give it a `client_name` that differs by more than that); `--cleanup` refuses in that case too.

## [2.7] - 2026-10-23

Changes from a review of 2.6:

* **Recordings play at normal speed (1.0x).** On PipeWire, BananaSplit could take the sample rate it had asked for (48 kHz) instead of the rate the audio server really ran at (44.1 kHz on many systems). The WAV files then said 48 kHz and played about 9 % too fast and too high. BananaSplit now asks the server for its rate once the audio runs and follows every change; the recorder, the player and VBAN all use it. If the server switches its rate during a take (an app that asks for 44.1 kHz can make it do that), the take goes on in a second file at the new rate (`…-2_<label>.wav`), and the recorder panel says why.
* **A menu on every channel.** The button under each channel's name says what the channel takes in or plays on; click it for everything the channel can use:
  * on a strip: microphones and sound cards, the **apps that play sound** (tick Discord, and it plays into this strip, as if you had picked the strip's cable as Discord's output in the sound settings), other JACK programs, what an output plays, the desktop's default input, and the strip's own **virtual cable**;
  * on a bus: speakers and headphones, the **apps that record** (tick OBS, and it records this bus), other JACK programs, the default output, and the bus's own **virtual microphone**.

  What you tick is connected at once and kept (as a `connect =` line, or the `cable =` switch), so it comes back after a restart and when a device is plugged in again; what you untick is disconnected. Each cable shows which apps use it, and the menus follow changes made elsewhere (the sound settings, qpwgraph, Helvum).
* **Inputs and outputs are interchangeable.** Any strip can take in devices, apps, its virtual cable, or any mix of them. Any bus, A1–A3 as well as B1–B2, can play on devices, be a virtual microphone, or both: A1–A3 get a virtual microphone (`BananaSplit-Out-A1` …) when you switch it on in their menu. It is off by default, so your sound settings only change when you do that. A strip's cable, or B1's or B2's, can be switched off.
* **Buses feed buses.** Each bus has a **TO** row under its fader, like the strips: the other buses its mix also goes to, after its fader. B1 → A2, for example, puts what Discord hears from B1 in your headphones. A bus never feeds itself, and a send that would close a loop is greyed out and refused. A send reaches the other bus in the same audio cycle (no delay) and switches without clicks; sends are saved (`routes =` under `[bus.XX]`) and are part of scenes. In the terminal UI, `1`–`5` on a bus row switch its sends.
* **All five outputs under the faders.** The bar at the bottom shows A1, A2, A3, B1 and B2 with their levels, next to the inputs' quick mix (2.6 showed A1 only).
* **Muted channels show their input, greyed out.** While a channel is muted (or a strip is soloed out), its meter shows the level coming in, in grey, so you can see there is signal before you unmute it. The terminal UI does the same.
* **VBAN shows this computer's IP addresses:** in the VBAN window (with a Copy button and what to enter in Voicemeeter), on the VBAN button, in the terminal UI's VBAN line and in the log. Each network's broadcast address is listed too.
* **The window resizes properly.** The console is scaled as a whole to fit the window: you always see all of it, as large as the window allows. When the window's shape differs from the console's, the faders get longer or the strips wider, so nothing wraps and no empty space is left. Zoom in the menu still works on top of that, and a long channel name gets a smaller font before it is cut.

Upgrading from 2.6: your config and scenes load as they are. The new settings (`cable =`, and `routes =` on a bus) are only written once you use them; until then, 2.6 can still read the files. Takes that 2.6 recorded too fast still hold all their audio; only the rate in their header is wrong. To fix one, give it the rate the server really ran at (usually 44100; such a take sounds about 9 % too fast): `ffmpeg -i take.wav -af asetrate=44100 -c:a pcm_s24le fixed.wav` (use `pcm_s16le` for 16-bit takes and `pcm_f32le` for 32-bit float). It changes only the rate in the header; every sample stays as it was.

## [2.6]

* **A new studio interface**, built from the BananaSplit studio design: every strip has a **GAIN** knob (input gain, ±24 dB), a segmented level meter with peak hold, a long fader, a **PAN** knob (BALANCE on buses), MUTE / SOLO with their lamps, the EQ and MONO switches, and the A1–B2 routing chips; the six inputs, the three hardware buses and the two virtual buses each have their colour; a quick-mix bar shows every input's level and routing and the A1 master; a session panel shows messages, round trip, DSP load and xruns. Dark by default, with a light theme and zoom in the menu. Every control is live and is saved.
* **A real desktop app.** `bananasplit --gui` opens BananaSplit in **its own window** (GTK + WebKitGTK, no browser) and puts an **icon in the system tray**: left click shows or hides the window, its menu has Show, Start/Stop recording and Quit. **Closing the window keeps the mixer running** in the tray, like Voicemeeter (or quits it, if you switch that off). **Start with the system** starts it in the tray when you log in. Launching it again from the app menu brings the window back. If the window crashes, the mixer keeps running. Without WebKitGTK the page opens in a browser's app window and the tray icon still works.
* **Install it like an app:** a **.deb package** (Debian, Ubuntu, Mint, Pop!_OS) and a **portable package** for every other distribution (`./install.sh`, or "Add BananaSplit to the app menu" in the GUI): app-menu entry with its icon, "Quit BananaSplit" in the launcher's right-click menu, `bananasplit --install` / `--uninstall`. The binary needs glibc 2.34 or newer (Ubuntu 22.04, Debian 12, Fedora 35, RHEL 9 and later).
* **No more `pw-jack` on Debian/Ubuntu:** when the system's libjack finds no JACK server but PipeWire is running, BananaSplit uses PipeWire's own libjack from its private folder, so it starts from the app menu as it is.
* **Player:** the recorder panel plays recordings back (PLAY / PAUSE / STOP, a waveform you can click to jump, ‹ › to go through your takes) into the buses you choose (PLAY TO, A1 by default). Multitrack takes play as their mix. Any WAV file in the recordings folder plays, from 8 kHz up to four times the mixer's sample rate.
* **Scenes:** save the whole mix (faders, gains, pans, mutes, solos, routing, EQ, armed tracks) under a name and load it back with one click, from the SCENE menu.
* The recorder panel shows the disk space left, and opens the recordings folder.
* **Only your user account can reach the mixer** over its local web port; 2.5 also answered other users of the same computer.
* Upgrading from 2.5: your config loads as it is (the new settings start at 0 dB, centre, and the dark theme). Once 2.6 has saved it, 2.5 can no longer read it. Your recordings, VBAN streams and EQs are untouched. If you copied 2.5's `contrib/bananasplit.desktop` into `~/.local/share/applications/`, `./install.sh` (or `bananasplit --install`) replaces it; with the .deb, delete that copy, so the package's entry with the new icon shows. Update a copied `contrib/systemd/bananasplit.service` the same way (it no longer needs `pw-jack`).

## [2.5]

* **New name.** vmix is now **BananaSplit**: the command is `bananasplit`, the JACK client and the virtual cables are called `BananaSplit` (`BananaSplit-In1-Mic`, `BananaSplit-Out-B1`, …), the config lives in `~/.config/bananasplit/bananasplit.ini`, and recordings go to `~/Music/BananaSplit/` by default.
* **Dark mode for the GUI**, redesigned: near-black panels, banana-yellow keys and faders, and a **theme switch** in the top bar (Auto follows your desktop, or pick Light or Dark; the GUI remembers it).

Upgrading from vmix 2.4:

* The first start copies `~/.config/vmix/vmix.ini` to the new place, so your strips, routing, EQ, VBAN and recorder settings come along. The old file is left alone. If your JACK name was the old default `vmix`, it becomes `BananaSplit`; that renames the virtual cables, so pick them again once in the apps that used them (sound settings, Discord, OBS). A name you chose yourself is kept, and so is your recorder folder.
* If you ran `vmix --low-latency`, BananaSplit sees those files as installed. `bananasplit --low-latency` replaces them with its own, and `bananasplit --low-latency off` removes them too.
* Copy `contrib/bananasplit.desktop` and `contrib/systemd/bananasplit.service` again if you installed the old ones (and `systemctl --user disable --now vmix`).

## [2.4]

* **VBAN**, compatible with Voicemeeter (Banana, Potato and the others) and with VB-Audio's VBAN apps: up to 8 incoming streams, each added to a strip of your choice, and up to 8 outgoing streams, each sending a strip or a bus to another computer. Incoming streams can have any sample rate, format and channel count; BananaSplit resamples them and follows the sender's clock, so they play without gaps or drift. See [VBAN](#vban-network-audio-voicemeeter-compatible).
* **EQ** on all 11 channels: 6 bands, each a bell, low or high shelf, low or high cut, or notch, from 20 Hz to 20 kHz, ±24 dB, Q 0.1–30. It is off by default; off, or flat, it leaves the audio bit for bit unchanged. See [EQ](#eq).
* Both work in the terminal UI (`e`, `E`, `v`), in the GUI (an EQ editor with a draggable curve, a VBAN window) and in `--headless` mode, and both are saved in the config file.

## [2.3]

Changes from a review on real hardware:

* `--no-cables` now applies to one run only. It is no longer written into the config file.
* **`connect = @default`** follows the input and output devices chosen in your desktop's sound settings, and it is the new default for strip 1 and A1. `@physical` now prefers the default device too, instead of whichever device JACK lists first (a webcam, for example).
* **`bananasplit --list-ports`** shows every JACK name a `connect` pattern can match, with a ready-made `connect =` line per device. When a pattern matches nothing, BananaSplit says why: a PipeWire node name was used instead of the JACK name, the case is wrong, or the port points the wrong way. On stderr it also lists the ports that would work.
* **`bananasplit --low-latency`** installs and applies the PipeWire low-latency settings in one step. At startup, BananaSplit warns if the round trip is 10 ms or more and says what to do about it.
* **Killed BananaSplit:** a small watchdog process removes the virtual cables if BananaSplit is killed with SIGKILL or crashes. `bananasplit --cleanup` and the systemd unit's `ExecStopPost` cover the case where the watchdog was killed too.
* **Terminal UI:** names get 15 columns at 80 columns (more on a wider terminal) and end in `…` when cut. Long status lines are cut instead of wrapping, and the PEAK column no longer shifts by one between silent and loud rows.
