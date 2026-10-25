#!/bin/sh
# End-to-end test: runs the real BananaSplit binary against mockjack (no audio server needed).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${1:-$HERE/../../target/release/bananasplit}
gcc -shared -fPIC -O2 -o "$HERE/libjack.so.0" "$HERE/mockjack.c" -lpthread -lm
TMP=$(mktemp -d); cp "$HERE/e2e.ini" "$TMP/bananasplit.ini"
export MOCKJACK_REPORT="$TMP/report.txt"
LD_LIBRARY_PATH="$HERE" "$BIN" --headless --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
sleep 7
kill -INT $PID; wait $PID; RC=$?
cat "$TMP/stderr.txt"; cat "$MOCKJACK_REPORT"
fail() {
  echo "E2E FAIL: $1"
  # on GitHub Actions also an annotation, shown on the run's page
  [ "${GITHUB_ACTIONS:-}" = true ] && printf '::error title=End-to-end test::%s\n' "$(printf '%s' "$1" | sed 's/%/%25/g' | awk 'BEGIN{ORS="%0A"} {print}' | sed 's/%0A$//')"
  exit 1
}
[ $RC -eq 0 ] || fail "exit code $RC"
grep -q GARBAGE "$MOCKJACK_REPORT" && fail "output buffers not fully written"
grep -q "Speakers:monitor" "$MOCKJACK_REPORT" && fail "@physical picked a monitor port (feedback loop)"
grep -q "^SIGNAL cycle=500 frame=77 BananaSplit:A1_L -> system:playback_1 value=0.625000$" "$MOCKJACK_REPORT" || fail "A1_L"
grep -q "^SIGNAL cycle=500 frame=77 BananaSplit:A1_R -> system:playback_2 value=0.312500$" "$MOCKJACK_REPORT" || fail "A1_R"
grep -q "^SIGNAL cycle=500 frame=77 BananaSplit:B1_L -> OBS:input_1 value=0.500000$" "$MOCKJACK_REPORT" || fail "B1_L"
grep -q "^SIGNAL cycle=500 frame=77 BananaSplit:B1_R -> OBS:input_1 value=0.250000$" "$MOCKJACK_REPORT" || fail "B1_R"
[ "$(grep -c '^SIGNAL' "$MOCKJACK_REPORT")" -eq 4 ] || fail "signal leaked to other frames/cycles/buses"
grep -q "^CYCLES 2000 " "$MOCKJACK_REPORT" || fail "process callback did not run every cycle"
echo "E2E PASS: all outputs sample-aligned at cycle 500 frame 77, zero added latency"
rm -rf "$TMP"

# ---- phase 2: PipeWire virtual cables (fake pactl + cable ports in the mock) ----
TMP=$(mktemp -d)
export PACTL_LOG="$TMP/pactl.log" MOCKJACK_REPORT="$TMP/report.txt" MOCKJACK_CABLES=1 XDG_CONFIG_HOME="$TMP"
PATH="$HERE/fakebin:$PATH" LD_LIBRARY_PATH="$HERE" "$BIN" --headless 2>"$TMP/stderr.txt" &
PID=$!
sleep 2
kill -TERM $PID; wait $PID; RC=$?
[ $RC -eq 0 ] || fail "cables: exit code $RC"
grep -q "^pactl unload-module 7$" "$PACTL_LOG" || fail "stale cable not removed"
grep -q "unload-module 9" "$PACTL_LOG" && fail "removed a module that is not ours"
[ "$(grep -c 'load-module module-null-sink' "$PACTL_LOG")" -eq 8 ] || fail "expected 8 cables"
[ "$(grep -c '^pactl unload-module 1[0-9][0-9]$' "$PACTL_LOG")" -eq 8 ] || fail "cables not removed on exit"
grep -q "node.description=BananaSplit-Out-B1 media.class=Audio/Source/Virtual" "$PACTL_LOG" || fail "B1 virtual mic args"
for s in 1 2 3 4 5 6; do
  grep -q "^CONN BananaSplit-In$s-[A-Za-z]*:monitor_FL -> BananaSplit:in${s}_L$" "$MOCKJACK_REPORT" || fail "cable $s L"
  grep -q "^CONN BananaSplit-In$s-[A-Za-z]*:monitor_FR -> BananaSplit:in${s}_R$" "$MOCKJACK_REPORT" || fail "cable $s R"
