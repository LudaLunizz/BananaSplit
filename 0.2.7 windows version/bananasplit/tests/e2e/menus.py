#!/usr/bin/env python3
"""End-to-end check of the channels' menus with virtual cables and apps (2.7).

Runs BananaSplit (--gui) on the mock JACK server with the fake pactl (its
devices, and Discord, Firefox and OBS as apps), drives the menus over HTTP
exactly like the page does, and checks what BananaSplit asks the sound
server to do and what it saves:

* a cable switched on and off on A1 (a hardware bus by default) while it runs;
* an app moved into strip 3's cable and back to the default output;
* a recording app given A1's mix (A1's cable is switched on for it);
* a cable switched off: PipeWire's move of its apps shows in the menu;
* every cable removed on quit, and `cable =` saved.

usage: menus.py BINARY E2EDIR
"""
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

binary, here = sys.argv[1], sys.argv[2]
port = 19700 + random.randint(0, 200)
home = tempfile.mkdtemp(prefix="bs-menus-")
log_path = os.path.join(home, "pactl.log")
cfg_path = os.path.join(home, "bananasplit", "bananasplit.ini")
os.makedirs(os.path.dirname(cfg_path))
with open(cfg_path, "w") as f:
    f.write(f"[global]\ncreate_cables = true\n[recorder]\nfolder = {home}/rec\n")
env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=home, LD_LIBRARY_PATH=here, PATH=os.path.join(here, "fakebin") + ":" + os.environ["PATH"],
           PACTL_LOG=log_path, PACTL_DEVICES="1", PACTL_STREAMS="1", MOCKJACK_CABLES="1", MOCKJACK_DEVICES="1",
           MOCKJACK_REPORT=os.path.join(home, "report.txt"), MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000")
for k in ("DISPLAY", "WAYLAND_DISPLAY"):
    env.pop(k, None)
out = open(os.path.join(home, "log.txt"), "w")
proc = subprocess.Popen([binary, "--gui", "--no-open", "--gui-port", str(port)], env=env, stdout=out, stderr=out)
url = f"http://127.0.0.1:{port}"
failed = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failed.append(what)


def post(**kw):
    data = "&".join(f"{k}={urllib.request.quote(str(v))}" for k, v in kw.items()).encode()
    r = urllib.request.Request(url + "/api/cmd", data=data,
                               headers={"X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded"})
    try:
        with urllib.request.urlopen(r, timeout=8) as resp:
            return resp.status, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def get(path):
    return json.loads(urllib.request.urlopen(url + path, timeout=8).read())


def until(pred, what, secs=5.0, path="/api/devices"):
    t0 = time.time()
    d = None
    while time.time() - t0 < secs:
        try:
            d = get(path)
            if pred(d):
                check(True, what)
                return d
        except (OSError, KeyError, IndexError, ValueError):
            pass
        time.sleep(0.1)
    check(False, what)
    return d


def pactl_log():
    return open(log_path).read()


def stream(d, kind, sid):
    return next(x for x in d[kind] if x["id"] == sid)


