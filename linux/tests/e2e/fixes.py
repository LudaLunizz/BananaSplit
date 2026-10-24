#!/usr/bin/env python3
"""End-to-end checks of the 2.7.1 fixes, on the mock JACK server and the
fake pactl (see menus.py for the setup):

* the channels that follow the desktop's default device (`@default`) move
  with it at once, without keeping the old one, and stay put while the
  default is one of BananaSplit's own cables;
* an app that plays or records is listed once in the menus (as an app),
  not again as a JACK program;
* a headset whose mono mic and stereo output share one JACK name gives a
  strip its mic on both sides, never the monitor of its output;
* `pactl subscribe` (it runs as long as BananaSplit) does not inherit
  descriptors that libjack left open;
* a virtual cable removed behind BananaSplit's back (pipewire-pulse
  restarted, or its module was unloaded) is made again, can be switched off
  and on, and BananaSplit never unloads a module by an id it remembered;
  cables gone with a restart come back each time, in one message;
* a channel stays with its device while the default is BananaSplit's own
  cable only while that device is there (and comes back to it), and not
  once it is unticked; `@physical` stays too;
* without a sound server that answers, `pactl subscribe` is not started
  again and again, and ended ones leave nothing behind;
* a second BananaSplit whose name differs only in case leaves the first
  one's cables alone;
* a menu line for a JACK name that PipeWire numbered ("Name-63") still
  connects when the number changes, if only one client fits (never one
  that another line names);
* the menus mark the default input on the input, not on an output's monitor;
* switching on Default input answers about that channel;
* the menu button's tooltip shows what is connected now; renaming a bus
  says nothing about a cable named after it;
* a multi-track take that continues in new files still has its tracks;
* when the audio server shuts down, the settings are saved.

usage: fixes.py BINARY E2EDIR
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
failed = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failed.append(what)


class Mixer:
    """BananaSplit --gui on the mocks, in a home of its own."""

    next_port = 19900 + random.randint(0, 40)

    def __init__(self, ini="", glob="", rec="", log=None, **extra_env):
        """`ini`: more config sections; `glob`, `rec`: more lines for
        [global] and [recorder]; `log`: share another Mixer's fake sound
        server (its pactl log)."""
        self.port = Mixer.next_port
        Mixer.next_port += 1
        self.home = tempfile.mkdtemp(prefix="bs-fixes-")
        self.log = log or os.path.join(self.home, "pactl.log")
        self.cfg = os.path.join(self.home, "bananasplit", "bananasplit.ini")
        os.makedirs(os.path.dirname(self.cfg))
        with open(self.cfg, "w") as f:
            f.write(f"[global]\ncreate_cables = true\n{glob}[recorder]\nfolder = {self.home}/rec\n{rec}{ini}")
        env = dict(os.environ, HOME=self.home, XDG_CONFIG_HOME=self.home, LD_LIBRARY_PATH=here,
                   PATH=os.path.join(here, "fakebin") + ":" + os.environ["PATH"], PACTL_LOG=self.log,
                   PACTL_DEVICES="1", PACTL_STREAMS="1", MOCKJACK_CABLES="1", MOCKJACK_DEVICES="1",
                   MOCKJACK_REPORT=os.path.join(self.home, "report.txt"), MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000")
        env.update(extra_env)
        for k in ("DISPLAY", "WAYLAND_DISPLAY"):
            env.pop(k, None)
        self.env = env
        self.out = open(os.path.join(self.home, "log.txt"), "w")
        self.proc = subprocess.Popen([binary, "--gui", "--no-open", "--gui-port", str(self.port)], env=env, stdout=self.out, stderr=self.out)
        self.url = f"http://127.0.0.1:{self.port}"
        for _ in range(100):
            try:
                urllib.request.urlopen(self.url + "/api/ping", timeout=1)
                break
            except OSError:
                if self.proc.poll() is not None:
                    break
                time.sleep(0.1)
        time.sleep(0.8)

    def post(self, **kw):
        data = "&".join(f"{k}={urllib.request.quote(str(v))}" for k, v in kw.items()).encode()
        r = urllib.request.Request(self.url + "/api/cmd", data=data,
                                   headers={"X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded"})
        try:
            with urllib.request.urlopen(r, timeout=8) as resp:
                return resp.status, resp.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode()

    def get(self, path):
        return json.loads(urllib.request.urlopen(self.url + path, timeout=8).read())

    def until(self, pred, what, secs=4.0, path="/api/devices"):
        t0 = time.time()
        d = None
        while time.time() - t0 < secs:
            try:
                d = self.get(path)
                if pred(d):
                    check(True, what)
                    return d
            except (OSError, KeyError, IndexError, ValueError):
                pass
            time.sleep(0.1)
        check(False, f"{what} (last: {json.dumps(d)[:300] if d is not None else None})")
        return d

    def pactl(self, *args):
        subprocess.run([os.path.join(here, "fakebin", "pactl"), *args], env=self.env, check=True)

    def modules(self):
        """The fake server's modules: (id, name, args)."""
        return [tuple(line.split("\t") + [""])[:3] for line in open(self.log + ".modules").read().splitlines() if line]

    def set_modules(self, mods):
        with open(self.log + ".modules.tmp", "w") as f:
            f.write("".join("\t".join(m) + "\n" for m in mods))
        os.replace(self.log + ".modules.tmp", self.log + ".modules")

    def cable(self, node):
        """Ids of the modules that make cable `node`."""
        return [i for i, name, a in self.modules() if name == "module-null-sink" and f"sink_name={node}" in a.split()]

    def ini(self):
        return open(self.cfg).read()

    def section(self, name):
        parts = self.ini().split(f"[{name}]")
        return parts[1].split("\n[")[0] if len(parts) > 1 else ""

    def stop(self):
        if self.proc.poll() is None:
            try:
                self.post(op="quit")
                self.proc.wait(10)
            except Exception:
                self.proc.kill()
                self.proc.wait()
        return self.proc.returncode

    def stderr(self):
        self.out.flush()
        return open(os.path.join(self.home, "log.txt")).read()


