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

REC_FORMAT=mp3 records MP3 (2.8.4): the files must say the MP3 rate the
graph's rate is stored at (96 kHz is converted to 48 kHz), and their LAME
tags must give as much time as went by. The tone is read with libmpg123 if
it is installed, else with the ffmpeg command, else not at all (said so).

usage: rec_rate.py BINARY MOCKDIR SECONDS EXPECT_FILES [ENV=VALUE ...] [REC_FORMAT=mp3]
"""
import array
import ctypes
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
import urllib.request
import wave

binary, mock, secs, expect_files = sys.argv[1], sys.argv[2], float(sys.argv[3]), int(sys.argv[4])
extra = dict(a.split("=", 1) for a in sys.argv[5:])
fmt = extra.pop("REC_FORMAT", "wav24")
ext = ".mp3" if fmt == "mp3" else ".wav"
port = 19300 + random.randint(0, 300)
home = tempfile.mkdtemp(prefix="bs-recrate-")
rec = os.path.join(home, "rec")
os.makedirs(os.path.join(home, ".config/bananasplit"))
with open(os.path.join(home, ".config/bananasplit/bananasplit.ini"), "w") as f:
    f.write(f"[global]\ncreate_cables = false\n[strip1]\nconnect = ^system:capture_\nroutes = A1\nmono = false\n"
            f"[recorder]\nfolder = {rec}\nlinks = 1\nformat = {fmt}\nbitrate = 320\nplay_to = A2\n")
env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=os.path.join(home, ".config"), LD_LIBRARY_PATH=mock,
           MOCKJACK_REPORT=os.path.join(home, "report.txt"), MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000",
           MOCKJACK_TONE="1000", **extra)
for k in ("DISPLAY", "WAYLAND_DISPLAY", "PIPEWIRE_LATENCY"):
    env.pop(k, None)
log = open(os.path.join(home, "log.txt"), "w")
proc = subprocess.Popen([binary, "--gui", "--no-open", "--gui-port", str(port)], env=env, stdout=log, stderr=log)
url = f"http://127.0.0.1:{port}"
failed = []
TITLE = "Recording rate (rec_rate.py, %s)" % fmt


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


def mp3_header(path):
    """(rate, frames a decoder gives) from the first frame and its LAME tag"""
    b = open(path, "rb").read(4096)
    rate = [[11025, 12000, 8000], None, [22050, 24000, 16000], [44100, 48000, 32000]][(b[1] >> 3) & 3][(b[2] >> 2) & 3]
    x = 36  # MPEG-1 stereo: the tag follows 32 bytes of side information
    assert b[x:x + 4] in (b"Info", b"Xing"), "no Info frame"
    frames = int.from_bytes(b[x + 8:x + 12], "big")
    d = b[x + 120 + 21:x + 120 + 24]
    delay, padding = (d[0] << 4) | (d[1] >> 4), ((d[1] & 15) << 8) | d[2]
    return rate, frames * 1152 - delay - padding


def mp3_left(path):
    """the left channel as floats, as a gapless decoder plays it, or None"""
    try:
        lib = ctypes.CDLL("libmpg123.so.0")
        lib.mpg123_new.restype = ctypes.c_void_p
        lib.mpg123_new.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.c_int)]
        lib.mpg123_param.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_long, ctypes.c_double]
        lib.mpg123_format_none.argtypes = [ctypes.c_void_p]
        lib.mpg123_format.argtypes = [ctypes.c_void_p, ctypes.c_long, ctypes.c_int, ctypes.c_int]
        lib.mpg123_open.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.mpg123_read.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
        lib.mpg123_close.argtypes = [ctypes.c_void_p]
        lib.mpg123_delete.argtypes = [ctypes.c_void_p]
        err = ctypes.c_int(0)
        mh = lib.mpg123_new(None, ctypes.byref(err))
        lib.mpg123_param(mh, 2, 0x20 | 0x40 | 0x400 | 0x8, 0.0)  # add flags: quiet, gapless, float, stereo
        lib.mpg123_format_none(mh)
        for r in (32000, 44100, 48000):
            lib.mpg123_format(mh, r, 2, 0x200)
        lib.mpg123_open(mh, path.encode())
        out = array.array("f")
        buf = ctypes.create_string_buffer(1 << 16)
        done = ctypes.c_size_t(0)
        while True:
            r = lib.mpg123_read(mh, buf, len(buf), ctypes.byref(done))
            out.frombytes(buf.raw[:done.value])
            if r not in (0, -11):
                break
        lib.mpg123_close(mh)
        lib.mpg123_delete(mh)
        return list(out[0::2])
    except OSError:
        pass
    try:
        raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-ac", "2", "-"],
                             capture_output=True, check=True).stdout
        out = array.array("f")
        out.frombytes(raw)
        return list(out[0::2])
    except (OSError, subprocess.CalledProcessError):
        return None


def tone_hz(path):
    """(header rate, frames, frequency of the tone at the header's rate)"""
    if path.endswith(".mp3"):
        sr, n = mp3_header(path)
        left = mp3_left(path)
        if left is None:
            print("note: neither libmpg123 nor ffmpeg is installed; the tone is not read")
            return sr, n, 1000.0
        check(len(left) == n, f"{os.path.basename(path)}: a decoder plays {len(left)} frames, the LAME tag says {n}")
        left = [v * 8388608.0 for v in left]
    else:
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
    def take_order(path):
        # `<stamp>_mix` first, then its continuation `<stamp>-2_mix`
        m = re.match(r"(\d{4}-\d\d-\d\d_\d\d-\d\d-\d\d)(?:-(\d+))?_", os.path.basename(path))
        return (m.group(1), int(m.group(2) or 1)) if m else (os.path.basename(path), 0)
    files = sorted((os.path.join(rec, f) for f in os.listdir(rec) if f.endswith(ext)), key=take_order)
    check(len(files) == expect_files, f"{expect_files} file(s) recorded: {[os.path.basename(f) for f in files]}")
    total = 0.0
    rates = []
    for f in files:
        sr, n, hz = tone_hz(f)
        rates.append(sr)
        total += n / sr
        check(abs(hz - 1000) < 3, f"{os.path.basename(f)}: {sr} Hz header, the tone reads {hz:.1f} Hz (1.000x = 1000 Hz)")
    # the rate of each file: the graph's, or for MP3 the one it is stored at
    graph = [rate] + ([int(extra["MOCKJACK_RATE_SWITCH"].split(":")[1])] if expect_files > 1 else [])
    stored = {96000: 48000, 192000: 48000, 88200: 44100}
    want = [stored.get(r, r) if fmt == "mp3" else r for r in graph]
    check(rates == want, f"the files' rates are {rates} ({want} expected)")
    check(abs(total - wall) < 0.25, f"the files hold {total:.2f} s for {wall:.2f} s of recording")
    check(abs(st["secs"] - total) < 0.05, f"the recorder says {st['secs']:.2f} s")
    if expect_files > 1:
        check(bool(st.get("note")) and "kHz" in st["note"], f"the GUI says why there is a second file: {st.get('note')!r}")
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
        # The position comes from the state, a snapshot of up to one GUI
        # frame (33 ms) before, and the mock server runs its cycles in
        # bursts: two readings 1 s apart were off by up to 6 %. The slope of
        # a least-squares line through readings every 40 ms is not.
        dur = state()["player"]["dur"]
        until = t0 + max(0.3, min(1.0, dur - p0 - 0.25))
        pts = [(t0, p0)]
        while time.time() < until:
            time.sleep(0.04)
            p = state()["player"]
            if p["state"] != "playing":
                break
            pts.append((time.time(), p["pos"]))
        n = len(pts)
        mt, mp = sum(t for t, _ in pts) / n, sum(p for _, p in pts) / n
        var = sum((t - mt) ** 2 for t, _ in pts)
        speed = sum((t - mt) * (p - mp) for t, p in pts) / var if n > 2 and var > 0 else 0.0
        check(abs(speed - 1.0) < 0.06, f"the player runs at {speed:.3f}x ({n} readings)")
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
