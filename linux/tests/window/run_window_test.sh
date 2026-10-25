#!/bin/sh
# The app window and the tray icon, tested for real: GTK 3 on an X server
# (Xvfb), a D-Bus session of its own, a tray written from the
# StatusNotifierItem/dbusmenu specifications (fake_tray.py), a stand-in for
# WebKitGTK (mockwebkit.c: a GTK label that records what it is asked to
# load) and the mock JACK server of the end-to-end test.
#
#   tests/window/run_window_test.sh [path/to/bananasplit]
#
# Needs: gcc and GLib headers (pkg-config gobject-2.0), libgtk-3, Xvfb,
# dbus-run-session, curl, and a python3 with dbus-python and PyGObject
# (set PYTHON=… if that is not the default python3).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
BIN=${1:-$ROOT/target/release/bananasplit}
PYTHON=${PYTHON:-python3}
export BIN PYTHON

if [ "$2" != "--inner" ]; then
  DISP=:$((90 + $$ % 9))
  Xvfb "$DISP" -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
  XPID=$!
  trap 'kill $XPID 2>/dev/null' EXIT
  i=0; until DISPLAY=$DISP "$PYTHON" "$HERE/xtool.py" list >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 50 ] || { echo "Xvfb did not start"; exit 1; }; sleep 0.1; done
  DISPLAY=$DISP dbus-run-session -- sh "$0" "$BIN" --inner
  exit $?
fi

T=$(mktemp -d)
mkdir -p "$T/webkit" "$T/home" "$T/fakebin"
gcc -shared -fPIC -O2 -o "$T/webkit/libwebkit2gtk-4.1.so.0" "$HERE/mockwebkit.c" $(pkg-config --cflags --libs gobject-2.0 glib-2.0) \
  "$(pkg-config --variable=libdir gobject-2.0 2>/dev/null || echo /usr/lib/x86_64-linux-gnu)/libgtk-3.so.0"
[ -f "$ROOT/tests/e2e/libjack.so.0" ] || gcc -shared -fPIC -O2 -o "$ROOT/tests/e2e/libjack.so.0" "$ROOT/tests/e2e/mockjack.c" -lpthread -lm
printf '#!/bin/sh\necho "$@" >> "%s"\n' "$T/browser.log" > "$T/fakebin/chromium"
chmod +x "$T/fakebin/chromium"
export HOME="$T/home" XDG_CONFIG_HOME="$T/home/.config" XDG_DATA_HOME="$T/home/.local/share" XDG_CACHE_HOME="$T/home/.cache"
export MOCKJACK_REPORT="$T/report.txt" MOCKJACK_REALTIME=1 MOCKJACK_CYCLES=100000000 NO_AT_BRIDGE=1 GDK_BACKEND=x11
export MOCKWEBKIT_LOG="$T/webkit.log" MOCKWEBKIT_FIRE=context-menu,load-failed PATH="$T/fakebin:$PATH"
unset WAYLAND_DISPLAY XDG_ACTIVATION_TOKEN DESKTOP_STARTUP_ID
PORT=18960; URL="http://127.0.0.1:$PORT"
WITH_WEBKIT="$T/webkit:$ROOT/tests/e2e"
NO_WEBKIT="$ROOT/tests/e2e"
PID=""; TRAY=""

