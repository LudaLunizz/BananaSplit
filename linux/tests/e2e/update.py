#!/usr/bin/env python3
"""End-to-end checks of the updater, against a stand-in for GitHub on this
computer (BANANASPLIT_UPDATE_SOURCE) and the mock JACK server:

* with automatic updates off (the default), BananaSplit asks nobody
  anything, and the setting is not written to the config;
* "Check now" finds the newest published release (not a draft, not a
  pre-release, not an older one), and its notes;
* a download that does not match the release's checksums is refused, and
  nothing is changed;
* "Install" puts the new program in the place of the running one (after
  it ran and said its version), and "Restart now" starts it, with the same
  command line, once the old one has saved and let go;
* with automatic updates on, it checks, downloads and installs by itself,
  says so, and does not restart by itself; switching it off and on is
  saved at once;
* GitHub refusing the request (its rate limit) is a message, not a crash;
* `bananasplit --update` does the same from a terminal;
* a folder BananaSplit cannot write to is said, and offers no install.

usage: update.py BINARY E2EDIR
"""
import hashlib
import http.server
import io
import json
import os
import random
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.error
import urllib.request

binary, here = sys.argv[1], sys.argv[2]
failed = []
TITLE = "updater (update.py)"
REPO = "LudaLunizz/BananaSplit"
NEW = "9.9.9"


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failed.append(what)
        if os.environ.get("GITHUB_ACTIONS") == "true":
            text = what.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
            print(f"::error title={TITLE}::{text}", flush=True)


current = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.split()[1]

# ---- the new version: a script that says it is 9.9.9 and starts the real program
WORK = tempfile.mkdtemp(prefix="bs-update-")
REAL = os.path.join(WORK, "real", "bananasplit")
os.makedirs(os.path.dirname(REAL))
shutil.copy2(binary, REAL)
NEW_PROGRAM = f"""#!/bin/sh
if [ "$1" = "--version" ]; then echo "BananaSplit {NEW}"; exit 0; fi
echo "$@" > "$UPDATE_MARKER"
echo "wait=$BANANASPLIT_WAIT_PID" >> "$UPDATE_MARKER"
exec "{REAL}" "$@"
""".encode()


def tarball(program):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as t:
        top = f"bananasplit-{NEW}-linux-x86_64"
        d = tarfile.TarInfo(top)
        d.type, d.mode = tarfile.DIRTYPE, 0o755
        t.addfile(d)
        for name, data, mode in [("bananasplit", program, 0o755), ("install.sh", b"#!/bin/sh\nexit 0\n", 0o755), ("README.md", b"x\n", 0o644)]:
            i = tarfile.TarInfo(f"{top}/{name}")
            i.size, i.mode = len(data), mode
            t.addfile(i, io.BytesIO(data))
    return buf.getvalue()


TGZ = tarball(NEW_PROGRAM)
TGZ_NAME = f"bananasplit-{NEW}-linux-x86_64.tar.gz"
# the same size, one byte changed
TAMPERED = bytearray(TGZ)
TAMPERED[len(TGZ) // 2] ^= 0xFF
TAMPERED = bytes(TAMPERED)


class Fake:
    """The parts of GitHub the updater uses."""

    def __init__(self):
        self.mode = "good"
        self.log = []
        fake = self

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def send(self, code, body, ctype="application/octet-stream", extra=()):
                self.send_response(code)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(body)))
                for k, v in extra:
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                fake.log.append((self.path, self.headers.get("User-Agent", ""), self.headers.get("Accept", "")))
                base = f"http://127.0.0.1:{fake.port}"
                if self.path == f"/repos/{REPO}/releases?per_page=30":
                    if fake.mode == "limited":
                        return self.send(403, b'{"message":"API rate limit exceeded for 127.0.0.1."}', "application/json")
                    return self.send(200, json.dumps(fake.releases(base)).encode(), "application/json")
                dl = f"/{REPO}/releases/download/v{NEW}/"
                if self.path.startswith(dl):
                    # GitHub sends release files from another host: a redirect
                    return self.send(302, b"", extra=[("Location", base + "/blob/" + self.path[len(dl):])])
                if self.path == "/blob/" + TGZ_NAME:
                    body = TGZ if fake.mode != "tampered" else TAMPERED
                    return self.send(200, body)
                if self.path == "/blob/SHA256SUMS.txt":
                    return self.send(200, f"{hashlib.sha256(TGZ).hexdigest()}  {TGZ_NAME}\n".encode(), "text/plain")
                self.send(404, b"not found", "text/plain")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.port}"

    def releases(self, base):
        def rel(tag, files, draft=False, pre=False):
            assets = [{"name": n, "state": "uploaded", "size": len(data), "digest": "sha256:" + hashlib.sha256(data).hexdigest(),
                       "browser_download_url": f"{base}/{REPO}/releases/download/{tag}/{n}"} for n, data in files]
            return {"tag_name": tag, "name": "BananaSplit " + tag, "draft": draft, "prerelease": pre,
                    "html_url": f"{base}/{REPO}/releases/tag/{tag}", "body": f"What changed in {tag}.\r\n- the updater", "assets": assets}
        if self.mode == "old":
            return [rel("v2.0.0", [("bananasplit-2.0.0-linux-x86_64.tar.gz", b"old")])]
        sums = f"{hashlib.sha256(TGZ).hexdigest()}  {TGZ_NAME}\n".encode()
        return [
            rel("v10.0.0", [("bananasplit-10.0.0-linux-x86_64.tar.gz", b"draft")], draft=True),
            rel("v9.10.0", [("bananasplit-9.10.0-linux-x86_64.tar.gz", b"pre")], pre=True),
            rel(f"v{NEW}", [(TGZ_NAME, TGZ), ("SHA256SUMS.txt", sums), (f"bananasplit_{NEW}_amd64.deb", b"deb")]),
            rel("v2.0.0", [("bananasplit-2.0.0-linux-x86_64.tar.gz", b"old")]),
        ]

    def api_calls(self):
        return [p for p, _, _ in self.log if p.startswith("/repos/")]


