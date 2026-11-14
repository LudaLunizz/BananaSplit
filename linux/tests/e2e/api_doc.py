#!/usr/bin/env python3
"""Does docs/api.md say what the server does? Runs BananaSplit (--gui) on the mock JACK server and
checks the document against it:

* every key of the state (and of a channel, the recorder, VBAN, the player, the app object, the
  takes) appears in the document's "The state" section, so a new field cannot go undocumented;
* the commands in the document answer what it says, and change the state;
* errors are plain text by default and JSON for a client that sends
  "Accept: application/json", and the refusals the document lists (403, 404,
  405, 411) are what it says;
* the endpoints answer with the keys the document names.

usage: api_doc.py BINARY E2EDIR API.md
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time

binary, here, doc_path = sys.argv[1:4]
doc = open(doc_path, encoding='utf-8').read()
with socket.socket() as probe:  # a free port
    probe.bind(('127.0.0.1', 0))
    port = probe.getsockname()[1]
tmp = tempfile.mkdtemp(); cfg = os.path.join(tmp, 'bananasplit.ini'); open(cfg, 'w').write(open(os.path.join(here, 'e2e.ini')).read())
env = {'PATH': os.environ['PATH'], 'HOME': tmp, 'XDG_CONFIG_HOME': tmp, 'XDG_RUNTIME_DIR': tmp, 'LD_LIBRARY_PATH': here,
       'MOCKJACK_REPORT': tmp + '/report.txt', 'MOCKJACK_CYCLES': '200000'}
p = subprocess.Popen([binary, '--gui', '--no-open', '--gui-port', str(port), '--config', cfg], env=env, stderr=subprocess.DEVNULL)
def req(method, path, body='', hdr=None, host=None):
    s = socket.create_connection(('127.0.0.1', port), timeout=5)
    h = {'Host': host or f'127.0.0.1:{port}', 'Connection': 'close'}; h.update(hdr or {})
    if body: h['Content-Length'] = str(len(body.encode()))
    s.sendall((f'{method} {path} HTTP/1.1\r\n' + ''.join(f'{k}: {v}\r\n' for k, v in h.items()) + '\r\n' + body).encode())
    buf = b''
    while True:
        try: c = s.recv(65536)
        except socket.timeout: break
        if not c: break
        buf += c
    head, _, b = buf.partition(b'\r\n\r\n'); return head.decode('latin1'), b.decode('utf-8', 'replace')
bad, checks = [], []
def check(ok, what):
    checks.append(what)
    if not ok:
        print('FAIL ' + what)
        bad.append(what)
X = {'X-BananaSplit': '1'}
try:
    for _ in range(100):
        try:
            if req('GET', '/api/ping')[0].startswith('HTTP/1.1 200'): break
        except OSError: time.sleep(0.05)
    time.sleep(1.0)
    st = json.loads(req('GET', '/api/state')[1])
    # --- the documented state fields are the real ones
    def doc_keys(section_start, section_end):
        seg = doc[doc.index(section_start):doc.index(section_end)]
        return seg
    state_doc = doc[doc.index('## The state'):doc.index('## Other answers')]
    def documented(key): return f'`{key}`' in state_doc
    for k in st: check(documented(k), f'state key {k!r} is documented')
    for k in st['ch'][0]: check(documented(k), f'channel key {k!r} is documented')
    for k in st['rec']: check(documented(k), f'rec key {k!r} is documented')
    for k in st['vban']: check(documented(k), f'vban key {k!r} is documented')
    for k in st['vban']['in'][0]: check(documented(k), f'vban.in key {k!r} is documented')
    for k in st['vban']['out'][0]: check(documented(k), f'vban.out key {k!r} is documented')
    for k in st['player']: check(documented(k), f'player key {k!r} is documented')
    for k in st['app']: check(documented(k), f'app key {k!r} is documented')
    for k in st['takes']: check(documented(k), f'takes key {k!r} is documented')
    check(st['app']['platform'] == 'linux', 'app.platform is "linux"')
    check('welcome' not in st['app'], 'no welcome on Linux')
    # --- the channels: 11, ids, strips have dyn, buses do not
    check([c['id'] for c in st['ch']] == ['1','2','3','4','5','6','V1','V2','V3','B1','B2'], 'channel ids as documented')
    check('dyn' in st['ch'][0] and 'dyn' not in st['ch'][6], 'dyn on strips only')
    check(len(st['ch'][0]['eq']['b']) == 6 and len(st['ch'][0]['eq']['b'][0]) == 5, 'eq: 6 bands of 5 values')
    check(len(st['ch'][0]['dyn']['g']) == 5 and len(st['ch'][0]['dyn']['c']) == 6, 'dyn: 5 gate and 6 compressor settings')
    # --- documented command answers
    def cmd(body, hdr=None): 
        h, b = req('POST', '/api/cmd', body, {**X, **(hdr or {})}); return int(h.split()[1]), b
    for body, want in [('op=gain&ch=0&v=-6', '-6.0 dB'), ('op=trim&ch=1&v=6', 'gain +6.0 dB'), ('op=pan&ch=0&v=0', 'C'), ('op=pan&ch=0&v=-0.25', 'L25'),
                       ('op=mute&ch=3&v=1', 'ok'), ('op=route&ch=6&bus=3&v=1', 'ok'), ('op=eq&ch=0&v=on', 'EQ on'), ('op=eqreset&ch=0', 'EQ bands reset (flat)'),
                       ('op=gate&ch=0&v=1', 'Gate on'), ('op=comp&ch=0&v=0', 'Compressor off'), ('op=dynreset&ch=0', 'Gate and compressor settings reset'),
                       ('op=vban&v=0', 'VBAN off'), ('op=vbanport&v=6981', 'VBAN listens on UDP port 6981'), ('op=pause', 'paused'), ('op=stop', 'stopped'),
                       ('op=save', 'saving'), ('op=reconnect', 'reconnecting'), ('op=scene&name=Live&act=save', 'scene "Live" saved'),
                       ('op=scene&name=Live&act=delete', 'scene "Live" deleted'), ('op=name&ch=0&v=Voice', 'ok'), ('op=zoom&v=1.25', 'ok'), ('op=theme&v=light', 'ok')]:
        code, b = cmd(body); check((code, b) == (200, want), f'{body} -> 200 {want!r} (got {code} {b!r})')
    time.sleep(0.2)
    st2 = json.loads(req('GET', '/api/state')[1])
    check(st2['ch'][0]['gain'] == -6.0 and st2['ch'][3]['mute'] is True and st2['ch'][0]['name'] == 'Voice', 'the commands changed the state')
    # --- errors: text by default, JSON when asked, before-the-request refusals are text
    h, b = req('POST', '/api/cmd', 'op=gain&v=x', X)
    check(h.startswith('HTTP/1.1 400') and 'text/plain; charset=utf-8' in h and b == 'missing "ch"', 'error as text by default')
    h, b = req('POST', '/api/cmd', 'op=gain&ch=0&v=x', {**X, 'Accept': 'application/json'})
    check('application/json' in h and b == '{"error":"gain must be a number"}', 'error as JSON on Accept')
    h, b = req('POST', '/api/cmd', 'op=gain&ch=0&v=x', {**X, 'Accept': '*/*'})
    check('text/plain' in h and b == 'gain must be a number', '*/* gets text')
    h, b = req('GET', '/api/state', host='evil.example'); check(h.startswith('HTTP/1.1 403') and b == 'wrong Host', '403 wrong Host')
    h, b = req('POST', '/api/cmd', 'op=mute&ch=0&v=1'); check(h.startswith('HTTP/1.1 403') and b == 'forbidden', '403 without X-BananaSplit')
    h, b = req('GET', '/api/state', hdr={'Sec-Fetch-Site': 'cross-site'}); check(h.startswith('HTTP/1.1 403'), '403 cross-site')
    h, b = req('GET', '/api/state', hdr={'Origin': 'http://evil.example'}); check(h.startswith('HTTP/1.1 403'), '403 foreign Origin')
    h, b = req('DELETE', '/api/cmd'); check(h.startswith('HTTP/1.1 405') and 'Allow: GET, POST' in h, '405 with Allow')
    h, b = req('POST', '/api/cmd', '', {**X, 'Transfer-Encoding': 'chunked'}); check(h.startswith('HTTP/1.1 411'), '411 chunked')
    h, b = req('GET', '/api/cables'); check(h.startswith('HTTP/1.1 404'), '/api/cables is 404 on Linux')
    h, b = req('GET', '/api/wave'); check(h.startswith('HTTP/1.1 404'), '/api/wave without a key is 404')
    h, b = req('GET', '/api/ping'); check(b == 'bananasplit-gui' and 'X-BananaSplit-Client: BananaSplit' in h, 'ping')
    tk = json.loads(req('GET', '/api/takes')[1]); check(set(tk) == {'rev', 'takes'}, 'takes: rev and takes')
    dv = json.loads(req('GET', '/api/devices')[1]); check({'cables', 'pulse', 'strip', 'bus', 'play', 'rec', 'ch'} <= set(dv), 'devices keys')
    check(all({'client','label','kind','default'} == set(c) for c in dv['strip']), 'devices candidates: client, label, kind, default')
    check(all({'id','cable','conn','connect'} == set(c) for c in dv['ch']), 'devices ch: id, cable, conn, connect')
    h, b = req('GET', '/api/update'); check(h.startswith('HTTP/1.1 200') and set(json.loads(b)) == {'page','notes','manual','rev'}, 'update keys')
    check(st['update'] is None or set(st['update']) >= {'auto','phase'}, 'state.update')
    # --- API v1: the documented paths, answers and examples
    J = {**X, 'Content-Type': 'application/json'}
    def v1(body, path='/api/v1/command'):
        h, b = req('POST', path, body, J); check('application/json' in h, f'{path} answers JSON'); return int(h.split()[1]), json.loads(b)
    v1st = json.loads(req('GET', '/api/v1/state')[1])
    check(v1st['api'] == 1 and isinstance(v1st['rev'], int) and set(v1st) - {'api', 'rev'} == set(st), 'v1 state = api, rev + the state')
    v1doc = doc[doc.index('## API v1'):doc.index('## Errors')]
    paths = json.loads(req('GET', '/api/v1/paths')[1])['paths']
    check(len(paths) == 151, f'151 v1 paths (got {len(paths)})')
    for pth in paths:
        tail = pth['path'].split('/')[-1] if '/sends/' not in pth['path'] else 'sends'
        check(f'`{tail}`' in v1doc or f'/{tail}`' in v1doc or f'{tail}/' in v1doc, f'v1 path {pth["path"]} is documented')
        check({'path', 'label', 'kind', 'value'} <= set(pth) and (pth['kind'] == 'flag' or {'min', 'max', 'step', 'unit'} <= set(pth)), f'{pth["path"]} fields')
    code, r = v1('{"cmd":"set","path":"/strips/1/gain_db","value":-6}')
    check((code, r) == (200, {'ok': True, 'text': '-6.0 dB', 'path': '/strips/1/gain_db', 'value': -6.0}), f'v1 set as documented (got {r})')
    code, r = v1('{"cmd":"toggle","path":"/buses/B1/mute"}'); check(code == 200 and r['value'] in (True, False), 'v1 toggle')
    code, r = v1('{"cmd":"step","path":"/strips/2/gain_db","by":3}'); check(code == 200 and r['value'] == 3.0, f'v1 step (got {r})')
    code, r = v1('[{"cmd":"fade","path":"/buses/V1/gain_db","to":-20,"ms":500},{"cmd":"set","path":"/strips/3/sends/B1","value":true}]')
    check(code == 200 and r['ok'] is True and len(r['results']) == 2, f'v1 batch (got {r})')
    time.sleep(0.8)
    check(json.loads(req('GET', '/api/v1/state')[1])['ch'][6]['gain'] == -20.0, 'v1 fade arrived')
    code, r = v1('[{"cmd":"set","path":"/strips/4/mute","value":false},{"cmd":"set","path":"/nope","value":1}]')
    check(code == 400 and r['error'].startswith('command 2: '), f'v1 batch checked whole (got {r})')
    check(v1('{"cmd":"recall","scene":"Gone"}')[0] == 400, 'v1 recall of a missing scene is 400')
    code, r = v1('', '/api/v1/recorder/stop'); check(code == 200 and r['value'] is False, f'v1 recorder/stop (got {r})')
    # --- MIDI: the documented ops (no sequencer in the sandbox: bindings still work, ports do not)
    for body, want in [('op=midibind&src=cc:1:7&target=/strips/1/gain_db', 'CC 7 · ch 1 → Strip 1 (Voice): fader'),
                       ('op=midibind&src=note:1:16&target=/strips/1/mute', 'Note 16 · ch 1 → Strip 1 (Voice): mute'),
                       ('op=midi&a=learn', 'move a control on your MIDI controller'), ('op=midi&a=stop', 'learning stopped'),
                       ('op=midiunbind&src=note:1:16', 'Note 16 · ch 1 unbound'), ('op=midiport&in=&out=', 'ok')]:
        code, b = cmd(body); check((code, b) == (200, want), f'{body} -> 200 {want!r} (got {code} {b!r})')
    code, b = cmd('op=midibind&src=note:1:1&target=/strips/1/gain_db'); check(code == 400 and 'is a button' in b, 'a button cannot set a number')
    time.sleep(0.2)
    mst = json.loads(req('GET', '/api/state')[1])['midi']
    midi_doc = state_doc[state_doc.index('| `midi` |'):]
    midi_doc = midi_doc[:midi_doc.index('\n')]
    for k in mst: check(f'`{k}`' in midi_doc, f'midi key {k!r} is documented')
    check([b['src'] for b in mst['bindings']] == ['cc:1:7'], f'midi bindings (got {mst["bindings"]})')
    h, b = req('POST', '/api/v1/command', '{"cmd":"toggle","path":"/strips/1/mute"}')
    check(h.startswith('HTTP/1.1 403') and b == '{"error":"forbidden"}', 'v1 403 without X-BananaSplit, as JSON')
finally:
    subprocess.run([binary, '--quit', '--gui-port', str(port)], env=env, capture_output=True, timeout=20)
    try: p.wait(timeout=10)
    except subprocess.TimeoutExpired: p.kill()
print(f'api_doc: {len(checks)} checks' + (f', FAILED: {bad}' if bad else ', all pass'))
sys.exit(1 if bad else 0)
