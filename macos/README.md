# BananaSplit for macOS

The macOS port of BananaSplit, built on Core Audio, with its own app window (the system's WKWebView) and a menu bar icon.

The mixer core is shared with the [Linux app](../linux/README.md) and the [Windows app](../windows/README.md). See the Linux manual for the mixer, EQ, VBAN, recorder and scene features.

> [!NOTE]
> The macOS version is new. Please report anything that doesn't work as described here.

## Requirements

- macOS 11 (Big Sur) or newer, on Apple silicon or Intel, with Safari 15.4 or newer: the app's window is drawn by Safari. macOS 12.3 and newer come with it; on older versions, Software Update installs it.
- Optional, for per-channel virtual cables: [BlackHole](https://github.com/ExistentialAudio/BlackHole) (free), or VB-Audio's VB-Cable, Rogue Amoeba's Loopback

## Installing

1. Unzip `bananasplit-<version>-macos.zip` and drag **BananaSplit** into your **Applications** folder.
2. Open it. BananaSplit is not signed with an Apple Developer ID yet, so the first time macOS refuses to open it: go to **System Settings › Privacy & Security** (macOS 11 and 12: **System Preferences › Security & Privacy**), find the message about BananaSplit and click **Open Anyway**, then open it again. (In the Terminal, `xattr -dr com.apple.quarantine /Applications/BananaSplit.app` does the same.)
3. macOS asks whether BananaSplit may use the **microphone**: allow it. Without that every input is silent, the virtual cables' too. When you first use VBAN, macOS 15 and newer also ask about the **local network**: allow that for VBAN to reach other computers.

Both can be changed later in **System Settings › Privacy & Security** (Microphone, Local Network; macOS 11 and 12: System Preferences › Security & Privacy › Privacy › Microphone). After allowing the microphone there, quit and start BananaSplit again.

## Running

BananaSplit opens in its own window, with a Dock icon and a menu bar while the window is shown.

- **Menu bar icon.** BananaSplit lives in the menu bar: closing the window (red button or Cmd+W) keeps the mixer running there, and the Dock icon goes away. Click the icon for **Show BananaSplit**, **Start/Stop recording** and **Quit BananaSplit**; it turns red while recording.
- **Quitting.** **Quit BananaSplit** in the icon's menu or the app menu (Cmd+Q), the power button in the app, the Dock's Quit, or `bananasplit --quit`. Turn off **Menu ▸ Closing the window keeps it running** if you want closing the window to quit instead.
- **Opening it again** (from Finder, Launchpad or the Dock) brings up the running mixer's window rather than starting a second one.
- **Settings are saved** when you quit, every two minutes, and when you log out, restart or shut down.
- **Copy and paste** work in the app's text fields (Edit menu, Cmd+C/Cmd+V).

## Devices

- A channel's menu lists the devices macOS shows: microphones, sound cards, headsets, the virtual cables. A strip takes in a device's stereo pair (or its only channel, on both sides); a bus plays on its stereo pair (or, on a mono device, the mix of both). The stereo pair is the one set in Audio MIDI Setup › Configure Speakers: the first two channels unless you changed it.
- BananaSplit runs only the side of a device it uses: playing on a headset doesn't switch its microphone on.
- **Default input** and **Default output** follow what you pick in System Settings › Sound. They never follow macOS into a virtual cable (BlackHole set as the default output, say): the mixer would record itself. `@physical` skips every software device.
- The mixer runs at **48 kHz**. Devices that run at another rate (44.1 kHz, 96 kHz, …) are converted, with a high-quality resampler, so nothing has to be changed in Audio MIDI Setup. Each device runs on its own clock; BananaSplit keeps them in step.
- **Latency.** BananaSplit asks every device it uses for IO cycles of `buffer_size` frames (`[global] buffer_size = 128`, about 2.7 ms at 48 kHz), within what the device allows. This is BananaSplit's own setting: other apps keep theirs. `buffer_size = 0` leaves the devices as they are. Bluetooth devices allow only large cycles, and add latency of their own.
- In the config file, `connect =` takes `@default`, `@physical` or part of a device's name as macOS shows it: `bananasplit --list-devices` lists them.
- Devices that come and go (USB, Bluetooth, a changed default device or rate) are picked up within a few seconds.

macOS has no per-app stream routing like PipeWire: pick a channel's cable as the output (or microphone) in the app itself.

## Virtual cables

A virtual cable is an audio device that exists only in software: what an app plays into it comes out of its input. On a strip it is an output device for apps (a game or Discord plays into it, and its sound has a strip of its own); on a bus it is a microphone (Discord, Zoom or OBS record it, and hear that bus's mix).

- BananaSplit uses **BlackHole** ("BlackHole 2ch", "BlackHole 16ch", "BlackHole 64ch": three cables), VB-Audio's **VB-Cable**, Rogue Amoeba's **Loopback** (devices called "Loopback Audio …") and the older **Soundflower**. It never installs a driver itself.
- Install BlackHole from its page, or with Homebrew: `brew install blackhole-2ch` (and `blackhole-16ch`, `blackhole-64ch` for more cables).
- **Each cable serves one channel** whose cable is on (every strip, B1 and B2 to begin with): strips first, then buses. A channel keeps the cable it was given, also when other channels' cables are switched on or off, after a restart, or while a cable is gone for a moment; BananaSplit remembers which in `cables.txt` next to the config. The channel's menu shows its cable's name: pick that device in the app.
- If a bus plays into a cable that a strip records, the status line warns that the strip mustn't go to that bus.
- **Menu ▸ Virtual cables…** explains them, shows which are installed and the current default devices, and opens BlackHole's page, the sound settings and Audio MIDI Setup. On the first start without cables it opens by itself, once.
- **Default devices:** BananaSplit never changes them, but a newly installed cable can become the default output or input. Check them in the Virtual cables window or System Settings › Sound after installing.
- **Quitting** BananaSplit leaves the cables installed; they carry no sound until it starts again.

## Updates

**Menu ▸ Check for updates…** looks for the newest BananaSplit release on GitHub that has a macOS app (`bananasplit-<version>-macos.zip`), is published and not marked pre-release, and shows its notes. A macOS app is replaced as a whole, so BananaSplit doesn't install it itself: **Release page** opens the download, and the new app goes in place of the old one in Applications. With **Check for updates automatically** on, BananaSplit checks a minute after it starts and once a day, and says when there is a new version. The new app isn't signed with an Apple Developer ID either: open it the first time as under [Installing](#installing), and allow the microphone again if macOS asks.

## Command line

The program is inside the app: `/Applications/BananaSplit.app/Contents/MacOS/bananasplit`.

| Option | Description |
| --- | --- |
| `--gui` | Run the app with its window (default) |
| `--headless` | Run the audio engine without any UI |
| `--no-open`, `--hidden` | Start in the background: only the menu bar icon shows |
| `--gui-port PORT` | Use a specific local port (default `8790`) |
| `--config FILE` | Use a custom config file |
| `--list-devices` | List the audio devices |
| `--print-default-config` | Print the default config and exit |
| `--update` | Look for a new version on GitHub |
| `--quit` | Quit a running instance |
| `-V`, `--version` | Print the version |

Started from the Terminal, BananaSplit uses the Terminal's microphone permission, not the app's.

## Files

| What | Where |
| --- | --- |
| Config | `~/Library/Application Support/bananasplit/bananasplit.ini` |
| Scenes | `~/Library/Application Support/bananasplit/scenes/` |
| Each channel's cable | `~/Library/Application Support/bananasplit/cables.txt` |
| One BananaSplit per config | `~/Library/Application Support/bananasplit/bananasplit.lock` |
| Recordings | `~/Music/BananaSplit/` by default, configurable in the app or config |
| Port file | `~/Library/Caches/bananasplit/<client>.port` |
| Update checks | `~/Library/Caches/bananasplit/update/` (`--update`: `update-cli/`) |
| Start at login | `~/Library/LaunchAgents/io.github.ludalunizz.bananasplit.plist` |

## System integration

**Menu ▸ Start with the system** adds a login item of your own (the LaunchAgent above; macOS says "Background Items Added"). At login BananaSplit then starts quietly in the menu bar; click its icon to open the window. If BananaSplit was moved after you switched it on, switch it on again.

## Building

```sh
cargo build --release        # target/release/bananasplit, for this Mac
sh packaging/bundle.sh       # dist/BananaSplit.app (Apple silicon and Intel) and the release zip
```

Needs Rust 1.85 or newer and Xcode's command line tools (`xcode-select --install`). The app is signed ad hoc; a Developer ID signature and notarization need Apple's developer program.

## Testing

```sh
cargo test                                         # unit and no-allocation tests
sh packaging/bundle.sh && sh tests/launch/smoke.sh dist/BananaSplit.app
                                                   # starts the app, asks it, starts it again and quits it
```

## Notes

- The UI talks to the mixer over a loopback-only local connection. macOS can't say which user a local connection belongs to, so another user logged in to the same Mac at the same time could reach it, as on Linux without `/proc`.
- If a device stops delivering audio, BananaSplit reconnects the devices once by itself; Menu ▸ Reconnect devices does it again. If it is the device the mixer runs on (A1's, normally) and it stops again, the mixer goes on without waiting for it, and that device plays again as soon as it runs.
- Default routing uses `@default` and `@physical`; change it from the channel menus.