fake = Fake()


class Mixer:
    """BananaSplit --gui, run from a copy in a folder of the user's."""

    next_port = 20900 + random.randint(0, 40)

    def __init__(self, gui="", delay="0.5", start=True):
        self.port = Mixer.next_port
        Mixer.next_port += 1
        self.home = tempfile.mkdtemp(prefix="bs-upd-home-", dir=WORK)
        self.app = os.path.join(self.home, "apps", "bananasplit")
        os.makedirs(os.path.dirname(self.app))
        shutil.copy2(binary, self.app)
        self.cfg = os.path.join(self.home, "bananasplit", "bananasplit.ini")
        os.makedirs(os.path.dirname(self.cfg))
        with open(self.cfg, "w") as f:
            f.write(f"[global]\ncreate_cables = false\n[gui]\n{gui}[recorder]\nfolder = {self.home}/rec\n")
        self.marker = os.path.join(self.home, "restarted.txt")
        env = dict(os.environ, HOME=self.home, XDG_CONFIG_HOME=self.home, XDG_CACHE_HOME=os.path.join(self.home, "cache"),
                   XDG_DATA_HOME=os.path.join(self.home, "data"), LD_LIBRARY_PATH=here,
                   MOCKJACK_REPORT=os.path.join(self.home, "report.txt"), MOCKJACK_REALTIME="1", MOCKJACK_CYCLES="100000000",
                   BANANASPLIT_UPDATE_SOURCE=fake.url, BANANASPLIT_UPDATE_DELAY=delay, UPDATE_MARKER=self.marker)
        for k in ("DISPLAY", "WAYLAND_DISPLAY", "BANANASPLIT_WAIT_PID", "PIPEWIRE_LATENCY"):
            env.pop(k, None)
        self.env = env
        self.url = f"http://127.0.0.1:{self.port}"
        self.args = ["--gui", "--no-open", "--gui-port", str(self.port), "--config", self.cfg]
        self.proc = None
        if start:
            self.start()

    def start(self):
        self.out = open(os.path.join(self.home, "log.txt"), "a")
        self.proc = subprocess.Popen([self.app, *self.args], env=self.env, stdout=self.out, stderr=self.out)
        self.wait_up()

    def wait_up(self, secs=10):
        t0 = time.time()
        while time.time() - t0 < secs:
            try:
                urllib.request.urlopen(self.url + "/api/ping", timeout=1)
                time.sleep(0.3)
                return True
            except OSError:
                time.sleep(0.1)
        return False

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

    def update(self):
        return self.get("/api/state")["update"]

    def until(self, pred, what, secs=15.0):
        t0 = time.time()
        u = None
        while time.time() - t0 < secs:
            try:
                u = self.update()
                if pred(u):
                    check(True, what)
                    return u
            except (OSError, KeyError, ValueError):
                pass
            time.sleep(0.1)
        check(False, f"{what} (last: {json.dumps(u)[:400] if u is not None else None})")
        return u

    def ini(self):
        return open(self.cfg).read()

    def stop(self):
        if self.proc and self.proc.poll() is None:
            try:
                self.post(op="quit")
                self.proc.wait(10)
            except Exception:
                self.proc.kill()
                self.proc.wait()
        return self.proc.returncode if self.proc else None

    def log(self):
        self.out.flush()
        return open(os.path.join(self.home, "log.txt")).read()


def same(a, b):
    return open(a, "rb").read() == open(b, "rb").read()


