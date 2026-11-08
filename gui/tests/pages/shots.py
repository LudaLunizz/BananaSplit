#!/usr/bin/env python3
"""Render one gui.html in headless Firefox against a fixed fake backend and record what it shows.

usage: shots.py LABEL PAGE.html PLATFORM OUTDIR [attr]
  PLATFORM  linux | windows | macos : which backend to pretend to be (state, devices, cables)
  attr      add data-platform="PLATFORM" to <html> as the new Hub::page() does

For every scenario writes OUTDIR/<scenario>.png (the viewport) and <scenario>.txt (the visible text of the
part of the page the scenario is about). Old and new pages are then compared file by file.
"""
import base64, copy, json, os, shutil, subprocess, sys, threading, time, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
label, page_path, platform, outdir = sys.argv[1:5]
attr = len(sys.argv) > 5 and sys.argv[5] == 'attr'
os.makedirs(outdir, exist_ok=True)
PAGE = open(page_path, encoding='utf-8').read()
canned = lambda n: json.load(open(f'{HERE}/canned/{n}.json'))

# ---- the fake backend ----------------------------------------------------------------------
STATE = canned('state')
DEVICES = canned('devices')
if platform == 'linux':
    pass
else:
    DEVICES['cables'] = {'ok': True, 'none': False, 'why': ''}
    if platform == 'macos':
        DEVICES['strip'] = [x for x in DEVICES['strip'] if x['kind'] == 'device']
        DEVICES['bus'] = [x for x in DEVICES['bus'] if x['kind'] == 'device']
CABLES = {
    'windows': {'splitter': {'found': True, 'cables': ['Splitter 1', 'Splitter 2', 'Splitter 3', 'Splitter 4', 'Splitter 5', 'Splitter 6'], 'missing': []},
                'pairs': ['CABLE'], 'voicemeeter': ['Voicemeeter Input', 'Voicemeeter AUX Input'],
                'output': {'name': 'Speakers (Realtek Audio)', 'cable': False}, 'input': {'name': 'CABLE Output', 'cable': True}},
    'macos': {'cables': ['BlackHole 2ch', 'BlackHole 16ch'],
              'output': {'name': 'MacBook Pro Speakers', 'cable': False}, 'input': {'name': 'MacBook Pro Microphone', 'cable': False}},
}
mode = {'welcome': False, 'nocables': False}


def state_json():
    s = copy.deepcopy(STATE)
    if platform != 'linux':
        s['app']['welcome'] = mode['welcome']
    return json.dumps(s, separators=(',', ':'))


def devices_json():
    d = copy.deepcopy(DEVICES)
    if mode['nocables']:
        d['cables'] = {'ok': False, 'none': True, 'why': 'No virtual cables are installed'}
    return json.dumps(d, separators=(',', ':'))


class H(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *a): pass

    def send(self, code, ctype, body):
        b = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(b)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(b)

    def do_POST(self):
        n = int(self.headers.get('Content-Length') or 0)
        self.rfile.read(n)
        self.send(200, 'text/plain', 'ok')

    def do_GET(self):
        p = self.path.split('?')[0]
        if p == '/':
            attrs = ' data-theme="dark" data-zoom="1"' + (f' data-platform="{platform}"' if attr else '')
            html = PAGE.replace('<html lang="en">', f'<html lang="en"{attrs}>', 1)
            self.send(200, 'text/html; charset=utf-8', html)
        elif p == '/api/events':
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            try:
                while True:
                    self.wfile.write(f'data: {state_json()}\n\n'.encode())
                    self.wfile.flush()
                    time.sleep(0.4)
            except OSError:
                pass
        elif p == '/api/state': self.send(200, 'application/json', state_json())
        elif p == '/api/devices': self.send(200, 'application/json', devices_json())
        elif p == '/api/takes': self.send(200, 'application/json', open(f'{HERE}/canned/takes.json').read())
        elif p == '/api/update': self.send(200, 'application/json', open(f'{HERE}/canned/update.json').read())
        elif p == '/api/cables' and platform in CABLES: self.send(200, 'application/json', json.dumps(CABLES[platform]))
        elif p == '/api/ping': self.send(200, 'text/plain', 'bananasplit-gui')
        else: self.send(404, 'text/plain', 'not found')


