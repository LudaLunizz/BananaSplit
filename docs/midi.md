# MIDI controllers

BananaSplit can be driven by a hardware MIDI controller: its faders, knobs, endless encoders and buttons move the mixer's values, and its LEDs and motor faders follow what changes in the mixer. This page describes how that works, on Linux, Windows and macOS alike.

> [!NOTE]
> MIDI is new (2.9.0). It has been tested with a simulated device (the tests feed MIDI bytes to the code and check what it does and what it sends back), not yet with a real controller. If you use one, please report what works and what doesn't.

MIDI runs while BananaSplit runs as the app (`--gui`, also with `--no-open` or `--hidden`). The terminal UI and `--headless` have no MIDI.

## Quick start

1. Plug in the controller and open **MENU ▸ MIDI controllers…**.
2. Under **From (input)** pick the port the controller sends on. If it has LEDs or motor faders, pick the port it listens on under **To (LEDs, motor faders)**.
3. Click **Learn a control** and move one fader, knob or button on the controller.
4. Choose what it drives and how it reads (the mode; BananaSplit suggests one), then click **Bind**.

The **Last message** line shows the last message the controller sent, which helps to see whether anything arrives at all.

## What a binding is

A binding joins one control of the controller (its *source*) to one value of the mixer (its *target*), with a *mode* that says how the control's messages are read. It also has two switches: **Pickup** (soft-takeover) and **LED / motor** (feedback).

A control has at most one binding: binding it again replaces the old binding. One value may have several controls bound to it. There are at most **256** bindings.

### Sources

A source is written as text, with the MIDI channel as 1 to 16:

| Source | Text | What it is |
| --- | --- | --- |
| Control change | `cc:CH:N` | CC number N (0 to 127): a fader, a knob, an encoder or a button. |
| 14-bit control change | `cc14:CH:N` | A high-resolution fader: its coarse part on CC N (0 to 31), its fine part on CC N+32. |
| Note | `note:CH:N` | Note N (0 to 127): a pad or a button that sends notes. |
| Pitch bend | `pb:CH` | The 14-bit pitch bend of a channel: the faders of Mackie Control style surfaces. |
| NRPN | `nrpn:CH:N` | Non-registered parameter N (0 to 16383): addressed with CC 99 and 98, its value sent with CC 6 and 38. |
| Program change | `pc:CH:N` | Program N (0 to 127): a button that sends a program change. |

As the MIDI specification says, a new coarse value (CC 0 to 31, or a new NRPN address) sets the fine part back to 0. CC 101 and 100 (an RPN) end NRPN addressing. Aftertouch, channel pressure, clock, active sensing, SysEx and other system messages are ignored.

### Targets

A target is a path of [API v1](api.md), or a scene:

| Target | What it drives |
| --- | --- |
| `/strips/N/gain_db`, `/buses/ID/gain_db` | The fader, −60 to +12 dB |
| `/strips/N/trim_db`, `/buses/ID/trim_db` | The GAIN knob (input gain), −24 to +24 dB |
| `/strips/N/pan`, `/buses/ID/pan` | Pan (balance on a bus), −1 to 1 |
| `/buses/ID/limit_db` | The bus limiter, −40 to +12 dB (the top is off) |
| `/strips/N/mute`, `/buses/ID/mute`, `.../mono`, `.../eq`, `.../record` | On or off: mute, mono, the EQ, the link to the recorder |
| `/strips/N/solo`, `/strips/N/gate`, `/strips/N/comp` | On or off, strips only: solo, the gate, the compressor |
| `/strips/N/sends/BUS`, `/buses/ID/sends/BUS` | On or off: a strip's route to a bus, a bus's send to another bus |
| `.../eq/B/on` (B = band 1 to 6) | On or off: one EQ band |
| `.../eq/B/freq_hz`, `.../eq/B/q` | A band's frequency (20 to 20000 Hz) and Q (0.1 to 30), moved on a log scale |
| `.../eq/B/gain_db` | A band's gain, −24 to +24 dB |
| `/strips/N/gate/threshold_db`, `range_db`, `attack_ms`, `hold_ms`, `release_ms` | The gate's settings |
| `/strips/N/comp/threshold_db`, `ratio`, `knee_db`, `attack_ms`, `release_ms`, `makeup_db` | The compressor's settings |
| `/recorder/recording` | On or off: recording |
| `/player/playing` | On or off: play or pause the player |
| `/player/sends/BUS` | On or off: the player plays into that bus |
| `/vban/on`, `/vban/in/K/on`, `/vban/out/K/on` (K = 1 to 8) | On or off: VBAN as a whole, one incoming or outgoing stream |
| `scene:<name>` | Load the saved scene of that name |

