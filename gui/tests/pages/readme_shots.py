#!/usr/bin/env python3
"""Make the five screenshots of the README (docs/screenshots/) from the current gui.html.

usage: readme_shots.py OUTDIR

Same fake backend and headless Firefox as shots.py (no window opens), but a lived-in mixer: a Yeti and a game
running, limits set, a take loaded in the player, VBAN on. Writes dark.png, light.png, menu-strip.png,
menu-bus.png and vban.png at 1920x1080. Needs Firefox and geckodriver on the PATH. Names, addresses and
take times are made up (192.0.2.x is the documentation range).
"""
import base64, copy, json, os, shutil, subprocess, sys, threading, time, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
outdir = sys.argv[1]
label, page_path, platform, attr = 'readme', f'{HERE}/../../gui.html', 'linux', True
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
# What the MIDI window shows (2.9.0): a controller, two bindings, a control just caught by Learn.
MIDI = {'available': True, 'in': 'X-TOUCH MINI MIDI 1', 'out': 'X-TOUCH MINI MIDI 1', 'inOpen': True, 'outOpen': True,
        'ins': ['X-TOUCH MINI MIDI 1', 'Midi Through Port-0'], 'outs': ['X-TOUCH MINI MIDI 1', 'Midi Through Port-0'],
        'error': None, 'last': 'CC 1 = 74 · ch 11',
        'learn': {'src': 'cc:11:1', 'name': 'CC 1 · ch 11', 'mode': None},
        'bindings': [
            {'src': 'cc:11:7', 'name': 'CC 7 · ch 11', 'target': '/strips/1/gain_db', 'label': 'Strip 1 (Mic): fader', 'mode': 'abs', 'takeover': True, 'feedback': True},
            {'src': 'note:11:8', 'name': 'Note 8 · ch 11', 'target': '/strips/1/mute', 'label': 'Strip 1 (Mic): mute', 'mode': 'toggle', 'takeover': False, 'feedback': True}]}
PATHS = [{'path': f'/strips/{n}/{k}', 'label': f'Strip {n}: {t}', 'kind': kind, 'value': 0} for n in range(1, 7)
         for k, t, kind in [('gain_db', 'fader', 'number'), ('mute', 'mute', 'flag')]] + \
        [{'path': f'/buses/{b}/{k}', 'label': f'{b}: {t}', 'kind': kind, 'value': 0} for b in ['V1', 'V2', 'V3', 'B1', 'B2']
         for k, t, kind in [('gain_db', 'fader', 'number'), ('mute', 'mute', 'flag')]] + \
        [{'path': '/recorder/recording', 'label': 'Recorder: recording', 'kind': 'flag', 'value': False}]


TAKES = [{'key': f'2026-11-1{d}_{t}', 'n': i + 1, 'when': f'2026-11-1{d} {t.replace("-", ":")}', 'label': 'mix', 'tracks': ['mix'],
          'secs': secs, 'sr': 48000, 'fmt': 'MP3 320 kbit/s', 'bytes': int(secs * 40000)}
         for i, (d, t, secs) in enumerate([(2, '20-15-02', 1740.0), (3, '21-02-40', 3310.0), (4, '19-48-11', 2285.0)])]
SHOW = {'theme': 'dark', 'rec_on': False}
# LOOK=console (environment): the pictures in that look instead of Classic
LOOK = os.environ.get('LOOK', 'classic')
NAMES = [('Mic', 'Yeti Stereo Microphone Analog Stereo', 'Yeti Stereo Microphone'), ('Discord', 'Discord', 'Discord'),
         ('Desktop', 'system', 'system'), ('Game', 'Game', 'Game'), ('Music', 'Strawberry', 'Strawberry'), ('Aux', 'Firefox', 'Firefox'),
         ('Speakers', 'Starship/Matisse HD Audio Controller Analog Stereo', 'Speakers'), ('Headphones', 'Yeti Stereo Microphone Analog Stereo', 'Headphones'),
         ('Monitor', '(nothing connected)', 'Not connected'), ('Discord Mic', '(nothing connected)', 'vesktop'), ('Stream', '(nothing connected)', 'obs')]