done
grep -q "^CONN BananaSplit:B1_L -> BananaSplit-Out-B1:input_FL$" "$MOCKJACK_REPORT" || fail "B1 -> virtual mic"
grep -q "^CONN BananaSplit:B2_R -> BananaSplit-Out-B2:input_FR$" "$MOCKJACK_REPORT" || fail "B2 -> virtual mic"
grep -q "removed 1 virtual cable left behind by a BananaSplit that was killed" "$TMP/stderr.txt" || fail "stale cable not reported"
# a clean exit disarms the watchdog: it must not sweep again after bananasplit
# (which lists the modules itself at start, and on exit to remove its cables
# by their names): the last call is bananasplit's last unload
LAST_LIST=$(grep -n '^pactl list short modules$' "$PACTL_LOG" | tail -n 1 | cut -d: -f1)
LAST_UNLOAD=$(grep -n '^pactl unload-module 1[0-9][0-9]$' "$PACTL_LOG" | tail -n 1 | cut -d: -f1)
{ [ "$LAST_LIST" -lt "$LAST_UNLOAD" ] && [ "$LAST_UNLOAD" -eq "$(wc -l < "$PACTL_LOG")" ]; } || fail "watchdog swept after a clean exit"
echo "E2E PASS: 8 virtual cables created, wired, and removed; stale cable cleaned up"
rm -rf "$TMP"

# ---- phase 3: GUI + recorder (real binary, driven over HTTP exactly like the page) ----
command -v curl >/dev/null || fail "phase 3 needs curl"
TMP=$(mktemp -d); cp "$HERE/e2e.ini" "$TMP/bananasplit.ini"
unset MOCKJACK_CABLES PACTL_LOG XDG_CONFIG_HOME
export MOCKJACK_REPORT="$TMP/report.txt" MOCKJACK_IMPULSE_CYCLE=1200 MOCKJACK_CYCLES=20000
PORT=18791; URL="http://127.0.0.1:$PORT"
LD_LIBRARY_PATH="$HERE" "$BIN" --gui --no-open --gui-port $PORT --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
trap 'rc=$?; kill $PID 2>/dev/null; exit $rc' EXIT   # never leave BananaSplit running if a check fails
cmd() { curl -s -o "$TMP/resp" -w '%{http_code}' -H 'X-BananaSplit: 1' --data "$1" "$URL/api/cmd"; }
i=0; until curl -sf "$URL/api/ping" >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 200 ] || fail "GUI did not come up"; sleep 0.05; done
curl -s "$URL/" | grep -q '<title>BananaSplit</title>' || fail "page not served"
[ "$(curl -s -o /dev/null -w '%{http_code}' --data 'op=mute&ch=0&v=1' "$URL/api/cmd")" = 403 ] || fail "POST without X-BananaSplit accepted"
[ "$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example' "$URL/api/state")" = 403 ] || fail "foreign Host accepted"
# another user on this computer is refused (run as root, the test acts as nobody)
if [ "$(id -u)" = 0 ] && command -v setpriv >/dev/null; then
  as_nobody() { setpriv --reuid=65534 --regid=65534 --clear-groups curl -s -o /dev/null -w '%{http_code}' "$@"; }
  [ "$(as_nobody -H 'X-BananaSplit: 1' --data 'op=mute&ch=0&v=1' "$URL/api/cmd")" = 403 ] || fail "another user's command accepted"
  [ "$(as_nobody "$URL/api/state")" = 403 ] || fail "another user read the state"
  [ "$(as_nobody "$URL/")" = 403 ] || fail "another user got the page"
  echo "E2E: another user's requests are refused"
else
  echo "E2E SKIP: another user's requests (needs root and setpriv to act as another user)"
fi
LD_LIBRARY_PATH="$HERE" "$BIN" --gui --no-open --gui-port $PORT --config "$TMP/bananasplit.ini" 2>"$TMP/second.txt" || fail "second --gui failed"
grep -q "already running" "$TMP/second.txt" || fail "second --gui did not find the running one"
[ "$(cmd 'op=link&ch=0&v=1')" = 200 ] || fail "link strip 1"
[ "$(cmd 'op=link&ch=6&v=1')" = 200 ] || fail "link A1"
[ "$(cmd "op=recset&mode=multi&format=wav32f&folder=$TMP/rec")" = 200 ] || fail "recorder settings"
[ "$(cmd 'op=rec&v=1')" = 200 ] || fail "start recording: $(cat "$TMP/resp")"
# state is republished every UI frame (33 ms): allow a few frames for it to show up
i=0; until curl -s "$URL/api/state" | grep -q '"rec":{"on":true,"takeMode":"multi"'; do
  i=$((i+1)); [ $i -lt 40 ] || fail "state does not show the recording"; sleep 0.05
done
# wait until the impulse has gone through strip 1 (its meter jumps to -6 dB)
i=0
until [ "$(curl -s "$URL/api/state" | grep -o '"id":"1"[^}]*' | grep -o '"m":\[[-0-9.]*' | cut -d[ -f2 | awk '{print ($1 > -20)}')" = 1 ]; do
  i=$((i+1)); [ $i -lt 600 ] || fail "impulse never reached strip 1"; sleep 0.05