fail() {
  echo "WINDOW TEST FAIL: $1"
  # on GitHub Actions also an annotation, shown on the run's page
  [ "${GITHUB_ACTIONS:-}" = true ] && printf '::error title=Window test::%s\n' "$(printf '%s' "$1" | sed 's/%/%25/g' | awk 'BEGIN{ORS="%0A"} {print}' | sed 's/%0A$//')"
  for f in out.txt tray.log webkit.log browser.log; do [ -f "$T/$f" ] && { echo "--- $f"; tail -25 "$T/$f"; }; done
  "$PYTHON" "$HERE/xtool.py" list
  [ -n "$PID" ] && kill "$PID" 2>/dev/null
  [ -n "$TRAY" ] && kill "$TRAY" 2>/dev/null
  exit 1
}
cfg() { mkdir -p "$HOME/.config/bananasplit"; printf '[global]\ncreate_cables = false\n%b' "$1" > "$HOME/.config/bananasplit/bananasplit.ini"; }
start() { # start LD_LIBRARY_PATH [extra args]
  LD_LIBRARY_PATH="$1" "$BIN" --gui --gui-port $PORT $2 </dev/null >>"$T/out.txt" 2>&1 &
  PID=$!
  i=0; until curl -sf "$URL/api/ping" >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 100 ] || fail "BananaSplit did not start"; sleep 0.1; done
}
again() { LD_LIBRARY_PATH="$1" "$BIN" --gui --gui-port $PORT </dev/null >>"$T/out.txt" 2>&1 || fail "second --gui failed"; }
win() { "$PYTHON" "$HERE/xtool.py" list | grep '"title": "BananaSplit"' | grep '"wm_class": \["bananasplit", "bananasplit"\]' || true; }
mapped() { win | grep -q '"mapped": true'; }
waitfor() { # waitfor "shell condition" "what" [tenths]
  i=0; until eval "$1"; do i=$((i+1)); [ $i -lt "${3:-50}" ] || fail "$2"; sleep 0.1; done
}
tray_start() { : > "$T/tray.log"; : > "$T/cmd"; "$PYTHON" "$HERE/fake_tray.py" "$T/tray.log" "$T/cmd" & TRAY=$!; waitfor 'grep -q "watcher up" "$T/tray.log"' "fake tray did not start"; }
tray_stop() { kill "$TRAY" 2>/dev/null; wait "$TRAY" 2>/dev/null || true; TRAY=""; }
tray() { echo "$1" >> "$T/cmd"; }
state() { curl -s "$URL/api/state"; }
quit_api() { curl -s -o /dev/null -H 'X-BananaSplit: 1' --data op=quit "$URL/api/cmd"; wait "$PID" || fail "exit code $? after quit"; PID=""; }
winpid() { win | sed -n 's/.*"pid": \([0-9]*\).*/\1/p' | head -1; }
nowin() { for p in /proc/[0-9]*; do tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q -e "--window http://127.0.0.1:$PORT/" && return 1; done; return 0; }

# ---- W1: the app window with a tray ------------------------------------------------
cfg ""
tray_start
start "$WITH_WEBKIT"
waitfor mapped "the window did not appear"
win | grep -q '"width": 1780, "height": 1000' || fail "window size (want 1780x1000 on a 1920x1080 screen): $(win)"
WP=$(winpid); [ -n "$WP" ] && [ "$WP" != "$PID" ] || fail "the window should be its own process"
grep -q "^load_uri $URL/$" "$T/webkit.log" || fail "the web view did not load the mixer page"
grep -q "^background" "$T/webkit.log" || fail "no dark background before the page paints"
waitfor 'grep -q "^emit context-menu -> 1" "$T/webkit.log"' "the browser's context menu is not suppressed outside text fields"
waitfor 'grep -q "^emit load-failed -> 1" "$T/webkit.log"' "load-failed not handled"
waitfor 'grep -q "^reload" "$T/webkit.log"' "a failed load is not retried" 30
waitfor 'grep -q "^registered org.kde.StatusNotifierItem-$WP-1" "$T/tray.log"' "the tray icon did not register"
waitfor 'grep -q "^props " "$T/tray.log"' "the tray could not read the icon"
grep -q "^props Id=bananasplit Title=BananaSplit Category=ApplicationStatus Status=Active Menu=/MenuBar ItemIsMenu=False IconName='' IconPixmap=22x22:1936,32x32:4096,48x48:9216,64x64:16384 ToolTip=BananaSplit/Mixer running$" "$T/tray.log" || fail "tray properties"
waitfor 'grep -q "^layout rev=[0-9]* root=0 version=3 1:Hide BananaSplit | 2:- | 3:Start recording | 4:- | 5:Quit BananaSplit$" "$T/tray.log"' "tray menu"
grep -q "^group 1:Hide BananaSplit,5:Quit BananaSplit$" "$T/tray.log" || fail "GetGroupProperties"
echo "WINDOW PASS: app window (own process, 1780x1000, WM_CLASS bananasplit) with the mixer page; tray icon and menu"

tray "click 3"
waitfor 'state | grep -q "\"rec\":{\"on\":true"' "tray: Start recording did not record"
waitfor 'grep -q "^signal NewIcon" "$T/tray.log"' "the tray icon did not change while recording"
waitfor 'grep -q "3:Stop recording" "$T/tray.log"' "tray menu does not offer Stop recording"
tray "click 3"
waitfor 'state | grep -q "\"rec\":{\"on\":false"' "tray: Stop recording did not stop"
echo "WINDOW PASS: tray menu starts and stops recording; icon and menu follow"

