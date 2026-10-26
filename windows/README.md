# BananaSplit for Windows

The Windows port of BananaSplit, built on WASAPI shared mode with a native desktop window powered by the Microsoft Edge WebView2 Runtime.

The mixer core is shared with the [Linux app](../linux/README.md). See that manual for the mixer, EQ, VBAN, recorder and scene features.

## Requirements

- Windows 10 or 11, x64
- [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
- Optional, for per-channel virtual cables: [Splitter](https://github.com/LudaLunizz/Splitter/releases), BananaSplit's own cables (one for every channel), and/or [VB-Audio CABLE](https://vb-audio.com/Cable/) pairs (CABLE, CABLE-A … CABLE-D)

## Running

Extract the release zip, put `bananasplit.exe` in a folder of your own (your Desktop, Documents or `%LOCALAPPDATA%\Programs`) and double-click it. (It runs from Program Files too, but then it cannot update itself.) It is a single, self-contained program: BananaSplit opens in its own window, with no console window and no external browser.

- **Tray icon.** BananaSplit lives in the notification area, like Voicemeeter. Closing the window hides it there and the mixer keeps running. Click the icon to bring the window back; right-click it for **Show**, **Start/Stop recording** and **Quit**. The icon turns red while recording.
- **Quitting.** Use **Quit** in the tray menu, the power button in the app, or `bananasplit.exe --quit`. Turn off **Closing the window keeps it running** in the app menu if you want the close button to quit instead.
- **Starting it again** brings up the running instance's window rather than starting a second mixer.
- **Settings are saved** when you quit, every two minutes, and when you log off, restart or shut down Windows. `taskkill /im bananasplit.exe` (without `/f`) also quits cleanly.
- **No WebView2?** BananaSplit opens the mixer in your web browser instead and offers Microsoft's download page for the WebView2 Runtime.

## Updates

**Menu ▸ Check for updates…** opens the Updates window.

- **Check now** looks for the newest BananaSplit release on GitHub that is published and not marked pre-release, and shows its notes.
- **Install** downloads `bananasplit-<version>-windows-x64.exe` to `%LOCALAPPDATA%\bananasplit\update\` and checks it first: it must come from BananaSplit's release downloads over HTTPS, have the size GitHub lists, and match the release's `SHA256SUMS.txt`, which must be signed with BananaSplit's release key (`SHA256SUMS.txt.sig`), and GitHub's SHA-256 of the file; then it must run and say it is that version. Windows doesn't let a running program be overwritten, so the running `bananasplit.exe` is renamed to `bananasplit.exe.old` and the new one takes its name (and so keeps your Start Menu shortcut and "Start with Windows"). The next start removes the `.old` file.
- **Restart now** saves, quits and starts the new version. While you are recording it asks for a second click.
- **Install updates automatically** is **off** by default. Off, BananaSplit checks only when you click Check now. On, it checks a minute after it starts and once a day, installs a new version by itself once it has been out for two days (Install takes it at once) and says when a restart finishes it; it never restarts on its own.
- In a folder BananaSplit cannot write to (under Program Files, for example), it says so and **Release page** opens the download in your browser. Keep `bananasplit.exe` in a folder of your own (your Desktop, Documents or `%LOCALAPPDATA%\Programs`) to let it update itself.
- `bananasplit.exe --update` checks and installs from a terminal. While BananaSplit is running it refuses (the running app would open its window from the new file): use its menu, or quit it first.
- Downloads use Windows' own `curl.exe` (part of Windows 10 and 11). It uses the `HTTPS_PROXY` variable for a proxy, not the proxy set in Windows' settings.

## Command line

| Option | Description |
| --- | --- |
| `--gui` | Run with the desktop window (default) |
| `--headless` | Run the audio engine without any UI |
| `--no-open`, `--hidden` | Start in the background: only the tray icon shows |
| `--gui-port PORT` | Use a specific local port (default `8790`) |
| `--config FILE` | Use a custom config file |
| `--list-devices` | List WASAPI recording and playback devices |
| `--print-default-config` | Print the default config and exit |
| `--update` | Look for a new version on GitHub and install it |
| `--quit` | Quit a running instance |
| `-V`, `--version` | Print the version |

`bananasplit.exe` is a Windows program, not a console one. Run from cmd or PowerShell, it prints into that terminal, but the terminal doesn't wait for it, so its prompt may come back first. To wait and capture the output, pipe it (`bananasplit --list-devices | more`, or `| Out-String` in PowerShell) or use `start /wait bananasplit --list-devices`.

## Files

| What | Where |
| --- | --- |
| Config | `%APPDATA%\bananasplit\bananasplit.ini` |
| Scenes | `%APPDATA%\bananasplit\scenes\` |
| Recordings | `~/Music/BananaSplit/` by default, configurable in the app or config |
| Port file | `%LOCALAPPDATA%\bananasplit\<client>.port` |
| Window (WebView2) data | `%LOCALAPPDATA%\bananasplit\WebView2\` |
| Downloaded updates (until installed) | `%LOCALAPPDATA%\bananasplit\update\` (`--update`: `update-cli\`) |

## System integration

From the app menu:

- **Install to Start Menu** creates a shortcut.
- **Start with Windows** registers BananaSplit under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. At login it then starts quietly in the tray; click the icon (or start BananaSplit) to open the window.

## Virtual cables

Per-channel virtual cables come from two drivers. Without either, cables are unavailable; everything else works without them.

- **Splitter**, BananaSplit's own cables, installed separately: a cable for every channel, named after it, so switching one channel's cable off leaves the others alone.
  - Strip N: apps play into `Splitter N`, and strip N records `Splitter N Output`. The strip's channel menu shows `Splitter N`.
  - Bus A1 … B2: the bus plays into `Splitter B1 Input` (say), and apps record `Splitter B1` as their microphone. The bus's channel menu shows `Splitter B1`.
- **VB-Audio** cable pairs. They cover the channels whose Splitter cable is missing (all of them, without Splitter), strips first, then buses. Each pair is used by at most one channel. Installing Splitter therefore moves channels off VB-Audio's cables: point apps that used "CABLE Input" or "CABLE Output" at the channel's Splitter cable.

A channel uses its cable while the cable is switched on in its menu: every strip and B1, B2 to begin with (A1 … A3 are speakers and headsets, so `Splitter A1` … `Splitter A3` stay silent until you switch their cables on). `@default` never follows Windows into a cable of either kind, because BananaSplit would then record itself. If a bus plays into a Splitter cable that a strip records, the status line warns that the strip mustn't go to that bus.

**Menu ▸ Virtual cables…** explains them and shows which cables are installed (and which Splitter cables are missing), whether Voicemeeter is, and the current Windows default devices. On the first start without cables it opens by itself, once. It has buttons for the Splitter and VB-Audio download pages and the Windows sound settings.

- **Installing Splitter:** for now Splitter is a test build, not yet signed by Microsoft, so Windows loads it only in test-signing mode (Secure Boot off; some games' anti-cheat refuses to run in it). Switch test signing on as the README.txt in its zip explains, run `install.cmd`, then start BananaSplit.
- **Installing VB-Audio's cables:** download the VB-CABLE driver pack (CABLE-A … CABLE-D come in two extra packs), run `VBCABLE_Setup_x64.exe` as administrator, **Install Driver**, restart Windows, then start BananaSplit.
- BananaSplit never installs drivers itself.
- **With Voicemeeter:** Splitter and VB-CABLE add devices of their own and leave Voicemeeter's devices and settings alone. BananaSplit never uses Voicemeeter's devices as its cables. To pass audio between the two without a driver, use VBAN, with BananaSplit on a port other than 6980 (Voicemeeter's) and 127.0.0.1 as the address.
- **Default devices:** BananaSplit never changes them, but Windows may make a newly installed cable the default output or input. Check them in the Virtual cables window or the Windows sound settings after installing.
- **Quitting** BananaSplit leaves the cables installed; they carry no sound until it starts again.

Windows has no per-app stream routing like PipeWire, so device lists show WASAPI endpoints only.

## Building

```powershell
cargo build --release
```

`build.rs` embeds the app icon (`assets/icons/bananasplit.ico`), the version information and the application manifest. It needs no extra tools, but works with the MSVC toolchain only.

Release builds in CI use a statically linked C runtime:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --target x86_64-pc-windows-msvc
```

## Notes

- The UI talks to the mixer over a loopback-only local connection.
- If audio glitches after a device change, BananaSplit reconnects automatically.
- Default routing uses `@default` and `@physical`; change it from the channel menus.
