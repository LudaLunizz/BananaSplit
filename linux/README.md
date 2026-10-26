# BananaSplit: a 6-strip virtual audio mixer for Linux

A Voicemeeter Banana-style mixer written in Rust, with **6 input strips** and **5 buses** (A1–A3 play on your speakers and headphones, B1–B2 are virtual microphones for other apps; any bus can be either or both, and buses can feed each other). Every channel has a **menu to pick its devices and apps**, a **virtual cable** (from the start on the strips and B1–B2), input gain, pan and a **6-band EQ**, **VBAN network audio** that works with Voicemeeter, a **multitrack recorder and player**, and **scenes**. It runs as a single real-time JACK client, natively on JACK2 or on PipeWire's JACK layer. It is a desktop app: its own window, an icon in the app menu and in the system tray, and it can start with the system (`bananasplit --gui`). It also runs in a terminal or as a background service.

```
 BananaSplit  6 strips · 5 buses · JACK client "BananaSplit" · 8 cables
 48000 Hz · 128 fr · 2.67 ms/period · RTT 6.10 ms [OK <10ms] · DSP 1% · xruns 0
 VBAN 192.168.1.23:6980 · in: 1 of 1 receiving · out: 1 of 1 sending
  #  NAME              GAIN  L          R          PEAK M S O E  A1 A2 A3 B1 B2
> 1  Mic               +0.0  ██████···· ██████····  -24 · · O E  A1 A2 A3 B1 B2
  2  Discord           -7.4  █████····· █████·····  -31 · · · ·  A1 A2 A3 B1 B2
  ...
 A2  Blue Headphones   +0.0  ████······ ████······  -30 ·   · E  A1 ── A3 B1 B2
```

## What's new in 2.8.4