"$PYTHON" "$HERE/xtool.py" close BananaSplit
waitfor '! mapped' "closing the window did not hide it (tray available)"
curl -sf "$URL/api/ping" >/dev/null || fail "closing the window stopped the mixer"
waitfor 'tail -1 "$T/tray.log" | grep -q "1:Show BananaSplit" || grep -q "1:Show BananaSplit" "$T/tray.log"' "tray menu should offer Show after hiding"
again "$WITH_WEBKIT"
waitfor mapped "launching again did not bring the window back"
tray "click 1"
waitfor '! mapped' "tray Hide did not hide"
tray "activate"
waitfor mapped "left click on the tray icon did not show the window"
echo "WINDOW PASS: close hides to the tray, the mixer keeps running; app menu launch and tray bring it back"

kill -9 "$WP"
waitfor 'grep -q "the window closed" "$T/out.txt"' "the mixer did not notice the window crash"
curl -sf "$URL/api/ping" >/dev/null || fail "a window crash took the mixer down"
again "$WITH_WEBKIT"
waitfor mapped "launching again after a crash did not open a new window"
WP2=$(winpid); [ "$WP2" != "$WP" ] || fail "no new window process"
waitfor '[ "$(grep -c "^registered " "$T/tray.log")" -ge 2 ]' "the new window process has no tray icon"
echo "WINDOW PASS: a crashed window does not stop the mixer; launching again opens a new one"

tray "click 5"
wait "$PID" || fail "tray Quit: exit code $?"
PID=""
waitfor nowin "the window process outlived the mixer"
grep -q "^window = 1780x1000$" "$HOME/.config/bananasplit/bananasplit.ini" || fail "window size not saved"
echo "WINDOW PASS: tray Quit stops the mixer and the window; the window size is saved"
tray_stop

# ---- W2: no tray: closing minimizes, the mixer keeps running ---------------------------
start "$WITH_WEBKIT"
waitfor mapped "W2: no window"
"$PYTHON" "$HERE/xtool.py" close BananaSplit
sleep 1
mapped || fail "without a tray, closing must not hide the window (nothing could bring it back)"
curl -sf "$URL/api/ping" >/dev/null || fail "W2: closing stopped the mixer"
quit_api
waitfor nowin "W2: window outlived the mixer"
echo "WINDOW PASS: without a tray, closing minimizes instead of hiding"

# ---- W3: close_to_tray = false: closing quits ------------------------------------------
cfg "[gui]\nclose_to_tray = false\n"
tray_start
start "$WITH_WEBKIT"
waitfor mapped "W3: no window"
"$PYTHON" "$HERE/xtool.py" close BananaSplit
i=0; while kill -0 "$PID" 2>/dev/null; do i=$((i+1)); [ $i -lt 50 ] || fail "W3: closing did not quit"; sleep 0.1; done
wait "$PID" || fail "W3: exit code $?"
PID=""
echo "WINDOW PASS: with close_to_tray = false, closing the window quits BananaSplit"

# ---- W4: no WebKitGTK: tray icon + the page in a browser --------------------------------
cfg ""
: > "$T/browser.log"
start "$NO_WEBKIT"
waitfor 'grep -q "^--app=$URL/$" "$T/browser.log"' "no browser app window without WebKitGTK"
grep -q "WebKitGTK not found" "$T/out.txt" || fail "no message about the missing WebKitGTK"
waitfor '[ "$(grep -c "^registered " "$T/tray.log")" -ge 1 ]' "W4: no tray icon"
waitfor 'grep -q "1:Show BananaSplit" "$T/tray.log"' "W4: tray menu"
tray "click 1"
waitfor '[ "$(grep -c "^--app=" "$T/browser.log")" -eq 2 ]' "tray Show did not open the page again"
again "$NO_WEBKIT"
waitfor '[ "$(grep -c "^--app=" "$T/browser.log")" -eq 3 ]' "launching again did not open the page"
tray "click 5"
wait "$PID" || fail "W4: exit code $?"
PID=""
echo "WINDOW PASS: without WebKitGTK the tray icon stays and the page opens in a browser app window"

# ---- W5: --hidden (Start with the system) ----------------------------------------------
start "$WITH_WEBKIT" --hidden
sleep 3.5
nowin && fail "--hidden: no window process"
mapped && fail "--hidden with a tray must not show the window"
grep -q "^--app=" "$T/browser.log" && [ "$(grep -c "^--app=" "$T/browser.log")" -gt 3 ] && fail "--hidden opened a browser"
again "$WITH_WEBKIT"
waitfor mapped "--hidden: launching again did not show the window"
quit_api
tray_stop
start "$WITH_WEBKIT" --hidden
waitfor mapped "--hidden without a tray must still show the window (minimized)" 60
quit_api
echo "WINDOW PASS: --hidden starts in the tray; without a tray it starts minimized"

