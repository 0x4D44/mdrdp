#!/usr/bin/env python3
"""Isolate which of the three srl.rs defects each contributes, by fixing them one at a time."""
import random
from srl_diff import BR, FreeRdpSrl


def mdrdp(data, n, num_bits, fix_kp=False, fix_mode=False, fix_unary=False):
    if n == 0 or len(data) == 0:
        return [0] * n
    out = []
    r = BR(data)
    kp = 8 if fix_kp else 0
    nz = 0
    mode = 0
    while len(out) < n:
        k = kp >> 3
        if nz > 0:
            nz -= 1
            out.append(0)
            continue
        if (not fix_mode) or mode == 0:
            bit = r.bit()
            if not bit:
                nz = (1 << k) - 1
                kp = min(kp + 4, 80)
                out.append(0)
                continue
            zeros = r.bits(k)
            if fix_mode:
                mode = 1
            if zeros > 0:
                nz = zeros - 1
                out.append(0)
                continue
        mode = 0
        kp = 0 if kp < 6 else kp - 6
        sign = r.bit()
        if num_bits == 1:
            out.append(-1 if sign else 1)
            continue
        if fix_unary:
            mag = 1
            mx = (1 << num_bits) - 1
            while mag < mx:
                if r.bit():
                    break
                mag += 1
        else:
            q = 0
            while True:
                if r.bit() or q >= 0x8000:
                    break
                q += 1
            extra = num_bits - 1
            mag = ((q << extra) | r.bits(extra)) if 0 < extra < 16 else q
        out.append(-mag if sign else mag)
    return out


random.seed(11)
cases = []
for _ in range(4000):
    nb = random.randint(1, 6)
    data = bytes(random.getrandbits(8) for _ in range(random.randint(2, 12)))
    n = random.randint(1, 40)
    cases.append((data, n, nb))

variants = [
    ("as-shipped                       ", dict()),
    ("+ kp=8 only                      ", dict(fix_kp=True)),
    ("+ mode latch only                ", dict(fix_mode=True)),
    ("+ unary(mag=1,cap) only          ", dict(fix_unary=True)),
    ("+ kp + mode                      ", dict(fix_kp=True, fix_mode=True)),
    ("+ kp + mode + unary  (all three) ", dict(fix_kp=True, fix_mode=True, fix_unary=True)),
]
for name, kw in variants:
    bad = 0
    for data, n, nb in cases:
        st = FreeRdpSrl(data)
        ref = [st.read(nb) for _ in range(n)]
        if ref != mdrdp(data, n, nb, **kw):
            bad += 1
    print(f"{name} mismatching streams: {bad}/{len(cases)}")