def conn(d, ch):
    return d["ch"][ch]["conn"]


# ---- the desktop's default device ----------------------------------------
m = Mixer(PACTL_MORE_DEVICES="1")
try:
    YETI_OUT, YETI_IN = "Yeti Stereo Microphone Analog Stereo-63", "Yeti Stereo Microphone Analog Stereo"
    m.until(lambda d: conn(d, 6) == [YETI_OUT] and conn(d, 0) == [YETI_IN], "A1 plays on the default output, strip 1 takes the default input")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.pactl("set-default-source", "alsa_input.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: conn(d, 6) == ["system"] and conn(d, 0) == ["system"],
            "a new default output and input: A1 and strip 1 move there at once, the old devices are let go")
    st, _ = m.post(op="reconnect")
    time.sleep(0.6)
    d = m.get("/api/devices")
    check(conn(d, 6) == ["system"] and conn(d, 0) == ["system"], f"another pass keeps only the new ones: {conn(d, 6)} {conn(d, 0)}")
    # a device the channel also has its own line for stays
    st, _ = m.post(op="dev", ch=6, client=YETI_OUT, v=1)
    m.pactl("set-default-sink", "alsa_output.usb-Blue_Yeti-00.analog-stereo")
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "back to the Yeti")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: sorted(conn(d, 6)) == sorted([YETI_OUT, "system"]), "a device ticked in the menu stays when the default moves on")
    st, _ = m.post(op="dev", ch=6, client=YETI_OUT, v=0)
    m.until(lambda d: conn(d, 6) == ["system"] and "^Yeti" not in " ".join(d["ch"][6]["connect"]) and "@default" in d["ch"][6]["connect"],
            "unticking the Yeti drops its line; A1 still follows the default (system)")
    # The README's own setup: the desktop's default output is strip 3's
    # cable. A1 stays on the device it had, not on @physical's first device.
    m.pactl("set-default-sink", "bananasplit_in3")
    time.sleep(1.5)
    d = m.get("/api/devices")
    s = m.get("/api/state")
    check(conn(d, 6) == ["system"], f"the default output is BananaSplit's own cable: A1 stays on system ({conn(d, 6)})")
    check("staying with \"system\"" in s["status"], f"and says so: {s['status']}")
    # the default input off in the menu: what it connected goes too
    st, _ = m.post(op="devdefault", ch=0, v=0)
    m.until(lambda d: conn(d, 0) == [] and "@default" not in d["ch"][0]["connect"], "Default input off: strip 1 lets the default mic go")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- staying with a device: only while it is there, and not once unticked ----
