#!/usr/bin/env python3
"""End-to-end check that recordings play at their own speed (2.7).

Runs BananaSplit (--gui, on the mock JACK server) where the graph runs at
MOCKJACK_RATE while libjack says MOCKJACK_PRE_RATE until the first cycle,
as PipeWire's libjack does with PIPEWIRE_LATENCY. A 1 kHz tone comes in on
strip 1. It records over HTTP like the page does and checks every file:
the header's rate is the rate the graph ran at, the tone reads 1000 Hz and
the files hold as much time as went by. With MOCKJACK_RATE_SWITCH the
server changes its rate during the take: the take continues in a second
file at the new rate. Then it plays the take and checks that the player's
position advances at 1.0x.

usage: rec_rate.py BINARY MOCKDIR SECONDS EXPECT_FILES [ENV=VALUE ...]
"""
import json
import os
import random
import subprocess
import sys
import tempfile
import time
import urllib.request
import wave

binary, mock, secs, expect_files = sys.argv[1], sys.argv[2], float(sys.argv[3]), int(sys.argv[4])
extra = dict(a.split("=", 1) for a in sys.argv[5:])
port = 19300 + random.randint(0, 300)
home = tempfile.mkdtemp(prefix="bs-recrate-")
rec = os.path.join(home, "rec")
os.makedirs(os.path.join(home, ".config/bananasplit"))
with open(os.path.join(home, ".config/bananasplit/bananasplit.ini"), "w") as f:
    f.write(f"[global]\ncreate_cables = false\n[strip1]\nconnect = ^system:capture_\nroutes = A1\nmono = false\n"
            f"[recorder]\nfolder = {rec}\nlinks = 1\nformat = wav24\nplay_to = A2\n")
env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=os.path.join(home, ".config"), LD_LIBRARY_PATH=mock,
           MOCKJACK_REPORT=os.path.join(home, "report.txt"), MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000",
           MOCKJACK_TONE="1000", **extra)
for k in ("DISPLAY", "WAYLAND_DISPLAY", "PIPEWIRE_LATENCY"):
    env.pop(k, None)
log = open(os.path.join(home, "log.txt"), "w")
proc = subprocess.Popen([binary, "--gui", "--no-open", "--gui-port", str(port)], env=env, stdout=log, stderr=log)
url = f"http://127.0.0.1:{port}"
failed = []
TITLE = "Recording rate (rec_rate.py)"


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failed.append(what)
        # on GitHub Actions also an annotation, shown on the run's page
        if os.environ.get("GITHUB_ACTIONS") == "true":
            text = what.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
            print(f"::error title={TITLE}::{text}", flush=True)


def cmd(**kw):
    data = "&".join(f"{k}={v}" for k, v in kw.items()).encode()
    r = urllib.request.Request(url + "/api/cmd", data=data,
                               headers={"X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded"})
    return urllib.request.urlopen(r, timeout=5).read().decode()


def state():
    return json.loads(urllib.request.urlopen(url + "/api/state", timeout=5).read())


def tone_hz(path):
    """(header rate, frames, frequency of the tone at the header's rate)"""
    w = wave.open(path)
    sr, n, ch = w.getframerate(), w.getnframes(), w.getnchannels()
    raw = w.readframes(n)
    left = [int.from_bytes(raw[i:i + 3], "little", signed=True) for i in range(0, len(raw), 3 * ch)]
    loud = [i for i, v in enumerate(left) if abs(v) > 100000]
    if len(loud) < 2:
        return sr, n, 0.0
    seg = left[loud[0]:loud[-1]]
    rises = [i for i in range(1, len(seg)) if seg[i - 1] < 0 <= seg[i]]
    if len(rises) < 2:
        return sr, n, 0.0
    return sr, n, (len(rises) - 1) * sr / (rises[-1] - rises[0])


try:
    for _ in range(100):
        try:
            urllib.request.urlopen(url + "/api/ping", timeout=1)
            break
        except OSError:
            time.sleep(0.1)
    time.sleep(0.8)
    rate = int(extra.get("MOCKJACK_RATE", "48000"))
    check(state()["sr"] == rate, f"the GUI shows the graph's rate, {rate} Hz")
    cmd(op="rec", v=1)
    t0 = time.time()
    time.sleep(secs)
    cmd(op="rec", v=0)
    wall = time.time() - t0
    time.sleep(1.0)
    st = state()["rec"]
    files = sorted(os.path.join(rec, f) for f in os.listdir(rec) if f.endswith(".wav"))
    check(len(files) == expect_files, f"{expect_files} file(s) recorded: {[os.path.basename(f) for f in files]}")
    total = 0.0
    for f in files:
        sr, n, hz = tone_hz(f)
        total += n / sr
        check(abs(hz - 1000) < 3, f"{os.path.basename(f)}: {sr} Hz header, the tone reads {hz:.1f} Hz (1.000x = 1000 Hz)")
    check(abs(total - wall) < 0.25, f"the files hold {total:.2f} s for {wall:.2f} s of recording")
    check(abs(st["secs"] - total) < 0.05, f"the recorder says {st['secs']:.2f} s")
    if expect_files > 1:
        check(bool(st.get("note")) and "kHz" in st["note"], f"the GUI says why there is a second file: {st.get('note')!r}")
        rates = [wave.open(f).getframerate() for f in files]
        check(len(set(rates)) == len(rates), f"each file has its own rate: {rates}")
    # the player plays it back at 1.0x (its position follows the clock)
    time.sleep(3.5)  # the take list refreshes every 3 s
    cmd(op="play", key=state()["takes"]["newest"])
    p0 = None
    for _ in range(50):
        p = state()["player"]
        if p["state"] == "playing" and p["pos"] > 0.05:
            p0, t0 = p["pos"], time.time()
            break
        time.sleep(0.05)
    check(p0 is not None, "the take plays")
    if p0 is not None:
        dur = state()["player"]["dur"]
        time.sleep(max(0.3, min(1.0, dur - p0 - 0.25)))
        p1, t1 = state()["player"]["pos"], time.time()
        speed = (p1 - p0) / (t1 - t0)
        check(abs(speed - 1.0) < 0.06, f"the player runs at {speed:.3f}x")
        cmd(op="stop")
finally:
    proc.terminate()
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        proc.kill()
    if failed:
        print(open(os.path.join(home, "log.txt")).read())
    else:
        import shutil
        shutil.rmtree(home, ignore_errors=True)
print("REC RATE " + ("FAIL: " + "; ".join(failed) if failed else "PASS"))
sys.exit(1 if failed else 0)
