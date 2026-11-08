import os, sys
base = sys.argv[1]
bad = 0
for plat in ('linux', 'windows', 'macos'):
    o, n = f'{base}/old/{plat}', f'{base}/new/{plat}'
    if not os.path.isdir(n): print(plat, 'no new run'); continue
    names = sorted({f[:-4] for f in os.listdir(o) if f.endswith('.png')} | {f[:-4] for f in os.listdir(n) if f.endswith('.png')})
    for s in names:
        po, pn, to, tn = f'{o}/{s}.png', f'{n}/{s}.png', f'{o}/{s}.txt', f'{n}/{s}.txt'
        if not (os.path.exists(po) and os.path.exists(pn)): print(f'{plat:8} {s:26} MISSING on one side'); bad += 1; continue
        same_png = open(po, 'rb').read() == open(pn, 'rb').read()
        same_txt = open(to, encoding='utf-8').read() == open(tn, encoding='utf-8').read()
        if not (same_png and same_txt): bad += 1
        print(f'{plat:8} {s:26} pixels {"same" if same_png else "DIFFER"}   text {"same" if same_txt else "DIFFER"}')
print('ALL IDENTICAL' if not bad else f'{bad} scenario(s) differ')