Strips are numbered 1 to 6; buses go by name, `V1` to `B2` (the old names `A1` to `A3` are read too). `GET /api/v1/paths` lists every path; [`api.md`](api.md) gives each one's range.

Paths whose value is text or a choice from a list (a band's type, a stream's name or address, the player's take or state), and numbers that only jump rather than glide (VBAN ports, a stream's strip and buffer, the player's position), cannot be bound to a MIDI control: binding one is refused with "cannot be driven by a MIDI control: set it through the API". A binding drives a value exactly as the window and the API do, through the same commands, with the same click-free ramps.

A fader bound to `gain_db` follows the window's fader scale, which gives more travel around 0 dB: the bottom is −60 dB, a quarter of the way about −28 dB, halfway about −14 dB, 80 % is 0 dB and the top +12 dB. A band's frequency and Q are spread on a log scale; the other numbers are spread evenly over the travel.

### Modes

| Mode | Text | How the messages are read |
| --- | --- | --- |
| Absolute | `abs` | The control's position is the value. On an on/off target: the upper half is on. |
| Relative, two's complement | `rel2` | Endless encoders: 1 to 63 are steps up, 127 down to 65 are steps down. |
| Relative, around 64 | `rel64` | 65 and up are steps up, 63 and down are steps down. |
| Relative, sign bit | `relsign` | 1 to 63 are steps up, 65 to 127 are steps down. |
| Toggle | `toggle` | Each press switches the value on or off. |
| Momentary | `momentary` | The value is on while the button is held. |
| Trigger | `trigger` | Each press does it once: loads the scene. |

One step of a relative encoder moves the value by 1 % of its travel. A CC used as a button counts as pressed at 64 and above. A 14-bit fader, a pitch bend fader or an NRPN used as a button counts as pressed past halfway.

Not every mode fits every source and target. The first mode in each list is the one used when none is chosen:

| Source | On a number (fader, gain, pan, limit, EQ band, gate or compressor setting) | On an on/off value | On a scene |
| --- | --- | --- | --- |
| `cc` | `abs`, `rel2`, `rel64`, `relsign` | `toggle`, `abs`, `momentary` | `trigger` |
| `cc14`, `pb`, `nrpn` | `abs` | `toggle`, `abs`, `momentary` | `trigger` |
| `note` | none: a button can't set a number | `toggle`, `momentary` | `trigger` |
| `pc` | none: a button can't set a number | `toggle` | `trigger` |

## Learning a control

**Learn a control** catches the next control you move. Nothing in the mixer changes while learning.

* The first message counts, except a button's release and NRPN addressing on its own.
* For 600 ms after it, BananaSplit keeps listening to tell what kind of control it is:
  * a CC N (0 to 31) followed by CC N+32 is a 14-bit fader (`cc14`, `abs`);
  * small steps up (1, 2, ...) or down (127, 126, ...) on one CC are an encoder (`rel2`);
  * the same small step around 64 again and again is an encoder (`rel64`);
  * an NRPN is caught as `nrpn` (`abs`), pitch bend as `pb` (`abs`).
* Anything else is offered as a plain CC, note or program change. BananaSplit never guesses `relsign`: choose it yourself if the encoder sends that.
* Learning stops by itself after 60 seconds, when you bind, when you click **Learning…** or **Cancel**, or when you close the MIDI controllers window.

Turn an encoder or move a fader a little while learning, so BananaSplit sees more than one message.

## Pickup (soft-takeover)

A fader or knob on the controller is often somewhere else than the value it drives, because the value was moved in the window, the deck, the API or by a scene. With **Pickup** on, such a control does nothing until it reaches the value, so the value never jumps. It picks the value up when:

* it comes within 3/127 of the travel (about 2.4 %) of the value, or
* it crosses the value between two of its messages, or
* this binding set the value last and nothing else moved it since.

Pickup is on by default for absolute controls on numbers, and off for pitch bend (pitch bend is used by motor faders, which are moved to the value anyway). It has no effect on relative encoders and buttons.

## LEDs and motor faders (feedback)

With **LED / motor** on and an output port chosen, BananaSplit sends the value back to the control whenever it changes, from any source: the window, the deck, the API, a scene or another control. It looks for changes every 30 ms.

* A CC gets its value (0 to 127), a 14-bit CC its two halves, pitch bend its 14-bit value, an NRPN its address and value (CC 99, 98, 6, 38).
* A note gets Note On with velocity 127 when the value is on (or past halfway), and Note On with velocity 0 when it is off. BananaSplit sends velocity 0 instead of Note Off because some controllers' LEDs ignore Note Off.
* **Echo guard:** a binding that received a message in the last 250 ms is not sent its value, so a motor fader isn't pulled while you move it. A value a control sets itself is not sent back to that control; other controls bound to the same value do get it.
* When the output port is opened (also again after the controller came back), every bound value is sent, so the LEDs and faders show the mixer's state at once.
* If a send fails, the port is closed and opened again at the next port check, and every value is sent again.

Feedback is on by default, except for scene bindings and program changes (they have no value to show).

## Ports

BananaSplit opens one input port and one output port, chosen by name. It lists the system's ports every 2 seconds, so a controller plugged in later, or unplugged and plugged in again, is opened by itself.

| System | MIDI backend | Notes |
| --- | --- | --- |
| Linux | ALSA sequencer (`libasound`, loaded when MIDI starts) | BananaSplit is the sequencer client "BananaSplit". PipeWire and JACK bridge their MIDI into the sequencer, so their ports show up too. |
| Windows | WinMM (`winmm.dll`, part of Windows) | Windows 10 lets only one program open a MIDI port at a time; Windows 11 with Windows MIDI Services (24H2 and later) lets several. A port in use says so. Two ports with the same name are listed as "Name" and "Name (2)". |
| macOS | CoreMIDI | BananaSplit is the MIDI client "BananaSplit". Ports have the names Audio MIDI Setup shows. |

Messages are passed from the system's MIDI thread to BananaSplit through a buffer of 1024 messages. If a controller sends faster than they are handled, the extra messages are dropped and counted: the MIDI window shows them, and the state has them as `midi.lost` (see [`api.md`](api.md)).

## midi.json

The ports and the bindings are kept in `midi.json`, next to the config:

| System | File |
| --- | --- |
| Linux | `~/.config/bananasplit/midi.json` (Flatpak: `~/.var/app/io.github.LudaLunizz.BananaSplit/config/bananasplit/midi.json`) |
| Windows | `%APPDATA%\bananasplit\midi.json` |
| macOS | `~/Library/Application Support/bananasplit/midi.json` |

It is written at every change in the MIDI window or through the API (to `midi.json.tmp`, then renamed over the old file). It looks like this:

```json
{
  "in": "nanoKONTROL2 MIDI 1",
  "out": "nanoKONTROL2 MIDI 1",
  "bindings": [
    {"src": "cc:1:0", "target": "/strips/1/gain_db", "mode": "abs", "takeover": true, "feedback": true},
    {"src": "cc:1:16", "target": "/strips/1/pan", "mode": "rel2", "takeover": false, "feedback": true},
    {"src": "note:1:48", "target": "/strips/1/mute", "mode": "toggle", "takeover": false, "feedback": true},
    {"src": "pc:1:3", "target": "scene:Live Room", "mode": "trigger", "takeover": false, "feedback": false}
  ]
}
```

* `in` and `out` are the port names (empty: none).
* Each binding has `src`, `target` and `mode` as described above. `takeover` and `feedback` may be left out; they then get their defaults.
* A binding that is not valid (an unknown path, a path MIDI cannot drive, a mode that doesn't fit) is skipped with a message on the terminal; the others load. Only the first 256 are read.
* A file that isn't valid JSON loads no bindings at all, and the next change in the MIDI window writes a new file without them. Keep a copy before editing it by hand.

Scripts can manage the same bindings with the `midiport`, `midi`, `midibind` and `midiunbind` commands of `POST /api/cmd`; see [`api.md`](api.md).