hide = tempfile.mktemp(prefix="bs-hide-")
m = Mixer(PACTL_MORE_DEVICES="1", MOCKJACK_HIDE_FILE=hide)
try:
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "A1 on the default output")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: conn(d, 6) == ["system"], "A1 follows it to system")
    m.pactl("set-default-sink", "bananasplit_in3")
    m.until(lambda s: "staying with \"system\"" in s["status"], "the default output is strip 3's cable: A1 stays with system",
            path="/api/state")
    with open(hide, "w") as f:
        f.write("system\n")  # unplugged
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "system unplugged: A1 takes the other output instead of nothing")
    with open(hide, "w") as f:
        f.write("")  # plugged in again
    m.until(lambda d: conn(d, 6) == ["system"], "plugged in again: A1 goes back to system (and lets the other output go)")
finally:
    check(m.stop() == 0, "quit cleanly")
m = Mixer(PACTL_MORE_DEVICES="1")
try:
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "A1 on the default output")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: conn(d, 6) == ["system"], "A1 follows it to system")
    m.pactl("set-default-sink", "bananasplit_in3")
    m.until(lambda s: "staying with \"system\"" in s["status"], "A1 stays with system", path="/api/state")
    st, msg = m.post(op="dev", ch=6, client="system", v=0)
    check(st == 200, f"untick system in A1's menu while A1 stays with it: {msg}")
    m.post(op="reconnect")
    time.sleep(1.0)
    d = m.get("/api/devices")
    check(conn(d, 6) == [] and "@default" not in d["ch"][6]["connect"],
          f"it stays off after the next pass (the @default line that kept it goes): {conn(d, 6)} {d['ch'][6]['connect']}")
    st, msg = m.post(op="devdefault", ch=6, v=1)
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "Default output on again: A1 takes a fallback")
finally:
    check(m.stop() == 0, "quit cleanly")
m = Mixer(PACTL_MORE_DEVICES="1")
try:
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "A1 on the default output")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: conn(d, 6) == ["system"], "A1 follows it to system")
    m.pactl("set-default-sink", "bananasplit_in3")
    m.until(lambda s: "staying with \"system\"" in s["status"], "A1 stays with system", path="/api/state")
    m.post(op="dev", ch=6, client="system", v=0)
    m.post(op="devdefault", ch=6, v=1)  # at once, before any other pass
    d = m.until(lambda d: conn(d, 6) == [YETI_OUT], "unticked, then Default output on at once: a fallback, not the device just unticked")
finally:
    check(m.stop() == 0, "quit cleanly")
m = Mixer(ini="[bus.A1]\nconnect = @physical\n", PACTL_MORE_DEVICES="1")
try:
    m.until(lambda d: conn(d, 6) == [YETI_OUT], "A1 (@physical) on the default output, a physical one")
    m.pactl("set-default-sink", "alsa_output.pci-0000_00_1f.3.analog-stereo")
    m.until(lambda d: conn(d, 6) == ["system"], "A1 (@physical) follows the default to system")
    m.pactl("set-default-sink", "bananasplit_in3")
    time.sleep(1.2)
    m.post(op="reconnect")
    time.sleep(1.0)
    check(conn(m.get("/api/devices"), 6) == ["system"],
          f"the default is strip 3's cable: A1 stays with system, not the first device listed: {conn(m.get('/api/devices'), 6)}")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- apps are listed once, as apps ---------------------------------------
