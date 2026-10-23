BananaSplit 2.7.0 for Windows (WASAPI)

WHAT THIS IS
------------
BananaSplit is a low-latency virtual audio mixer with a browser GUI. This is the
Windows port built on WASAPI shared mode with zero external dependencies.

HOW TO RUN
----------
1. Extract the zip. Put bananasplit.exe anywhere (e.g. Desktop).
2. Start it (double-click or from a terminal): "bananasplit.exe --gui"
   - The browser will open to http://127.0.0.1:8790/ (if not, open that URL manually).
   - It can also run hidden: "bananasplit.exe --gui --no-open"
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
- --gui                Run with browser GUI (default)
- --headless           Run without UI
- --no-open / --hidden Do not open the browser
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
- Closing the browser window does not quit the app; use the power/quit button
  in the GUI or run with --quit. Starting again brings up the existing instance.

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
- All processing stays local (localhost only).
- Source: 0.2.7 channel menus + review fixes (ported to WASAPI).