# ---- off by default: nobody is asked anything --------------------------------
m = Mixer()
try:
    time.sleep(3.0)
    u = m.update()
    check(u["auto"] is False and u["phase"] == "idle", f"automatic updates are off by default: {u}")
    check(fake.api_calls() == [], f"with them off, BananaSplit asks nobody anything: {fake.log}")
    check(u["current"] == current and u["latest"] is None and u["canInstall"] is False, f"it knows its own version only: {u}")
    st, msg = m.post(op="update", a="restart")
    check(st == 400 and "no update is installed" in msg, f"restart before an update is refused: {st} {msg}")
    st, msg = m.post(op="update", a="install")
    check(st == 400, f"install before a check is refused: {st} {msg}")

    # ---- "Check now" ---------------------------------------------------------
    st, msg = m.post(op="update", a="check")
    check(st == 200, f"check: {st} {msg}")
    u = m.until(lambda u: u["phase"] == "available", "Check now finds a new version")
    check(u["latest"] == NEW, f"the newest published one, not the draft or the pre-release: {u['latest']}")
    check(u["canInstall"] is True and u["method"] == "replace", f"it can install itself here (a folder of the user's): {u}")
    calls = [x for x in fake.log if x[0].startswith("/repos/")]
    check(len(calls) == 1 and calls[0][1].startswith("BananaSplit/") and "github" in calls[0][2],
          f"one request, with its User-Agent and GitHub's API type: {calls}")
    d = m.get("/api/update")
    check(d["notes"] == f"What changed in v{NEW}.\n- the updater" and d["page"].endswith(f"/releases/tag/v{NEW}") and d["manual"] is None,
          f"the release's notes and page: {d}")
    check(fake.log[-1][0].startswith("/repos/"), "nothing is downloaded before Install")

    # ---- a download that does not match its checksums ------------------------
    fake.mode = "tampered"
    st, msg = m.post(op="update", a="install")
    check(st == 200, f"install: {st} {msg}")
    u = m.until(lambda u: u["phase"] == "failed", "a tampered download fails")
    check("does not match" in u["message"] and "nothing was changed" in u["message"], f"and says why: {u['message']}")
    check(same(m.app, binary), "the program was not touched")
    check(u["canInstall"] is True, "it can be tried again")

    # ---- Install ---------------------------------------------------------------
    fake.mode = "good"
    st, msg = m.post(op="update", a="install")
    u = m.until(lambda u: u["phase"] == "ready", "Install downloads, checks and installs it")
    check(u["installed"] == NEW and "Restart BananaSplit" in u["message"], f"and asks for a restart: {u}")
    check(open(m.app, "rb").read() == NEW_PROGRAM, "the new program took the old one's place")
    check(stat.S_IMODE(os.stat(m.app).st_mode) == 0o755, "and can be run")
    check(sorted(os.listdir(os.path.dirname(m.app))) == ["bananasplit"], f"nothing else left next to it: {os.listdir(os.path.dirname(m.app))}")
    left = os.listdir(os.path.join(m.home, "cache", "bananasplit", "update"))
    check(left == [], f"the downloads folder is emptied: {left}")
    check(m.proc.poll() is None, "the running mixer goes on (the old program, until a restart)")
    check(m.update()["canInstall"] is False, "nothing more to install")
    # a check that fails now (offline, GitHub's limit) keeps the restart
    fake.mode = "limited"
    m.post(op="update", a="check")
    u = m.until(lambda u: u["phase"] == "ready" and "Checking again failed" in u["message"], "a failed check after Install still offers the restart")
    st, msg = m.post(op="update", a="restart")
    fake.mode = "good"
    check(st == 200, f"and Restart now works then: {st} {msg}")
    try:
        m.proc.wait(10)
    except subprocess.TimeoutExpired:
        pass
    check(m.wait_up(15), "the new version comes up")
    marker = open(m.marker).read() if os.path.exists(m.marker) else ""
    check(marker.count("wait=") == 1, f"started once: {marker!r}")
    # (that one is the real program again, so the restart below starts over)
    m.post(op="quit")
    time.sleep(1.5)
    os.remove(m.marker)
    shutil.copy2(binary, m.app)
    m.start()
    m.post(op="update", a="check")
    m.until(lambda u: u["phase"] == "available", "checked again")
    m.post(op="update", a="install")
    m.until(lambda u: u["phase"] == "ready", "installed again")
    st, msg = m.post(op="update", a="check")
    time.sleep(1.0)
    u = m.until(lambda u: u["phase"] == "ready", "checking again keeps it ready for the restart", secs=8)
    check(m.update()["latest"] == NEW, "same version")

    # ---- Restart now -------------------------------------------------------------
    old = m.proc.pid
    st, msg = m.post(op="update", a="restart")
    check(st == 200, f"restart: {st} {msg}")
    try:
        rc = m.proc.wait(10)
    except subprocess.TimeoutExpired:
        rc = None
    check(rc == 0, f"the old BananaSplit saves and quits: exit {rc}")
    up = m.wait_up(15)
    check(up, "the new version comes up on the same port")
    marker = open(m.marker).read() if os.path.exists(m.marker) else ""
    check(marker.splitlines()[:1] == [" ".join(m.args)], f"with the same command line: {marker!r}")
    check(f"wait={old}" in marker, f"and waited for the old one: {marker!r}")
    if up:
        u = m.update()
        check(u["phase"] == "idle" and u["installed"] is None, f"the new one starts fresh: {u}")
        m.post(op="quit")
        time.sleep(1.5)
