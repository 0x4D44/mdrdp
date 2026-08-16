#!/usr/bin/env python3
"""Count 64px tile-aligned block artefacts in a captured frame.

A correctly decoded photographic image varies smoothly across a tile boundary. A tile
that decoded wrongly shows up as a near-uniform patch whose edges disagree sharply with
all four neighbours. That combination — internally flat AND discontinuous on every side —
is what distinguishes an artefact from genuine image content, which is why neither test
alone is used.
"""
import struct, subprocess, sys

def load(path):
    bmp = "/tmp/_blk.bmp"
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

def main():
    W, H, px = load(sys.argv[1])
    T = 64
    flat_and_edged = 0
    for ty in range(1, H // T - 1):
        for tx in range(1, W // T - 1):
            x0, y0 = tx * T, ty * T
            samples = [px(x0 + dx, y0 + dy) for dy in range(4, T, 8) for dx in range(4, T, 8)]
            mean = tuple(sum(s[c] for s in samples) / len(samples) for c in range(3))
            spread = max(max(abs(s[c] - mean[c]) for c in range(3)) for s in samples)
            if spread > 18:
                continue  # genuine detail inside the tile, not a flat artefact
            # discontinuity across all four edges
            def edge(ax, ay, bx, by):
                return sum(
                    abs(px(ax + i * dx1, ay + i * dy1)[c] - px(bx + i * dx1, by + i * dy1)[c])
                    for i in range(0, T, 8) for c in range(3)
                ) / (len(range(0, T, 8)) * 3)
            dx1, dy1 = (1, 0) if True else (0, 1)
            top = sum(abs(px(x0 + i, y0)[c] - px(x0 + i, y0 - 1)[c]) for i in range(0, T, 8) for c in range(3)) / (len(range(0, T, 8)) * 3)
            bot = sum(abs(px(x0 + i, y0 + T - 1)[c] - px(x0 + i, y0 + T)[c]) for i in range(0, T, 8) for c in range(3)) / (len(range(0, T, 8)) * 3)
            lef = sum(abs(px(x0, y0 + i)[c] - px(x0 - 1, y0 + i)[c]) for i in range(0, T, 8) for c in range(3)) / (len(range(0, T, 8)) * 3)
            rig = sum(abs(px(x0 + T - 1, y0 + i)[c] - px(x0 + T, y0 + i)[c]) for i in range(0, T, 8) for c in range(3)) / (len(range(0, T, 8)) * 3)
            if min(top, bot, lef, rig) > 12:
                flat_and_edged += 1
    total = (W // T - 2) * (H // T - 2)
    print(f"  {sys.argv[1].split('/')[-1]:22} block artefacts: {flat_and_edged} of {total} interior tiles")

main()