* **Recordings are MP3 now.** Takes are recorded as MP3 at 320 kbit/s: about 144 MB per hour and track, a seventh of 24-bit WAV, and they play everywhere. **FORMAT** in the recorder panel has MP3 next to 16-bit, 24-bit and 32-bit float WAV, which are still there for lossless takes; `bitrate =` under `[recorder]` sets 128 to 320 kbit/s. BananaSplit encodes the MP3 itself, on Linux and on Windows: nothing else to install.
* **At their own speed, to the sample.** An MP3 has the sample rate the audio server runs at, so it plays at 1.0x. MP3 holds 32, 44.1 and 48 kHz: when the server runs at another rate (96 kHz, say), the take is converted to 48 kHz, and the recorder panel says so. Every file starts with an Info frame whose LAME tag gives its exact length, so players that read it (mpg123, GStreamer, ffmpeg and the programs built on them, and BananaSplit's own player) play exactly the samples that were recorded, without the short silence an MP3 otherwise starts with.
* **Hourly parts that join without a seam.** A long take still continues in `_part2.mp3`, `_part3.mp3` … every hour, at the same sample on every track. Each part also holds the end of the part before it and the start of the next one, hidden by its LAME tag, so the moment where two parts meet is encoded like any other: played one after the other, the parts give the whole take back, without a gap, a repeated sample or a click.
* **Every track is written by a thread of its own.** Encoding up to 11 tracks, finishing a part and waiting for the disk to store it no longer hold up the recorder. Before, a disk that took longer than the 4-second buffer to finish the files at an hourly split left a gap of silence at the start of the next part.
* **The player plays MP3:** BananaSplit's takes and any other MP3 in the recordings folder (MPEG-1, 2 and 2.5, constant or variable bitrate, with ID3 or APE tags), with their waveform, from any point. A WAV file and an MP3 file of the same name are two takes.

Configs saved by 2.8.3 and older say `format = wav24`, the default they were written with: those record MP3 now. A 16-bit or 32-bit float choice stays; 24-bit WAV is kept once you choose it again. Configs saved by 2.8.4 have a `bitrate =` line, and may say `format = mp3`, which 2.8.3 and older refuse; delete the line (and pick a WAV format) to go back.

## What's new in 2.8.3

* **Windows: BananaSplit's own virtual cables.** BananaSplit for Windows now works with **Splitter**, its own cable driver (installed separately), which gives every channel a cable of its own, as Linux has always had: apps play into `Splitter 1` … `Splitter 6` to reach strips 1 to 6, and record `Splitter A1` … `Splitter B2` to hear buses A1 to B2 (once the bus's cable is on in its menu; B1 and B2 have theirs on to begin with). Switching one channel's cable off no longer moves another channel's. VB-Audio's cables still work, for any channel whose Splitter cable is missing; with Splitter installed, the other channels move to Splitter's cables, so apps you pointed at "CABLE Input" or "CABLE Output" need pointing at the Splitter cable of that channel. **Menu ▸ Virtual cables…** shows both. For now Splitter is a test build: Windows loads it only in test-signing mode. See the [Windows manual](../windows/README.md#virtual-cables).
* **Windows:** the Quit item and the "BananaSplit has stopped" message no longer say that quitting removes the virtual cables; on Windows they stay installed and fall silent.

Nothing changes on Linux. On Windows without Splitter, nothing changes either.

## What's new in 2.8.2

* **Long recordings are split every hour.** A take now continues in `_part2.wav`, `_part3.wav` … every 60 minutes, at the same sample for every track, so a long session is a set of files you can open and send one at a time. Change it with `split =` under `[recorder]` (minutes, up to a day; `0` or `off` splits only before 4 GB, as before).
* **Resizing the window is smooth again.** On Linux, BananaSplit switched WebKitGTK's GPU renderer off everywhere, which made the window lag while it was resized; it now does that only with NVIDIA's own driver, where the window would otherwise stay empty. Channel names are no longer measured again on every resize step.
* **Windows: a setup window for virtual cables.** **Menu ▸ Virtual cables…** explains what the optional VB-Audio cables are for, how to install them, that they leave Voicemeeter alone, and that Windows may make a new cable its default device. It shows which cables are installed, whether Voicemeeter is, and the Windows defaults, and opens by itself on the first start without cables (instead of a message box that could hide behind the window). See the [Windows manual](../windows/README.md#virtual-cables).

Nothing to change on your side. Configs saved by 2.8.2 have a `split =` line, which 2.8.1 and older refuse; delete that line to go back.

## What's new in 2.8.1

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

## What's new in 2.8.0

* **BananaSplit can update itself.** **Menu ▸ Check for updates…** opens the Updates window: **Check now** looks for a new version on BananaSplit's GitHub releases, shows what is new in it, and **Install** downloads it, checks it and puts it in the place of the running program. The mixer keeps running on the old version; **Restart now** (or your next start) runs the new one, with your settings. See [Updates](#updates).
* **Automatic updates are off by default.** Switched off, BananaSplit never contacts anything by itself: it looks for a new version only when you click Check now. Switch **Install updates automatically** on in the Updates window and it checks a minute after it starts and once a day, installs a new version by itself and tells you when a restart finishes the update. It never restarts on its own, so your audio is never cut.
* `bananasplit --update` does the same from a terminal.

Nothing to change on your side: configs, scenes and recordings load as they are, and 2.7.3 can still read a config saved by 2.8.0 while automatic updates are off.

## What's new in 2.7.3

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

## What's new in 2.7.1

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

## What's new in 2.7

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

## What's new in 2.6

* **A new studio interface**, built from the BananaSplit studio design: every strip has a **GAIN** knob (input gain, ±24 dB), a segmented level meter with peak hold, a long fader, a **PAN** knob (BALANCE on buses), MUTE / SOLO with their lamps, the EQ and MONO switches, and the A1–B2 routing chips; the six inputs, the three hardware buses and the two virtual buses each have their colour; a quick-mix bar shows every input's level and routing and the A1 master; a session panel shows messages, round trip, DSP load and xruns. Dark by default, with a light theme and zoom in the menu. Every control is live and is saved.
* **A real desktop app.** `bananasplit --gui` opens BananaSplit in **its own window** (GTK + WebKitGTK, no browser) and puts an **icon in the system tray**: left click shows or hides the window, its menu has Show, Start/Stop recording and Quit. **Closing the window keeps the mixer running** in the tray, like Voicemeeter (or quits it, if you switch that off). **Start with the system** starts it in the tray when you log in. Launching it again from the app menu brings the window back. If the window crashes, the mixer keeps running. Without WebKitGTK the page opens in a browser's app window and the tray icon still works.
* **Install it like an app:** a **.deb package** (Debian, Ubuntu, Mint, Pop!_OS) and a **portable package** for every other distribution (`./install.sh`, or "Add BananaSplit to the app menu" in the GUI): app-menu entry with its icon, "Quit BananaSplit" in the launcher's right-click menu, `bananasplit --install` / `--uninstall`. The binary needs glibc 2.34 or newer (Ubuntu 22.04, Debian 12, Fedora 35, RHEL 9 and later).
* **No more `pw-jack` on Debian/Ubuntu:** when the system's libjack finds no JACK server but PipeWire is running, BananaSplit uses PipeWire's own libjack from its private folder, so it starts from the app menu as it is.
* **Player:** the recorder panel plays recordings back (PLAY / PAUSE / STOP, a waveform you can click to jump, ‹ › to go through your takes) into the buses you choose (PLAY TO, A1 by default). Multitrack takes play as their mix. Any WAV file in the recordings folder plays, from 8 kHz up to four times the mixer's sample rate.
* **Scenes:** save the whole mix (faders, gains, pans, mutes, solos, routing, EQ, armed tracks) under a name and load it back with one click, from the SCENE menu.
* The recorder panel shows the disk space left, and opens the recordings folder.
* **Only your user account can reach the mixer** over its local web port; 2.5 also answered other users of the same computer.
* Upgrading from 2.5: your config loads as it is (the new settings start at 0 dB, centre, and the dark theme). Once 2.6 has saved it, 2.5 can no longer read it. Your recordings, VBAN streams and EQs are untouched. If you copied 2.5's `contrib/bananasplit.desktop` into `~/.local/share/applications/`, `./install.sh` (or `bananasplit --install`) replaces it; with the .deb, delete that copy, so the package's entry with the new icon shows. Update a copied `contrib/systemd/bananasplit.service` the same way (it no longer needs `pw-jack`).

## What's new in 2.5

* **New name.** vmix is now **BananaSplit**: the command is `bananasplit`, the JACK client and the virtual cables are called `BananaSplit` (`BananaSplit-In1-Mic`, `BananaSplit-Out-B1`, …), the config lives in `~/.config/bananasplit/bananasplit.ini`, and recordings go to `~/Music/BananaSplit/` by default.
* **Dark mode for the GUI**, redesigned: near-black panels, banana-yellow keys and faders, and a **theme switch** in the top bar (Auto follows your desktop, or pick Light or Dark; the GUI remembers it).

Upgrading from vmix 2.4:

* The first start copies `~/.config/vmix/vmix.ini` to the new place, so your strips, routing, EQ, VBAN and recorder settings come along. The old file is left alone. If your JACK name was the old default `vmix`, it becomes `BananaSplit`; that renames the virtual cables, so pick them again once in the apps that used them (sound settings, Discord, OBS). A name you chose yourself is kept, and so is your recorder folder.
* If you ran `vmix --low-latency`, BananaSplit sees those files as installed. `bananasplit --low-latency` replaces them with its own, and `bananasplit --low-latency off` removes them too.
* Copy `contrib/bananasplit.desktop` and `contrib/systemd/bananasplit.service` again if you installed the old ones (and `systemctl --user disable --now vmix`).

## What's new in 2.4

* **VBAN**, compatible with Voicemeeter (Banana, Potato and the others) and with VB-Audio's VBAN apps: up to 8 incoming streams, each added to a strip of your choice, and up to 8 outgoing streams, each sending a strip or a bus to another computer. Incoming streams can have any sample rate, format and channel count; BananaSplit resamples them and follows the sender's clock, so they play without gaps or drift. See [VBAN](#vban-network-audio-voicemeeter-compatible).
* **EQ** on all 11 channels: 6 bands, each a bell, low or high shelf, low or high cut, or notch, from 20 Hz to 20 kHz, ±24 dB, Q 0.1–30. It is off by default; off, or flat, it leaves the audio bit for bit unchanged. See [EQ](#eq).
* Both work in the terminal UI (`e`, `E`, `v`), in the GUI (an EQ editor with a draggable curve, a VBAN window) and in `--headless` mode, and both are saved in the config file.

## What's new in 2.3

Changes from a review on real hardware:

* `--no-cables` now applies to one run only. It is no longer written into the config file.
* **`connect = @default`** follows the input and output devices chosen in your desktop's sound settings, and it is the new default for strip 1 and A1. `@physical` now prefers the default device too, instead of whichever device JACK lists first (a webcam, for example).
* **`bananasplit --list-ports`** shows every JACK name a `connect` pattern can match, with a ready-made `connect =` line per device. When a pattern matches nothing, BananaSplit says why: a PipeWire node name was used instead of the JACK name, the case is wrong, or the port points the wrong way. On stderr it also lists the ports that would work.
* **`bananasplit --low-latency`** installs and applies the PipeWire low-latency settings in one step. At startup, BananaSplit warns if the round trip is 10 ms or more and says what to do about it.
* **Killed BananaSplit:** a small watchdog process removes the virtual cables if BananaSplit is killed with SIGKILL or crashes. `bananasplit --cleanup` and the systemd unit's `ExecStopPost` cover the case where the watchdog was killed too.
* **Terminal UI:** names get 15 columns at 80 columns (more on a wider terminal) and end in `…` when cut. Long status lines are cut instead of wrapping, and the PEAK column no longer shifts by one between silent and loud rows.

## ASIO / WASAPI on Linux

ASIO and WASAPI are **Windows-only** driver APIs and do not exist on Linux. What they give you on Windows (exclusive device access, no OS mixer or resampler in the path, small fixed buffers, and one callback that sees every channel at once) is what **JACK** provides on Linux. PipeWire implements the same API.

| Windows                       | BananaSplit on Linux                                                       |
|-------------------------------|---------------------------------------------------------------------|
| ASIO driver                   | JACK2 server, or PipeWire + `pipewire-jack`                         |
| WASAPI exclusive mode         | Device opened only by JACK/PipeWire at a fixed rate and quantum     |
| VB-Cable / "Voicemeeter Input"| `BananaSplit-In1…6` virtual output devices (PipeWire sinks)                |
| "Voicemeeter Output" (B1/B2)  | `BananaSplit-Out-B1`, `BananaSplit-Out-B2` virtual microphones                    |
| Voicemeeter Banana            | `bananasplit`                                                              |

## Sync and latency

* **Sample-synchronous by design.** The whole mixer (all 12 input channels and 10 output channels) is computed inside **one** JACK process callback for the same block of frames. A sample entering any strip at frame *k* leaves every bus at frame *k* of the same cycle. The mixer itself adds **0 samples** of latency.
* **Virtual cables add no buffering.** They are nodes in the same PipeWire graph, driven by the same clock, and processed in the same cycle.
* **Sub-10 ms** depends on the server's period. Use 48 kHz with 128 frames: 2.67 ms per period, which gives about 6–8 ms hardware round-trip including driver headroom. Run `bananasplit --low-latency` once to pin this (see [Latency](#latency)). The UI shows the measured round-trip from JACK's latency API and turns red if it reaches 10 ms or more.
* **Real-time safe.** The audio callback does no allocation, no locks, no syscalls, and cannot panic. Every gain, mute, solo, or routing change is ramped over one period, so changes don't click.

## Install

**Debian, Ubuntu, Mint, Pop!_OS:** install the package, which pulls in what it needs:

```sh
sudo apt install ./bananasplit_2.8.0_amd64.deb
```

**Any other distribution (Fedora, Arch, openSUSE, …):** unpack the portable package and run its installer. It copies `bananasplit` to `~/.local/bin` and adds BananaSplit with its icon to your app menu; nothing outside your home folder is touched (`./install.sh --uninstall` removes it again).

```sh
tar -xzf bananasplit-2.8.0-linux-x86_64.tar.gz
cd bananasplit-2.8.0-linux-x86_64 && ./install.sh
```

Then start **BananaSplit** from the app menu. What it needs from the system (PipeWire is the default on current Fedora, Ubuntu, Debian, Arch, and others; the .deb asks for these itself):

| Distro         | Audio (required)                                        | App window and tray (recommended)  |
|----------------|---------------------------------------------------------|------------------------------------|
| Fedora         | `pipewire-jack-audio-connection-kit pulseaudio-utils`   | `webkit2gtk4.1`                    |
| Arch           | `pipewire-jack libpulse`                                | `webkit2gtk-4.1`                   |
| openSUSE       | `pipewire-libjack-0_3 pulseaudio-utils`                 | `libwebkit2gtk-4_1-0`              |
| Debian/Ubuntu  | `pipewire-jack pulseaudio-utils`                        | `libwebkit2gtk-4.1-0`              |

Or JACK2 (`jackd2`) instead of PipeWire. The virtual cables need PipeWire; without it, set `create_cables = false` (or start with `--no-cables`, which applies to that run only) and route with JACK. Without WebKitGTK, BananaSplit shows its window in Chromium, Chrome, Brave, Edge or Vivaldi (or your default browser) instead.

**From source:**

```sh
cargo build --release            # zero dependencies
target/release/bananasplit --install    # into ~/.local/bin and the app menu
bananasplit --low-latency               # optional but recommended for < 10 ms (see Latency)
```

`packaging/build-packages.sh` builds the .deb and the portable package. Run BananaSplit with `bananasplit --gui` (the app), `bananasplit` (terminal UI) or `bananasplit --headless` (no UI; `contrib/systemd/bananasplit.service` runs it in the background).

## Using it

1. Start BananaSplit from the app menu (or `bananasplit --gui`; `bananasplit` alone runs it in the terminal). Eight devices appear in your sound settings: `BananaSplit-In1-Mic` … `BananaSplit-In6-Aux` (outputs) and `BananaSplit-Out-B1`, `BananaSplit-Out-B2` (microphones). Strip 1 takes your default microphone and A1 plays on your default output (`@default`). Each channel's menu (the button under its name) shows what else you can connect; so does `bananasplit --list-ports`.
2. Point applications at a strip: pick the app in the strip's menu, or set the app's output device (pavucontrol, GNOME/KDE settings, or the app itself), for example Discord to `BananaSplit-In2-Discord`. To send the whole desktop through BananaSplit, make `BananaSplit-In3-Desktop` the default output.
3. Choose where each strip goes with its A1 A2 A3 B1 B2 chips (keys `1`–`5` in the terminal UI). Buses have the same chips, to feed other buses.
4. In Discord or OBS, select **`BananaSplit-Out-B1`** as the microphone to receive the B1 mix (or tick the app in B1's menu).
5. The mixer state is saved automatically on quit; "Save settings" in the app's menu (`w` in the terminal UI) saves it at once.

Terminal UI keys: `↑↓` select · `←→` gain ±1 dB · `,` `.` ±0.1 dB · `0` 0 dB · `m` mute · `s` solo · `o` mono · `e` EQ on/off · `E` EQ page · `1-5` route · `c` link to recorder · `R` record/stop · `v` VBAN on/off · `r` reconnect · `w` save · `q` quit

A `●` after a row's number means that channel is linked to the recorder, and an `E` in the flags means its EQ is on. The line under the mixer shows the recorder: elapsed time and file while recording, otherwise the mode, format, links and folder. When VBAN is on, the line above the mixer shows how many streams are receiving and sending, and the line under the mixer lists the streams of the selected channel.

`E` opens the **EQ page** of the selected channel: its six bands, with the response drawn underneath (+18 to −15 dB, 20 Hz to 20 kHz). Keys there: `↑↓` band · `←→` field (type, frequency, gain, Q) · `,` `.` change it (a semitone, 0.5 dB, or a small Q step) · `<` `>` change it faster (a third of an octave, 3 dB) · `space` band on/off · `0` flatten the band · `e` EQ on/off · `[` `]` previous/next channel · `E` or `Enter` back to the mixer.

## The app (`bananasplit --gui`)

Start **BananaSplit** from the app menu, or run `bananasplit --gui`. The window shows, from left to right, the six **input strips** (green), the buses A1–A3 (blue: speakers, headphones), the buses B1–B2 (purple: the virtual microphones other apps record from) and the **recorder**; underneath, the quick-mix bar with every input, the five outputs, and the session panel. (Colours and places stay as they are when you give a bus another role: an A bus can be a virtual microphone too, a B bus can play on a device.)

**A strip, top to bottom:**

* the **name** (double-click it, or press F2, to rename) and a lamp that lights green with signal and red when it clipped; underneath, the channel's **menu button**, which says what it takes in (IN and a microphone, or Discord) or where it plays (OUT and your speakers, or OBS); click it to choose (see [Channel menus](#channel-menus)). Hover for the full list;
* **GAIN**: the input gain, −24 to +24 dB (on a bus: before its fader). Drag the knob up or down, scroll over it, or use the arrow keys (Shift for fine steps); double-click for 0 dB. The **EQ** switch next to it opens the channel's EQ editor and lights while the EQ is on;
* **LEVEL**: the meter (left and right, after the fader; while the channel is muted or soloed out, the level coming in, in grey) and the **fader**, −60 to +12 dB: drag it, scroll over it, use the arrow keys or Page Up/Down, double-click for 0 dB, or type a value into the box under it. **PK** is the highest level of the last 3 seconds (red after clipping);
* **PAN** (on buses **BALANCE**): the side it turns away from fades out, the other side stays at full level; double-click for centre. **MONO** sums left and right;
* **MUTE** and **SOLO** (strips only; while any strip is soloed, only soloed strips are heard), with their lamps;
* **TO**: where the strip goes, A1 A2 A3 B1 B2. On a bus, the other buses it feeds after its fader (its own chip is left out; one that would make a loop is greyed out).

Click a strip to select it (green outline); then **M** mutes it, **S** solos it, **O** switches mono, **E** opens its EQ, **1**–**5** route it (on a bus: its sends), **[** and **]** select the previous or next strip. **Space** plays or pauses. **Ctrl +**, **Ctrl −** and **Ctrl 0** zoom.

**The recorder:** the **state** (ready, recording, playing, with the free disk space), the **waveform** of the selected recording (‹ › go through your takes; click into it to play from there; while recording it shows the armed channels' level), the **time code**, and **PLAY** (PAUSE while playing), **REC** and **STOP** (stops playback and recording). **PLAY TO** chooses the buses recordings play on (A1 by default): playback joins those buses before their EQ and fader, so it is heard, recorded and sent like anything else on them. **ARMED TRACKS** are the channels that are recorded: MIX puts them into one stereo file, MULTI writes one file per channel. **FORMAT** is MP3 (the default), or 16-bit, 24-bit or 32-bit float WAV. **OPEN** opens the recordings folder; the bar at the bottom is the disk.

**Scenes:** the **SCENE** menu saves the whole mix under a name ("Save the mix as a new scene…") and loads it back with one click; "Update" saves over the current scene, "Delete" (click twice) removes it. A scene holds every fader, gain, pan, mute, solo, mono, routing and EQ, and the armed tracks; it never changes names, devices, VBAN or recorder settings, and everything changes with the usual click-free ramps. Scenes are small files in `~/.config/bananasplit/scenes/`.

**MENU:** Save settings, Reconnect devices, Open the recordings folder, **Theme** (Dark, Light, or Auto to follow the desktop's light or dark style, GNOME's included), **Zoom**, **Start with the system**, **Closing the window keeps it running**, Add BananaSplit to the app menu (when it is not there yet), and **Quit BananaSplit** (click twice; it removes the virtual cables). The **VBAN** button opens the network audio window (its dot is green while streams flow).

* **EQ editor:** drag a numbered point on the curve to change that band's frequency and gain, scroll over it to change its Q, double-click it to flatten it, or select it and use the arrow keys (Page Up/Down for Q, Space for on/off). Below the curve, each band has its on switch, type, frequency (`1.5k` works), gain and Q. The switch at the top turns the channel's EQ on and off.
* **VBAN window:** this computer's IP addresses (with Copy, and each network's broadcast address), the on switch, the UDP port, the incoming and outgoing streams with their status, and a list of streams that arrive but are not set up yet; **Receive** next to one of those adds it as an incoming stream. The VBAN button shows the main IP address while VBAN is on.

### Channel menus

Every strip and every bus has a menu: the button under its name. It lists what the channel can use, in groups, with a tick at what it uses now:

| Group | On a strip (what comes in) | On a bus (where it goes) |
|---|---|---|
| Virtual cable | `BananaSplit-In1-Mic` …: an output device for apps; it lists the apps that play into it | `BananaSplit-Out-A1` … `-B2`: a microphone for apps; it lists the apps that record it |
| Apps | the apps that play sound: tick one to move it into this strip's cable (the cable is switched on if needed); untick it to send it back to the default output | the apps that record: tick one to make it record this bus (the virtual microphone is switched on if needed); untick it to give it the default microphone back |
| Devices | microphones and sound cards | speakers and headphones |
| Programs (JACK) | other JACK programs that send audio | other JACK programs that take audio |
| What an output plays | the monitor of an output device (records what plays there; can feed back) | – |
| Follow the desktop | **Default input**: the device picked in the sound settings (`connect = @default`) | **Default output** |
| Config lines | `connect =` lines with a pattern of your own: click one to remove it | same |

* Ticking a device or program connects it at once and adds `connect = ^<its JACK name>:` to the channel, so it is connected again after a restart and when it is plugged in again; unticking disconnects it and removes the lines that brought it in. **Disconnect all** at the bottom clears the channel's connections (its virtual cable stays). The config is saved like every other change: with "Save settings" in the menu, and on quit.
* Moving an app is what pavucontrol or the sound settings do (`pactl move-sink-input` / `move-source-output`); WirePlumber normally remembers it for the next time that app plays. Without PipeWire's `pactl`, the menus still list devices and JACK programs, but not apps.
* Switching a cable off removes it from the sound settings; PipeWire moves the apps that used it to the default device. A strip without its cable still takes in whatever is connected to it.
* The menus follow changes made elsewhere (the sound settings, pavucontrol, qpwgraph, Helvum) within a moment.

### Window, tray and starting with the system

* The window is a separate process (GTK 3 and WebKitGTK 4.1 or 4.0, loaded when it starts, so BananaSplit itself still has no dependencies). If it crashes, the mixer keeps running; open it again from the app menu.
* **The tray icon** (StatusNotifierItem, the standard of KDE, Xfce, Cinnamon, MATE, Budgie, and of GNOME with the AppIndicator extension that Ubuntu ships): left click shows or hides the window; the menu has Show, Start/Stop recording (the icon gets a red dot while recording) and Quit. On GNOME without that extension there is no tray; BananaSplit then minimizes the window instead of hiding it, so you can always get it back from the dock.
* **Closing the window** keeps BananaSplit running in the tray (the mixer, its cables and its routing keep working, like Voicemeeter). To quit, use Quit in the menu or the tray, or "Quit BananaSplit" in the launcher's right-click menu (`bananasplit --quit`). Switch off "Closing the window keeps it running" to make closing quit.
* **Start with the system** writes `~/.config/autostart/bananasplit.desktop`, which runs `bananasplit --gui --hidden`: the mixer starts in the tray when you log in (it waits up to 30 seconds for the audio server). Switch it off in the menu.
* Launching BananaSplit again while it runs brings its window to the front (on Wayland too, through the desktop's activation token).
* If BananaSplit cannot start when launched from the app menu (no JACK or PipeWire, for example), it shows the reason in a dialog.
* The window size, theme, zoom and close behaviour are saved in the `[gui]` section of the config.
* The console is scaled to fit the window at any size, and stretches to the window's shape (longer faders in a tall window, wider strips in a wide one). Below about 1000 × 500 pixels at 100 % zoom it stops shrinking, and the window scrolls instead.
* **Without WebKitGTK** the page opens as an app window of Chromium, Chrome, Brave, Edge or Vivaldi, or in your default browser, and the tray icon still works (Show opens the page again).
* The page is served by BananaSplit itself on **http://127.0.0.1:8790** (this computer only); you can also open that address in any browser. Options: `--gui-port PORT` (default 8790; if 8790 is busy, BananaSplit picks a free port, prints it and notes it in `$XDG_RUNTIME_DIR/bananasplit/`, so launching it again and `--quit` still find it), `--no-open` (no window and no tray icon; open the printed URL yourself), `--hidden` (start in the tray). For a background service you can open at any time, change the service to run `bananasplit --gui --no-open`. Without `--gui`, no TCP port is open (VBAN, when you switch it on, uses a UDP port).
* Only you can use it: BananaSplit answers only connections from programs of the user it runs as (it checks who owns the connecting socket), so other users of the computer cannot listen in or change the mix. Requests must name `127.0.0.1`/`localhost` as their host, and changes need a header that other websites cannot send, so a web page open in your browser cannot drive it either. Programs you run yourself can, like the page does (as they could use your microphone directly).
* It controls the same mixer state as the terminal UI, with the same click-free ramps, and saves on quit the same way. Input gain, pan, the player and scenes are GUI controls; the terminal UI keeps them as they are and saves them too.

### Updates

**Menu ▸ Check for updates…** opens the Updates window. It shows this version, what the last check found and, for a new version, its release notes.

* **Check now** asks GitHub for BananaSplit's releases and picks the newest one that is published and not marked pre-release, with the file for your installation: the portable package (`bananasplit-<version>-linux-x86_64.tar.gz`) or, if BananaSplit came from its .deb, the new .deb.
* **Install** downloads it to `~/.cache/bananasplit/update/` (`bananasplit --update`: `update-cli/`) and checks it before anything is changed: it must come from BananaSplit's own release downloads over HTTPS, have the size GitHub lists, and match the release's `SHA256SUMS.txt`, which must be signed with BananaSplit's release key (`SHA256SUMS.txt.sig`; an unsigned release is refused), and GitHub's SHA-256 of the file. The new program must also run on this computer and say it is that version.
  * **Portable package, or a copy in a folder of yours:** the new `bananasplit` is written next to the old one and renamed over it (in one step, so there is never half a program). The copy in `~/.local/bin` that your app menu starts is updated too. The mixer that runs keeps using the old program until it ends.
  * **The .deb:** BananaSplit installs the downloaded package with `apt-get` through `pkexec`, so your desktop asks for your password. As root, the package is first copied to a folder only root can write and checked again, and apt-get never removes another package for it.
  * **Anything else** (another package manager, an AppImage, a folder BananaSplit cannot write to): the window says so, and **Release page** opens the release in your browser so you can install it yourself.
* **Restart now** saves everything, removes the cables, quits, and starts the new version with the same options. It stays offered until you restart, also when a later check fails. While you are recording it asks for a second click, because the restart ends the take.
* **Install updates automatically** is **off** by default (`auto_update` in `[gui]`, written only when it is on). Off, BananaSplit asks nothing of anybody: it checks only when you click Check now. On, it checks a minute after it starts and then once a day (an hour later if a check fails), installs a new version by itself once it has been published for two days (so a broken release can be withdrawn before it reaches you; Install takes it at once), and says in the status line and with a dot on the Menu button that a restart finishes the update. It never restarts on its own. With the .deb it downloads and checks the package by itself and waits for you to click Install (and enter your password).
* `bananasplit --update` checks and installs from a terminal (a running BananaSplit uses the new version after a restart). With the .deb, it installs in a terminal (it asks for your password there) and otherwise prints the `sudo apt install` command for the package it checked.
* Downloads use `curl` (or `wget` when there is no curl; only curl is told to stay on HTTPS through GitHub's redirects, so prefer it); the .deb recommends curl. Only the terminal UI and `--headless` never check by themselves: the setting is the app's.

## Recorder

The recorder can take any combination of the 11 channels, numbered the same way everywhere: strips `1`–`6` and buses `A1`–`B2`. Arm a channel under ARMED TRACKS in the app's recorder panel, or link it with `c` in the terminal UI. Then press record (REC, or `R`).

* **What is recorded:** strips after their fader (gain, mute, solo and mono applied, exactly what the strip sends to the buses), and buses as they leave the mixer.
* **One mix file** (`mode = mix`): all linked channels are added into one stereo file. Links can change while recording; each change is ramped like a routing change, so it doesn't click. A strip linked together with a bus it feeds counts twice.
* **A file per source** (`mode = multi`): one stereo file per linked channel, for example `…_1-Mic.mp3`, `…_A1-Speakers.mp3`. They are taken from the same audio cycle, so they start on the same sample and always have the same length; drop them into a DAW and they line up. The set of tracks is fixed when recording starts.
* **Formats:** MP3 (default), at 320 kbit/s unless `bitrate =` says 128, 160, 192, 224 or 256; or 16-bit, 24-bit or 32-bit float WAV. MP3 and 16- and 24-bit files clip anything above full scale, and the GUI and terminal UI warn you when that happens; 32-bit float keeps it. BananaSplit's MP3 encoder is its own (MPEG-1 Layer III, constant bitrate, joint stereo, short blocks on attacks).
* **Files** go to `~/Music/BananaSplit/` by default, named `YYYY-MM-DD_HH-MM-SS_<label>.mp3` (or `.wav`) in local time. An existing file is never overwritten. The WAV header, or the MP3's Info frame, is rewritten every 2 seconds, so a recording is still playable if BananaSplit is killed. Takes continue in `_part2`, `_part3` … files every hour (`split =` in `[recorder]`, in minutes; `0` = off) and always before 4 GB, split at the same sample for every track. An MP3 part also holds a few milliseconds of its neighbours, which its LAME tag hides, so the parts join without a seam.
* **Sample rate:** files have the rate the audio server runs at (the top bar shows it), so they play at their real speed. MP3 holds 32, 44.1 and 48 kHz: at another server rate the take is converted (96 and 192 kHz to 48 kHz, 88.2 kHz to 44.1 kHz) with the windowed-sinc resampler VBAN uses, and the recorder panel says so. If the server changes its rate during a take, the take continues in new files at the new rate, `YYYY-MM-DD_HH-MM-SS-2_<label>.mp3`, starting on the same sample for every track, and the recorder panel says so.
* **Real-time safe:** the audio callback copies each cycle into a preallocated lock-free ring buffer (no allocation, locks or syscalls; see `tests/recorder_no_alloc.rs`), a separate thread takes the tracks apart, and a thread per track encodes and writes them. If the disk stalls for longer than the buffers hold (about 9 s with all 11 channels, much longer for fewer), the missing part is filled with silence so the tracks stay in sync, and the UI reports how many frames were lost.
* The recorder is available in the terminal UI and the GUI. `--headless` runs without it, exactly as before.

### Playing recordings back

The app's recorder panel also plays: it lists the MP3 and WAV files in the recordings folder (newest last; ‹ › step through them), draws the selected one's waveform, and plays it with PLAY, PAUSE and STOP; clicking into the waveform plays from there. A multitrack take (the `_1-Mic.mp3`, `_A1-Speakers.mp3` … files of one recording) plays as one take, its tracks mixed. Files at another sample rate, from 8 kHz up to four times the mixer's (192 kHz at 48 kHz), are converted with the same windowed-sinc resampler VBAN uses. MP3 (MPEG-1, 2 and 2.5 Layer III, constant or variable bitrate, after an ID3v2 tag and before ID3v1 or APE tags; not the rare free-format streams) and 8-, 16-, 24- and 32-bit PCM and 32/64-bit float WAV play. An MP3 with a LAME tag (BananaSplit's, LAME's, ffmpeg's) plays exactly its own samples, so the parts of a take join and its tracks stay together. Stopping, seeking and changing takes fade out over one period, so they do not click. **PLAY TO** chooses the buses it plays on (`play_to` in `[recorder]`, A1 by default): playback joins those buses before their EQ and fader, so it is heard, recorded and sent over VBAN like anything else on them. The file is read by a separate thread; the audio callback only reads a preallocated lock-free buffer (see `tests/player_no_alloc.rs`). Recording and playing at the same time works; a take that is still being recorded plays up to where it was when the list was last refreshed (every 3 seconds).

## EQ

Every strip and every bus has a 6-band parametric EQ. On a strip it comes after mono and before the fader, so it shapes what the strip sends to every bus, the recorder and VBAN. On a bus it comes before the bus fader.

| Type | What it does | Gain | Q |
|---|---|---|---|
| `bell` | boosts or cuts around the frequency | ±24 dB | width: higher = narrower |
| `lowshelf`, `highshelf` | boosts or cuts everything below / above the frequency | ±24 dB | 0.71 = no bump |
| `lowcut`, `highcut` | removes everything below / above (12 dB/octave) | – | 0.71 = flat (Butterworth) |
| `notch` | removes a narrow band (hum, a whistle) | – | width |

* The defaults are a low shelf at 100 Hz, bells at 250 Hz, 600 Hz, 1.5 kHz and 4 kHz, and a high shelf at 10 kHz, all at 0 dB. A new channel's EQ is off.
* The filters are the standard "Audio EQ Cookbook" responses (the GUI and terminal curves are computed from the same formulas), built as state-variable filters so that any change, even dragging a band across the whole range, glides smoothly without clicks or zipper noise.
* An EQ that is off, or whose bells and shelves are all at 0 dB with no cut or notch switched on, does nothing at all: the filters are skipped and the audio is unchanged bit for bit.
* Boosts can push a channel above 0 dBFS. Watch the meter and lower the fader if it clips.

## VBAN (network audio, Voicemeeter compatible)

VBAN is VB-Audio's protocol for sending audio over a network as UDP packets. BananaSplit implements the VBAN audio protocol (specification revision 13), so it can exchange audio with Voicemeeter on a Windows PC, with VB-Audio's VBAN apps, and with other BananaSplit machines.

* **Incoming streams** (8): a stream is known by its **name** and its **sender's IP address**, as in Voicemeeter. Each one is added to the input of a strip you choose, before that strip's EQ, fader and routing, so it mixes like anything else on that strip. Names are compared without regard to case. BananaSplit accepts every VBAN sample rate up to four times its own (for example up to 192 kHz at 48 kHz), 8-, 16-, 24- and 32-bit PCM and 32/64-bit float, and any number of channels (channels 1 and 2 are used; a mono stream plays on both sides).
* **Outgoing streams** (8): each sends one of the 11 channels (a strip after its fader, or a bus) to an IPv4 address and port, at BananaSplit's sample rate, as 16-, 24- or 32-bit PCM or 32-bit float, stereo or mono. A broadcast address such as `192.168.1.255` sends to every computer on the network.
* **One UDP port** (6980 by default, the same as Voicemeeter) receives every incoming stream and sends every outgoing one, so packets leave from port 6980 like Voicemeeter's do. VBAN is **off** by default; while it is off, BananaSplit does not listen. A stream BananaSplit sends that comes back to it (sent to its own address or to a broadcast address) is ignored, so it cannot feed back into itself.
* **Clock drift and jitter:** two computers' audio clocks never run at exactly the same speed, and networks (Wi-Fi especially) deliver packets in bursts. For each incoming stream BananaSplit measures the sender's clock against its own and follows it by resampling (high-quality windowed sinc). The speed changes glide and stay tiny: the clock difference itself (usually a few tens of ppm) plus at most 0.05 % to keep the buffer at its level, far below what anyone can hear. BananaSplit keeps a reserve of audio for late packets, `buffer_ms` (10 ms by default) at the buffer's lowest point, on top of what the sender's packet timing needs; the latency shown next to the stream includes all of it. If the audio still drops out (a network hiccup longer than the reserve), BananaSplit fades out, waits until it holds enough to ride out the same hiccup again, and fades back in. It also adds 5 ms to the reserve (up to 200 ms, given back 1 ms per 30 s without dropouts), and trims the extra delay of the restart back slowly, about half a millisecond per second. A lost packet becomes a short silence of the same length, so the stream does not shift.
* **Real-time safe:** the audio callback only reads and writes preallocated lock-free buffers; the network work is done by two separate threads (see `tests/eq_vban_no_alloc.rs`).

### With Voicemeeter

1. On the Linux computer, switch VBAN on: `v` in the terminal UI, the switch in the GUI's VBAN window, or `enabled = true` under `[vban]`. Its IP address is at the top of the VBAN window (and on the VBAN button, in the terminal UI's VBAN line and in the log).
2. Open UDP port 6980 in the Linux firewall if it has one: `sudo ufw allow 6980/udp` (Ubuntu) or `sudo firewall-cmd --add-port=6980/udp --permanent && sudo firewall-cmd --reload` (Fedora). Windows asks whether to allow Voicemeeter through its firewall the first time; allow it on private networks.
3. **Voicemeeter → BananaSplit:** in Voicemeeter's VBAN window, set up an outgoing stream to the Linux computer's IP, port 6980, with a name such as `Stream1`, and switch VBAN on there. 16- or 24-bit PCM works well. On BananaSplit, it then appears above the incoming streams in the GUI's VBAN window (click **Receive**), or add it to the config: `name = Stream1`, `from = <the Windows PC's IP>`, `strip = 6`.
4. **BananaSplit → Voicemeeter:** add an outgoing stream in BananaSplit with `to = <the Windows PC's IP>`, port 6980, and a name; in Voicemeeter, add an incoming stream with the same name and the Linux computer's IP as the source. 16-bit (the default) or 24-bit PCM works well. If Voicemeeter's side crackles over Wi-Fi, raise that incoming stream's Net Quality setting in Voicemeeter.

VBAN audio is not encrypted or authenticated, in Voicemeeter or here. BananaSplit only plays a stream whose name and sender IP match one you set up, so give incoming streams the sender's address rather than `any` on networks you don't trust.

## Configuration: `~/.config/bananasplit/bananasplit.ini`

Created with defaults on the first run (see `bananasplit --print-default-config`). Parsing is strict: a typo produces an error with a line number instead of being ignored. A value runs to the end of its line, so comments (`#` or `;`) go on lines of their own.

```ini
[global]
client_name = BananaSplit
# period requested from PipeWire (PIPEWIRE_LATENCY); 0 = don't ask
buffer_size = 128
# true = jack_set_buffer_size() (JACK2: changes it for every client)
force_buffer_size = false
create_cables = true

[gui]
# dark | light | auto (follows the desktop)
theme = dark
# closing the window keeps BananaSplit running in the tray
close_to_tray = true
# 0.5 .. 2
zoom = 1
# the window's size, saved when it closes
window = 1780x1000
# the scene loaded last (the SCENE menu shows it)
scene = Live Room
# install new versions by itself, checking once a day; a release is installed
# once it has been out for two days (default false:
# BananaSplit checks only when you click Check now)
auto_update = false

[strip1]
name = Mic
# the fader: -60 .. +12
gain_db = 0.0
# the GAIN knob (input gain): -24 .. +24
trim_db = 0.0
# -1 (left) .. 1 (right); on a bus, the balance
pan = 0.0
mono = true
# any of A1,A2,A3,B1,B2 or none
routes = B1
# the default microphone; may be repeated
connect = @default

[strip2]
# any JACK port regex works too
connect = ^Firefox:output_F[LR]$

[bus.A1]
name = Speakers
# the default output device
connect = @default
# also a virtual microphone, BananaSplit-Out-A1 (A1-A3: default off)
cable = on
[bus.A2]
# from `bananasplit --list-ports`
connect = ^Yeti Stereo Microphone Analog Stereo-63:
[bus.B1]
# buses this bus feeds after its fader (never itself, never a loop)
routes = A2

[strip6]
# no virtual cable for this strip (strips, B1, B2: default on)
cable = off

[strip3]
# this channel's EQ: on | off (default off)
eq = on
# band 1-6: type freq gain Q [off]
eq1 = lowcut 80 +0 0.71
# bands left out keep their defaults
eq4 = bell 3000 -4.5 2

[recorder]
folder = ~/Music/BananaSplit
# mp3 | wav16 | wav24 | wav32f
format = mp3
# kbit/s of MP3: 128 | 160 | 192 | 224 | 256 | 320
bitrate = 320
# mix (one stereo file) | multi (one file per linked channel)
mode = mix
# 1-6 = strips, A1-A3 / B1-B2 = buses, or none
links = 1,A1
# the buses recordings play on (any of A1..B2, or none)
play_to = A1
# minutes per file: a longer take goes on in _part2, _part3, ... (0 = off)
split = 60

[vban]
# default false: nothing listens
enabled = true
# UDP; receives every incoming stream, sends every outgoing one
port = 6980

# incoming streams: [vban.in1] .. [vban.in8]
[vban.in1]
on = true
# the sender's stream name (case does not matter)
name = Stream1
# the sender's IPv4 address, or any
from = 192.168.1.20
# added to this strip's input (1-6)
strip = 6
# reserve for late packets (0-200; raised by itself after dropouts)
buffer_ms = 10

# outgoing streams: [vban.out1] .. [vban.out8]
[vban.out1]
on = true
name = Linux
# the receiver's IPv4 address (x.x.x.255 = broadcast)
to = 192.168.1.20
port = 6980
# 1-6 = a strip after its fader, A1-A3 / B1-B2 = a bus
source = B1
# int16 | int24 | int32 | float32
format = int16
# 2 (stereo) | 1 (mono: (L+R)/2)
channels = 2
```

EQ types are `bell`, `lowshelf`, `highshelf`, `lowcut`, `highcut` and `notch`; the frequency is 20–20000 Hz, the gain −24 to +24 dB (ignored by the cuts and the notch) and Q 0.1–30. BananaSplit writes only the bands and streams that differ from the defaults.

A config file from an earlier BananaSplit (without `[gui]`, `trim_db`, `pan`, `play_to`, `[recorder]`, EQ, `[vban]`, `cable` or bus `routes`) still loads; what is missing starts with its default: 0 dB input gain, centre, the dark theme, closing to the tray, the recorder linked to A1 only and playing to A1, every EQ off, VBAN off, a virtual cable for every strip and for B1 and B2, and no bus sends. `cable` and bus `routes` are written only when they differ from the default, so 2.6 can read a file that does not use them.

**Scenes** are saved next to the config, in `~/.config/bananasplit/scenes/<name>.ini`, in the same format (the `[stripN]`, `[bus.XX]` and `[recorder]` keys a scene holds, bus sends included; a key left out takes its default value). You can copy them between computers. Only files that start with BananaSplit's `# BananaSplit scene` line are listed, and only those are ever replaced or deleted.

### Choosing devices

The channels' menus in the app write these lines for you (see [Channel menus](#channel-menus)).

| `connect =`  | strip (takes audio from)                       | bus (sends audio to)              |
|--------------|-------------------------------------------------|-----------------------------------|
| `@default`   | the default input in your sound settings       | the default output                |
| `@physical`  | a physical capture device: the default if it is one, else the one it had while that is there, else the first JACK lists | a physical playback device, same rule |
| a regex      | the first two matching JACK ports → L, R       | same                              |

`@default` needs PipeWire and `pactl`. It follows the sound settings: pick another default device there and the channel moves to it at once, letting go of the one it had (`@physical` moves with the default too, when the default is a physical device). Without PipeWire and `pactl` it falls back to `@physical` and says so. When the default device is one of BananaSplit's own cables (for example if you made `BananaSplit-In3-Desktop` the default output), a channel that already follows a device stays with it and says so (while that device is unplugged it falls back to `@physical`, and returns when the device does); one that has none yet (at start) falls back to `@physical`. To be sure what A1 plays on, give it an explicit pattern.

**Patterns match JACK names, not PipeWire node names.** PipeWire calls a device something like `alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo`, but its JACK layer names the ports after the description: `Yeti Stereo Microphone Analog Stereo:capture_FL`. When two JACK clients would get the same name (a device's microphone and headphone sides can share a description), one of them gets `-NN` appended, PipeWire's number for it. That number can change when the device comes back or the computer restarts: a `^Name-NN:` line (as the menus write it) that no longer matches still connects the client now called `Name` or `Name-<other number>` on that side, if exactly one fits. A client that another `^Name:` line names is left to that line. `bananasplit --list-ports` prints every name with a ready-made line:

```
For a strip, [strip1] … [strip6] (ports that send audio):
  Razer Kiyo Analog Stereo                capture_FL capture_FR
      physical · PipeWire node alsa_input.usb-Razer_Kiyo-02.analog-stereo
      connect = ^Razer Kiyo Analog Stereo:
  Yeti Stereo Microphone Analog Stereo    capture_FL capture_FR
      physical · default input · PipeWire node alsa_input.usb-Blue_Yeti-00.analog-stereo
      connect = ^Yeti Stereo Microphone Analog Stereo:
…
@default  → strips: "Yeti Stereo Microphone Analog Stereo"
          → buses:  "Yeti Stereo Microphone Analog Stereo-63"
```

When a pattern matches nothing, the message says why if it can: `that is the PipeWire node name; JACK calls this device "…": use connect = ^…:`, `names are case-sensitive: did you mean "…"?`, or that the matching ports point the wrong way. In `--headless` and `--gui` mode the ports that side could use are also listed on stderr.

When ports appear or disappear (hot-plugged headsets, apps starting), BananaSplit re-applies the configured connections automatically. It never removes connections on its own, including ones you made yourself; only unticking something in a channel's menu (or Disconnect all) does, and a `@default` (or `@physical`) line lets go of the device it connected when the desktop's default moves to another one.

## Latency

`bananasplit --low-latency` writes `contrib/pipewire`'s two files (built into the binary) to `~/.config/pipewire/pipewire.conf.d/` and `pipewire-pulse.conf.d/`. It also applies the graph settings to the running PipeWire with `pw-metadata`, so 128-frame periods start immediately, without a restart. The smaller default buffers for browsers and Discord apply the next time pipewire-pulse starts (`systemctl --user restart pipewire pipewire-pulse wireplumber`, which briefly interrupts all audio). A file you have changed is never overwritten. `bananasplit --low-latency off` removes the files again (only if they are unchanged) and drops the live settings.

About 1.5 s after starting, once JACK reports the latency of the connected devices, BananaSplit checks the round trip. If it is 10 ms or more, it prints one line saying what to do. In the terminal UI that line has its own row.

| Message | Meaning |
|---|---|
| `… (goal < 10 ms): run bananasplit --low-latency` | The low-latency settings are not installed. |
| `1024-frame periods despite the low-latency config: restart PipeWire` | Installed but not active. |
| `… the sound card adds N ms (see README)` | The period is small already. The rest is the device's own buffering, typical for USB audio. You can lower it per device with a WirePlumber rule, see below. Test it, because some devices crackle with less headroom. |
| `…: start jackd with -p 128` | Plain JACK2, not PipeWire. |

Per-device ALSA buffering (WirePlumber 0.5), in `~/.config/wireplumber/wireplumber.conf.d/51-bananasplit-usb.conf`:

```
monitor.alsa.rules = [
  { matches = [ { node.name = "~alsa_.*usb-Blue.*" } ]
    actions = { update-props = { api.alsa.period-size = 128, api.alsa.headroom = 0 } } }
]
```

## If BananaSplit is killed

On a normal exit (Quit in the app's menu or tray, `bananasplit --quit`, `q` in the terminal UI, Ctrl-C, `systemctl stop`) BananaSplit removes its virtual cables (eight by default), and PipeWire moves any app that was playing into them back to the default output. If BananaSplit is killed with SIGKILL or crashes, a small watchdog process it started (`bananasplit --reap-cables bananasplit` in `ps`) sees that BananaSplit is gone and removes the cables within a fraction of a second. If the watchdog was killed too, the next `bananasplit` start removes the leftovers, or you can run `bananasplit --cleanup` now. `--cleanup` refuses while that BananaSplit is still running. The systemd unit runs `bananasplit --cleanup` as `ExecStopPost`. All of these match cables by their exact names, so nothing else is touched.

If a cable disappears while BananaSplit runs (pipewire-pulse was restarted, or someone ran `pactl unload-module`), BananaSplit makes it again at once and says so. If someone removes it again within a minute (a restart of pipewire-pulse does not count), BananaSplit leaves it off; switch it on in the channel's menu.

Cable names come from `client_name` in lower case, with `-` as `_` (`bananasplit_in1` …). Two BananaSplits whose names differ only in that way cannot both have cables: the second one to start makes none and says so.

## Tests

```sh
cargo test                       # 225 unit tests, 4 "audio path never allocates" tests, MP3 against libmpg123
tests/e2e/run_e2e.sh             # runs the real binary against a mock libjack (needs gcc, curl and python3)
node tests/gui/ui_check.js       # clicks every control of the app in Chromium (needs Node.js + Playwright)
node tests/gui/fixes_check.js    # the 2.7.1 fixes that show in the page, in Chromium
tests/window/run_window_test.sh  # the app window and the tray icon on Xvfb (needs GTK 3, Xvfb, dbus-python)
```

The end-to-end test drives BananaSplit's real process callback through a mock JACK server. It checks that impulses fed to two different strips come out on A1 and B1 at the **exact same frame and cycle** they went in, with the correct mix values. It also checks that every output buffer is fully written each cycle, that 8 cables are created, wired and removed, that a stale cable from a crash is cleaned up, and that SIGINT/SIGTERM shut down cleanly. A third phase starts `bananasplit --gui`, drives it over HTTP exactly like the page does (including the security checks and a second `--gui` launch), records strip 1 and A1 in multi mode, and checks that both WAV files hold the impulse on the **same sample** with the correct values.

New in 2.8.4: MP3. Unit tests encode and decode every MP3 rate at 128 and 320 kbit/s (the same length, level and timing come back), check the LAME tag field by field and the standard frame lengths, seek to the exact sample anywhere, read files after ID3v2 and before ID3v1 and APE tags or without an Info frame, survive damaged files, give attacks short blocks (with as little pre-echo as LAME 3.100), and check that the Huffman tables are complete prefix codes. Recorder tests split MP3 takes into parts that play back to back as cleanly as anywhere else, convert a 96 kHz take to 48 kHz, follow a rate change, and record the engine's impulses to the same sample of every MP3 track; config tests cover the old `format = wav24` becoming MP3. `tests/mp3_libmpg123.rs` plays BananaSplit's files with libmpg123 (when it is installed) and needs the same length and samples. The eighth end-to-end phase runs its three cases in MP3 too. While it was written, the encoder was also checked against ffmpeg's decoder and against LAME 3.100 on recordings, and the decoder against the ISO/IEC 11172-4 conformance streams (within half an LSB of their reference output, intensity stereo and mixed blocks included).

A fourth phase covers the 2.3 fixes on a mock of the reviewed setup: a webcam listed before a USB mic, the mic's headphone side named `…-63`, and a 17.3 ms round trip. It checks that:

* `--no-cables` leaves `create_cables = true` in the saved config;
* `@physical` and `@default` take the mic, not the webcam, and never a monitor port;
* each kind of pattern miss gets the right hint, and the available ports are listed;
* the latency advice appears;
* `--list-ports` prints the right lines;
* `--low-latency` installs, applies and undoes;
* after `kill -9` the watchdog removes all 8 cables and nothing else;
* `--cleanup` refuses while BananaSplit runs and sweeps afterwards.

The GUI phase also sets an EQ and VBAN streams over HTTP, checks that bad values are refused, and that everything is saved. A fifth phase tests VBAN and the EQ with a VBAN peer written separately, from VB-Audio's specification only (`tests/e2e/vban_peer.py`, Python standard library). The peer plays Voicemeeter: it sends a 1 kHz sine at 44.1 kHz, 24-bit, in bursts of 512 samples, mixed with packets BananaSplit must ignore (VBAN-TEXT, an unknown stream name, a wrong size, a truncated header). BananaSplit adds it to strip 4, whose EQ boosts 1 kHz by 6 dB, and sends strip 4 back at 48 kHz, 16-bit, while the mock JACK server runs in real time. The peer checks every header field, the frame counter, the source port, and that the audio is a clean 1 kHz sine at the expected level in every 100 ms (no dropout or glitch), with left and right in the right order.

Unit tests also cover the VBAN header against the specification (every sample rate and format, every rejection rule, random garbage), the resampler (44.1 ↔ 48 kHz, 8 to 192 kHz, aliasing below −85 dB), the drift tracking (simulated senders whose clocks run up to 2000 ppm off, with jitter, lost packets, stalls and pauses), two VBAN instances talking over real sockets, the EQ against the Audio EQ Cookbook formulas at three sample rates, and the EQ's click-free changes.

The no-allocation tests run thousands of cycles with parameters changing underneath: one with the recorder recording (both modes, live link changes, a full buffer), one with every EQ changing and VBAN streams flowing both ways (underruns, a full outgoing buffer, flushes), and one with the player playing, pausing, seeking and changing buses while input gain and pan move. A counting allocator proves the audio path never touches the heap.

New in 2.8.0: the updater. An eleventh end-to-end phase (`tests/e2e/update.py`) runs BananaSplit from a folder of its own against a stand-in for GitHub on this computer (`BANANASPLIT_UPDATE_SOURCE`, which only accepts `http://127.0.0.1:PORT` or `http://localhost:PORT`, and from which a .deb is never installed): with automatic updates off it asks nothing; Check now finds the newest published release (not a draft, a pre-release or an older one) and its notes; a download with one byte changed is refused and nothing is touched; Install replaces the program (only after the new one ran and said its version) and empties the downloads folder; Restart now quits cleanly and starts the new version with the same options, which waits for the old one; automatic updates install by themselves without restarting, and the switch is saved at once; GitHub's rate limit is a message; a folder it cannot write to offers no install; and `bananasplit --update` installs, finds nothing newer, and refuses a tampered file. Unit tests cover the JSON reader, SHA-256 (the standard test vectors), version order, picking the release from GitHub's real answer, links outside the repository, checksum files, downloads through curl (errors, size limits, HTTPS only), unpacking, replacing the program, and what is cleaned up after an update. `tests/gui/ui_check.js` opens the Updates window, checks a new version and switches automatic updates on and off; the Windows launch test installs a new version over the running `bananasplit.exe`, reopens the window from the moved program, and restarts.

New in 2.7.3: every fix has a check that fails on 2.7.1. Unit tests: the recordings scan with names in other scripts, take order (`-10` after `-2`, the night summer time ends, folders copied without their file times), a channel called "part2" through a 4 GB split, two takes in one second, a take that grew while selected, a VBAN stream switched off while playing, two senders of one stream, senders that restart or do not number packets, the dropout count, the README's example config, comments after values, saving through a symlink, byte order marks, and the owner of the other end of a local connection. The seventh end-to-end phase adds Start with the system after Add to the app menu; `tests/gui/fixes_check.js` takes every event stream the server allows and checks that the page connects once one is free.

New in 2.7.1:

* A tenth end-to-end phase (`tests/e2e/fixes.py`) has a check for each fix, and each check fails on 2.7.0. It changes the default output and input with the fake `pactl` and checks that A1 and strip 1 move at once and let go of the old devices, that a device ticked in the menu stays, and that A1 stays put while the default is strip 3's cable. It checks that apps are listed once, that a headset's mono mic reaches both sides of a strip, and that `pactl subscribe` inherits no descriptor libjack left open. It unloads a cable's module behind BananaSplit's back (and gives its number to another module, as pipewire-pulse does), unloads it twice, restarts pipewire-pulse twice within a minute (all modules gone, `pactl subscribe` ended), and once more while it cannot make cables yet. It unplugs the device a channel stays with and plugs it in again, unticks it, and checks `@physical` too. It runs `--headless` with a `pactl` that reaches no server and with one whose `subscribe` keeps ending, and checks that nothing piles up. It starts a second BananaSplit named `bananasplit` next to `BananaSplit`, loads `^Name-71:` lines while PipeWire calls the device `Name-63` (also with two microphones of the same model), swaps which side of the Yeti gets the number, switches on Default input with a broken `pactl`, renames strips and buses, records two tracks across a change of sample rate, and shuts the audio server down with unsaved changes. The fake `pactl` learned `set-default-sink`/`-source`, pipewire-pulse's reuse of module numbers and its restarts; the mock JACK server learned a headset, twin microphones, devices unplugged while running, a server shutdown and a descriptor left open.
* `tests/gui/fixes_check.js` checks in Chromium that a long status keeps to three lines at four window sizes and zooms, so the strips end above the bottom bar; that a channel button's tooltip follows changes made in its menu; and that the REC title still says "2 tracks" when the take continues in new files.
* Unit tests cover the menus' `^Name:` lines and PipeWire's numbers, the default input never being a monitor, which apps are streams under their JACK names, monitor ports only when nothing else matches, cable names shared by two client names, streams named by their description, the files a take is writing, and the messages about cables that came back.

New in 2.7:

* An eighth end-to-end phase records a 1 kHz tone the way PipeWire's libjack behaves: it says 48 kHz until the mixer has run once, while the graph runs at 44.1 kHz. Every file must say 44.1 kHz, the tone must read 1000 Hz, the files must hold as much time as went by, and the player must run at 1.0x. It also changes the rate from 48 to 96 kHz during a take (the take must continue in a second file at 96 kHz, and the GUI must say why), and runs with a libjack that never calls the sample-rate callback. 2.6 fails the first part (its file plays at about 1.09x).
* A ninth phase works the channels' menus over HTTP like the page does, with the fake `pactl` playing PipeWire (devices, and Discord, Firefox and OBS as apps): A1's virtual microphone switched on and off while running, an app moved into strip 3 and back, OBS given A1's mix (A1's cable comes on for it), PipeWire moving OBS back to the default microphone when that cable goes, devices connected, saved as `connect =` lines and disconnected, refusals, and every cable removed on quit.
* `tests/gui/ui_check.js` now also opens the channel menus and connects and disconnects with them, checks that a muted strip's meter shows its input in grey, sends a bus into another one and checks that loops are blocked in the page and refused by the server, the five outputs in the bottom bar, the VBAN window's IP addresses, that the console fills windows from 1024 × 640 to 3440 × 1440 in one row of equal strips without scrolling (and scrolls when smaller), and long names (131 checks).
* Unit tests cover the bus sends (same-cycle order, ramps, loops that never form even when two changes race), the input meters of muted and soloed-out channels, the engine following the server's sample rate, the recorder continuing a take in new files at a new rate, cables on any channel, the menus' labels, the apps' streams as `pactl` lists them, the local IP addresses, and the new config keys.

New in 2.6:

* Phase 3 also checks that requests from another user of the computer are refused (when the test runs as root, it acts as `nobody`; otherwise it prints a SKIP line).
* A sixth end-to-end phase gives BananaSplit a system libjack that finds no JACK server and a second libjack in PipeWire's private folder: without PipeWire running it reports the error as before; with PipeWire running it opens PipeWire's libjack, the mixer runs, and JACK2's error messages stay hidden.
* A seventh phase keeps port 8790 busy: BananaSplit moves to another port and notes it, a second launch and `--quit` find it there (and `--quit` for another client name leaves it alone), and after the program file is replaced while it runs (as an update does), "Start with the system" names the new file.
* `tests/gui/ui_check.js` starts BananaSplit on the mock JACK server (in real time) and works the page in Chromium like a person: it drags faders and knobs, double-clicks, types, scrolls, renames a strip, switches MUTE, SOLO, MONO and routing, edits an EQ band by dragging it, arms tracks, records a take, plays it back and checks that it reaches A1, pauses, plays from a click into the waveform and jumps while playing, saves and loads a scene, switches the theme and zoom, uses the keyboard shortcuts, and checks after every action that the mixer's state changed the way the control says (83 checks).
* `tests/window/run_window_test.sh` runs the real app window (GTK 3 on Xvfb) with a tray written from the StatusNotifierItem and dbusmenu specifications: the window opens with the page, the tray menu starts and stops recording, closing hides to the tray and the mixer keeps running, the app-menu launch and the tray bring it back, a crashed window leaves the mixer running, Quit stops everything and saves the window size; without a tray closing minimizes, `close_to_tray = false` quits, without WebKitGTK the page opens in a browser, `--hidden` starts in the tray, a start-up error launched from the app menu appears in a dialog, the Auto theme follows the desktop's dark style live (from a settings portal written from its specification), and a window restarted while recording, after an update replaced the program file, comes from the running program (not the new file, not a browser) and shows the recording in the tray.
* Unit tests cover the input gain and pan (centre and 0 dB are bit-exact), the WAV reader (every format, broken and still-growing files), playback (bit-exact at the same rate, a 44.1 kHz sine played at 48 kHz, fades on stop and seek and before running dry, sample rates it refuses), who may connect, scenes (names, strict parsing, round trips), the desktop entries (Exec quoting, TryExec, the icons), the tray's D-Bus menu, and the new `[gui]` settings. `packaging/build-packages.sh` checks the packages it builds: the .deb's contents and control file, the desktop entry, and the portable package's install and uninstall in a throw-away home folder (which also check that files BananaSplit did not write are never touched).

## Limitations

* Not included from Voicemeeter: Intellipan, the compressor/gate, macro buttons, and VBAN's other parts: remote control (VBAN-TEXT), MIDI/serial, the service ping that lists VBAN devices, and the VB-Audio codecs (VBCA/VBCV). BananaSplit sends and receives uncompressed PCM audio only, the format Voicemeeter uses by default.
* VBAN uses IPv4 only. An incoming stream may have at most four times BananaSplit's sample rate.
* VBAN and the EQ were tested against a VBAN implementation written from the specification, not against Voicemeeter itself. Check a stream in both directions on your own network before relying on it live.
* **The app window was tested with GTK 3 on an X server and a stand-in for WebKitGTK, and the page itself in Chromium**, not on a real desktop with WebKitGTK; the tray was tested against a tray written from the specifications, not against KDE's, GNOME's or Xfce's own. Check the window, the tray and Start with the system on your desktop before relying on them live. WebKitGTK 2.40 or newer draws every detail (some lit buttons lose their tint on older versions).
* On GNOME the tray icon needs the AppIndicator extension (Ubuntu ships it; on Fedora install `gnome-shell-extension-appindicator`). Without it, closing the window minimizes it.
* On Wayland the window manager decides where the window opens; BananaSplit only sets its size.
* The page in a browser needs a current one (Chromium/Chrome 111+, Firefox 113+); the browser app-window mode needs a Chromium-based one.
* Only an x86-64 (amd64) build is packaged; on ARM, build from source.
* **App-side buffering is not under any mixer's control.** PulseAudio-API apps (browsers, Discord) choose their own buffer sizes. The `pipewire-pulse` snippet lowers the default, but an app that asks for 40 ms gets 40 ms. Native PipeWire and JACK apps follow the graph quantum.
* With **two different sound cards** on A1/A2, PipeWire clocks one and adaptively resamples the other, which is the correct way to handle two clock domains. Plain JACK2 can only drive one card.
* The 2.3 changes were tested against a mock JACK server and a fake `pactl`/`pw-metadata` that print what PipeWire prints. Check `bananasplit --list-ports` and `bananasplit --low-latency` on your own machine before relying on them live.
* The same goes for 2.7's channel menus, the virtual microphones of A1–A3 and moving apps between channels: they were tested against the mock JACK server and the fake `pactl`, not a real PipeWire. The sample-rate fix was tested with a mock libjack that behaves as PipeWire's did here; record a minute and check its length on your own system once.
* The 2.7.1 fixes were tested the same way, against the mock JACK server and the fake `pactl`; how PipeWire names JACK clients and reuses module numbers was checked in PipeWire's source code, not on a running system.
* An app moved into a strip is moved the way pavucontrol does it. Some apps pick their output again themselves when they restart (or follow the default output); pick the strip in their own settings then.
