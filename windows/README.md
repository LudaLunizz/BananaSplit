# BananaSplit for Windows

The Windows port of BananaSplit, built on WASAPI shared mode with a native desktop window powered by the Microsoft Edge WebView2 Runtime.

The mixer core is shared with the [Linux app](../linux/README.md). See that manual for the mixer, EQ, VBAN, recorder and scene features.

## Requirements

- Windows 10 or 11, x64
- [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
- Optional: [VB-Audio CABLE](https://vb-audio.com/Cable/) pairs (CABLE, CABLE-A … CABLE-D) for per-channel virtual cables

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
- **Install** downloads `bananasplit-<version>-windows-x64.exe` to `%LOCALAPPDATA%\bananasplit\update\` and checks it first: it must come from BananaSplit's release downloads over HTTPS, have the size GitHub lists, and match GitHub's SHA-256 of the file and the release's `SHA256SUMS.txt`; then it must run and say it is that version. Windows doesn't let a running program be overwritten, so the running `bananasplit.exe` is renamed to `bananasplit.exe.old` and the new one takes its name (and so keeps your Start Menu shortcut and "Start with Windows"). The next start removes the `.old` file.
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

Per-channel virtual cables require genuine VB-Audio cable pairs. Each installed pair is assigned to at most one channel, strips first, then buses. Without any pairs, cables are unavailable.

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