LEVELS = [(-14.0, -15.5), (-22.0, -23.0), (-9.0, -10.5), (-18.0, -19.0), (-12.0, -12.5), (-60.0, -60.0),
          (-7.0, -8.0), (-8.5, -9.0), (-60.0, -60.0), (-12.0, -13.0), (-10.0, -11.0)]
GAIN = [-3.0, -6.0, -4.5, 0.0, -12.0, 0.0, 0.0, -1.5, 0.0, 0.0, 0.0]
ROUTES = [0b01001, 0b00011, 0b10011, 0b10011, 0b10011, 0b00001]   # buses V1 V2 V3 B1 B2, one bit each
LIMITS = [-3.0, -3.0, 12.0, -4.2, -5.3]                          # 12 = off

# What the channel menus list: a Yeti and a webcam mic, a Discord and a Firefox playing, OBS and Vesktop recording.
YETI, KIYO, ALC = 'Yeti Stereo Microphone Analog Stereo', 'Razer Kiyo Analog Stereo', 'Starship/Matisse HD Audio Controller Analog Stereo'
IDS_ = ['1', '2', '3', '4', '5', '6', 'V1', 'V2', 'V3', 'B1', 'B2']
CABLE = lambda i: f'BananaSplit-In{i + 1}-{NAMES[i][0]}' if i < 6 else f'BananaSplit-Out-{IDS_[i]}'
DEVICES = {'cables': {'ok': True, 'none': False, 'why': ''}, 'pulse': True,
    'strip': [{'client': YETI, 'label': 'Yeti Stereo Microphone', 'kind': 'device', 'default': True},
              {'client': KIYO, 'label': 'Razer Kiyo', 'kind': 'device', 'default': False},
              {'client': 'system', 'label': 'system', 'kind': 'device', 'default': False},
              {'client': 'Firefox', 'label': 'Firefox', 'kind': 'app', 'default': False},
              {'client': 'Yeti Stereo Microphone Analog Stereo-63', 'label': 'Yeti Stereo Microphone', 'kind': 'monitor', 'default': False},
              {'client': 'Speakers', 'label': 'Speakers', 'kind': 'monitor', 'default': False}],
    'bus': [{'client': ALC, 'label': 'ALC1220 Analog', 'kind': 'device', 'default': True},
            {'client': YETI, 'label': 'Yeti Stereo Microphone', 'kind': 'device', 'default': False},
            {'client': 'system', 'label': 'system', 'kind': 'device', 'default': False},
            {'client': 'OBS', 'label': 'OBS', 'kind': 'app', 'default': False}],
    'play': [{'id': 31, 'app': 'Discord', 'media': 'Playback', 'ch': 1, 'on': ''},
             {'id': 32, 'app': 'Firefox', 'media': 'YouTube', 'ch': -1, 'on': 'Speakers'}],
    'rec': [{'id': 41, 'app': 'vesktop', 'media': '', 'ch': 9, 'on': ''}],
    'ch': [{'id': IDS_[i], 'cable': {'on': i in (1, 2, 3, 4, 9, 10), 'name': CABLE(i)},
            'conn': {0: [YETI], 1: [], 6: [ALC], 7: [YETI], 10: ['OBS']}.get(i, []), 'connect': {0: ['@default']}.get(i, [])} for i in range(11)]}


