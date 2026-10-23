#!/usr/bin/env python3
"""A VBAN peer for the end-to-end test, written separately from BananaSplit's own
code, straight from VB-Audio's "VBAN Protocol Specifications" (rev. 13).
Python standard library only.

It behaves like Voicemeeter on another computer: it sends a 1 kHz sine as a
44.1 kHz, 24-bit stereo stream in bursts of four 128-sample packets (a
512-sample audio buffer; 256 24-bit stereo samples would not fit the 1436
bytes a packet may carry), with R = -L so the channel order is checked too,
plus some packets BananaSplit must ignore. At the same time it receives the
stream BananaSplit sends back and checks every header field and the audio.

usage: vban_peer.py BANANASPLIT_PORT MY_PORT SECONDS EXPECTED_AMPLITUDE
"""
import gc
import math
import select
import socket
import struct
import sys
import time

SR_LIST = [6000, 12000, 24000, 48000, 96000, 192000, 384000, 8000, 16000, 32000, 64000,
           128000, 256000, 512000, 11025, 22050, 44100, 88200, 176400, 352800, 705600]
INT16, INT24 = 1, 2


def header(sr, nbs, nbc, dtype, name, frame, sub=0x00):
    return struct.pack("<4sBBBB16sI", b"VBAN", SR_LIST.index(sr) | sub, nbs - 1, nbc - 1, dtype,
                       name.encode("ascii").ljust(16, b"\0"), frame)


def int24(v):
    v = max(-8388608, min(8388607, int(round(v * 8388607))))
    return struct.pack("<i", v)[:3]


def fail(msg):
    print("VBAN PEER FAIL: " + msg)
    sys.exit(1)


