#!/usr/bin/env python3
"""Let a binary built on a new glibc start on an older one.

Rust's standard library refers to a few new glibc functions (glibc 2.39's
pidfd_spawnp and pidfd_getpid) as *weak* symbols: it checks at run time
whether they exist and works without them. The linker still records
"needs GLIBC_2.39" for them, and the dynamic loader then refuses to start
the program on an older glibc (Debian 12, Ubuntu 22.04) before that check
can happen.

This removes such a version requirement and makes its symbols unversioned,
which is what they would have been when linked against an older glibc: on
a new glibc they bind to the same functions as before, on an older one
they are absent and the standard library uses its fallback. Only
requirements newer than --max whose symbols are all weak are removed; if a
strong symbol needs a newer glibc, this refuses and changes nothing.

usage: glibc_compat.py BINARY [--max 2.34]
"""
import struct
import sys


def ver_key(name):
    return tuple(int(x) for x in name.split("_", 1)[1].split("."))


def main():
    args = sys.argv[1:]
    path = args[0]
    maxv = ver_key("GLIBC_" + (args[args.index("--max") + 1] if "--max" in args else "2.34"))
    data = bytearray(open(path, "rb").read())
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        sys.exit("not a 64-bit little-endian ELF file")
    e_shoff, = struct.unpack_from("<Q", data, 0x28)
    e_shentsize, e_shnum = struct.unpack_from("<HH", data, 0x3A)
    secs = []
    for i in range(e_shnum):
        _, typ, _, _, off, size, link, _, _, entsize = struct.unpack_from("<IIQQQQIIQQ", data, e_shoff + i * e_shentsize)
        secs.append(dict(type=typ, off=off, size=size, link=link, entsize=entsize))
    find = lambda t: [s for s in secs if s["type"] == t]
    if not (find(0x6FFFFFFE) and find(0x6FFFFFFF) and find(11)):
        sys.exit("no symbol versions in this file")
    vn, vs, ds = find(0x6FFFFFFE)[0], find(0x6FFFFFFF)[0], find(11)[0]
    strtab = secs[vn["link"]]

    def cstr(o):
        e = data.index(0, strtab["off"] + o)
        return data[strtab["off"] + o:e].decode()

    nsyms = ds["size"] // ds["entsize"]
    by_ver = {}
    for i in range(nsyms):
        info = data[ds["off"] + i * ds["entsize"] + 4]
        ver, = struct.unpack_from("<H", data, vs["off"] + 2 * i)
        by_ver.setdefault(ver & 0x7FFF, []).append((i, info >> 4))
    removed = []
    o = vn["off"]
    while True:
        vn_version, vn_cnt, vn_file, vn_aux, vn_next = struct.unpack_from("<HHIII", data, o)
        if cstr(vn_file).startswith("libc.so"):
            prev, a = None, o + vn_aux
            while True:
                vna_hash, vna_flags, vna_other, vna_name, vna_next = struct.unpack_from("<IHHII", data, a)
                name = cstr(vna_name)
                drop = name.startswith("GLIBC_") and ver_key(name) > maxv
                if drop:
                    syms = by_ver.get(vna_other, [])
                    if not syms or any(b != 2 for _, b in syms):
                        sys.exit(f"{path}: {name} is needed by a strong symbol; this binary really needs that glibc")
                    for i, _ in syms:  # VER_NDX_GLOBAL: unversioned
                        struct.pack_into("<H", data, vs["off"] + 2 * i, 1)
                    # unlink this entry from the chain
                    if prev is None:
                        struct.pack_into("<I", data, o + 8, vn_aux + vna_next if vna_next else 0)
                    else:
                        pa = prev
                        p_next, = struct.unpack_from("<I", data, pa + 12)
                        struct.pack_into("<I", data, pa + 12, p_next + vna_next if vna_next else 0)
                    vn_cnt -= 1
                    struct.pack_into("<H", data, o + 2, vn_cnt)
                    removed.append(f"{name} ({len(syms)} weak symbol(s))")
                    if not vna_next:
                        break
                    a += vna_next
                    continue
                if not vna_next:
                    break
                prev, a = a, a + vna_next
        if not vn_next:
            break
        o += vn_next
    if removed:
        open(path, "wb").write(data)
    print(f"{path}: removed {', '.join(removed) if removed else 'nothing'}")


if __name__ == "__main__":
    main()