try:
    for _ in range(100):
        try:
            urllib.request.urlopen(url + "/api/ping", timeout=1)
            break
        except OSError:
            time.sleep(0.1)
    time.sleep(0.8)
    check(pactl_log().count("load-module module-null-sink") == 8, "8 cables at start: the strips' and B1's, B2's")
    d = until(lambda d: len(d["play"]) == 2 and len(d["rec"]) == 1, "the menus list the apps that play and record")
    check(d["pulse"] and d["cables"]["ok"], "apps and cables are available")
    check([c["cable"]["on"] for c in d["ch"]] == [True] * 6 + [False] * 3 + [True] * 2, "which channels have a cable")
    check(d["ch"][6]["cable"]["name"] == "BananaSplit-Out-A1", "A1's cable would be BananaSplit-Out-A1")
    labels = [c["label"] for c in d["strip"] if c["kind"] == "device"]
    check("Yeti Stereo Microphone Analog Stereo" in labels and "Razer Kiyo Analog Stereo" in labels, f"input devices: {labels}")
    check(not any(c["client"].startswith("BananaSplit-") for c in d["strip"] + d["bus"]), "BananaSplit's own cables are not offered as devices")
    check(any(c["default"] for c in d["strip"]), "the default input is marked")

    # A1, a hardware bus, becomes a virtual microphone as well
    st, msg = post(op="cable", ch=6, v=1)
    check(st == 200 and "BananaSplit-Out-A1" in msg, f"A1's cable on: {msg}")
    check(re.search(r"load-module module-null-sink sink_name=bananasplit_out_a1 .*node.description=BananaSplit-Out-A1 media.class=Audio/Source/Virtual", pactl_log()) is not None,
          "A1's cable is a virtual microphone")
    d = until(lambda d: d["ch"][6]["cable"]["on"], "the menu shows A1's cable on")
    until(lambda s: s["ch"][6]["cable"] and s["cables"] == 9, "the mixer counts 9 cables", path="/api/state")
    mod = re.findall(r"^(\d+)\tmodule-null-sink\tsink_name=bananasplit_out_a1", open(log_path + ".modules").read(), re.M)
    st, msg = post(op="cable", ch=6, v=0)
    check(st == 200 and mod and f"pactl unload-module {mod[0]}" in pactl_log(), f"A1's cable off again: {msg}")

    # an app into a strip, and back
    st, msg = post(op="stream", kind="play", id=202, ch=2)
    check(st == 200 and "Firefox now plays into strip 3" in msg, f"Firefox into strip 3: {msg}")
    check("pactl move-sink-input 202 bananasplit_in3" in pactl_log(), "moved into strip 3's cable")
    until(lambda d: stream(d, "play", 202)["ch"] == 2, "the menu shows Firefox in strip 3")
    until(lambda s: s["ch"][2]["dev"] == "Firefox", "strip 3's menu button says Firefox", path="/api/state")
    st, msg = post(op="stream", kind="play", id=202, ch="none")
    check(st == 200 and "pactl move-sink-input 202 alsa_output.usb-Blue_Yeti-00.analog-stereo" in pactl_log(), f"and back to the default output: {msg}")
    until(lambda d: stream(d, "play", 202)["ch"] == -1, "the menu shows Firefox out of strip 3")
    st, msg = post(op="stream", kind="play", id=202, ch=7)
    check(st == 400, f"a playing app cannot go into a bus: {msg}")

    # a recording app takes A1's mix: A1 gets its cable for it
    st, msg = post(op="stream", kind="rec", id=301, ch=6)
    check(st == 200 and "OBS now records A1" in msg, f"OBS records A1: {msg}")
    check("pactl move-source-output 301 bananasplit_out_a1" in pactl_log(), "moved to A1's virtual microphone")
    d = until(lambda d: d["ch"][6]["cable"]["on"] and stream(d, "rec", 301)["ch"] == 6, "A1's cable is on and OBS records it")
    # switching the cable off: PipeWire moves OBS to the default microphone
    st, msg = post(op="cable", ch=6, v=0)
    until(lambda d: not d["ch"][6]["cable"]["on"] and stream(d, "rec", 301)["ch"] == -1, "A1's cable off: OBS is back on the default microphone")

    # devices: connect, the config line, disconnect
    st, msg = post(op="dev", ch=0, client="Razer Kiyo Analog Stereo", v=1)
    check(st == 200, f"strip 1 takes in the webcam: {msg}")
    until(lambda d: "Razer Kiyo Analog Stereo" in d["ch"][0]["conn"] and "^Razer Kiyo Analog Stereo:" in d["ch"][0]["connect"], "connected, and kept as connect = ^Razer Kiyo Analog Stereo:")
    st, msg = post(op="devdefault", ch=0, v=0)
    until(lambda d: "@default" not in d["ch"][0]["connect"] and "Yeti Stereo Microphone Analog Stereo" not in d["ch"][0]["conn"], "Default input off: the default mic is disconnected")
    st, msg = post(op="dev", ch=0, client="Razer Kiyo Analog Stereo", v=0)
    until(lambda d: d["ch"][0]["conn"] == [] and d["ch"][0]["connect"] == [], "the webcam off again: strip 1 takes in only its cable")
    st, msg = post(op="cable", ch=10, v=0)
    check(st == 200, f"B2's cable off: {msg}")
    st, msg = post(op="devpattern", ch=0, pattern="^nothing:")
    check(st == 400, f"removing a line strip 1 does not have is refused: {msg}")
    st, msg = post(op="cable", ch=11, v=1)
    check(st == 400, "a channel that does not exist is refused")

    st, _ = post(op="quit")
    proc.wait(10)
    check(proc.returncode == 0, "quit cleanly")
    log = pactl_log()
    loads = len(re.findall(r"^pactl load-module", log, re.M))
    unloads = len(re.findall(r"^pactl unload-module 1\d\d$", log, re.M))
    check(loads == unloads, f"every cable made was removed ({loads} made, {unloads} removed)")
    check("bananasplit_" not in open(log_path + ".modules").read(), "no cable left behind")
    ini = open(cfg_path).read()
    b2 = ini.split("[bus.B2]")[1].split("\n[")[0]
    s1 = ini.split("[strip1]")[1].split("\n[")[0]
    check("cable = off" in b2, "B2's cable switch is saved")
    check("connect =\n" in s1 and "Razer" not in s1, "strip 1's connections are saved as they were left")
    check("cable =" not in ini.split("[bus.A1]")[1].split("\n[")[0], "A1 keeps its default (no cable line)")
finally:
    if proc.poll() is None:
        proc.kill()
        proc.wait()
    if failed:
        print(open(os.path.join(home, "log.txt")).read())
print("MENUS " + ("FAIL: " + "; ".join(failed) if failed else "PASS"))
sys.exit(1 if failed else 0)