def lived_in(s):
    s['sr'], s['cpu'], s['xruns'], s['rtt'], s['status'] = 48000, 3.2, 0, 8.0, ''
    for i, c in enumerate(s['ch']):
        c['name'], c['conn'], c['dev'] = NAMES[i]
        c['m'] = list(LEVELS[i]); c['mi'] = [x - 1 for x in LEVELS[i]]
        c['gain'] = GAIN[i]
        if i < 6:
            c['routes'] = ROUTES[i]; c['apps'] = []
            c['link'] = i in (0, 1, 2, 3)
            if i == 0: c['trim'], c['mono'] = 6.0, True
        else:
            b = i - 6
            c['from'] = sum(1 << k for k in range(6) if ROUTES[k] >> b & 1); c['via'] = 0
            c['limit'] = LIMITS[b]; c['limGr'] = 2.4 if b == 3 else 0.0
            c['cable'] = b >= 3; c['apps'] = ['vesktop'] if b == 3 else ['obs'] if b == 4 else []
    s['ch'][2]['dyn'].update({'comp': True, 'cgr': 3.1}); s['ch'][3]['dyn'].update({'gate': True})
    s['rec'].update({'format': 'mp3', 'kbps': 320, 'mode': 'mix', 'folder': '~/Music/BananaSplit', 'path': '/home/you/Music/BananaSplit'})
    s['disk'] = {'total': 1000204886016, 'free': 640000000000}
    s['takes'] = {'rev': 1, 'count': len(TAKES), 'newest': TAKES[-1]['key']}
    s['player'] = {'state': 'playing', 'take': TAKES[1]['key'], 'pos': 1204.3, 'dur': TAKES[1]['secs'], 'underruns': 0, 'error': None}
    s['playTo'] = 0b00011
    s['scene'], s['scenes'] = 'Streaming', ['Streaming', 'Podcast', 'Quiet night']
    s['app']['theme'] = SHOW['theme']; s['app']['look'] = LOOK; s['app']['listen'] = NAMES[6][1]
    s['vban'].update({'on': True, 'ips': [{'if': 'eth0', 'ip': '192.0.2.2', 'bc': '192.0.2.255'}], 'listening': 6980, 'seen': []})
    s['vban']['out'][0].update({'on': True, 'name': 'Linux', 'to': '192.0.2.20', 'src': 10, 'state': 'ok', 'text': 'to 192.0.2.20:6980 · 48 kHz · stereo · 16-bit', 'packets': 5200})
    s['update']['current'] = '2.9.0'


def state_json():
    s = copy.deepcopy(STATE)
    lived_in(s)
    s['app']['platform'] = platform
    if platform != 'linux':
        s['app']['welcome'] = mode['welcome']
    s['midi'] = MIDI
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
            attrs = ' data-theme="dark" data-zoom="1"' + ('' if LOOK == 'classic' else f' data-look="{LOOK}"') + (f' data-platform="{platform}"' if attr else '')
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
        elif p == '/api/takes': self.send(200, 'application/json', json.dumps({'rev': 1, 'takes': TAKES}))
        elif p == '/api/wave':
            import random; rnd = random.Random(7); n = int(self.path.split('n=')[-1].split('&')[0])
            self.send(200, 'application/json', json.dumps({'key': TAKES[1]['key'], 'n': n, 'secs': TAKES[1]['secs'],
                      'peaks': [int(min(100, 18 + 55 * abs(__import__('math').sin(i / 9.0)) * rnd.random() + rnd.random() * 20)) for i in range(n)]}))
        elif p == '/api/update': self.send(200, 'application/json', open(f'{HERE}/canned/update.json').read())
        elif p == '/api/cables' and platform in CABLES: self.send(200, 'application/json', json.dumps(CABLES[platform]))
        elif p == '/api/ping': self.send(200, 'text/plain', 'bananasplit-gui')
        elif p == '/api/v1/paths': self.send(200, 'application/json', json.dumps({'api': 1, 'paths': PATHS}))
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
call('POST', f'{S}/window/rect', {'width': 1920, 'height': 1166})


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




def shot(name):
    time.sleep(0.8)
    open(f'{outdir}/{name}.png', 'wb').write(base64.b64decode(call('GET', f'{S}/screenshot')))


def go(theme='dark'):
    SHOW['theme'] = theme
    call('POST', f'{S}/url', {'url': f'http://127.0.0.1:{PORT}/'})
    wait('document.body && /engine/i.test(document.body.innerText)', 'the first state')
    js('const st = document.createElement("style"); st.textContent = ".pk span { contain: layout style !important; width: 40px; }"; document.head.append(st)')
    time.sleep(1.5)  # the meters' peak hold fills in from the levels


try:
    go('dark'); shot('dark')
    go('light'); shot('light')
    go('dark'); js('toggleChanMenu(1)'); wait('document.querySelector("#cmenu .ci")', 'the strip menu'); shot('menu-strip')
    go('dark'); js('toggleChanMenu(9)'); wait('document.querySelector("#cmenu .ci")', 'the bus menu'); shot('menu-bus')
    go('dark'); js('document.getElementById("vbanbtn").click()'); time.sleep(0.6); shot('vban')
    print(f'{len(os.listdir(outdir))} screenshots -> {outdir}')
finally:
    try: call('DELETE', S)
    except Exception: pass
    gd.terminate()
