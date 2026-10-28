#!/bin/sh
# Starts the built app as a person would, checks that it answers, starts it
# a second time and quits it (CI runs it on GitHub's macOS runners).
#
#   sh tests/launch/smoke.sh dist/BananaSplit.app
#
# It uses a home folder of its own, so the computer's own BananaSplit
# settings and login item are never touched.
set -eu

app=$1
exe="$app/Contents/MacOS/bananasplit"
port=8799
work=$(mktemp -d)
export HOME="$work"
log="$work/log.txt"
url="http://127.0.0.1:$port"
pid=

fail() {
    echo "smoke test failed: $1" >&2
    # the app started in the background, if it still runs
    if [ -n "$pid" ]; then
        kill "$pid" 2>/dev/null || true
    fi
    echo "--- the app's output:" >&2
    cat "$log" >&2 || true
    exit 1
}

"$exe" --version | grep -q '^BananaSplit .*macOS' || fail "--version"
"$exe" --print-default-config | grep -q '^\[global\]' || fail "--print-default-config"
"$exe" --list-devices > "$work/devices.txt" 2>&1 || fail "--list-devices: $(cat "$work/devices.txt")"
cat "$work/devices.txt"

# Started in the background (as at login): the menu bar icon only.
"$exe" --no-open --gui-port "$port" > "$log" 2>&1 &
pid=$!
i=0
until curl -fsS "$url/api/ping" 2>/dev/null | grep -q bananasplit-gui; do
    i=$((i + 1))
    if [ "$i" -gt 150 ] || ! kill -0 "$pid" 2>/dev/null; then
        fail "it does not answer on $url"
    fi
    sleep 0.2
done
echo "answers on $url"

# Its first state follows a moment after it answers (until then: "{}").
i=0
until curl -fsS "$url/api/state" 2>/dev/null | grep -q '^{"client":"BananaSplit"'; do
    i=$((i + 1))
    if [ "$i" -gt 50 ]; then
        fail "/api/state"
    fi
    sleep 0.2
done
curl -fsS "$url/api/devices" | grep -q '^{"cables":{"ok":' || fail "/api/devices"
curl -fsS "$url/api/cables" | grep -q '^{"cables":\[' || fail "/api/cables"
curl -fsS "$url/api/state" | grep -q '"welcome":' || fail "the welcome flag in /api/state"
curl -fsS "$url/" | grep -q '<title>' || fail "the page"

# Another user on this Mac is turned away (403), by the kernel's socket
# list. Needs sudo without a password (CI has it); skipped otherwise.
if sudo -n -u nobody true 2>/dev/null; then
    code=$(sudo -n -u nobody curl -s -o /dev/null -w '%{http_code}' -H 'X-BananaSplit: 1' "$url/api/state" || true)
    [ "$code" = 403 ] || fail "another user got $code from /api/state, not 403"
    echo "another user: 403, as it should"
else
    echo "skipped the other-user check (no sudo here)"
fi

# A second start asks the running one to show its window, and ends.
"$exe" --gui-port "$port" || fail "a second start"
kill -0 "$pid" 2>/dev/null || fail "the first one ended at the second start"

# Quit: it saves its settings and ends.
"$exe" --quit --gui-port "$port" || fail "--quit"
i=0
while kill -0 "$pid" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -gt 100 ]; then
        fail "it did not quit"
    fi
    sleep 0.2
done
code=0
wait "$pid" || code=$?
pid=
[ "$code" -eq 0 ] || fail "it ended with exit code $code"
test -f "$work/Library/Application Support/bananasplit/bananasplit.ini" || fail "no settings saved"
echo "--- the app's output:"
cat "$log"
rm -rf "$work"
echo "smoke test passed"