# ---- W6: an error is shown in a dialog when there is no terminal ----------------------------
LD_LIBRARY_PATH=/nonexistent "$BIN" --gui --gui-port $PORT </dev/null >"$T/err.txt" 2>&1 &
EPID=$!
waitfor '"$PYTHON" "$HERE/xtool.py" list | grep "\"title\": \"BananaSplit\"" | grep -q "\"mapped\": true"' "no error dialog"
grep -q -e "could not load libjack" -e "cannot open JACK client" "$T/err.txt" || fail "error not printed"
for p in /proc/[0-9]*; do tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q -e "--error-dialog BananaSplit could not start" && kill "${p#/proc/}"; done
RC=0; wait $EPID || RC=$?
[ $RC -eq 1 ] || fail "exit code $RC after an error"
echo "WINDOW PASS: started from the app menu without JACK, the error is shown in a dialog"

# ---- W7: Auto theme follows the desktop's dark style (settings portal) -----------------------
: > "$T/webkit.log"; : > "$T/portal.log"; : > "$T/pcmd"
"$PYTHON" "$HERE/fake_portal.py" "$T/portal.log" "$T/pcmd" 1 & PORTAL=$!
waitfor 'grep -q "portal up" "$T/portal.log"' "fake portal did not start"
cfg ""
start "$WITH_WEBKIT"
waitfor 'grep -q "^read org.freedesktop.appearance color-scheme$" "$T/portal.log"' "the window did not ask the portal for the dark style"
waitfor 'grep -q "^prefer-dark 1$" "$T/webkit.log"' "prefer dark (portal: 1) not passed to GTK/WebKit"
echo 2 > "$T/pcmd"
waitfor 'grep -q "^prefer-dark 0$" "$T/webkit.log"' "switching the desktop to light did not follow"
echo 1 > "$T/pcmd"
waitfor '[ "$(grep -c "^prefer-dark 1$" "$T/webkit.log")" -ge 2 ]' "switching back to dark did not follow"
quit_api
kill "$PORTAL" 2>/dev/null; wait "$PORTAL" 2>/dev/null || true
echo "WINDOW PASS: the page's Auto theme follows the desktop's dark style (settings portal), live"

# ---- W8: a window restarted while recording, after an update replaced the program ---------------
# The tray shows the recording, and the new window is the running program's
# own (not the replaced file's, not a browser).
ORIGBIN=$BIN; BIN="$T/bin8"; cp "$ORIGBIN" "$BIN"
cfg ""
tray_start
: > "$T/out.txt"
start "$WITH_WEBKIT"
waitfor mapped "the window did not appear (W8)"
curl -s -o /dev/null -H 'X-BananaSplit: 1' --data 'op=rec&v=1' "$URL/api/cmd"
waitfor 'state | grep -q "\"rec\":{\"on\":true"' "recording did not start (W8)"
waitfor 'grep -q "3:Stop recording" "$T/tray.log"' "tray does not show the recording (W8)"
cp "$BIN" "$BIN.new" && mv "$BIN.new" "$BIN"   # the update
kill -9 "$(winpid)"
waitfor 'grep -q "the window closed" "$T/out.txt"' "the mixer did not notice the window crash (W8)"
: > "$T/tray.log"; : > "$T/browser.log"
again "$WITH_WEBKIT"
waitfor mapped "no app window after the update (W8)"
readlink "/proc/$(winpid)/exe" | grep -q "bin8 (deleted)" || fail "the new window did not come from the running program: $(readlink "/proc/$(winpid)/exe")"
[ ! -s "$T/browser.log" ] || fail "fell back to a browser after the update"
waitfor 'grep -q "^registered " "$T/tray.log"' "the new window process has no tray icon (W8)"
waitfor 'grep -q "3:Stop recording" "$T/tray.log"' "after a restart while recording, the tray menu offers Start recording"
tray "props"
waitfor 'grep "^props " "$T/tray.log" | tail -1 | grep -q "ToolTip=BananaSplit/Recording"' "after a restart while recording, the tray icon does not show it"
tray "click 3"
waitfor 'state | grep -q "\"rec\":{\"on\":false"' "tray: Stop recording did not stop (W8)"
quit_api
tray_stop
BIN=$ORIGBIN
echo "WINDOW PASS: a window restarted while recording, after an update, comes from the running program and shows the recording in the tray"

rm -rf "$T"
echo "WINDOW TEST: ALL PASS"