finally:
    m.stop()
    if m.proc:
        print(m.log()[-3000:])

# ---- automatic updates -------------------------------------------------------
fake.log.clear()
m = Mixer(gui="auto_update = true\n", delay="1")
try:
    u = m.until(lambda u: u["phase"] == "ready", "with automatic updates on, it checks, downloads and installs by itself", secs=25)
    check(u["auto"] is True, "and says automatic updates are on")
    check(open(m.app, "rb").read() == NEW_PROGRAM, "the program was replaced")
    s = m.get("/api/state")
    check("was installed" in s["status"] and "restart" in s["status"], f"the session line says so: {s['status']}")
    time.sleep(2.0)
    check(m.proc.poll() is None and not os.path.exists(m.marker), "it does not restart by itself")
    check(len(fake.api_calls()) == 1, f"one check: {fake.api_calls()}")
    st, msg = m.post(op="autoupdate", v=0)
    time.sleep(0.5)
    check(st == 200 and "auto_update" not in m.ini(), f"switched off: saved at once (and not written, the default): {msg}")
    check(m.update()["auto"] is False, "the page sees it off")
    st, msg = m.post(op="autoupdate", v=1)
    time.sleep(0.5)
    check("\nauto_update = true\n" in m.ini(), "switched on: saved at once")
    st, msg = m.post(op="autoupdate", v=2)
    check(st == 400, "only 0 or 1")
finally:
    m.stop()

# ---- GitHub's rate limit --------------------------------------------------------
fake.mode = "limited"
m = Mixer()
try:
    m.post(op="update", a="check")
    u = m.until(lambda u: u["phase"] == "failed", "GitHub refusing the request is a failed check")
    check("try again" in u["message"], f"with a message: {u['message']}")
    check(m.proc.poll() is None, "and BananaSplit runs on")
finally:
    m.stop()
fake.mode = "good"

# ---- a folder it cannot write to ------------------------------------------------
if os.geteuid() != 0:
    m = Mixer(start=False)
    os.chmod(os.path.dirname(m.app), 0o555)
    try:
        m.start()
        m.post(op="update", a="check")
        u = m.until(lambda u: u["phase"] == "available", "read-only folder: the new version is still found")
        check(u["method"] == "manual" and u["canInstall"] is False, f"but not offered for install: {u}")
        check("cannot write to" in u["message"], f"and it says why: {u['message']}")
        st, msg = m.post(op="update", a="install")
        check(st == 400 and "cannot write to" in msg, f"install says why: {st} {msg}")
    finally:
        m.stop()
        os.chmod(os.path.dirname(m.app), 0o755)
else:
    print("skip a read-only folder (running as root, which writes anywhere)")

# ---- bananasplit --update ------------------------------------------------------------
m = Mixer(start=False)
r = subprocess.run([m.app, "--update"], env=m.env, capture_output=True, text=True, timeout=60)
check(r.returncode == 0 and f"installed BananaSplit {NEW}" in r.stdout, f"--update installs it: {r.returncode} {r.stdout} {r.stderr}")
check(open(m.app, "rb").read() == NEW_PROGRAM, "--update replaced the program")
fake.mode = "old"
m = Mixer(start=False)
r = subprocess.run([m.app, "--update"], env=m.env, capture_output=True, text=True, timeout=60)
check(r.returncode == 0 and "is the newest version" in r.stdout, f"--update with nothing newer: {r.stdout} {r.stderr}")
check(same(m.app, binary), "and changes nothing")
fake.mode = "tampered"
m = Mixer(start=False)
r = subprocess.run([m.app, "--update"], env=m.env, capture_output=True, text=True, timeout=60)
check(r.returncode != 0 and "does not match" in r.stderr, f"--update refuses a tampered download: {r.stdout} {r.stderr}")
check(same(m.app, binary), "and changes nothing")
r = subprocess.run([m.app, "--update", "--gui"], env=m.env, capture_output=True, text=True, timeout=20)
check(r.returncode != 0, "--update is a command of its own")

fake.server.shutdown()
shutil.rmtree(WORK, ignore_errors=True)
if failed:
    print(f"{len(failed)} updater check(s) failed")
    sys.exit(1)
print("all updater checks passed")
