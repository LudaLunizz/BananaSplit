# BananaSplit for Windows

The Windows port of BananaSplit, built on WASAPI shared mode with a native desktop window powered by the Microsoft Edge WebView2 Runtime.

The mixer core is shared with the [Linux app](../linux/README.md). See that manual for the mixer, EQ, VBAN, recorder and scene features.

## Requirements

- Windows 10 or 11, x64
- [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
- Optional: [VB-Audio CABLE](https://vb-audio.com/Cable/) pairs (CABLE, CABLE-A … CABLE-D) for per-channel virtual cables

## Running

Extract the release zip, put `bananasplit.exe` anywhere and start it. BananaSplit opens in its own window; no external browser is used.

Closing the window minimizes it and leaves the mixer running; turn off **Closing the window keeps it running** in the app menu to make closing it quit BananaSplit. Use the quit button in the app or run `bananasplit.exe --quit` to quit. Starting it again brings up the running instance's window. The window always closes with the mixer.

## Command line

| Option | Description |
| --- | --- |
| `--gui` | Run with the desktop window (default) |
| `--headless` | Run the audio engine without any UI |
| `--no-open`, `--hidden` | Start in the background, without a window (started from a shortcut or at login, it closes its console window right away) |
| `--gui-port PORT` | Use a specific local port (default `8790`) |
| `--config FILE` | Use a custom config file |
| `--list-devices` | List WASAPI recording and playback devices |
| `--print-default-config` | Print the default config and exit |
| `--quit` | Quit a running instance |
| `-V`, `--version` | Print the version |

## Files

| What | Where |
| --- | --- |
| Config | `%APPDATA%\bananasplit\bananasplit.ini` |
| Scenes | `%APPDATA%\bananasplit\scenes\` |
| Recordings | `~/Music/BananaSplit/` by default, configurable in the app or config |
| Port file | `%LOCALAPPDATA%\bananasplit\<client>.port` |
| Window (WebView2) data | `%LOCALAPPDATA%\bananasplit\WebView2\` |

## System integration

From the app menu:

- **Install to Start Menu** creates a shortcut.
- **Start with Windows** registers BananaSplit under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. It then starts in the background, without a window; start BananaSplit to open it.

## Virtual cables

Per-channel virtual cables require genuine VB-Audio cable pairs. Each installed pair is assigned to at most one channel, strips first, then buses. Without any pairs, cables are unavailable.

Windows has no per-app stream routing like PipeWire, so device lists show WASAPI endpoints only.

## Building

```powershell
cargo build --release
```

Release builds in CI use a statically linked C runtime:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --target x86_64-pc-windows-msvc
```

## Notes

- The UI talks to the mixer over a loopback-only local connection.
- If audio glitches after a device change, BananaSplit reconnects automatically.
- Default routing uses `@default` and `@physical`; change it from the channel menus.