done
sleep 0.3
[ "$(cmd 'op=rec&v=0')" = 200 ] || fail "stop recording"
sleep 0.5
# EQ and VBAN settings from the GUI (after the take: they would change its audio)
[ "$(cmd 'op=eq&ch=7&v=1')" = 200 ] || fail "EQ on"
[ "$(cmd 'op=band&ch=7&band=0&type=lowcut&f=80&q=0.71')" = 200 ] || fail "EQ band"
[ "$(cmd 'op=band&ch=7&band=3&g=-4.5')" = 200 ] || fail "EQ gain"
[ "$(cmd 'op=band&ch=7&band=6&g=1')" = 400 ] || fail "EQ band 7 accepted"
[ "$(cmd 'op=band&ch=7&band=1&g=99')" = 400 ] || fail "EQ gain 99 accepted"
[ "$(cmd 'op=vbanin&slot=1&on=1&name=Laptop&from=192.168.1.30&strip=6&buffer=20')" = 200 ] || fail "VBAN in"
[ "$(cmd 'op=vbanout&slot=2&on=1&name=Mix&to=127.0.0.1&port=9&src=A1&format=int24')" = 200 ] || fail "VBAN out"
[ "$(cmd 'op=vbanout&slot=2&src=C1')" = 400 ] || fail "VBAN source C1 accepted"
i=0; until curl -s "$URL/api/state" | grep -q '"id":"A2".*"eq":{"on":true,"b":\[\[1,"lowcut",80,0.0,0.71\]'; do
  i=$((i+1)); [ $i -lt 40 ] || fail "state does not show the EQ"; sleep 0.05