m = Mixer()
try:
    d = m.until(lambda d: len(d["play"]) == 2 and len(d["rec"]) == 1, "the menus list the apps that play and record")
    progs = [c["client"] for c in d["strip"] + d["bus"] if c["kind"] == "app"]
    check("Firefox" not in progs and "OBS" not in progs, f"Firefox and OBS are not offered again as JACK programs: {progs}")
finally:
    check(m.stop() == 0, "quit cleanly")
m = Mixer(PACTL_STREAMS="")
try:
    d = m.until(lambda d: d["pulse"] and d["play"] == [], "without apps playing")
    progs = [c["client"] for c in d["strip"] if c["kind"] == "app"]
    check(progs == ["Firefox"], f"a JACK program that is no app is still offered: {progs}")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- a headset's mono mic, and its output's monitor in the same client ------
mark = tempfile.mktemp(prefix="bs-jackfd-")
m = Mixer(MOCKJACK_HEADSET="1", MOCKJACK_FD_MARK=mark)
try:
    st, msg = m.post(op="dev", ch=1, client="USB Headset", v=1)
    check(st == 200, f"strip 2 takes in the headset: {msg}")
    time.sleep(0.5)
    st, _ = m.post(op="reconnect")
    time.sleep(1.0)
finally:
    check(m.stop() == 0, "quit cleanly")
rep = open(os.path.join(m.home, "report.txt")).read()
ins = sorted(re.findall(r"^CONN (USB Headset:\S+) -> BananaSplit:in2_([LR])$", rep, re.M))
check(ins == [("USB Headset:capture_MONO", "L"), ("USB Headset:capture_MONO", "R")], f"its mono mic on both sides, no monitor: {ins}")
check("connect = ^USB Headset:" in m.section("strip2"), "and it is kept")
fds = open(m.log + ".subscribe-fds").read()
check(mark not in fds, "pactl subscribe did not inherit a descriptor libjack left open")
os.unlink(mark)

# ---- a cable removed behind BananaSplit's back ----------------------------
m = Mixer(PACTL_REUSE_IDS="1")
try:
    m.until(lambda d: d["ch"][3]["cable"]["on"], "strip 4 has its virtual cable")
    first = m.cable("bananasplit_in4")
    check(len(first) == 1, f"made by one module: {first}")
    # someone unloads it, and the next module anyone loads gets its id (as
    # in pipewire-pulse)
    m.set_modules([x for x in m.modules() if x[0] != first[0]] + [(first[0], "module-loopback", "source=someone")])
    d = m.until(lambda d: len(m.cable("bananasplit_in4")) == 1 and d["ch"][3]["cable"]["on"], "BananaSplit makes strip 4's cable again")
    s = m.get("/api/state")
    check("made it again" in s["status"], f"and says so: {s['status']}")
    st, msg = m.post(op="cable", ch=3, v=0)
    check(st == 200 and m.cable("bananasplit_in4") == [], f"switching it off removes it: {st} {msg}")
    check((first[0], "module-loopback", "source=someone") in m.modules(), "but not the module that has its old id now")
    d = m.get("/api/devices")
    check(not d["ch"][3]["cable"]["on"], "the menu says it is off")
    st, msg = m.post(op="cable", ch=3, v=1)
    check(st == 200 and len(m.cable("bananasplit_in4")) == 1, f"switching it on makes it again: {msg}")
    # removed twice in a row: BananaSplit leaves it off
    for _ in m.cable("bananasplit_in4")[:1]:
        m.pactl("unload-module", m.cable("bananasplit_in4")[0])
        m.until(lambda d: len(m.cable("bananasplit_in4")) == 1 and d["ch"][3]["cable"]["on"], "removed: made again")
    for _ in m.cable("bananasplit_in4")[:1]:
        m.pactl("unload-module", m.cable("bananasplit_in4")[0])
    m.until(lambda d: not d["ch"][3]["cable"]["on"] and m.cable("bananasplit_in4") == [], "removed again at once: BananaSplit leaves it off")
    s = m.get("/api/state")
    check("leaves it off" in s["status"], f"and says so: {s['status']}")
    # pipewire-pulse restarts: all its modules are gone, and `pactl
    # subscribe` loses it; twice within a minute
    want = ["bananasplit_in1", "bananasplit_in2", "bananasplit_in3", "bananasplit_in5", "bananasplit_in6", "bananasplit_out_b1", "bananasplit_out_b2"]
    for n in (1, 2):
        time.sleep(3.0)
        m.set_modules([x for x in m.modules() if x[1] != "module-null-sink"])
        with open(m.log + ".restart", "w") as f:
            f.write(str(n))
        m.until(lambda d: all(len(m.cable(n)) == 1 for n in want) and [c["cable"]["on"] for c in d["ch"]].count(True) == 7,
                f"pipewire-pulse restarted ({n}): the cables that were on are made again", secs=8)
    s = m.get("/api/state")
    check("the virtual cables " in s["status"] and "made them again" in s["status"], f"in one message: {s['status'][:120]}")
    # pipewire-pulse restarts once more, and making the cables again fails
    # for a while (it is not up yet)
    open(m.log + ".loadfail", "w").close()
    time.sleep(1.0)
    m.set_modules([x for x in m.modules() if "sink_name=bananasplit_in1" not in x[2]])
    with open(m.log + ".restart", "w") as f:
        f.write("3")
    time.sleep(4.0)
    d = m.get("/api/devices")
    check(d["ch"][0]["cable"]["on"] and m.cable("bananasplit_in1") == [], "it cannot be made again yet: strip 1 keeps its cable on and tries again")
    os.unlink(m.log + ".loadfail")
    m.until(lambda d: len(m.cable("bananasplit_in1")) == 1 and d["ch"][0]["cable"]["on"], "the server is up again: it is made again", secs=6)
