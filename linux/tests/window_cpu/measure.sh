#!/bin/sh
# Measures how much CPU the mixer window's web view (WebKitWebProcess) uses
# for a page, against the BananaSplit that is running (port 8790).
#
#   linux/tests/window_cpu/measure.sh [PAGE.html] [SECONDS]
#
# PAGE.html (default: linux/src/gui.html of this checkout) is served on port
# 8793 by serve_page.py, which passes everything else (/api/events, ...) on
# to the running BananaSplit, so a new page can be measured without
# installing it. A second port keeps the window from handing over to the
# running BananaSplit's own window.
#
# NOTE: this OPENS A VISIBLE WINDOW on the desktop for SECONDS + 8 seconds
# (there is no headless display for WebKitGTK here). Ask first on a shared
# desktop. A hidden window (in the tray) uses about 1%, so it must be visible.
#
# 2.8.6 page on a recording mixer: about 96% of one core; 2.8.7: about 35%.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
page=${1:-$here/../../src/gui.html}
secs=${2:-20}
port=8793
python3 -I "$here/serve_page.py" "$port" "$page" &
srv=$!
trap 'kill $srv 2>/dev/null; [ -n "${win:-}" ] && kill "$win" 2>/dev/null; true' EXIT
sleep 1
# the window quits when its stdin closes: keep it open
sleep $((secs + 10)) | bananasplit --window "http://127.0.0.1:$port/" --close-to-tray 1 --size 1780x1000 >/dev/null 2>&1 &
sleep 5
win=$(ps -o pid=,args= -C bananasplit | awk -v p=":$port/" 'index($0, p) { print $1 }' | tail -1)
web=$(ps -o pid=,ppid= -C WebKitWebProcess | awk -v w="$win" '$2 == w { print $1 }')
[ -n "$web" ] || { echo "no web view found for the test window" >&2; exit 1; }
hz=$(getconf CLK_TCK)
a=$(awk '{ print $14 + $15 }' "/proc/$web/stat")
sleep "$secs"
b=$(awk '{ print $14 + $15 }' "/proc/$web/stat")
echo "$(basename "$page"): $(( (b - a) * 100 / (hz * secs) ))% of one core over $secs s"