done
curl -s "$URL/api/state" | grep -q '"vban":{"on":false,"port":6980,' || fail "state has no VBAN"
[ "$(cmd 'op=quit')" = 200 ] || fail "quit"
RC=0; wait $PID || RC=$?
trap - EXIT
cat "$TMP/stderr.txt"
[ $RC -eq 0 ] || fail "gui: exit code $RC"
grep -q "GUI at $URL/" "$TMP/stderr.txt" || fail "GUI URL not printed"
grep -q "^mode = multi$" "$TMP/bananasplit.ini" && grep -q "^links = 1,A1$" "$TMP/bananasplit.ini" || fail "recorder settings not saved on quit"
sed -n '/^\[bus.A2\]$/,/^\[/p' "$TMP/bananasplit.ini" | grep -q "^eq = on$" || fail "EQ on not saved"
grep -q "^eq1 = lowcut 80 +0 0.71$" "$TMP/bananasplit.ini" && grep -q "^eq4 = bell 1500 -4.5 1$" "$TMP/bananasplit.ini" || fail "EQ bands not saved"
sed -n '/^\[vban.in2\]$/,/^$/p' "$TMP/bananasplit.ini" | tr '\n' ' ' | grep -q "on = true name = Laptop from = 192.168.1.30 strip = 6 buffer_ms = 20" || fail "VBAN in not saved"
sed -n '/^\[vban.out3\]$/,/^$/p' "$TMP/bananasplit.ini" | tr '\n' ' ' | grep -q "on = true name = Mix to = 127.0.0.1 port = 9 source = A1 format = int24 channels = 2" || fail "VBAN out not saved"
grep -q "^enabled = false$" "$TMP/bananasplit.ini" || fail "VBAN switched itself on"
F1=$(ls "$TMP"/rec/*_1-Mic.wav); FA=$(ls "$TMP"/rec/*_A1-Speakers.wav)
[ "$(ls "$TMP"/rec | wc -l)" -eq 2 ] || fail "expected exactly 2 tracks: $(ls "$TMP"/rec)"
SZ=$(stat -c %s "$F1")
[ "$SZ" = "$(stat -c %s "$FA")" ] || fail "tracks differ in length"
[ "$(od -A n -t u4 -j 54 -N 4 "$F1" | tr -d ' ')" -eq $((SZ - 58)) ] || fail "WAV data size"
[ "$(od -A n -t u4 -j 4 -N 4 "$F1" | tr -d ' ')" -eq $((SZ - 8)) ] || fail "RIFF size"
[ "$(od -A n -t u4 -j 46 -N 4 "$F1" | tr -d ' ')" -eq $(((SZ - 58) / 8)) ] || fail "fact frame count"
nz() { od -A n -v -t f4 -j 58 "$1" | tr -s ' \t' '\n\n' | grep -v '^$' | awk '{ if ($1 + 0 != 0) printf "%d %.6f\n", NR - 1, $1 }'; }
S1=$(nz "$F1"); SA=$(nz "$FA")
K=$(echo "$S1" | head -1 | cut -d' ' -f1)
[ -n "$K" ] && [ $((K % 2)) -eq 0 ] || fail "strip 1 track has no impulse: $S1"
[ "$S1" = "$(printf '%d 0.500000\n%d 0.250000' "$K" $((K + 1)))" ] || fail "strip 1 track: $S1"
[ "$SA" = "$(printf '%d 0.625000\n%d 0.312500' "$K" $((K + 1)))" ] || fail "A1 track: $SA"
echo "E2E PASS: GUI drove the recorder; strip 1 and A1 tracks hold the impulse on the same frame ($((K / 2)))"
rm -rf "$TMP"

# ---- phase 4: the review fixes (v2.3) ----
unset MOCKJACK_IMPULSE_CYCLE MOCKJACK_CYCLES
export PATH="$HERE/fakebin:$PATH"
reapers() { for p in /proc/[0-9]*; do tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q -e '--reap-cables BananaSplit ' && echo "${p#/proc/}"; done; }

# 4a: --no-cables is for this run only (the GUI saves the config on quit)
TMP=$(mktemp -d)
export PACTL_LOG="$TMP/pactl.log" MOCKJACK_REPORT="$TMP/report.txt"
sed 's/^create_cables = false$/create_cables = true/' "$HERE/e2e.ini" > "$TMP/bananasplit.ini"
URL="http://127.0.0.1:18792"
LD_LIBRARY_PATH="$HERE" "$BIN" --gui --no-open --gui-port 18792 --no-cables --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
trap 'rc=$?; kill $PID 2>/dev/null; exit $rc' EXIT
i=0; until curl -sf "$URL/api/ping" >/dev/null 2>&1; do i=$((i+1)); [ $i -lt 200 ] || fail "4a: GUI did not come up"; sleep 0.05; done
curl -s -o /dev/null -H 'X-BananaSplit: 1' --data 'op=save' "$URL/api/cmd"
curl -s -o /dev/null -H 'X-BananaSplit: 1' --data 'op=quit' "$URL/api/cmd"
RC=0; wait $PID || RC=$?; trap - EXIT
[ $RC -eq 0 ] || fail "4a: exit code $RC"
grep -q "^create_cables = true$" "$TMP/bananasplit.ini" || fail "--no-cables was written into the config"
grep -q "^pactl load-module" "$PACTL_LOG" && fail "--no-cables still created cables"
echo "E2E PASS: --no-cables applies to one run; config keeps create_cables = true"
rm -rf "$TMP"

# 4b: @physical prefers the default device, @default, hints for patterns that miss, latency advice
TMP=$(mktemp -d)
export PACTL_LOG="$TMP/pactl.log" MOCKJACK_REPORT="$TMP/report.txt" XDG_CONFIG_HOME="$TMP" PACTL_DEVICES=1 MOCKJACK_DEVICES=1 MOCKJACK_LATENCY=416
cat > "$TMP/bananasplit.ini" <<'EOF'
[global]
create_cables = false
[strip1]
connect = @physical
[strip2]
connect = @default
[strip3]
connect = ^alsa_input\.usb-Blue_Yeti
[strip4]
connect = ^Nope:
[bus.A1]
connect = @default
[bus.A2]
connect = ^yeti stereo microphone analog stereo-63:playback
EOF
LD_LIBRARY_PATH="$HERE" "$BIN" --headless --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
sleep 2.5
kill -TERM $PID; wait $PID || fail "4b: exit code $?"
cat "$TMP/stderr.txt"
Y="Yeti Stereo Microphone Analog Stereo"
for s in 1 2; do
  grep -q "^CONN $Y:capture_FL -> BananaSplit:in${s}_L$" "$MOCKJACK_REPORT" || fail "strip $s did not get the default mic"
  grep -q "^CONN $Y:capture_FR -> BananaSplit:in${s}_R$" "$MOCKJACK_REPORT" || fail "strip $s R"
done
grep -q "Razer Kiyo" "$MOCKJACK_REPORT" && fail "picked the webcam"
grep -q "^CONN BananaSplit:A1_L -> $Y-63:playback_FL$" "$MOCKJACK_REPORT" || fail "A1 did not get the default output"
grep -q "monitor" "$MOCKJACK_REPORT" && fail "connected a monitor port"
grep -qF "strip 3: nothing matches \"^alsa_input\\.usb-Blue_Yeti\" — that is the PipeWire node name; JACK calls this device \"$Y\": use connect = ^$Y:" "$TMP/stderr.txt" || fail "node-name hint"
grep -qF 'strip 4: nothing matches "^Nope:" (bananasplit --list-ports shows the names)' "$TMP/stderr.txt" || fail "generic hint"
grep -qF "bananasplit:     Razer Kiyo Analog Stereo  (capture_FL capture_FR)" "$TMP/stderr.txt" || fail "available ports not listed"
grep -qF "names are case-sensitive: did you mean \"$Y-63:playback_FL\"?" "$TMP/stderr.txt" || fail "case hint"
grep -qF 'bananasplit: warning: round trip 17.3 ms at 128 frames (goal < 10 ms): `bananasplit --low-latency`' "$TMP/stderr.txt" || fail "latency advice"
LD_LIBRARY_PATH="$HERE" "$BIN" --list-ports --config "$TMP/bananasplit.ini" > "$TMP/ports.txt" || fail "--list-ports"
cat "$TMP/ports.txt"
grep -qF "      connect = ^$Y:" "$TMP/ports.txt" || fail "--list-ports: connect line"
grep -qF "physical · default input · PipeWire node alsa_input.usb-Blue_Yeti-00.analog-stereo" "$TMP/ports.txt" || fail "--list-ports: tags"
grep -qF "@default  → strips: \"$Y\"" "$TMP/ports.txt" || fail "--list-ports: @default"
grep -qF "          → buses:  \"$Y-63\"" "$TMP/ports.txt" || fail "--list-ports: @default bus"
echo "E2E PASS: @physical/@default pick the default mic, not the webcam; misses explain themselves; latency advice"
unset PACTL_DEVICES MOCKJACK_DEVICES MOCKJACK_LATENCY

# 4c: --low-latency installs the contrib files, applies them live, and undoes it
export PWMETA_LOG="$TMP/pwmeta.log"
"$BIN" --low-latency > "$TMP/ll.txt" || fail "--low-latency"
cmp -s "$TMP/pipewire/pipewire.conf.d/10-bananasplit-lowlatency.conf" "$HERE/../../contrib/pipewire/pipewire.conf.d/10-bananasplit-lowlatency.conf" || fail "graph conf"
cmp -s "$TMP/pipewire/pipewire-pulse.conf.d/10-bananasplit-lowlatency.conf" "$HERE/../../contrib/pipewire/pipewire-pulse.conf.d/10-bananasplit-lowlatency.conf" || fail "pulse conf"
grep -q "^pw-metadata -n settings 0 clock.max-quantum 128$" "$PWMETA_LOG" || fail "not applied live"
"$BIN" --low-latency | grep -q "^already installed" || fail "second run"
"$BIN" --low-latency off > "$TMP/ll.txt" || fail "--low-latency off"
[ -e "$TMP/pipewire/pipewire.conf.d/10-bananasplit-lowlatency.conf" ] && fail "off did not remove the file"
grep -q "^pw-metadata -n settings -d 0 clock.max-quantum$" "$PWMETA_LOG" || fail "off not applied live"
echo "E2E PASS: --low-latency installs, applies live, and undoes"
rm -rf "$TMP"

# 4d: SIGKILL: the watchdog removes the cables; --cleanup refuses while BananaSplit runs and sweeps after
TMP=$(mktemp -d)
export PACTL_LOG="$TMP/pactl.log" MOCKJACK_REPORT="$TMP/report.txt" MOCKJACK_CABLES=1 XDG_CONFIG_HOME="$TMP"
LD_LIBRARY_PATH="$HERE" "$BIN" --headless 2>"$TMP/stderr.txt" &
PID=$!
trap 'rc=$?; kill $PID 2>/dev/null; exit $rc' EXIT
i=0; until [ "$(grep -c '^pactl load-module' "$PACTL_LOG" 2>/dev/null)" = 8 ]; do i=$((i+1)); [ $i -lt 100 ] || fail "4d: cables not created"; sleep 0.05; done
[ -n "$(reapers)" ] || fail "no watchdog running"
"$BIN" --cleanup 2>"$TMP/cleanup.txt" && fail "--cleanup ran while BananaSplit was running"
grep -q "is running" "$TMP/cleanup.txt" || fail "--cleanup message"
[ "$(grep -c '^pactl unload-module 1' "$PACTL_LOG")" -eq 0 ] || fail "--cleanup touched a running BananaSplit"
kill -KILL $PID; wait $PID 2>/dev/null || true; trap - EXIT
i=0; while [ -n "$(reapers)" ]; do i=$((i+1)); [ $i -lt 100 ] || fail "watchdog did not finish"; sleep 0.05; done
cat "$TMP/stderr.txt"
[ "$(grep -c '^pactl unload-module 1[0-9][0-9]$' "$PACTL_LOG")" -eq 8 ] || fail "watchdog did not remove the 8 cables"
grep -q "bananasplit_" "$PACTL_LOG.modules" && fail "cables left after SIGKILL"
grep -q "someone_else" "$PACTL_LOG.modules" || fail "watchdog removed a module that is not ours"
grep -q "removed 8 virtual cables of \"BananaSplit\" (BananaSplit did not exit cleanly)" "$TMP/stderr.txt" || fail "watchdog message"
# watchdog killed too (systemctl kill -s KILL): --cleanup / ExecStopPost
printf '300\tmodule-null-sink\tsink_name=bananasplit_out_b2 channels=2\n' >> "$PACTL_LOG.modules"
"$BIN" --cleanup | grep -q "removed 1 virtual cable of \"BananaSplit\"" || fail "--cleanup"
grep -q "bananasplit_" "$PACTL_LOG.modules" && fail "--cleanup left a cable"
echo "E2E PASS: after SIGKILL the watchdog removed all 8 cables; --cleanup refuses while running, sweeps after"
rm -rf "$TMP"

# ---- phase 5: VBAN against an independent peer (written from VB-Audio's spec), and the EQ ----
# The peer plays Voicemeeter: a 1 kHz sine at 44.1 kHz, 24-bit, in 512-sample bursts. bananasplit
# adds it to strip 4, whose EQ boosts 1 kHz by 6 dB, and sends strip 4 back (48 kHz, 16-bit).
# The peer checks every header field and that the audio is a clean 1 kHz sine at 0.9976.
command -v python3 >/dev/null || fail "phase 5 needs python3"
TMP=$(mktemp -d)
unset MOCKJACK_CABLES PACTL_DEVICES MOCKJACK_DEVICES MOCKJACK_LATENCY
export PACTL_LOG="$TMP/pactl.log" XDG_CONFIG_HOME="$TMP" MOCKJACK_REPORT="$TMP/report.txt" MOCKJACK_REALTIME=1 MOCKJACK_CYCLES=100000
cat > "$TMP/bananasplit.ini" <<'INI'
[global]
create_cables = false
[strip1]
connect =
[strip4]
name = Network
routes = none
connect =
eq = on
eq3 = bell 1000 +6 1
[bus.A1]
connect =
[vban]
enabled = true
port = 16990
[vban.in1]
on = true
name = voicemeeter
from = 127.0.0.1
strip = 4
buffer_ms = 40
[vban.out1]
on = true
name = Stream1
to = 127.0.0.1
port = 16991
source = 4
INI
LD_LIBRARY_PATH="$HERE" "$BIN" --headless --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
trap 'rc=$?; kill $PID 2>/dev/null; exit $rc' EXIT
i=0; until grep -q "vban: listening on UDP port 16990" "$TMP/stderr.txt"; do i=$((i+1)); [ $i -lt 100 ] || fail "5: VBAN did not start"; sleep 0.05; done
python3 "$HERE/vban_peer.py" 16990 16991 8 0.99763 || { cat "$TMP/stderr.txt"; fail "VBAN peer"; }
kill -INT $PID; RC=0; wait $PID || RC=$?; trap - EXIT
cat "$TMP/stderr.txt"
[ $RC -eq 0 ] || fail "5: exit code $RC"
grep -q 'vban in1 "voicemeeter" -> strip 4: receiving 44100 Hz, 2 ch, 24-bit from 127.0.0.1$' "$TMP/stderr.txt" || fail "5: incoming stream not logged"
grep -q 'vban out1 "Stream1": sending 4 to 127.0.0.1:16991 · 48 kHz · stereo · 16-bit$' "$TMP/stderr.txt" || fail "5: outgoing stream not logged"
[ "$(grep -c '^SIGNAL' "$MOCKJACK_REPORT")" -eq 0 ] || fail "5: strip 4 is routed nowhere, yet something reached an output"
echo "E2E PASS: VBAN in (44.1 kHz 24-bit, resampled) and out (48 kHz 16-bit) with an independent peer; EQ +6 dB at 1 kHz"
rm -rf "$TMP"

# ---- phase 6: PipeWire's libjack when the system's finds no server (2.6) ----
# Debian/Ubuntu: libjack.so.0 is often JACK2's, with no jackd running, while
# PipeWire keeps its own libjack.so.0 in a private folder (pw-jack's). Both
# mocks carry the real SONAME, so the loader has to keep them apart.
unset MOCKJACK_REALTIME MOCKJACK_IMPULSE_CYCLE MOCKJACK_CYCLES
TMP=$(mktemp -d); mkdir -p "$TMP/sys" "$TMP/pw" "$TMP/run"
gcc -shared -fPIC -O2 -Wl,-soname,libjack.so.0 -o "$TMP/sys/libjack.so.0" "$HERE/mockjack.c" -lpthread -lm
gcc -shared -fPIC -O2 -Wl,-soname,libjack.so.0 -DMOCK_PIPEWIRE -o "$TMP/pw/libjack.so.0" "$HERE/mockjack.c" -lpthread -lm
printf '[global]\ncreate_cables = false\n' > "$TMP/bananasplit.ini"
export MOCKJACK_REPORT="$TMP/report.txt" MOCKJACK_REPORT_PW="$TMP/pw.txt" MOCKJACK_NOSERVER=1 BANANASPLIT_PIPEWIRE_LIBJACK="$TMP/pw/libjack.so.0"
# PipeWire not running: no fallback, the usual message
XDG_RUNTIME_DIR="$TMP/run" LD_LIBRARY_PATH="$TMP/sys" "$BIN" --headless --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" && fail "6: started without a server"
grep -q "cannot open JACK client" "$TMP/stderr.txt" || fail "6: wrong error: $(cat "$TMP/stderr.txt")"
[ -f "$TMP/pw.txt" ] && fail "6: used PipeWire's libjack although PipeWire is not running"
# PipeWire running (its socket exists): PipeWire's libjack takes over
touch "$TMP/run/pipewire-0"
XDG_RUNTIME_DIR="$TMP/run" LD_LIBRARY_PATH="$TMP/sys" "$BIN" --headless --config "$TMP/bananasplit.ini" 2>"$TMP/stderr.txt" &
PID=$!
trap 'rc=$?; kill $PID 2>/dev/null; exit $rc' EXIT
sleep 2
kill -INT $PID; RC=0; wait $PID || RC=$?; trap - EXIT
cat "$TMP/stderr.txt"
[ $RC -eq 0 ] || fail "6: exit code $RC"
grep -q "^PIPEWIRE LIBJACK BananaSplit$" "$TMP/pw.txt" || fail "6: did not fall back to PipeWire's libjack"
grep -q "^CYCLES [1-9]" "$MOCKJACK_REPORT" || fail "6: the mixer did not run on PipeWire's libjack"
grep -q "Cannot connect\|cannot open JACK client" "$TMP/stderr.txt" && fail "6: JACK2's error messages leaked out"
LD_LIBRARY_PATH="$TMP/sys" XDG_RUNTIME_DIR="$TMP/run" "$BIN" --list-ports --config "$TMP/bananasplit.ini" > "$TMP/ports.txt" || fail "6: --list-ports"
grep -q "system" "$TMP/ports.txt" || fail "6: --list-ports did not use PipeWire's libjack"
unset MOCKJACK_NOSERVER BANANASPLIT_PIPEWIRE_LIBJACK MOCKJACK_REPORT_PW
echo "E2E PASS: with PipeWire running, PipeWire's own libjack is used when the system's finds no JACK server (no pw-jack needed)"
rm -rf "$TMP"

# ---- phase 7: the desktop app's housekeeping (2.6) ----
# Port 8790 busy: the GUI moves to another port and notes it, and launching
# it again and --quit still find it. An update replaces the program while it
# runs: "Start with the system" must name the new file, not "… (deleted)".
TMP=$(mktemp -d); mkdir -p "$TMP/home/.config" "$TMP/run" "$TMP/bin"
printf '[global]\ncreate_cables = false\n' > "$TMP/bananasplit.ini"
cp "$BIN" "$TMP/bin/bananasplit"
export MOCKJACK_REPORT="$TMP/report.txt"
python3 -c "import socket, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
try:
    s.bind(('127.0.0.1', 8790)); s.listen()
except OSError:
    pass  # busy already: just as good
time.sleep(120)" &
BLOCK=$!
sleep 0.3
# exec: in the background, $! must be BananaSplit itself, not a subshell
app() { exec env HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" XDG_RUNTIME_DIR="$TMP/run" LD_LIBRARY_PATH="$HERE" "$TMP/bin/bananasplit" "$@" --config "$TMP/bananasplit.ini"; }
app --gui --no-open 2>"$TMP/stderr.txt" >/dev/null &
PID=$!
trap 'rc=$?; kill $PID $BLOCK 2>/dev/null; exit $rc' EXIT
NOTE="$TMP/run/bananasplit/BananaSplit.port"
i=0; until [ -s "$NOTE" ]; do i=$((i+1)); [ $i -lt 100 ] || fail "7: no port note"; sleep 0.1; done
P=$(cat "$NOTE")
[ "$P" != 8790 ] || fail "7: port 8790 was busy"
curl -sf "http://127.0.0.1:$P/api/ping" >/dev/null || fail "7: nothing on the noted port $P"
grep -q "port 8790 is busy, using $P instead" "$TMP/stderr.txt" || fail "7: no message about the busy port"
(app --gui --no-open) 2>"$TMP/second.txt" || fail "7: second launch failed: $(cat "$TMP/second.txt")"
grep -q "already running; open http://127.0.0.1:$P/" "$TMP/second.txt" || fail "7: second launch did not find the GUI on $P: $(cat "$TMP/second.txt")"
# a second mixer with its own client name does not take this GUI for its own
printf '[global]\nclient_name = Other\ncreate_cables = false\n' > "$TMP/other.ini"
(exec env HOME="$TMP/home" XDG_RUNTIME_DIR="$TMP/run" LD_LIBRARY_PATH="$HERE" "$TMP/bin/bananasplit" --quit --config "$TMP/other.ini") 2>/dev/null \
  && fail "7: --quit for client Other stopped the GUI of client BananaSplit"
curl -sf "http://127.0.0.1:$P/api/ping" >/dev/null || fail "7: the GUI is gone after another client's --quit"
# the update: a new file at the same place (as dpkg and install.sh do it)
cp "$TMP/bin/bananasplit" "$TMP/bin/.new" && mv "$TMP/bin/.new" "$TMP/bin/bananasplit"
readlink "/proc/$PID/exe" | grep -q "(deleted)" || fail "7: the update simulation did not replace the file"
[ "$(curl -s -o "$TMP/resp" -w '%{http_code}' -H 'X-BananaSplit: 1' --data 'op=autostart&v=1' "http://127.0.0.1:$P/api/cmd")" = 200 ] || fail "7: autostart: $(cat "$TMP/resp")"
AUTO="$TMP/home/.config/autostart/bananasplit.desktop"
grep -qx "Exec=$TMP/bin/bananasplit --gui --hidden" "$AUTO" || fail "7: autostart Exec after an update: $(grep Exec "$AUTO")"
grep -qx "TryExec=$TMP/bin/bananasplit" "$AUTO" || fail "7: autostart TryExec"
[ "$(curl -s -o /dev/null -w '%{http_code}' -H 'X-BananaSplit: 1' --data 'op=autostart&v=0' "http://127.0.0.1:$P/api/cmd")" = 200 ] || fail "7: autostart off"
[ ! -e "$AUTO" ] || fail "7: autostart file not removed"
# Added to the app menu, then Start with the system: the autostart entry
# starts the copy the menu starts, not this one (a download folder, say),
# whichever was clicked first.
[ "$(curl -s -o "$TMP/resp" -w '%{http_code}' -H 'X-BananaSplit: 1' --data 'op=install' "http://127.0.0.1:$P/api/cmd")" = 200 ] || fail "7: install: $(cat "$TMP/resp")"
[ "$(curl -s -o "$TMP/resp" -w '%{http_code}' -H 'X-BananaSplit: 1' --data 'op=autostart&v=1' "http://127.0.0.1:$P/api/cmd")" = 200 ] || fail "7: autostart after install: $(cat "$TMP/resp")"
grep -qx "Exec=$TMP/home/.local/bin/bananasplit --gui --hidden" "$AUTO" || fail "7: autostart after Add to the app menu: $(grep Exec "$AUTO")"
env HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" "$TMP/home/.local/bin/bananasplit" --uninstall >/dev/null 2>&1 || fail "7: uninstall"
[ ! -e "$AUTO" ] && [ ! -e "$TMP/home/.local/bin/bananasplit" ] || fail "7: uninstall left the autostart entry or the copy"
(app --quit) || fail "7: --quit did not find the GUI on the noted port"
RC=0; wait $PID || RC=$?; trap - EXIT
[ $RC -eq 0 ] || fail "7: exit code $RC after --quit"
[ ! -e "$NOTE" ] || fail "7: the port note was left behind"
(app --quit) 2>"$TMP/quit2.txt" && fail "7: --quit without a running GUI succeeded"
grep -q "no BananaSplit GUI is running" "$TMP/quit2.txt" || fail "7: --quit message: $(cat "$TMP/quit2.txt")"
kill $BLOCK 2>/dev/null; wait $BLOCK 2>/dev/null || true
echo "E2E PASS: with port 8790 busy, launching again and --quit find the GUI; after an update, Start with the system names the new file"
rm -rf "$TMP"

# ---- phase 8: recordings play at their own speed (2.7) ----
# PipeWire's libjack says 48 kHz (the PIPEWIRE_LATENCY denominator) until the
# graph runs the mixer at 44.1 kHz: the files must say 44.1 kHz, or they play
# 8.8 % too fast. And when the server changes its rate during a take
# (pw-metadata clock.force-rate), the take goes on in a second file.
python3 "$HERE/rec_rate.py" "$BIN" "$HERE" 2.0 1 MOCKJACK_RATE=44100 MOCKJACK_PRE_RATE=48000 || fail "8: recording at 44.1 kHz"
# 3 s: the take continues in a file of its own after the switch (a take of
# its own in the list), long enough to time the player over a second
python3 "$HERE/rec_rate.py" "$BIN" "$HERE" 3.0 2 MOCKJACK_RATE=48000 MOCKJACK_RATE_SWITCH=800:96000 || fail "8: rate change during a take"
python3 "$HERE/rec_rate.py" "$BIN" "$HERE" 2.0 1 MOCKJACK_RATE=48000 MOCKJACK_PRE_RATE=44100 MOCKJACK_NO_SRATE_CB=1 || fail "8: libjack without the rate callback"
echo "E2E PASS: recordings carry the rate the server really runs at, play at 1.0x, and follow a rate change"

# ---- phase 9: the channels' menus, cables on every channel, apps (2.7) ----
python3 "$HERE/menus.py" "$BIN" "$HERE" || fail "9: channel menus"
echo "E2E PASS: cables switched on any channel while running; apps moved into strips and buses; devices connected and saved"

# ---- phase 10: the fixes of 2.7.1 ----
# the desktop's default device moving, apps listed once, a headset's mono mic,
# what `pactl subscribe` inherits, cables removed behind BananaSplit's back,
# two instances whose names differ only in case, renumbered JACK names, the
# default input's mark, the menus' answers and tooltips, takes that continue
# in new files, and the audio server shutting down
python3 "$HERE/fixes.py" "$BIN" "$HERE" || fail "10: the 2.7.1 fixes"
echo "E2E PASS: the 2.7.1 fixes (defaults followed, cables remade, numbered names, settings saved on server shutdown, ...)"

# ---- phase 11: the updater (2.8.0) ----
# a stand-in for GitHub on this computer: nothing is asked while automatic
# updates are off (the default); Check now, a tampered download refused,
# Install, Restart now, automatic updates, GitHub's rate limit, a folder it
# cannot write to, and `bananasplit --update`
python3 "$HERE/update.py" "$BIN" "$HERE" || fail "11: the updater"
echo "E2E PASS: the updater checks only when asked (or when switched on), refuses a tampered file, installs, restarts"