finally:
    check(m.stop() == 0, "quit cleanly")
left = [x for x in m.modules() if x[1] == "module-null-sink" and "sink_name=bananasplit" in x[2]]
check(left == [], f"on quitting, its cables are removed: {left}")
check((first[0], "module-loopback", "source=someone") in m.modules(), "and only its cables")

# ---- the same in --headless mode ------------------------------------------
hl = tempfile.mkdtemp(prefix="bs-fixes-hl-")
os.makedirs(os.path.join(hl, "bananasplit"))
with open(os.path.join(hl, "bananasplit", "bananasplit.ini"), "w") as f:
    f.write("[global]\ncreate_cables = true\n")
hlenv = dict(os.environ, HOME=hl, XDG_CONFIG_HOME=hl, LD_LIBRARY_PATH=here, PATH=os.path.join(here, "fakebin") + ":" + os.environ["PATH"],
             PACTL_LOG=os.path.join(hl, "pactl.log"), MOCKJACK_CABLES="1", MOCKJACK_REPORT=os.path.join(hl, "report.txt"),
             MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000")
def hlmods():
    path = os.path.join(hl, "pactl.log.modules")
    text = open(path).read() if os.path.exists(path) else ""
    return [line.split("\t") for line in text.splitlines() if "sink_name=bananasplit_" in line]


with open(os.path.join(hl, "stderr.txt"), "w") as hlerr:
    p = subprocess.Popen([binary, "--headless"], env=hlenv, stdout=hlerr, stderr=hlerr)
try:
    for _ in range(100):
        if len(hlmods()) == 8:
            break
        time.sleep(0.1)
    in2 = [x[0] for x in hlmods() if "sink_name=bananasplit_in2" in x[2].split()]
    check(len(in2) == 1, f"--headless: strip 2's cable is there: {in2}")
    if in2:
        subprocess.run([os.path.join(here, "fakebin", "pactl"), "unload-module", in2[0]], env=hlenv, check=True)
    back = False
    for _ in range(60):
        back = any("sink_name=bananasplit_in2" in x[2].split() for x in hlmods()) and "made it again" in open(os.path.join(hl, "stderr.txt")).read()
        if back:
            break
        time.sleep(0.1)
    check(back, "--headless: removed behind its back, it is made again (and logged)")
finally:
    p.terminate()
    rc = p.wait(10)
check(rc == 0 and hlmods() == [], f"--headless: stops cleanly and removes its cables (exit code {rc}, left: {hlmods()})")

# ---- no sound server answers: no `pactl subscribe` every 10 s, no leak --------
def headless(home, path, env_extra):
    os.makedirs(os.path.join(home, "bananasplit"), exist_ok=True)
    with open(os.path.join(home, "bananasplit", "bananasplit.ini"), "w") as f:
        f.write("[global]\ncreate_cables = false\n")
    env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=home, LD_LIBRARY_PATH=here, PATH=path + ":" + os.environ["PATH"],
               PACTL_LOG=os.path.join(home, "pactl.log"), MOCKJACK_REPORT=os.path.join(home, "report.txt"),
               MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000", **env_extra)
    err = open(os.path.join(home, "stderr.txt"), "w")
    return subprocess.Popen([binary, "--headless"], env=env, stdout=err, stderr=err)


def vm_kb(pid):
    return int(re.search(r"VmSize:\s+(\d+)", open(f"/proc/{pid}/status").read()).group(1))


# (a) pactl is there, no server answers (JACK2 without PulseAudio)
dead = tempfile.mkdtemp(prefix="bs-nopulse-")
with open(os.path.join(dead, "pactl"), "w") as f:
    f.write(f"#!/bin/sh\necho \"$*\" >> {dead}/calls\necho 'Connection failure: Connection refused' >&2\nexit 1\n")
os.chmod(os.path.join(dead, "pactl"), 0o755)
pa = headless(dead, dead, {})
# (b) the server answers, but `pactl subscribe` ends at once every time
pb_home = tempfile.mkdtemp(prefix="bs-subfail-")
pb = headless(pb_home, os.path.join(here, "fakebin"), {"PACTL_SUBSCRIBE_FAILS": "1"})
def pa_calls(what):
    path = os.path.join(dead, "calls")
    return sum(1 for c in (open(path).read().split("\n") if os.path.exists(path) else []) if c.startswith(what))


def pb_subs():
    return sum(1 for line in open(os.path.join(pb_home, "pactl.log")) if line.startswith("pactl subscribe"))


try:
    time.sleep(4.0)
    vm0, looks0, t0 = vm_kb(pb.pid), pa_calls("info"), time.time()
    # (b) starts `pactl subscribe` again every 10 s: wait for two more
    while pb_subs() < 4 and time.time() - t0 < 45:
        time.sleep(0.5)
    secs = time.time() - t0
    subs = pa_calls("subscribe")
    check(subs == 1, f"no server answers: `pactl subscribe` is not started again and again ({subs} times in {secs + 4:.0f} s)")
    looks = pa_calls("info") - looks0
    check(1 <= looks <= secs / 10 + 1, f"and the server is asked every 10 s ({looks} times in {secs:.0f} s)")
    grew = vm_kb(pb.pid) - vm0
    check(pb_subs() >= 4 and grew < 1024, f"`pactl subscribe` ended {pb_subs()} times: memory grew {grew} kB (ended readers are freed)")
finally:
    for p in (pa, pb):
        p.terminate()
        p.wait(10)

# ---- two BananaSplits whose names differ only in case ---------------------
a = Mixer()
try:
    ours = lambda m: sorted(x for x in m.modules() if "sink_name=bananasplit_" in x[2])
    a.until(lambda d: [c["cable"]["on"] for c in d["ch"]].count(True) == 8, "BananaSplit has its 8 virtual cables")
    before = ours(a)
    b = Mixer(glob="client_name = bananasplit\n", log=a.log)
    try:
        d = b.until(lambda d: d["cables"]["ok"] is False, "a second one named \"bananasplit\" makes none")
        why = (d or {}).get("cables", {}).get("why") or ""
        check("BananaSplit \"BananaSplit\" is running" in why and "bananasplit_in1" in why, f"and says why: {why}")
        check(ours(a) == before, "the first one's cables are left alone (not taken for leftovers)")
    finally:
        check(b.stop() == 0, "quit cleanly")
    check(ours(a) == before, "and are still there when the second one quits")
finally:
    check(a.stop() == 0, "quit cleanly")
check(ours(a) == [], f"the first one removes them when it quits: {ours(a)}")

# ---- a menu line for a JACK name that PipeWire numbered ----------------------
YETI = "Yeti Stereo Microphone Analog Stereo"
m = Mixer(ini=f"[strip4]\nconnect = ^{YETI}-71:\n[strip5]\nconnect = ^system-1:\n[bus.A2]\nconnect = ^{YETI}-71:\n"
             "[bus.A3]\nconnect = ^system:\n[bus.B2]\nconnect = ^system-71:\n")
try:
    # saved when the Yeti's output was "-71"; now it is "-63"
    m.until(lambda d: conn(d, 7) == [f"{YETI}-63"], "A2 finds the Yeti's output under its new number")
    d = m.get("/api/devices")
    check(conn(d, 3) == [], f"strip 4: two fit (the Yeti's mic, and what its output plays), so no guess: {conn(d, 3)}")
    check(f'PipeWire numbers JACK names that are taken twice, and now "{YETI}" and "{YETI}-63" are there' in m.stderr(),
          "and says which two")
    check(conn(d, 4) == [], f"strip 5: \"system-1\" is a name of its own, not system renumbered: {conn(d, 4)}")
    check(conn(d, 8) == ["system"] and conn(d, 10) == [], f"B2's \"system-71\" does not take A3's \"system\": A3 {conn(d, 8)}, B2 {conn(d, 10)}")
    st, msg = m.post(op="dev", ch=7, client=f"{YETI}-63", v=0)
    d = m.until(lambda d: conn(d, 7) == [] and d["ch"][7]["connect"] == [], "unticking it in A2's menu removes the old line too")
finally:
    check(m.stop() == 0, "quit cleanly")
# two microphones of the same model: PipeWire numbers the second one
m = Mixer(ini="[strip5]\nconnect = ^USB Audio:\n[strip6]\nconnect = ^USB Audio-75:\n", MOCKJACK_TWIN="1")
try:
    m.until(lambda d: conn(d, 4) == ["USB Audio"] and conn(d, 5) == ["USB Audio-80"],
            "strip 6's \"USB Audio-75\" finds the second mic (the first is strip 5's)")
finally:
    check(m.stop() == 0, "quit cleanly")
m = Mixer(ini="[strip6]\nconnect = ^USB Audio-75:\n", MOCKJACK_TWIN="1")
try:
    time.sleep(1.0)
    d = m.get("/api/devices")
    check(conn(d, 5) == [], f"without strip 5's line both fit, so no guess: {conn(d, 5)}")
    check('PipeWire numbers JACK names that are taken twice, and now "USB Audio" and "USB Audio-80" are there' in m.stderr(),
          "and says which two")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- the default input is marked on the input, not on an output's monitor --
m = Mixer(MOCKJACK_YETI_SWAP="1")
try:
    d = m.until(lambda d: conn(d, 0) == [f"{YETI}-63"], "strip 1 takes the default input (the Yeti's mic, now \"-63\")")
    tagged = [c["client"] for c in d["strip"] if c["default"]]
    check(tagged == [f"{YETI}-63"], f"the menu marks the mic as the default input, not what the Yeti's output plays: {tagged}")
finally:
    check(m.stop() == 0, "quit cleanly")
out = subprocess.run([binary, "--list-ports"], env=dict(m.env, HOME=m.home), capture_output=True, text=True, timeout=30).stdout
strips = out.split("For a strip")[-1].split("For a bus")[0].splitlines()
tags = {line.strip().split("  ")[0]: strips[i + 1] for i, line in enumerate(strips[:-1]) if line.startswith("  ") and not line.startswith("   ")}
check("default input" in tags.get(f"{YETI}-63", "") and "default input" not in tags.get(YETI, "x"),
      f"--list-ports marks the mic as the default input too: {tags.get(YETI + '-63', '').strip()}")

# ---- Default input switched on: the answer is about that channel ------------
nopactl = tempfile.mkdtemp(prefix="bs-nopactl-")
with open(os.path.join(nopactl, "pactl"), "w") as f:
    f.write("#!/bin/sh\necho 'Connection failure: Connection refused' >&2\nexit 1\n")
os.chmod(os.path.join(nopactl, "pactl"), 0o755)
m = Mixer(ini="[strip3]\nconnect =\n", PATH=nopactl + ":" + os.environ["PATH"])
try:
    st, msg = m.post(op="devdefault", ch=2, v=1)
    check(st == 200 and msg.startswith("strip 3: @default:"), f"strip 3's own problem, not strip 1's: {msg}")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- the menu button's tooltip, and renaming -------------------------------
m = Mixer(ini="[strip3]\nconnect = ^system:\n")
try:
    m.until(lambda s: s["ch"][2]["conn"] == "system", "strip 3's button: connected to system", path="/api/state")
    m.post(op="dev", ch=2, client="system", v=0)
    m.post(op="dev", ch=2, client="Razer Kiyo Analog Stereo", v=1)
    m.until(lambda s: s["ch"][2]["conn"] == "Razer Kiyo Analog Stereo", "after changes in its menu, it says what is connected now",
            secs=2.0, path="/api/state")
    m.post(op="name", ch=9, v="Stream")
    s = m.until(lambda s: s["ch"][9]["name"] == "Stream", "B1 renamed", path="/api/state")
    check(s["status"] == "renamed bus B1", f"a bus's cable is not named after it, so nothing about its cable: {s['status']}")
    m.post(op="name", ch=2, v="Browser")
    s = m.until(lambda s: s["ch"][2]["name"] == "Browser", "strip 3 renamed", path="/api/state")
    check(s["status"].startswith("renamed strip 3; its virtual cable keeps its old name"), f"a strip's cable is: {s['status']}")
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- a multi-track take that continues in new files --------------------------
m = Mixer(ini="[strip1]\nroutes = A1\n", rec="links = 1,A1\nmode = multi\n", MOCKJACK_TONE="1000", MOCKJACK_RATE="48000",
          MOCKJACK_RATE_SWITCH="900:44100")
try:
    st, msg = m.post(op="rec", v=1)
    check(st == 200, f"recording 2 tracks: {msg}")
    s = m.until(lambda s: s["rec"]["sr"] == 44100 and len(s["rec"]["files"]) == 4, "the server changes its rate: 2 more files", secs=8,
                path="/api/state")
    r = (s or {}).get("rec", {})
    check(r.get("tracks") == 2, f"still 2 tracks (not 4): {r.get('tracks')}")
    now = r.get("files", [])[-2:]
    check(len(now) == 2 and all("-2_" in os.path.basename(f) for f in now), f"the files being written are the new ones: {now}")
    m.post(op="rec", v=0)
finally:
    check(m.stop() == 0, "quit cleanly")

# ---- the audio server shuts down -----------------------------------------
m = Mixer(MOCKJACK_SHUTDOWN_AT="1200")
try:
    m.post(op="gain", ch=2, v=-9.5)
    m.post(op="cable", ch=6, v=1)
    m.post(op="route", ch=9, bus=0, v=1)
    for _ in range(100):
        if m.proc.poll() is not None:
            break
        time.sleep(0.1)
finally:
    rc = m.stop()
check(rc == 1, f"the audio server shut down: BananaSplit stops (exit code {rc})")
check("server shut down; your settings are saved" in m.stderr(), "and says the settings are saved")
check("gain_db = -9.5" in m.section("strip3"), "a fader moved before is saved")
check("cable = on" in m.section("bus.A1"), "a cable switched on is saved")
check("routes = A1" in m.section("bus.B1"), "a bus send is saved")

print("FIXES " + ("FAIL: " + "; ".join(failed) if failed else "PASS"))
sys.exit(1 if failed else 0)
