#!/usr/bin/env python3
"""Fraction of the frame occupied by unnaturally FLAT 32x32 patches.

A photographic wallpaper is smooth but never perfectly uniform: quantisation alone keeps
a little variation in every 32x32 window. A decoder artefact is a patch of exactly one
colour. Counting the share of the image inside such patches separates the two without
needing a reference capture.
"""
import struct, subprocess, sys

def load(path):
    bmp = "/tmp/_flat.bmp"
    subprocess.run(["sips", "-s", "format", "bmp", path, "--out", bmp], capture_output=True)
    f = open(bmp, "rb").read()
    off = struct.unpack_from("<I", f, 10)[0]
    W = struct.unpack_from("<i", f, 18)[0]
    H = abs(struct.unpack_from("<i", f, 22)[0])
    bpp = struct.unpack_from("<H", f, 28)[0] // 8
    stride = W * bpp + ((4 - (W * bpp % 4)) % 4)
    def px(x, y):
        p = off + (H - 1 - y) * stride + x * bpp
        return (f[p + 2], f[p + 1], f[p])
    return W, H, px

W, H, px = load(sys.argv[1])
S = 32
flat = 0
total = 0
for y0 in range(0, H - S, S):
    for x0 in range(0, W - S, S):
        total += 1
        vals = [px(x0 + dx, y0 + dy) for dy in range(0, S, 4) for dx in range(0, S, 4)]
        spread = max(max(v[c] for v in vals) - min(v[c] for v in vals) for c in range(3))
        if spread <= 2:
            flat += 1
print(f"  {sys.argv[1].split('/')[-1]:20} perfectly-flat 32px patches: {flat:4} / {total} = {flat*100/total:5.1f}%")
