#!/usr/bin/env python3
# Third-party code: see tools/NOTICE for provenance and licence.
"""Differential model: FreeRDP progressive_rfx_srl_read vs mdrdp srl::decode_srl."""

import random


class BR:
    """MSB-first bit reader; past-end reads as 0 (matches both impls)."""

    def __init__(self, data):
        self.d = data
        self.p = 0

    def bit(self):
        if self.p >= len(self.d) * 8:
            self.p += 1
            return 0
        b = (self.d[self.p >> 3] >> (7 - (self.p & 7))) & 1
        self.p += 1
        return b

    def bits(self, n):
        v = 0
        for _ in range(n):
            v = (v << 1) | self.bit()
        return v


# ---------------- FreeRDP reference (progressive.c:1075-1162) ----------------
class FreeRdpSrl:
    def __init__(self, data):
        self.bs = BR(data)
        self.kp = 8          # progressive.c:1272  state.kp = 8
        self.nz = 0
        self.mode = 0        # progressive.c:1273  state.mode = 0

    def read(self, num_bits):
        if self.nz:
            self.nz -= 1
            return 0
        k = self.kp // 8
        if not self.mode:
            bit = self.bs.bit()
            if not bit:
                self.nz = 1 << k
                self.kp = min(self.kp + 4, 80)
                self.nz -= 1
                return 0
            else:
                self.nz = 0
                self.mode = 1            # <-- latch: unary is next
                if k:
                    self.nz = self.bs.bits(k)
                if self.nz:
                    self.nz -= 1
                    return 0
        self.mode = 0
        sign = self.bs.bit()
        self.kp = 0 if self.kp < 6 else self.kp - 6
        if num_bits == 1:
            return -1 if sign else 1
        mag = 1
        mx = (1 << num_bits) - 1
        while mag < mx:
            if self.bs.bit():
                break
            mag += 1
        return -mag if sign else mag


# ---------------- mdrdp (srl.rs:19-109) ----------------
def mdrdp_decode_srl(data, num_values, num_bits):
    if num_values == 0 or len(data) == 0:
        return [0] * num_values
    out = []
    r = BR(data)
    kp = 0               # srl.rs:26  let mut kp: u32 = 0
    nz = 0
    while len(out) < num_values:
        k = kp >> 3
        if nz > 0:
            nz -= 1
            out.append(0)
            continue
        bit = r.bit()
        if not bit:
            nz = 1 << k
            kp = min(kp + 4, 80)
            nz -= 1
            out.append(0)
            continue
        zeros = r.bits(k)
        if zeros > 0:
            nz = zeros - 1
            out.append(0)
            continue
        # unary mode
        kp = 0 if kp < 6 else kp - 6
        sign = r.bit()
        if num_bits == 1:
            out.append(-1 if sign else 1)
            continue
        q = 0
        while True:
            if r.bit() or q >= 0x8000:
                break
            q += 1
        extra = num_bits - 1
        if 0 < extra < 16:
            mag = (q << extra) | r.bits(extra)
        else:
            mag = q
        out.append(-mag if sign else mag)
    return out


def compare(data, n, num_bits):
    ref_state = FreeRdpSrl(data)
    ref = [ref_state.read(num_bits) for _ in range(n)]
    ours = mdrdp_decode_srl(data, n, num_bits)
    return ref, ours


if __name__ == "__main__":
    print("=== hand-picked cases ===")
    cases = [
        (bytes([0b10111000, 0x00]), 4, 3),
        (bytes([0b11011100, 0x00]), 4, 3),
        (bytes([0b01111111, 0x00]), 4, 2),
        (bytes([0b10101010, 0xAA, 0x00]), 6, 4),
    ]
    for data, n, nb in cases:
        ref, ours = compare(data, n, nb)
        print(f"data={[bin(b) for b in data]} n={n} numBits={nb}")
        print(f"  freerdp: {ref}")
        print(f"  mdrdp  : {ours}   {'MATCH' if ref == ours else '*** DIFFER ***'}")

    print("\n=== random fuzz ===")
    random.seed(7)
    diff = 0
    total = 0
    first_diff_at = {}
    for _ in range(3000):
        nb = random.randint(1, 6)
        data = bytes(random.getrandbits(8) for _ in range(random.randint(2, 12)))
        n = random.randint(1, 40)
        ref, ours = compare(data, n, nb)
        total += 1
        if ref != ours:
            diff += 1
            for i, (a, b) in enumerate(zip(ref, ours)):
                if a != b:
                    first_diff_at[i] = first_diff_at.get(i, 0) + 1
                    break
    print(f"differing streams: {diff}/{total}")
    print("first-divergence index histogram (index -> count):",
          dict(sorted(first_diff_at.items())[:8]))
