BananaSplit 2.7.0 for Windows (WASAPI)

WHAT THIS IS
------------
BananaSplit is a low-latency virtual audio mixer with a native desktop window.
This is the Windows port built on WASAPI shared mode and Microsoft's WebView2
Runtime.

HOW TO RUN
----------
1. Extract the zip. Put bananasplit.exe anywhere (e.g. Desktop).
2. Start it (double-click or from a terminal): "bananasplit.exe"
   - BananaSplit opens in its own desktop window. No external browser is opened.
   - It can also run without opening the window: "bananasplit.exe --gui --no-open"
   - Headless mode (audio engine only): "bananasplit.exe --headless"
3. Right-click channels (or use the channel menu) to connect devices, apps, and
   virtual cables. Use the bottom bar for outputs, recording, VBAN, etc.

FILES & PATHS
-------------
- Config: %APPDATA%\bananasplit\bananasplit.ini
- Scenes: %APPDATA%\bananasplit\scenes\
- Recordings: ~/Music/BananaSplit/ by default (configurable in the GUI/INI)
- Port file (for "already running"): %LOCALAPPDATA%\bananasplit\<client>.port

COMMAND LINE
------------
- --gui                Run with desktop GUI (default)
- --headless           Run without UI
- --no-open / --hidden Do not open a window
- --gui-port PORT      Use a specific port (default 8790)
- --config FILE        Use a custom config file
- --list-devices       List WASAPI recording/playback devices
- --print-default-config Print default config and exit
- --quit               Quit a running instance
- -V / --version       Show version

INTEGRATION (from the GUI)
-------------------------
- Start Menu: "Install to Start Menu" creates a shortcut
- Autostart: "Start with Windows" adds BananaSplit to HKCU\...\Run
- Closing the app window leaves the mixer running; use the power/quit button
  in the GUI or run with --quit. Starting again opens the existing instance.

VIRTUAL CABLES (VB-AUDIO)
-------------------------
Per-channel virtual cables require genuine VB-Audio CABLE, CABLE-A, CABLE-B,
CABLE-C, or CABLE-D pairs installed on the system. This build uses only real
VB-Audio cable pairs (CABLE/CABLE-A..D). If no pairs are installed, cables are
unavailable. Each pair can be assigned to at most one channel (strips first, then
buses). Windows has no per-app JACK-style stream routing; device lists show
WASAPI endpoints only.

TIPS
----
- If the GUI says "another instance is running", it will open the existing one.
- If audio glitches after device changes, BananaSplit will reconnect automatically.
- Default routing uses @default/@physical; change via the channel menu.

NOTES
-----
- This is a Windows WASAPI port of BananaSplit 2.7 (channel menus + review fixes).
- Install the Microsoft Edge WebView2 Runtime if it is not present on your PC.
- The UI communicates with the mixer over a loopback-only local connection.
- The checked-in portable EXE still uses the previous browser launcher until it
  is replaced by a build from this source tree.
- Source: 0.2.7 channel menus + review fixes (ported to WASAPI).