def main():
    bananasplit_port, my_port, seconds, expect_amp = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
    tx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.bind(("127.0.0.1", my_port))
    rx.setblocking(False)

    sr, nbs, burst, f = 44100, 128, 4, 1000.0
    gc.disable()  # no collector pauses in the middle of a burst
    # one second of the stream, precomputed (44100 = 441 x 100 samples, so
    # a 1 kHz sine repeats exactly every 441 samples)
    period_pcm = [int24(0.5 * math.sin(2 * math.pi * f * k / sr)) + int24(-0.5 * math.sin(2 * math.pi * f * k / sr)) for k in range(441)]
    frame = 0
    t0 = time.monotonic()
    received = []          # (frame counter, samples [(l, r), ...])
    sources = set()
    bad = []

    def send_packet():
        nonlocal frame
        start = frame * nbs
        pcm = b"".join(period_pcm[(start + k) % 441] for k in range(nbs))
        assert 28 + len(pcm) <= 1464
        tx.sendto(header(sr, nbs, 2, INT24, "Voicemeeter", frame) + bytes(pcm), ("127.0.0.1", bananasplit_port))
        frame += 1
        if frame % 200 == 0:  # things BananaSplit must ignore, now and then
            tx.sendto(header(sr, 16, 1, INT16, "Voicemeeter", 0, sub=0x40) + b"Strip[0].Mute=1;", ("127.0.0.1", bananasplit_port))
            tx.sendto(header(sr, 64, 2, INT16, "SomeoneElse", frame) + bytes(256), ("127.0.0.1", bananasplit_port))
            tx.sendto(header(sr, 64, 2, INT16, "Voicemeeter", frame) + bytes(100), ("127.0.0.1", bananasplit_port))  # wrong size
            tx.sendto(b"VBAN" + bytes(10), ("127.0.0.1", bananasplit_port))

    def receive():
        while True:
            try:
                data, addr = rx.recvfrom(2048)
            except BlockingIOError:
                return
            sources.add(addr)
            if len(data) < 28 or len(data) > 1464 or data[:4] != b"VBAN":
                bad.append("not a VBAN packet (%d bytes)" % len(data))
                continue
            magic, fsr, fnbs, fnbc, fbit, name, fr = struct.unpack("<4sBBBB16sI", data[:28])
            nbs_, nbc_ = fnbs + 1, fnbc + 1
            checks = [
                (fsr & 0xE0 == 0, "sub-protocol is not audio"),
                (fsr & 0x1F < len(SR_LIST) and SR_LIST[fsr & 0x1F] == 48000, "sample rate index %d is not 48 kHz" % (fsr & 0x1F)),
                (fbit == INT16, "format byte 0x%02x is not PCM int16" % fbit),
                (nbc_ == 2, "%d channels, not 2" % nbc_),
                (1 <= nbs_ <= 256, "bad sample count %d" % nbs_),
                (len(data) - 28 == nbs_ * nbc_ * 2, "payload %d bytes for %d x %d samples" % (len(data) - 28, nbs_, nbc_)),
                (name == b"Stream1".ljust(16, b"\0"), "stream name %r" % name),
            ]
            for ok, msg in checks:
                if not ok:
                    bad.append(msg)
            s = struct.unpack("<%dh" % (nbs_ * 2), data[28:28 + nbs_ * 4])
            received.append((fr, [(s[2 * i] / 32768.0, s[2 * i + 1] / 32768.0) for i in range(nbs_)]))

    while time.monotonic() - t0 < seconds:
        due = t0 + (frame // burst) * burst * nbs / sr
        now = time.monotonic()
        if now >= due:
            for _ in range(burst):
                send_packet()
            continue
        select.select([rx], [], [], due - now)
        receive()
    time.sleep(0.2)
    receive()

    # ---- the stream BananaSplit sent ----
    if bad:
        fail("%d bad packets, e.g. %s" % (len(bad), bad[0]))
    if len(received) < 1000:
        fail("only %d packets from BananaSplit" % len(received))
    if sources != {("127.0.0.1", bananasplit_port)}:
        fail("packets came from %r, not from BananaSplit's VBAN port %d" % (sources, bananasplit_port))
    for (a, _), (b, _) in zip(received, received[1:]):
        if b != (a + 1) & 0xFFFFFFFF:
            fail("frame counter went from %d to %d" % (a, b))
    sizes = sorted({len(s) for _, s in received})
    samples = [x for _, s in received for x in s]
    import os
    if os.environ.get("VBAN_PEER_DUMP"):
        with open(os.environ["VBAN_PEER_DUMP"], "wb") as fh:
            fh.write(struct.pack("<%df" % len(samples), *[l for l, _ in samples]))
    # 3 seconds near the end. The rate is still being trimmed (by at most 0.05 %
    # for the level, plus the clock drift), so: the frequency from the
    # zero crossings must be 1 kHz within 0.1 % (44.1 kHz -> 48 kHz done
    # right), every 100 ms must be a clean sine at that frequency (no
    # dropout, click or glitch anywhere), at the expected level, R = -L.
    # (leaving out the last half second: by then this peer has stopped sending)
    tail = samples[-168000:-24000]
    if len(tail) < 144000:
        fail("only %d frames received" % len(samples))
    left = [l for l, _ in tail]
    ups = []
    for i in range(1, len(left)):
        if left[i - 1] < 0.0 <= left[i]:
            ups.append(i - 1 + (-left[i - 1]) / (left[i] - left[i - 1]))
    n = len(ups)
    if n < 100:
        fail("no sine came back (%d zero crossings)" % n)
    mi, mt = (n - 1) / 2.0, sum(ups) / n
    period = sum((k - mi) * (t - mt) for k, t in enumerate(ups)) / sum((k - mi) ** 2 for k in range(n))
    freq = 48000.0 / period
    worst_res, amps = 0.0, []
    for start in range(0, len(left) - 4800 + 1, 4800):
        seg = left[start:start + 4800]
        # this stretch's own frequency (the trim changes it by a few ppm)
        cr = [t - start for t in ups if start + 1 <= t < start + 4799]
        m = len(cr)
        if m < 20:
            fail("no sine in the stretch at %d" % start)
        ci, ct = (m - 1) / 2.0, sum(cr) / m
        local = sum((k - ci) * (t - ct) for k, t in enumerate(cr)) / sum((k - ci) ** 2 for k in range(m))
        w = 2 * math.pi / local
        ss = sc = cc = ys = yc = 0.0
        for i, y in enumerate(seg):
            s_, c_ = math.sin(w * i), math.cos(w * i)
            ss += s_ * s_; sc += s_ * c_; cc += c_ * c_; ys += y * s_; yc += y * c_
        det = ss * cc - sc * sc
        a = (ys * cc - yc * sc) / det
        b = (yc * ss - ys * sc) / det
        amp = math.hypot(a, b)
        res = math.sqrt(sum((y - (a * math.sin(w * i) + b * math.cos(w * i))) ** 2 for i, y in enumerate(seg)) / len(seg))
        amps.append(amp)
        worst_res = max(worst_res, res / max(amp, 1e-9))
    worst_lr = max(abs(l + r) for l, r in tail)
    print("VBAN PEER: %d packets of %s samples, %d frames; %.3f Hz, amplitude %.4f..%.4f (want %.4f), worst residual %.1f dB, |L+R| max %.5f"
          % (len(received), "/".join(map(str, sizes)), len(samples), freq, min(amps), max(amps), expect_amp,
             20 * math.log10(max(worst_res, 1e-12)), worst_lr))
    if abs(freq - f) > f * 1e-3:
        fail("%.3f Hz: the sample rate conversion is off" % freq)
    if any(abs(a - expect_amp) > 0.01 for a in amps):
        fail("amplitude %.4f..%.4f, want %.4f" % (min(amps), max(amps), expect_amp))
    if worst_res > 10 ** (-50 / 20):
        fail("a 100 ms stretch is not a clean sine (%.1f dB): a dropout or glitch" % (20 * math.log10(worst_res)))
    if worst_lr > 2e-4:
        fail("R is not -L (channels swapped or mixed): %.5f" % worst_lr)
    print("VBAN PEER PASS")


main()