srv = ThreadingHTTPServer(('127.0.0.1', 0), H)
srv.daemon_threads = True
PORT = srv.server_address[1]
threading.Thread(target=srv.serve_forever, daemon=True).start()

# ---- WebDriver -----------------------------------------------------------------------------
GD = 4470 + (os.getpid() % 200)
gd = subprocess.Popen([shutil.which('geckodriver') or '/snap/bin/geckodriver', '--port', str(GD)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def call(method, path, body=None):
    req = urllib.request.Request(f'http://127.0.0.1:{GD}{path}', data=None if body is None else json.dumps(body).encode(),
                                 method=method, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)['value']


for _ in range(50):
    try:
        urllib.request.urlopen(f'http://127.0.0.1:{GD}/status', timeout=2).read()
        break
    except OSError:
        time.sleep(0.2)
sid = call('POST', '/session', {'capabilities': {'alwaysMatch': {'browserName': 'firefox', 'moz:firefoxOptions': {'args': ['-headless']}}}})['sessionId']
S = f'/session/{sid}'
call('POST', f'{S}/window/rect', {'width': 1500, 'height': 1000})


def js(script, *args):
    return call('POST', f'{S}/execute/sync', {'script': script, 'args': list(args)})


def wait(cond, what, secs=10):
    t0 = time.time()
    while time.time() - t0 < secs:
        try:
            if js('return !!(' + cond + ')'): return
        except Exception:
            pass
        time.sleep(0.1)
    raise SystemExit(f'[{label}/{platform}] timed out waiting for {what}: {cond}')


def load(**m):
    mode.update({'welcome': False, 'nocables': False}); mode.update(m)
    call('POST', f'{S}/url', {'url': f'http://127.0.0.1:{PORT}/'})
    wait('document.body && /firefox/i.test(document.body.innerText)', 'the first state')
    time.sleep(0.6)


def snap(name, selector=None):
    time.sleep(0.5)
    open(f'{outdir}/{name}.png', 'wb').write(base64.b64decode(call('GET', f'{S}/screenshot')))
    text = js('const e = arguments[0] ? document.querySelector(arguments[0]) : document.body; return e ? e.innerText : "(none)"', selector)
    open(f'{outdir}/{name}.txt', 'w', encoding='utf-8').write(text)


try:
    load()
    snap('main')
    js('document.getElementById("menubtn").click()'); snap('menu', '#mainmenu')
    js('document.getElementById("menubtn").click()')
    js('document.getElementById("vbanbtn").click()'); time.sleep(0.4); snap('vban', '#vbandlg')
    js('document.getElementById("vbanclose").click()')
    js('document.getElementById("menubtn").click()'); js('document.getElementById("m-update").click()'); time.sleep(0.4); snap('update', '#upddlg')
    js('document.getElementById("updclose").click()')
    load(); js('toggleChanMenu(0)'); wait('document.querySelector("#cmenu .ci")', 'the strip menu'); snap('chanmenu-strip', '#cmenu')
    load(); js('toggleChanMenu(7)'); wait('document.querySelector("#cmenu .ci")', 'the bus menu'); snap('chanmenu-bus', '#cmenu')
    if platform != 'linux':
        load(); js('document.getElementById("menubtn").click()'); js('document.getElementById("m-cables").click()')
        wait('document.getElementById("cab-state").innerText.indexOf("Looking") < 0', 'the cables report'); snap('cables', '#cabdlg')
        load(welcome=True)
        wait('document.getElementById("cabdlg").open', 'the welcome dialog'); wait('document.getElementById("cab-state").innerText.indexOf("Looking") < 0', 'the cables report')
        snap('welcome', '#cabdlg')
        load(nocables=True); js('toggleChanMenu(0)'); wait('document.querySelector("#cmenu .ci")', 'the strip menu'); snap('chanmenu-nocables', '#cmenu')
        js('(function(){const b=[...document.querySelectorAll("#cmenu .ci")].find(x=>!x.disabled && /cable|BlackHole|Splitter|CABLE/i.test(x.innerText)); if(b) b.click();})()')
        time.sleep(0.6); snap('chanmenu-nocables-click', '#cabdlg')
    print(f'[{label}/{platform}] {len(os.listdir(outdir)) // 2} scenarios -> {outdir}')
finally:
    try: call('DELETE', S)
    except Exception: pass
    gd.terminate()
